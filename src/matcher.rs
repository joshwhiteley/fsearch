use crate::filters::Filters;
use crate::index::PathStore;
use crate::quiet::Quiet;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rayon::prelude::*;
use std::collections::HashMap;

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
    // a `/` in the query, a path: filter, or dir: means the user is
    // navigating paths on purpose — quiet demotion switches off
    let path_intent = query.contains('/') || !filters.path_terms.is_empty() || filters.dirs_only;
    let demote = (!quiet.is_empty() && !path_intent).then_some(quiet);
    if query.is_empty() {
        return Ok(head_with_boosts(store, limit, boosts, filters, demote));
    }
    match mode {
        FilenameMode::Fuzzy => Ok(fuzzy(store, query, limit, boosts, filters, demote)),
        FilenameMode::Regex => regex_filter(store, query, limit, boosts, filters),
    }
}

fn passes(store: &PathStore, i: usize, filters: &Filters) -> bool {
    filters.matches(store.get(i)) && filters.matches_meta(&store.meta(i))
}

/// First `limit` entries (already newest-first), with boosted paths —
/// wherever they sit in the full list — floated to the front.
fn head_with_boosts(
    store: &PathStore,
    limit: usize,
    boosts: &HashMap<String, u32>,
    filters: &Filters,
    demote: Option<&Quiet>,
) -> Ranked {
    // frecency-boosted entries first (opening something is an explicit
    // signal, quiet or not), then plain entries newest-first; quiet paths
    // sink into a trailing block behind the weaker-matches fold, which
    // keeps log/state churn off the launch screen
    let mut out: Vec<usize> = Vec::new();
    let mut in_boosted: std::collections::HashSet<usize> = std::collections::HashSet::new();
    if !boosts.is_empty() {
        let mut boosted: Vec<(u32, usize)> = (0..store.len())
            .filter(|&i| passes(store, i, filters))
            .filter_map(|i| boosts.get(store.get(i)).map(|&b| (b, i)))
            .collect();
        boosted.sort_by_key(|&(b, i)| (std::cmp::Reverse(b), i));
        out.extend(boosted.iter().map(|&(_, i)| i));
        in_boosted.extend(out.iter().copied());
    }
    let mut normal: Vec<usize> = Vec::new();
    let mut quiet_tail: Vec<usize> = Vec::new();
    for i in 0..store.len() {
        if normal.len() >= limit {
            break;
        }
        if in_boosted.contains(&i) || !passes(store, i, filters) {
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
    Ranked {
        indices: out,
        strong,
    }
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

fn fuzzy(
    store: &PathStore,
    query: &str,
    limit: usize,
    boosts: &HashMap<String, u32>,
    filters: &Filters,
    demote: Option<&Quiet>,
) -> Ranked {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut scored: Vec<(u32, usize)> = (0..store.len())
        .into_par_iter()
        .with_min_len(CHUNK)
        .fold(
            || {
                let mut cfg = Config::DEFAULT;
                cfg.set_match_paths();
                (Matcher::new(cfg), Vec::new(), Vec::new())
            },
            |(mut matcher, mut buf, mut acc), i| {
                if passes(store, i, filters) {
                    let path = store.get(i);
                    if let Some(score) = pattern.score(Utf32Str::new(path, &mut buf), &mut matcher)
                    {
                        // a query that also matches within the filename alone is far more
                        // likely what the user meant than letters scattered across the path;
                        // adding the basename score roughly doubles such results
                        let name = path
                            .trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .unwrap_or(path);
                        let fname_bonus = pattern
                            .score(Utf32Str::new(name, &mut buf), &mut matcher)
                            .unwrap_or(0);
                        let boost = boosts.get(path).copied().unwrap_or(0);
                        let mut total = score + fname_bonus + boost;
                        // quiet paths score at 2/5: even with a filename
                        // match they land under the best/2 floor whenever a
                        // non-quiet candidate exists, i.e. behind the fold
                        if demote.is_some_and(|q| q.is_quiet(path)) {
                            total = total * 2 / 5;
                        }
                        acc.push((total, i));
                    }
                }
                (matcher, buf, acc)
            },
        )
        .map(|(_, _, acc)| acc)
        .reduce(Vec::new, |mut a, mut b| {
            a.append(&mut b);
            a
        });
    scored.par_sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // Because filename matches score roughly double (change 1), a "best / 2"
    // floor self-regulates: when a real filename match exists, scattered
    // path-only matches fall below it and disappear; when nothing matches the
    // filename, all candidates score within range of each other and survive.
    let floor_strong = |scored: &[(u32, usize)]| {
        let mut s = 0;
        if let Some(&(best, _)) = scored.first() {
            let floor = best / 2;
            s = scored.partition_point(|&(v, _)| v >= floor);
            s = s.max(MIN_KEEP.min(scored.len()));
        }
        s
    };
    // Quiet paths fold behind the weak-match row whenever a louder,
    // non-quiet match exists — regardless of MIN_KEEP. This keeps log and
    // app-internal churn out of the default view while staying reachable
    // via ctrl-x. Path-intent queries (demote = None) skip this entirely.
    let mut strong;
    if let Some(q) = demote {
        // stable partition: non-quiet first, score order preserved within
        scored.sort_by_key(|&(_, i)| q.is_quiet(store.get(i)));
        let nq = scored.partition_point(|&(_, i)| !q.is_quiet(store.get(i)));
        if nq > 0 && nq < scored.len() {
            strong = floor_strong(&scored[..nq]);
        } else {
            strong = floor_strong(&scored);
        }
    } else {
        strong = floor_strong(&scored);
    }
    scored.truncate(limit);
    strong = strong.min(scored.len());
    let indices = scored.into_iter().map(|(_, i)| i).collect();
    Ranked { indices, strong }
}

fn regex_filter(
    store: &PathStore,
    query: &str,
    limit: usize,
    boosts: &HashMap<String, u32>,
    filters: &Filters,
) -> Result<Ranked, String> {
    let smart_case_insensitive = !query.chars().any(|c| c.is_uppercase());
    let re = regex::RegexBuilder::new(query)
        .case_insensitive(smart_case_insensitive)
        .build()
        .map_err(|e| e.to_string())?;
    let mut hits: Vec<usize> = (0..store.len())
        .into_par_iter()
        .with_min_len(CHUNK)
        .filter(|&i| passes(store, i, filters) && re.is_match(store.get(i)))
        .collect();
    hits.sort_unstable();
    apply_boost_order(&mut hits, store, boosts);
    hits.truncate(limit);
    let strong = hits.len();
    Ok(Ranked {
        indices: hits,
        strong,
    })
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
}
