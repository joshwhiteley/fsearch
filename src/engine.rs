use crate::content::{self, ContentMatch};
use crate::filters::{self, Filters};
use crate::frecency::Frecency;
use crate::index::PathStore;
use crate::matcher::{self, FilenameMode};
use crate::sem;
use crate::util::unix_now;
use crate::walker::FileMeta;
use crate::{config::Config, index, walker};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

pub const FILENAME_LIMIT: usize = 500;
pub const CONTENT_LIMIT: usize = 1000;
pub const SEMANTIC_LIMIT: usize = 100;
pub const CONTENT_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Fuzzy,
    Regex,
    Content,
    Semantic,
    Calc,
}

pub fn parse_query(input: &str, regex_mode: bool) -> (Mode, String) {
    if let Some(rest) = input.strip_prefix('=') {
        (Mode::Calc, rest.trim().to_string())
    } else if let Some(rest) = input.strip_prefix('>') {
        (Mode::Content, rest.trim_start().to_string())
    } else if let Some(rest) = input.strip_prefix('?') {
        (Mode::Semantic, rest.trim_start().to_string())
    } else if regex_mode {
        (Mode::Regex, input.to_string())
    } else {
        (Mode::Fuzzy, input.to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultRow {
    pub path: String,
    pub line_number: Option<u64>,
    pub line: Option<String>,
    /// True when this row ranks high because the user opened it before
    /// (frecency) — the UI groups these under "recent opens".
    pub recent_open: bool,
    /// Index metadata (mtime seconds, size bytes) for filename rows;
    /// content-hit and semantic-only rows carry none.
    pub meta: Option<crate::walker::FileMeta>,
    /// Semantic match score 0..=1; None for non-semantic and filename-only
    /// rows.
    pub score: Option<f32>,
}

/// Merges filename and semantic rankings with reciprocal rank fusion. Ranks
/// are one-based, matching the RRF formula `1 / (60 + rank)`.
type SourceRank = Option<(usize, ResultRow)>;
type UnifiedSources = (SourceRank, SourceRank);

fn merge_unified_results(filename: &[ResultRow], semantic: &[ResultRow]) -> Vec<ResultRow> {
    let mut by_path: HashMap<String, UnifiedSources> = HashMap::new();
    for (rank, row) in filename.iter().enumerate() {
        by_path
            .entry(row.path.clone())
            .or_default()
            .0
            .get_or_insert((rank + 1, row.clone()));
    }
    for (rank, row) in semantic.iter().enumerate() {
        by_path
            .entry(row.path.clone())
            .or_default()
            .1
            .get_or_insert((rank + 1, row.clone()));
    }

    let mut ranked: Vec<(f64, usize, usize, ResultRow)> = by_path
        .into_values()
        .map(|(filename, semantic)| {
            let filename_rank = filename.as_ref().map_or(usize::MAX, |(rank, _)| *rank);
            let semantic_rank = semantic.as_ref().map_or(usize::MAX, |(rank, _)| *rank);
            let rrf = filename
                .as_ref()
                .map_or(0.0, |(rank, _)| 1.0 / (60.0 + *rank as f64))
                + semantic
                    .as_ref()
                    .map_or(0.0, |(rank, _)| 1.0 / (60.0 + *rank as f64));
            let row = match (filename, semantic) {
                (Some((_, mut filename)), Some((_, semantic))) => {
                    // Keep filename metadata/frecency/display fields, but add
                    // the best semantic context so the row explains the hit.
                    filename.line_number = semantic.line_number;
                    filename.line = semantic.line;
                    filename.score = semantic.score;
                    filename
                }
                (Some((_, filename)), None) => filename,
                (None, Some((_, semantic))) => semantic,
                (None, None) => unreachable!("unified row has no source"),
            };
            (rrf, filename_rank, semantic_rank, row)
        })
        .collect();
    // RRF is the primary order; source ranks and then the path make ties
    // deterministic without changing the ranking signal.
    ranked.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.path.cmp(&b.3.path))
    });
    ranked.into_iter().map(|(_, _, _, row)| row).collect()
}

#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub indexed: usize,
    pub indexing: bool,
    pub matches: usize,
    pub error: Option<String>,
    /// (files walked so far, expected total from the previous index) during
    /// the startup walk; None once the walk finishes.
    pub walk: Option<(usize, Option<usize>)>,
}

enum Msg {
    IndexSnapshot {
        store: Arc<PathStore>,
        indexing: bool,
    },
    IndexProgress {
        count: usize,
        /// Some when the walk is re-checking a cached index (expected total
        /// = cached store length), None on a cold start with no cache.
        expected: Option<usize>,
    },
    /// One-time setup failure (bad excludes glob, unwatchable root). Keeps
    /// whatever index snapshot is already live and surfaces the reason.
    IndexError {
        error: String,
    },
    FilenameResults {
        generation: u64,
        indices: Vec<usize>,
        strong: usize,
        error: Option<String>,
    },
    ContentHit {
        generation: u64,
        hit: ContentMatch,
    },
    /// The content-search pattern was invalid; carries the reason for
    /// display (no hits were produced).
    ContentError {
        generation: u64,
        error: String,
    },
    SemanticResults {
        generation: u64,
        rows: Vec<ResultRow>,
        error: Option<String>,
    },
}

struct SemJob {
    generation: u64,
    query: String,
    filters: crate::filters::Filters,
}

/// The text line a semantic hit starts on, trimmed and capped, for row display.
fn snippet_line(path: &str, line: u64, pdf_cache: &std::path::Path) -> Option<String> {
    let text = if crate::pdf::is_pdf_path(path) {
        crate::pdf::extract_cached(path, pdf_cache).ok()
    } else if crate::office::is_office_path(path) {
        let office_cache = crate::office::cache_dir_for(pdf_cache);
        crate::office::extract_cached(path, &office_cache).ok()
    } else {
        std::fs::read_to_string(path).ok()
    }?;
    let s = text.lines().nth(line.saturating_sub(1) as usize)?;
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.chars().take(160).collect())
}

