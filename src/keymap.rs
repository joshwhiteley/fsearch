//! Command keybindings, remappable via `[keys]` in config.toml.
//!
//! Text editing keys are never remappable (see [`is_editing_key`]):
//! - plain characters (typing), backspace, left/right cursor movement
//! - readline-style ctrl-a / ctrl-e / ctrl-w / ctrl-d

use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use std::collections::HashMap;

/// A command the UI can run, mapped from one or more keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Open,
    Menu,
    QuickLook,
    CopyPath,
    Reveal,
    ClearQuery,
    RegexToggle,
    HistoryPrev,
    HistoryNext,
    MoveUp,
    MoveDown,
    PreviewLayout,
    DensityToggle,
    FoldToggle,
    ThemeCycle,
    PreviewPageUp,
    PreviewPageDown,
    Help,
    ToggleMark,
    ClearMarks,
}

/// Key spec strings for the default bindings (identical to the historical
/// hardcoded `handle_key` table).
const DEFAULT_BINDINGS: &[(Action, &[&str])] = &[
    (Action::Quit, &["esc", "ctrl-c"]),
    (Action::Open, &["enter"]),
    (Action::Menu, &["right"]),
    (Action::QuickLook, &["ctrl-space"]),
    (Action::CopyPath, &["ctrl-y"]),
    (Action::Reveal, &["ctrl-f"]),
    (Action::ClearQuery, &["ctrl-u"]),
    (Action::RegexToggle, &["ctrl-r"]),
    (Action::HistoryPrev, &["ctrl-p"]),
    (Action::HistoryNext, &["ctrl-n"]),
    (Action::MoveUp, &["up", "ctrl-k"]),
    (Action::MoveDown, &["down", "ctrl-j"]),
    (Action::PreviewLayout, &["tab"]),
    (Action::DensityToggle, &["ctrl-t"]),
    (Action::FoldToggle, &["ctrl-x"]),
    (Action::ThemeCycle, &["ctrl-g"]),
    (Action::PreviewPageUp, &["pgup"]),
    (Action::PreviewPageDown, &["pgdn"]),
    (Action::Help, &["f1", "ctrl-o"]),
    (Action::ToggleMark, &["ctrl-b"]),
    (Action::ClearMarks, &["alt-b"]),
];

/// Parses a key spec like `"ctrl-y"`, `"CTRL+Y"`, `"alt+shift-x"`, `"f5"`,
/// `"pgdn"`, `"ctrl-space"` or `"x"`. Case-insensitive; modifiers (`ctrl`,
/// `alt`, `shift`) come first and are dash- or plus-separated from the base
/// key. Returns `None` for anything unrecognized.
pub fn parse_key(spec: &str) -> Option<(KeyCode, KeyModifiers)> {
    let lower = spec.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let parts: Vec<&str> = lower.split(['-', '+']).collect();
    let mut mods = KeyModifiers::NONE;
    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            "ctrl" => mods |= KeyModifiers::CONTROL,
            "alt" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => break,
        }
        i += 1;
    }
    if i == parts.len() || i + 1 != parts.len() {
        // no base key, or leftover non-modifier parts
        return None;
    }
    let base = parts[i];
    let code = match base {
        "esc" => KeyCode::Esc,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pgup" | "pageup" => KeyCode::PageUp,
        "pgdn" | "pagedown" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        _ => {
            if let Some(f) = base.strip_prefix('f')
                && let Ok(n) = f.parse::<u8>()
                && (1..=12).contains(&n)
            {
                KeyCode::F(n)
            } else {
                let mut chars = base.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None; // multi-char name we don't know
                }
                KeyCode::Char(c)
            }
        }
    };
    Some((code, mods))
}

