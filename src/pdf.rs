use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// PDF extraction reads and parses the whole file; skip anything larger.
pub const MAX_PDF_BYTES: u64 = 20 * 1024 * 1024;

/// Upper bound on cached extracted texts; oldest entries are evicted.
pub const MAX_PDF_CACHE_FILES: usize = 4096;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    crate::util::create_private_dir(cache_dir).map_err(|e| format!("private PDF cache: {e}"))?;
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular PDF file".into());
    }
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
        .map_or(0, |d| d.as_nanos());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    let key = format!("{:016x}-{mtime}-{}.txt", hasher.finish(), meta.len());
    let cached = cache_dir.join(&key);
    if let Ok(text) = std::fs::read_to_string(&cached) {
        return Ok(text);
    }
    // failures are cached too, so a PDF that crashes or defeats the parser
    // is attempted once — not re-parsed (and re-panicked) on every search
    let cached_err = cache_dir.join(format!("{key}.err"));
    if let Ok(msg) = std::fs::read_to_string(&cached_err) {
        return Err(msg);
    }
    let result = extract(path);
    let (target, body) = match &result {
        Ok(text) => (&cached, text.as_str()),
        Err(e) => (&cached_err, e.as_str()),
    };
    // write to a pid + counter unique temp then rename, so concurrent
    // threads and instances never observe a half-written cache entry; a
    // failed write is removed rather than renamed into place, so a disk-full
    // event can't publish truncated text as a cache hit
    let nonce = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = cache_dir.join(format!(".{key}.{}-{nonce}.tmp", std::process::id()));
    if let Ok(mut file) = crate::util::create_private_file(&tmp) {
        use std::io::Write;
        let written = file
            .write_all(body.as_bytes())
            .and_then(|_| std::fs::rename(&tmp, target));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    evict_oldest(cache_dir);
    result
}

thread_local! {
    static IN_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// True while [`extract`] is running its panic-guarded parser on this
/// thread. Panic hooks check it so caught pdf-extract panics stay silent
/// instead of spraying over the UI (or tearing the terminal down).
pub fn in_extract_guard() -> bool {
    IN_GUARD.with(|g| g.get())
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
    use std::io::Read;
    let file = crate::util::open_regular_file(Path::new(path)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_PDF_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PDF_BYTES {
        return Err("PDF grew beyond the input-size limit".into());
    }
    IN_GUARD.with(|g| g.set(true));
    let result = std::panic::catch_unwind(move || {
        pdf_extract::extract_text_from_mem(&bytes).map_err(|e| e.to_string())
    });
    IN_GUARD.with(|g| g.set(false));
    result.unwrap_or_else(|_| Err("pdf parser crashed on this file".to_string()))
}

/// A minimal single-page PDF containing `text`, for tests.
#[doc(hidden)]
pub fn minimal_pdf(text: &str) -> Vec<u8> {
    build_pdf(
        text,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    )
}

fn build_pdf(text: &str, font: &str) -> Vec<u8> {
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
        font.to_string(),
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
    fn symbol_encoding_pdf_is_an_error_not_a_panic() {
        // pdf-extract 0.12 panics on fonts declaring /SymbolEncoding;
        // the guard must turn that into an Err
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("weird.pdf");
        std::fs::write(
            &file,
            build_pdf(
                "hello",
                "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol \
                 /Encoding /SymbolEncoding >>",
            ),
        )
        .unwrap();
        let cache = dir.path().join("cache");
        assert!(extract_cached(file.to_str().unwrap(), &cache).is_err());
        assert!(!in_extract_guard(), "guard flag must reset after the call");
    }

    #[test]
    fn extraction_failures_are_cached() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.pdf");
        std::fs::write(&file, b"%PDF-1.4 not really").unwrap();
        let cache = dir.path().join("cache");
        let first = extract_cached(file.to_str().unwrap(), &cache).unwrap_err();
        // exactly one cache entry, and it is the failure marker
        let names: Vec<String> = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names[0].ends_with(".err"), "got {names:?}");
        // the second call is served from the marker, not re-parsed: plant a
        // sentinel in the cache file and see it come back
        assert_eq!(
            extract_cached(file.to_str().unwrap(), &cache).unwrap_err(),
            first
        );
        std::fs::write(cache.join(&names[0]), "sentinel").unwrap();
        assert_eq!(
            extract_cached(file.to_str().unwrap(), &cache).unwrap_err(),
            "sentinel"
        );
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

    #[test]
    fn same_second_equal_size_edits_invalidate_cached_text() {
        use std::time::{Duration, UNIX_EPOCH};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.pdf");
        let cache = dir.path().join("cache");
        let save = |text, nanos| {
            std::fs::write(&file, minimal_pdf(text)).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&file)
                .unwrap()
                .set_modified(
                    UNIX_EPOCH + Duration::from_secs(1_700_000_000) + Duration::from_nanos(nanos),
                )
                .unwrap();
        };
        save("First", 100_000_000);
        assert!(
            extract_cached(file.to_str().unwrap(), &cache)
                .unwrap()
                .contains("First")
        );
        save("Other", 900_000_000);
        assert!(
            extract_cached(file.to_str().unwrap(), &cache)
                .unwrap()
                .contains("Other")
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_cache_write_publishes_nothing_and_leaves_no_tmp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.pdf");
        std::fs::write(&file, minimal_pdf("Needle in a haystack")).unwrap();
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o555)).unwrap();
        // extraction succeeds but the cache write cannot: the result must
        // still be returned, with no truncated entry or tmp file left behind
        let text = extract_cached(file.to_str().unwrap(), &cache);
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(text.is_ok());
        let names: Vec<String> = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.is_empty(), "cache should stay empty, got {names:?}");
    }
}