struct FilenameJob {
    generation: u64,
    query: String,
    mode: FilenameMode,
    store: Arc<PathStore>,
    boosts: Arc<HashMap<String, u32>>,
    filters: Filters,
    quiet: Arc<crate::quiet::Quiet>,
}

pub struct Engine {
    msg_rx: Receiver<Msg>,
    msg_tx: Sender<Msg>,
    job_tx: Sender<FilenameJob>,
    store: Arc<PathStore>,
    results: Vec<ResultRow>,
    /// Source rankings retained so a delayed semantic response can be merged
    /// without losing the instant filename result set.
    filename_results: Vec<ResultRow>,
    semantic_results: Vec<ResultRow>,
    status: EngineStatus,
    mode: Mode,
    generation: u64,
    query: String,
    strong: usize,
    max_content_filesize: u64,
    pdf_cache: PathBuf,
    filters: Filters,
    pending_content: Option<(String, Instant)>,
    content_cancel: Option<Arc<AtomicBool>>,
    pending_semantic: Option<(String, Instant)>,
    sem_tx: Option<Sender<SemJob>>,
    /// Open history; None in filter mode (stdin lines are not files, so
    /// nothing is recorded or persisted).
    frecency: Option<Frecency>,
    boosts: Arc<HashMap<String, u32>>,
    quiet: Arc<crate::quiet::Quiet>,
    /// Whether bare fuzzy queries may blend in semantic results.
    unified: bool,
    /// Position of the unified fold boundary; derived from filename strong
    /// matches so semantic-only rows do not redefine the filename floor.
    unified_strong: usize,
    filter: bool,
}

const WATCH_DEBOUNCE: Duration = Duration::from_millis(400);
const WATCH_SAVE_EVERY: Duration = Duration::from_secs(60);

fn cache_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Bare queries only start the semantic worker when a persisted store exists;
/// this keeps builds without semantic support and first-run installs on the
/// instant filename-only path.
fn semantic_store_available() -> bool {
    sem::default_store_path().is_file()
}

/// Live watcher plus its event stream (the watcher must stay alive for
/// events to keep flowing).
type WatcherStream = (
    notify::RecommendedWatcher,
    Receiver<notify::Result<notify::Event>>,
);

/// Starts watching `roots` and returns the live watcher plus the number of
/// roots that could not be watched (watcher creation failure counts as all
/// of them). Armed *before* the initial walk so that no change can slip
/// between walk completion and stream start.
fn start_watcher(roots: &[std::path::PathBuf]) -> (Option<WatcherStream>, usize) {
    use notify::Watcher;
    let (event_tx, event_rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = event_tx.send(res);
    }) {
        Ok(w) => w,
        // no watcher at all: every root is effectively unwatched
        Err(_) => return (None, roots.len()),
    };
    let mut failed = 0usize;
    for root in roots {
        if watcher
            .watch(root, notify::RecursiveMode::Recursive)
            .is_err()
        {
            failed += 1;
        }
    }
    // a watcher that watches nothing is worse than none: it only burns the
    // event thread; callers surface the failure count instead
    (
        (failed < roots.len()).then_some((watcher, event_rx)),
        failed,
    )
}

/// Folds filesystem events into fresh index snapshots, forever. Changed and
/// created files go to the front of the list (they are the newest); deleted
/// paths — including whole directories — are filtered out.
fn watch_loop(
    event_rx: &Receiver<notify::Result<notify::Event>>,
    excludes: &globset::GlobSet,
    cache_path: &std::path::Path,
    indexer_tx: &Sender<Msg>,
    mut current: Vec<(String, FileMeta)>,
) {
    let mut last_save = Instant::now();
    // single-writer courtesy: remember the cache state we produced; if the
    // file changes underneath us (another instance, a --reindex), stop
    // saving so we never clobber fresher data with our older snapshot
    let mut our_stamp = cache_mtime(cache_path);
    loop {
        // block until something happens, then debounce-collect the burst
        let Ok(first) = event_rx.recv() else { return };
        let mut touched: std::collections::HashSet<std::path::PathBuf> = HashSet::new();
        let mut absorb = |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                touched.extend(event.paths);
            }
        };
        absorb(first);
        let deadline = Instant::now() + WATCH_DEBOUNCE;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match event_rx.recv_timeout(left) {
                Ok(res) => absorb(res),
                Err(_) => break,
            }
        }

        let mut fronts: Vec<(String, FileMeta)> = Vec::new();
        let mut gone: std::collections::HashSet<String> = HashSet::new();
        let mut gone_dir_prefixes: Vec<String> = Vec::new();
        let push_front = |fronts: &mut Vec<(String, FileMeta)>,
                          gone: &mut HashSet<String>,
                          entry: (String, FileMeta)| {
            if gone.insert(entry.0.clone()) {
                fronts.push(entry);
            }
        };
        for path in touched {
            if excludes.is_match(&path) {
                continue;
            }
            let s = path.to_string_lossy().into_owned();
            if path.is_file() {
                // front-inserted (it is the newest); the stale base copy is
                // filtered via `gone`
                let std_meta = std::fs::metadata(&path).ok();
                let meta = FileMeta {
                    mtime: std_meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs() as i64),
                    size: std_meta.map_or(0, |m| m.len()),
                };
                push_front(&mut fronts, &mut gone, (s.clone(), meta));
                // the file may sit where a directory used to be (deleted and
                // replaced inside one debounce burst): prune the old subtree
                // so no ghost children survive
                gone_dir_prefixes.push(format!("{s}/"));
            } else if path.is_dir() {
                // a directory appeared or changed: replace its prior state —
                // a recreated dir must not keep stale children — then index
                // what exists now
                gone_dir_prefixes.push(format!("{s}/"));
                gone.insert(s.clone());
                let (entries, _) =
                    walker::collect_sorted(std::slice::from_ref(&path), excludes, false);
                // collect_sorted skips the walk root, so re-add the dir
                // itself with fresh metadata
                let std_meta = std::fs::metadata(&path).ok();
                let meta = FileMeta {
                    mtime: std_meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs() as i64),
                    size: 0,
                };
                push_front(&mut fronts, &mut gone, (format!("{s}/"), meta));
                for entry in entries {
                    push_front(&mut fronts, &mut gone, entry);
                }
            } else {
                // deleted: could have been a file or a whole directory
                gone_dir_prefixes.push(format!("{s}/"));
                gone.insert(s);
            }
        }
        if fronts.is_empty() && gone.is_empty() {
            continue;
        }
        let mut next: Vec<(String, FileMeta)> = Vec::with_capacity(current.len() + fronts.len());
        next.extend(fronts);
        next.extend(
            current
                .iter()
                .filter(|(p, _)| {
                    !gone.contains(p) && !gone_dir_prefixes.iter().any(|d| p.starts_with(d))
                })
                .cloned(),
        );
        current = next;
        if indexer_tx
            .send(Msg::IndexSnapshot {
                store: Arc::new(PathStore::from_entries(&current)),
                indexing: false,
            })
            .is_err()
        {
            return; // engine dropped
        }
        if last_save.elapsed() >= WATCH_SAVE_EVERY {
            last_save = Instant::now();
            if cache_mtime(cache_path) == our_stamp {
                let _ = index::save(&current, cache_path);
                our_stamp = cache_mtime(cache_path);
            }
        }
    }
}

