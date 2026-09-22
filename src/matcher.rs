use crate::filters::Filters;
use crate::index::PathStore;
use crate::quiet::Quiet;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rayon::prelude::*;
use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilenameMode {
    Fuzzy,
    Regex,
}

/// Ranked indices plus how many lead entries are "strong" (above the
/// relative score floor). Non-fuzzy modes report everything strong.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ranked {
    pub indices: Vec<usize>,
    pub strong: usize,
}

impl Ranked {
    pub fn len(&self) -> usize {
        self.indices.len()
    }
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

pub fn search(
    store: &PathStore,
    query: &str,
    mode: FilenameMode,
    limit: usize,
) -> Result<Ranked, String> {
    search_boosted(
        store,
        query,
        mode,
        limit,
        &HashMap::new(),
        &Filters::default(),
        &Quiet::new(Vec::new()),
    )
}

/// Like [`search`], with per-path ranking boosts (from open history).
pub fn search_boosted(
    store: &PathStore,
    query: &str,
    mode: FilenameMode,
    limit: usize,
    boosts: &HashMap<String, u32>,
    filters: &Filters,
    quiet: &Quiet,
) -> Result<Ranked, String> {
    search_with_scope(
        store,
        query,
        mode,
        limit,
        boosts,
        MatchScope {
            filters,
            lines: false,
        },
        quiet,
        None,
    )
    .map(|result| result.expect("uncancelled search cannot be cancelled"))
}

/// Like [`search_boosted`], but drops work when a newer engine generation is
/// published. The existing search functions remain synchronous and unchanged.
pub fn search_boosted_generation(
    store: &PathStore,
    query: &str,
    mode: FilenameMode,
    limit: usize,
    boosts: &HashMap<String, u32>,
    filters: &Filters,
    quiet: &Quiet,
    generation: u64,
    current_generation: &AtomicU64,
) -> Result<Option<Ranked>, String> {
    search_with_scope(
        store,
        query,
        mode,
        limit,
        boosts,
        MatchScope {
            filters,
            lines: false,
        },
        quiet,
        Some((generation, current_generation)),
    )
}

/// Match arbitrary stdin lines without interpreting a trailing slash as a
/// filesystem directory. Explicit filters keep their normal meaning.
pub fn search_lines(
    store: &PathStore,
    query: &str,
    mode: FilenameMode,
    limit: usize,
    filters: &Filters,
) -> Result<Ranked, String> {
    search_with_scope(
        store,
        query,
        mode,
        limit,
        &HashMap::new(),
        MatchScope {
            filters,
            lines: true,
        },
        &Quiet::new(Vec::new()),
        None,
    )
    .map(|result| result.expect("uncancelled search cannot be cancelled"))
}

pub fn search_lines_generation(
    store: &PathStore,
    query: &str,
    mode: FilenameMode,
    limit: usize,
    filters: &Filters,
    generation: u64,
    current_generation: &AtomicU64,
) -> Result<Option<Ranked>, String> {
    search_with_scope(
        store,
        query,
        mode,
        limit,
        &HashMap::new(),
        MatchScope {
            filters,
            lines: true,
        },
        &Quiet::new(Vec::new()),
        Some((generation, current_generation)),
    )
}

#[derive(Clone, Copy)]
struct MatchScope<'a> {
    filters: &'a Filters,
    lines: bool,
}

fn search_with_scope(
    store: &PathStore,
    query: &str,
    mode: FilenameMode,
    limit: usize,
    boosts: &HashMap<String, u32>,
    scope: MatchScope<'_>,
    quiet: &Quiet,
    generation: Option<(u64, &AtomicU64)>,
) -> Result<Option<Ranked>, String> {
    let filters = scope.filters;
    if cancelled(generation) {
        return Ok(None);
    }
    // a `/` in the query, a path: filter, or dir: means the user is
    // navigating paths on purpose — quiet demotion switches off
    let path_intent = query.contains('/') || !filters.path_terms.is_empty() || filters.dirs_only;
    let demote = (!quiet.is_empty() && !path_intent).then_some(quiet);
    if query.is_empty() {
        return Ok(head_with_boosts(
            store, limit, boosts, scope, demote, generation,
        ));
    }
    match mode {
        FilenameMode::Fuzzy => Ok(fuzzy(
            store, query, limit, boosts, scope, demote, generation,
        )),
        FilenameMode::Regex => regex_filter(store, query, limit, boosts, scope, generation),
    }
}

