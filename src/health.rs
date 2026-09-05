//! Read-only index diagnostics and explicit, narrowly scoped cleanup.
use crate::{config::Config, index, sem};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct RootHealth {
    pub path: PathBuf,
    pub readable: bool,
}

#[derive(Debug, Serialize)]
pub struct CacheHealth {
    pub path: PathBuf,
    pub state: &'static str,
    pub bytes: u64,
    pub age_seconds: Option<u64>,
    pub entries: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ExtractedCacheHealth {
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub truncated: bool,
}

fn extracted_cache(path: PathBuf) -> ExtractedCacheHealth {
    let mut report = ExtractedCacheHealth {
        path,
        files: 0,
        bytes: 0,
        truncated: false,
    };
    if let Ok(entries) = std::fs::read_dir(&report.path) {
        for (i, entry) in entries.enumerate() {
            if i == 8192 {
                report.truncated = true;
                break;
            }
            if let Ok(entry) = entry
                && entry.file_type().is_ok_and(|kind| kind.is_file())
                && let Ok(meta) = entry.metadata()
            {
                report.files += 1;
                report.bytes = report.bytes.saturating_add(meta.len());
            }
        }
    }
    report
}

#[derive(Debug, Serialize)]
pub struct Health {
    pub roots: Vec<RootHealth>,
    pub path_index: CacheHealth,
    pub semantic_index: CacheHealth,
    pub semantic_chunks: Option<usize>,
    pub extracted_caches: Vec<ExtractedCacheHealth>,
    pub semantic_feature: bool,
    pub chafa_feature: bool,
    pub remember_history: bool,
    /// This command does not start the interactive watcher or verify on-disk
    /// documents against stored vectors. Never claim freshness from age alone.
    pub live_updates: &'static str,
    pub freshness: &'static str,
}

fn cache_health(path: PathBuf, entries: Option<usize>) -> CacheHealth {
    let meta = std::fs::metadata(&path).ok();
    CacheHealth {
        state: if entries.is_some() {
            "valid"
        } else if meta.is_some() {
            "invalid"
        } else {
            "missing"
        },
        bytes: meta.as_ref().map_or(0, |m| m.len()),
        age_seconds: meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|age| age.as_secs()),
        path,
        entries,
    }
}

pub fn inspect(config: &Config, cache: &Path) -> Health {
    let paths = index::load(cache);
    let semantic_path = cache.with_file_name("semantic.bin");
    let semantic = sem::SemStore::load(&semantic_path);
    Health {
        roots: config
            .roots
            .iter()
            .map(|path| RootHealth {
                path: path.clone(),
                readable: std::fs::read_dir(path).is_ok(),
            })
            .collect(),
        path_index: cache_health(cache.to_path_buf(), paths.as_ref().map(|s| s.len())),
        semantic_index: cache_health(semantic_path, semantic.as_ref().map(|s| s.docs.len())),
        semantic_chunks: semantic.as_ref().map(|s| s.chunk_count()),
        extracted_caches: ["pdftext", "officetext"]
            .into_iter()
            .map(|name| extracted_cache(cache.with_file_name(name)))
            .collect(),
        semantic_feature: cfg!(feature = "semantic"),
        chafa_feature: cfg!(feature = "chafa"),
        remember_history: config.remember_history,
        live_updates: "not running in this command; watcher failures are reported in the interactive UI",
        freshness: "snapshot age is not a freshness guarantee; --reindex refreshes paths, --index-semantic refreshes documents",
    }
}

impl Health {
    pub fn text(&self) -> String {
        let mut text = format!(
            "semantic feature: {}\nchafa feature: {}\nhistory enabled: {}\n",
            self.semantic_feature, self.chafa_feature, self.remember_history
        );
        for root in &self.roots {
            text.push_str(&format!(
                "root: {} ({})\n",
                root.path.display(),
                if root.readable {
                    "readable"
                } else {
                    "missing or unreadable"
                }
            ));
        }
        for (name, cache) in [
            ("path index", &self.path_index),
            ("semantic index", &self.semantic_index),
        ] {
            text.push_str(&format!(
                "{name}: {} — {}\n  {} entries, {}, age {}\n",
                cache.state,
                cache.path.display(),
                cache.entries.map_or("unknown".into(), |n| n.to_string()),
                crate::util::human_size(cache.bytes),
                cache
                    .age_seconds
                    .map_or("unknown".into(), |s| format!("{s}s"))
            ));
        }
        for cache in &self.extracted_caches {
            text.push_str(&format!(
                "extracted cache: {} — {} files, {}{}\n",
                cache.path.display(),
                cache.files,
                crate::util::human_size(cache.bytes),
                if cache.truncated {
                    " (partial count)"
                } else {
                    ""
                }
            ));
        }
        text.push_str(&format!(
            "live updates: {}\n{}\n",
            self.live_updates, self.freshness
        ));
        text
    }
}

fn clear_known(root: &Path, names: &[&str]) -> std::io::Result<usize> {
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(std::io::Error::other(
                "refusing cleanup through a non-directory or symlink",
            ));
        }
        _ => {}
    }
    let mut removed = 0;
    for name in names {
        let path = root.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.is_dir() && !meta.file_type().is_symlink() {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
                removed += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(removed)
}

/// Leaves downloaded models, configuration and original documents untouched.
pub fn clear_cache(root: &Path) -> std::io::Result<usize> {
    clear_known(
        root,
        &["index.bin", "semantic.bin", "pdftext", "officetext"],
    )
}

pub fn clear_history(root: &Path) -> std::io::Result<usize> {
    clear_known(root, &["history", "queries", "session.toml"])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspection_does_not_build_or_download() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("absent/index.bin");
        let config = Config {
            roots: vec![dir.path().into(), dir.path().join("missing")],
            ..Config::default()
        };
        let health = inspect(&config, &cache);
        assert_eq!(health.path_index.state, "missing");
        assert!(health.roots[0].readable);
        assert!(!health.roots[1].readable);
        assert!(!cache.parent().unwrap().exists());
    }
    #[test]
    fn cleanup_is_scoped_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["index.bin", "semantic.bin", "config.toml", "notes.txt"] {
            std::fs::write(dir.path().join(name), "keep unless cache").unwrap();
        }
        std::fs::create_dir(dir.path().join("models")).unwrap();
        assert_eq!(clear_cache(dir.path()).unwrap(), 2);
        assert_eq!(clear_cache(dir.path()).unwrap(), 0);
        assert!(dir.path().join("models").exists());
        assert!(dir.path().join("notes.txt").exists());
        assert!(dir.path().join("config.toml").exists());
    }
    #[cfg(unix)]
    #[test]
    fn cleanup_does_not_follow_links() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "keep").unwrap();
        symlink(outside.path(), dir.path().join("pdftext")).unwrap();
        clear_cache(dir.path()).unwrap();
        assert!(outside.path().join("secret").exists());
        symlink(outside.path(), dir.path().join("root")).unwrap();
        assert!(clear_cache(&dir.path().join("root")).is_err());
    }
}
