//! Headless query execution for script mode (`fsearch --print`): one entry
//! point that understands the same mode prefixes (`?`, `>`) and filter
//! tokens (`ext:`, `path:`, ...) as the interactive engine, and reports hits
//! through a callback so the caller owns printing.

use crate::content::{self, ContentMatch};
use crate::engine::{FILENAME_LIMIT, Mode, SEMANTIC_LIMIT, parse_query};
use crate::filters::{self, Filters};
use crate::index::PathStore;
use crate::matcher::{self, FilenameMode};
use crate::quiet::Quiet;
use crate::sem;
use crate::util::unix_now;
use crate::walker::FileMeta;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

/// Context a headless search needs beyond the query itself.
pub struct Options {
    /// Files larger than this are skipped by content (`>`) searches.
    pub max_content_filesize: u64,
    /// Paths to demote in fuzzy rankings (the config's quiet markers).
    pub quiet: Quiet,
    /// Where PDF text extraction caches its output.
    pub pdf_cache: std::path::PathBuf,
}

/// One search result, in print order.
pub enum Hit {
    /// Filename (fuzzy) match.
    Path(String),
    /// Content (grep) match.
    Line {
        path: String,
        line_number: u64,
        line: String,
    },
    /// Semantic match with its raw similarity score.
    Semantic {
        path: String,
        line_start: u32,
        score: f32,
    },
}

/// Runs `input` as a full query against `store`, invoking `on_hit` per
/// result. Returns false when nothing matched. `Err` carries the message
/// printed after `fsearch: ` (an invalid pattern, a missing or mismatched
/// semantic index, an embedder failure).
pub fn search(
    store: &PathStore,
    input: &str,
    opts: &Options,
    on_hit: &mut dyn FnMut(Hit),
) -> Result<bool, String> {
    let (mode, query) = parse_query(input, false);
    // script mode has no calculator: an `=` query stays fuzzy text
    let (mode, query) = if mode == Mode::Calc {
        (Mode::Fuzzy, input.to_string())
    } else {
        (mode, query)
    };
    let (query_filters, stripped) = filters::parse(&query, unix_now());
    let query = if query_filters.is_empty() {
        query
    } else {
        stripped
    };
    match mode {
        Mode::Semantic => semantic(&query, &query_filters, on_hit),
        Mode::Content => content(store, &query, &query_filters, opts, on_hit),
        Mode::Fuzzy | Mode::Regex | Mode::Calc => {
            filename(store, &query, &query_filters, opts, on_hit)
        }
    }
}

/// Ranks eligible documents against the semantic index, applying filters
/// before the result limit, just like the engine's semantic worker.
fn semantic(
    query: &str,
    query_filters: &Filters,
    on_hit: &mut dyn FnMut(Hit),
) -> Result<bool, String> {
    let mut embedder = sem::make_embedder()?;
    let store = match sem::SemStore::load(&sem::default_store_path()) {
        Some(s) if s.dim as usize == embedder.dim() => s,
        Some(_) => {
            return Err(
                "semantic index is from another model — rerun fsearch --index-semantic".to_string(),
            );
        }
        None => {
            return Err("no semantic index yet — run fsearch --index-semantic".to_string());
        }
    };
    let qv = embedder.embed(&[query.to_string()])?;
    let mut reported = 0usize;
    for hit in store.query_filtered(&qv[0], SEMANTIC_LIMIT, |doc| {
        query_filters.is_empty()
            || (query_filters.matches(&doc.path)
                && query_filters.matches_meta(&FileMeta {
                    mtime: doc.mtime,
                    size: doc.size,
                }))
    }) {
        let doc = &store.docs[hit.doc];
        reported += 1;
        on_hit(Hit::Semantic {
            path: doc.path.clone(),
            line_start: hit.line_start,
            score: hit.score,
        });
    }
    Ok(reported > 0)
}

