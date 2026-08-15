use crate::filters::Filters;
use crate::index::PathStore;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rayon::prelude::*;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilenameMode {
    Fuzzy,
    Regex,
}

pub fn search(
    store: &PathStore,
    query: &str,
    mode: FilenameMode,
    limit: usize,
) -> Result<Vec<usize>, String> {
    search_boosted(
        store,
        query,
        mode,
        limit,
        &HashMap::new(),
        &Filters::default(),
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
) -> Result<Vec<usize>, String> {
    if query.is_empty() {
        return Ok(head_with_boosts(store, limit, boosts, filters));
    }
    match mode {
        FilenameMode::Fuzzy => Ok(fuzzy(store, query, limit, boosts, filters)),
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
) -> Vec<usize> {
    if boosts.is_empty() {
        return (0..store.len())
            .filter(|&i| passes(store, i, filters))
            .take(limit)
            .collect();
    }
    let mut boosted: Vec<(u32, usize)> = (0..store.len())
        .filter(|&i| passes(store, i, filters))
        .filter_map(|i| boosts.get(store.get(i)).map(|&b| (b, i)))
        .collect();
    boosted.sort_by_key(|&(b, i)| (std::cmp::Reverse(b), i));
    let mut out: Vec<usize> = boosted.iter().map(|&(_, i)| i).collect();
    let in_boosted: std::collections::HashSet<usize> = out.iter().copied().collect();
    out.extend(
        (0..store.len())
            .filter(|&i| !in_boosted.contains(&i) && passes(store, i, filters))
            .take(limit),
    );
    out.truncate(limit);
    out
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

fn fuzzy(
    store: &PathStore,
    query: &str,
    limit: usize,
    boosts: &HashMap<String, u32>,
    filters: &Filters,
) -> Vec<usize> {
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
                        let boost = boosts.get(path).copied().unwrap_or(0);
                        acc.push((score + boost, i));
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
    scored.truncate(limit);
    scored.into_iter().map(|(_, i)| i).collect()
}

fn regex_filter(
    store: &PathStore,
    query: &str,
    limit: usize,
    boosts: &HashMap<String, u32>,
    filters: &Filters,
) -> Result<Vec<usize>, String> {
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
    Ok(hits)
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
        assert_eq!(search(&p, "", FilenameMode::Fuzzy, 2).unwrap(), vec![0, 1]);
    }

    #[test]
    fn fuzzy_ranks_filename_match_over_scattered() {
        let p = paths(&[
            "/code/rust/tools/everything/notes.txt", // scattered match for "rest"
            "/docs/rest-api.md",                     // filename match
        ]);
        let r = search(&p, "rest", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r[0], 1);
    }

    #[test]
    fn fuzzy_is_smart_case() {
        let p = paths(&["/docs/README.md", "/docs/readme-draft.md"]);
        // lowercase query matches both
        assert_eq!(
            search(&p, "readme", FilenameMode::Fuzzy, 10).unwrap().len(),
            2
        );
        // uppercase query matches only the uppercase path
        let r = search(&p, "README", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r, vec![0]);
    }

    #[test]
    fn regex_filters_by_full_path() {
        let p = paths(&["/a/report_2024.pdf", "/a/report.txt", "/b/2024.pdf"]);
        let r = search(&p, r"report_\d+\.pdf$", FilenameMode::Regex, 10).unwrap();
        assert_eq!(r, vec![0]);
    }

    #[test]
    fn regex_is_smart_case() {
        let p = paths(&["/a/README.md", "/a/readme.md"]);
        assert_eq!(
            search(&p, "readme", FilenameMode::Regex, 10).unwrap().len(),
            2
        );
        assert_eq!(
            search(&p, "README", FilenameMode::Regex, 10).unwrap(),
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
        )
        .unwrap();
        assert_eq!(r[0], 1);
        // empty query: boosted file floats to the top
        let r = search_boosted(
            &p,
            "",
            FilenameMode::Fuzzy,
            10,
            &boosts,
            &Filters::default(),
        )
        .unwrap();
        assert_eq!(r, vec![1, 0]);
        // regex: boosted file first, others keep index order
        let r = search_boosted(
            &p,
            "readme",
            FilenameMode::Regex,
            10,
            &boosts,
            &Filters::default(),
        )
        .unwrap();
        assert_eq!(r, vec![1, 0]);
    }

    #[test]
    fn filters_narrow_all_modes() {
        let p = paths(&["/docs/a.pdf", "/docs/a.txt", "/docs/sub/", "/extra/b.pdf"]);
        let (f, _) = crate::filters::parse("ext:pdf path:docs x", 0);
        let none = HashMap::new();
        // empty query honors filters
        let r = search_boosted(&p, "", FilenameMode::Fuzzy, 10, &none, &f).unwrap();
        assert_eq!(r, vec![0]);
        // fuzzy honors filters
        let r = search_boosted(&p, "a", FilenameMode::Fuzzy, 10, &none, &f).unwrap();
        assert_eq!(r, vec![0]);
        // dirs only with dir:
        let (fd, _) = crate::filters::parse("dir: x", 0);
        let r = search_boosted(&p, "", FilenameMode::Fuzzy, 10, &none, &fd).unwrap();
        assert_eq!(r, vec![2]);
        // default (no filters) excludes dirs
        let r = search(&p, "", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r, vec![0, 1, 3]);
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
    fn limit_is_respected() {
        let owned: Vec<String> = (0..100).map(|i| format!("/f/file{i}.txt")).collect();
        let strs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        let p = paths(&strs);
        assert_eq!(search(&p, "file", FilenameMode::Fuzzy, 5).unwrap().len(), 5);
    }
}
