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

pub fn walk(roots: &[PathBuf], excludes: &GlobSet, tx: &Sender<String>) -> WalkStats {
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
                    if tx.send(e.path().to_string_lossy().into_owned()).is_err() {
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
        let paths: Vec<String> = rx.into_iter().collect();
        assert_eq!(stats.files as usize, paths.len());
        paths
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
