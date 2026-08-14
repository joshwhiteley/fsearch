# file-search (`fsearch`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An Alfred-inspired terminal file searcher for macOS: instant fuzzy/regex filename search over a persisted home-directory index, plus on-demand regex search inside file contents.

**Architecture:** A single Rust crate. A persisted path index (`~/.cache/fsearch/index.bin`) loads in milliseconds at launch while a background thread re-walks the roots and swaps in a fresh index. A search worker thread runs fuzzy (nucleo) or regex matching per keystroke; a `>`-prefixed query spawns a streaming parallel content grep (ripgrep's engine crates). A ratatui TUI polls the engine and renders input/results/preview.

**Tech Stack:** Rust 2024 edition. `ratatui` 0.30 (+ its `crossterm` re-export), `ignore` 0.4 + `globset` 0.4, `nucleo-matcher` 0.3, `regex` 1, `grep-searcher` 0.1 + `grep-regex` 0.1, `rayon` 1, `serde` 1 + `toml` 1, `dirs` 6, `anyhow` 1; dev: `tempfile` 3.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-14-file-search-design.md` — all behavior defers to it.
- Commits: plain style — `add: [thing]`, `feat: [feature]`, `bug: [fix]`, `docs: [doc]`. Lowercase, no scopes, no bodies unless needed.
- Sole author: Josh Whiteley <joshwhiteley89@gmail.com> (already the repo's git config). **Never add Co-Authored-By or any trailer.**
- Config file: `$XDG_CONFIG_HOME/fsearch/config.toml`, falling back to `~/.config/fsearch/config.toml`.
- Cache file: `$XDG_CACHE_HOME/fsearch/index.bin`, falling back to `~/.cache/fsearch/index.bin`.
- Walker: gitignore semantics disabled, hidden files included, excludes from config always applied, symlinks not followed, files only (no dirs) in the index.
- Content search: skip files larger than `max_content_filesize` (default 2 MiB) and binary files (NUL byte detection); debounce 300 ms; smart-case.
- Filename search: smart-case in both fuzzy and regex modes; result cap 500 rows. Content result cap 1000 rows.
- Errors never crash the TUI: invalid regex → status message and keep last results; unreadable files → skip and count; corrupt cache → silently rebuild.
- Every task: `cargo test` and `cargo clippy --all-targets -- -D warnings` must pass before its commit.

## File Structure

```
Cargo.toml            crate manifest, release profile
LICENSE               MIT, copyright Josh Whiteley
.gitignore            /target
README.md             install, usage, keys, config reference
src/main.rs           entry: load config → Engine → tui::run
src/config.rs         Config load/create, defaults, tilde expansion
src/walker.rs         parallel walk, exclude globset, WalkStats
src/index.rs          cache save/load (versioned binary format)
src/matcher.rs        fuzzy (nucleo) + regex filename matching
src/content.rs        streaming parallel content grep
src/engine.rs         orchestration: threads, query parsing, debounce, state
src/actions.rs        open / reveal-in-Finder / copy-path
src/tui.rs            ratatui app: layout, events, preview, status line
tests/engine_test.rs  headless end-to-end over a temp tree
```

Unit tests live in `#[cfg(test)] mod tests` inside each module.

---

### Task 1: Scaffold

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `.gitignore`, `LICENSE`

**Interfaces:**
- Produces: a compiling binary crate named `fsearch` with all dependencies resolved; later tasks add modules via `mod` declarations in `main.rs`.

- [ ] **Step 1: `cargo init` and manifest**

Run `cargo init --name fsearch` in the repo root, then replace `Cargo.toml` with:

```toml
[package]
name = "fsearch"
version = "0.1.0"
edition = "2024"
license = "MIT"
description = "Fast Alfred-style file search for the terminal"

[dependencies]
anyhow = "1"
dirs = "6"
globset = "0.4"
grep-regex = "0.1"
grep-searcher = "0.1"
ignore = "0.4"
nucleo-matcher = "0.3"
ratatui = "0.30"
rayon = "1"
regex = "1"
serde = { version = "1", features = ["derive"] }
toml = "1"

[dev-dependencies]
tempfile = "3"

[profile.release]
lto = "thin"
```

`.gitignore` (cargo init creates it): must contain `/target`.

`src/main.rs`:

```rust
fn main() {
    println!("fsearch");
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build && cargo clippy --all-targets -- -D warnings`
Expected: clean build. (First build fetches deps; if `ratatui::crossterm` turns out not to be re-exported in 0.30, add `crossterm = "0.29"` here.)

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src/main.rs
git commit -m "add: cargo scaffold with dependencies"
```

- [ ] **Step 4: MIT license, commit**

Create `LICENSE` with the standard MIT text, `Copyright (c) 2026 Josh Whiteley`.

```bash
git add LICENSE
git commit -m "add: mit license"
```

---

### Task 2: Config

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

**Interfaces:**
- Produces:
  - `pub struct Config { pub roots: Vec<PathBuf>, pub excludes: Vec<String>, pub max_content_filesize: u64 }` with `impl Default`
  - `pub fn default_config_path() -> PathBuf`
  - `pub fn load_or_create(path: &Path) -> anyhow::Result<Config>` — missing file: writes commented default TOML, returns defaults; invalid TOML: `Err`
  - `pub fn expand_tilde(s: &str) -> PathBuf`

- [ ] **Step 1: Write failing tests** (in `src/config.rs` under the impl-to-be)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.roots, vec![dirs::home_dir().unwrap()]);
        assert!(c.excludes.iter().any(|e| e == ".git"));
        assert_eq!(c.max_content_filesize, 2 * 1024 * 1024);
    }

    #[test]
    fn missing_file_is_created_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let c = load_or_create(&path).unwrap();
        assert_eq!(c.roots, Config::default().roots);
        assert!(path.exists());
        // the created file must itself parse back
        let again = load_or_create(&path).unwrap();
        assert_eq!(again.excludes, c.excludes);
    }

    #[test]
    fn partial_file_merges_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "roots = [\"/tmp\"]\n").unwrap();
        let c = load_or_create(&path).unwrap();
        assert_eq!(c.roots, vec![PathBuf::from("/tmp")]);
        assert_eq!(c.excludes, Config::default().excludes);
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "roots = not toml").unwrap();
        assert!(load_or_create(&path).is_err());
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/Documents"), home.join("Documents"));
        assert_eq!(expand_tilde("/etc"), PathBuf::from("/etc"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test config`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement**

```rust
use anyhow::Context;
use std::path::{Path, PathBuf};

pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".cache",
    ".npm",
    ".Trash",
    ".venv",
    "__pycache__",
    "Library/Caches",
    "Library/Containers",
    "Library/Application Support/MobileSync",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub roots: Vec<PathBuf>,
    pub excludes: Vec<String>,
    pub max_content_filesize: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: vec![dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))],
            excludes: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            max_content_filesize: 2 * 1024 * 1024,
        }
    }
}

#[derive(serde::Deserialize)]
struct RawConfig {
    roots: Option<Vec<String>>,
    excludes: Option<Vec<String>>,
    max_content_filesize: Option<u64>,
}

pub fn expand_tilde(s: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    if s == "~" {
        home
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(s)
    }
}

pub fn default_config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")).join(".config")
        });
    base.join("fsearch").join("config.toml")
}

const DEFAULT_TEMPLATE_HEADER: &str = "\
# fsearch configuration
# roots: directories to index (~ expands to your home directory)
# excludes: directory or file names/paths never indexed
# max_content_filesize: content search skips files larger than this (bytes)
";

pub fn load_or_create(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        let d = Config::default();
        let body = format!(
            "{}roots = [\"~\"]\nexcludes = {:?}\nmax_content_filesize = {}\n",
            DEFAULT_TEMPLATE_HEADER,
            d.excludes,
            d.max_content_filesize
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("creating config dir")?;
        }
        std::fs::write(path, body).context("writing default config")?;
        return Ok(d);
    }
    let text = std::fs::read_to_string(path).context("reading config")?;
    let raw: RawConfig = toml::from_str(&text).context("parsing config.toml")?;
    let d = Config::default();
    Ok(Config {
        roots: raw
            .roots
            .map(|v| v.iter().map(|s| expand_tilde(s)).collect())
            .unwrap_or(d.roots),
        excludes: raw.excludes.unwrap_or(d.excludes),
        max_content_filesize: raw.max_content_filesize.unwrap_or(d.max_content_filesize),
    })
}
```

Add `mod config;` to `src/main.rs`. (Modules not yet referenced from `main` need `#[allow(dead_code)]` on the module declaration until Task 9 wires them; remove those allows in Task 9.)

- [ ] **Step 4: Run tests**

Run: `cargo test config && cargo clippy --all-targets -- -D warnings`
Expected: all 5 pass, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: config file loading with defaults"
```

---

### Task 3: Walker

**Files:**
- Create: `src/walker.rs`
- Modify: `src/main.rs` (add `mod walker;`)

**Interfaces:**
- Consumes: `config::Config` fields (`roots`, `excludes`).
- Produces:
  - `pub struct WalkStats { pub files: u64, pub skipped: u64 }`
  - `pub fn build_exclude_set(excludes: &[String]) -> anyhow::Result<globset::GlobSet>`
  - `pub fn walk(roots: &[PathBuf], excludes: &globset::GlobSet, tx: &std::sync::mpsc::Sender<String>) -> WalkStats` — sends every file path as an owned `String`; returns after the walk completes.

- [ ] **Step 1: Write failing tests**

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test walker`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement**

```rust
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
```

Add `mod walker;` to `src/main.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test walker && cargo clippy --all-targets -- -D warnings`
Expected: 4 pass, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/walker.rs src/main.rs
git commit -m "feat: parallel file walker with excludes"
```

---

### Task 4: Index cache

**Files:**
- Create: `src/index.rs`
- Modify: `src/main.rs` (add `mod index;`)

**Interfaces:**
- Produces:
  - `pub fn default_cache_path() -> PathBuf`
  - `pub fn save(paths: &[String], path: &Path) -> std::io::Result<()>` — atomic (temp file + rename)
  - `pub fn load(path: &Path) -> Option<Vec<String>>` — `None` on missing/corrupt/version mismatch

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        let paths = vec![
            "/a/b.txt".to_string(),
            "/c/déjà vu.md".to_string(),
            String::new(),
        ];
        save(&paths, &file).unwrap();
        assert_eq!(load(&file).unwrap(), paths);
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
        save(&["/a/very/long/path/to/a/file.txt".to_string()], &file).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() - 4]).unwrap();
        assert!(load(&file).is_none());
    }

    #[test]
    fn version_mismatch_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.bin");
        save(&["/a".to_string()], &file).unwrap();
        let mut bytes = std::fs::read(&file).unwrap();
        bytes[8] = 99; // stomp the version field
        std::fs::write(&file, &bytes).unwrap();
        assert!(load(&file).is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test index`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement**

Format: 8-byte magic `b"FSEARCH\0"`, `u32` LE version (=1), `u64` LE path count, then per path `u32` LE byte length + UTF-8 bytes.

```rust
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"FSEARCH\0";
const VERSION: u32 = 1;

pub fn default_cache_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")).join(".cache")
        });
    base.join("fsearch").join("index.bin")
}

pub fn save(paths: &[String], path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut w = BufWriter::new(std::fs::File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&(paths.len() as u64).to_le_bytes())?;
        for p in paths {
            w.write_all(&(p.len() as u32).to_le_bytes())?;
            w.write_all(p.as_bytes())?;
        }
        w.flush()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn load(path: &Path) -> Option<Vec<String>> {
    let mut r = BufReader::new(std::fs::File::open(path).ok()?);
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic).ok()?;
    if &magic != MAGIC {
        return None;
    }
    let mut u32buf = [0u8; 4];
    r.read_exact(&mut u32buf).ok()?;
    if u32::from_le_bytes(u32buf) != VERSION {
        return None;
    }
    let mut u64buf = [0u8; 8];
    r.read_exact(&mut u64buf).ok()?;
    let count = u64::from_le_bytes(u64buf) as usize;
    let mut paths = Vec::with_capacity(count.min(4_000_000));
    for _ in 0..count {
        r.read_exact(&mut u32buf).ok()?;
        let len = u32::from_le_bytes(u32buf) as usize;
        let mut bytes = vec![0u8; len];
        r.read_exact(&mut bytes).ok()?;
        paths.push(String::from_utf8(bytes).ok()?);
    }
    Some(paths)
}
```

Add `mod index;` to `src/main.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test index && cargo clippy --all-targets -- -D warnings`
Expected: 5 pass, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/index.rs src/main.rs
git commit -m "feat: persisted path index cache"
```

---

### Task 5: Filename matcher

**Files:**
- Create: `src/matcher.rs`
- Modify: `src/main.rs` (add `mod matcher;`)

**Interfaces:**
- Produces:
  - `pub enum FilenameMode { Fuzzy, Regex }` (derives `Debug, Clone, Copy, PartialEq`)
  - `pub fn search(paths: &[String], query: &str, mode: FilenameMode, limit: usize) -> Result<Vec<usize>, String>` — ranked indices into `paths`; empty query → first `limit` indices; `Err(msg)` only for an invalid regex.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_query_returns_head() {
        let p = paths(&["/a", "/b", "/c"]);
        assert_eq!(search(&p, "", FilenameMode::Fuzzy, 2).unwrap(), vec![0, 1]);
    }

    #[test]
    fn fuzzy_ranks_filename_match_over_scattered() {
        let p = paths(&[
            "/code/rust/tools/everything/notes.txt", // scattered match for "rest"
            "/docs/rest-api.md",                     // filename match
        ]);
        let r = search(&p, "rest", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r[0], 1);
    }

    #[test]
    fn fuzzy_is_smart_case() {
        let p = paths(&["/docs/README.md", "/docs/readme-draft.md"]);
        // lowercase query matches both
        assert_eq!(search(&p, "readme", FilenameMode::Fuzzy, 10).unwrap().len(), 2);
        // uppercase query matches only the uppercase path
        let r = search(&p, "README", FilenameMode::Fuzzy, 10).unwrap();
        assert_eq!(r, vec![0]);
    }

    #[test]
    fn regex_filters_by_full_path() {
        let p = paths(&["/a/report_2024.pdf", "/a/report.txt", "/b/2024.pdf"]);
        let r = search(&p, r"report_\d+\.pdf$", FilenameMode::Regex, 10).unwrap();
        assert_eq!(r, vec![0]);
    }

    #[test]
    fn regex_is_smart_case() {
        let p = paths(&["/a/README.md", "/a/readme.md"]);
        assert_eq!(search(&p, "readme", FilenameMode::Regex, 10).unwrap().len(), 2);
        assert_eq!(search(&p, "README", FilenameMode::Regex, 10).unwrap(), vec![0]);
    }

    #[test]
    fn invalid_regex_is_err() {
        let p = paths(&["/a"]);
        assert!(search(&p, "[unclosed", FilenameMode::Regex, 10).is_err());
    }

    #[test]
    fn limit_is_respected() {
        let p: Vec<String> = (0..100).map(|i| format!("/f/file{i}.txt")).collect();
        assert_eq!(search(&p, "file", FilenameMode::Fuzzy, 5).unwrap().len(), 5);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test matcher`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rayon::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilenameMode {
    Fuzzy,
    Regex,
}

pub fn search(
    paths: &[String],
    query: &str,
    mode: FilenameMode,
    limit: usize,
) -> Result<Vec<usize>, String> {
    if query.is_empty() {
        return Ok((0..paths.len().min(limit)).collect());
    }
    match mode {
        FilenameMode::Fuzzy => Ok(fuzzy(paths, query, limit)),
        FilenameMode::Regex => regex_filter(paths, query, limit),
    }
}

fn fuzzy(paths: &[String], query: &str, limit: usize) -> Vec<usize> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut scored: Vec<(u32, usize)> = paths
        .par_chunks(16_384)
        .enumerate()
        .map(|(chunk_no, chunk)| {
            let mut cfg = Config::DEFAULT;
            cfg.set_match_paths();
            let mut matcher = Matcher::new(cfg);
            let mut buf = Vec::new();
            let base = chunk_no * 16_384;
            chunk
                .iter()
                .enumerate()
                .filter_map(|(i, path)| {
                    pattern
                        .score(Utf32Str::new(path, &mut buf), &mut matcher)
                        .map(|score| (score, base + i))
                })
                .collect::<Vec<_>>()
        })
        .flatten()
        .collect();
    scored.par_sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(limit);
    scored.into_iter().map(|(_, i)| i).collect()
}

