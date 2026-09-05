use crate::walker::FileMeta;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const MAGIC: &[u8; 8] = b"FSEARCH\0";
const VERSION: u32 = 3;

/// Every indexed path in one contiguous byte arena, plus per-entry spans
/// and metadata. Loading is a single file read and two table scans — no
/// per-path allocations, which is what makes million-path startup instant.
pub struct PathStore {
    arena: Box<[u8]>,
    spans: Vec<(u32, u32)>,
    metas: Vec<FileMeta>,
}

impl PathStore {
    pub fn empty() -> PathStore {
        PathStore {
            arena: Box::from([]),
            spans: Vec::new(),
            metas: Vec::new(),
        }
    }

    /// Panics if the combined path bytes exceed the u32 arena size limit.
    pub fn from_entries(entries: &[(String, FileMeta)]) -> PathStore {
        let total = checked_arena_size(entries.iter().map(|(p, _)| p.len()))
            .expect("path arena exceeds u32 size limit");
        let mut arena = Vec::with_capacity(total as usize);
        let mut spans = Vec::with_capacity(entries.len());
        let mut metas = Vec::with_capacity(entries.len());
        for (p, m) in entries {
            spans.push((arena.len() as u32, p.len() as u32));
            arena.extend_from_slice(p.as_bytes());
            metas.push(*m);
        }
        PathStore {
            arena: arena.into_boxed_slice(),
            spans,
            metas,
        }
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn get(&self, i: usize) -> &str {
        let (off, len) = self.spans[i];
        // Construction/loading validates UTF-8 and every span boundary.
        unsafe { std::str::from_utf8_unchecked(&self.arena[off as usize..(off + len) as usize]) }
    }

    pub fn meta(&self, i: usize) -> FileMeta {
        self.metas[i]
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        (0..self.len()).map(|i| self.get(i))
    }
}

// Check both individual path lengths and their cumulative size before any
// narrowing casts or allocations. Kept separate so limits can be tested without
// allocating a multi-gigabyte arena.
fn checked_arena_size(lengths: impl IntoIterator<Item = usize>) -> std::io::Result<u32> {
    lengths.into_iter().try_fold(0u32, |total, len| {
        u32::try_from(len)
            .ok()
            .and_then(|len| total.checked_add(len))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "path arena exceeds u32 size limit",
                )
            })
    })
}

pub fn default_cache_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".cache")
        });
    base.join("fsearch").join("index.bin")
}

/// Pid + counter temp name next to `path`, so concurrent fsearch processes
/// never write each other's temp file.
fn tmp_path(path: &Path) -> PathBuf {
    let nonce = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("tmp-{}-{nonce}", std::process::id()))
}