/// Editing keys that can never be rebound (they are handled before the
/// keymap lookup in `tui::App::handle_key`):
/// - plain (and shifted) characters insert text
/// - backspace deletes backward
/// - left/right move the edit cursor
/// - ctrl-a / ctrl-e / ctrl-w / ctrl-d are readline-style editing
fn is_editing_key(code: KeyCode, mods: KeyModifiers) -> bool {
    match code {
        KeyCode::Char(c) => {
            !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                || (mods.contains(KeyModifiers::CONTROL) && matches!(c, 'a' | 'e' | 'w' | 'd'))
        }
        KeyCode::Backspace | KeyCode::Left | KeyCode::Right => true,
        _ => false,
    }
}

fn action_from_name(name: &str) -> Option<Action> {
    Some(match name {
        "quit" => Action::Quit,
        "open" => Action::Open,
        "menu" => Action::Menu,
        "quick_look" => Action::QuickLook,
        "copy_path" => Action::CopyPath,
        "reveal" => Action::Reveal,
        "clear_query" => Action::ClearQuery,
        "regex_toggle" => Action::RegexToggle,
        "history_prev" => Action::HistoryPrev,
        "history_next" => Action::HistoryNext,
        "move_up" => Action::MoveUp,
        "move_down" => Action::MoveDown,
        "preview_layout" => Action::PreviewLayout,
        "density_toggle" => Action::DensityToggle,
        "fold_toggle" => Action::FoldToggle,
        "theme_cycle" => Action::ThemeCycle,
        "preview_page_up" => Action::PreviewPageUp,
        "preview_page_down" => Action::PreviewPageDown,
        "help" => Action::Help,
        "toggle_mark" => Action::ToggleMark,
        "clear_marks" => Action::ClearMarks,
        _ => return None,
    })
}

/// Parses a key spec list, silently dropping unparsable specs and reserved
/// editing keys.
fn parse_specs<S: AsRef<str>>(specs: &[S]) -> Vec<(KeyCode, KeyModifiers)> {
    specs
        .iter()
        .filter_map(|s| parse_key(s.as_ref()))
        .filter(|(code, mods)| !is_editing_key(*code, *mods))
        .collect()
}

/// Key -> command map; `lookup` resolves a key event to its action.
pub struct Keymap {
    bindings: HashMap<(KeyCode, KeyModifiers), Action>,
}

impl Action {
    /// Human-readable name shown in the help overlay.
    pub fn label(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::Open => "open",
            Action::Menu => "actions menu",
            Action::QuickLook => "quick look",
            Action::CopyPath => "copy path",
            Action::Reveal => "reveal in finder",
            Action::ClearQuery => "clear query",
            Action::RegexToggle => "toggle regex",
            Action::HistoryPrev => "history back",
            Action::HistoryNext => "history forward",
            Action::MoveUp => "move up",
            Action::MoveDown => "move down",
            Action::PreviewLayout => "preview layout",
            Action::DensityToggle => "row density",
            Action::FoldToggle => "fold weak matches",
            Action::ThemeCycle => "cycle theme",
            Action::PreviewPageUp => "preview page up",
            Action::PreviewPageDown => "preview page down",
            Action::Help => "help",
        }
    }

    /// Display group used by the keymap-driven help overlay.
    pub fn help_group(self) -> &'static str {
        match self {
            Action::MoveUp
            | Action::MoveDown
            | Action::HistoryPrev
            | Action::HistoryNext
            | Action::Quit => "navigation",
            Action::Open | Action::Menu | Action::QuickLook | Action::CopyPath | Action::Reveal => {
                "open & actions"
            }
            Action::PreviewLayout
            | Action::DensityToggle
            | Action::FoldToggle
            | Action::ThemeCycle
            | Action::PreviewPageUp
            | Action::PreviewPageDown => "view",
            Action::RegexToggle | Action::ClearQuery | Action::Help => "query modes",
        }
    }
}

fn key_label(code: KeyCode, mods: KeyModifiers) -> Option<String> {
    let base = match code {
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::PageUp => "pgup".to_string(),
        KeyCode::PageDown => "pgdn".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        _ => return None,
    };
    let mut parts = Vec::new();
    if mods.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if mods.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    if mods.contains(KeyModifiers::SHIFT) {
        parts.push("shift".to_string());
    }
    parts.push(base);
    Some(parts.join("-"))
}

