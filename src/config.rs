use anyhow::Context;
use std::collections::HashMap;
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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThemeConfig {
    pub preset: String,
    pub accent: Option<String>,
    /// "sharp" (default) | "rounded" | "none"; None keeps the preset.
    pub borders: Option<String>,
    /// Hex overrides; None keeps the preset's choice.
    pub selection_bg: Option<String>,
    pub match_fg: Option<String>,
    pub section: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub roots: Vec<PathBuf>,
    pub excludes: Vec<String>,
    pub max_content_filesize: u64,
    pub theme: ThemeConfig,
    /// Command key overrides: action name -> key specs (see `[keys]`).
    pub keys: HashMap<String, Vec<String>>,
    /// Mouse support: click to select, double-click to open, wheel scrolls.
    pub mouse: bool,
    /// Restore the preview layout and row density from the last run
    /// (state file at ~/.local/state/fsearch/session.toml).
    pub remember_session: bool,
    /// Substring patterns demoted below the fold and off the launch screen.
    pub quiet: Vec<String>,
    /// Include /Applications app bundles in the index (macOS).
    pub index_apps: bool,
    /// Nerd-font glyphs before filenames in result rows.
    pub icons: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: vec![dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))],
            excludes: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            max_content_filesize: 2 * 1024 * 1024,
            theme: ThemeConfig::default(),
            keys: HashMap::new(),
            mouse: true,
            remember_session: true,
            quiet: crate::quiet::DEFAULT_QUIET
                .iter()
                .map(|s| s.to_string())
                .collect(),
            index_apps: true,
            icons: false,
        }
    }
}

#[derive(serde::Deserialize)]
struct RawConfig {
    roots: Option<Vec<String>>,
    excludes: Option<Vec<String>>,
    max_content_filesize: Option<u64>,
    theme: Option<RawTheme>,
    keys: Option<HashMap<String, KeySpec>>,
    mouse: Option<bool>,
    remember_session: Option<bool>,
    quiet: Option<Vec<String>>,
    index_apps: Option<bool>,
    icons: Option<bool>,
}

/// A `[keys]` value: either one spec string or a list of them.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum KeySpec {
    One(String),
    Many(Vec<String>),
}