/// v3 layout: magic, version, count, then three contiguous tables —
/// lengths (u32), metadata (i64 mtime + u64 size), and the path arena.
pub fn save(entries: &[(String, FileMeta)], path: &Path) -> std::io::Result<()> {
    checked_arena_size(entries.iter().map(|(p, _)| p.len()))?;
    if let Some(parent) = path.parent() {
        crate::util::create_private_dir(parent)?;
    }
    let tmp = tmp_path(path);
    let file = crate::util::create_private_file(&tmp)?;
    let written = (|| {
        let mut w = BufWriter::new(file);
        w.write_all(MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&(entries.len() as u64).to_le_bytes())?;
        for (p, _) in entries {
            w.write_all(&(p.len() as u32).to_le_bytes())?;
        }
        for (_, meta) in entries {
            w.write_all(&meta.mtime.to_le_bytes())?;
            w.write_all(&meta.size.to_le_bytes())?;
        }
        for (p, _) in entries {
            w.write_all(p.as_bytes())?;
        }
        w.flush()?;
        // make the bytes durable before the rename publishes them
        w.get_ref().sync_all()
    })();
    let result = written.and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn load(path: &Path) -> Option<PathStore> {
    let data = std::fs::read(path).ok()?;
    let header = data.get(..20)?;
    if &header[..8] != MAGIC {
        return None;
    }
    if u32::from_le_bytes(header[8..12].try_into().ok()?) != VERSION {
        return None;
    }
    let count = usize::try_from(u64::from_le_bytes(header[12..20].try_into().ok()?)).ok()?;
    let lens_at = 20usize;
    let metas_at = lens_at.checked_add(count.checked_mul(4)?)?;
    let arena_at = metas_at.checked_add(count.checked_mul(16)?)?;

    // Validate table bounds before allocating from untrusted counts.
    let lens = data.get(lens_at..metas_at)?;
    let meta_bytes = data.get(metas_at..arena_at)?;
    let arena = data.get(arena_at..)?;
    // A globally valid arena can still contain spans that split a codepoint.
    // Validate UTF-8 once, then check each cumulative boundary below.
    let arena_text = std::str::from_utf8(arena).ok()?;

    let mut spans = Vec::with_capacity(count);
    let mut offset: u32 = 0;
    for chunk in lens.chunks(4) {
        let len = u32::from_le_bytes(chunk.try_into().ok()?);
        spans.push((offset, len));
        offset = offset.checked_add(len)?;
        if !arena_text.is_char_boundary(offset as usize) {
            return None;
        }
    }
    if arena.len() != offset as usize {
        return None; // truncated or padded
    }

    let mut metas = Vec::with_capacity(count);
    for chunk in meta_bytes.chunks(16) {
        metas.push(FileMeta {
            mtime: i64::from_le_bytes(<[u8; 8]>::try_from(&chunk[..8]).ok()?),
            size: u64::from_le_bytes(<[u8; 8]>::try_from(&chunk[8..]).ok()?),
        });
    }
    Some(PathStore {
        arena: arena.to_vec().into_boxed_slice(),
        spans,
        metas,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(mtime: i64, size: u64) -> FileMeta {
        FileMeta { mtime, size }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        let entries = vec![
            ("/a/b.txt".to_string(), meta(1000, 42)),
            ("/c/déjà vu.md".to_string(), meta(2000, 7)),
            (String::new(), meta(0, 0)),
        ];
        save(&entries, &file).unwrap();
        let store = load(&file).unwrap();
        assert_eq!(store.len(), 3);
        assert_eq!(store.get(0), "/a/b.txt");
        assert_eq!(store.get(1), "/c/déjà vu.md");
        assert_eq!(store.get(2), "");
        assert_eq!(store.meta(0), meta(1000, 42));
        assert_eq!(store.meta(1), meta(2000, 7));
        assert_eq!(store.iter().count(), 3);
    }

    #[test]
    fn from_entries_preserves_unicode_and_empty_paths() {
        let entries = vec![
            (String::new(), meta(0, 0)),
            ("é/日本/🦀".to_string(), meta(1, 2)),
            (String::new(), meta(3, 4)),
            ("résumé".to_string(), meta(5, 6)),
        ];
        let store = PathStore::from_entries(&entries);
        for (i, (path, metadata)) in entries.iter().enumerate() {
            assert_eq!(store.get(i), path);
            assert_eq!(store.meta(i), *metadata);
        }
        assert!(PathStore::from_entries(&[]).is_empty());
    }

    #[test]
    fn arena_size_checks_individual_and_cumulative_limits() {
        let max = u32::MAX as usize;
        assert_eq!(checked_arena_size([]).unwrap(), 0);
        assert_eq!(checked_arena_size([max]).unwrap(), u32::MAX);
        assert_eq!(checked_arena_size([max - 1, 1, 0]).unwrap(), u32::MAX);
        assert_eq!(
            checked_arena_size([max, 1]).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        if let Some(too_large) = max.checked_add(1) {
            assert_eq!(
                checked_arena_size([too_large]).unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
        assert!(checked_arena_size([usize::MAX, 1]).is_err());
    }

    #[test]
    fn valid_utf8_arena_with_split_codepoints_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        for character in ["é", "日", "🦀"] {
            save(
                &[
                    (character.to_string(), meta(1, 1)),
                    (String::new(), meta(2, 2)),
                ],
                &file,
            )
            .unwrap();
            let original = std::fs::read(&file).unwrap();
            assert!(std::str::from_utf8(&original[60..]).is_ok());
            for split in 1..character.len() {
                let mut bytes = original.clone();
                bytes[20..24].copy_from_slice(&(split as u32).to_le_bytes());
                bytes[24..28].copy_from_slice(&((character.len() - split) as u32).to_le_bytes());
                std::fs::write(&file, bytes).unwrap();
                assert!(load(&file).is_none(), "{character} split at {split}");
            }
        }
    }

    #[test]
    fn span_lengths_outside_arena_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        save(&[("ab".to_string(), meta(1, 1))], &file).unwrap();
        let original = std::fs::read(&file).unwrap();
        for len in [0u32, 1, 3, u32::MAX] {
            let mut bytes = original.clone();
            bytes[20..24].copy_from_slice(&len.to_le_bytes());
            std::fs::write(&file, bytes).unwrap();
            assert!(load(&file).is_none());
        }
    }

    #[test]
    fn missing_file_is_none() {
        assert!(load(std::path::Path::new("/nonexistent/index.bin")).is_none());
    }

    #[test]
    fn corrupt_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        std::fs::write(&file, b"garbage").unwrap();
        assert!(load(&file).is_none());
    }

    #[test]
    fn truncated_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        save(
            &[("/a/very/long/path/to/a/file.txt".to_string(), meta(1, 1))],
            &file,
        )
        .unwrap();
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() - 4]).unwrap();
        assert!(load(&file).is_none());
    }

    #[test]
    fn invalid_utf8_arena_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        save(&[("/ab".to_string(), meta(1, 1))], &file).unwrap();
        let mut bytes = std::fs::read(&file).unwrap();
        let n = bytes.len();
        bytes[n - 1] = 0xff; // stomp an arena byte
        std::fs::write(&file, &bytes).unwrap();
        assert!(load(&file).is_none());
    }

    #[test]
    fn version_mismatch_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        save(&[("/a".to_string(), meta(1, 1))], &file).unwrap();
        let mut bytes = std::fs::read(&file).unwrap();
        bytes[8] = 99; // stomp the version field
        std::fs::write(&file, &bytes).unwrap();
        assert!(load(&file).is_none());
    }

    #[test]
    fn tmp_names_are_unique_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        let a = tmp_path(&file);
        let b = tmp_path(&file);
        assert_ne!(a, b);
        assert_eq!(a.parent(), b.parent());
    }
}
