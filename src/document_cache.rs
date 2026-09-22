//! Shared disk budgets for extracted document text (including failure markers).
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_ERROR_BYTES: u64 = 16 * 1024;
pub(crate) const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
pub(crate) const MAX_FILES: usize = 4096;
static NONCE: AtomicU64 = AtomicU64::new(0);
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn read(path: &Path, limit: u64) -> Option<String> {
    let file = crate::util::open_regular_file(path).ok()?;
    if file.metadata().ok()?.len() > limit {
        return None;
    }
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= limit).then_some(text)
}

pub(crate) fn store(dir: &Path, target: &Path, body: &str, office: bool) {
    let _lock = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    evict(dir, office, MAX_TOTAL_BYTES, MAX_FILES);
    let limit = if target.extension().is_some_and(|e| e == "err") {
        MAX_ERROR_BYTES
    } else {
        MAX_ENTRY_BYTES
    };
    if body.len() as u64 > limit {
        return;
    }
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".document-{}-{nonce}.tmp", std::process::id()));
    if let Ok(mut file) = crate::util::create_private_file(&tmp)
        && file
            .write_all(body.as_bytes())
            .and_then(|_| std::fs::rename(&tmp, target))
            .is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
    evict(dir, office, MAX_TOTAL_BYTES, MAX_FILES);
}

pub(crate) fn evict(dir: &Path, office: bool, bytes: u64, count: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let key = if office {
            let Some(key) = name.strip_prefix("office-") else {
                continue;
            };
            key
        } else {
            name
        };
        if !(key.ends_with(".txt") || key.ends_with(".txt.err"))
            || key
                .split('-')
                .next()
                .is_none_or(|hash| hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let limit = if name.ends_with(".err") {
            MAX_ERROR_BYTES
        } else {
            MAX_ENTRY_BYTES
        };
        if meta.len() > limit && std::fs::remove_file(&path).is_ok() {
            continue;
        }
        total = total.saturating_add(meta.len());
        files.push((
            meta.modified().unwrap_or(std::time::UNIX_EPOCH),
            path,
            meta.len(),
        ));
    }
    files.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut remaining = files.len();
    for (_, path, size) in files {
        if total <= bytes && remaining <= count {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
            remaining -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_caches_evict_by_bytes_and_count_including_errors() {
        for office in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let prefix = if office { "office-" } else { "" };
            for i in 0..6 {
                let path = dir.path().join(format!(
                    "{prefix}{i:016x}-0-0.txt{}",
                    if i % 2 == 0 { ".err" } else { "" }
                ));
                std::fs::write(&path, "1234567890").unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(i))
                    .unwrap();
            }
            evict(dir.path(), office, 30, 6);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
            assert!(
                !dir.path()
                    .join(format!("{prefix}{:016x}-0-0.txt.err", 2))
                    .exists()
            );
            evict(dir.path(), office, 30, 1);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
            assert!(
                dir.path()
                    .join(format!("{prefix}{:016x}-0-0.txt", 5))
                    .exists()
            );
        }
    }

    #[test]
    fn oversized_text_and_failure_entries_are_never_read_and_are_evicted() {
        for office in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let prefix = if office { "office-" } else { "" };
            for (suffix, limit) in [(".txt", MAX_ENTRY_BYTES), (".txt.err", MAX_ERROR_BYTES)] {
                let path = dir
                    .path()
                    .join(format!("{prefix}0123456789abcdef-0-0{suffix}"));
                std::fs::File::create(&path)
                    .unwrap()
                    .set_len(limit + 1)
                    .unwrap();
                assert!(read(&path, limit).is_none());
            }
            evict(dir.path(), office, MAX_TOTAL_BYTES, MAX_FILES);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn cache_reads_reject_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "secret").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(target, &link).unwrap();
        assert!(read(&link, MAX_ENTRY_BYTES).is_none());
    }
}
