use crate::content::{self, ContentMatch};
use crate::filters::{self, Filters};
use crate::frecency::Frecency;
use crate::index::PathStore;
use crate::matcher::{self, FilenameMode};
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
pub const CONTENT_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Fuzzy,
    Regex,
    Content,
}

pub fn parse_query(input: &str, regex_mode: bool) -> (Mode, String) {
    if let Some(rest) = input.strip_prefix('>') {
        (Mode::Content, rest.trim_start().to_string())
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
}

#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub indexed: usize,
    pub indexing: bool,
    pub matches: usize,
    pub error: Option<String>,
}

enum Msg {
    IndexSnapshot {
        store: Arc<PathStore>,
        indexing: bool,
    },
    FilenameResults {
        generation: u64,
        indices: Vec<usize>,
        error: Option<String>,
    },
    ContentHit {
        generation: u64,
        hit: ContentMatch,
    },
}

struct FilenameJob {
    generation: u64,
    query: String,
    mode: FilenameMode,
    store: Arc<PathStore>,
    boosts: Arc<HashMap<String, u32>>,
    filters: Filters,
}

pub struct Engine {
    msg_rx: Receiver<Msg>,
    msg_tx: Sender<Msg>,
    job_tx: Sender<FilenameJob>,
    store: Arc<PathStore>,
    results: Vec<ResultRow>,
    status: EngineStatus,
    mode: Mode,
    generation: u64,
    query: String,
    max_content_filesize: u64,
    pdf_cache: PathBuf,
    filters: Filters,
    pending_content: Option<(String, Instant)>,
    content_cancel: Option<Arc<AtomicBool>>,
    frecency: Frecency,
    boosts: Arc<HashMap<String, u32>>,
}

const WATCH_DEBOUNCE: Duration = Duration::from_millis(400);
const WATCH_SAVE_EVERY: Duration = Duration::from_secs(60);

