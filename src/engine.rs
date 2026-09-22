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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, mpsc};
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineStatus {
    pub indexed: usize,
    pub indexing: bool,
    /// Current-query work, including the debounce window and streamed content.
    pub searching: bool,
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
    /// Setup or runtime failure (bad excludes glob, watcher error). Keeps
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
    ContentDone {
        generation: u64,
    },
    SemanticResults {
        generation: u64,
        rows: Vec<ResultRow>,
        error: Option<String>,
    },
}

type WarmSemantic = (Box<dyn sem::Embedder + Send>, sem::SemStore);

fn release_semantic_store(
    ready: &mut Option<WarmSemantic>,
) -> Option<Box<dyn sem::Embedder + Send>> {
    ready.take().map(|(embedder, _)| embedder)
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
        use std::io::Read;
        let file = crate::util::open_regular_file(std::path::Path::new(path)).ok()?;
        if file.metadata().ok()?.len() > sem::MAX_SEMANTIC_BYTES {
            return None;
        }
        let mut text = String::new();
        file.take(sem::MAX_SEMANTIC_BYTES + 1)
            .read_to_string(&mut text)
            .ok()?;
        (text.len() as u64 <= sem::MAX_SEMANTIC_BYTES).then_some(text)
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
    lines: bool,
}

type PendingSnapshot = Arc<Mutex<Option<(Arc<PathStore>, bool)>>>;

/// Retain only the newest unpublished snapshot when the UI is busy.
struct IndexPublisher {
    tx: Sender<Msg>,
    snapshot: PendingSnapshot,
    /// Incomplete startup baselines must never overwrite a usable disk cache.
    save_allowed: bool,
}

impl IndexPublisher {
    fn send(&self, msg: Msg) -> Result<(), mpsc::SendError<Msg>> {
        match msg {
            Msg::IndexSnapshot { store, indexing } => {
                *self.snapshot.lock().unwrap() = Some((store, indexing));
                Ok(())
            }
            msg @ Msg::IndexError { .. } => {
                // An error may be consumed before the pending cached snapshot;
                // do not let that late snapshot revive the indexing spinner.
                if let Some((_, indexing)) = self.snapshot.lock().unwrap().as_mut() {
                    *indexing = false;
                }
                self.tx.send(msg)
            }
            msg => self.tx.send(msg),
        }
    }
}

pub struct Engine {
    msg_rx: Receiver<Msg>,
    snapshot: PendingSnapshot,
    shutdown: Arc<AtomicBool>,
    indexer: Option<std::thread::JoinHandle<()>>,
    content_rx: Option<Receiver<Msg>>,
    msg_tx: Sender<Msg>,
    job_tx: Sender<FilenameJob>,
    store: Arc<PathStore>,
    results: Vec<ResultRow>,
    /// Source rankings retained so a delayed semantic response can be merged
    /// without losing the instant filename result set.
    filename_results: Vec<ResultRow>,
    semantic_results: Vec<ResultRow>,
    status: EngineStatus,
    /// Index errors survive successful searches so watcher failures are visible.
    index_error: Option<String>,
    mode: Mode,
    generation: u64,
    current_generation: Arc<AtomicU64>,
    query: String,
    strong: usize,
    max_content_filesize: u64,
    pdf_cache: PathBuf,
    filters: Filters,
    pending_content: Option<(String, Instant)>,
    content_cancel: Option<Arc<AtomicBool>>,
    pending_semantic: Option<(String, Instant)>,
    filename_running: bool,
    semantic_running: bool,
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

fn walk_failure(result: std::thread::Result<walker::WalkStats>) -> Option<String> {
    match result {
        Ok(stats) if stats.complete() => None,
        Ok(stats) => Some(format!("index walk incomplete ({} errors)", stats.skipped)),
        Err(_) => Some("index walk failed".to_string()),
    }
}

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
    let mut watching = 0usize;
    for root in roots {
        // Never follow a configured symlink root that the walker ignores.
        if std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        if watcher
            .watch(root, notify::RecursiveMode::Recursive)
            .is_err()
        {
            failed += 1;
        } else {
            watching += 1;
        }
    }
    // a watcher that watches nothing is worse than none: it only burns the
    // event thread; callers surface the failure count instead
    ((watching > 0).then_some((watcher, event_rx)), failed)
}

#[derive(Default)]
struct WatchBatch {
    touched: HashSet<PathBuf>,
    rescan: bool,
    errors: Vec<String>,
}

impl WatchBatch {
    fn absorb(&mut self, result: notify::Result<notify::Event>) {
        match result {
            Ok(event) => {
                self.rescan |= event.need_rescan();
                // Reading previews/content must not trigger an indexing loop.
                if !matches!(event.kind, notify::EventKind::Access(_)) {
                    self.touched.extend(event.paths);
                }
            }
            Err(error) => {
                self.rescan = true;
                self.errors.push(format!("live update error: {error}"));
            }
        }
    }
}

/// An ancestor refresh already replaces its whole subtree. Remove duplicate
/// and nested paths before walking, including overlapping configured roots.
fn disjoint_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let paths: HashSet<PathBuf> = paths.into_iter().collect();
    let mut disjoint: Vec<_> = paths
        .iter()
        .filter(|path| !path.ancestors().skip(1).any(|p| paths.contains(p)))
        .cloned()
        .collect();
    disjoint.sort();
    disjoint
}

fn is_app_entry(path: &std::path::Path, app_dirs: &[PathBuf]) -> bool {
    path.extension().is_some_and(|e| e == "app")
        && app_dirs.iter().any(|root| {
            path.strip_prefix(root)
                .is_ok_and(|p| (1..=2).contains(&p.components().count()))
        })
}

fn sort_unique_entries(entries: &mut Vec<(String, FileMeta)>) {
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.mtime.cmp(&a.1.mtime)));
    entries.dedup_by(|a, b| a.0 == b.0);
    entries.sort_unstable_by(walker::mtime_cmp);
}

