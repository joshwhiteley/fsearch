//! Script output keeps record boundaries separate from display formatting.
use crate::{cli::OutputFormat, query::Hit};

pub fn hit(hit: Hit, format: OutputFormat) -> String {
    hit_for_terminal(hit, format, false)
}

/// Escape untrusted text only for human-readable terminal records. Pipes,
/// JSON and NUL records retain their exact machine-readable representation.
pub fn hit_for_terminal(hit: Hit, format: OutputFormat, terminal: bool) -> String {
    let hit = if terminal && format == OutputFormat::Text {
        match hit {
            Hit::Path(path) => Hit::Path(escape_controls(&path)),
            Hit::Line {
                path,
                line_number,
                line,
            } => Hit::Line {
                path: escape_controls(&path),
                line_number,
                line: escape_controls(&line),
            },
            Hit::Semantic {
                path,
                line_start,
                score,
            } => Hit::Semantic {
                path: escape_controls(&path),
                line_start,
                score,
            },
        }
    } else {
        hit
    };
    if format == OutputFormat::Nul {
        let path = match hit {
            Hit::Path(path) | Hit::Line { path, .. } | Hit::Semantic { path, .. } => path,
        };
        return format!("{path}\0");
    }
    let body = match (format, hit) {
        (OutputFormat::Json, Hit::Path(path)) => serde_json::json!({"type":"filename", "path":path}).to_string(),
        (OutputFormat::Json, Hit::Line {path, line_number, line}) => serde_json::json!({"type":"content", "path":path,"line_number":line_number,"text":line}).to_string(),
        (OutputFormat::Json, Hit::Semantic {path, line_start, score}) => serde_json::json!({"type":"semantic", "path":path,"line_number":line_start,"score":score}).to_string(),
        (_, Hit::Path(path)) => path,
        (_, Hit::Line {path, line_number, line}) => format!("{path}:{line_number}:{line}"),
        (_, Hit::Semantic {path, line_start, score}) => format!("{path}:{line_start}:{score:.2}"),
    };
    format!("{body}\n")
}

pub fn selection(value: &str, format: OutputFormat) -> String {
    selection_for_terminal(value, format, false)
}

pub fn selection_for_terminal(value: &str, format: OutputFormat, terminal: bool) -> String {
    if terminal && format == OutputFormat::Text {
        return format!("{}\n", escape_controls(value));
    }
    match format {
        OutputFormat::Text => format!("{value}\n"),
        OutputFormat::Nul => format!("{value}\0"),
        OutputFormat::Json => format!(
            "{}\n",
            serde_json::json!({"type":"selection", "value":value})
        ),
    }
}

/// Keep Unicode text, but render terminal controls as visible escape sequences.
pub fn escape_controls(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control() {
            escaped.extend(ch.escape_default());
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unusual_paths_roundtrip() {
        let path = "a\nquote\"\\é.txt";
        let json = hit(Hit::Path(path.into()), OutputFormat::Json);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["path"], path);
        assert_eq!(json.lines().count(), 1);
        assert_eq!(
            hit(Hit::Path(path.into()), OutputFormat::Nul),
            format!("{path}\0")
        );
    }
    #[test]
    fn terminal_text_cannot_inject_controls_but_machine_output_is_unchanged() {
        let value = "é\x1b]52;c;payload\x07\r\n\t\u{009b}31m";
        let tty = selection_for_terminal(value, OutputFormat::Text, true);
        assert_eq!(tty, format!("{}\n", escape_controls(value)));
        assert!(!tty[..tty.len() - 1].chars().any(char::is_control));
        assert_eq!(
            selection_for_terminal(value, OutputFormat::Text, false),
            format!("{value}\n")
        );
        for format in [OutputFormat::Json, OutputFormat::Nul] {
            assert_eq!(
                selection_for_terminal(value, format, true),
                selection(value, format)
            );
            assert_eq!(
                hit_for_terminal(Hit::Path(value.into()), format, true),
                hit(Hit::Path(value.into()), format)
            );
        }
        let line = hit_for_terminal(
            Hit::Line {
                path: value.into(),
                line_number: 7,
                line: value.into(),
            },
            OutputFormat::Text,
            true,
        );
        assert_eq!(
            line,
            format!("{}:7:{}\n", escape_controls(value), escape_controls(value))
        );
        assert_eq!(
            hit_for_terminal(
                Hit::Semantic {
                    path: value.into(),
                    line_start: 3,
                    score: 0.5,
                },
                OutputFormat::Text,
                true
            ),
            format!("{}:3:0.50\n", escape_controls(value))
        );
    }

    #[test]
    fn content_records_have_separate_fields() {
        let json = hit(
            Hit::Line {
                path: "a:b".into(),
                line_number: 42,
                line: "x\ny".into(),
            },
            OutputFormat::Json,
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["line_number"], 42);
        assert_eq!(value["text"], "x\ny");
    }
}
