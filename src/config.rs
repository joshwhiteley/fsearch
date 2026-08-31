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
    /// Blend bare filename searches with semantic results when available.
    pub unified: bool,
    /// User-defined commands shown in the actions menu.
    pub actions: Vec<CustomAction>,
    /// One warning for any malformed `[[actions]]` entries that were skipped.
    pub action_warning: Option<String>,
}

/// A command configured in an `[[actions]]` table.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomAction {
    pub name: String,
    pub cmd: Vec<String>,
    pub ext: Vec<String>,
    pub kind: Option<String>,
    pub enter: bool,
}

impl CustomAction {
    /// Whether this action applies to a file path. Directories are represented
    /// by a trailing slash in the index and never match custom actions.
    pub fn matches(&self, path: &str) -> bool {
        if path.ends_with('/') {
            return false;
        }
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if !self.ext.is_empty()
            && !ext
                .as_deref()
                .is_some_and(|e| self.ext.iter().any(|want| want.eq_ignore_ascii_case(e)))
        {
            return false;
        }
        if let Some(kind) = &self.kind {
            if !ext
                .as_deref()
                .and_then(crate::filters::kind_for_ext)
                .is_some_and(|actual| actual.eq_ignore_ascii_case(kind))
            {
                return false;
            }
        }
        true
    }
}