/// The single filename search worker: always process only the newest job.
fn spawn_search_worker(job_rx: Receiver<FilenameJob>, tx: Sender<Msg>) {
    let worker_tx = tx;
    std::thread::spawn(move || {
        while let Ok(mut job) = job_rx.recv() {
            while let Ok(newer) = job_rx.try_recv() {
                job = newer;
            }
            let (indices, strong, error) = match matcher::search_boosted(
                &job.store,
                &job.query,
                job.mode,
                FILENAME_LIMIT,
                &job.boosts,
                &job.filters,
                &job.quiet,
            ) {
                Ok(r) => (r.indices, r.strong, None),
                Err(e) => (Vec::new(), 0, Some(format!("invalid pattern: {e}"))),
            };
            if worker_tx
                .send(Msg::FilenameResults {
                    generation: job.generation,
                    indices,
                    strong,
                    error,
                })
                .is_err()
            {
                return;
            }
        }
    });
}

impl Engine {
    pub fn new(config: Config, cache_path: PathBuf, history_path: PathBuf) -> Engine {
        let pdf_cache = cache_path
            .parent()
            .map(|p| p.join("pdftext"))
            .unwrap_or_else(crate::pdf::default_cache_dir);
        let quiet_patterns = config.quiet.clone();
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let (job_tx, job_rx) = mpsc::channel::<FilenameJob>();

        // filename search worker: always process only the newest job
        spawn_search_worker(job_rx, msg_tx.clone());

        // indexer: cached paths first, then a fresh walk, then save
        let indexer_tx = msg_tx.clone();
        let max_content_filesize = config.max_content_filesize;
        let unified = config.unified;
        std::thread::spawn(move || {
            let cached = index::load(&cache_path);
            let expected = cached.as_ref().map(|c| c.len());
            if let Some(cached) = cached {
                let _ = indexer_tx.send(Msg::IndexSnapshot {
                    store: Arc::new(cached),
                    indexing: true,
                });
            }
            let excludes = match walker::build_exclude_set(&config.excludes) {
                Ok(set) => set,
                Err(e) => {
                    // keep the cached snapshot searchable; just report why
                    // no fresh walk can happen this session
                    let _ = indexer_tx.send(Msg::IndexError {
                        error: format!("invalid exclude pattern: {e}"),
                    });
                    return;
                }
            };
            // arm the watcher before walking: events raised mid-walk sit in
            // the channel and are folded in afterwards (re-stat makes them
            // idempotent), so nothing slips through the startup window
            let (watcher, watch_failures) = start_watcher(&config.roots);
            if watch_failures > 0 {
                let noun = if watch_failures == 1 { "root" } else { "roots" };
                let _ = indexer_tx.send(Msg::IndexError {
                    error: format!("live updates unavailable for {watch_failures} {noun}"),
                });
            }
            let (path_tx, path_rx) = mpsc::channel::<(String, FileMeta)>();
            let roots = config.roots.clone();
            let walk_excludes = excludes.clone();
            let index_apps = config.index_apps;
            let walk_thread = std::thread::spawn(move || {
                walker::walk(&roots, &walk_excludes, index_apps, &path_tx)
            });
            // the index is ordered newest-first, so head-of-list results,
            // regex hits (index order) and fuzzy score ties all favor recency
            let mut fresh: Vec<(String, FileMeta)> = Vec::new();
            let mut last_publish = Instant::now();
            for entry in path_rx {
                fresh.push(entry);
                // stream walk progress so the status gauge climbs even when a
                // cached index is already searchable; the on-screen "indexed"
                // count is only touched on a cold start (progress marks that)
                if fresh.len().is_multiple_of(8192)
                    && last_publish.elapsed() > Duration::from_millis(250)
                {
                    last_publish = Instant::now();
                    let _ = indexer_tx.send(Msg::IndexProgress {
                        count: fresh.len(),
                        expected,
                    });
                }
            }
            let _ = walk_thread.join();
            fresh.sort_unstable_by(walker::mtime_cmp);
            let _ = indexer_tx.send(Msg::IndexSnapshot {
                store: Arc::new(PathStore::from_entries(&fresh)),
                indexing: false,
            });
            let _ = index::save(&fresh, &cache_path);
            if let Some((_watcher, event_rx)) = watcher {
                // _watcher must stay alive for events to keep flowing
                watch_loop(&event_rx, &excludes, &cache_path, &indexer_tx, fresh);
            }
        });

        let frecency = Frecency::load(history_path);
        let boosts = Arc::new(frecency.boosts(unix_now()));
        Engine {
            msg_rx,
            msg_tx,
            job_tx,
            store: Arc::new(PathStore::empty()),
            results: Vec::new(),
            filename_results: Vec::new(),
            semantic_results: Vec::new(),
            status: EngineStatus {
                indexing: true,
                ..Default::default()
            },
            mode: Mode::Fuzzy,
            generation: 0,
            query: String::new(),
            strong: 0,
            max_content_filesize,
            pdf_cache,
            filters: Filters::default(),
            pending_content: None,
            content_cancel: None,
            pending_semantic: None,
            sem_tx: None,
            frecency: Some(frecency),
            boosts,
            quiet: Arc::new(crate::quiet::Quiet::new(quiet_patterns)),
            unified,
            unified_strong: 0,
            filter: false,
        }
    }