fn cancelled(generation: Option<(u64, &AtomicU64)>) -> bool {
    generation.is_some_and(|(job, current)| current.load(Ordering::Acquire) != job)
}

fn passes(store: &PathStore, i: usize, scope: MatchScope<'_>) -> bool {
    let filters = scope.filters;
    let path = store.get(i);
    let line_matches = scope.lines
        && !filters.dirs_only
        && filters.exts.is_empty()
        && filters
            .path_terms
            .iter()
            .all(|term| crate::filters::contains_ignore_ascii_case(path, term));
    (line_matches || filters.matches(path)) && filters.matches_meta(&store.meta(i))
}

/// Fuzzy filename searches may surface directories, while every other mode
/// keeps the filter's default file-only behavior. Extension filters still
/// exclude directories, and metadata/path filters apply to them normally.
fn passes_fuzzy(store: &PathStore, i: usize, scope: MatchScope<'_>) -> bool {
    let filters = scope.filters;
    if passes(store, i, scope) {
        return true;
    }
    let path = store.get(i);
    path.ends_with('/')
        && !filters.dirs_only
        && filters.exts.is_empty()
        && filters
            .path_terms
            .iter()
            .all(|term| crate::filters::contains_ignore_ascii_case(path, term))
        && filters.matches_meta(&store.meta(i))
}

/// First `limit` entries (already newest-first), with boosted paths —
/// wherever they sit in the full list — floated to the front.
fn head_with_boosts(
    store: &PathStore,
    limit: usize,
    boosts: &HashMap<String, u32>,
    scope: MatchScope<'_>,
    demote: Option<&Quiet>,
    generation: Option<(u64, &AtomicU64)>,
) -> Option<Ranked> {
    // frecency-boosted entries first (opening something is an explicit
    // signal, quiet or not), then plain entries newest-first; quiet paths
    // sink into a trailing block behind the weaker-matches fold, which
    // keeps log/state churn off the launch screen
    let mut out: Vec<usize> = Vec::new();
    let mut in_boosted: std::collections::HashSet<usize> = std::collections::HashSet::new();
    if !boosts.is_empty() {
        let mut boosted: Vec<(u32, usize)> = Vec::new();
        for i in 0..store.len() {
            if i % CHUNK == 0 && cancelled(generation) {
                return None;
            }
            if passes(store, i, scope)
                && let Some(&b) = boosts.get(store.get(i))
            {
                boosted.push((b, i));
            }
        }
        boosted.sort_by_key(|&(b, i)| (std::cmp::Reverse(b), i));
        out.extend(boosted.iter().map(|&(_, i)| i));
        in_boosted.extend(out.iter().copied());
    }
    let mut normal: Vec<usize> = Vec::new();
    let mut quiet_tail: Vec<usize> = Vec::new();
    for i in 0..store.len() {
        if i % CHUNK == 0 && cancelled(generation) {
            return None;
        }
        if normal.len() >= limit {
            break;
        }
        if in_boosted.contains(&i) || !passes(store, i, scope) {
            continue;
        }
        if demote.is_some_and(|q| q.is_quiet(store.get(i))) {
            if quiet_tail.len() < limit {
                quiet_tail.push(i);
            }
        } else {
            normal.push(i);
        }
    }
    out.extend(normal);
    let strong = out.len().min(limit);
    out.extend(quiet_tail);
    out.truncate(limit);
    Some(Ranked {
        indices: out,
        strong,
    })
}

fn apply_boost_order(hits: &mut [usize], store: &PathStore, boosts: &HashMap<String, u32>) {
    if !boosts.is_empty() {
        // stable: unboosted hits keep their recency order
        hits.sort_by_key(|&i| std::cmp::Reverse(boosts.get(store.get(i)).copied().unwrap_or(0)));
    }
}

/// Re-runs a fuzzy pattern against single strings to recover the matched
/// character positions (for highlighting the visible result rows).
pub struct Highlighter {
    pattern: Pattern,
    matcher: Matcher,
}

impl Highlighter {
    pub fn new(query: &str) -> Highlighter {
        let mut cfg = Config::DEFAULT;
        cfg.set_match_paths();
        Highlighter {
            pattern: Pattern::parse(query, CaseMatching::Smart, Normalization::Smart),
            matcher: Matcher::new(cfg),
        }
    }

