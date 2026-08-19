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
    PreviewPageUp,
    PreviewPageDown,
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
    (Action::PreviewPageUp, &["pgup"]),
    (Action::PreviewPageDown, &["pgdn"]),
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
        "preview_page_up" => Action::PreviewPageUp,
        "preview_page_down" => Action::PreviewPageDown,
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

impl Keymap {
    pub fn lookup(&self, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        self.bindings.get(&(code, mods)).copied()
    }

    /// Builds a keymap from config overrides (action name -> key specs).
    /// Starts from the defaults; a valid override REPLACES that action's
    /// whole key list (silently skipping unparsable specs and reserved
    /// editing keys; if none parse, the defaults are kept). Overrides are
    /// inserted last, so an overriding key wins over a colliding default key.
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
        // overrides last, so they win collisions
        for (name, specs) in overrides {
            let Some(action) = action_from_name(name) else {
                continue;
            };
            let parsed = parse_specs(specs);
            if parsed.is_empty() {
                continue; // nothing usable: keep the defaults for this action
            }
            // REPLACE: drop this action's default keys first
            for (default_action, default_specs) in DEFAULT_BINDINGS {
                if *default_action == action {
                    for spec in *default_specs {
                        if let Some((code, mods)) = parse_key(spec) {
                            bindings.remove(&(code, mods));
                        }
                    }
                }
            }
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
        check("pgup", Action::PreviewPageUp);
        check("pgdn", Action::PreviewPageDown);
        // keys never bound by default
        assert_eq!(km.lookup(KeyCode::Char('q'), KeyModifiers::CONTROL), None);
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
    fn unknown_action_names_are_ignored() {
        let mut overrides = HashMap::new();
        overrides.insert("nope".to_string(), vec!["ctrl-q".to_string()]);
        let km = Keymap::from_config(&overrides);
        assert_eq!(km.lookup(KeyCode::Char('q'), KeyModifiers::CONTROL), None);
    }
}
