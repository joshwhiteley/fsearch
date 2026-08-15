use anyhow::Context;
use std::path::{Path, PathBuf};

pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".bun",
    ".cache",
    ".cargo",
    ".npm",
    ".Trash",
    ".DS_Store",
    ".venv",
    ".venvs",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "__pycache__",
    "Library/Caches",
    "Library/Containers",
    "Library/Logs",
    "Library/Application Support/MobileSync",
    // cloud-synced trees are excluded by default; remove these entries from
    // config.toml to opt in (iCloud Drive = Mobile Documents; Box/Dropbox/
    // Google Drive/OneDrive live under CloudStorage)
    "Library/Mobile Documents",
    "Library/CloudStorage",
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
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".config")
        });
    base.join("fsearch").join("config.toml")
}

const DEFAULT_TEMPLATE_HEADER: &str = "\
# fsearch configuration
# roots: directories to index (~ expands to your home directory)
# excludes: directory or file names/paths never indexed
#   cloud drives are excluded by default; delete \"Library/Mobile Documents\"
#   (iCloud) or \"Library/CloudStorage\" (Box, Dropbox, ...) to index them
# max_content_filesize: content search skips files larger than this (bytes)
";

pub fn load_or_create(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        let d = Config::default();
        let body = format!(
            "{}roots = [\"~\"]\nexcludes = {:?}\nmax_content_filesize = {}\n",
            DEFAULT_TEMPLATE_HEADER, d.excludes, d.max_content_filesize
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
    fn package_caches_and_cloud_drives_excluded_by_default() {
        let c = Config::default();
        for e in [
            ".cargo",
            ".bun",
            ".DS_Store",
            ".venvs",
            "venv",
            "Library/Logs",
            "Library/Mobile Documents",
            "Library/CloudStorage",
        ] {
            assert!(c.excludes.iter().any(|x| x == e), "missing exclude: {e}");
        }
        // the real macOS folder has no space; the old entry never matched
        assert!(!c.excludes.iter().any(|x| x == "Library/Cloud Storage"));
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
