use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme};
use syntect::parsing::SyntaxSet;
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

/// Queries the terminal background color. Must run before entering raw mode /
/// the alternate screen, or the OSC reply interleaves with input events.
pub fn detect_appearance() -> Appearance {
    use terminal_colorsaurus::{QueryOptions, ThemeMode, theme_mode};
    match theme_mode(QueryOptions::default()) {
        Ok(ThemeMode::Light) => Appearance::Light,
        _ => Appearance::Dark,
    }
}

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn theme(appearance: Appearance) -> &'static Theme {
    static SET: OnceLock<EmbeddedLazyThemeSet> = OnceLock::new();
    let set = SET.get_or_init(two_face::theme::extra);
    set.get(match appearance {
        Appearance::Dark => EmbeddedThemeName::OneHalfDark,
        Appearance::Light => EmbeddedThemeName::OneHalfLight,
    })
}

/// Warm the lazily-loaded syntax and theme dumps off the render path.
pub fn preload() {
    std::thread::spawn(|| {
        let _ = syntax_set();
        let _ = theme(Appearance::Dark);
    });
}

// Skip highlighting pathological (e.g. minified) lines.
const MAX_LINE_LEN: usize = 2048;

/// bat's alpha-channel convention for themes that encode terminal-palette
/// colors: a == 0 → 16-color index carried in `r`; a == 1 → terminal default.
fn convert_color(c: syntect::highlighting::Color) -> Option<Color> {
    match c.a {
        0 => Some(Color::Indexed(c.r)),
        1 => None,
        _ => Some(Color::Rgb(c.r, c.g, c.b)),
    }
}

fn convert_style(s: syntect::highlighting::Style) -> Style {
    // No background: the terminal's own ground shows through.
    let mut style = Style::default();
    if let Some(fg) = convert_color(s.foreground) {
        style = style.fg(fg);
    }
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

/// Highlights up to `max_lines` of `text` as the language guessed from
/// `path`'s extension (or the first line, e.g. shebangs). Unknown languages
/// and over-long lines fall back to unstyled text.
pub fn highlight(
    path: &str,
    text: &str,
    appearance: Appearance,
    max_lines: usize,
) -> Vec<Line<'static>> {
    let set = syntax_set();
    let syntax = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| set.find_syntax_by_extension(ext))
        .or_else(|| set.find_syntax_by_first_line(text.lines().next().unwrap_or("")));
    let Some(syntax) = syntax else {
        return text
            .lines()
            .take(max_lines)
            .map(|l| Line::from(l.to_string()))
            .collect();
    };
    let mut highlighter = HighlightLines::new(syntax, theme(appearance));
    let mut out = Vec::with_capacity(max_lines.min(256));
    for line in text.lines().take(max_lines) {
        if line.len() > MAX_LINE_LEN {
            out.push(Line::from(line.to_string()));
            continue;
        }
        let with_newline = format!("{line}\n");
        match highlighter.highlight_line(&with_newline, set) {
            Ok(regions) => {
                let spans: Vec<Span<'static>> = regions
                    .into_iter()
                    .filter_map(|(style, s)| {
                        let s = s.trim_end_matches('\n');
                        (!s.is_empty()).then(|| Span::styled(s.to_string(), convert_style(style)))
                    })
                    .collect();
                out.push(Line::from(spans));
            }
            Err(_) => out.push(Line::from(line.to_string())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keywords_get_styled_spans() {
        let lines = highlight("main.rs", "fn main() {}\n", Appearance::Dark, 100);
        assert_eq!(lines.len(), 1);
        // at least one span carries a foreground color
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.fg.is_some() && s.content.contains("fn"))
        );
    }

    #[test]
    fn unknown_extension_falls_back_to_plain() {
        let lines = highlight("data.zzz", "no such language\n", Appearance::Dark, 100);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.iter().all(|s| s.style.fg.is_none()));
    }

    #[test]
    fn line_count_and_content_preserved() {
        let text = "let a = 1;\n\nlet b = 2;\n";
        let lines = highlight("x.rs", text, Appearance::Light, 100);
        assert_eq!(lines.len(), 3);
        let flat: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(flat, "let b = 2;");
    }

    #[test]
    fn max_lines_is_respected() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(highlight("x.txt", text, Appearance::Dark, 2).len(), 2);
    }

    #[test]
    fn pathological_long_lines_stay_unstyled() {
        let long = format!("fn x() {{ {} }}", "a + ".repeat(2000));
        let lines = highlight("x.rs", &long, Appearance::Dark, 100);
        assert_eq!(lines[0].spans.len(), 1);
    }

    #[test]
    fn bat_alpha_convention_is_decoded() {
        use syntect::highlighting::Color as SC;
        // a == 0: 16-color palette index carried in r
        assert_eq!(
            convert_color(SC { r: 3, g: 0, b: 0, a: 0 }),
            Some(Color::Indexed(3))
        );
        // a == 1: terminal default — emit nothing
        assert_eq!(convert_color(SC { r: 9, g: 9, b: 9, a: 1 }), None);
        // real rgb
        assert_eq!(
            convert_color(SC { r: 10, g: 20, b: 30, a: 255 }),
            Some(Color::Rgb(10, 20, 30))
        );
    }
}