    /// Matched char positions in `text`, sorted and deduplicated.
    pub fn positions(&mut self, text: &str) -> Vec<u32> {
        let mut buf = Vec::new();
        let mut indices = Vec::new();
        self.pattern.indices(
            Utf32Str::new(text, &mut buf),
            &mut self.matcher,
            &mut indices,
        );
        indices.sort_unstable();
        indices.dedup();
        indices
    }
}

const CHUNK: usize = 16_384;

/// Floor on how many fuzzy results survive the low-score cutoff, so a
/// "best / 2" tail trim never empties the list of an only-match.
const MIN_KEEP: usize = 8;

fn last_segment(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// Returns the final two path components without a leading separator.
fn last_two_segments(path: &str) -> Option<&str> {
    let path = path.trim_end_matches('/');
    let last_separator = path.rfind('/')?;
    let parent = &path[..last_separator];
    let pair_start = parent.rfind('/').map_or(0, |separator| separator + 1);
    Some(&path[pair_start..])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Candidate {
    score: u32,
    index: usize,
    quiet: bool,
}

/// The heap keeps the worst retained candidate at its top. This bounds the
/// ranking allocation to the requested result count while the final sort
/// still uses the exact old score/index order.
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.quiet
            .cmp(&other.quiet)
            .then_with(|| other.score.cmp(&self.score))
            .then_with(|| self.index.cmp(&other.index))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

struct TopK {
    cap: usize,
    heap: BinaryHeap<Candidate>,
}

impl TopK {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            heap: BinaryHeap::with_capacity(cap),
        }
    }

    fn push(&mut self, candidate: Candidate) {
        if self.cap == 0 {
            return;
        }
        if self.heap.len() < self.cap {
            self.heap.push(candidate);
        } else if self.heap.peek().is_some_and(|worst| candidate < *worst) {
            let _ = self.heap.pop();
            self.heap.push(candidate);
        }
    }

    fn merge(&mut self, other: Self) {
        for candidate in other.heap {
            self.push(candidate);
        }
    }

    fn into_sorted(self) -> Vec<Candidate> {
        let mut candidates = self.heap.into_vec();
        candidates.sort_unstable_by(|a, b| {
            a.quiet
                .cmp(&b.quiet)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.index.cmp(&b.index))
        });
        candidates
    }
}

struct FuzzyAccum {
    matcher: Matcher,
    buf: Vec<char>,
    top: TopK,
    processed: usize,
    cancelled: bool,
}

impl FuzzyAccum {
    fn new(limit: usize) -> Self {
        let mut cfg = Config::DEFAULT;
        cfg.set_match_paths();
        Self {
            matcher: Matcher::new(cfg),
            buf: Vec::new(),
            top: TopK::new(limit),
            processed: 0,
            cancelled: false,
        }
    }
}

