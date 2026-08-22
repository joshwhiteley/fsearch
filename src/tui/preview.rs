use super::chrome::themed_block;
use super::rows::{kind_label, path_name, row_age, shorten_home};
use super::{App, PREVIEW_BYTES};
use crate::highlight::{self, Appearance};
use crate::images;
use crate::util::human_size;
use crate::walker::FileMeta;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui_image::{StatefulImage, protocol::StatefulProtocol};
use std::time::SystemTime;

pub enum PreviewContent {
    Lines(Vec<Line<'static>>),
    Image(Box<StatefulProtocol>),
    /// Image rendered by fsearch's own chafa pipeline (geometric symbols,
    /// max quality); re-encoded only when the target area changes.
    #[cfg(feature = "chafa")]
    CellArt {
        img: image::DynamicImage,
        cols: u16,
        rows: u16,
        lines: Vec<Line<'static>>,
    },
}

/// One preview load job; everything the worker needs (no Picker/ratatui
/// image types cross the channel — protocol construction stays on the UI
/// thread).
pub struct PreviewRequest {
    pub generation: u64,
    pub path: String,
    pub line_number: Option<u64>,
    pub appearance: Appearance,
    pub gutter: Color,
}

pub struct PreviewResult {
    pub generation: u64,
    pub path: String,
    pub line_number: Option<u64>,
    pub payload: PreviewPayload,
}

pub enum PreviewPayload {
    /// Styled, line-numbered preview lines (text and PDFs).
    Lines(Vec<Line<'static>>),
    /// Decoded image; not yet converted to a ratatui-image protocol.
    Image(image::DynamicImage),
}

/// The expensive half of preview loading — read, syntax-highlight, PDF
/// extract, image decode — runs on this worker thread so the UI thread only
/// applies results. Mirrors the former synchronous load_preview logic.
pub(super) fn preview_payload(req: &PreviewRequest) -> PreviewPayload {
    if crate::pdf::is_pdf_path(&req.path) {
        return match crate::pdf::extract_cached(&req.path, &crate::pdf::default_cache_dir()) {
            Ok(text) => match req.line_number {
                Some(n) => {
                    let start = (n as usize).saturating_sub(6);
                    let gutter = Style::default().fg(req.gutter);
                    PreviewPayload::Lines(
                        text.lines()
                            .enumerate()
                            .skip(start)
                            .take(40)
                            .map(|(i, l)| {
                                Line::from(vec![
                                    Span::styled(format!("{:>5} ", i + 1), gutter),
                                    Span::raw(l.to_string()),
                                ])
                            })
                            .collect(),
                    )
                }
                None => PreviewPayload::Lines(
                    text.lines()
                        .take(100)
                        .map(|l| Line::from(l.to_string()))
                        .collect(),
                ),
            },
            Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(pdf: {e})"))]),
        };
    }
    if crate::office::is_office_path(&req.path) {
        return match crate::office::extract_cached(&req.path, &crate::office::default_cache_dir()) {
            Ok(text) => match req.line_number {
                Some(n) => {
                    let start = (n as usize).saturating_sub(6);
                    let gutter = Style::default().fg(req.gutter);
                    PreviewPayload::Lines(
                        text.lines()
                            .enumerate()
                            .skip(start)
                            .take(40)
                            .map(|(i, line)| {
                                Line::from(vec![
                                    Span::styled(format!("{:>5} ", i + 1), gutter),
                                    Span::raw(line.to_string()),
                                ])
                            })
                            .collect(),
                    )
                }
                None => PreviewPayload::Lines(
                    text.lines()
                        .take(100)
                        .map(|line| Line::from(line.to_string()))
                        .collect(),
                ),
            },
            Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(office: {e})"))]),
        };
    }
    if images::is_image_path(&req.path) {
        return match images::load(&req.path, images::MAX_IMAGE_BYTES) {
            Ok(img) => PreviewPayload::Image(img),
            Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(image: {e})"))]),
        };
    }
    match read_preview_bytes(&req.path) {
        Ok(bytes) if bytes.contains(&0) => PreviewPayload::Lines(vec![Line::from("(binary file)")]),
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            match req.line_number {
                // center the preview on the matching line, with a gutter
                Some(n) => {
                    let start = (n as usize).saturating_sub(6);
                    let end = start + 40;
                    PreviewPayload::Lines(
                        highlight::highlight(&req.path, &text, req.appearance, end)
                            .into_iter()
                            .enumerate()
                            .skip(start)
                            .map(|(i, line)| {
                                let gutter = Style::default().fg(req.gutter);
                                let mut spans =
                                    vec![Span::styled(format!("{:>5} ", i + 1), gutter)];
                                spans.extend(line.spans);
                                Line::from(spans)
                            })
                            .collect(),
                    )
                }
                None => PreviewPayload::Lines(highlight::highlight(
                    &req.path,
                    &text,
                    req.appearance,
                    100,
                )),
            }
        }
        Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(unreadable: {e})"))]),
    }
}

/// Reads at most PREVIEW_BYTES (plus a one-byte truncation sentinel) from
/// `path`: previewing must never slurp a multi-gigabyte file into memory
/// just to show its head. Binary detection then runs on that bounded head.
fn read_preview_bytes(path: &str) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    const CAP: u64 = PREVIEW_BYTES as u64 + 1;
    let file = std::fs::File::open(path)?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(CAP.min(len) as usize);
    file.take(CAP).read_to_end(&mut bytes)?;
    bytes.truncate(PREVIEW_BYTES);
    Ok(bytes)
}