fn regex_filter(paths: &[String], query: &str, limit: usize) -> Result<Vec<usize>, String> {
    let smart_case_insensitive = !query.chars().any(|c| c.is_uppercase());
    let re = regex::RegexBuilder::new(query)
        .case_insensitive(smart_case_insensitive)
        .build()
        .map_err(|e| e.to_string())?;
    let mut hits: Vec<usize> = paths
        .par_iter()
        .enumerate()
        .filter(|(_, p)| re.is_match(p))
        .map(|(i, _)| i)
        .collect();
    hits.sort_unstable();
    hits.truncate(limit);
    Ok(hits)
}
```

Add `mod matcher;` to `src/main.rs`.

Note: if `Config::set_match_paths` has a different name in nucleo-matcher 0.3, check `cargo doc` — the intent is nucleo's path-scoring config that boosts filename-segment matches.

- [ ] **Step 4: Run tests**

Run: `cargo test matcher && cargo clippy --all-targets -- -D warnings`
Expected: 7 pass, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/matcher.rs src/main.rs
git commit -m "feat: fuzzy and regex filename matching"
```

---

### Task 6: Content search

**Files:**
- Create: `src/content.rs`
- Modify: `src/main.rs` (add `mod content;`)

**Interfaces:**
- Consumes: nothing internal (paths come in as strings).
- Produces:
  - `pub struct ContentMatch { pub path: String, pub line_number: u64, pub line: String }` (derives `Debug, Clone, PartialEq`)
  - `pub fn search(paths: &[String], pattern: &str, max_filesize: u64, cancel: &AtomicBool, tx: &std::sync::mpsc::Sender<ContentMatch>) -> Result<(), String>` — streams matches; smart-case; skips big/binary/unreadable files; ≤20 matches per file; returns `Err(msg)` only for an invalid pattern; stops early when `cancel` is set or the receiver hangs up.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    fn run(
        dir: &std::path::Path,
        files: &[(&str, &[u8])],
        pattern: &str,
        max: u64,
    ) -> Result<Vec<ContentMatch>, String> {
        let mut paths = Vec::new();
        for (name, body) in files {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            paths.push(p.to_string_lossy().into_owned());
        }
        let (tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        search(&paths, pattern, max, &cancel, &tx)?;
        drop(tx);
        Ok(rx.into_iter().collect())
    }

    #[test]
    fn finds_matching_lines_with_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let hits = run(
            dir.path(),
            &[("a.txt", b"one\ntwo needle two\nthree\nneedle\n")],
            "needle",
            1024,
        )
        .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|h| h.line_number == 2 && h.line.contains("two needle two")));
        assert!(hits.iter().any(|h| h.line_number == 4));
    }

    #[test]
    fn smart_case() {
        let dir = tempfile::tempdir().unwrap();
        let files: &[(&str, &[u8])] = &[("a.txt", b"Needle\nneedle\n")];
        assert_eq!(run(dir.path(), files, "needle", 1024).unwrap().len(), 2);
        assert_eq!(run(dir.path(), files, "Needle", 1024).unwrap().len(), 1);
    }

    #[test]
    fn skips_binary_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let big = "needle\n".repeat(200); // > 1KiB when max is 1024
        let hits = run(
            dir.path(),
            &[
                ("bin.dat", b"needle\x00needle" as &[u8]),
                ("big.txt", big.as_bytes()),
                ("ok.txt", b"needle\n"),
            ],
            "needle",
            1024,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("ok.txt"));
    }

    #[test]
    fn regex_patterns_work() {
        let dir = tempfile::tempdir().unwrap();
        let hits = run(
            dir.path(),
            &[("a.txt", b"invoice 2024-08\nno match\n")],
            r"invoice \d{4}-\d{2}",
            1024,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn invalid_pattern_is_err() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(dir.path(), &[], "[bad", 1024).is_err());
    }

    #[test]
    fn per_file_cap_is_20() {
        let dir = tempfile::tempdir().unwrap();
        let body = "needle\n".repeat(100);
        let hits = run(dir.path(), &[("a.txt", body.as_bytes())], "needle", 10_240).unwrap();
        assert_eq!(hits.len(), 20);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test content`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, PartialEq)]
pub struct ContentMatch {
    pub path: String,
    pub line_number: u64,
    pub line: String,
}

const PER_FILE_CAP: usize = 20;

pub fn search(
    paths: &[String],
    pattern: &str,
    max_filesize: u64,
    cancel: &AtomicBool,
    tx: &Sender<ContentMatch>,
) -> Result<(), String> {
    let smart_case_insensitive = !pattern.chars().any(|c| c.is_uppercase());
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(smart_case_insensitive)
        .build(pattern)
        .map_err(|e| e.to_string())?;

    paths.par_iter().for_each(|path| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match std::fs::metadata(path) {
            Ok(m) if m.len() <= max_filesize && m.is_file() => {}
            _ => return,
        }
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(0))
            .line_number(true)
            .build();
        let mut sent = 0usize;
        let _ = searcher.search_path(
            &matcher,
            path,
            UTF8(|line_number, line| {
                if cancel.load(Ordering::Relaxed) || sent >= PER_FILE_CAP {
                    return Ok(false);
                }
                let hit = ContentMatch {
                    path: path.clone(),
                    line_number,
                    line: line.trim_end().to_string(),
                };
                sent += 1;
                if tx.send(hit).is_err() {
                    cancel.store(true, Ordering::Relaxed);
                    return Ok(false);
                }
                Ok(true)
            }),
        );
    });
    Ok(())
}
```

Add `mod content;` to `src/main.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test content && cargo clippy --all-targets -- -D warnings`
Expected: 6 pass, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/content.rs src/main.rs
git commit -m "feat: regex search inside file contents"
```

---

### Task 7: Engine

**Files:**
- Create: `src/engine.rs`, `tests/engine_test.rs`
- Modify: `src/main.rs` (add `mod engine;` — and make modules `pub` as needed for the integration test: the simplest route is `src/lib.rs`; see Step 0)

**Interfaces:**
- Consumes: `config::Config`, `walker::{build_exclude_set, walk}`, `index::{load, save}`, `matcher::{search, FilenameMode}`, `content::{search, ContentMatch}`.
- Produces:
  - `pub enum Mode { Fuzzy, Regex, Content }` (derives `Debug, Clone, Copy, PartialEq`)
  - `pub fn parse_query(input: &str, regex_mode: bool) -> (Mode, String)` — `>`-prefix → `(Content, rest_trimmed_start)`; else `(Fuzzy|Regex, input)`
  - `pub struct ResultRow { pub path: String, pub line_number: Option<u64>, pub line: Option<String> }`
  - `pub struct EngineStatus { pub indexed: usize, pub indexing: bool, pub matches: usize, pub error: Option<String> }`
  - `pub struct Engine` with:
    - `pub fn new(config: config::Config, cache_path: PathBuf) -> Engine`
    - `pub fn set_query(&mut self, input: &str, regex_mode: bool)`
    - `pub fn tick(&mut self)` — drain worker messages, fire debounced content searches; call every UI frame
    - `pub fn results(&self) -> &[ResultRow]`
    - `pub fn status(&self) -> EngineStatus`
    - `pub fn mode(&self) -> Mode`
  - Constants: `pub const FILENAME_LIMIT: usize = 500;` `pub const CONTENT_LIMIT: usize = 1000;` `pub const CONTENT_DEBOUNCE: Duration = Duration::from_millis(300);`

- [ ] **Step 0: Convert to lib + bin layout**

Create `src/lib.rs`:

```rust
pub mod actions;
pub mod config;
pub mod content;
pub mod engine;
pub mod index;
pub mod matcher;
pub mod tui;
pub mod walker;
```

(`actions`/`tui` lines get added in Tasks 8–9; at this task's end the file lists only the modules that exist.) `src/main.rs` becomes:

```rust
fn main() {
    println!("fsearch");
}
```

with all `mod` declarations removed (they move to `lib.rs`); remove any `#[allow(dead_code)]` markers — the lib exports make everything reachable.

- [ ] **Step 1: Write failing unit tests** (in `src/engine.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_is_fuzzy() {
        assert_eq!(parse_query("notes", false), (Mode::Fuzzy, "notes".to_string()));
    }

    #[test]
    fn parse_respects_regex_toggle() {
        assert_eq!(
            parse_query(r"\.pdf$", true),
            (Mode::Regex, r"\.pdf$".to_string())
        );
    }

    #[test]
    fn parse_gt_prefix_is_content() {
        assert_eq!(
            parse_query("> hello world", false),
            (Mode::Content, "hello world".to_string())
        );
        // regex toggle does not override content mode
        assert_eq!(parse_query(">x", true), (Mode::Content, "x".to_string()));
    }

    #[test]
    fn parse_bare_gt_is_empty_content() {
        assert_eq!(parse_query(">", false), (Mode::Content, String::new()));
    }
}
```

- [ ] **Step 2: Write the failing integration test** (`tests/engine_test.rs`)

```rust
use fsearch::config::Config;
use fsearch::engine::{Engine, Mode};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn make_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("docs")).unwrap();
    std::fs::create_dir_all(p.join("node_modules")).unwrap();
    std::fs::write(p.join("docs/meeting-notes.md"), "agenda\nfind the needle here\n").unwrap();
    std::fs::write(p.join("docs/todo.txt"), "buy milk\n").unwrap();
    std::fs::write(p.join("node_modules/junk.js"), "needle\n").unwrap();
    dir
}

fn config_for(root: &std::path::Path) -> Config {
    Config {
        roots: vec![root.to_path_buf()],
        excludes: vec!["node_modules".to_string()],
        max_content_filesize: 1024 * 1024,
    }
}

fn wait_until(engine: &mut Engine, deadline: Duration, pred: impl Fn(&Engine) -> bool) {
    let start = Instant::now();
    while start.elapsed() < deadline {
        engine.tick();
        if pred(engine) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("condition not met within {deadline:?}");
}

#[test]
fn end_to_end_filename_and_content_search() {
    let tree = make_tree();
    let cache_dir = tempfile::tempdir().unwrap();
    let cache = cache_dir.path().join("index.bin");
    let mut engine = Engine::new(config_for(tree.path()), cache.clone());

    // index builds in the background (no cache on first run)
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().indexed == 2
    });

    // fuzzy filename search
    engine.set_query("notes", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().iter().any(|r| r.path.ends_with("meeting-notes.md"))
    });
    assert_eq!(engine.mode(), Mode::Fuzzy);

    // regex filename search
    engine.set_query(r"todo\.txt$", true);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 1 && e.results()[0].path.ends_with("todo.txt")
    });

    // content search (debounced), excluded dirs stay excluded
    engine.set_query("> needle", false);
    wait_until(&mut engine, Duration::from_secs(5), |e| !e.results().is_empty());
    assert_eq!(engine.mode(), Mode::Content);
    let rows = engine.results();
    assert!(rows.iter().all(|r| !r.path.contains("node_modules")));
    let hit = rows.iter().find(|r| r.path.ends_with("meeting-notes.md")).unwrap();
    assert_eq!(hit.line_number, Some(2));
    assert!(hit.line.as_deref().unwrap().contains("needle"));

    // invalid regex → error surfaced, no crash
    engine.set_query("[bad", true);
    wait_until(&mut engine, Duration::from_secs(2), |e| e.status().error.is_some());

    // cache was written; a second engine loads it instantly
    wait_until(&mut engine, Duration::from_secs(5), |_| cache.exists());
    let mut engine2 = Engine::new(config_for(tree.path()), cache);
    wait_until(&mut engine2, Duration::from_secs(2), |e| e.status().indexed == 2);
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test`
Expected: compile error — `engine` module missing.

- [ ] **Step 4: Implement `src/engine.rs`**

Threading model: one indexer thread per `Engine::new` (load cache → publish → walk → publish fresh → save); one long-lived search worker receiving the latest filename query; one short-lived thread per content search holding a cancel flag. All workers report back on a single `mpsc` channel drained by `tick()`.

```rust
use crate::content::{self, ContentMatch};
use crate::matcher::{self, FilenameMode};
use crate::{config::Config, index, walker};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub const FILENAME_LIMIT: usize = 500;
pub const CONTENT_LIMIT: usize = 1000;
pub const CONTENT_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Fuzzy,
    Regex,
    Content,
}

pub fn parse_query(input: &str, regex_mode: bool) -> (Mode, String) {
    if let Some(rest) = input.strip_prefix('>') {
        (Mode::Content, rest.trim_start().to_string())
    } else if regex_mode {
        (Mode::Regex, input.to_string())
    } else {
        (Mode::Fuzzy, input.to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultRow {
    pub path: String,
    pub line_number: Option<u64>,
    pub line: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub indexed: usize,
    pub indexing: bool,
    pub matches: usize,
    pub error: Option<String>,
}

enum Msg {
    IndexSnapshot { paths: Arc<Vec<String>>, indexing: bool },
    FilenameResults { generation: u64, indices: Vec<usize>, error: Option<String> },
    ContentHit { generation: u64, hit: ContentMatch },
}

struct FilenameJob {
    generation: u64,
    query: String,
    mode: FilenameMode,
    paths: Arc<Vec<String>>,
}

pub struct Engine {
    msg_rx: Receiver<Msg>,
    msg_tx: Sender<Msg>,
    job_tx: Sender<FilenameJob>,
    paths: Arc<Vec<String>>,
    results: Vec<ResultRow>,
    status: EngineStatus,
    mode: Mode,
    generation: u64,
    query: String,
    max_content_filesize: u64,
    pending_content: Option<(String, Instant)>,
    content_cancel: Option<Arc<AtomicBool>>,
}

impl Engine {
    pub fn new(config: Config, cache_path: PathBuf) -> Engine {
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let (job_tx, job_rx) = mpsc::channel::<FilenameJob>();

        // filename search worker: always process only the newest job
        let worker_tx = msg_tx.clone();
        std::thread::spawn(move || {
            while let Ok(mut job) = job_rx.recv() {
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                let (indices, error) =
                    match matcher::search(&job.paths, &job.query, job.mode, FILENAME_LIMIT) {
                        Ok(ix) => (ix, None),
                        Err(e) => (Vec::new(), Some(format!("invalid pattern: {e}"))),
                    };
                if worker_tx
                    .send(Msg::FilenameResults { generation: job.generation, indices, error })
                    .is_err()
                {
                    return;
                }
            }
        });

        // indexer: cached paths first, then a fresh walk, then save
        let indexer_tx = msg_tx.clone();
        let max_content_filesize = config.max_content_filesize;
        std::thread::spawn(move || {
            if let Some(cached) = index::load(&cache_path) {
                let _ = indexer_tx
                    .send(Msg::IndexSnapshot { paths: Arc::new(cached), indexing: true });
            }
            let Ok(excludes) = walker::build_exclude_set(&config.excludes) else {
                let _ = indexer_tx
                    .send(Msg::IndexSnapshot { paths: Arc::new(Vec::new()), indexing: false });
                return;
            };
            let (path_tx, path_rx) = mpsc::channel::<String>();
            let roots = config.roots.clone();
            let walk_thread = std::thread::spawn(move || walker::walk(&roots, &excludes, &path_tx));
            let mut fresh: Vec<String> = Vec::new();
            let mut last_publish = Instant::now();
            for path in path_rx {
                fresh.push(path);
                // stream early results on a cold start so the UI isn't empty
                if fresh.len() % 8192 == 0 && last_publish.elapsed() > Duration::from_millis(250) {
                    last_publish = Instant::now();
                    let _ = indexer_tx.send(Msg::IndexSnapshot {
                        paths: Arc::new(fresh.clone()),
                        indexing: true,
                    });
                }
            }
            let _ = walk_thread.join();
            fresh.sort_unstable();
            let paths = Arc::new(fresh);
            let _ = indexer_tx.send(Msg::IndexSnapshot { paths: paths.clone(), indexing: false });
            let _ = index::save(&paths, &cache_path);
        });

        Engine {
            msg_rx,
            msg_tx,
            job_tx,
            paths: Arc::new(Vec::new()),
            results: Vec::new(),
            status: EngineStatus { indexing: true, ..Default::default() },
            mode: Mode::Fuzzy,
            generation: 0,
            query: String::new(),
            max_content_filesize,
            pending_content: None,
            content_cancel: None,
        }
    }

    pub fn set_query(&mut self, input: &str, regex_mode: bool) {
        let (mode, query) = parse_query(input, regex_mode);
        self.generation += 1;
        self.mode = mode;
        self.query = query.clone();
        self.status.error = None;
        self.cancel_content();
        match mode {
            Mode::Content => {
                self.results.clear();
                self.status.matches = 0;
                if !query.is_empty() {
                    self.pending_content = Some((query, Instant::now()));
                }
            }
            Mode::Fuzzy | Mode::Regex => {
                self.pending_content = None;
                self.dispatch_filename();
            }
        }
    }

    pub fn tick(&mut self) {
        self.fire_due_content_search();
        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                Msg::IndexSnapshot { paths, indexing } => {
                    self.paths = paths;
                    self.status.indexed = self.paths.len();
                    self.status.indexing = indexing;
                    if matches!(self.mode, Mode::Fuzzy | Mode::Regex) {
                        self.generation += 1;
                        self.dispatch_filename();
                    }
                }
                Msg::FilenameResults { generation, indices, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.results = indices
                        .into_iter()
                        .filter_map(|i| self.paths.get(i))
                        .map(|p| ResultRow { path: p.clone(), line_number: None, line: None })
                        .collect();
                    self.status.matches = self.results.len();
                    self.status.error = error;
                }
                Msg::ContentHit { generation, hit } => {
                    if generation != self.generation || self.results.len() >= CONTENT_LIMIT {
                        continue;
                    }
                    self.results.push(ResultRow {
                        path: hit.path,
                        line_number: Some(hit.line_number),
                        line: Some(hit.line),
                    });
                    self.status.matches = self.results.len();
                    if self.results.len() >= CONTENT_LIMIT {
                        self.cancel_content();
                    }
                }
            }
        }
    }

    pub fn results(&self) -> &[ResultRow] {
        &self.results
    }

    pub fn status(&self) -> EngineStatus {
        self.status.clone()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    fn dispatch_filename(&self) {
        let mode = match self.mode {
            Mode::Regex => FilenameMode::Regex,
            _ => FilenameMode::Fuzzy,
        };
        let _ = self.job_tx.send(FilenameJob {
            generation: self.generation,
            query: self.query.clone(),
            mode,
            paths: self.paths.clone(),
        });
    }

    fn fire_due_content_search(&mut self) {
        let due = self
            .pending_content
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= CONTENT_DEBOUNCE);
        if !due {
            return;
        }
        let (pattern, _) = self.pending_content.take().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        self.content_cancel = Some(cancel.clone());
        let paths = self.paths.clone();
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let max = self.max_content_filesize;
        std::thread::spawn(move || {
            let (hit_tx, hit_rx) = mpsc::channel::<ContentMatch>();
            let search_cancel = cancel.clone();
            let search_paths = paths.clone();
            let pattern2 = pattern.clone();
            let searcher = std::thread::spawn(move || {
                content::search(&search_paths, &pattern2, max, &search_cancel, &hit_tx)
            });
            for hit in hit_rx {
                if tx.send(Msg::ContentHit { generation, hit }).is_err() {
                    cancel.store(true, Ordering::Relaxed);
                    break;
                }
            }
            if let Ok(Err(e)) = searcher.join() {
                let _ = tx.send(Msg::FilenameResults {
                    generation,
                    indices: Vec::new(),
                    error: Some(format!("invalid pattern: {e}")),
                });
            }
        });
    }

    fn cancel_content(&mut self) {
        if let Some(flag) = self.content_cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}
```

(Design note: an invalid content pattern reuses the `FilenameResults` message with empty indices purely to carry the error string; generation matching makes this safe.)

- [ ] **Step 5: Run tests**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: unit + integration all pass. If the integration test is flaky on the `indexed == 2` wait, the walker/indexer has a bug — fix it, don't loosen the test.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/main.rs src/engine.rs tests/engine_test.rs
git commit -m "feat: search engine with background index refresh"
```

---

### Task 8: Actions

**Files:**
- Create: `src/actions.rs`
- Modify: `src/lib.rs` (add `pub mod actions;`)

**Interfaces:**
- Produces:
  - `pub fn open_args(path: &str) -> (&'static str, Vec<String>)` → `("open", [path])`
  - `pub fn reveal_args(path: &str) -> (&'static str, Vec<String>)` → `("open", ["-R", path])`
  - `pub fn open(path: &str) -> std::io::Result<()>`
  - `pub fn reveal(path: &str) -> std::io::Result<()>`
  - `pub fn copy(path: &str) -> std::io::Result<()>` — pipes the path to `pbcopy`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_uses_macos_open() {
        assert_eq!(open_args("/a b.txt"), ("open", vec!["/a b.txt".to_string()]));
    }

    #[test]
    fn reveal_uses_open_dash_r() {
        assert_eq!(
            reveal_args("/a.txt"),
            ("open", vec!["-R".to_string(), "/a.txt".to_string()])
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test actions`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
use std::io::Write;
use std::process::{Command, Stdio};

pub fn open_args(path: &str) -> (&'static str, Vec<String>) {
    ("open", vec![path.to_string()])
}

pub fn reveal_args(path: &str) -> (&'static str, Vec<String>) {
    ("open", vec!["-R".to_string(), path.to_string()])
}

fn run(args: (&'static str, Vec<String>)) -> std::io::Result<()> {
    Command::new(args.0)
        .args(&args.1)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

pub fn open(path: &str) -> std::io::Result<()> {
    run(open_args(path))
}

pub fn reveal(path: &str) -> std::io::Result<()> {
    run(reveal_args(path))
}

pub fn copy(path: &str) -> std::io::Result<()> {
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn()?;
    child
        .stdin
        .as_mut()
        .expect("pbcopy stdin is piped")
        .write_all(path.as_bytes())?;
    child.wait().map(|_| ())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test actions && cargo clippy --all-targets -- -D warnings`
Expected: 2 pass, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/actions.rs src/lib.rs
git commit -m "feat: open, reveal and copy actions"
```

---

### Task 9: TUI and wiring

**Files:**
- Create: `src/tui.rs`
- Modify: `src/lib.rs` (add `pub mod tui;`), `src/main.rs` (real entry point)

**Interfaces:**
- Consumes: `engine::{Engine, Mode, ResultRow, EngineStatus}`, `actions::{open, reveal, copy}`, `config::{load_or_create, default_config_path}`, `index::default_cache_path`.
- Produces:
  - `pub struct App { … }` with `pub fn new(engine: Engine) -> App` and `pub fn handle_key(&mut self, key: KeyEvent) -> bool` (returns `false` to quit)
  - `pub fn draw(frame: &mut Frame, app: &mut App)` — pure render, testable with `TestBackend`
  - `pub fn run(engine: Engine) -> anyhow::Result<()>` — event loop

- [ ] **Step 1: Write failing render/key tests** (in `src/tui.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::engine::Engine;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    fn test_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            roots: vec![dir.path().to_path_buf()],
            excludes: vec![],
            max_content_filesize: 1024,
        };
        let engine = Engine::new(config, dir.path().join("index.bin"));
        // keep the tempdir alive for the test's duration by leaking it (test-only)
        std::mem::forget(dir);
        App::new(engine)
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn renders_input_and_status() {
        let mut app = test_app();
        app.input = "notes".to_string();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("notes"));
        assert!(text.contains("fuzzy"));
    }

    #[test]
    fn typing_updates_input_and_esc_quits() {
        let mut app = test_app();
        assert!(app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert_eq!(app.input, "a");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
        assert_eq!(app.input, "");
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_r_toggles_regex_mode() {
        let mut app = test_app();
        assert!(!app.regex_mode);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.regex_mode);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test tui`
Expected: compile error.

- [ ] **Step 3: Implement `src/tui.rs`**

```rust
use crate::actions;
use crate::engine::{Engine, Mode};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use std::time::Duration;

const PREVIEW_BYTES: usize = 64 * 1024;

pub struct App {
    pub engine: Engine,
    pub input: String,
    pub selected: usize,
    pub regex_mode: bool,
    pub show_preview: bool,
    pub message: Option<String>,
    preview_for: Option<(String, Option<u64>)>,
    preview_lines: Vec<String>,
}

impl App {
    pub fn new(engine: Engine) -> App {
        App {
            engine,
            input: String::new(),
            selected: 0,
            regex_mode: false,
            show_preview: true,
            message: None,
            preview_for: None,
            preview_lines: Vec::new(),
        }
    }

    /// Returns false when the app should quit.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.message = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => return false,
            (KeyCode::Char('r'), true) => {
                self.regex_mode = !self.regex_mode;
                self.refresh_query();
            }
            (KeyCode::Char('u'), true) => {
                self.input.clear();
                self.refresh_query();
            }
            (KeyCode::Char('j'), true) | (KeyCode::Down, _) => self.move_selection(1),
            (KeyCode::Char('k'), true) | (KeyCode::Up, _) => self.move_selection(-1),
            (KeyCode::Char('y'), true) => self.act(actions::copy, "copied"),
            (KeyCode::Char('f'), true) => self.act(actions::reveal, "revealed"),
            (KeyCode::Tab, _) => self.show_preview = !self.show_preview,
            (KeyCode::Enter, _) => self.act(actions::open, "opened"),
            (KeyCode::Backspace, _) => {
                self.input.pop();
                self.refresh_query();
            }
            (KeyCode::Char(c), false) => {
                self.input.push(c);
                self.refresh_query();
            }
            _ => {}
        }
        true
    }

    fn refresh_query(&mut self) {
        self.selected = 0;
        self.engine.set_query(&self.input, self.regex_mode);
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.engine.results().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected =
            (self.selected as isize + delta).rem_euclid(len as isize) as usize;
    }

    fn act(&mut self, f: impl Fn(&str) -> std::io::Result<()>, verb: &str) {
        let Some(row) = self.engine.results().get(self.selected) else {
            return;
        };
        self.message = Some(match f(&row.path) {
            Ok(()) => format!("{verb}: {}", row.path),
            Err(e) => format!("error: {e}"),
        });
    }

    fn load_preview(&mut self) {
        let Some(row) = self.engine.results().get(self.selected) else {
            self.preview_for = None;
            self.preview_lines = vec!["no selection".to_string()];
            return;
        };
        let key = (row.path.clone(), row.line_number);
        if self.preview_for.as_ref() == Some(&key) {
            return;
        }
        self.preview_for = Some(key);
        self.preview_lines = match std::fs::read(&row.path) {
            Ok(bytes) if bytes.contains(&0) => vec!["(binary file)".to_string()],
            Ok(mut bytes) => {
                bytes.truncate(PREVIEW_BYTES);
                let text = String::from_utf8_lossy(&bytes);
                let all: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                match row.line_number {
                    // center the preview on the matching line
                    Some(n) => {
                        let start = (n as usize).saturating_sub(6);
                        all.into_iter()
                            .enumerate()
                            .skip(start)
                            .take(40)
                            .map(|(i, l)| format!("{:>5} {l}", i + 1))
                            .collect()
                    }
                    None => all.into_iter().take(100).collect(),
                }
            }
            Err(e) => vec![format!("(unreadable: {e})")],
        };
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_input(frame, app, outer[0]);

    if app.show_preview {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(outer[1]);
        draw_results(frame, app, cols[0]);
        draw_preview(frame, app, cols[1]);
    } else {
        draw_results(frame, app, outer[1]);
    }

    draw_status(frame, app, outer[2]);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let mode = match (app.engine.mode(), app.regex_mode) {
        (Mode::Content, _) => "content",
        (_, true) => "regex",
        _ => "fuzzy",
    };
    let input = Paragraph::new(app.input.as_str())
        .block(Block::default().borders(Borders::ALL).title(format!("fsearch [{mode}]")));
    frame.render_widget(input, area);
    frame.set_cursor_position((area.x + 1 + app.input.len() as u16, area.y + 1));
}

fn draw_results(frame: &mut Frame, app: &App, area: Rect) {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let items: Vec<ListItem> = app
        .engine
        .results()
        .iter()
        .map(|r| {
            let shown = match &home {
                Some(h) if r.path.starts_with(h) => format!("~{}", &r.path[h.len()..]),
                _ => r.path.clone(),
            };
            match (r.line_number, &r.line) {
                (Some(n), Some(line)) => ListItem::new(Line::from(vec![
                    Span::styled(format!("{shown}:{n} "), Style::default().fg(Color::Cyan)),
                    Span::raw(line.clone()),
                ])),
                _ => ListItem::new(shown),
            }
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("results"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    app.load_preview();
    let text: Vec<Line> = app.preview_lines.iter().map(|l| Line::from(l.as_str())).collect();
    let preview =
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("preview"));
    frame.render_widget(preview, area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let s = app.engine.status();
    let mut parts = vec![
        format!("{} files", s.indexed),
        format!("{} matches", s.matches),
    ];
    if s.indexing {
        parts.push("indexing…".to_string());
    }
    if let Some(e) = &s.error {
        parts.push(e.clone());
    }
    if let Some(m) = &app.message {
        parts.push(m.clone());
    }
    let status = Paragraph::new(parts.join(" · ")).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(status, area);
}

pub fn run(engine: Engine) -> anyhow::Result<()> {
    let mut terminal = ratatui::init(); // installs a terminal-restoring panic hook
    let mut app = App::new(engine);
    let result = loop {
        app.engine.tick();
        let len = app.engine.results().len();
        if app.selected >= len && len > 0 {
            app.selected = len - 1;
        }
        if let Err(e) = terminal.draw(|f| draw(f, &mut app)) {
            break Err(e.into());
        }
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => {
                if let Ok(Event::Key(key)) = event::read() {
                    if key.is_press() && !app.handle_key(key) {
                        break Ok(());
                    }
                }
            }
            Ok(false) => {}
            Err(e) => break Err(e.into()),
        }
    };
    ratatui::restore();
    result
}
```

(If `key.is_press()` doesn't exist in this crossterm version, use `key.kind == KeyEventKind::Press`.)

`src/main.rs`:

```rust
use fsearch::{config, engine::Engine, index, tui};

fn main() {
    let config = match config::load_or_create(&config::default_config_path()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fsearch: {e:#}");
            std::process::exit(1);
        }
    };
    let engine = Engine::new(config, index::default_cache_path());
    if let Err(e) = tui::run(engine) {
        eprintln!("fsearch: {e:#}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 4: Run tests + manual smoke test**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass.

Manual smoke (run in a real terminal): `cargo run --release` — verify: instant launch, typing filters, `Ctrl-R` switches mode in the title, `> some-word` streams content hits, `Tab` hides preview, `Esc` exits with terminal restored.

- [ ] **Step 5: Commit**

```bash
git add src/tui.rs src/lib.rs src/main.rs
git commit -m "feat: terminal ui"
```

---

### Task 10: Perf sanity + README

**Files:**
- Create: `tests/perf_test.rs`, `README.md`

**Interfaces:**
- Consumes: `matcher::search`, `FilenameMode`.

- [ ] **Step 1: Write the perf test** (`tests/perf_test.rs`)

```rust
use fsearch::matcher::{search, FilenameMode};
use std::time::Instant;

/// Manual perf check: `cargo test --release --test perf_test -- --ignored --nocapture`
#[test]
#[ignore]
fn million_path_search_under_100ms() {
    let dirs = ["src", "docs", "Library", "projects", "Downloads", "notes"];
    let paths: Vec<String> = (0..1_000_000)
        .map(|i| format!("/Users/josh/{}/sub{}/file-{i}.txt", dirs[i % 6], i % 997))
        .collect();

    for (query, mode) in [
        ("filetxt", FilenameMode::Fuzzy),
        (r"file-\d{3}\.txt$", FilenameMode::Regex),
    ] {
        let start = Instant::now();
        let hits = search(&paths, query, mode, 500).unwrap();
        let elapsed = start.elapsed();
        println!("{mode:?} {query:?}: {elapsed:?} ({} hits)", hits.len());
        assert!(!hits.is_empty());
        assert!(elapsed.as_millis() < 100, "{mode:?} took {elapsed:?}");
    }
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --release --test perf_test -- --ignored --nocapture`
Expected: passes with printed timings under 100 ms. If it fails, profile before loosening anything (chunk size, sort strategy).

- [ ] **Step 3: Commit**

```bash
git add tests/perf_test.rs
git commit -m "add: perf sanity test"
```

- [ ] **Step 4: Write README.md**

Sections (write real content, not placeholders):
- **fsearch** — one-paragraph pitch: Alfred-style instant file search for the macOS terminal; fuzzy or regex over filenames, regex over contents.
- **Install** — `cargo install --path .` (and `cargo build --release` → `target/release/fsearch`).
- **Usage** — launch `fsearch`; type to fuzzy-search; `Ctrl-R` for regex mode; `> pattern` to search inside files.
- **Keys** — table of all bindings from the spec (navigate, open, reveal, copy, preview toggle, quit).
- **Configuration** — path `~/.config/fsearch/config.toml`, the three fields with defaults and an example snippet.
- **How it works** — three sentences: persisted index at `~/.cache/fsearch/index.bin`, background re-walk on each launch, on-demand parallel content grep.
- **License** — MIT.

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs: readme"
```

---

## Self-Review Notes

- Spec coverage: config (T2), walker (T3), index+cache lifecycle (T4, T7), fuzzy/regex matching (T5), content grep with size/binary limits and debounce (T6, T7), TUI layout/keys/preview/status (T9), actions (T8), error handling (T2 invalid config exit in `main`, T5/T7 invalid regex status, T3 skip counting, T4 corrupt cache → `None` → rebuild), panic-safe terminal (ratatui::init hook, T9), perf target test (T10), README+license (T1, T10). First-run streaming covered by the indexer's periodic `IndexSnapshot` publishes (T7).
- Types cross-checked: `walk(&[PathBuf], &GlobSet, &Sender<String>)` matches T7's usage; `matcher::search(&[String], …) -> Result<Vec<usize>, String>` (Arc deref-coerces at the call site); `content::search` signature matches the engine's spawn; `ResultRow`/`EngineStatus` fields match the TUI's reads.
- Known deliberate simplifications (in spec's non-goals or within targets): no FSEvents; content preview centers on the match rather than highlighting the span; cursor position assumes ASCII input width.