fn fuzzy(
    store: &PathStore,
    query: &str,
    limit: usize,
    boosts: &HashMap<String, u32>,
    scope: MatchScope<'_>,
    demote: Option<&Quiet>,
    generation: Option<(u64, &AtomicU64)>,
) -> Option<Ranked> {
    let filters = scope.filters;
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let multi_word = query.split_whitespace().count() > 1;
    let accumulated = (0..store.len())
        .into_par_iter()
        .with_min_len(CHUNK)
        .fold(
            || FuzzyAccum::new(limit),
            |mut acc, i| {
                if acc.cancelled {
                    return acc;
                }
                if acc.processed % CHUNK == 0 && cancelled(generation) {
                    acc.cancelled = true;
                    return acc;
                }
                acc.processed += 1;
                if passes_fuzzy(store, i, scope) {
                    let path = store.get(i);
                    let is_dir = !scope.lines && path.ends_with('/');
                    if let Some(score) =
                        pattern.score(Utf32Str::new(path, &mut acc.buf), &mut acc.matcher)
                    {
                        // A directory is useful only when its own name matches,
                        // rather than merely inheriting a match from an ancestor.
                        // Path-intent and dir: queries retain full-path matching.
                        let name = last_segment(path);
                        let name_score =
                            pattern.score(Utf32Str::new(name, &mut acc.buf), &mut acc.matcher);
                        if is_dir
                            && !filters.dirs_only
                            && filters.path_terms.is_empty()
                            && !query.contains('/')
                            && name_score.is_none()
                        {
                            return acc;
                        }
                        // a query that also matches within the filename alone is far more
                        // likely what the user meant than letters scattered across the path;
                        // adding the basename score roughly doubles such results
                        let fname_bonus = name_score.unwrap_or(0);
                        // Multi-word queries can express intent split across a project
                        // directory and its file name. Keep this bonus below the filename
                        // bonus so single-component filename ranking stays unchanged.
                        let pair_bonus = if multi_word && !is_dir {
                            last_two_segments(path)
                                .and_then(|pair| {
                                    pattern
                                        .score(Utf32Str::new(pair, &mut acc.buf), &mut acc.matcher)
                                })
                                .map_or(0, |pair_score| pair_score / 2)
                        } else {
                            0
                        };
                        let boost = boosts.get(path).copied().unwrap_or(0);
                        let mut total = score + fname_bonus + pair_bonus + boost;
                        // quiet paths score at 2/5: even with a filename
                        // match they land under the best/2 floor whenever a
                        // non-quiet candidate exists, i.e. behind the fold
                        if demote.is_some_and(|q| q.is_quiet(path)) {
                            total = total * 2 / 5;
                        }
                        acc.top.push(Candidate {
                            score: total,
                            index: i,
                            quiet: demote.is_some_and(|q| q.is_quiet(path)),
                        });
                    }
                }
                acc
            },
        )
        .reduce(
            || FuzzyAccum::new(limit),
            |mut a, b| {
                a.cancelled |= b.cancelled;
                a.top.merge(b.top);
                a
            },
        );
    if accumulated.cancelled || cancelled(generation) {
        return None;
    }
    let scored = accumulated.top.into_sorted();
    // Because filename matches score roughly double (change 1), a "best / 2"
    // floor self-regulates: when a real filename match exists, scattered
    // path-only matches fall below it and disappear; when nothing matches the
    // filename, all candidates score within range of each other and survive.
    let floor_strong = |scored: &[Candidate]| {
        let mut s = 0;
        if let Some(&best) = scored.first() {
            let floor = best.score / 2;
            s = scored.partition_point(|candidate| candidate.score >= floor);
            s = s.max(MIN_KEEP.min(scored.len()));
        }
        s
    };
    // The top-k comparator already performs the stable quiet partition. The
    // selected prefix is sufficient to compute the visible fold: if more
    // candidates existed, truncating to `limit` would cap `strong` there too.
    let non_quiet = scored.partition_point(|candidate| !candidate.quiet);
    let strong = if let Some(_) = demote {
        if non_quiet > 0 {
            floor_strong(&scored[..non_quiet])
        } else {
            floor_strong(&scored)
        }
    } else {
        floor_strong(&scored)
    }
    .min(scored.len());
    let indices = scored
        .into_iter()
        .map(|candidate| candidate.index)
        .collect();
    Some(Ranked { indices, strong })
}

