use ratatui::style::Color;

/// How the pane chrome is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BorderKind {
    #[default]
    Sharp,
    Rounded,
    None,
}

/// The colors and shapes the UI is painted with. Everything else is the
/// terminal's own foreground/background.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// Match highlights, directory entries, content-hit paths.
    pub accent: Color,
    /// Status line, hints, gutters.
    pub dim: Color,
    /// Pane borders.
    pub border: Color,
    /// Pane titles.
    pub title: Color,
    /// Selected-row background; None keeps the REVERSED style.
    pub selection_bg: Option<Color>,
    /// Match-highlight color; None falls back to accent.
    pub match_fg: Option<Color>,
    /// Section-header color; None falls back to dim.
    pub section: Option<Color>,
    /// Badge palette: [image, video/audio, doc, code, archive, other].
    pub badges: [Color; 6],
    /// Pane border style.
    pub borders: BorderKind,
}

/// The default badge palette — the same set the row renderer hardcoded
/// before themes grew a palette.
const DEFAULT_BADGES: [Color; 6] = [
    Color::Cyan,     // image
    Color::Magenta,  // video/audio
    Color::Yellow,   // doc
    Color::Green,    // code
    Color::Red,      // archive
    Color::DarkGray, // other / FILE
];

const PRESETS: &[(&str, Theme)] = &[
    (
        "default",
        Theme {
            accent: Color::Cyan,
            dim: Color::DarkGray,
            border: Color::Reset,
            title: Color::Reset,
            selection_bg: None,
            match_fg: None,
            section: None,
            badges: DEFAULT_BADGES,
            borders: BorderKind::Sharp,
        },
    ),
    (
        "catppuccin",
        Theme {
            accent: Color::Rgb(0xcb, 0xa6, 0xf7),
            dim: Color::Rgb(0x6c, 0x70, 0x86),
            border: Color::Rgb(0x58, 0x5b, 0x70),
            title: Color::Rgb(0xb4, 0xbe, 0xfe),
            selection_bg: Some(Color::Rgb(0x31, 0x32, 0x44)),
            match_fg: Some(Color::Rgb(0xcb, 0xa6, 0xf7)),
            section: Some(Color::Rgb(0xa6, 0xad, 0xc8)),
            badges: [
                Color::Rgb(0x94, 0xe2, 0xd5), // teal
                Color::Rgb(0xeb, 0xa0, 0xac), // maroon
                Color::Rgb(0xf9, 0xe2, 0xaf), // yellow
                Color::Rgb(0xa6, 0xe3, 0xa1), // green
                Color::Rgb(0xfa, 0xb3, 0x87), // peach
                Color::Rgb(0x89, 0xb4, 0xfa), // blue
            ],
            borders: BorderKind::Sharp,
        },
    ),
    (
        "gruvbox",
        Theme {
            accent: Color::Rgb(0xfe, 0x80, 0x19),
            dim: Color::Rgb(0x92, 0x83, 0x74),
            border: Color::Rgb(0x50, 0x49, 0x45),
            title: Color::Rgb(0xfa, 0xbd, 0x2f),
            selection_bg: Some(Color::Rgb(0x3c, 0x38, 0x36)),
            match_fg: Some(Color::Rgb(0xfe, 0x80, 0x19)),
            section: Some(Color::Rgb(0xa8, 0x99, 0x84)),
            badges: [
                Color::Rgb(0x8e, 0xc0, 0x7c), // aqua
                Color::Rgb(0xd3, 0x86, 0x9b), // purple
                Color::Rgb(0xfa, 0xbd, 0x2f), // yellow
                Color::Rgb(0xb8, 0xbb, 0x26), // green
                Color::Rgb(0xfb, 0x49, 0x34), // red
                Color::Rgb(0xa8, 0x99, 0x84), // gray
            ],
            borders: BorderKind::Sharp,
        },
    ),
    (
        "nord",
        Theme {
            accent: Color::Rgb(0x88, 0xc0, 0xd0),
            dim: Color::Rgb(0x4c, 0x56, 0x6a),
            border: Color::Rgb(0x43, 0x4c, 0x5e),
            title: Color::Rgb(0x81, 0xa1, 0xc1),
            selection_bg: Some(Color::Rgb(0x3b, 0x42, 0x52)),
            match_fg: Some(Color::Rgb(0x88, 0xc0, 0xd0)),
            section: Some(Color::Rgb(0x8f, 0xbc, 0xbb)),
            badges: [
                Color::Rgb(0x8f, 0xbc, 0xbb), // nord7 teal
                Color::Rgb(0xb4, 0x8e, 0xad), // nord15 purple
                Color::Rgb(0xeb, 0xcb, 0x8b), // nord13 yellow
                Color::Rgb(0xa3, 0xbe, 0x8c), // nord14 green
                Color::Rgb(0xbf, 0x61, 0x6a), // nord11 red
                Color::Rgb(0x81, 0xa1, 0xc1), // nord9 blue
            ],
            borders: BorderKind::Sharp,
        },
    ),
    (
        "tokyonight",
        Theme {
            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            dim: Color::Rgb(0x56, 0x5f, 0x89),
            border: Color::Rgb(0x3b, 0x42, 0x61),
            title: Color::Rgb(0xbb, 0x9a, 0xf7),
            selection_bg: Some(Color::Rgb(0x24, 0x28, 0x3b)),
            match_fg: Some(Color::Rgb(0x7a, 0xa2, 0xf7)),
            section: Some(Color::Rgb(0xa9, 0xb1, 0xd6)),
            badges: [
                Color::Rgb(0x7d, 0xcf, 0xff), // cyan
                Color::Rgb(0xbb, 0x9a, 0xf7), // purple
                Color::Rgb(0xe0, 0xaf, 0x68), // yellow
                Color::Rgb(0x9e, 0xce, 0x6a), // green
                Color::Rgb(0xf7, 0x76, 0x8e), // red
                Color::Rgb(0x7a, 0xa2, 0xf7), // blue
            ],
            borders: BorderKind::Sharp,
        },
    ),
    (
        // high-contrast dark theme: readable dim text and a selection tint
        // instead of reverse-video, for terminals where default fg/bg clash
        "slate",
        Theme {
            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            dim: Color::Rgb(0xa9, 0xb1, 0xd6),
            border: Color::Rgb(0x3b, 0x42, 0x61),
            title: Color::Rgb(0xc0, 0xca, 0xf5),
            selection_bg: Some(Color::Rgb(0x28, 0x34, 0x57)),
            match_fg: Some(Color::Rgb(0x7a, 0xa2, 0xf7)),
            section: Some(Color::Rgb(0xa9, 0xb1, 0xd6)),
            badges: [
                Color::Rgb(0x7d, 0xcf, 0xee), // image
                Color::Rgb(0xbb, 0x9a, 0xf7), // video/audio
                Color::Rgb(0xe0, 0xaf, 0x68), // doc
                Color::Rgb(0x9e, 0xce, 0x6a), // code
                Color::Rgb(0xf7, 0x76, 0x8e), // archive
                Color::Rgb(0x56, 0x5f, 0x89), // other
            ],
            borders: BorderKind::Sharp,
        },
    ),
];