/// Built-in actions offered when the user has not defined any `[[actions]]`:
/// "open in <editor>" menu entries for GUI code editors that are actually
/// installed. Menu-only (`enter = false`) so the default opener is untouched.
pub fn default_actions() -> Vec<CustomAction> {
    #[cfg(target_os = "macos")]
    {
        editor_actions_in(std::path::Path::new("/Applications"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        editor_actions_on_path(std::env::var_os("PATH").as_deref())
    }
}

/// macOS: one action per installed editor bundle under `apps`.
fn editor_actions_in(apps: &std::path::Path) -> Vec<CustomAction> {
    const EDITORS: &[(&str, &str, &str)] = &[
        ("Cursor.app", "Cursor", "open in cursor"),
        (
            "Visual Studio Code.app",
            "Visual Studio Code",
            "open in vs code",
        ),
        ("Zed.app", "Zed", "open in zed"),
        ("Sublime Text.app", "Sublime Text", "open in sublime text"),
    ];
    EDITORS
        .iter()
        .filter(|(bundle, _, _)| apps.join(bundle).exists())
        .map(|(_, app, name)| CustomAction {
            name: (*name).to_string(),
            cmd: vec![
                "open".into(),
                "-a".into(),
                (*app).to_string(),
                "{path}".into(),
            ],
            ext: Vec::new(),
            kind: Some("code".into()),
            enter: false,
        })
        .collect()
}

/// Elsewhere: one action per editor binary found on PATH.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn editor_actions_on_path(path: Option<&std::ffi::OsStr>) -> Vec<CustomAction> {
    const EDITORS: &[(&str, &str)] = &[
        ("cursor", "open in cursor"),
        ("code", "open in vs code"),
        ("zed", "open in zed"),
        ("subl", "open in sublime text"),
    ];
    let Some(path) = path else {
        return Vec::new();
    };
    let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    EDITORS
        .iter()
        .filter(|(bin, _)| dirs.iter().any(|d| d.join(bin).is_file()))
        .map(|(bin, name)| CustomAction {
            name: (*name).to_string(),
            cmd: vec![(*bin).to_string(), "{path}".into()],
            ext: Vec::new(),
            kind: Some("code".into()),
            enter: false,
        })
        .collect()
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
            unified: true,
            actions: Vec::new(),
            action_warning: None,
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
    unified: Option<bool>,
    actions: Option<Vec<toml::Value>>,
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
# [theme] preset: default, catppuccin, gruvbox, nord, slate, tokyonight, forge
#         accent: optional hex override, e.g. \"#7aa2f7\"
#         borders: \"sharp\" (default), \"rounded\", or \"none\"
#         selection_bg / match_fg / section: optional hex overrides,
#           e.g. selection_bg = \"#313244\"
# [keys] remaps commands, e.g. quit = \"ctrl-q\", move_up = [\"up\", \"ctrl-k\"]
#   help = [\"f1\", \"ctrl-o\"], toggle_mark = \"ctrl-s\", clear_marks = \"alt-s\"
#   theme_cycle = \"ctrl-g\"
#   (text editing keys - typing, backspace, cursor, ctrl-a/e/w/d - are fixed)
# index_apps: include /Applications app bundles in the index (macOS)
# icons: nerd-font glyphs before filenames (true/false; needs a nerd font)
# unified: blend bare filename and semantic results (true/false)
# quiet: substrings demoted in ranking (app internals); set [] to disable
# mouse: click to select, double-click to open, wheel scrolls (true/false)
# remember_session: restore preview layout and row density between runs
#   (true/false)
# Custom actions run argv directly (never through a shell). Paths support
# {path}, {paths} (one argv element spliced per marked file), and {dir}.
# Until you define any [[actions]], fsearch offers built-in defaults:
# \"open in <editor>\" menu entries for installed GUI code editors
# (Cursor, VS Code, Zed, Sublime Text). Defining your own replaces them.
#
# [[actions]]
# name = \"open in cursor\"
# cmd = [\"cursor\", \"{path}\"]
# kind = \"code\"
#
# [[actions]]
# name = \"open in Preview\"
# cmd = [\"open\", \"-a\", \"Preview\", \"{path}\"]
# ext = [\"pdf\"]
#
# [[actions]]
# name = \"open in Word\"
# cmd = [\"open\", \"-a\", \"Microsoft Word\", \"{path}\"]
# ext = [\"docx\"]
# enter = true replaces the default opener for matching files.
# Malformed actions are skipped; only add commands you trust.
";

fn parse_actions(raw: Option<Vec<toml::Value>>) -> (Vec<CustomAction>, usize) {
    let mut skipped = 0;
    let actions = raw
        .unwrap_or_default()
        .into_iter()
        .filter_map(|raw| {
            let Some(table) = raw.as_table() else {
                skipped += 1;
                return None;
            };
            let name = table
                .get("name")
                .and_then(toml::Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(str::trim)
                .map(str::to_string);
            let cmd = table
                .get("cmd")
                .and_then(toml::Value::as_array)
                .and_then(|cmd| {
                    let cmd: Option<Vec<String>> = cmd
                        .iter()
                        .map(toml::Value::as_str)
                        .map(|arg| arg.map(str::to_string))
                        .collect();
                    cmd.filter(|cmd| {
                        !cmd.is_empty()
                            && cmd
                                .first()
                                .is_some_and(|program| !program.trim().is_empty())
                    })
                });
            let ext = match table.get("ext") {
                Some(ext) => ext.as_array().and_then(|ext| {
                    ext.iter()
                        .map(toml::Value::as_str)
                        .map(|value| value.map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                }),
                None => Some(Vec::new()),
            };
            let kind = table
                .get("kind")
                .and_then(toml::Value::as_str)
                .map(|kind| kind.trim().to_ascii_lowercase());
            let enter = match table.get("enter") {
                Some(enter) => enter.as_bool(),
                None => Some(false),
            };
            let valid_kind = match table.get("kind") {
                None => true,
                Some(_) => kind.as_deref().is_some_and(crate::filters::is_known_kind),
            };
            if name.is_none() || cmd.is_none() || ext.is_none() || enter.is_none() || !valid_kind {
                skipped += 1;
                return None;
            }
            let ext = ext
                .unwrap()
                .into_iter()
                .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|ext| !ext.is_empty())
                .collect();
            Some(CustomAction {
                name: name.unwrap(),
                cmd: cmd.unwrap(),
                ext,
                kind,
                enter: enter.unwrap(),
            })
        })
        .collect();
    (actions, skipped)
}

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
    let (actions, skipped_actions) = parse_actions(raw.actions);
    let action_warning = (skipped_actions > 0).then(|| {
        format!(
            "warning: skipped {skipped_actions} malformed custom action{}",
            if skipped_actions == 1 { "" } else { "s" }
        )
    });
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
        unified: raw.unified.unwrap_or(true),
        actions,
        action_warning,
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
        let generated = std::fs::read_to_string(&path).unwrap();
        assert!(generated.contains("# cmd = [\"open\", \"-a\", \"Preview\", \"{path}\"]"));
        assert!(generated.contains("# cmd = [\"open\", \"-a\", \"Microsoft Word\", \"{path}\"]"));
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
    fn default_actions_detect_installed_editors_only() {
        let apps = tempfile::tempdir().unwrap();
        assert!(editor_actions_in(apps.path()).is_empty());
        std::fs::create_dir(apps.path().join("Cursor.app")).unwrap();
        std::fs::create_dir(apps.path().join("Zed.app")).unwrap();
        let actions = editor_actions_in(apps.path());
        let names: Vec<&str> = actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["open in cursor", "open in zed"]);
        // menu-only entries scoped to code files, argv through `open -a`
        assert!(actions.iter().all(|a| !a.enter));
        assert!(actions.iter().all(|a| a.kind.as_deref() == Some("code")));
        assert_eq!(actions[0].cmd, ["open", "-a", "Cursor", "{path}"]);

        let bins = tempfile::tempdir().unwrap();
        assert!(editor_actions_on_path(Some(bins.path().as_os_str())).is_empty());
        std::fs::write(bins.path().join("code"), "").unwrap();
        let actions = editor_actions_on_path(Some(bins.path().as_os_str()));
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name, "open in vs code");
        assert_eq!(actions[0].cmd, ["code", "{path}"]);
        assert!(editor_actions_on_path(None).is_empty());
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
    fn unified_flag_parses_and_defaults_to_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "unified = false\n").unwrap();
        assert!(!load_or_create(&path).unwrap().unified);
        assert!(Config::default().unified);
        let path2 = dir.path().join("plain.toml");
        std::fs::write(&path2, "roots = [\"/tmp\"]\n").unwrap();
        assert!(load_or_create(&path2).unwrap().unified);
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/Documents"), home.join("Documents"));
        assert_eq!(expand_tilde("/etc"), PathBuf::from("/etc"));
    }

    #[test]
    fn custom_actions_parse_and_normalize_filters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[actions]]\nname = \"code\"\ncmd = [\"cursor\", \"{path}\"]\n\
             ext = [\"RS\", \".Py\"]\nkind = \"CODE\"\nenter = true\n",
        )
        .unwrap();
        let c = load_or_create(&path).unwrap();
        assert_eq!(c.actions.len(), 1);
        assert_eq!(c.actions[0].name, "code");
        assert_eq!(c.actions[0].cmd, vec!["cursor", "{path}"]);
        assert_eq!(c.actions[0].ext, vec!["rs", "py"]);
        assert_eq!(c.actions[0].kind.as_deref(), Some("code"));
        assert!(c.actions[0].enter);
        assert!(c.actions[0].matches("/tmp/main.rs"));
        assert!(!c.actions[0].matches("/tmp/main.txt"));
    }

    #[test]
    fn malformed_custom_actions_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[actions]]\nname = \"\"\ncmd = [\"open\"]\n\n\
             [[actions]]\nname = \"bad command\"\ncmd = []\n\n\
             [[actions]]\nname = \"bad kind\"\ncmd = [\"open\"]\nkind = \"spreadsheet\"\n\n\
             [[actions]]\nname = \"valid\"\ncmd = [\"open\"]\n",
        )
        .unwrap();
        let c = load_or_create(&path).unwrap();
        assert_eq!(
            c.actions
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            ["valid"]
        );
        assert_eq!(
            c.action_warning.as_deref(),
            Some("warning: skipped 3 malformed custom actions")
        );
    }

    #[test]
    fn wrong_typed_custom_actions_do_not_reject_valid_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[actions]]\nname = \"wrong command\"\ncmd = \"open\"\n\n\
             [[actions]]\nname = \"valid\"\ncmd = [\"open\"]\n",
        )
        .unwrap();
        let c = load_or_create(&path).unwrap();
        assert_eq!(c.actions.len(), 1);
        assert_eq!(c.actions[0].name, "valid");
        assert_eq!(
            c.action_warning.as_deref(),
            Some("warning: skipped 1 malformed custom action")
        );
    }

    #[test]
    fn action_kind_vocabulary_matches_query_kinds() {
        for kind in ["image", "video", "audio", "doc", "code", "app", "archive"] {
            assert!(crate::filters::is_known_kind(kind));
        }
        assert!(!crate::filters::is_known_kind("spreadsheet"));
    }
}
