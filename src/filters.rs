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
    ("app", &["app"]),
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

/// Whether `kind` is one of the buckets accepted by the `kind:` query
/// filter.
pub fn is_known_kind(kind: &str) -> bool {
    KINDS
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(kind))
}

/// "7d", "12h", "30m", "2w" → seconds.
fn parse_duration_secs(s: &str) -> Option<i64> {
    let (num, unit) = s.split_at_checked(s.len().checked_sub(1)?)?;
    let n: i64 = num.parse().ok()?;
    if n < 0 {
        return None;
    }
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
    let bytes = v * mult;
    // u64::MAX rounds up to 2^64 as f64, so the upper bound is exclusive.
    (bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64).then_some(bytes as u64)
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
/// remaining pattern text. Whitespace is preserved except around removed
/// filter tokens; separated pattern fragments are joined with one space.
/// `now` (unix seconds) anchors relative dates.
pub fn parse(query: &str, now: i64) -> (Filters, String) {
    let mut filters = Filters::default();
    let mut rest = String::with_capacity(query.len());
    let mut cursor = 0;
    let mut removed_previous = false;
    for token in query.split_whitespace() {
        let gap_start = cursor;
        let start = cursor + query[cursor..].find(token).unwrap();
        cursor = start + token.len();
        let mut removed = true;
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
                removed = false; // unknown kind: leave it visible in the query
            }
        } else if let Some(dur) = token.strip_prefix("changed:") {
            match parse_duration_secs(dur).and_then(|secs| now.checked_sub(secs)) {
                Some(after) => filters.changed_after = Some(after),
                None => removed = false,
            }
        } else if let Some(sz) = token.strip_prefix("larger:") {
            match parse_size_bytes(sz) {
                Some(b) => filters.min_size = Some(b),
                None => removed = false,
            }
        } else if let Some(sz) = token.strip_prefix("smaller:") {
            match parse_size_bytes(sz) {
                Some(b) => filters.max_size = Some(b),
                None => removed = false,
            }
        } else if token == "dir:" {
            filters.dirs_only = true;
        } else {
            removed = false;
        }
        if !removed {
            if !removed_previous {
                rest.push_str(&query[gap_start..start]);
            } else if !rest.is_empty() {
                rest.push(' ');
            }
            rest.push_str(token);
        }
        removed_previous = removed;
    }
    if !removed_previous {
        rest.push_str(&query[cursor..]);
    }
    (filters, rest)
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
    fn untouched_pattern_whitespace_is_preserved() {
        for query in [
            "",
            " \t\n",
            "  meeting  notes\t2026 ",
            "é\u{2003}日\n🦀",
            "  changed:é\t kind:unknown  larger:NaN ",
        ] {
            let (filters, rest) = parse(query, 0);
            assert!(filters.is_empty());
            assert_eq!(rest, query);
        }
        for (query, expected) in [
            ("ext:pdf  meeting  notes\t2026 ", "meeting  notes\t2026 "),
            ("  meeting  notes\text:pdf", "  meeting  notes"),
            ("a\tb  ext:pdf path:docs\nc  d", "a\tb c  d"),
            ("ext:pdf\u{2003}é\u{2003}日\t", "é\u{2003}日\t"),
            (" ext:pdf\tpath:docs ", ""),
        ] {
            let (filters, rest) = parse(query, 0);
            assert!(!filters.is_empty());
            assert_eq!(rest, expected, "{query:?}");
        }
    }

    #[test]
    fn invalid_durations_remain_in_pattern() {
        for duration in [
            "",
            "é",
            "1é",
            "🦀",
            "-1d",
            "soon",
            "1",
            "1.5h",
            "9223372036854775808m",
            "9223372036854775807w",
        ] {
            assert_eq!(parse_duration_secs(duration), None, "{duration}");
            let query = format!("changed:{duration}");
            let (filters, rest) = parse(&query, 0);
            assert!(filters.is_empty(), "{query}");
            assert_eq!(rest, query);
        }
        let (filters, rest) = parse("changed:1m", i64::MIN);
        assert!(filters.is_empty());
        assert_eq!(rest, "changed:1m");
        assert_eq!(parse_duration_secs("0m"), Some(0));
        assert_eq!(parse_duration_secs("12h"), Some(43_200));
        assert_eq!(
            parse("changed:0m", i64::MIN).0.changed_after,
            Some(i64::MIN)
        );
        let max_minutes = i64::MAX / 60;
        assert_eq!(
            parse_duration_secs(&format!("{max_minutes}m")),
            Some(max_minutes * 60)
        );
        assert_eq!(parse_duration_secs(&format!("{}m", max_minutes + 1)), None);
    }

    #[test]
    fn invalid_sizes_remain_in_pattern() {
        for size in [
            "",
            "-1b",
            "NaN",
            "NaNgb",
            "inf",
            "infinitymb",
            "-inf",
            "1e309",
            "1e308gb",
            "18446744073709551616b",
            "18446744074gb",
        ] {
            assert_eq!(parse_size_bytes(size), None, "{size}");
            for prefix in ["larger:", "smaller:"] {
                let query = format!("{prefix}{size}");
                let (filters, rest) = parse(&query, 0);
                assert!(filters.is_empty(), "{query}");
                assert_eq!(rest, query);
            }
        }
        for (size, bytes) in [
            ("0b", 0),
            ("512", 512),
            ("1.5GB", 1_500_000_000),
            ("0.5kb", 500),
            ("1.9b", 1),
            ("18446744073709549568b", 18_446_744_073_709_549_568),
        ] {
            assert_eq!(parse_size_bytes(size), Some(bytes), "{size}");
        }
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
        assert_eq!(kind_for_ext("app"), Some("app"));
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