fn regex_filter(
    store: &PathStore,
    query: &str,
    limit: usize,
    boosts: &HashMap<String, u32>,
    scope: MatchScope<'_>,
    generation: Option<(u64, &AtomicU64)>,
) -> Result<Option<Ranked>, String> {
    let smart_case_insensitive = !query.chars().any(|c| c.is_uppercase());
    let re = regex::RegexBuilder::new(query)
        .case_insensitive(smart_case_insensitive)
        .build()
        .map_err(|e| e.to_string())?;
    let mut hits: Vec<usize> = (0..store.len())
        .into_par_iter()
        .with_min_len(CHUNK)
        .filter(|&i| {
            (i % CHUNK != 0 || !cancelled(generation))
                && passes(store, i, scope)
                && re.is_match(store.get(i))
        })
        .collect();
    if cancelled(generation) {
        return Ok(None);
    }
    hits.sort_unstable();
    apply_boost_order(&mut hits, store, boosts);
    hits.truncate(limit);
    let strong = hits.len();
    Ok(Some(Ranked {
        indices: hits,
        strong,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> PathStore {
        let entries: Vec<(String, crate::walker::FileMeta)> = v
            .iter()
            .map(|s| (s.to_string(), Default::default()))
            .collect();
        PathStore::from_entries(&entries)
    }

    #[test]
    fn arbitrary_lines_with_trailing_slashes_match_in_all_modes() {
        let store = paths(&["alpha/nested/", "alpha.txt", "beta/"]);
        for mode in [FilenameMode::Fuzzy, FilenameMode::Regex] {
            assert_eq!(
                search_lines(&store, "", mode, 10, &Filters::default())
                    .unwrap()
                    .indices,
                [0, 1, 2]
            );
            let hits = search_lines(&store, "alpha", mode, 10, &Filters::default()).unwrap();
            assert_eq!(hits.len(), 2);
            assert!(
                hits.indices.contains(&0),
                "full-line match, not just basename"
            );
            for (input, expected) in [
                ("path:alpha", vec![0, 1]),
                ("ext:txt", vec![1]),
                ("dir:", vec![0, 2]),
            ] {
                let (filters, query) = crate::filters::parse(input, 0);
                assert_eq!(
                    search_lines(&store, &query, mode, 10, &filters)
                        .unwrap()
                        .indices,
                    expected
                );
            }
        }
    }

    #[test]
    fn empty_query_returns_head() {
        let p = paths(&["/a", "/b", "/c"]);
        assert_eq!(
            search(&p, "", FilenameMode::Fuzzy, 2).unwrap().indices,
            vec![0, 1]
        );
    }

    #[test]
    fn fuzzy_ranks_filename_match_over_scattered() {
        let p = paths(&[
            "/code/rust/tools/everything/notes.txt", // scattered match for "rest"
            "/docs/rest-api.md",                     // filename match
        ]);
        let r = search(&p, "rest", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r.indices[0], 1);
    }

    #[test]
    fn fuzzy_is_smart_case() {
        let p = paths(&["/docs/README.md", "/docs/readme-draft.md"]);
        // lowercase query matches both
        assert_eq!(
            search(&p, "readme", FilenameMode::Fuzzy, 10)
                .unwrap()
                .indices
                .len(),
            2
        );
        // uppercase query matches only the uppercase path
        let r = search(&p, "README", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r.indices, vec![0]);
    }

    #[test]
    fn regex_filters_by_full_path() {
        let p = paths(&["/a/report_2024.pdf", "/a/report.txt", "/b/2024.pdf"]);
        let r = search(&p, r"report_\d+\.pdf$", FilenameMode::Regex, 10).unwrap();
        assert_eq!(r.indices, vec![0]);
    }

    #[test]
    fn regex_is_smart_case() {
        let p = paths(&["/a/README.md", "/a/readme.md"]);
        assert_eq!(
            search(&p, "readme", FilenameMode::Regex, 10)
                .unwrap()
                .indices
                .len(),
            2
        );
        assert_eq!(
            search(&p, "README", FilenameMode::Regex, 10)
                .unwrap()
                .indices,
            vec![0]
        );
    }

    #[test]
    fn invalid_regex_is_err() {
        let p = paths(&["/a"]);
        assert!(search(&p, "[unclosed", FilenameMode::Regex, 10).is_err());
    }

    #[test]
    fn boosts_break_fuzzy_ties_and_order_lists() {
        let p = paths(&["/docs/readme-a.md", "/docs/readme-b.md"]);
        let mut boosts = HashMap::new();
        boosts.insert("/docs/readme-b.md".to_string(), 50u32);
        // identical fuzzy quality: boost wins
        let r = search_boosted(
            &p,
            "readme",
            FilenameMode::Fuzzy,
            10,
            &boosts,
            &Filters::default(),
            &Quiet::new(Vec::new()),
        )
        .unwrap();
        assert_eq!(r.indices[0], 1);
        // empty query: boosted file floats to the top
        let r = search_boosted(
            &p,
            "",
            FilenameMode::Fuzzy,
            10,
            &boosts,
            &Filters::default(),
            &Quiet::new(Vec::new()),
        )
        .unwrap();
        assert_eq!(r.indices, vec![1, 0]);
        // regex: boosted file first, others keep index order
        let r = search_boosted(
            &p,
            "readme",
            FilenameMode::Regex,
            10,
            &boosts,
            &Filters::default(),
            &Quiet::new(Vec::new()),
        )
        .unwrap();
        assert_eq!(r.indices, vec![1, 0]);
    }

    #[test]
    fn filters_narrow_all_modes() {
        let p = paths(&["/docs/a.pdf", "/docs/a.txt", "/docs/sub/", "/extra/b.pdf"]);
        let (f, _) = crate::filters::parse("ext:pdf path:docs x", 0);
        let none = HashMap::new();
        // empty query honors filters
        let r = search_boosted(
            &p,
            "",
            FilenameMode::Fuzzy,
            10,
            &none,
            &f,
            &Quiet::new(Vec::new()),
        )
        .unwrap();
        assert_eq!(r.indices, vec![0]);
        // fuzzy honors filters
        let r = search_boosted(
            &p,
            "a",
            FilenameMode::Fuzzy,
            10,
            &none,
            &f,
            &Quiet::new(Vec::new()),
        )
        .unwrap();
        assert_eq!(r.indices, vec![0]);
        // dirs only with dir:
        let (fd, _) = crate::filters::parse("dir: x", 0);
        let r = search_boosted(
            &p,
            "",
            FilenameMode::Fuzzy,
            10,
            &none,
            &fd,
            &Quiet::new(Vec::new()),
        )
        .unwrap();
        assert_eq!(r.indices, vec![2]);
        // default (no filters) excludes dirs
        let r = search(&p, "", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r.indices, vec![0, 1, 3]);
    }

    #[test]
    fn quiet_paths_fold_behind_strong_matches() {
        let p = paths(&[
            "/Users/j/Documents/cisco-notes.md",
            "/Users/j/.cisco/vpn/log/UIHistory.txt",
            "/Users/j/Library/Application Support/Cisco/state.json",
        ]);
        let q = Quiet::default();
        let r = search_boosted(
            &p,
            "cisco",
            FilenameMode::Fuzzy,
            10,
            &HashMap::new(),
            &Filters::default(),
            &q,
        )
        .unwrap();
        // the real document is strong; the log/state churn sits behind the fold
        assert_eq!(r.indices[0], 0);
        assert_eq!(r.strong, 1, "quiet matches must fall below the floor");
        assert_eq!(r.indices.len(), 3, "still reachable via ctrl-x");
    }

    #[test]
    fn slash_in_query_disables_quiet_demotion() {
        let p = paths(&[
            "/Users/j/Documents/cisco-notes.md",
            "/Users/j/.cisco/vpn/log/UIHistory.txt",
        ]);
        let q = Quiet::default();
        let r = search_boosted(
            &p,
            "cisco/",
            FilenameMode::Fuzzy,
            10,
            &HashMap::new(),
            &Filters::default(),
            &q,
        )
        .unwrap();
        // path intent: the hidden-dir hit competes on equal terms
        assert_eq!(r.strong, r.indices.len());
        assert!(r.indices.contains(&1));
    }

    #[test]
    fn all_quiet_matches_stay_visible() {
        let p = paths(&[
            "/Users/j/.config/nvim/init.lua",
            "/Users/j/.config/nvim/lazy-lock.json",
        ]);
        let q = Quiet::default();
        let r = search_boosted(
            &p,
            "nvim",
            FilenameMode::Fuzzy,
            10,
            &HashMap::new(),
            &Filters::default(),
            &q,
        )
        .unwrap();
        // demotion is relative: with no louder candidate, nothing folds
        assert_eq!(r.strong, 2);
    }

    #[test]
    fn launch_screen_head_sinks_quiet_churn() {
        let p = paths(&[
            "/Users/j/Library/Biome/sessions/heartbeat", // newest, junk
            "/Users/j/.cisco/vpn/log/UIHistory.txt",
            "/Users/j/Documents/report.md",
            "/Users/j/Desktop/photo.png",
        ]);
        let q = Quiet::default();
        let r = search_boosted(
            &p,
            "",
            FilenameMode::Fuzzy,
            10,
            &HashMap::new(),
            &Filters::default(),
            &q,
        )
        .unwrap();
        assert_eq!(&r.indices[..2], &[2, 3], "real files first");
        assert_eq!(r.strong, 2);
        assert_eq!(r.indices.len(), 4, "churn folded, not hidden");
    }

    #[test]
    fn highlighter_finds_match_positions() {
        let mut h = Highlighter::new("rest");
        let pos = h.positions("/docs/rest-api.md");
        // "rest" sits at chars 6..10
        assert_eq!(pos, vec![6, 7, 8, 9]);
        // non-matching text yields no positions
        assert!(h.positions("/zzz").is_empty());
    }

    #[test]
    fn passport_ranks_real_files_and_cuts_scattered_junk() {
        let mut owned: Vec<String> = vec![
            "/documents/passport.pdf".to_string(),
            "/scans/Passport-2024.jpg".to_string(),
        ];
        // each decoy carries p-a-s-s-p-o-r-t scattered across
        // pkgs/assets/support but its filename does not match
        owned.extend((0..30).map(|i| format!("/code{i}/pkgs/assets/support/notes.txt")));
        let strs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        let p = paths(&strs);
        let r = search(&p, "passport", FilenameMode::Fuzzy, 500).unwrap();
        // the two real files are the top two results, in some order
        assert_ne!(r.indices[0], r.indices[1]);
        assert!(r.indices[0] == 0 || r.indices[0] == 1);
        assert!(r.indices[1] == 0 || r.indices[1] == 1);
        // more than 8 candidates matched (the decoys are real subsequence
        // matches), so a strong count of exactly MIN_KEEP marks the relative
        // score floor while the decoys live on as fold-away weaker matches
        assert_eq!(r.strong, 8);
        // the decoys are retained after the strong block, ready to be
        // revealed with ctrl-x
        assert_eq!(r.indices.len(), 32);
    }

    #[test]
    fn exact_atom_requires_contiguous_substring() {
        let p = paths(&["/a/passport.pdf", "/a/pass_port.txt"]);
        // plain fuzzy matches the underscore-scattered path as a subsequence
        assert_eq!(
            search(&p, "passport", FilenameMode::Fuzzy, 10)
                .unwrap()
                .indices
                .len(),
            2
        );
        // the ' atom requires a contiguous substring
        assert_eq!(
            search(&p, "'passport", FilenameMode::Fuzzy, 10)
                .unwrap()
                .indices,
            vec![0]
        );
    }

    #[test]
    fn filename_bonus_outranks_path_only_match() {
        let p = paths(&["/passport/archive/list.txt", "/misc/passport.pdf"]);
        let r = search(&p, "passport", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r.indices[0], 1);
    }

    #[test]
    fn project_queries_rank_directories_and_files_inside_them() {
        let p = paths(&[
            "/Users/j/Documents/sage-kc/",
            "/Users/j/Documents/sage-kc/README.md",
            "/Users/j/Documents/sage-kc/src/main.rs",
            "/Users/j/Documents/Sage Kc.md",
            "/Users/j/Documents/staging/keep/cache.txt",
        ]);
        let r = search(&p, "sage kc", FilenameMode::Fuzzy, 10).unwrap();
        let directory = r.indices.iter().position(|&i| i == 0).unwrap();
        assert!(directory < 3, "project directory ranked at {directory}");

        let noise = r.indices.iter().position(|&i| i == 4).unwrap();
        let inside = r.indices.iter().position(|&i| i == 1 || i == 2).unwrap();
        assert!(inside < noise, "project file ranked above noise");
    }

    #[test]
    fn fuzzy_directories_need_a_last_segment_match() {
        let p = paths(&[
            "/Users/j/Documents/sage-kc/",
            "/Users/j/Documents/sage-kc/archive/",
            "/Users/j/Documents/noise.txt",
        ]);
        let r = search(&p, "sage kc", FilenameMode::Fuzzy, 10).unwrap();
        assert!(r.indices.contains(&0));
        assert!(
            !r.indices.contains(&1),
            "nested directory matched only through its parent"
        );
    }

    #[test]
    fn path_intent_can_find_a_nested_directory() {
        let p = paths(&["/Users/j/Documents/sage-kc/archive/"]);
        let (filters, query) = crate::filters::parse("path:sage-kc archive", 0);
        let r = search_boosted(
            &p,
            &query,
            FilenameMode::Fuzzy,
            10,
            &HashMap::new(),
            &filters,
            &Quiet::new(Vec::new()),
        )
        .unwrap();
        assert_eq!(r.indices, vec![0]);
    }

    #[test]
    fn single_word_queries_keep_filename_ordering() {
        let p = paths(&["/sage-kc/readme.txt", "/misc/readme.txt"]);
        let r = search(&p, "readme", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r.indices, vec![0, 1]);
    }

    #[test]
    fn limit_is_respected() {
        let owned: Vec<String> = (0..100).map(|i| format!("/f/file{i}.txt")).collect();
        let strs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        let p = paths(&strs);
        assert_eq!(
            search(&p, "file", FilenameMode::Fuzzy, 5)
                .unwrap()
                .indices
                .len(),
            5
        );
    }

    #[test]
    fn bounded_fuzzy_order_matches_full_sort_oracle() {
        let mut owned: Vec<String> = (0..240)
            .map(|i| match i % 6 {
                0 => format!("/docs/project-{i}/report-{i}.md"),
                1 => format!("/Users/j/Library/state/report-{i}.json"),
                2 => format!("/misc/archive/re-pair-{i}.txt"),
                3 => format!("/docs/é/{i}/résumé-{i}.md"),
                4 => format!("/noise-{i}/support/notes.txt"),
                _ => format!("/docs/report-{i}.bak/"),
            })
            .collect();
        owned.extend([
            "/docs/report-final.md".to_string(),
            "/Users/j/Library/report-final.json".to_string(),
            "/docs/project-final/README.md".to_string(),
        ]);
        let path_refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        let store = paths(&path_refs);
        let mut boosts = HashMap::new();
        boosts.insert("/docs/report-final.md".to_string(), 50);
        boosts.insert("/misc/archive/re-pair-2.txt".to_string(), 50);
        let quiet = Quiet::default();
        let actual = search_boosted(
            &store,
            "report",
            FilenameMode::Fuzzy,
            17,
            &boosts,
            &Filters::default(),
            &quiet,
        )
        .unwrap();

        let pattern = Pattern::parse("report", CaseMatching::Smart, Normalization::Smart);
        let mut cfg = Config::DEFAULT;
        cfg.set_match_paths();
        let mut matcher = Matcher::new(cfg);
        let mut buf = Vec::new();
        let mut scored = Vec::new();
        for i in 0..store.len() {
            let filters = Filters::default();
            if !passes_fuzzy(
                &store,
                i,
                MatchScope {
                    filters: &filters,
                    lines: false,
                },
            ) {
                continue;
            }
            let path = store.get(i);
            let Some(score) = pattern.score(Utf32Str::new(path, &mut buf), &mut matcher) else {
                continue;
            };
            let name = last_segment(path);
            let name_score = pattern.score(Utf32Str::new(name, &mut buf), &mut matcher);
            let is_dir = path.ends_with('/');
            if is_dir && name_score.is_none() {
                continue;
            }
            let boost = boosts.get(path).copied().unwrap_or(0);
            let quiet_hit = quiet.is_quiet(path);
            let mut total = score + name_score.unwrap_or(0) + boost;
            if quiet_hit {
                total = total * 2 / 5;
            }
            scored.push(Candidate {
                score: total,
                index: i,
                quiet: quiet_hit,
            });
        }
        scored.sort_unstable_by(|a, b| {
            a.quiet
                .cmp(&b.quiet)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.index.cmp(&b.index))
        });
        let non_quiet = scored.partition_point(|candidate| !candidate.quiet);
        let floor = |candidates: &[Candidate]| {
            candidates
                .partition_point(|candidate| {
                    candidate.score >= candidates.first().map_or(0, |best| best.score / 2)
                })
                .max(MIN_KEEP.min(candidates.len()))
        };
        let strong = if non_quiet > 0 {
            floor(&scored[..non_quiet])
        } else {
            floor(&scored)
        };
        scored.truncate(17);
        let expected = Ranked {
            indices: scored.iter().map(|candidate| candidate.index).collect(),
            strong: strong.min(scored.len()),
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn stale_generation_is_cancelled_without_changing_legacy_api() {
        let store = paths(&["/a/report.txt", "/b/report.txt"]);
        let current = AtomicU64::new(2);
        assert!(
            search_boosted_generation(
                &store,
                "report",
                FilenameMode::Fuzzy,
                10,
                &HashMap::new(),
                &Filters::default(),
                &Quiet::new(Vec::new()),
                1,
                &current,
            )
            .unwrap()
            .is_none()
        );
        current.store(1, Ordering::Release);
        assert!(
            search(&store, "report", FilenameMode::Fuzzy, 10)
                .unwrap()
                .indices
                .len()
                > 0
        );
    }
}