fn parse_hex(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(Color::Rgb(
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
    ))
}

/// Resolves a preset name (unknown names fall back to the default) with an
/// optional accent override. Kept for tests and the bare App::new default;
/// real runs go through [`resolve_config`].
pub fn resolve(preset: &str, accent: Option<&str>) -> Theme {
    resolve_config(&crate::config::ThemeConfig {
        preset: preset.to_string(),
        accent: accent.map(String::from),
        ..Default::default()
    })
}

/// Full theme resolution: preset + accent, then the config's border style
/// and the hex overrides (`selection_bg` / `match_fg` / `section`).
/// Unknown or malformed values are ignored, leaving the preset's choice.
pub fn resolve_config(cfg: &crate::config::ThemeConfig) -> Theme {
    let mut theme = PRESETS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&cfg.preset))
        .map_or(PRESETS[0].1, |(_, t)| *t);
    if let Some(color) = cfg.accent.as_deref().and_then(parse_hex) {
        theme.accent = color;
    }
    if let Some(color) = cfg.selection_bg.as_deref().and_then(parse_hex) {
        theme.selection_bg = Some(color);
    }
    if let Some(color) = cfg.match_fg.as_deref().and_then(parse_hex) {
        theme.match_fg = Some(color);
    }
    if let Some(color) = cfg.section.as_deref().and_then(parse_hex) {
        theme.section = Some(color);
    }
    match cfg.borders.as_deref() {
        Some("rounded") => theme.borders = BorderKind::Rounded,
        Some("none") => theme.borders = BorderKind::None,
        _ => {} // unknown → keep the preset's border style
    }
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_and_unknowns_fall_back() {
        assert_eq!(
            resolve("gruvbox", None).accent,
            Color::Rgb(0xfe, 0x80, 0x19)
        );
        assert_eq!(resolve("Nord", None).accent, Color::Rgb(0x88, 0xc0, 0xd0));
        assert_eq!(resolve("no-such-theme", None), resolve("default", None));
    }

    #[test]
    fn slate_is_high_contrast() {
        let t = resolve("slate", None);
        assert_eq!(t.dim, Color::Rgb(0xa9, 0xb1, 0xd6));
        assert_eq!(t.selection_bg, Some(Color::Rgb(0x28, 0x34, 0x57)));
        assert_eq!(t.match_fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
    }

    #[test]
    fn accent_override_applies() {
        let t = resolve("default", Some("#ff0080"));
        assert_eq!(t.accent, Color::Rgb(0xff, 0x00, 0x80));
        // dim/border untouched
        assert_eq!(t.dim, Color::DarkGray);
        // bad hex ignored
        assert_eq!(resolve("default", Some("chartreuse")).accent, Color::Cyan);
    }

    #[test]
    fn default_preset_keeps_classic_tokens() {
        let t = resolve("default", None);
        assert_eq!(t.selection_bg, None);
        assert_eq!(t.match_fg, None);
        assert_eq!(t.section, None);
        assert_eq!(t.borders, BorderKind::Sharp);
        assert_eq!(
            t.badges,
            [
                Color::Cyan,
                Color::Magenta,
                Color::Yellow,
                Color::Green,
                Color::Red,
                Color::DarkGray,
            ]
        );
    }

    #[test]
    fn named_presets_carry_palette_tokens() {
        let t = resolve("catppuccin", None);
        assert_eq!(t.borders, BorderKind::Sharp);
        assert!(t.selection_bg.is_some());
        assert_eq!(t.match_fg, Some(t.accent));
        assert!(t.section.is_some());
        assert_eq!(t.badges.len(), 6);
        assert_ne!(t.badges[0], t.badges[5]);
    }

    #[test]
    fn resolve_config_applies_borders_and_overrides() {
        use crate::config::ThemeConfig;
        let cfg = ThemeConfig {
            preset: "default".into(),
            borders: Some("rounded".into()),
            selection_bg: Some("#313244".into()),
            match_fg: Some("#cba6f7".into()),
            section: Some("#a6adc8".into()),
            ..Default::default()
        };
        let t = resolve_config(&cfg);
        assert_eq!(t.borders, BorderKind::Rounded);
        assert_eq!(t.selection_bg, Some(Color::Rgb(0x31, 0x32, 0x44)));
        assert_eq!(t.match_fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert_eq!(t.section, Some(Color::Rgb(0xa6, 0xad, 0xc8)));
        let none = ThemeConfig {
            borders: Some("none".into()),
            ..Default::default()
        };
        assert_eq!(resolve_config(&none).borders, BorderKind::None);
        let sharp = ThemeConfig {
            borders: Some("sharp".into()),
            ..Default::default()
        };
        assert_eq!(resolve_config(&sharp).borders, BorderKind::Sharp);
    }

    #[test]
    fn bad_theme_values_fall_back() {
        use crate::config::ThemeConfig;
        let cfg = ThemeConfig {
            preset: "default".into(),
            borders: Some("wobbly".into()),
            selection_bg: Some("chartreuse".into()),
            match_fg: Some("#12".into()),
            section: Some("red".into()),
            ..Default::default()
        };
        let t = resolve_config(&cfg);
        assert_eq!(t.borders, BorderKind::Sharp); // preset default
        assert_eq!(t.selection_bg, None);
        assert_eq!(t.match_fg, None);
        assert_eq!(t.section, None);
    }
}