fn cache_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Starts watching `roots` and returns the live watcher (keep it alive!)
/// plus the event stream. Armed *before* the initial walk so that no change
/// can slip between walk completion and stream start.
fn start_watcher(
    roots: &[std::path::PathBuf],
) -> Option<(
    notify::RecommendedWatcher,
    Receiver<notify::Result<notify::Event>>,
)> {
    use notify::Watcher;
    let (event_tx, event_rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = event_tx.send(res);
    })
    .ok()?;
    for root in roots {
        let _ = watcher.watch(root, notify::RecursiveMode::Recursive);
    }
    Some((watcher, event_rx))
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
                push_front(&mut fronts, &mut gone, (s, meta));
            } else if path.is_dir() {
                // a directory appeared or changed: index its files
                let (entries, _) = walker::collect_sorted(&[path], excludes);
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

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Engine {
    pub fn new(config: Config, cache_path: PathBuf, history_path: PathBuf) -> Engine {
        let pdf_cache = cache_path
            .parent()
            .map(|p| p.join("pdftext"))
            .unwrap_or_else(crate::pdf::default_cache_dir);
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let (job_tx, job_rx) = mpsc::channel::<FilenameJob>();

        // filename search worker: always process only the newest job
        let worker_tx = msg_tx.clone();
        std::thread::spawn(move || {
            while let Ok(mut job) = job_rx.recv() {
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                let (indices, error) = match matcher::search_boosted(
                    &job.store,
                    &job.query,
                    job.mode,
                    FILENAME_LIMIT,
                    &job.boosts,
                    &job.filters,
                ) {
                    Ok(ix) => (ix, None),
                    Err(e) => (Vec::new(), Some(format!("invalid pattern: {e}"))),
                };
                if worker_tx
                    .send(Msg::FilenameResults {
                        generation: job.generation,
                        indices,
                        error,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });

        // indexer: cached paths first, then a fresh walk, then save
        let indexer_tx = msg_tx.clone();
        let max_content_filesize = config.max_content_filesize;
        std::thread::spawn(move || {
            if let Some(cached) = index::load(&cache_path) {
                let _ = indexer_tx.send(Msg::IndexSnapshot {
                    store: Arc::new(cached),
                    indexing: true,
                });
            }
            let Ok(excludes) = walker::build_exclude_set(&config.excludes) else {
                let _ = indexer_tx.send(Msg::IndexSnapshot {
                    store: Arc::new(PathStore::empty()),
                    indexing: false,
                });
                return;
            };
            // arm the watcher before walking: events raised mid-walk sit in
            // the channel and are folded in afterwards (re-stat makes them
            // idempotent), so nothing slips through the startup window
            let watcher = start_watcher(&config.roots);
            let (path_tx, path_rx) = mpsc::channel::<(String, FileMeta)>();
            let roots = config.roots.clone();
            let walk_excludes = excludes.clone();
            let walk_thread =
                std::thread::spawn(move || walker::walk(&roots, &walk_excludes, &path_tx));
            // the index is ordered newest-first, so head-of-list results,
            // regex hits (index order) and fuzzy score ties all favor recency
            let mut fresh: Vec<(String, FileMeta)> = Vec::new();
            let mut last_publish = Instant::now();
            for entry in path_rx {
                fresh.push(entry);
                // stream early results on a cold start so the UI isn't empty
                if fresh.len().is_multiple_of(8192)
                    && last_publish.elapsed() > Duration::from_millis(250)
                {
                    last_publish = Instant::now();
                    let mut snapshot = fresh.clone();
                    snapshot.sort_unstable_by(walker::mtime_cmp);
                    let _ = indexer_tx.send(Msg::IndexSnapshot {
                        store: Arc::new(PathStore::from_entries(&snapshot)),
                        indexing: true,
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
            status: EngineStatus {
                indexing: true,
                ..Default::default()
            },
            mode: Mode::Fuzzy,
            generation: 0,
            query: String::new(),
            max_content_filesize,
            pdf_cache,
            filters: Filters::default(),
            pending_content: None,
            content_cancel: None,
            frecency,
            boosts,
        }
    }

    /// Records that `path` was opened, boosting it in future rankings.
    pub fn record_open(&mut self, path: &str) {
        self.frecency.record(path);
        self.boosts = Arc::new(self.frecency.boosts(unix_now()));
    }

    pub fn set_query(&mut self, input: &str, regex_mode: bool) {
        let (mode, query) = parse_query(input, regex_mode);
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
        match mode {
            Mode::Content => {
                self.results.clear();
                self.status.matches = 0;
                if !query.is_empty() {
                    self.pending_content = Some((query, Instant::now()));
                }
            }
            Mode::Fuzzy | Mode::Regex => {
                self.pending_content = None;
                self.dispatch_filename();
            }
        }
    }

    pub fn tick(&mut self) {
        self.fire_due_content_search();
        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                Msg::IndexSnapshot { store, indexing } => {
                    self.store = store;
                    self.status.indexed = self.store.len();
                    self.status.indexing = indexing;
                    if matches!(self.mode, Mode::Fuzzy | Mode::Regex) {
                        self.generation += 1;
                        self.dispatch_filename();
                    }
                }
                Msg::FilenameResults {
                    generation,
                    indices,
                    error,
                } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.results = indices
                        .into_iter()
                        .filter(|&i| i < self.store.len())
                        .map(|i| ResultRow {
                            path: self.store.get(i).to_string(),
                            line_number: None,
                            line: None,
                        })
                        .collect();
                    self.status.matches = self.results.len();
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
                    });
                    self.status.matches = self.results.len();
                    if self.results.len() >= CONTENT_LIMIT {
                        self.cancel_content();
                    }
                }
            }
        }
    }

    pub fn results(&self) -> &[ResultRow] {
        &self.results
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
        // content search still wants owned strings (it runs for seconds on a
        // background thread while snapshots keep swapping underneath)
        let f = &self.filters;
        let paths: Arc<Vec<String>> = Arc::new(
            (0..self.store.len())
                .filter(|&i| {
                    f.is_empty()
                        || (f.matches(self.store.get(i)) && f.matches_meta(&self.store.meta(i)))
                })
                .map(|i| self.store.get(i).to_string())
                .collect(),
        );
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let max = self.max_content_filesize;
        let pdf_cache = self.pdf_cache.clone();
        std::thread::spawn(move || {
            let (hit_tx, hit_rx) = mpsc::channel::<ContentMatch>();
            let search_cancel = cancel.clone();
            let search_paths = paths.clone();
            let pattern2 = pattern.clone();
            let searcher = std::thread::spawn(move || {
                content::search(
                    &search_paths,
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
            // an invalid content pattern reuses FilenameResults purely to carry
            // the error string; generation matching makes this safe
            if let Ok(Err(e)) = searcher.join() {
                let _ = tx.send(Msg::FilenameResults {
                    generation,
                    indices: Vec::new(),
                    error: Some(format!("invalid pattern: {e}")),
                });
            }
        });
    }

    fn cancel_content(&mut self) {
        if let Some(flag) = self.content_cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
