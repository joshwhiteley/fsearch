use crate::walker::FileMeta;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"FSEARCH\0";
const VERSION: u32 = 2;

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
        for (p, meta) in entries {
            w.write_all(&(p.len() as u32).to_le_bytes())?;
            w.write_all(p.as_bytes())?;
            w.write_all(&meta.mtime.to_le_bytes())?;
            w.write_all(&meta.size.to_le_bytes())?;
        }
        w.flush()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn load(path: &Path) -> Option<(Vec<String>, Vec<FileMeta>)> {
    // one bulk read, then parse from the slice — measurably faster than
    // per-record buffered reads on multi-million-path caches
    let data = std::fs::read(path).ok()?;
    let header = data.get(..20)?;
    if &header[..8] != MAGIC {
        return None;
    }
    if u32::from_le_bytes(header[8..12].try_into().ok()?) != VERSION {
        return None;
    }
    let count = u64::from_le_bytes(header[12..20].try_into().ok()?) as usize;
    let mut paths = Vec::with_capacity(count.min(4_000_000));
    let mut metas = Vec::with_capacity(count.min(4_000_000));
    let mut pos = 20;
    for _ in 0..count {
        let len_bytes = data.get(pos..pos + 4)?;
        let len = u32::from_le_bytes(len_bytes.try_into().ok()?) as usize;
        pos += 4;
        let bytes = data.get(pos..pos + len)?;
        pos += len;
        paths.push(std::str::from_utf8(bytes).ok()?.to_string());
        let mtime = i64::from_le_bytes(data.get(pos..pos + 8)?.try_into().ok()?);
        pos += 8;
        let size = u64::from_le_bytes(data.get(pos..pos + 8)?.try_into().ok()?);
        pos += 8;
        metas.push(FileMeta { mtime, size });
    }
    Some((paths, metas))
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
        let (paths, metas) = load(&file).unwrap();
        assert_eq!(paths, vec!["/a/b.txt", "/c/déjà vu.md", ""]);
        assert_eq!(metas, vec![meta(1000, 42), meta(2000, 7), meta(0, 0)]);
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