pub(super) fn directory_listing(path: &str, accent: Color) -> Vec<Line<'static>> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return vec![Line::from("(unreadable directory)")];
    };
    let mut names: Vec<(bool, String)> = entries
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (is_dir, e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    names.sort_by(|a, b| (!a.0, &a.1).cmp(&(!b.0, &b.1)));
    names.truncate(200);
    if names.is_empty() {
        return vec![Line::from("(empty directory)")];
    }
    names
        .into_iter()
        .map(|(is_dir, name)| {
            if is_dir {
                Line::from(Span::styled(
                    format!("{name}/"),
                    Style::default().fg(accent),
                ))
            } else {
                Line::from(name)
            }
        })
        .collect()
}

/// Styled spans for the query input: a leading `>` / `?` mode prefix lights
/// up in the accent, and tokens the real parser consumes as filters (ext:,
/// kind:, changed:, ...) turn yellow. Concatenating the span contents
/// reproduces `input` exactly, so the cursor math below stays valid.
fn preview_meta(app: &App) -> Option<FileMeta> {
    let row = app.engine.results().get(app.selected)?;
    if let Some(m) = row.meta {
        return Some(m);
    }
    if app.status.path == row.path
        && let Some((_, len, modified)) = app.status.meta
    {
        let mtime = modified
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        return Some(FileMeta { mtime, size: len });
    }
    None
}

