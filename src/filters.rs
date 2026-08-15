/// Query-side result filters, parsed from `ext:` / `path:` / `dir:` tokens.
/// Directories are marked in the index by a trailing `/`; without `dir:`
/// they are excluded so plain searches keep returning files.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filters {
    pub exts: Vec<String>,
    pub path_terms: Vec<String>,
    pub dirs_only: bool,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        self.exts.is_empty() && self.path_terms.is_empty() && !self.dirs_only
    }

    pub fn matches(&self, path: &str) -> bool {
        let is_dir = path.ends_with('/');
        if is_dir != self.dirs_only {
            return false;
        }
        if !self.exts.is_empty() {
            let ext = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str());
            let ok = ext.is_some_and(|e| self.exts.iter().any(|want| e.eq_ignore_ascii_case(want)));
            if !ok {
                return false;
            }
        }
        self.path_terms
            .iter()
            .all(|t| contains_ignore_ascii_case(path, t))
    }
}

/// Substring search that folds ASCII case without allocating — this runs
/// per-path in the hot matching loop.
pub fn contains_ignore_ascii_case(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let (hay, needle) = (hay.as_bytes(), needle.as_bytes());
    hay.windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

/// Splits filter tokens out of a query; returns the filters and the
/// remaining pattern text.
pub fn parse(query: &str) -> (Filters, String) {
    let mut filters = Filters::default();
    let mut rest: Vec<&str> = Vec::new();
    for token in query.split_whitespace() {
        if let Some(ext) = token.strip_prefix("ext:") {
            let ext = ext.trim_start_matches('.');
            if !ext.is_empty() {
                filters.exts.push(ext.to_ascii_lowercase());
            }
        } else if let Some(term) = token.strip_prefix("path:") {
            if !term.is_empty() {
                filters.path_terms.push(term.to_ascii_lowercase());
            }
        } else if token == "dir:" {
            filters.dirs_only = true;
        } else {
            rest.push(token);
        }
    }
    (filters, rest.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_queries_have_no_filters() {
        let (f, rest) = parse("meeting notes");
        assert!(f.is_empty());
        assert_eq!(rest, "meeting notes");
    }

    #[test]
    fn ext_tokens_are_extracted() {
        let (f, rest) = parse("ext:pdf report 2026");
        assert_eq!(f.exts, vec!["pdf"]);
        assert_eq!(rest, "report 2026");
        // multiple ext: tokens OR together, case folds, leading dot tolerated
        let (f, rest) = parse("ext:PDF ext:.md notes");
        assert_eq!(f.exts, vec!["pdf", "md"]);
        assert_eq!(rest, "notes");
    }

    #[test]
    fn path_tokens_are_extracted() {
        let (f, rest) = parse("path:Documents tax");
        assert_eq!(f.path_terms, vec!["documents"]);
        assert_eq!(rest, "tax");
    }

    #[test]
    fn dir_token_switches_to_directories() {
        let (f, rest) = parse("dir: proj");
        assert!(f.dirs_only);
        assert_eq!(rest, "proj");
    }

    #[test]
    fn matches_applies_ext_and_path_and_kind() {
        let (f, _) = parse("ext:pdf path:docs x");
        assert!(f.matches("/home/Docs/a.pdf"));
        assert!(!f.matches("/home/Docs/a.txt")); // wrong ext
        assert!(!f.matches("/home/other/a.pdf")); // missing path term
        assert!(!f.matches("/home/Docs/sub.pdf/")); // dirs excluded by default

        let (f, _) = parse("dir: x");
        assert!(f.matches("/home/projects/"));
        assert!(!f.matches("/home/projects/a.txt"));

        // no filters: files pass, dirs stay out of results
        let (f, _) = parse("x");
        assert!(f.matches("/a/b.txt"));
        assert!(!f.matches("/a/b/"));
    }

    #[test]
    fn path_terms_match_case_insensitively_without_alloc() {
        assert!(contains_ignore_ascii_case("/Users/Josh/DOCS/x", "docs"));
        assert!(!contains_ignore_ascii_case("/Users/Josh/x", "docs"));
        assert!(contains_ignore_ascii_case("abc", ""));
    }
}