impl Keymap {
    pub fn lookup(&self, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        self.bindings.get(&(code, mods)).copied()
    }

    /// Actions that currently have at least one configured binding. The help
    /// overlay uses this instead of duplicating the binding list.
    pub fn actions(&self) -> Vec<Action> {
        let mut actions: Vec<Action> = self.bindings.values().copied().collect();
        actions.sort_by_key(|action| (action.help_group(), action.label()));
        actions.dedup();
        actions
    }

    /// All configured key labels for an action, shortest first. The help
    /// overlay lists every binding, not just the shortest.
    pub fn labels(&self, action: Action) -> Vec<String> {
        let mut labels: Vec<String> = self
            .bindings
            .iter()
            .filter(|(_, bound)| **bound == action)
            .filter_map(|(&(code, mods), _)| key_label(code, mods))
            .collect();
        labels.sort_by(|a, b| {
            a.chars()
                .count()
                .cmp(&b.chars().count())
                .then_with(|| a.cmp(b))
        });
        labels
    }

    /// Shortest configured key label for an action, for contextual UI help.
    pub fn shortcut(&self, action: Action) -> Option<String> {
        self.labels(action).into_iter().next()
    }

    /// Builds a keymap from config overrides (action name -> key specs).
    /// Starts from the defaults; a valid override REPLACES that action's
    /// whole key list (silently skipping unparsable specs and reserved
    /// editing keys; if none parse, the defaults are kept). Overrides are
    /// inserted after all defaults, in action-name order, so collisions are
    /// deterministic and an override wins over a colliding default key.
    pub fn from_config(overrides: &HashMap<String, Vec<String>>) -> Keymap {
        let mut bindings: HashMap<(KeyCode, KeyModifiers), Action> = HashMap::new();
        // defaults first, inserted verbatim -- the reserved-key rule applies
        // to user overrides only, so e.g. `right` can stay bound to Menu
        for (action, specs) in DEFAULT_BINDINGS {
            for spec in *specs {
                if let Some((code, mods)) = parse_key(spec) {
                    bindings.insert((code, mods), *action);
                }
            }
        }
        let mut parsed_overrides = Vec::new();
        for (name, specs) in overrides {
            let Some(action) = action_from_name(name) else {
                continue;
            };
            let parsed = parse_specs(specs);
            if parsed.is_empty() {
                continue; // nothing usable: keep the defaults for this action
            }
            parsed_overrides.push((name, action, parsed));
        }
        // A HashMap has no config-file order. Sort the valid overrides so
        // that two commands claiming the same key resolve consistently.
        parsed_overrides.sort_by_key(|(name, _, _)| *name);
        // Remove every replaced action's defaults before inserting any
        // override. Otherwise one override can erase another override's key
        // when that key was also a default for the second action.
        for (_, action, _) in &parsed_overrides {
            for (default_action, default_specs) in DEFAULT_BINDINGS {
                if *default_action == *action {
                    for spec in *default_specs {
                        if let Some((code, mods)) = parse_key(spec) {
                            bindings.remove(&(code, mods));
                        }
                    }
                }
            }
        }
        // Overrides are inserted last, so they win collisions with defaults
        // and later action names win collisions with earlier ones.
        for (_, action, parsed) in parsed_overrides {
            for (code, mods) in parsed {
                bindings.insert((code, mods), action);
            }
        }
        Keymap { bindings }
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::from_config(&HashMap::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(spec: &str) -> Option<(KeyCode, KeyModifiers)> {
        parse_key(spec)
    }

    #[test]
    fn parse_key_forms() {
        assert_eq!(
            parse("ctrl-y"),
            Some((KeyCode::Char('y'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            parse("CTRL+Y"),
            Some((KeyCode::Char('y'), KeyModifiers::CONTROL))
        );
        assert_eq!(parse("esc"), Some((KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(parse("f5"), Some((KeyCode::F(5), KeyModifiers::NONE)));
        assert_eq!(parse("pgdn"), Some((KeyCode::PageDown, KeyModifiers::NONE)));
        assert_eq!(
            parse("ctrl-space"),
            Some((KeyCode::Char(' '), KeyModifiers::CONTROL))
        );
        assert_eq!(parse("x"), Some((KeyCode::Char('x'), KeyModifiers::NONE)));
        assert_eq!(
            parse("alt+shift-tab"),
            Some((KeyCode::Tab, KeyModifiers::ALT | KeyModifiers::SHIFT))
        );
        // bad specs
        for bad in ["", "ctrl", "ctrl-y-z", "foo", "f13", "  ", "ctrl--y"] {
            assert_eq!(parse(bad), None, "expected {bad:?} to be unparsable");
        }
    }

    #[test]
    fn defaults_match_the_historical_bindings() {
        let km = Keymap::default();
        let check = |spec: &str, action: Action| {
            let (code, mods) = parse_key(spec).unwrap();
            assert_eq!(km.lookup(code, mods), Some(action), "for {spec:?}");
        };
        check("esc", Action::Quit);
        check("ctrl-c", Action::Quit);
        check("enter", Action::Open);
        check("right", Action::Menu);
        check("ctrl-space", Action::QuickLook);
        check("ctrl-y", Action::CopyPath);
        check("ctrl-f", Action::Reveal);
        check("ctrl-u", Action::ClearQuery);
        check("ctrl-r", Action::RegexToggle);
        check("ctrl-p", Action::HistoryPrev);
        check("ctrl-n", Action::HistoryNext);
        check("up", Action::MoveUp);
        check("ctrl-k", Action::MoveUp);
        check("down", Action::MoveDown);
        check("ctrl-j", Action::MoveDown);
        check("tab", Action::PreviewLayout);
        check("ctrl-t", Action::DensityToggle);
        check("ctrl-x", Action::FoldToggle);
        check("ctrl-g", Action::ThemeCycle);
        check("pgup", Action::PreviewPageUp);
        check("pgdn", Action::PreviewPageDown);
        check("ctrl-b", Action::ToggleMark);
        check("alt-b", Action::ClearMarks);
        // keys never bound by default
        assert_eq!(km.lookup(KeyCode::Char('q'), KeyModifiers::CONTROL), None);
    }

    #[test]
    fn shortcut_labels_use_the_shortest_configured_binding() {
        let defaults = Keymap::default();
        assert_eq!(defaults.shortcut(Action::Open).as_deref(), Some("enter"));
        assert_eq!(defaults.shortcut(Action::Menu).as_deref(), Some("→"));
        assert_eq!(
            defaults.shortcut(Action::CopyPath).as_deref(),
            Some("ctrl-y")
        );
        assert_eq!(defaults.shortcut(Action::Quit).as_deref(), Some("esc"));

        let mut overrides = HashMap::new();
        overrides.insert("copy_path".to_string(), vec!["alt-y".to_string()]);
        let custom = Keymap::from_config(&overrides);
        assert_eq!(custom.shortcut(Action::CopyPath).as_deref(), Some("alt-y"));
    }

    #[test]
    fn mark_actions_are_configurable() {
        let mut overrides = HashMap::new();
        overrides.insert("toggle_mark".to_string(), vec!["ctrl-m".to_string()]);
        overrides.insert("clear_marks".to_string(), vec!["alt-m".to_string()]);
        let km = Keymap::from_config(&overrides);
        assert_eq!(
            km.lookup(KeyCode::Char('m'), KeyModifiers::CONTROL),
            Some(Action::ToggleMark)
        );
        assert_eq!(
            km.lookup(KeyCode::Char('m'), KeyModifiers::ALT),
            Some(Action::ClearMarks)
        );
        assert_eq!(km.lookup(KeyCode::Char('b'), KeyModifiers::CONTROL), None);
        assert_eq!(km.lookup(KeyCode::Char('b'), KeyModifiers::ALT), None);
    }

    #[test]
    fn override_replaces_an_action_and_removes_its_defaults() {
        let mut overrides = HashMap::new();
        overrides.insert("quit".to_string(), vec!["ctrl-q".to_string()]);
        let km = Keymap::from_config(&overrides);
        assert_eq!(
            km.lookup(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        assert_eq!(km.lookup(KeyCode::Esc, KeyModifiers::NONE), None);
    }

    #[test]
    fn override_with_only_invalid_specs_keeps_defaults() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "quit".to_string(),
            vec!["not-a-key".to_string(), "f13".to_string()],
        );
        let km = Keymap::from_config(&overrides);
        assert_eq!(
            km.lookup(KeyCode::Esc, KeyModifiers::NONE),
            Some(Action::Quit)
        );
        assert_eq!(
            km.lookup(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
    }

    #[test]
    fn editing_keys_are_ignored_in_overrides() {
        let mut overrides = HashMap::new();
        // backspace, a plain char, and ctrl-w are all editing keys
        overrides.insert(
            "quit".to_string(),
            vec![
                "backspace".to_string(),
                "x".to_string(),
                "ctrl-w".to_string(),
                "left".to_string(),
            ],
        );
        let km = Keymap::from_config(&overrides);
        // nothing was bound; the default quit keys remain
        assert_eq!(
            km.lookup(KeyCode::Esc, KeyModifiers::NONE),
            Some(Action::Quit)
        );
        assert_eq!(km.lookup(KeyCode::Backspace, KeyModifiers::NONE), None);
        assert_eq!(km.lookup(KeyCode::Char('x'), KeyModifiers::NONE), None);
        assert_eq!(km.lookup(KeyCode::Char('w'), KeyModifiers::CONTROL), None);
    }

    #[test]
    fn help_defaults_and_override() {
        let km = Keymap::default();
        let (f1, none) = (KeyCode::F(1), KeyModifiers::NONE);
        let (o, ctrl) = (KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(km.lookup(f1, none), Some(Action::Help));
        assert_eq!(km.lookup(o, ctrl), Some(Action::Help));
        assert_eq!(km.shortcut(Action::Help).as_deref(), Some("f1"));
        // both default bindings show up, shortest first
        assert_eq!(km.labels(Action::Help), vec!["f1", "ctrl-o"]);

        // a config override replaces the whole list, like any other action
        let mut overrides = HashMap::new();
        overrides.insert("help".to_string(), vec!["f2".to_string()]);
        let custom = Keymap::from_config(&overrides);
        assert_eq!(custom.lookup(KeyCode::F(2), none), Some(Action::Help));
        assert_eq!(custom.lookup(f1, none), None);
        assert_eq!(custom.lookup(o, ctrl), None);
    }

    #[test]
    fn cross_action_overrides_keep_reassigned_default_keys() {
        let mut overrides = HashMap::new();
        // Help takes ThemeCycle's old key while ThemeCycle moves away. The
        // result must not depend on HashMap iteration order.
        overrides.insert("help".to_string(), vec!["ctrl-g".to_string()]);
        overrides.insert("theme_cycle".to_string(), vec!["f2".to_string()]);
        let km = Keymap::from_config(&overrides);
        assert_eq!(
            km.lookup(KeyCode::Char('g'), KeyModifiers::CONTROL),
            Some(Action::Help)
        );
        assert_eq!(
            km.lookup(KeyCode::F(2), KeyModifiers::NONE),
            Some(Action::ThemeCycle)
        );
        assert_eq!(km.lookup(KeyCode::Char('o'), KeyModifiers::CONTROL), None);
    }

    #[test]
    fn unknown_action_names_are_ignored() {
        let mut overrides = HashMap::new();
        overrides.insert("nope".to_string(), vec!["ctrl-q".to_string()]);
        let km = Keymap::from_config(&overrides);
        assert_eq!(km.lookup(KeyCode::Char('q'), KeyModifiers::CONTROL), None);
    }
}