    /// A filter-mode engine over arbitrary stdin lines: same matcher, no
    /// indexer, watcher, content or semantic machinery.
    pub fn from_lines(lines: Vec<String>) -> Engine {
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let (job_tx, job_rx) = mpsc::channel::<FilenameJob>();
        spawn_search_worker(job_rx, msg_tx.clone());
        // entries in INPUT order (no recency sort): each line is a "path"
        let entries: Vec<(String, FileMeta)> = lines
            .into_iter()
            .map(|l| (l, FileMeta::default()))
            .collect();
        let store = Arc::new(PathStore::from_entries(&entries));
        // publish the store so the first tick populates results immediately
        let _ = msg_tx.send(Msg::IndexSnapshot {
            store: store.clone(),
            indexing: false,
        });
        // no history for filter mode: an empty boost map, nothing persisted
        // (record_open is never called here)
        let boosts = Arc::new(HashMap::new());
        Engine {
            msg_rx,
            msg_tx,
            job_tx,
            store,
            results: Vec::new(),
            filename_results: Vec::new(),
            semantic_results: Vec::new(),
            status: EngineStatus {
                indexing: false,
                ..Default::default()
            },
            mode: Mode::Fuzzy,
            generation: 0,
            query: String::new(),
            strong: 0,
            max_content_filesize: 0,
            pdf_cache: PathBuf::new(),
            filters: Filters::default(),
            pending_content: None,
            content_cancel: None,
            pending_semantic: None,
            sem_tx: None,
            frecency: None,
            boosts,
            // stdin lines are whatever the pipe says they are — no demotion
            quiet: Arc::new(crate::quiet::Quiet::new(Vec::new())),
            unified: false,
            unified_strong: 0,
            filter: true,
        }
    }

    /// True when the engine filters piped stdin lines (`--filter`).
    pub fn is_filter(&self) -> bool {
        self.filter
    }

    /// Records that `path` was opened, boosting it in future rankings.
    pub fn record_open(&mut self, path: &str) {
        if let Some(frecency) = self.frecency.as_mut() {
            frecency.record(path);
            self.boosts = Arc::new(frecency.boosts(unix_now()));
        }
    }

    fn is_unified_query(&self) -> bool {
        self.unified && self.mode == Mode::Fuzzy && !self.query.is_empty()
    }

    /// Rebuilds the visible list from both source rankings. The fold boundary
    /// is the position of the last filename strong match, so filename scoring
    /// still controls weaker-match folding while semantic-only rows retain
    /// their RRF order within the visible list.
    fn rebuild_unified(&mut self) {
        self.results = merge_unified_results(&self.filename_results, &self.semantic_results);
        self.status.matches = self.results.len();
        if self.strong == 0 {
            // No filename match means every merged row is semantic-only;
            // there is no filename weak tail to fold away.
            self.unified_strong = self.results.len();
            return;
        }
        let strong_paths: HashSet<&str> = self
            .filename_results
            .iter()
            .take(self.strong)
            .map(|row| row.path.as_str())
            .collect();
        self.unified_strong = self
            .results
            .iter()
            .enumerate()
            .filter(|(_, row)| strong_paths.contains(row.path.as_str()))
            .map(|(i, _)| i + 1)
            .max()
            .unwrap_or(0);
    }

    pub fn set_query(&mut self, input: &str, regex_mode: bool) {
        if self.filter {
            // filter mode: no `>`/`?`/prefix parsing — those are ordinary
            // text; only the regex toggle and the ext:/path: filters apply
            let mode = if regex_mode { Mode::Regex } else { Mode::Fuzzy };
            let (query_filters, pattern) = filters::parse(input, unix_now());
            let query = if query_filters.is_empty() {
                input.to_string()
            } else {
                pattern
            };
            self.filters = query_filters;
            self.generation += 1;
            self.mode = mode;
            self.query = query.clone();
            self.status.error = None;
            self.pending_content = None;
            self.pending_semantic = None;
            self.dispatch_filename();
            return;
        }
        let (mode, query) = parse_query(input, regex_mode);
        if mode == Mode::Calc {
            // the calculator is synchronous and takes the raw expression —
            // no filter tokens, no worker round-trip
            self.generation += 1;
            self.mode = mode;
            self.query = query.clone();
            self.status.error = None;
            self.cancel_content();
            self.pending_content = None;
            self.pending_semantic = None;
            self.results = match (query.is_empty(), crate::calc::eval(&query)) {
                (false, Some(v)) => vec![ResultRow {
                    path: crate::calc::format_result(v),
                    line_number: None,
                    line: Some(format!("{query} =")),
                    recent_open: false,
                    meta: None,
                    score: None,
                }],
                _ => Vec::new(),
            };
            self.status.matches = self.results.len();
            return;
        }
        let (query_filters, pattern) = filters::parse(&query, unix_now());
        // rejoin preserves regex/content patterns without filter tokens
        let query = if query_filters.is_empty() {
            query
        } else {
            pattern
        };
        self.filters = query_filters;
        self.generation += 1;
        self.mode = mode;
        self.query = query.clone();
        self.status.error = None;
        self.cancel_content();
        self.filename_results.clear();
        self.semantic_results.clear();
        self.unified_strong = 0;
        match mode {
            Mode::Content => {
                self.results.clear();
                self.status.matches = 0;
                self.pending_semantic = None;
                if !query.is_empty() {
                    self.pending_content = Some((query, Instant::now()));
                }
            }
            Mode::Semantic => {
                self.results.clear();
                self.status.matches = 0;
                self.pending_content = None;
                if !query.is_empty() {
                    self.pending_semantic = Some((query, Instant::now()));
                }
            }
            Mode::Fuzzy => {
                self.pending_content = None;
                self.pending_semantic =
                    if !query.is_empty() && self.unified && semantic_store_available() {
                        Some((query, Instant::now()))
                    } else {
                        None
                    };
                self.dispatch_filename();
            }
            Mode::Regex | Mode::Calc => {
                self.pending_content = None;
                self.pending_semantic = None;
                self.dispatch_filename();
            }
        }
    }

