use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Copy, Default)]
pub struct WalkStats {
    pub files: u64,
    pub skipped: u64,
}

pub fn build_exclude_set(excludes: &[String]) -> anyhow::Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for pat in excludes {
        b.add(Glob::new(&format!("**/{pat}"))?);
        b.add(Glob::new(&format!("**/{pat}/**"))?);
    }
    Ok(b.build()?)
}

/// Newest first; equal mtimes fall back to path order for determinism.
pub fn mtime_cmp(a: &(String, i64), b: &(String, i64)) -> std::cmp::Ordering {
    b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0))
}

/// Walks the roots and returns all file paths ordered newest-first.
pub fn collect_sorted(roots: &[PathBuf], excludes: &GlobSet) -> (Vec<String>, WalkStats) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stats = walk(roots, excludes, &tx);
    drop(tx);
    let mut entries: Vec<(String, i64)> = rx.into_iter().collect();
    entries.sort_unstable_by(mtime_cmp);
    (entries.into_iter().map(|(p, _)| p).collect(), stats)
}

/// Sends `(path, mtime)` pairs; mtime is seconds since the Unix epoch (0 when unavailable).
pub fn walk(roots: &[PathBuf], excludes: &GlobSet, tx: &Sender<(String, i64)>) -> WalkStats {
    let mut roots = roots.iter().filter(|r| r.exists());
    let Some(first) = roots.next() else {
        return WalkStats::default();
    };
    let mut builder = WalkBuilder::new(first);
    for root in roots {
        builder.add(root);
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let excludes = excludes.clone();
    builder
        .standard_filters(false) // no gitignore, no hidden-file skipping
        .follow_links(false)
        .threads(threads)
        .filter_entry(move |entry| !excludes.is_match(entry.path()));

    let files = AtomicU64::new(0);
    let skipped = AtomicU64::new(0);
    builder.build_parallel().run(|| {
        let tx = tx.clone();
        let files = &files;
        let skipped = &skipped;
        Box::new(move |entry| {
            match entry {
                Ok(e) if e.file_type().is_some_and(|t| t.is_file()) => {
                    files.fetch_add(1, Ordering::Relaxed);
                    let mtime = e
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs() as i64);
                    if tx.send((e.path().to_string_lossy().into_owned(), mtime)).is_err() {
                        return WalkState::Quit;
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                }
            }
            WalkState::Continue
        })
    });
    WalkStats {
        files: files.load(Ordering::Relaxed),
        skipped: skipped.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("docs")).unwrap();
        std::fs::create_dir_all(p.join(".hidden")).unwrap();
        std::fs::create_dir_all(p.join("proj/node_modules/x")).unwrap();
        std::fs::create_dir_all(p.join("Library/Caches")).unwrap();
        std::fs::write(p.join("docs/readme.md"), "hello").unwrap();
        std::fs::write(p.join(".hidden/secret.txt"), "shh").unwrap();
        std::fs::write(p.join("proj/node_modules/x/dep.js"), "x").unwrap();
        std::fs::write(p.join("Library/Caches/junk.dat"), "x").unwrap();
        std::fs::write(p.join("proj/main.rs"), "fn main() {}").unwrap();
        dir
    }

    fn walk_all(root: &std::path::Path, excludes: &[&str]) -> Vec<String> {
        let ex: Vec<String> = excludes.iter().map(|s| s.to_string()).collect();
        let set = build_exclude_set(&ex).unwrap();
        let (tx, rx) = mpsc::channel();
        let stats = walk(&[PathBuf::from(root)], &set, &tx);
        drop(tx);
        let paths: Vec<String> = rx.into_iter().map(|(p, _)| p).collect();
        assert_eq!(stats.files as usize, paths.len());
        paths
    }

    #[test]
    fn collect_sorted_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let hour = std::time::Duration::from_secs(3600);
        let now = std::time::SystemTime::now();
        for (name, age) in [("old.txt", 2), ("new.txt", 0)] {
            let path = dir.path().join(name);
            std::fs::write(&path, "x").unwrap();
            let f = std::fs::File::options().write(true).open(&path).unwrap();
            f.set_modified(now - age * hour).unwrap();
        }
        let set = build_exclude_set(&[]).unwrap();
        let (paths, stats) = collect_sorted(&[dir.path().to_path_buf()], &set);
        assert_eq!(stats.files, 2);
        assert!(paths[0].ends_with("new.txt"));
        assert!(paths[1].ends_with("old.txt"));
    }

    #[test]
    fn emits_plausible_mtimes() {
        let dir = tree();
        let set = build_exclude_set(&[]).unwrap();
        let (tx, rx) = mpsc::channel();
        walk(&[PathBuf::from(dir.path())], &set, &tx);
        drop(tx);
        let entries: Vec<(String, i64)> = rx.into_iter().collect();
        assert!(!entries.is_empty());
        // every freshly created file has an mtime after 2020-01-01
        assert!(entries.iter().all(|(_, m)| *m > 1_577_836_800));
    }

    #[test]
    fn finds_files_including_hidden() {
        let dir = tree();
        let paths = walk_all(dir.path(), &[]);
        assert!(paths.iter().any(|p| p.ends_with("docs/readme.md")));
        assert!(paths.iter().any(|p| p.ends_with(".hidden/secret.txt")));
    }

    #[test]
    fn name_excludes_prune_directories() {
        let dir = tree();
        let paths = walk_all(dir.path(), &["node_modules"]);
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
        assert!(paths.iter().any(|p| p.ends_with("proj/main.rs")));
    }

    #[test]
    fn path_excludes_match_subpaths() {
        let dir = tree();
        let paths = walk_all(dir.path(), &["Library/Caches"]);
        assert!(!paths.iter().any(|p| p.contains("Library/Caches")));
    }

    #[test]
    fn gitignore_is_not_honored() {
        let dir = tree();
        std::fs::write(dir.path().join(".gitignore"), "docs/\n").unwrap();
        let paths = walk_all(dir.path(), &[]);
        assert!(paths.iter().any(|p| p.ends_with("docs/readme.md")));
    }
}
