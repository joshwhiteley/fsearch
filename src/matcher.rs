use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rayon::prelude::*;

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
    if query.is_empty() {
        return Ok((0..paths.len().min(limit)).collect());
    }
    match mode {
        FilenameMode::Fuzzy => Ok(fuzzy(paths, query, limit)),
        FilenameMode::Regex => regex_filter(paths, query, limit),
    }
}

const CHUNK: usize = 16_384;

fn fuzzy(paths: &[String], query: &str, limit: usize) -> Vec<usize> {
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
                        .map(|score| (score, base + i))
                })
                .collect::<Vec<_>>()
        })
        .flatten()
        .collect();
    scored.par_sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(limit);
    scored.into_iter().map(|(_, i)| i).collect()
}

fn regex_filter(paths: &[String], query: &str, limit: usize) -> Result<Vec<usize>, String> {
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
        assert_eq!(search(&p, "readme", FilenameMode::Fuzzy, 10).unwrap().len(), 2);
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
        assert_eq!(search(&p, "readme", FilenameMode::Regex, 10).unwrap().len(), 2);
        assert_eq!(search(&p, "README", FilenameMode::Regex, 10).unwrap(), vec![0]);
    }

    #[test]
    fn invalid_regex_is_err() {
        let p = paths(&["/a"]);
        assert!(search(&p, "[unclosed", FilenameMode::Regex, 10).is_err());
    }

    #[test]
    fn limit_is_respected() {
        let p: Vec<String> = (0..100).map(|i| format!("/f/file{i}.txt")).collect();
        assert_eq!(search(&p, "file", FilenameMode::Fuzzy, 5).unwrap().len(), 5);
    }
}
