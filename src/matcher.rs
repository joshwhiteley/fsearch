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
    paths: &[String],
    query: &str,
    mode: FilenameMode,
    limit: usize,
) -> Result<Vec<usize>, String> {
    search_boosted(paths, query, mode, limit, &HashMap::new())
}

/// Like [`search`], with per-path ranking boosts (from open history).
pub fn search_boosted(
    paths: &[String],
    query: &str,
    mode: FilenameMode,
    limit: usize,
    boosts: &HashMap<String, u32>,
) -> Result<Vec<usize>, String> {
    if query.is_empty() {
        return Ok(head_with_boosts(paths, limit, boosts));
    }
    match mode {
        FilenameMode::Fuzzy => Ok(fuzzy(paths, query, limit, boosts)),
        FilenameMode::Regex => regex_filter(paths, query, limit, boosts),
    }
}

/// First `limit` paths (already newest-first), with boosted paths — wherever
/// they sit in the full list — floated to the front.
fn head_with_boosts(paths: &[String], limit: usize, boosts: &HashMap<String, u32>) -> Vec<usize> {
    if boosts.is_empty() {
        return (0..paths.len().min(limit)).collect();
    }
    let mut boosted: Vec<(u32, usize)> = paths
        .iter()
        .enumerate()
        .filter_map(|(i, p)| boosts.get(p).map(|&b| (b, i)))
        .collect();
    boosted.sort_by_key(|&(b, i)| (std::cmp::Reverse(b), i));
    let mut out: Vec<usize> = boosted.iter().map(|&(_, i)| i).collect();
    let in_boosted: std::collections::HashSet<usize> = out.iter().copied().collect();
    out.extend(
        (0..paths.len())
            .filter(|i| !in_boosted.contains(i))
            .take(limit),
    );
    out.truncate(limit);
    out
}

fn apply_boost_order(hits: &mut [usize], paths: &[String], boosts: &HashMap<String, u32>) {
    if !boosts.is_empty() {
        // stable: unboosted hits keep their recency order
        hits.sort_by_key(|&i| std::cmp::Reverse(boosts.get(&paths[i]).copied().unwrap_or(0)));
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

fn fuzzy(paths: &[String], query: &str, limit: usize, boosts: &HashMap<String, u32>) -> Vec<usize> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut scored: Vec<(u32, usize)> = paths
        .par_chunks(CHUNK)
        .enumerate()
        .map(|(chunk_no, chunk)| {
            let mut cfg = Config::DEFAULT;
            cfg.set_match_paths();
            let mut matcher = Matcher::new(cfg);
            let mut buf = Vec::new();
            let base = chunk_no * CHUNK;
            chunk
                .iter()
                .enumerate()
                .filter_map(|(i, path)| {
                    pattern
                        .score(Utf32Str::new(path, &mut buf), &mut matcher)
                        .map(|score| {
                            let boost = boosts.get(path).copied().unwrap_or(0);
                            (score + boost, base + i)
                        })
                })
                .collect::<Vec<_>>()
        })
        .flatten()
        .collect();
    scored.par_sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(limit);
    scored.into_iter().map(|(_, i)| i).collect()
}

fn regex_filter(
    paths: &[String],
    query: &str,
    limit: usize,
    boosts: &HashMap<String, u32>,
) -> Result<Vec<usize>, String> {
    let smart_case_insensitive = !query.chars().any(|c| c.is_uppercase());
    let re = regex::RegexBuilder::new(query)
        .case_insensitive(smart_case_insensitive)
        .build()
        .map_err(|e| e.to_string())?;
    let mut hits: Vec<usize> = paths
        .par_iter()
        .enumerate()
        .filter(|(_, p)| re.is_match(p))
        .map(|(i, _)| i)
        .collect();
    hits.sort_unstable();
    apply_boost_order(&mut hits, paths, boosts);
    hits.truncate(limit);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
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
        let r = search_boosted(&p, "readme", FilenameMode::Fuzzy, 10, &boosts).unwrap();
        assert_eq!(r[0], 1);
        // empty query: boosted file floats to the top
        let r = search_boosted(&p, "", FilenameMode::Fuzzy, 10, &boosts).unwrap();
        assert_eq!(r, vec![1, 0]);
        // regex: boosted file first, others keep index order
        let r = search_boosted(&p, "readme", FilenameMode::Regex, 10, &boosts).unwrap();
        assert_eq!(r, vec![1, 0]);
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
        let p: Vec<String> = (0..100).map(|i| format!("/f/file{i}.txt")).collect();
        assert_eq!(search(&p, "file", FilenameMode::Fuzzy, 5).unwrap().len(), 5);
    }
}
