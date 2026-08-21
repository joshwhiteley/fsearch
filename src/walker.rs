use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

/// Per-file metadata carried through the index.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FileMeta {
    pub mtime: i64,
    pub size: u64,
}

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
pub fn mtime_cmp(a: &(String, FileMeta), b: &(String, FileMeta)) -> std::cmp::Ordering {
    b.1.mtime.cmp(&a.1.mtime).then_with(|| a.0.cmp(&b.0))
}

/// Emits .app bundles found at depth <= 2 under `dirs` as plain entries
/// (no trailing slash, real mtime, size 0) so they rank and open like
/// files — the default excludes never see them and plain queries match.
fn emit_apps_from(dirs: &[PathBuf], tx: &Sender<(String, FileMeta)>) {
    let emit = |path: &std::path::Path| {
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        let _ = tx.send((
            path.to_string_lossy().into_owned(),
            FileMeta { mtime, size: 0 },
        ));
    };
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if path.extension().is_some_and(|e| e == "app") {
                emit(&path);
            } else if let Ok(sub) = std::fs::read_dir(&path) {
                // one level deeper catches /Applications/Utilities
                for s in sub.flatten() {
                    let sp = s.path();
                    if s.file_type().is_ok_and(|t| t.is_dir())
                        && sp.extension().is_some_and(|e| e == "app")
                    {
                        emit(&sp);
                    }
                }
            }
        }
    }
}

/// Where app bundles live; empty off macOS.
fn default_app_dirs() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let mut dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

/// Walks the roots and returns all entries (with metadata) newest-first.
pub fn collect_sorted(
    roots: &[PathBuf],
    excludes: &GlobSet,
    apps: bool,
) -> (Vec<(String, FileMeta)>, WalkStats) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stats = walk(roots, excludes, apps, &tx);
    drop(tx);
    let mut entries: Vec<(String, FileMeta)> = rx.into_iter().collect();
    entries.sort_unstable_by(mtime_cmp);
    (entries, stats)
}

/// Sends `(path, meta)` pairs; times are seconds since the Unix epoch.
/// With `apps`, .app bundles from the standard application folders are
/// appended after the walk.
pub fn walk(
    roots: &[PathBuf],
    excludes: &GlobSet,
    apps: bool,
    tx: &Sender<(String, FileMeta)>,
) -> WalkStats {
    if apps {
        emit_apps_from(&default_app_dirs(), tx);
    }
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
                Ok(e) if e.file_type().is_some() && e.depth() > 0 => {
                    let is_file = e.file_type().is_some_and(|t| t.is_file());
                    let is_dir = e.file_type().is_some_and(|t| t.is_dir());
                    if !is_file && !is_dir {
                        return WalkState::Continue;
                    }
                    if is_file {
                        files.fetch_add(1, Ordering::Relaxed);
                    }
                    let meta = e.metadata().ok();
                    let mtime = meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs() as i64);
                    let size = if is_file {
                        meta.as_ref().map_or(0, |m| m.len())
                    } else {
                        0
                    };
                    let mut path = e.path().to_string_lossy().into_owned();
                    if is_dir {
                        // directories are marked with a trailing slash
                        path.push('/');
                    }
                    if tx.send((path, FileMeta { mtime, size })).is_err() {
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
        let stats = walk(&[PathBuf::from(root)], &set, false, &tx);
        drop(tx);
        let paths: Vec<String> = rx.into_iter().map(|(p, _)| p).collect();
        let files = paths.iter().filter(|p| !p.ends_with('/')).count();
        assert_eq!(stats.files as usize, files);
        paths
    }

    #[test]
    fn app_bundles_emit_without_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Foo.app/Contents")).unwrap();
        std::fs::create_dir_all(dir.path().join("Utilities/Deep.app")).unwrap();
        std::fs::write(dir.path().join("note.txt"), "x").unwrap();
        let (tx, rx) = mpsc::channel();
        emit_apps_from(&[dir.path().to_path_buf()], &tx);
        drop(tx);
        let mut paths: Vec<String> = rx.into_iter().map(|(p, _)| p).collect();
        paths.sort();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("Foo.app"));
        assert!(paths[1].ends_with("Utilities/Deep.app"));
        assert!(paths.iter().all(|p| !p.ends_with('/')));
    }

    #[test]
    fn directories_are_indexed_with_trailing_slash() {
        let dir = tree();
        let paths = walk_all(dir.path(), &[]);
        assert!(paths.iter().any(|p| p.ends_with("/docs/")));
        // the root itself is not an entry
        let root = format!("{}/", dir.path().to_string_lossy());
        assert!(!paths.contains(&root));
        // excluded dirs stay excluded
        let paths = walk_all(dir.path(), &["node_modules"]);
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
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
        let (entries, stats) = collect_sorted(&[dir.path().to_path_buf()], &set, false);
        assert_eq!(stats.files, 2);
        assert!(entries[0].0.ends_with("new.txt"));
        assert!(entries[1].0.ends_with("old.txt"));
        assert_eq!(entries[0].1.size, 1); // "x"
    }

    #[test]
    fn emits_plausible_mtimes() {
        let dir = tree();
        let set = build_exclude_set(&[]).unwrap();
        let (tx, rx) = mpsc::channel();
        walk(&[PathBuf::from(dir.path())], &set, false, &tx);
        drop(tx);
        let entries: Vec<(String, FileMeta)> = rx.into_iter().collect();
        assert!(!entries.is_empty());
        // every freshly created file has an mtime after 2020-01-01
        assert!(entries.iter().all(|(_, m)| m.mtime > 1_577_836_800));
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
