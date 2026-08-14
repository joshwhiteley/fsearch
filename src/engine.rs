use crate::content::{self, ContentMatch};
use crate::matcher::{self, FilenameMode};
use crate::{config::Config, index, walker};
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
    IndexSnapshot { paths: Arc<Vec<String>>, indexing: bool },
    FilenameResults { generation: u64, indices: Vec<usize>, error: Option<String> },
    ContentHit { generation: u64, hit: ContentMatch },
}

struct FilenameJob {
    generation: u64,
    query: String,
    mode: FilenameMode,
    paths: Arc<Vec<String>>,
}

pub struct Engine {
    msg_rx: Receiver<Msg>,
    msg_tx: Sender<Msg>,
    job_tx: Sender<FilenameJob>,
    paths: Arc<Vec<String>>,
    results: Vec<ResultRow>,
    status: EngineStatus,
    mode: Mode,
    generation: u64,
    query: String,
    max_content_filesize: u64,
    pending_content: Option<(String, Instant)>,
    content_cancel: Option<Arc<AtomicBool>>,
}

impl Engine {
    pub fn new(config: Config, cache_path: PathBuf) -> Engine {
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let (job_tx, job_rx) = mpsc::channel::<FilenameJob>();

        // filename search worker: always process only the newest job
        let worker_tx = msg_tx.clone();
        std::thread::spawn(move || {
            while let Ok(mut job) = job_rx.recv() {
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                let (indices, error) =
                    match matcher::search(&job.paths, &job.query, job.mode, FILENAME_LIMIT) {
                        Ok(ix) => (ix, None),
                        Err(e) => (Vec::new(), Some(format!("invalid pattern: {e}"))),
                    };
                if worker_tx
                    .send(Msg::FilenameResults { generation: job.generation, indices, error })
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
                let _ = indexer_tx
                    .send(Msg::IndexSnapshot { paths: Arc::new(cached), indexing: true });
            }
            let Ok(excludes) = walker::build_exclude_set(&config.excludes) else {
                let _ = indexer_tx
                    .send(Msg::IndexSnapshot { paths: Arc::new(Vec::new()), indexing: false });
                return;
            };
            let (path_tx, path_rx) = mpsc::channel::<String>();
            let roots = config.roots.clone();
            let walk_thread = std::thread::spawn(move || walker::walk(&roots, &excludes, &path_tx));
            let mut fresh: Vec<String> = Vec::new();
            let mut last_publish = Instant::now();
            for path in path_rx {
                fresh.push(path);
                // stream early results on a cold start so the UI isn't empty
                if fresh.len().is_multiple_of(8192)
                    && last_publish.elapsed() > Duration::from_millis(250)
                {
                    last_publish = Instant::now();
                    let _ = indexer_tx.send(Msg::IndexSnapshot {
                        paths: Arc::new(fresh.clone()),
                        indexing: true,
                    });
                }
            }
            let _ = walk_thread.join();
            fresh.sort_unstable();
            let paths = Arc::new(fresh);
            let _ = indexer_tx.send(Msg::IndexSnapshot { paths: paths.clone(), indexing: false });
            let _ = index::save(&paths, &cache_path);
        });

        Engine {
            msg_rx,
            msg_tx,
            job_tx,
            paths: Arc::new(Vec::new()),
            results: Vec::new(),
            status: EngineStatus { indexing: true, ..Default::default() },
            mode: Mode::Fuzzy,
            generation: 0,
            query: String::new(),
            max_content_filesize,
            pending_content: None,
            content_cancel: None,
        }
    }

    pub fn set_query(&mut self, input: &str, regex_mode: bool) {
        let (mode, query) = parse_query(input, regex_mode);
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
                Msg::IndexSnapshot { paths, indexing } => {
                    self.paths = paths;
                    self.status.indexed = self.paths.len();
                    self.status.indexing = indexing;
                    if matches!(self.mode, Mode::Fuzzy | Mode::Regex) {
                        self.generation += 1;
                        self.dispatch_filename();
                    }
                }
                Msg::FilenameResults { generation, indices, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.results = indices
                        .into_iter()
                        .filter_map(|i| self.paths.get(i))
                        .map(|p| ResultRow { path: p.clone(), line_number: None, line: None })
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
            paths: self.paths.clone(),
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
        let paths = self.paths.clone();
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let max = self.max_content_filesize;
        std::thread::spawn(move || {
            let (hit_tx, hit_rx) = mpsc::channel::<ContentMatch>();
            let search_cancel = cancel.clone();
            let search_paths = paths.clone();
            let pattern2 = pattern.clone();
            let searcher = std::thread::spawn(move || {
                content::search(&search_paths, &pattern2, max, &search_cancel, &hit_tx)
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
        assert_eq!(parse_query("notes", false), (Mode::Fuzzy, "notes".to_string()));
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
