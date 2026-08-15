use ratatui::style::Color;

/// The handful of colors the UI is painted with. Everything else is the
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
}

const PRESETS: &[(&str, Theme)] = &[
    (
        "default",
        Theme {
            accent: Color::Cyan,
            dim: Color::DarkGray,
            border: Color::Reset,
            title: Color::Reset,
        },
    ),
    (
        "catppuccin",
        Theme {
            accent: Color::Rgb(0xcb, 0xa6, 0xf7),
            dim: Color::Rgb(0x6c, 0x70, 0x86),
            border: Color::Rgb(0x58, 0x5b, 0x70),
            title: Color::Rgb(0xb4, 0xbe, 0xfe),
        },
    ),
    (
        "gruvbox",
        Theme {
            accent: Color::Rgb(0xfe, 0x80, 0x19),
            dim: Color::Rgb(0x92, 0x83, 0x74),
            border: Color::Rgb(0x50, 0x49, 0x45),
            title: Color::Rgb(0xfa, 0xbd, 0x2f),
        },
    ),
    (
        "nord",
        Theme {
            accent: Color::Rgb(0x88, 0xc0, 0xd0),
            dim: Color::Rgb(0x4c, 0x56, 0x6a),
            border: Color::Rgb(0x43, 0x4c, 0x5e),
            title: Color::Rgb(0x81, 0xa1, 0xc1),
        },
    ),
    (
        "tokyonight",
        Theme {
            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            dim: Color::Rgb(0x56, 0x5f, 0x89),
            border: Color::Rgb(0x3b, 0x42, 0x61),
            title: Color::Rgb(0xbb, 0x9a, 0xf7),
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
/// optional accent override.
pub fn resolve(preset: &str, accent: Option<&str>) -> Theme {
    let mut theme = PRESETS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(preset))
        .map_or(PRESETS[0].1, |(_, t)| *t);
    if let Some(color) = accent.and_then(parse_hex) {
        theme.accent = color;
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
    fn accent_override_applies() {
        let t = resolve("default", Some("#ff0080"));
        assert_eq!(t.accent, Color::Rgb(0xff, 0x00, 0x80));
        // dim/border untouched
        assert_eq!(t.dim, Color::DarkGray);
        // bad hex ignored
        assert_eq!(resolve("default", Some("chartreuse")).accent, Color::Cyan);
    }
}
