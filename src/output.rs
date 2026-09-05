//! Script output keeps record boundaries separate from display formatting.
use crate::{cli::OutputFormat, query::Hit};

pub fn hit(hit: Hit, format: OutputFormat) -> String {
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
    match format {
        OutputFormat::Text => format!("{value}\n"),
        OutputFormat::Nul => format!("{value}\0"),
        OutputFormat::Json => format!(
            "{}\n",
            serde_json::json!({"type":"selection", "value":value})
        ),
    }
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