/// Kind label for the preview header: uppercased extension, or DIR / FILE.
pub fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = themed_block("preview", &app.theme);
    let inner = block.inner(area);
    app.hit_test.preview_area = inner;
    frame.render_widget(block, area);
    // 2-line header: dim parent path + bold filename, then a dim
    // kind · size · age line (with pixel dims for images, line count for text)
    if let Some(row) = app.engine.results().get(app.selected) {
        let shown = shorten_home(&row.path);
        let name = path_name(&row.path);
        let parent = shown[..shown.len() - name.len()].to_string();
        let dim = Style::default().fg(app.theme.dim);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(parent, dim),
                Span::styled(name, bold),
            ])),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
        let mut meta_line = vec![Span::styled(kind_label(&row.path), dim)];
        // image previews carry their pixels between the kind and the size
        if !matches!(&app.preview.content, PreviewContent::Lines(_))
            && let Some((w, h)) = app.preview.image_dims
        {
            meta_line.push(Span::styled(format!(" · {w}×{h}"), dim));
        }
        if let Some(meta) = preview_meta(app) {
            meta_line.push(Span::styled(format!(" · {}", human_size(meta.size)), dim));
            if let Some(age) = row_age(Some(meta)) {
                meta_line.push(Span::styled(format!(" · {age}"), dim));
            }
        }
        if let PreviewContent::Lines(lines) = &app.preview.content
            && !lines.is_empty()
        {
            meta_line.push(Span::styled(format!(" · {} lines", lines.len()), dim));
        }
        frame.render_widget(
            Paragraph::new(Line::from(meta_line)),
            Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: 1,
            },
        );
    }
    // the preview body: everything below the two header rows
    let body = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    let dim = Style::default().fg(app.theme.dim);
    match &mut app.preview.content {
        PreviewContent::Lines(lines) => {
            let total = lines.len();
            let visible = body.height as usize;
            if total > visible && visible > 1 {
                // the last body row shows the position line, so the content
                // gets one row less
                let content_rows = visible - 1;
                app.preview.scroll = app.preview.scroll.min(total.saturating_sub(content_rows));
                let shown: Vec<Line<'static>> = lines
                    .iter()
                    .skip(app.preview.scroll)
                    .take(content_rows)
                    .cloned()
                    .collect();
                frame.render_widget(
                    Paragraph::new(shown),
                    Rect {
                        x: body.x,
                        y: body.y,
                        width: body.width,
                        height: content_rows as u16,
                    },
                );
                let first = app.preview.scroll + 1;
                let last = (app.preview.scroll + content_rows).min(total);
                let pos = format!("{first}–{last} / {total}");
                let avail = body.width.saturating_sub(1); // scrollbar column
                let pad = avail.saturating_sub(pos.chars().count() as u16) as usize;
                let line = Line::from(Span::styled(format!("{}{pos}", " ".repeat(pad)), dim));
                frame.render_widget(
                    Paragraph::new(line),
                    Rect {
                        x: body.x,
                        y: body.y + body.height - 1,
                        width: body.width,
                        height: 1,
                    },
                );
                let mut bar_state = ScrollbarState::new(total)
                    .viewport_content_length(content_rows)
                    .position(app.preview.scroll);
                let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight).style(dim);
                frame.render_stateful_widget(bar, body, &mut bar_state);
            } else {
                app.preview.scroll = 0;
                let shown: Vec<Line<'static>> = lines.to_vec();
                frame.render_widget(Paragraph::new(shown), body);
            }
        }
        PreviewContent::Image(protocol) => {
            frame.render_stateful_widget(StatefulImage::default(), body, protocol.as_mut());
        }
        #[cfg(feature = "chafa")]
        PreviewContent::CellArt {
            img,
            cols,
            rows,
            lines,
        } => {
            let (want_cols, want_rows) =
                crate::cellart::fit_cells(img.width(), img.height(), body.width, body.height);
            if (*cols, *rows) != (want_cols, want_rows) {
                *lines = crate::cellart::render(img, want_cols, want_rows);
                (*cols, *rows) = (want_cols, want_rows);
            }
            frame.render_widget(Paragraph::new(lines.clone()), body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    #[test]
    fn office_text_is_available_to_preview() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"<w:document xmlns:w="x"><w:body><w:p><w:t>Preview Needle</w:t></w:p></w:body></w:document>"#,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.docx");
        std::fs::write(&path, zip.finish().unwrap().into_inner()).unwrap();
        let payload = preview_payload(&PreviewRequest {
            generation: 0,
            path: path.to_string_lossy().into_owned(),
            line_number: None,
            appearance: Appearance::Dark,
            gutter: Color::Gray,
        });
        let PreviewPayload::Lines(lines) = payload else {
            panic!("office preview should be text");
        };
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.contains("Preview Needle"))
        );
    }

    #[test]
    fn preview_read_is_bounded_and_keeps_the_head() {
        let dir = tempfile::tempdir().unwrap();
        // far larger than PREVIEW_BYTES: the read must stop at the cap
        let big = dir.path().join("big.txt");
        std::fs::write(&big, vec![b'a'; PREVIEW_BYTES * 4]).unwrap();
        let bytes = read_preview_bytes(big.to_str().unwrap()).unwrap();
        assert_eq!(bytes.len(), PREVIEW_BYTES);
        assert!(bytes.iter().all(|&b| b == b'a'));

        // small files round-trip intact
        let small = dir.path().join("small.txt");
        std::fs::write(&small, b"hello").unwrap();
        let bytes = read_preview_bytes(small.to_str().unwrap()).unwrap();
        assert_eq!(bytes, b"hello");

        // missing paths surface the io error like std::fs::read did
        assert!(read_preview_bytes(dir.path().join("gone.txt").to_str().unwrap()).is_err());
    }

    #[test]
    fn oversized_text_file_previews_only_its_head() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.log");
        std::fs::write(&path, {
            let mut v = vec![b'x'; PREVIEW_BYTES * 3];
            for slot in v.chunks_mut(80) {
                slot[slot.len() - 1] = b'\n'; // many lines, >100 of them
            }
            v[PREVIEW_BYTES] = 0; // a NUL beyond the cap must not matter
            v
        })
        .unwrap();
        let payload = preview_payload(&PreviewRequest {
            generation: 0,
            path: path.to_string_lossy().into_owned(),
            line_number: None,
            appearance: Appearance::Dark,
            gutter: Color::Gray,
        });
        // binary detection runs on the bounded head, so no false "binary"
        let PreviewPayload::Lines(lines) = payload else {
            panic!("oversized text file should preview as text");
        };
        assert_eq!(lines.len(), 100);
        let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first.contains('x'));
    }
}