fn content(
    store: &PathStore,
    pattern: &str,
    query_filters: &Filters,
    opts: &Options,
    on_hit: &mut dyn FnMut(Hit),
) -> Result<bool, String> {
    // scope the grep with any ext:/path: filters from the query
    let indices: Vec<usize> = (0..store.len())
        .filter(|&i| {
            query_filters.is_empty()
                || (query_filters.matches(store.get(i))
                    && query_filters.matches_meta(&store.meta(i)))
        })
        .collect();
    let (tx, rx) = mpsc::sync_channel::<ContentMatch>(content::QUEUE_CAPACITY);
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            let r = content::search_with_sink(
                &indices,
                |i| store.get(i),
                pattern,
                opts.max_content_filesize,
                &opts.pdf_cache,
                &cancel,
                usize::MAX,
                |hit| content::send_cancellable(&tx, hit, &cancel),
            )
            .map_err(|e| format!("invalid pattern: {e}"));
            drop(tx);
            r
        });
        let mut any = false;
        for hit in rx {
            any = true;
            on_hit(Hit::Line {
                path: hit.path,
                line_number: hit.line_number,
                line: hit.line,
            });
        }
        handle
            .join()
            .map_err(|_| "content search failed".to_string())?
            .map(|_| any)
    })
}

fn filename(
    store: &PathStore,
    query: &str,
    query_filters: &Filters,
    opts: &Options,
    on_hit: &mut dyn FnMut(Hit),
) -> Result<bool, String> {
    let r = matcher::search_boosted(
        store,
        query,
        FilenameMode::Fuzzy,
        FILENAME_LIMIT,
        &HashMap::new(),
        query_filters,
        &opts.quiet,
    )
    .map_err(|e| e.to_string())?;
    // scripting keeps the old behavior: report only the strong matches,
    // not the fold-away weaker tail
    let mut reported = false;
    for i in r.indices.iter().take(r.strong) {
        on_hit(Hit::Path(store.get(*i).to_string()));
        reported = true;
    }
    Ok(reported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content_fixture() -> (tempfile::TempDir, PathStore, Options) {
        let dir = tempfile::tempdir().unwrap();
        let entries: Vec<_> = (0..60)
            .map(|i| {
                let path = dir.path().join(format!("{i}.txt"));
                std::fs::write(&path, "needle\n".repeat(20)).unwrap();
                (path.to_string_lossy().into_owned(), FileMeta::default())
            })
            .collect();
        let opts = Options {
            max_content_filesize: 1024,
            quiet: Quiet::new(Vec::new()),
            pdf_cache: dir.path().join("cache"),
        };
        (dir, PathStore::from_entries(&entries), opts)
    }

    #[test]
    fn slow_headless_callback_streams_all_hits_without_interactive_cap() {
        let (_dir, store, opts) = content_fixture();
        let mut count = 0;
        assert!(
            search(&store, ">needle", &opts, &mut |_| {
                count += 1;
                std::thread::sleep(std::time::Duration::from_micros(50));
            })
            .unwrap()
        );
        assert_eq!(count, 1200);
    }

    #[test]
    fn panicking_callback_disconnects_blocked_producers() {
        let (_dir, store, opts) = content_fixture();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = search(&store, ">needle", &opts, &mut |_| {
                    panic!("callback stopped")
                });
            }))
            .is_err();
            tx.send(panicked).unwrap();
        });
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn filename_search_reports_false_when_only_folded_rows_exist() {
        let store = PathStore::from_entries(&[("/quiet/state".to_string(), FileMeta::default())]);
        let opts = Options {
            max_content_filesize: 1_000,
            quiet: Quiet::new(vec!["/quiet/".to_string()]),
            pdf_cache: std::path::PathBuf::new(),
        };
        let mut hits = 0;
        assert!(!search(&store, "", &opts, &mut |_| hits += 1).unwrap());
        assert_eq!(hits, 0);
        assert!(search(&store, "state", &opts, &mut |_| hits += 1).unwrap());
        assert_eq!(hits, 1);
    }
}
