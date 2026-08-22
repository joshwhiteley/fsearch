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

/// Ranks the query against the semantic index, mirroring the engine's
/// semantic worker: over-fetch when filters are active (they are applied
/// after ranking) so filtering doesn't drop the recall count.
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
    let fetch = if query_filters.is_empty() {
        SEMANTIC_LIMIT
    } else {
        SEMANTIC_LIMIT * 4
    };
    let mut reported = 0usize;
    for hit in store.query(&qv[0], fetch) {
        if reported >= SEMANTIC_LIMIT {
            break;
        }
        let doc = &store.docs[hit.doc];
        if !(query_filters.is_empty()
            || (query_filters.matches(&doc.path)
                && query_filters.matches_meta(&FileMeta {
                    mtime: doc.mtime,
                    size: doc.size,
                })))
        {
            continue;
        }
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
    let (tx, rx) = mpsc::channel::<ContentMatch>();
    let cancel = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            let r = content::search(
                &indices,
                |i| store.get(i),
                pattern,
                opts.max_content_filesize,
                &opts.pdf_cache,
                &cancel,
                &tx,
            );
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
        handle.join().expect("content search panicked").map(|_| any)
    });
    result.map_err(|e| format!("invalid pattern: {e}"))
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
    for i in r.indices.iter().take(r.strong) {
        on_hit(Hit::Path(store.get(*i).to_string()));
    }
    Ok(!r.indices.is_empty())
}