/// Folds filesystem events into newest-first snapshots. Lost events rebuild
/// all configured roots, using the same excludes and app policy as startup.
#[allow(clippy::too_many_arguments)]
fn watch_loop(
    event_rx: &Receiver<notify::Result<notify::Event>>,
    excludes: &globset::GlobSet,
    cache_path: &std::path::Path,
    indexer_tx: &IndexPublisher,
    mut current: Vec<(String, FileMeta)>,
    roots: &[PathBuf],
    app_dirs: &[PathBuf],
    shutdown: &AtomicBool,
) {
    let roots = disjoint_paths(roots.iter().cloned());
    let mut last_save = Instant::now();
    // single-writer courtesy: remember the cache state we produced; if the
    // file changes underneath us (another instance, a --reindex), stop
    // saving so we never clobber fresher data with our older snapshot
    let mut our_stamp = cache_mtime(cache_path);
    loop {
        // block until something happens, then debounce-collect the burst
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        let first = match event_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let mut batch = WatchBatch::default();
        batch.absorb(first);
        let deadline = Instant::now() + WATCH_DEBOUNCE;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            match event_rx.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(res) => batch.absorb(res),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        for error in batch.errors {
            if indexer_tx.send(Msg::IndexError { error }).is_err() {
                return;
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
        let app_refresh = !batch.rescan
            && batch
                .touched
                .iter()
                .any(|p| app_dirs.iter().any(|d| p.starts_with(d)));
        if app_refresh {
            let (apps, stats) = walker::collect_apps(app_dirs, shutdown);
            if stats.complete() {
                for (path, _) in &current {
                    if !path.ends_with('/') && is_app_entry(std::path::Path::new(path), app_dirs) {
                        gone.insert(path.clone());
                    }
                }
                for entry in apps {
                    gone.insert(entry.0.clone());
                    fronts.push(entry);
                }
            } else {
                let _ = indexer_tx.send(Msg::IndexError {
                    error: "application walk incomplete; retaining previous entries".into(),
                });
            }
            // Bundle events refresh the file-like app entry, never its contents.
            batch.touched.retain(|p| {
                roots.iter().any(|r| p.starts_with(r))
                    && !p.ancestors().any(|a| is_app_entry(a, app_dirs))
            });
        }
        if batch.rescan {
            let (mut entries, mut stats) =
                walker::collect_cancellable(&roots, excludes, false, shutdown);
            let (apps, app_stats) = walker::collect_apps(app_dirs, shutdown);
            entries.extend(apps);
            stats.skipped += app_stats.skipped;
            stats.cancelled |= app_stats.cancelled;
            if !stats.complete() {
                let _ = indexer_tx.send(Msg::IndexError {
                    error: format!(
                        "live index walk incomplete ({} errors); retaining previous index",
                        stats.skipped
                    ),
                });
                continue;
            }
            sort_unique_entries(&mut entries);
            current = entries;
            batch.touched.clear();
        }
        // Events may name a child beneath a directory replaced with a symlink.
        // Re-stat ancestors too: symlink_metadata on the leaf alone would follow
        // that parent and introduce paths the walker never traverses.
        let touched = batch.touched.into_iter().map(|path| {
            path.ancestors()
                .find(|p| {
                    roots.iter().chain(app_dirs).any(|r| p.starts_with(r))
                        && std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
                })
                .map_or_else(|| path.clone(), std::path::Path::to_path_buf)
        });
        for path in disjoint_paths(touched) {
            if excludes.is_match(&path) {
                continue;
            }
            let s = path.to_string_lossy().into_owned();
            let std_meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => Some(meta),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    let _ = indexer_tx.send(Msg::IndexError {
                        error: format!("live index metadata: {error}"),
                    });
                    continue;
                }
            };
            if std_meta.as_ref().is_some_and(|m| m.is_file()) {
                // Replace stale metadata, using real mtime rather than event order.
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
            } else if std_meta.as_ref().is_some_and(|m| m.is_dir()) {
                // a directory appeared or changed: replace its prior state —
                // a recreated dir must not keep stale children — then index
                // what exists now
                let (entries, stats) = walker::collect_cancellable(
                    std::slice::from_ref(&path),
                    excludes,
                    false,
                    shutdown,
                );
                if !stats.complete() {
                    let _ = indexer_tx.send(Msg::IndexError {
                        error: format!(
                            "live subtree walk incomplete ({} errors); retaining previous entries",
                            stats.skipped
                        ),
                    });
                    continue;
                }
                gone_dir_prefixes.push(format!("{s}/"));
                gone.insert(s.clone());
                // collect_sorted skips the walk root, so re-add the dir
                // itself with fresh metadata
                let meta = FileMeta {
                    mtime: std_meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs() as i64),
                    size: 0,
                };
                if !roots.contains(&path) {
                    push_front(&mut fronts, &mut gone, (format!("{s}/"), meta));
                }
                for entry in entries {
                    push_front(&mut fronts, &mut gone, entry);
                }
            } else {
                // deleted: could have been a file or a whole directory
                gone_dir_prefixes.push(format!("{s}/"));
                gone.insert(s);
            }
        }
        if !batch.rescan && fronts.is_empty() && gone.is_empty() {
            continue;
        }
        // Sort only changes and merge by moving strings, not cloning the index.
        current.retain(|(p, _)| {
            !gone.contains(p) && !gone_dir_prefixes.iter().any(|d| p.starts_with(d))
        });
        sort_unique_entries(&mut fronts);
        let mut next = Vec::with_capacity(current.len() + fronts.len());
        let mut old = current.into_iter().peekable();
        let mut new = fronts.into_iter().peekable();
        while let (Some(a), Some(b)) = (old.peek(), new.peek()) {
            if walker::mtime_cmp(a, b).is_le() {
                next.push(old.next().unwrap());
            } else {
                next.push(new.next().unwrap());
            }
        }
        next.extend(old);
        next.extend(new);
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
        if indexer_tx.save_allowed && last_save.elapsed() >= WATCH_SAVE_EVERY {
            last_save = Instant::now();
            if cache_mtime(cache_path) == our_stamp {
                match index::save(&current, cache_path) {
                    Ok(()) => our_stamp = cache_mtime(cache_path),
                    Err(error) => {
                        let _ = indexer_tx.send(Msg::IndexError {
                            error: format!("saving live index: {error}"),
                        });
                    }
                }
            }
        }
    }
}