    pub fn tick(&mut self) {
        self.fire_due_content_search();
        self.fire_due_semantic_search();
        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                Msg::IndexSnapshot { store, indexing } => {
                    self.store = store;
                    self.status.indexed = self.store.len();
                    self.status.indexing = indexing;
                    if !indexing {
                        // the startup walk (fresh or re-walk) is done
                        self.status.walk = None;
                    }
                    if matches!(self.mode, Mode::Fuzzy | Mode::Regex) {
                        self.generation += 1;
                        self.dispatch_filename();
                        if self.is_unified_query() && semantic_store_available() {
                            // The index update invalidates the generation of a
                            // pending semantic response, so debounce it again.
                            self.pending_semantic = Some((self.query.clone(), Instant::now()));
                        }
                    }
                }
                Msg::IndexProgress { count, expected } => {
                    self.status.walk = Some((count, expected));
                    // cold start: keep the indexed count climbing so the UI
                    // isn't stuck at 0 while the store is still empty
                    if expected.is_none() && self.status.indexing {
                        self.status.indexed = count;
                    }
                }
                Msg::IndexError { error } => {
                    // one-time setup failure: keep whatever snapshot is live
                    // (possibly none) and stop the "indexing" spinner
                    self.status.indexing = false;
                    self.status.walk = None;
                    self.status.error = Some(error);
                }
                Msg::FilenameResults {
                    generation,
                    indices,
                    strong,
                    error,
                } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.filename_results = indices
                        .into_iter()
                        .filter(|&i| i < self.store.len())
                        .map(|i| {
                            let path = self.store.get(i).to_string();
                            let recent_open = self.boosts.contains_key(&path);
                            ResultRow {
                                path,
                                line_number: None,
                                line: None,
                                recent_open,
                                meta: Some(self.store.meta(i)),
                                score: None,
                            }
                        })
                        .collect();
                    self.strong = strong.min(self.filename_results.len());
                    if self.is_unified_query() {
                        self.rebuild_unified();
                    } else {
                        self.results = self.filename_results.clone();
                        self.unified_strong = self.strong;
                        self.status.matches = self.results.len();
                    }
                    self.status.error = error;
                }
                Msg::ContentHit { generation, hit } => {
                    if generation != self.generation || self.results.len() >= CONTENT_LIMIT {
                        continue;
                    }
                    self.results.push(ResultRow {
                        path: hit.path,
                        line_number: Some(hit.line_number),
                        line: Some(hit.line),
                        recent_open: false,
                        meta: None,
                        score: None,
                    });
                    self.status.matches = self.results.len();
                    if self.results.len() >= CONTENT_LIMIT {
                        self.cancel_content();
                    }
                }
                Msg::ContentError { generation, error } => {
                    // an invalid pattern produced no hits: show the reason
                    // for the matching generation only
                    if generation != self.generation {
                        continue;
                    }
                    self.status.error = Some(error);
                }
                Msg::SemanticResults {
                    generation,
                    rows,
                    error,
                } => {
                    if generation != self.generation {
                        continue;
                    }
                    let f = &self.filters;
                    let rows: Vec<ResultRow> = rows
                        .into_iter()
                        .filter(|r| f.is_empty() || f.matches(&r.path))
                        .collect();
                    if self.is_unified_query() {
                        self.semantic_results = rows;
                        self.rebuild_unified();
                        // Unified search is best-effort: a missing model or
                        // broken store must not change bare filename behavior.
                    } else if self.mode == Mode::Semantic {
                        self.results = rows;
                        self.status.matches = self.results.len();
                        self.status.error = error;
                    }
                }
            }
        }
    }

    pub fn results(&self) -> &[ResultRow] {
        &self.results
    }

    /// How many rows remain above the filename relative score floor (semantic
    /// rows are all visible when there is no filename match; content/semantic
    /// modes have no score floor).
    pub fn strong_count(&self) -> usize {
        match self.mode {
            Mode::Fuzzy if self.is_unified_query() => self.unified_strong,
            Mode::Fuzzy | Mode::Regex => self.strong,
            _ => self.results.len(),
        }
    }

    /// Test-only: place filename rows directly so UI states can be rendered
    /// without waiting on worker threads.
    #[doc(hidden)]
    pub fn inject_results_for_test(&mut self, rows: Vec<ResultRow>) {
        self.filename_results = rows.clone();
        self.results = rows;
    }

    /// Test-only: enqueue semantic rows as if they came from the worker.
    #[doc(hidden)]
    pub fn inject_semantic_results_for_test(&mut self, rows: Vec<ResultRow>) {
        self.inject_semantic_results_for_test_at(self.generation, rows);
    }

    /// Test-only: enqueue semantic rows with an explicit generation, allowing
    /// stale-worker rejection to be exercised without a real embedder.
    #[doc(hidden)]
    pub fn inject_semantic_results_for_test_at(&mut self, generation: u64, rows: Vec<ResultRow>) {
        let _ = self.msg_tx.send(Msg::SemanticResults {
            generation,
            rows,
            error: None,
        });
    }

    pub fn status(&self) -> EngineStatus {
        self.status.clone()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    fn dispatch_filename(&self) {
        let mode = match self.mode {
            Mode::Regex => FilenameMode::Regex,
            _ => FilenameMode::Fuzzy,
        };
        let _ = self.job_tx.send(FilenameJob {
            generation: self.generation,
            query: self.query.clone(),
            mode,
            store: self.store.clone(),
            boosts: self.boosts.clone(),
            filters: self.filters.clone(),
            quiet: self.quiet.clone(),
        });
    }

    fn fire_due_content_search(&mut self) {
        let due = self
            .pending_content
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= CONTENT_DEBOUNCE);
        if !due {
            return;
        }
        let (pattern, _) = self.pending_content.take().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        self.content_cancel = Some(cancel.clone());
        // scope the grep with any ext:/path: filters from the query
        // candidates are indices into the store; the store Arc is passed
        // along so the search thread resolves paths without materializing
        // a full Vec<String> of path clones per query
        let f = &self.filters;
        let indices: Vec<usize> = (0..self.store.len())
            .filter(|&i| {
                f.is_empty()
                    || (f.matches(self.store.get(i)) && f.matches_meta(&self.store.meta(i)))
            })
            .collect();
        let store = self.store.clone();
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let max = self.max_content_filesize;
        let pdf_cache = self.pdf_cache.clone();
        std::thread::spawn(move || {
            let (hit_tx, hit_rx) = mpsc::channel::<ContentMatch>();
            let search_cancel = cancel.clone();
            let pattern2 = pattern.clone();
            let searcher = std::thread::spawn(move || {
                content::search(
                    &indices,
                    |i| store.get(i),
                    &pattern2,
                    max,
                    &pdf_cache,
                    &search_cancel,
                    &hit_tx,
                )
            });
            for hit in hit_rx {
                if tx.send(Msg::ContentHit { generation, hit }).is_err() {
                    cancel.store(true, Ordering::Relaxed);
                    break;
                }
            }
            // an invalid content pattern surfaces as a typed error so the
            // filename channel stays pure filename results
            if let Ok(Err(e)) = searcher.join() {
                let _ = tx.send(Msg::ContentError {
                    generation,
                    error: format!("invalid pattern: {e}"),
                });
            }
        });
    }

    fn cancel_content(&mut self) {
        if let Some(flag) = self.content_cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    fn fire_due_semantic_search(&mut self) {
        let due = self
            .pending_semantic
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= CONTENT_DEBOUNCE);
        if !due {
            return;
        }
        let (query, _) = self.pending_semantic.take().unwrap();
        let tx = self.ensure_semantic_worker();
        let _ = tx.send(SemJob {
            generation: self.generation,
            query,
            filters: self.filters.clone(),
        });
    }

    /// One worker for the whole session: the embedding model loads once, on
    /// the first `?` query, and stays warm. The store is reloaded whenever
    /// its file's mtime changes (checked at most once per second per query),
    /// so an index built while the UI is open is picked up on the next
    /// query — no restart needed.
    fn ensure_semantic_worker(&mut self) -> Sender<SemJob> {
        if let Some(tx) = &self.sem_tx {
            return tx.clone();
        }
        let pdf_cache = self.pdf_cache.clone();
        let (tx, rx) = mpsc::channel::<SemJob>();
        let msg_tx = self.msg_tx.clone();
        std::thread::spawn(move || {
            // a successfully built embedder is kept across reloads (model
            // loading is expensive); creation *failures* are not cached —
            // the next job retries
            let mut spare: Option<Box<dyn sem::Embedder + Send>> = None;
            let mut ready: Option<(Box<dyn sem::Embedder + Send>, sem::SemStore)> = None;
            // mtime of the store file when `ready` was loaded; a different
            // mtime means another process rebuilt or migrated the index
            let mut loaded_stamp: Option<std::time::SystemTime> = None;
            let mut last_check = Instant::now();
            while let Ok(mut job) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    job = newer;
                }
                // liveness: if the store file changed on disk since it was
                // loaded (checked at most once per second), drop `ready` so
                // it is rebuilt below and new/changed docs are picked up
                // without a restart
                if ready.is_some() && last_check.elapsed() >= Duration::from_secs(1) {
                    last_check = Instant::now();
                    if cache_mtime(&sem::default_store_path()) != loaded_stamp {
                        ready = None;
                    }
                }
                let mut broken: Option<String> = None;
                if ready.is_none() {
                    let embedder = match spare.take() {
                        Some(e) => Ok(e),
                        None => sem::make_embedder(),
                    };
                    match embedder {
                        Ok(e) => match sem::SemStore::load(&sem::default_store_path()) {
                            Some(s) if s.dim as usize == e.dim() => {
                                loaded_stamp = cache_mtime(&sem::default_store_path());
                                ready = Some((e, s));
                            }
                            Some(_) => {
                                spare = Some(e);
                                broken = Some(
                                    "semantic index is from another model — \
                                     rerun fsearch --index-semantic"
                                        .to_string(),
                                );
                            }
                            None => {
                                spare = Some(e);
                                broken = Some(
                                    "no semantic index yet — run fsearch --index-semantic"
                                        .to_string(),
                                );
                            }
                        },
                        Err(e) => broken = Some(e),
                    }
                }
                let msg = match (&mut ready, &broken) {
                    (Some((embedder, store)), _) => {
                        match embedder.embed(std::slice::from_ref(&job.query)) {
                            Ok(qv) => {
                                // over-fetch when metadata/path filters are
                                // active (they are applied after ranking) so
                                // filtering doesn't drop the recall count
                                let fetch = if job.filters.is_empty() {
                                    SEMANTIC_LIMIT
                                } else {
                                    SEMANTIC_LIMIT * 4
                                };
                                let rows: Vec<ResultRow> = store
                                    .query(&qv[0], fetch)
                                    .into_iter()
                                    .filter(|h| {
                                        let doc = &store.docs[h.doc];
                                        job.filters.is_empty()
                                            || (job.filters.matches(&doc.path)
                                                && job.filters.matches_meta(&FileMeta {
                                                    mtime: doc.mtime,
                                                    size: doc.size,
                                                }))
                                    })
                                    .take(SEMANTIC_LIMIT)
                                    .enumerate()
                                    .map(|(i, h)| {
                                        let score = h.score.clamp(0.0, 1.0);
                                        let line = if i < 24 {
                                            snippet_line(
                                                &store.docs[h.doc].path,
                                                h.line_start as u64,
                                                &pdf_cache,
                                            )
                                            .or(Some(format!("{:.0}% match", score * 100.0)))
                                        } else {
                                            Some(format!("{:.0}% match", score * 100.0))
                                        };
                                        ResultRow {
                                            path: store.docs[h.doc].path.clone(),
                                            line_number: Some(h.line_start as u64),
                                            line,
                                            recent_open: false,
                                            meta: None,
                                            score: Some(score),
                                        }
                                    })
                                    .collect();
                                Msg::SemanticResults {
                                    generation: job.generation,
                                    rows,
                                    error: None,
                                }
                            }
                            Err(e) => Msg::SemanticResults {
                                generation: job.generation,
                                rows: Vec::new(),
                                error: Some(e),
                            },
                        }
                    }
                    _ => Msg::SemanticResults {
                        generation: job.generation,
                        rows: Vec::new(),
                        error: broken.clone(),
                    },
                };
                if msg_tx.send(msg).is_err() {
                    return;
                }
            }
        });
        self.sem_tx = Some(tx.clone());
        tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds one debounced event for `path` into `watch_loop` and returns
    /// the paths of the snapshot it publishes.
    fn watch_snapshot(
        current: Vec<(String, FileMeta)>,
        touched: std::path::PathBuf,
    ) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        event_tx
            .send(Ok(
                notify::Event::new(notify::EventKind::Other).add_path(touched)
            ))
            .unwrap();
        drop(event_tx); // ends the loop after the burst is folded
        let (index_tx, index_rx) = mpsc::channel();
        let excludes = globset::GlobSetBuilder::new().build().unwrap();
        watch_loop(
            &event_rx,
            &excludes,
            &dir.path().join("index.bin"),
            &index_tx,
            current,
        );
        match index_rx.recv().unwrap() {
            Msg::IndexSnapshot { store, .. } => store.iter().map(str::to_string).collect(),
            _ => panic!("expected an index snapshot"),
        }
    }

    #[test]
    fn watch_loop_prunes_children_of_dir_replaced_by_file() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("thing");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("old-child.txt"), "x").unwrap();
        let sub_str = sub.to_string_lossy().into_owned();
        let current = vec![
            (format!("{sub_str}/"), FileMeta::default()),
            (format!("{sub_str}/old-child.txt"), FileMeta::default()),
            (
                format!("{}", dir.path().join("keeper.txt").display()),
                FileMeta::default(),
            ),
        ];
        // deleted and replaced by a same-named file inside one burst
        std::fs::remove_dir_all(&sub).unwrap();
        std::fs::write(&sub, "now a file").unwrap();

        let paths = watch_snapshot(current, sub);
        // only the replacement file survives from the old subtree, plus the
        // untouched sibling
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&sub_str));
        assert!(paths.contains(&dir.path().join("keeper.txt").to_string_lossy().into_owned()));
    }

    #[test]
    fn watch_loop_recreated_dir_replaces_stale_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("proj");
        std::fs::create_dir_all(sub.join("old")).unwrap();
        std::fs::write(sub.join("old/gone.txt"), "x").unwrap();
        let sub_str = sub.to_string_lossy().into_owned();
        let current = vec![
            (format!("{sub_str}/"), FileMeta::default()),
            (format!("{sub_str}/old/gone.txt"), FileMeta::default()),
        ];
        // recreated with different contents inside one burst
        std::fs::remove_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("fresh.txt"), "x").unwrap();

        let paths = watch_snapshot(current, sub.clone());
        let mut sorted = paths;
        sorted.sort();
        assert_eq!(
            sorted,
            vec![format!("{sub_str}/"), format!("{sub_str}/fresh.txt"),]
        );
    }

    #[test]
    fn parse_plain_is_fuzzy() {
        assert_eq!(
            parse_query("notes", false),
            (Mode::Fuzzy, "notes".to_string())
        );
    }

    #[test]
    fn parse_respects_regex_toggle() {
        assert_eq!(
            parse_query(r"\.pdf$", true),
            (Mode::Regex, r"\.pdf$".to_string())
        );
    }

    #[test]
    fn parse_gt_prefix_is_content() {
        assert_eq!(
            parse_query("> hello world", false),
            (Mode::Content, "hello world".to_string())
        );
        // regex toggle does not override content mode
        assert_eq!(parse_query(">x", true), (Mode::Content, "x".to_string()));
    }

    #[test]
    fn parse_bare_gt_is_empty_content() {
        assert_eq!(parse_query(">", false), (Mode::Content, String::new()));
    }

    #[test]
    fn parse_equals_prefix_is_calc() {
        assert_eq!(
            parse_query("= 2*(3+4)", false),
            (Mode::Calc, "2*(3+4)".to_string())
        );
        // regex toggle does not override calc mode
        assert_eq!(parse_query("=1+1", true), (Mode::Calc, "1+1".to_string()));
        assert_eq!(parse_query("=", false), (Mode::Calc, String::new()));
    }

    #[test]
    fn parse_question_prefix_is_semantic() {
        assert_eq!(
            parse_query("? essays about patience", false),
            (Mode::Semantic, "essays about patience".to_string())
        );
        // regex toggle does not override semantic mode
        assert_eq!(parse_query("?x", true), (Mode::Semantic, "x".to_string()));
        assert_eq!(parse_query("?", false), (Mode::Semantic, String::new()));
    }

    #[test]
    fn snippet_line_returns_trimmed_capped_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.txt");
        std::fs::write(&path, "first line\n  second line padded  \nthird line\n").unwrap();
        let p = path.to_str().unwrap();
        let cache = std::path::Path::new("/nonexistent/pdftext");
        // 1-based line lookup, trimmed; not a pdf so the cache path is unused
        assert_eq!(
            snippet_line(p, 2, cache),
            Some("second line padded".to_string())
        );
        // out-of-range line yields None
        assert_eq!(snippet_line(p, 99, cache), None);
    }

    fn wait_for(engine: &mut Engine, pred: impl Fn(&Engine) -> bool) {
        for _ in 0..200 {
            engine.tick();
            if pred(engine) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn from_lines_keeps_input_order_for_empty_query() {
        let lines = vec![
            "git commit -m fix/thing".to_string(),
            "cargo build --release".to_string(),
            "alpha beta".to_string(),
        ];
        let mut engine = Engine::from_lines(lines);
        assert!(engine.is_filter());
        wait_for(&mut engine, |e| e.results().len() >= 3);
        let paths: Vec<&str> = engine.results().iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "git commit -m fix/thing",
                "cargo build --release",
                "alpha beta"
            ]
        );
    }

    #[test]
    fn from_lines_fuzzy_query_narrows_and_ranks() {
        let lines = vec![
            "cargo build".to_string(),
            "cargo test".to_string(),
            "git commit fix".to_string(),
            "alpha".to_string(),
        ];
        let mut engine = Engine::from_lines(lines);
        wait_for(&mut engine, |e| e.results().len() >= 4);
        engine.set_query("cargo", false);
        // wait until the new-generation fuzzy results replace the empty-query
        // snapshot (all remaining matches mention cargo)
        wait_for(&mut engine, |e| {
            !e.results().is_empty() && e.results().iter().all(|r| r.path.contains("cargo"))
        });
        let paths: Vec<&str> = engine.results().iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["cargo build", "cargo test"]);
        assert!(!paths.contains(&"alpha"));
        assert!(!paths.contains(&"git commit fix"));
    }

    #[test]
    fn filter_mode_skips_prefix_parsing() {
        let mut engine = Engine::from_lines(vec!["hello".to_string()]);
        // a leading `>` is ordinary text, not content mode
        engine.set_query("> x", false);
        assert_eq!(engine.mode(), Mode::Fuzzy);
        // the regex toggle still works
        engine.set_query("? y", true);
        assert_eq!(engine.mode(), Mode::Regex);
    }

    fn unified_test_engine(unified: bool) -> (Engine, tempfile::TempDir, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("topic.txt"), "topic").unwrap();
        let aux = tempfile::tempdir().unwrap();
        let config = Config {
            roots: vec![root.path().to_path_buf()],
            excludes: Vec::new(),
            index_apps: false,
            quiet: Vec::new(),
            unified,
            ..Config::default()
        };
        (
            Engine::new(
                config,
                aux.path().join("index.bin"),
                aux.path().join("history"),
            ),
            root,
            aux,
        )
    }

    fn test_row(path: &str, semantic: bool) -> ResultRow {
        ResultRow {
            path: path.to_string(),
            line_number: semantic.then_some(7),
            line: semantic.then(|| "semantic context".to_string()),
            recent_open: false,
            meta: (!semantic).then_some(FileMeta { mtime: 7, size: 11 }),
            score: semantic.then_some(0.8),
        }
    }

    fn ready_for_unified_query(unified: bool) -> (Engine, tempfile::TempDir, tempfile::TempDir) {
        let (mut engine, root, aux) = unified_test_engine(unified);
        wait_for(&mut engine, |e| {
            !e.status().indexing && e.status().indexed == 1
        });
        engine.set_query("topic", false);
        wait_for(&mut engine, |e| !e.results().is_empty());
        (engine, root, aux)
    }

    #[test]
    fn unified_injected_rows_use_rrf_and_merge_context() {
        let (mut engine, _root, _aux) = ready_for_unified_query(true);
        engine.inject_results_for_test(vec![
            test_row("/filename-a", false),
            test_row("/both", false),
            test_row("/filename-c", false),
        ]);
        engine.inject_semantic_results_for_test(vec![
            test_row("/semantic-only", true),
            test_row("/both", true),
        ]);
        engine.tick();

        let paths: Vec<&str> = engine
            .results()
            .iter()
            .map(|row| row.path.as_str())
            .collect();
        // /both has two rank-2 contributions; the remaining rows are ordered
        // by their one-list RRF scores, with deterministic source-rank ties.
        assert_eq!(
            paths,
            ["/both", "/filename-a", "/semantic-only", "/filename-c"]
        );
        // The filename strong row is second after fusion, so the fold keeps
        // it (and the semantic row ahead of it) visible.
        assert_eq!(engine.strong_count(), 2);
        let both = &engine.results()[0];
        assert_eq!(both.meta, Some(FileMeta { mtime: 7, size: 11 }));
        assert_eq!(both.line_number, Some(7));
        assert_eq!(both.line.as_deref(), Some("semantic context"));
        assert_eq!(both.score, Some(0.8));
    }

    #[test]
    fn unified_drops_stale_injected_semantic_rows() {
        let (mut engine, _root, _aux) = ready_for_unified_query(true);
        engine.inject_semantic_results_for_test(vec![test_row("/stale", true)]);
        engine.set_query("newer", false);
        engine.tick();
        assert!(!engine.results().iter().any(|row| row.path == "/stale"));
    }

    #[test]
    fn unified_false_ignores_semantic_rows() {
        let (mut engine, _root, _aux) = ready_for_unified_query(false);
        let before: Vec<String> = engine
            .results()
            .iter()
            .map(|row| row.path.clone())
            .collect();
        engine.inject_semantic_results_for_test(vec![test_row("/semantic-only", true)]);
        engine.tick();
        let after: Vec<String> = engine
            .results()
            .iter()
            .map(|row| row.path.clone())
            .collect();
        assert_eq!(after, before);
    }

    #[test]
    fn unified_semantic_only_rows_are_not_folded() {
        let (mut engine, _root, _aux) = ready_for_unified_query(true);
        engine.inject_results_for_test(Vec::new());
        // Simulate the filename worker returning no matches.
        engine.strong = 0;
        engine.inject_semantic_results_for_test(vec![test_row("/semantic-only", true)]);
        engine.tick();
        assert_eq!(engine.results().len(), 1);
        assert_eq!(engine.strong_count(), 1);
        assert_eq!(engine.results()[0].path, "/semantic-only");
    }
}