#[derive(serde::Deserialize)]
struct RawTheme {
    preset: Option<String>,
    accent: Option<String>,
    borders: Option<String>,
    selection_bg: Option<String>,
    match_fg: Option<String>,
    section: Option<String>,
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
# [theme] preset: default, catppuccin, gruvbox, nord, slate, tokyonight
#         accent: optional hex override, e.g. \"#7aa2f7\"
#         borders: \"sharp\" (default), \"rounded\", or \"none\"
#         selection_bg / match_fg / section: optional hex overrides,
#           e.g. selection_bg = \"#313244\"
# [keys] remaps commands, e.g. quit = \"ctrl-q\", move_up = [\"up\", \"ctrl-k\"]
#   help = [\"f1\", \"ctrl-o\"], toggle_mark = \"ctrl-b\", clear_marks = \"alt-b\"
#   theme_cycle = \"ctrl-g\"
#   (text editing keys - typing, backspace, cursor, ctrl-a/e/w/d - are fixed)
# index_apps: include /Applications app bundles in the index (macOS)
# icons: nerd-font glyphs before filenames (true/false; needs a nerd font)
# quiet: substrings demoted in ranking (app internals); set [] to disable
# mouse: click to select, double-click to open, wheel scrolls (true/false)
# remember_session: restore preview layout and row density between runs
#   (true/false)
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
        theme: raw
            .theme
            .map(|t| ThemeConfig {
                preset: t.preset.unwrap_or_default(),
                accent: t.accent,
                borders: t.borders,
                selection_bg: t.selection_bg,
                match_fg: t.match_fg,
                section: t.section,
            })
            .unwrap_or_default(),
        keys: raw
            .keys
            .map(|m| {
                m.into_iter()
                    .map(|(name, spec)| {
                        let specs = match spec {
                            KeySpec::One(s) => vec![s],
                            KeySpec::Many(v) => v,
                        };
                        (name, specs)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        mouse: raw.mouse.unwrap_or(true),
        remember_session: raw.remember_session.unwrap_or(true),
        quiet: raw.quiet.unwrap_or_else(|| d.quiet.clone()),
        index_apps: raw.index_apps.unwrap_or(true),
        icons: raw.icons.unwrap_or(false),
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
    fn theme_section_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[theme]\npreset = \"nord\"\naccent = \"#ff0080\"\n").unwrap();
        let c = load_or_create(&path).unwrap();
        assert_eq!(c.theme.preset, "nord");
        assert_eq!(c.theme.accent.as_deref(), Some("#ff0080"));
        // absent section: defaults
        let d = Config::default();
        assert_eq!(d.theme.preset, "");
    }

    #[test]
    fn theme_section_parses_borders_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[theme]\npreset = \"nord\"\nborders = \"rounded\"\n\
             selection_bg = \"#3b4252\"\nmatch_fg = \"#88c0d0\"\nsection = \"#8fbcbb\"\n",
        )
        .unwrap();
        let c = load_or_create(&path).unwrap();
        assert_eq!(c.theme.preset, "nord");
        assert_eq!(c.theme.borders.as_deref(), Some("rounded"));
        assert_eq!(c.theme.selection_bg.as_deref(), Some("#3b4252"));
        assert_eq!(c.theme.match_fg.as_deref(), Some("#88c0d0"));
        assert_eq!(c.theme.section.as_deref(), Some("#8fbcbb"));
        // a [theme] with only the preset leaves the new keys None
        let dir2 = tempfile::tempdir().unwrap();
        let path2 = dir2.path().join("config.toml");
        std::fs::write(&path2, "[theme]\npreset = \"gruvbox\"\n").unwrap();
        let c2 = load_or_create(&path2).unwrap();
        assert_eq!(c2.theme.preset, "gruvbox");
        assert_eq!(c2.theme.borders, None);
        assert_eq!(c2.theme.selection_bg, None);
        assert_eq!(c2.theme.accent, None);
    }

    #[test]
    fn keys_section_parses_single_and_list_forms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[keys]\nquit = \"ctrl-q\"\nmove_up = [\"up\", \"ctrl-k\"]\n",
        )
        .unwrap();
        let c = load_or_create(&path).unwrap();
        assert_eq!(c.keys.get("quit"), Some(&vec!["ctrl-q".to_string()]));
        assert_eq!(
            c.keys.get("move_up"),
            Some(&vec!["up".to_string(), "ctrl-k".to_string()])
        );
        // absent [keys] section: empty map
        let d = Config::default();
        assert!(d.keys.is_empty());
    }

    #[test]
    fn mouse_flag_parses_and_defaults_to_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "mouse = false\n").unwrap();
        let c = load_or_create(&path).unwrap();
        assert!(!c.mouse);
        // default is on
        assert!(Config::default().mouse);
    }

    #[test]
    fn remember_session_flag_parses_and_defaults_to_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "remember_session = false\n").unwrap();
        let c = load_or_create(&path).unwrap();
        assert!(!c.remember_session);
        // default is on, and a config without the key still parses
        assert!(Config::default().remember_session);
        let path2 = dir.path().join("plain.toml");
        std::fs::write(&path2, "roots = [\"/tmp\"]\n").unwrap();
        assert!(load_or_create(&path2).unwrap().remember_session);
    }

    #[test]
    fn icons_flag_parses_and_defaults_to_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "icons = true\n").unwrap();
        let c = load_or_create(&path).unwrap();
        assert!(c.icons);
        // default is off
        assert!(!Config::default().icons);
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/Documents"), home.join("Documents"));
        assert_eq!(expand_tilde("/etc"), PathBuf::from("/etc"));
    }
}