/// The single filename search worker: always process only the newest job.
fn spawn_search_worker(
    job_rx: Receiver<FilenameJob>,
    tx: Sender<Msg>,
    current_generation: Arc<AtomicU64>,
) {
    let worker_tx = tx;
    std::thread::spawn(move || {
        while let Ok(mut job) = job_rx.recv() {
            while let Ok(newer) = job_rx.try_recv() {
                job = newer;
            }
            let matched = if job.lines {
                matcher::search_lines_generation(
                    &job.store,
                    &job.query,
                    job.mode,
                    FILENAME_LIMIT,
                    &job.filters,
                    job.generation,
                    &current_generation,
                )
            } else {
                matcher::search_boosted_generation(
                    &job.store,
                    &job.query,
                    job.mode,
                    FILENAME_LIMIT,
                    &job.boosts,
                    &job.filters,
                    &job.quiet,
                    job.generation,
                    &current_generation,
                )
            };
            let (indices, strong, error) = match matched {
                Ok(Some(r)) => (r.indices, r.strong, None),
                Ok(None) => continue,
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
        let current_generation = Arc::new(AtomicU64::new(0));
        spawn_search_worker(job_rx, msg_tx.clone(), current_generation.clone());

        // indexer: cached paths first, then a checked fresh walk, then save
        let snapshot = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let mut indexer_tx = IndexPublisher {
            tx: msg_tx.clone(),
            snapshot: snapshot.clone(),
            save_allowed: true,
        };
        let max_content_filesize = config.max_content_filesize;
        let unified = config.unified;
        let remember_history = config.remember_history;
        let indexer = std::thread::spawn(move || {
            let cached = index::load(&cache_path).map(Arc::new);
            let expected = cached.as_ref().map(|c| c.len());
            if let Some(cached) = cached.as_ref() {
                let _ = indexer_tx.send(Msg::IndexSnapshot {
                    store: cached.clone(),
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
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let app_dirs = if config.index_apps {
                walker::default_app_dirs()
            } else {
                Vec::new()
            };
            let mut watch_roots = config.roots.clone();
            watch_roots.extend(app_dirs.iter().filter(|p| p.exists()).cloned());
            let (watcher, watch_failures) = start_watcher(&disjoint_paths(watch_roots));
            if watch_failures > 0 {
                let noun = if watch_failures == 1 { "root" } else { "roots" };
                let _ = indexer_tx.send(Msg::IndexError {
                    error: format!("live updates unavailable for {watch_failures} {noun}"),
                });
            }
            let (path_tx, path_rx) = mpsc::channel::<(String, FileMeta)>();
            let roots = disjoint_paths(config.roots.iter().cloned());
            let walk_excludes = excludes.clone();
            let index_apps = config.index_apps;
            let walk_stop = stop.clone();
            let walk_thread = std::thread::spawn(move || {
                walker::walk_cancellable(&roots, &walk_excludes, index_apps, &path_tx, &walk_stop)
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
            let walked = walk_thread.join();
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if let Some(error) = walk_failure(walked) {
                indexer_tx.save_allowed = false;
                if let Some(cached) = cached.as_ref() {
                    // Keep a usable warm baseline, including for later watcher batches.
                    fresh = (0..cached.len())
                        .map(|i| (cached.get(i).to_string(), cached.meta(i)))
                        .collect();
                    sort_unique_entries(&mut fresh);
                } else {
                    // A cold partial walk is still useful, but is never persisted
                    // as a complete replacement. Keep its failure visible.
                    sort_unique_entries(&mut fresh);
                    let _ = indexer_tx.send(Msg::IndexSnapshot {
                        store: Arc::new(PathStore::from_entries(&fresh)),
                        indexing: false,
                    });
                }
                let disposition = if cached.is_some() {
                    "retaining previous index"
                } else {
                    "showing partial results"
                };
                let _ = indexer_tx.send(Msg::IndexError {
                    error: format!("{error}; {disposition}"),
                });
            } else {
                sort_unique_entries(&mut fresh);
                let _ = indexer_tx.send(Msg::IndexSnapshot {
                    store: Arc::new(PathStore::from_entries(&fresh)),
                    indexing: false,
                });
                if let Err(error) = index::save(&fresh, &cache_path) {
                    let _ = indexer_tx.send(Msg::IndexError {
                        error: format!("saving index: {error}"),
                    });
                }
            }
            drop(cached);
            if let Some((_watcher, event_rx)) = watcher {
                // _watcher must stay alive for events to keep flowing
                watch_loop(
                    &event_rx,
                    &excludes,
                    &cache_path,
                    &indexer_tx,
                    fresh,
                    &config.roots,
                    &app_dirs,
                    &stop,
                );
            }
        });

        let frecency = remember_history.then(|| Frecency::load(history_path));
        let boosts = Arc::new(
            frecency
                .as_ref()
                .map_or_else(HashMap::new, |f| f.boosts(unix_now())),
        );
        Engine {
            msg_rx,
            snapshot,
            shutdown,
            indexer: Some(indexer),
            content_rx: None,
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
            index_error: None,
            mode: Mode::Fuzzy,
            generation: 0,
            current_generation,
            query: String::new(),
            strong: 0,
            max_content_filesize,
            pdf_cache,
            filters: Filters::default(),
            pending_content: None,
            content_cancel: None,
            pending_semantic: None,
            filename_running: false,
            semantic_running: false,
            sem_tx: None,
            frecency,
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
        let current_generation = Arc::new(AtomicU64::new(0));
        spawn_search_worker(job_rx, msg_tx.clone(), current_generation.clone());
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
            snapshot: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
            indexer: None,
            content_rx: None,
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
            index_error: None,
            mode: Mode::Fuzzy,
            generation: 0,
            current_generation,
            query: String::new(),
            strong: 0,
            max_content_filesize: 0,
            pdf_cache: PathBuf::new(),
            filters: Filters::default(),
            pending_content: None,
            content_cancel: None,
            pending_semantic: None,
            filename_running: false,
            semantic_running: false,
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

    fn advance_generation(&mut self) {
        self.generation += 1;
        self.current_generation
            .store(self.generation, Ordering::Relaxed);
    }

    pub fn set_query(&mut self, input: &str, regex_mode: bool) {
        // Old-generation replies cannot settle work for the new query.
        self.filename_running = false;
        self.semantic_running = false;
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
            self.advance_generation();
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
            self.advance_generation();
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
        self.advance_generation();
        self.mode = mode;
        self.query = query.clone();
        self.status.error = None;
        self.cancel_content();
        self.pending_content = None;
        self.pending_semantic = None;
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

    /// Apply worker updates and due searches.
    pub fn tick(&mut self) {
        self.tick_changed();
    }

    /// Apply updates and report whether results or status need repainting.
    /// Unlike `tick`, this is intended for demand-driven UI event loops.
    pub fn tick_changed(&mut self) -> bool {
        let previous_status = self.status();
        let mut results_changed = false;
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap()
            .take()
            .map(|(store, indexing)| Msg::IndexSnapshot { store, indexing });
        while let Some(msg) = snapshot
            .take()
            .or_else(|| self.msg_rx.try_recv().ok())
            .or_else(|| self.content_rx.as_ref().and_then(|rx| rx.try_recv().ok()))
        {
            match msg {
                Msg::IndexSnapshot { store, indexing } => {
                    results_changed = true;
                    self.store = store;
                    self.status.indexed = self.store.len();
                    self.status.indexing = indexing;
                    if !indexing {
                        // the startup walk (fresh or re-walk) is done
                        self.status.walk = None;
                    }
                    if self.mode == Mode::Content && !self.query.is_empty() {
                        self.advance_generation();
                        self.cancel_content();
                        self.results.clear();
                        self.status.matches = 0;
                        // Preserve an existing debounce; active searches restart now.
                        let at = self
                            .pending_content
                            .take()
                            .map_or_else(|| Instant::now() - CONTENT_DEBOUNCE, |(_, at)| at);
                        self.pending_content = Some((self.query.clone(), at));
                    }
                    if matches!(self.mode, Mode::Fuzzy | Mode::Regex) {
                        self.advance_generation();
                        self.semantic_running = false;
                        self.pending_semantic = None;
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
                    // Keep whatever snapshot is live
                    // (possibly none) and stop the "indexing" spinner
                    self.status.indexing = false;
                    self.status.walk = None;
                    match self.index_error.as_mut() {
                        Some(previous) if previous.contains(&error) => {}
                        Some(previous) if previous.len() + error.len() < 4096 => {
                            previous.push_str("; ");
                            previous.push_str(&error);
                        }
                        _ => self.index_error = Some(error),
                    }
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
                    results_changed = true;
                    self.filename_running = false;
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
                    results_changed = true;
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
                Msg::ContentDone { generation } => {
                    if generation == self.generation {
                        self.content_cancel = None;
                    }
                }
                Msg::SemanticResults {
                    generation,
                    rows,
                    error,
                } => {
                    if generation != self.generation {
                        continue;
                    }
                    results_changed = true;
                    self.semantic_running = false;
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
        // Drain index changes before choosing the content-search scope.
        self.fire_due_content_search();
        self.fire_due_semantic_search();
        results_changed || self.status() != previous_status
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
        let mut status = self.status.clone();
        status.searching = self.filename_running
            || self.semantic_running
            || self.pending_content.is_some()
            || self.content_cancel.is_some()
            || self.pending_semantic.is_some();
        status.error = status.error.or_else(|| self.index_error.clone());
        status
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    fn dispatch_filename(&mut self) {
        let mode = match self.mode {
            Mode::Regex => FilenameMode::Regex,
            _ => FilenameMode::Fuzzy,
        };
        self.filename_running = self
            .job_tx
            .send(FilenameJob {
                generation: self.generation,
                query: self.query.clone(),
                mode,
                store: self.store.clone(),
                boosts: self.boosts.clone(),
                filters: self.filters.clone(),
                quiet: self.quiet.clone(),
                lines: self.filter,
            })
            .is_ok();
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
        // Scope candidates on the worker, not the UI thread. Hits flow directly
        // through one bounded queue; there is no unbounded relay channel.
        let filters = self.filters.clone();
        let store = self.store.clone();
        let (tx, rx) = mpsc::sync_channel(content::QUEUE_CAPACITY);
        self.content_rx = Some(rx);
        let generation = self.generation;
        let max = self.max_content_filesize;
        let pdf_cache = self.pdf_cache.clone();
        std::thread::spawn(move || {
            let searched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut indices = Vec::new();
                for i in 0..store.len() {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    if filters.is_empty()
                        || (filters.matches(store.get(i)) && filters.matches_meta(&store.meta(i)))
                    {
                        indices.push(i);
                    }
                }
                content::search_with_sink(
                    &indices,
                    |i| store.get(i),
                    &pattern,
                    max,
                    &pdf_cache,
                    &cancel,
                    CONTENT_LIMIT,
                    |hit| {
                        content::send_cancellable(&tx, Msg::ContentHit { generation, hit }, &cancel)
                    },
                )
            }));
            let error = match searched {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("invalid pattern: {e}")),
                Err(_) => Some("content search failed".to_string()),
            };
            if let Some(error) = error {
                content::send_cancellable(&tx, Msg::ContentError { generation, error }, &cancel);
            }
            content::send_cancellable(&tx, Msg::ContentDone { generation }, &cancel);
        });
    }

    fn cancel_content(&mut self) {
        self.content_rx = None;
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
        self.semantic_running = tx
            .send(SemJob {
                generation: self.generation,
                query,
                filters: self.filters.clone(),
            })
            .is_ok();
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
            let mut ready: Option<WarmSemantic> = None;
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
                        spare = release_semantic_store(&mut ready);
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
                                let rows: Vec<ResultRow> = store
                                    .query_filtered(&qv[0], SEMANTIC_LIMIT, |doc| {
                                        job.filters.is_empty()
                                            || (job.filters.matches(&doc.path)
                                                && job.filters.matches_meta(&FileMeta {
                                                    mtime: doc.mtime,
                                                    size: doc.size,
                                                }))
                                    })
                                    .into_iter()
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

impl Drop for Engine {
    fn drop(&mut self) {
        self.cancel_content();
        self.shutdown.store(true, Ordering::Relaxed);
        self.current_generation.store(u64::MAX, Ordering::Relaxed);
        if let Some(indexer) = self.indexer.take() {
            let _ = indexer.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_store_reload_preserves_the_warm_embedder() {
        struct CountingEmbedder(usize);
        impl sem::Embedder for CountingEmbedder {
            fn dim(&self) -> usize {
                1
            }
            fn embed(&mut self, _: &[String]) -> Result<Vec<Vec<f32>>, String> {
                self.0 += 1;
                Ok(vec![vec![self.0 as f32]])
            }
        }
        let mut ready: Option<WarmSemantic> =
            Some((Box::new(CountingEmbedder(7)), sem::SemStore::new(1)));
        let mut warm = release_semantic_store(&mut ready).unwrap();
        assert!(ready.is_none());
        assert_eq!(warm.embed(&[]).unwrap(), vec![vec![8.0]]);
    }

    #[test]
    fn semantic_snippets_reject_grown_files_and_cap_normal_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.txt");
        std::fs::write(&file, format!("first\n{}\n", "é".repeat(300))).unwrap();
        let text = snippet_line(file.to_str().unwrap(), 2, dir.path()).unwrap();
        assert_eq!(text.chars().count(), 160);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_len(sem::MAX_SEMANTIC_BYTES * 1000)
            .unwrap();
        assert!(snippet_line(file.to_str().unwrap(), 1, dir.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn semantic_snippets_reject_fifo_without_waiting_for_writer() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replaced.txt");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid C path to a unique temporary-directory entry.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(snippet_line(path.to_str().unwrap(), 1, dir.path()).is_none());
    }

    fn watch_messages(
        current: Vec<(String, FileMeta)>,
        events: Vec<notify::Result<notify::Event>>,
        roots: &[PathBuf],
        excludes: &globset::GlobSet,
        apps: bool,
    ) -> Vec<Msg> {
        watch_messages_with_apps(
            current,
            events,
            roots,
            excludes,
            &if apps {
                walker::default_app_dirs()
            } else {
                Vec::new()
            },
        )
    }

    fn watch_messages_with_apps(
        current: Vec<(String, FileMeta)>,
        events: Vec<notify::Result<notify::Event>>,
        roots: &[PathBuf],
        excludes: &globset::GlobSet,
        app_dirs: &[PathBuf],
    ) -> Vec<Msg> {
        let dir = tempfile::tempdir().unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        for event in events {
            event_tx.send(event).unwrap();
        }
        // No timing or OS watcher dependency: EOF ends the debounce burst.
        drop(event_tx);
        let (tx, index_rx) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(None));
        let index_tx = IndexPublisher {
            tx,
            snapshot: snapshot.clone(),
            save_allowed: true,
        };
        watch_loop(
            &event_rx,
            excludes,
            &dir.path().join("index.bin"),
            &index_tx,
            current,
            roots,
            app_dirs,
            &AtomicBool::new(false),
        );
        drop(index_tx);
        let mut messages: Vec<_> = index_rx.into_iter().collect();
        if let Some((store, indexing)) = snapshot.lock().unwrap().take() {
            messages.push(Msg::IndexSnapshot { store, indexing });
        }
        messages
    }

    fn snapshot_entries(messages: &[Msg]) -> Vec<(String, FileMeta)> {
        messages
            .iter()
            .find_map(|msg| match msg {
                Msg::IndexSnapshot { store, .. } => Some(
                    (0..store.len())
                        .map(|i| (store.get(i).to_string(), store.meta(i)))
                        .collect(),
                ),
                _ => None,
            })
            .expect("snapshot")
    }

    /// No worker timing: discard the startup snapshot before it dispatches,
    /// then enqueue exactly the messages under test.
    fn redraw_test_engine() -> Engine {
        let engine = Engine::from_lines(vec!["alpha".into(), "beta".into()]);
        assert!(matches!(
            engine.msg_rx.try_recv(),
            Ok(Msg::IndexSnapshot { .. })
        ));
        engine
    }

    #[test]
    fn tick_dirty_ignores_idle_and_stale_work_but_reports_empty_completion() {
        let mut engine = redraw_test_engine();
        assert!(!engine.tick_changed());
        engine.filename_running = true;
        engine
            .msg_tx
            .send(Msg::FilenameResults {
                generation: engine.generation.wrapping_add(1),
                indices: vec![0],
                strong: 1,
                error: None,
            })
            .unwrap();
        assert!(!engine.tick_changed());
        assert!(engine.status().searching);
        engine
            .msg_tx
            .send(Msg::FilenameResults {
                generation: engine.generation,
                indices: vec![],
                strong: 0,
                error: None,
            })
            .unwrap();
        assert!(engine.tick_changed());
        assert!(!engine.status().searching);
        assert!(engine.results().is_empty());
        assert!(!engine.tick_changed());
    }

    #[test]
    fn tick_dirty_reports_same_count_replacements_and_search_errors() {
        let mut engine = redraw_test_engine();
        for index in [0, 1] {
            engine
                .msg_tx
                .send(Msg::FilenameResults {
                    generation: engine.generation,
                    indices: vec![index],
                    strong: 1,
                    error: None,
                })
                .unwrap();
            assert!(engine.tick_changed());
            assert_eq!(engine.results().len(), 1);
            assert_eq!(
                engine.results()[0].path,
                if index == 0 { "alpha" } else { "beta" }
            );
            assert!(!engine.tick_changed());
        }
        engine
            .msg_tx
            .send(Msg::ContentError {
                generation: engine.generation.wrapping_add(1),
                error: "stale".into(),
            })
            .unwrap();
        assert!(!engine.tick_changed());
        engine
            .msg_tx
            .send(Msg::ContentError {
                generation: engine.generation,
                error: "invalid pattern".into(),
            })
            .unwrap();
        assert!(engine.tick_changed());
        assert_eq!(engine.status().error.as_deref(), Some("invalid pattern"));
        engine.content_cancel = Some(Arc::new(AtomicBool::new(false)));
        engine
            .msg_tx
            .send(Msg::ContentDone {
                generation: engine.generation,
            })
            .unwrap();
        assert!(engine.tick_changed());
        assert!(!engine.status().searching);
        assert!(!engine.tick_changed());
    }

    #[test]
    fn tick_dirty_reports_index_progress_failure_and_empty_snapshot() {
        let mut engine = redraw_test_engine();
        engine.status.indexing = true;
        for changed in [true, false] {
            engine
                .msg_tx
                .send(Msg::IndexProgress {
                    count: 10,
                    expected: Some(100),
                })
                .unwrap();
            assert_eq!(engine.tick_changed(), changed);
        }
        engine
            .msg_tx
            .send(Msg::IndexError {
                error: "synthetic failure".into(),
            })
            .unwrap();
        assert!(engine.tick_changed());
        assert!(!engine.status().indexing);
        assert!(!engine.tick_changed());
        // Calc mode avoids dispatching real matcher work in this fixture.
        engine.mode = Mode::Calc;
        *engine.snapshot.lock().unwrap() = Some((Arc::new(PathStore::empty()), false));
        assert!(engine.tick_changed());
        assert_eq!(engine.status().indexed, 0);
        assert!(!engine.tick_changed());
    }

    #[test]
    fn tick_dirty_reports_streamed_hits_and_semantic_completion() {
        let mut engine = redraw_test_engine();
        engine.mode = Mode::Content;
        for (generation, expected) in [(1, false), (0, true)] {
            engine
                .msg_tx
                .send(Msg::ContentHit {
                    generation,
                    hit: content::ContentMatch {
                        path: "synthetic".into(),
                        line_number: 1,
                        line: "match".into(),
                    },
                })
                .unwrap();
            assert_eq!(engine.tick_changed(), expected);
        }
        assert_eq!(engine.results.len(), 1);
        engine.results.clear();
        engine.status.matches = 0;
        engine.mode = Mode::Semantic;
        engine.semantic_running = true;
        for (generation, expected) in [(1, false), (0, true)] {
            engine
                .msg_tx
                .send(Msg::SemanticResults {
                    generation,
                    rows: Vec::new(),
                    error: None,
                })
                .unwrap();
            assert_eq!(engine.tick_changed(), expected);
        }
        assert!(!engine.status().searching);
        assert!(engine.results.is_empty());
        assert!(!engine.tick_changed());
    }

    #[test]
    fn snapshots_precede_due_content_dispatch_and_restart_completed_content() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.txt");
        let new = dir.path().join("new.txt");
        std::fs::write(&old, "needle\n").unwrap();
        std::fs::write(&new, "needle\n").unwrap();
        let mut engine = Engine::from_lines(vec![old.to_string_lossy().into_owned()]);
        engine.tick();
        engine.filter = false;
        engine.max_content_filesize = 1024;
        engine.set_query(">needle", false);
        engine.pending_content = Some(("needle".into(), Instant::now() - CONTENT_DEBOUNCE));
        let new_store = Arc::new(PathStore::from_entries(&[(
            new.to_string_lossy().into_owned(),
            FileMeta::default(),
        )]));
        *engine.snapshot.lock().unwrap() = Some((new_store, false));
        engine.tick();
        wait_for(&mut engine, |e| !e.status().searching);
        assert_eq!(engine.results.len(), 1);
        assert_eq!(engine.results[0].path, new.to_string_lossy());
        let old_store = Arc::new(PathStore::from_entries(&[(
            old.to_string_lossy().into_owned(),
            FileMeta::default(),
        )]));
        *engine.snapshot.lock().unwrap() = Some((old_store, false));
        engine.tick();
        wait_for(&mut engine, |e| !e.status().searching);
        assert_eq!(engine.results.len(), 1);
        assert_eq!(engine.results[0].path, old.to_string_lossy());
    }

    #[test]
    fn snapshot_replaces_blocked_content_generation() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..10)
            .map(|i| {
                let path = dir.path().join(format!("{i}.txt"));
                std::fs::write(&path, "needle\n".repeat(20)).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let mut engine = Engine::from_lines(paths);
        engine.tick();
        engine.filter = false;
        engine.max_content_filesize = 1024;
        engine.set_query(">needle", false);
        engine.pending_content = Some(("needle".into(), Instant::now() - CONTENT_DEBOUNCE));
        engine.tick();
        let old_cancel = engine.content_cancel.as_ref().unwrap().clone();
        std::thread::sleep(Duration::from_millis(30));
        *engine.snapshot.lock().unwrap() = Some((Arc::new(PathStore::empty()), false));
        engine.tick();
        assert!(old_cancel.load(Ordering::Relaxed));
        wait_for(&mut engine, |e| !e.status().searching);
        assert!(engine.results.is_empty());
    }

    #[test]
    fn walker_join_failures_are_not_successful_empty_walks() {
        assert_eq!(
            walk_failure(Err(Box::new("injected panic"))).as_deref(),
            Some("index walk failed")
        );
        assert!(walk_failure(Ok(walker::WalkStats::default())).is_none());
    }

    #[test]
    fn pending_snapshots_are_coalesced() {
        let (tx, _rx) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(None));
        let publisher = IndexPublisher {
            tx,
            snapshot: snapshot.clone(),
            save_allowed: true,
        };
        let first = Arc::new(PathStore::empty());
        let weak = Arc::downgrade(&first);
        publisher
            .send(Msg::IndexSnapshot {
                store: first,
                indexing: true,
            })
            .unwrap();
        publisher
            .send(Msg::IndexSnapshot {
                store: Arc::new(PathStore::empty()),
                indexing: false,
            })
            .unwrap();
        assert!(weak.upgrade().is_none());
        assert!(!snapshot.lock().unwrap().as_ref().unwrap().1);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_prunes_symlinks_and_does_not_follow_symlink_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("hidden.txt"), "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let stale = format!("{}/old.txt", link.display());
        for event_path in [&link, &link.join("hidden.txt")] {
            let messages = watch_messages(
                vec![(stale.clone(), FileMeta::default())],
                vec![Ok(
                    notify::Event::new(notify::EventKind::Any).add_path(event_path.clone())
                )],
                &[dir.path().to_path_buf()],
                &walker::build_exclude_set(&[]).unwrap(),
                false,
            );
            assert!(snapshot_entries(&messages).is_empty());
        }
        let file_link = dir.path().join("file.txt");
        std::os::unix::fs::symlink(outside.path().join("hidden.txt"), &file_link).unwrap();
        let messages = watch_messages(
            Vec::new(),
            vec![Ok(
                notify::Event::new(notify::EventKind::Any).add_path(file_link)
            )],
            &[dir.path().to_path_buf()],
            &walker::build_exclude_set(&[]).unwrap(),
            false,
        );
        assert!(snapshot_entries(&messages).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn watcher_allows_symlinked_parents_above_configured_roots() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("root")).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let root = alias.join("root");
        let file = root.join("new.txt");
        std::fs::write(&file, "x").unwrap();
        let messages = watch_messages(
            Vec::new(),
            vec![Ok(
                notify::Event::new(notify::EventKind::Any).add_path(file.clone())
            )],
            &[root],
            &walker::build_exclude_set(&[]).unwrap(),
            false,
        );
        assert_eq!(snapshot_entries(&messages)[0].0, file.to_string_lossy());
    }

    #[test]
    fn watcher_refreshes_application_entries_without_trailing_slashes() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Utilities/New.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        let excludes = walker::build_exclude_set(&["*.app".into()]).unwrap();
        let stale = dir.path().join("Old.app").to_string_lossy().into_owned();
        let messages = watch_messages_with_apps(
            vec![(stale, FileMeta::default())],
            vec![Ok(
                notify::Event::new(notify::EventKind::Any).add_path(app.join("Contents"))
            )],
            &[],
            &excludes,
            &[dir.path().to_path_buf()],
        );
        let entries = snapshot_entries(&messages);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, app.to_string_lossy());
        assert_eq!(entries[0].1.size, 0);
    }

    #[test]
    fn failed_rescan_keeps_previous_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let messages = watch_messages(
            vec![("/cached.txt".into(), FileMeta::default())],
            vec![Ok(
                notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan)
            )],
            &[dir.path().join("missing")],
            &walker::build_exclude_set(&[]).unwrap(),
            false,
        );
        assert!(messages.iter().any(|m| matches!(m, Msg::IndexError { .. })));
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, Msg::IndexSnapshot { .. }))
        );
    }

    #[test]
    fn idle_watcher_shutdown_and_engine_drop_join_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("index");
        let (event_tx, event_rx) = mpsc::channel();
        let (tx, _rx) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            watch_loop(
                &event_rx,
                &walker::build_exclude_set(&[]).unwrap(),
                &cache,
                &IndexPublisher {
                    tx,
                    snapshot,
                    save_allowed: true,
                },
                Vec::new(),
                &[],
                &[],
                &worker_stop,
            );
            done_tx.send(()).unwrap();
        });
        let mut engine = Engine::from_lines(Vec::new());
        engine.shutdown = stop;
        engine.indexer = Some(worker);
        drop(engine);
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            event_tx
                .send(Ok(notify::Event::new(notify::EventKind::Any)))
                .is_err()
        );
    }

    #[test]
    fn watcher_ignores_access_events() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("read.txt"), "x").unwrap();
        let excludes = walker::build_exclude_set(&[]).unwrap();
        let events = vec![Ok(notify::Event::new(notify::EventKind::Access(
            notify::event::AccessKind::Any,
        ))
        .add_path(dir.path().to_path_buf()))];
        assert!(watch_messages(Vec::new(), events, &[], &excludes, false).is_empty());
    }

    #[test]
    fn watcher_rescan_and_errors_rebuild_configured_roots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("skip")).unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("skip/hidden.txt"), "x").unwrap();
        std::fs::write(dir.path().join("nested/new.txt"), "x").unwrap();
        let roots = vec![dir.path().to_path_buf(), dir.path().join("nested")];
        let excludes = walker::build_exclude_set(&["skip".to_string()]).unwrap();
        let expected = walker::collect_sorted(&roots[..1], &excludes, false).0;
        for event in [
            Ok(notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan)),
            Err(notify::Error::generic("lost event stream")),
        ] {
            let is_error = event.is_err();
            let messages = watch_messages(
                vec![("/stale.txt".to_string(), FileMeta::default())],
                vec![event],
                &roots,
                &excludes,
                false,
            );
            assert_eq!(snapshot_entries(&messages), expected);
            assert_eq!(
                messages.iter().any(|msg| matches!(msg,
                    Msg::IndexError { error } if error.contains("lost event stream")
                )),
                is_error
            );
        }
    }

    #[test]
    fn watcher_rescan_preserves_app_policy() {
        let excludes = walker::build_exclude_set(&["*.app".to_string()]).unwrap();
        // Match the production app collector without assuming apps are installed.
        let mut expected = walker::collect_sorted(&[], &excludes, true).0;
        sort_unique_entries(&mut expected);
        let events = vec![Ok(
            notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan)
        )];
        let messages = watch_messages(Vec::new(), events, &[], &excludes, true);
        assert_eq!(snapshot_entries(&messages), expected);
    }

    #[test]
    fn watcher_sorts_real_mtimes_and_deduplicates_overlapping_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let sub = root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let old = sub.join("old.txt");
        let newest = root.join("newest.txt");
        for (path, time) in [(&old, 100), (&newest, 300), (&sub, 200)] {
            if path != &sub {
                std::fs::write(path, "x").unwrap();
            }
            let file = std::fs::File::open(path).unwrap();
            file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(time))
                .unwrap();
        }
        let excludes = walker::build_exclude_set(&[]).unwrap();
        let current = vec![(
            newest.to_string_lossy().into_owned(),
            FileMeta {
                mtime: 300,
                size: 1,
            },
        )];
        let events = [sub.clone(), old.clone(), sub.clone(), old]
            .into_iter()
            .map(|path| Ok(notify::Event::new(notify::EventKind::Other).add_path(path)))
            .collect();
        let messages = watch_messages(current, events, &[root, sub.clone()], &excludes, false);
        let entries = snapshot_entries(&messages);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .iter()
                .map(|(_, meta)| meta.mtime)
                .collect::<Vec<_>>(),
            [300, 200, 100]
        );
        assert_eq!(
            disjoint_paths([sub.clone(), sub.join("child"), sub]),
            vec![dir.path().join("sub")]
        );
    }

    #[test]
    fn index_entries_are_unique_and_mtime_ties_use_path_order() {
        let mut entries = vec![
            ("/b".into(), FileMeta { mtime: 10, size: 1 }),
            ("/a".into(), FileMeta { mtime: 10, size: 1 }),
            ("/b".into(), FileMeta { mtime: 5, size: 1 }),
            ("/new".into(), FileMeta { mtime: 20, size: 1 }),
        ];
        sort_unique_entries(&mut entries);
        assert_eq!(
            entries
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["/new", "/a", "/b"]
        );
        assert_eq!(entries[2].1.mtime, 10);
    }

    #[test]
    fn watcher_root_refresh_does_not_add_root_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("child.txt"), "x").unwrap();
        let root = dir.path().to_path_buf();
        let excludes = walker::build_exclude_set(&[]).unwrap();
        let messages = watch_messages(
            Vec::new(),
            vec![Ok(
                notify::Event::new(notify::EventKind::Other).add_path(root.clone())
            )],
            &[root],
            &excludes,
            false,
        );
        assert_eq!(snapshot_entries(&messages).len(), 1);
    }

    #[test]
    fn runtime_index_errors_survive_successful_searches() {
        let mut engine = Engine::from_lines(vec!["alpha".to_string()]);
        engine
            .msg_tx
            .send(Msg::IndexError {
                error: "live update error".to_string(),
            })
            .unwrap();
        wait_for(&mut engine, |e| e.results().len() == 1);
        engine.set_query("alpha", false);
        engine.tick();
        assert_eq!(engine.status().error.as_deref(), Some("live update error"));
    }

    #[test]
    fn empty_queries_cancel_debounced_content_and_semantic_jobs() {
        for (input, empty) in [
            ("> old", ">"),
            ("? old", "?"),
            ("> old", "> ext:txt"),
            ("? old", "? ext:txt"),
        ] {
            let mut engine = Engine::from_lines(Vec::new());
            engine.filter = false;
            engine.set_query(input, false);
            assert!(
                engine.status().searching,
                "{input} must include debounce time"
            );
            let past = Instant::now() - CONTENT_DEBOUNCE;
            if let Some((_, at)) = engine.pending_content.as_mut() {
                *at = past;
            }
            if let Some((_, at)) = engine.pending_semantic.as_mut() {
                *at = past;
            }
            engine.set_query(empty, false);
            engine.tick();
            assert!(engine.pending_content.is_none(), "{empty}");
            assert!(engine.pending_semantic.is_none(), "{empty}");
            assert!(engine.content_cancel.is_none(), "{empty}");
            assert!(engine.sem_tx.is_none(), "{empty}");
            assert!(engine.results().is_empty(), "{empty}");
            assert!(!engine.status().searching, "{empty}");
        }
    }

    #[test]
    fn search_status_settles_for_filename_content_and_invalid_patterns() {
        let mut engine = Engine::from_lines(vec!["alpha".into()]);
        engine.tick();
        wait_for(&mut engine, |e| !e.status().searching);
        engine.set_query("missing", false);
        assert!(engine.status().searching);
        wait_for(&mut engine, |e| !e.status().searching);
        assert!(engine.results().is_empty());

        engine.filter = false;
        for input in ["> missing", "> ["] {
            engine.set_query(input, false);
            assert!(engine.status().searching);
            engine.pending_content.as_mut().unwrap().1 = Instant::now() - CONTENT_DEBOUNCE;
            wait_for(&mut engine, |e| !e.status().searching);
            assert!(engine.content_cancel.is_none());
            assert!(engine.results().is_empty());
            assert_eq!(engine.status().error.is_some(), input == "> [");
        }
    }

    #[test]
    fn stale_completions_cannot_clear_current_search_status() {
        let mut engine = Engine::from_lines(Vec::new());
        engine.tick();
        wait_for(&mut engine, |e| !e.status().searching);
        engine.generation += 1;
        engine.filename_running = true;
        engine.semantic_running = true;
        engine.content_cancel = Some(Arc::new(AtomicBool::new(false)));
        for generation in [engine.generation - 1, engine.generation] {
            engine
                .msg_tx
                .send(Msg::FilenameResults {
                    generation,
                    indices: Vec::new(),
                    strong: 0,
                    error: None,
                })
                .unwrap();
            engine
                .msg_tx
                .send(Msg::SemanticResults {
                    generation,
                    rows: Vec::new(),
                    error: None,
                })
                .unwrap();
            engine.msg_tx.send(Msg::ContentDone { generation }).unwrap();
            engine.tick();
            let stale = generation != engine.generation;
            assert_eq!(engine.filename_running, stale);
            assert_eq!(engine.semantic_running, stale);
            assert_eq!(engine.content_cancel.is_some(), stale);
            assert_eq!(engine.status().searching, stale);
        }
    }

    #[test]
    fn unified_search_stays_busy_until_both_workers_finish() {
        let mut engine = Engine::from_lines(Vec::new());
        engine.tick();
        wait_for(&mut engine, |e| !e.status().searching);
        engine.filename_running = true;
        engine.semantic_running = true;
        engine
            .msg_tx
            .send(Msg::FilenameResults {
                generation: engine.generation,
                indices: Vec::new(),
                strong: 0,
                error: None,
            })
            .unwrap();
        engine.tick();
        assert!(engine.status().searching);
        engine
            .msg_tx
            .send(Msg::SemanticResults {
                generation: engine.generation,
                rows: Vec::new(),
                error: None,
            })
            .unwrap();
        engine.tick();
        assert!(!engine.status().searching);
    }

    /// Feeds one debounced event for `path` into `watch_loop` and returns
    /// the paths of the snapshot it publishes.
    fn watch_snapshot(
        current: Vec<(String, FileMeta)>,
        touched: std::path::PathBuf,
    ) -> Vec<String> {
        let excludes = walker::build_exclude_set(&[]).unwrap();
        let messages = watch_messages(
            current,
            vec![Ok(
                notify::Event::new(notify::EventKind::Other).add_path(touched)
            )],
            &[],
            &excludes,
            false,
        );
        snapshot_entries(&messages)
            .into_iter()
            .map(|(path, _)| path)
            .collect()
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
