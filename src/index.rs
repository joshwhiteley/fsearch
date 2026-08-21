use crate::walker::FileMeta;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

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

    pub fn from_entries(entries: &[(String, FileMeta)]) -> PathStore {
        let total: usize = entries.iter().map(|(p, _)| p.len()).sum();
        let mut arena = Vec::with_capacity(total);
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
        // the arena is validated as UTF-8 when constructed/loaded
        unsafe { std::str::from_utf8_unchecked(&self.arena[off as usize..(off + len) as usize]) }
    }

    pub fn meta(&self, i: usize) -> FileMeta {
        self.metas[i]
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        (0..self.len()).map(|i| self.get(i))
    }
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

/// v3 layout: magic, version, count, then three contiguous tables —
/// lengths (u32), metadata (i64 mtime + u64 size), and the path arena.
pub fn save(entries: &[(String, FileMeta)], path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut w = BufWriter::new(std::fs::File::create(&tmp)?);
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
    }
    std::fs::rename(&tmp, path)
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
    let count = u64::from_le_bytes(header[12..20].try_into().ok()?) as usize;
    let lens_at = 20usize;
    let metas_at = lens_at.checked_add(count.checked_mul(4)?)?;
    let arena_at = metas_at.checked_add(count.checked_mul(16)?)?;

    let mut spans = Vec::with_capacity(count.min(8_000_000));
    let mut offset: u32 = 0;
    let lens = data.get(lens_at..metas_at)?;
    for chunk in lens.chunks(4) {
        let len = u32::from_le_bytes(chunk.try_into().ok()?);
        spans.push((offset, len));
        offset = offset.checked_add(len)?;
    }
    let mut metas = Vec::with_capacity(count.min(8_000_000));
    let meta_bytes = data.get(metas_at..arena_at)?;
    for chunk in meta_bytes.chunks(16) {
        metas.push(FileMeta {
            mtime: i64::from_le_bytes(<[u8; 8]>::try_from(&chunk[..8]).ok()?),
            size: u64::from_le_bytes(<[u8; 8]>::try_from(&chunk[8..]).ok()?),
        });
    }
    let arena = data.get(arena_at..)?;
    if arena.len() != offset as usize {
        return None; // truncated or padded
    }
    // one linear validation pass; from then on gets are zero-cost
    std::str::from_utf8(arena).ok()?;
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
}
