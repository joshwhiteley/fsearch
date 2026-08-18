use crate::walker::FileMeta;

/// Query-side result filters, parsed from `ext:` / `path:` / `dir:` /
/// `kind:` / `changed:` / `larger:` / `smaller:` tokens. Directories are
/// marked in the index by a trailing `/`; without `dir:` they are excluded
/// so plain searches keep returning files.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filters {
    pub exts: Vec<String>,
    pub path_terms: Vec<String>,
    pub dirs_only: bool,
    pub changed_after: Option<i64>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
}

/// `kind:` shorthand → extension sets.
const KINDS: &[(&str, &[&str])] = &[
    (
        "image",
        &[
            "png", "jpg", "jpeg", "gif", "webp", "tif", "tiff", "bmp", "heic", "svg", "ico",
        ],
    ),
    ("video", &["mp4", "mov", "mkv", "avi", "webm", "m4v"]),
    ("audio", &["mp3", "wav", "flac", "m4a", "aac", "ogg"]),
    (
        "doc",
        &[
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pages", "key", "numbers", "md",
            "txt", "rtf",
        ],
    ),
    (
        "code",
        &[
            "rs", "py", "js", "ts", "tsx", "jsx", "go", "c", "h", "cpp", "hpp", "java", "rb", "sh",
            "swift", "kt", "lua", "sql", "html", "css", "toml", "yaml", "yml", "json",
        ],
    ),
    (
        "archive",
        &["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "dmg"],
    ),
];

/// The `kind:` bucket this extension belongs to ("image", "video", ...).
pub fn kind_for_ext(ext: &str) -> Option<&'static str> {
    KINDS
        .iter()
        .find(|(_, exts)| exts.iter().any(|e| e.eq_ignore_ascii_case(ext)))
        .map(|(name, _)| *name)
}

/// "7d", "12h", "30m", "2w" → seconds.
fn parse_duration_secs(s: &str) -> Option<i64> {
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "m" => 60,
        "h" => 3600,
        "d" => 24 * 3600,
        "w" => 7 * 24 * 3600,
        _ => return None,
    };
    n.checked_mul(mult)
}

