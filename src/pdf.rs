use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// PDF extraction reads and parses the whole file; skip anything larger.
pub const MAX_PDF_BYTES: u64 = 20 * 1024 * 1024;

/// Upper bound on cached extracted texts; oldest entries are evicted.
pub const MAX_PDF_CACHE_FILES: usize = 4096;

pub fn is_pdf_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Where extracted text is cached, next to the index cache.
pub fn default_cache_dir() -> PathBuf {
    crate::index::default_cache_path()
        .parent()
        .map(|p| p.join("pdftext"))
        .unwrap_or_else(|| PathBuf::from("/tmp/fsearch-pdftext"))
}

/// Extracted text for a PDF, cached on disk keyed by path + mtime + size so
/// repeat searches don't re-parse. Errors are strings suitable for the UI.
pub fn extract_cached(path: &str, cache_dir: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_PDF_BYTES {
        return Err(format!(
            "pdf larger than {} MiB",
            MAX_PDF_BYTES / (1024 * 1024)
        ));
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    let key = format!("{:016x}-{mtime}-{}.txt", hasher.finish(), meta.len());
    let cached = cache_dir.join(&key);
    if let Ok(text) = std::fs::read_to_string(&cached) {
        return Ok(text);
    }
    let text = extract(path)?;
    let _ = std::fs::create_dir_all(cache_dir);
    // write to a pid-unique temp then rename, so two concurrent instances
    // never observe a half-written cache entry
    let tmp = cache_dir.join(format!(".{key}.{}.tmp", std::process::id()));
    let _ = std::fs::write(&tmp, &text);
    let _ = std::fs::rename(&tmp, &cached);
    evict_oldest(cache_dir);
    Ok(text)
}

/// Keeps the cache from growing without bound: on a miss, removes the oldest
/// entries (by modified time) until only MAX_PDF_CACHE_FILES remain.
fn evict_oldest(cache_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, e.path()))
        })
        .collect();
    if files.len() <= MAX_PDF_CACHE_FILES {
        return;
    }
    files.sort_by_key(|(m, _)| *m);
    let excess = files.len() - MAX_PDF_CACHE_FILES;
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// pdf-extract is known to panic on malformed files; contain that.
fn extract(path: &str) -> Result<String, String> {
    let path = path.to_string();
    std::panic::catch_unwind(move || pdf_extract::extract_text(&path).map_err(|e| e.to_string()))
        .unwrap_or_else(|_| Err("pdf parser crashed on this file".to_string()))
}

/// A minimal single-page PDF containing `text`, for tests.
#[doc(hidden)]
pub fn minimal_pdf(text: &str) -> Vec<u8> {
    let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref_at = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for off in &offsets {
        out.push_str(&format!("{off:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_pdf_paths() {
        assert!(is_pdf_path("/a/report.pdf"));
        assert!(is_pdf_path("/a/REPORT.PDF"));
        assert!(!is_pdf_path("/a/report.pdf.txt"));
    }

    #[test]
    fn extracts_text_and_caches_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.pdf");
        std::fs::write(&file, minimal_pdf("Needle in a haystack")).unwrap();
        let cache = dir.path().join("cache");
        let text = extract_cached(file.to_str().unwrap(), &cache).unwrap();
        assert!(text.contains("Needle in a haystack"), "got: {text:?}");
        // a cache entry was written and satisfies the second call
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
        let again = extract_cached(file.to_str().unwrap(), &cache).unwrap();
        assert_eq!(again, text);
    }

    #[test]
    fn corrupt_pdf_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.pdf");
        std::fs::write(&file, b"%PDF-1.4 not really").unwrap();
        assert!(extract_cached(file.to_str().unwrap(), dir.path()).is_err());
    }

    #[test]
    fn oversized_pdfs_are_refused_without_reading() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.pdf");
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(MAX_PDF_BYTES + 1).unwrap();
        assert!(extract_cached(file.to_str().unwrap(), dir.path()).is_err());
    }
}