/// "100mb", "1.5gb", "200kb", "512b" → bytes.
fn parse_size_bytes(s: &str) -> Option<u64> {
    let lower = s.to_ascii_lowercase();
    let (num, mult) = if let Some(n) = lower.strip_suffix("gb") {
        (n, 1_000_000_000f64)
    } else if let Some(n) = lower.strip_suffix("mb") {
        (n, 1_000_000f64)
    } else if let Some(n) = lower.strip_suffix("kb") {
        (n, 1_000f64)
    } else if let Some(n) = lower.strip_suffix("b") {
        (n, 1f64)
    } else {
        (lower.as_str(), 1f64)
    };
    let v: f64 = num.parse().ok()?;
    (v >= 0.0).then_some((v * mult) as u64)
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        self.exts.is_empty()
            && self.path_terms.is_empty()
            && !self.dirs_only
            && self.changed_after.is_none()
            && self.min_size.is_none()
            && self.max_size.is_none()
    }

    /// Metadata-side checks; pair with [`Filters::matches`] for the path side.
    pub fn matches_meta(&self, meta: &FileMeta) -> bool {
        if let Some(after) = self.changed_after
            && meta.mtime < after
        {
            return false;
        }
        if let Some(min) = self.min_size
            && meta.size < min
        {
            return false;
        }
        if let Some(max) = self.max_size
            && meta.size > max
        {
            return false;
        }
        true
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
/// remaining pattern text. `now` (unix seconds) anchors relative dates.
pub fn parse(query: &str, now: i64) -> (Filters, String) {
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
        } else if let Some(kind) = token.strip_prefix("kind:") {
            if let Some((_, exts)) = KINDS
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(kind))
            {
                filters.exts.extend(exts.iter().map(|e| e.to_string()));
            } else {
                rest.push(token); // unknown kind: leave it visible in the query
            }
        } else if let Some(dur) = token.strip_prefix("changed:") {
            match parse_duration_secs(dur) {
                Some(secs) => filters.changed_after = Some(now - secs),
                None => rest.push(token),
            }
        } else if let Some(sz) = token.strip_prefix("larger:") {
            match parse_size_bytes(sz) {
                Some(b) => filters.min_size = Some(b),
                None => rest.push(token),
            }
        } else if let Some(sz) = token.strip_prefix("smaller:") {
            match parse_size_bytes(sz) {
                Some(b) => filters.max_size = Some(b),
                None => rest.push(token),
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
        let (f, rest) = parse("meeting notes", 0);
        assert!(f.is_empty());
        assert_eq!(rest, "meeting notes");
    }

    #[test]
    fn ext_tokens_are_extracted() {
        let (f, rest) = parse("ext:pdf report 2026", 0);
        assert_eq!(f.exts, vec!["pdf"]);
        assert_eq!(rest, "report 2026");
        // multiple ext: tokens OR together, case folds, leading dot tolerated
        let (f, rest) = parse("ext:PDF ext:.md notes", 0);
        assert_eq!(f.exts, vec!["pdf", "md"]);
        assert_eq!(rest, "notes");
    }

    #[test]
    fn path_tokens_are_extracted() {
        let (f, rest) = parse("path:Documents tax", 0);
        assert_eq!(f.path_terms, vec!["documents"]);
        assert_eq!(rest, "tax");
    }

    #[test]
    fn dir_token_switches_to_directories() {
        let (f, rest) = parse("dir: proj", 0);
        assert!(f.dirs_only);
        assert_eq!(rest, "proj");
    }

    #[test]
    fn matches_applies_ext_and_path_and_kind() {
        let (f, _) = parse("ext:pdf path:docs x", 0);
        assert!(f.matches("/home/Docs/a.pdf"));
        assert!(!f.matches("/home/Docs/a.txt")); // wrong ext
        assert!(!f.matches("/home/other/a.pdf")); // missing path term
        assert!(!f.matches("/home/Docs/sub.pdf/")); // dirs excluded by default

        let (f, _) = parse("dir: x", 0);
        assert!(f.matches("/home/projects/"));
        assert!(!f.matches("/home/projects/a.txt"));

        // no filters: files pass, dirs stay out of results
        let (f, _) = parse("x", 0);
        assert!(f.matches("/a/b.txt"));
        assert!(!f.matches("/a/b/"));
    }

    #[test]
    fn kind_changed_and_size_tokens_parse() {
        let now = 1_000_000;
        let (f, rest) = parse("kind:image changed:7d larger:1mb smaller:2gb vacation", now);
        assert!(f.exts.contains(&"png".to_string()));
        assert!(f.exts.contains(&"heic".to_string()));
        assert_eq!(f.changed_after, Some(now - 7 * 24 * 3600));
        assert_eq!(f.min_size, Some(1_000_000));
        assert_eq!(f.max_size, Some(2_000_000_000));
        assert_eq!(rest, "vacation");
        // malformed tokens stay in the query text
        let (f, rest) = parse("changed:soon larger:big kind:widget x", now);
        assert!(f.is_empty());
        assert_eq!(rest, "changed:soon larger:big kind:widget x");
    }

    #[test]
    fn meta_filters_apply() {
        use crate::walker::FileMeta;
        let now = 1_000_000;
        let (f, _) = parse("changed:1d larger:100b", now);
        let fresh_big = FileMeta {
            mtime: now - 3600,
            size: 500,
        };
        let fresh_small = FileMeta {
            mtime: now - 3600,
            size: 50,
        };
        let old_big = FileMeta {
            mtime: now - 200_000,
            size: 500,
        };
        assert!(f.matches_meta(&fresh_big));
        assert!(!f.matches_meta(&fresh_small));
        assert!(!f.matches_meta(&old_big));
    }

    #[test]
    fn kind_for_ext_maps_extension_to_kind() {
        assert_eq!(kind_for_ext("png"), Some("image"));
        assert_eq!(kind_for_ext("PNG"), Some("image"));
        assert_eq!(kind_for_ext("rs"), Some("code"));
        assert_eq!(kind_for_ext("pdf"), Some("doc"));
        assert_eq!(kind_for_ext("zzz"), None);
        assert_eq!(kind_for_ext(""), None);
    }

    #[test]
    fn path_terms_match_case_insensitively_without_alloc() {
        assert!(contains_ignore_ascii_case("/Users/Josh/DOCS/x", "docs"));
        assert!(!contains_ignore_ascii_case("/Users/Josh/x", "docs"));
        assert!(contains_ignore_ascii_case("abc", ""));
    }
}
