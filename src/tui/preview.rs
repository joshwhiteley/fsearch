use super::chrome::themed_block;
use super::rows::{display_path_parts, kind_label, row_age, shorten_home};
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

const TEXT_PREVIEW_LINES: usize = 100;
const CONTEXT_PREVIEW_LINES: usize = 40;
const CONTEXT_LINES_BEFORE_MATCH: usize = 5;
const ARCHIVE_PREVIEW_ENTRIES: usize = 200;
// Match the Office ZIP guard: metadata parsing is bounded before opening the
// central directory, while entry contents are never decompressed.
const MAX_ZIP_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 4096;
const MAX_TAR_ENTRIES: usize = 10_000;
const MAX_TAR_INPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
enum ArchiveKind {
    Zip,
    Tar,
    GzipTar,
}

struct ArchiveEntry {
    name: String,
    size: u64,
    is_dir: bool,
}

struct ArchiveListing {
    entries: Vec<ArchiveEntry>,
    total_entries: usize,
    total_size: u64,
    capped: bool,
}

fn preview_notice(message: impl Into<String>, gutter: Color) -> Line<'static> {
    Line::from(Span::styled(
        message.into(),
        Style::default().fg(gutter).add_modifier(Modifier::ITALIC),
    ))
}

fn text_preview_lines(
    path: &str,
    text: &str,
    line_number: Option<u64>,
    appearance: Appearance,
    gutter: Color,
    syntax_highlight: bool,
    byte_truncated: bool,
) -> Vec<Line<'static>> {
    let source_line_count = text.lines().count();
    if source_line_count == 0 {
        return vec![preview_notice(
            match line_number {
                Some(line) => format!("(empty text; match at source line {line} is unavailable)"),
                None => "(empty text)".to_string(),
            },
            gutter,
        )];
    }

    let Some(requested_line) = line_number else {
        let mut lines = if syntax_highlight {
            highlight::highlight(path, text, appearance, TEXT_PREVIEW_LINES)
        } else {
            text.lines()
                .take(TEXT_PREVIEW_LINES)
                .map(|line| Line::from(line.to_string()))
                .collect()
        };
        if source_line_count > TEXT_PREVIEW_LINES {
            lines.push(preview_notice(
                format!("… preview limited to first {TEXT_PREVIEW_LINES} source lines"),
                gutter,
            ));
        }
        if byte_truncated {
            lines.push(preview_notice(
                format!(
                    "… source read limited to first {} KiB",
                    PREVIEW_BYTES / 1024
                ),
                gutter,
            ));
        }
        return lines;
    };

    let Ok(match_line) = usize::try_from(requested_line) else {
        return vec![preview_notice(
            format!(
                "(match at source line {requested_line} is beyond the text loaded for preview)"
            ),
            gutter,
        )];
    };
    if match_line == 0 || match_line > source_line_count {
        let message = if byte_truncated {
            format!(
                "(match at source line {requested_line} is beyond the first {} KiB loaded for preview)",
                PREVIEW_BYTES / 1024
            )
        } else {
            format!(
                "(match at source line {requested_line} is unavailable; available text has {source_line_count} source lines)"
            )
        };
        return vec![preview_notice(message, gutter)];
    }

    let start = match_line.saturating_sub(CONTEXT_LINES_BEFORE_MATCH + 1);
    let end = start
        .saturating_add(CONTEXT_PREVIEW_LINES)
        .min(source_line_count);
    let number_style = Style::default().fg(gutter);
    let active_style = number_style.add_modifier(Modifier::BOLD);
    let excerpt: Vec<Line<'static>> = if syntax_highlight {
        highlight::highlight(path, text, appearance, end)
            .into_iter()
            .skip(start)
            .collect()
    } else {
        text.lines()
            .skip(start)
            .take(end - start)
            .map(|line| Line::from(line.to_string()))
            .collect()
    };
    let mut lines: Vec<Line<'static>> = excerpt
        .into_iter()
        .enumerate()
        .map(|(offset, mut line)| {
            let source_line = start + offset + 1;
            let is_match = source_line == match_line;
            if is_match {
                for span in &mut line.spans {
                    span.style = span.style.add_modifier(Modifier::BOLD);
                }
            }
            let marker = if is_match { '▶' } else { ' ' };
            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            spans.push(Span::styled(
                format!("{marker}{source_line:>5} "),
                if is_match { active_style } else { number_style },
            ));
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect();

    if start > 0 || end < source_line_count {
        lines.push(preview_notice(
            format!(
                "… context limited to {CONTEXT_PREVIEW_LINES} source lines; showing source lines {}–{end}",
                start + 1
            ),
            gutter,
        ));
    }
    if byte_truncated {
        lines.push(preview_notice(
            format!(
                "… source read limited to first {} KiB",
                PREVIEW_BYTES / 1024
            ),
            gutter,
        ));
    }
    lines
}

/// The expensive half of preview loading — read, syntax-highlight, PDF
/// extract, image decode — runs on this worker thread so the UI thread only
/// applies results. Mirrors the former synchronous load_preview logic.
pub(super) fn preview_payload(req: &PreviewRequest) -> PreviewPayload {
    match std::fs::metadata(&req.path) {
        Ok(meta) if meta.is_dir() => {
            return PreviewPayload::Lines(directory_listing(&req.path, req.gutter));
        }
        Ok(meta) if !meta.is_file() => {
            return PreviewPayload::Lines(vec![Line::from("(not a regular file)")]);
        }
        _ => {}
    }
    if crate::pdf::is_pdf_path(&req.path) {
        return match crate::pdf::extract_cached(&req.path, &crate::pdf::default_cache_dir()) {
            Ok(text) => PreviewPayload::Lines(text_preview_lines(
                &req.path,
                &text,
                req.line_number,
                req.appearance,
                req.gutter,
                false,
                false,
            )),
            Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(pdf: {e})"))]),
        };
    }
    if crate::office::is_office_path(&req.path) {
        return match crate::office::extract_cached(&req.path, &crate::office::default_cache_dir()) {
            Ok(text) => PreviewPayload::Lines(text_preview_lines(
                &req.path,
                &text,
                req.line_number,
                req.appearance,
                req.gutter,
                false,
                false,
            )),
            Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(office: {e})"))]),
        };
    }
    if images::is_image_path(&req.path) {
        return match images::load(&req.path, images::MAX_IMAGE_BYTES) {
            Ok(img) => PreviewPayload::Image(img),
            Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(image: {e})"))]),
        };
    }
    if let Some(kind) = archive_kind(&req.path) {
        return PreviewPayload::Lines(archive_preview(&req.path, kind, req.gutter));
    }
    match read_preview_bytes(&req.path) {
        Ok(preview) if preview.bytes.contains(&0) => {
            PreviewPayload::Lines(vec![Line::from("(binary file)")])
        }
        Ok(preview) => {
            let text = String::from_utf8_lossy(&preview.bytes);
            PreviewPayload::Lines(text_preview_lines(
                &req.path,
                &text,
                req.line_number,
                req.appearance,
                req.gutter,
                true,
                preview.truncated,
            ))
        }
        Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(unreadable: {e})"))]),
    }
}

fn archive_kind(path: &str) -> Option<ArchiveKind> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some(ArchiveKind::GzipTar)
    } else if lower.ends_with(".tar") {
        Some(ArchiveKind::Tar)
    } else if lower.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else {
        None
    }
}

fn archive_preview(path: &str, kind: ArchiveKind, gutter: Color) -> Vec<Line<'static>> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match kind {
        ArchiveKind::Zip => zip_listing(path),
        ArchiveKind::Tar => tar_listing(path, false),
        ArchiveKind::GzipTar => tar_listing(path, true),
    }));
    match result {
        Ok(Ok(listing)) => archive_lines(listing, gutter),
        Ok(Err(())) | Err(_) => vec![Line::from("(unreadable archive)")],
    }
}

fn zip_listing(path: &str) -> Result<ArchiveListing, ()> {
    let file = crate::util::open_regular_file(std::path::Path::new(path)).map_err(|_| ())?;
    if file.metadata().map_err(|_| ())?.len() > MAX_ZIP_BYTES {
        return Err(());
    }
    let mut archive = zip::ZipArchive::new(file).map_err(|_| ())?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(());
    }

    let mut entries = Vec::with_capacity(archive.len().min(ARCHIVE_PREVIEW_ENTRIES));
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|_| ())?;
        total_size = total_size.checked_add(entry.size()).ok_or(())?;
        if entries.len() < ARCHIVE_PREVIEW_ENTRIES {
            entries.push(ArchiveEntry {
                name: entry.name().to_owned(),
                size: entry.size(),
                is_dir: entry.is_dir(),
            });
        }
    }
    Ok(ArchiveListing {
        entries,
        total_entries: archive.len(),
        total_size,
        capped: false,
    })
}

struct LimitedReader<R> {
    inner: R,
    remaining: u64,
    capped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<R> LimitedReader<R> {
    fn new(inner: R, limit: u64, capped: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            inner,
            remaining: limit,
            capped,
        }
    }
}

impl<R: std::io::Read> std::io::Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            self.capped
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Err(std::io::Error::other("archive read limit exceeded"));
        }
        let len = (buf.len() as u64).min(self.remaining) as usize;
        let read = self.inner.read(&mut buf[..len])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

fn tar_listing(path: &str, gzip: bool) -> Result<ArchiveListing, ()> {
    let file = crate::util::open_regular_file(std::path::Path::new(path)).map_err(|_| ())?;
    let capped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if gzip {
        let compressed = LimitedReader::new(file, MAX_TAR_INPUT_BYTES, capped.clone());
        let decoded = flate2::read::MultiGzDecoder::new(compressed);
        read_tar_entries(
            LimitedReader::new(decoded, MAX_TAR_INPUT_BYTES, capped.clone()),
            capped,
        )
    } else {
        read_tar_entries(
            LimitedReader::new(file, MAX_TAR_INPUT_BYTES, capped.clone()),
            capped,
        )
    }
}

fn read_tar_entries<R: std::io::Read>(
    reader: R,
    capped: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<ArchiveListing, ()> {
    let mut archive = tar::Archive::new(reader);
    let (entries, total_entries, total_size, mut was_capped) = {
        let mut entries = Vec::with_capacity(ARCHIVE_PREVIEW_ENTRIES);
        let mut total_entries = 0;
        let mut total_size = 0_u64;
        let mut was_capped = false;
        let mut archive_entries = archive.entries().map_err(|_| ())?;

        loop {
            match archive_entries.next() {
                Some(Ok(entry)) => {
                    total_entries += 1;
                    let size = entry.header().size().map_err(|_| ())?;
                    total_size = total_size.checked_add(size).ok_or(())?;
                    if entries.len() < ARCHIVE_PREVIEW_ENTRIES {
                        let is_dir = entry.header().entry_type().is_dir();
                        let name = entry.path().map_err(|_| ())?.to_string_lossy().into_owned();
                        entries.push(ArchiveEntry { name, size, is_dir });
                    }
                    if total_entries >= MAX_TAR_ENTRIES {
                        match archive_entries.next() {
                            Some(Ok(_)) => {
                                capped.store(true, std::sync::atomic::Ordering::Relaxed);
                                was_capped = true;
                                break;
                            }
                            Some(Err(_)) if capped.load(std::sync::atomic::Ordering::Relaxed) => {
                                was_capped = true;
                                break;
                            }
                            Some(Err(_)) => return Err(()),
                            None => {}
                        }
                    }
                }
                Some(Err(_)) if capped.load(std::sync::atomic::Ordering::Relaxed) => {
                    was_capped = true;
                    break;
                }
                Some(Err(_)) => return Err(()),
                None => break,
            }
        }
        (entries, total_entries, total_size, was_capped)
    };
    let mut reader = archive.into_inner();
    if std::io::copy(&mut reader, &mut std::io::sink()).is_err()
        && !capped.load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(());
    }
    was_capped |= capped.load(std::sync::atomic::Ordering::Relaxed);
    Ok(ArchiveListing {
        entries,
        total_entries,
        total_size,
        capped: was_capped,
    })
}

fn archive_lines(listing: ArchiveListing, gutter: Color) -> Vec<Line<'static>> {
    let count = if listing.capped {
        format!("at least {}", listing.total_entries)
    } else {
        listing.total_entries.to_string()
    };
    let total = if listing.capped {
        format!("at least {}", human_size(listing.total_size))
    } else {
        human_size(listing.total_size)
    };
    let mut lines = Vec::with_capacity(listing.entries.len() + 2);
    let accent = Style::default().fg(gutter);
    lines.push(Line::from(Span::styled(
        format!("archive · {count} entries · {total} total"),
        accent.add_modifier(Modifier::BOLD),
    )));
    for entry in listing.entries {
        let name = if entry.is_dir && !entry.name.ends_with('/') {
            format!("{}/", entry.name)
        } else {
            entry.name
        };
        lines.push(Line::from(vec![
            Span::raw(name),
            Span::styled(format!(" · {}", human_size(entry.size)), accent),
        ]));
    }
    let omitted = listing
        .total_entries
        .saturating_sub(ARCHIVE_PREVIEW_ENTRIES);
    if omitted > 0 {
        let more = if listing.capped {
            format!("at least {omitted}")
        } else {
            omitted.to_string()
        };
        lines.push(Line::from(format!("… and {more} more")));
    }
    lines
}

struct PreviewBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Reads at most PREVIEW_BYTES (plus a one-byte truncation sentinel) from
/// `path`: previewing must never slurp a multi-gigabyte file into memory
/// just to show its head. Binary detection then runs on that bounded head.
fn read_preview_bytes(path: &str) -> std::io::Result<PreviewBytes> {
    use std::io::Read as _;
    const READ_LIMIT: u64 = PREVIEW_BYTES as u64 + 1;
    let file = crate::util::open_regular_file(std::path::Path::new(path))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(READ_LIMIT.min(len) as usize);
    file.take(READ_LIMIT).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > PREVIEW_BYTES;
    bytes.truncate(PREVIEW_BYTES);
    Ok(PreviewBytes { bytes, truncated })
}

pub(super) fn directory_listing(path: &str, accent: Color) -> Vec<Line<'static>> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return vec![Line::from("(unreadable directory)")];
    };
    // Bound iteration as well as storage. Sort only the first 200 entries
    // returned by the filesystem, rather than enumerating an entire tree.
    let mut entries = entries;
    let mut names: Vec<(bool, String)> = entries
        .by_ref()
        .take(200)
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (is_dir, e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    let truncated = entries.next().is_some();
    names.sort_by(|a, b| (!a.0, &a.1).cmp(&(!b.0, &b.1)));
    if names.is_empty() {
        return vec![Line::from("(empty directory)")];
    }
    let mut lines: Vec<_> = names
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
        .collect();
    if truncated {
        lines.push(Line::from("… more entries (showing first 200)"));
    }
    lines
}

/// Styled spans for the query input: a leading `>` / `?` mode prefix lights
/// up in the accent, and tokens the real parser consumes as filters (ext:,
/// kind:, changed:, ...) turn yellow. Concatenating the span contents
/// reproduces `input` exactly, so the cursor math below stays valid.
fn preview_meta(app: &App) -> Option<FileMeta> {
    let row = app.visible_selected_row()?;
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
    if let Some(row) = app.visible_selected_row() {
        let shown = shorten_home(&row.path);
        let (parent, name) = display_path_parts(&shown);
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
            let row_label = if lines.len() == 1 {
                "preview row"
            } else {
                "preview rows"
            };
            meta_line.push(Span::styled(format!(" · {} {row_label}", lines.len()), dim));
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
                let pos = format!("{first}–{last} / {total} preview rows");
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

    fn preview_text(payload: PreviewPayload) -> Vec<String> {
        let PreviewPayload::Lines(lines) = payload else {
            panic!("archive preview should be text");
        };
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    fn request(path: &std::path::Path) -> PreviewRequest {
        request_at(path, None)
    }

    fn request_at(path: &std::path::Path, line_number: Option<u64>) -> PreviewRequest {
        PreviewRequest {
            generation: 0,
            path: path.to_string_lossy().into_owned(),
            line_number,
            appearance: Appearance::Dark,
            gutter: Color::Gray,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn directory_worker_bounds_listing_and_marks_truncation() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..205 {
            std::fs::write(dir.path().join(format!("entry-{n}")), b"").unwrap();
        }
        let lines = preview_text(preview_payload(&request(dir.path())));
        assert_eq!(lines.len(), 201);
        assert_eq!(lines[200], "… more entries (showing first 200)");
        assert!(lines[..200].iter().all(|line| line.starts_with("entry-")));
    }

    #[test]
    fn exactly_200_directory_entries_need_no_truncation_marker() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..200 {
            std::fs::write(dir.path().join(format!("entry-{n}")), b"").unwrap();
        }
        let lines = preview_text(preview_payload(&request(dir.path())));
        assert_eq!(lines.len(), 200);
        assert!(lines.iter().all(|line| line.starts_with("entry-")));
    }

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
        let payload = preview_payload(&request_at(&path, Some(1)));
        let PreviewPayload::Lines(lines) = payload else {
            panic!("office preview should be text");
        };
        assert_eq!(
            lines
                .iter()
                .filter(|line| line_text(line).starts_with('▶'))
                .count(),
            1
        );
        assert!(line_text(&lines[0]).contains("Preview Needle"));
        assert!(
            lines[0]
                .spans
                .iter()
                .skip(1)
                .all(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn zip_archive_preview_lists_entries_and_total() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.add_directory("docs/", SimpleFileOptions::default())
            .unwrap();
        zip.start_file("docs/readme.md", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"hello").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.zip");
        std::fs::write(&path, zip.finish().unwrap().into_inner()).unwrap();

        let lines = preview_text(preview_payload(&request(&path)));
        assert_eq!(lines[0], "archive · 2 entries · 5 B total");
        assert_eq!(lines[1], "docs/ · 0 B");
        assert_eq!(lines[2], "docs/readme.md · 5 B");
    }

    #[test]
    fn zip_archive_preview_truncates_after_200_entries() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for index in 0..205 {
            zip.start_file(format!("file-{index}.txt"), SimpleFileOptions::default())
                .unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.zip");
        std::fs::write(&path, zip.finish().unwrap().into_inner()).unwrap();

        let lines = preview_text(preview_payload(&request(&path)));
        assert_eq!(lines.len(), 202);
        assert_eq!(lines[0], "archive · 205 entries · 0 B total");
        assert_eq!(lines[1], "file-0.txt · 0 B");
        assert_eq!(lines[200], "file-199.txt · 0 B");
        assert_eq!(lines[201], "… and 5 more");
    }

    #[test]
    fn gzip_tar_archive_preview_lists_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_size(0);
        directory.set_cksum();
        builder
            .append_data(&mut directory, "docs", Cursor::new(Vec::<u8>::new()))
            .unwrap();

        let mut readme = tar::Header::new_gnu();
        readme.set_size(5);
        readme.set_cksum();
        builder
            .append_data(&mut readme, "docs/readme.md", Cursor::new(b"hello"))
            .unwrap();
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        let lines = preview_text(preview_payload(&request(&path)));
        assert_eq!(lines[0], "archive · 2 entries · 5 B total");
        assert_eq!(lines[1], "docs/ · 0 B");
        assert_eq!(lines[2], "docs/readme.md · 5 B");
    }

    #[test]
    fn tar_archive_preview_lists_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.tar");
        let file = std::fs::File::create(&path).unwrap();
        let mut builder = tar::Builder::new(file);
        let mut entry = tar::Header::new_gnu();
        entry.set_size(5);
        entry.set_cksum();
        builder
            .append_data(&mut entry, "readme.txt", Cursor::new(b"hello"))
            .unwrap();
        builder.finish().unwrap();

        let lines = preview_text(preview_payload(&request(&path)));
        assert_eq!(
            lines,
            ["archive · 1 entries · 5 B total", "readme.txt · 5 B"]
        );
    }

    #[test]
    fn corrupt_archive_preview_is_friendly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.tgz");
        std::fs::write(&path, b"not an archive").unwrap();

        let lines = preview_text(preview_payload(&request(&path)));
        assert_eq!(lines, ["(unreadable archive)"]);
    }

    #[test]
    fn preview_read_is_bounded_and_keeps_the_head() {
        let dir = tempfile::tempdir().unwrap();
        // far larger than PREVIEW_BYTES: the read must stop at the cap
        let big = dir.path().join("big.txt");
        std::fs::write(&big, vec![b'a'; PREVIEW_BYTES * 4]).unwrap();
        let preview = read_preview_bytes(big.to_str().unwrap()).unwrap();
        assert_eq!(preview.bytes.len(), PREVIEW_BYTES);
        assert!(preview.bytes.iter().all(|&b| b == b'a'));
        assert!(preview.truncated);

        // The sentinel distinguishes the exact cap from actual truncation.
        let exact = dir.path().join("exact.txt");
        std::fs::write(&exact, vec![b'b'; PREVIEW_BYTES]).unwrap();
        let preview = read_preview_bytes(exact.to_str().unwrap()).unwrap();
        assert_eq!(preview.bytes.len(), PREVIEW_BYTES);
        assert!(!preview.truncated);

        // small files round-trip intact
        let small = dir.path().join("small.txt");
        std::fs::write(&small, b"hello").unwrap();
        let preview = read_preview_bytes(small.to_str().unwrap()).unwrap();
        assert_eq!(preview.bytes, b"hello");
        assert!(!preview.truncated);

        // missing paths surface the io error like std::fs::read did
        assert!(read_preview_bytes(dir.path().join("gone.txt").to_str().unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn text_and_archive_readers_reject_fifo_and_symlink_sources() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated path and the mode is valid.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let regular = dir.path().join("regular");
        std::fs::write(&regular, "text").unwrap();
        let link = dir.path().join("link");
        symlink(&regular, &link).unwrap();
        for path in [&fifo, &link] {
            let path = path.to_str().unwrap();
            assert!(read_preview_bytes(path).is_err());
            assert!(zip_listing(path).is_err());
            assert!(tar_listing(path, false).is_err());
            assert!(tar_listing(path, true).is_err());
        }
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
        assert_eq!(lines.len(), TEXT_PREVIEW_LINES + 2);
        assert!(line_text(&lines[0]).contains('x'));
        assert!(line_text(&lines[TEXT_PREVIEW_LINES]).contains("first 100 source lines"));
        assert!(line_text(&lines[TEXT_PREVIEW_LINES + 1]).contains("first 64 KiB"));
    }

    #[test]
    fn normal_text_marks_only_actual_truncation() {
        let exact = (1..=TEXT_PREVIEW_LINES)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = text_preview_lines(
            "notes.txt",
            &exact,
            None,
            Appearance::Dark,
            Color::Gray,
            true,
            false,
        );
        assert_eq!(lines.len(), TEXT_PREVIEW_LINES);
        assert!(
            lines
                .iter()
                .all(|line| !line_text(line).contains("limited"))
        );

        let truncated = format!("{exact}\nline 101");
        let lines = text_preview_lines(
            "notes.txt",
            &truncated,
            None,
            Appearance::Dark,
            Color::Gray,
            true,
            false,
        );
        assert_eq!(lines.len(), TEXT_PREVIEW_LINES + 1);
        assert!(line_text(lines.last().unwrap()).contains("first 100 source lines"));
    }

    #[test]
    fn context_marks_only_matching_line_and_preserves_syntax_styles() {
        let text = "let alpha = 1;\nlet café = 2; // 東京\nlet omega = 3;";
        let lines = text_preview_lines(
            "sample.rs",
            text,
            Some(2),
            Appearance::Dark,
            Color::Cyan,
            true,
            false,
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line_text(line).starts_with('▶'))
                .count(),
            1
        );
        assert!(line_text(&lines[1]).starts_with("▶    2 "));
        assert!(line_text(&lines[1]).contains("café = 2; // 東京"));
        assert!(
            lines[1]
                .spans
                .iter()
                .skip(1)
                .all(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .skip(1)
                .any(|span| span.style.fg.is_some()),
            "active-line bolding must retain syntax colors"
        );
        assert!(
            lines
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != 1)
                .all(|(_, line)| line_text(line).starts_with(' '))
        );
    }

    #[test]
    fn context_cap_is_reported_only_when_source_rows_are_omitted() {
        let exact = (1..=CONTEXT_PREVIEW_LINES)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = text_preview_lines(
            "notes.txt",
            &exact,
            Some(6),
            Appearance::Dark,
            Color::Gray,
            true,
            false,
        );
        assert_eq!(lines.len(), CONTEXT_PREVIEW_LINES);

        let truncated = format!("{exact}\nline 41");
        let lines = text_preview_lines(
            "notes.txt",
            &truncated,
            Some(6),
            Appearance::Dark,
            Color::Gray,
            true,
            false,
        );
        assert_eq!(lines.len(), CONTEXT_PREVIEW_LINES + 1);
        assert!(line_text(lines.last().unwrap()).contains("context limited to 40 source lines"));

        let late_context = text_preview_lines(
            "notes.txt",
            &truncated,
            Some(41),
            Appearance::Dark,
            Color::Gray,
            true,
            false,
        );
        assert!(line_text(&late_context[0]).starts_with(&format!(" {:>5} ", 36)));
        assert!(line_text(&late_context[5]).starts_with(&format!("▶{:>5} ", 41)));
    }

    #[test]
    fn empty_text_and_late_match_have_informative_rows() {
        let empty = text_preview_lines(
            "empty.txt",
            "",
            Some(7),
            Appearance::Dark,
            Color::Gray,
            true,
            false,
        );
        assert_eq!(empty.len(), 1);
        assert!(line_text(&empty[0]).contains("empty text"));
        assert!(line_text(&empty[0]).contains("source line 7"));

        let late = text_preview_lines(
            "bounded.txt",
            "first\nsecond",
            Some(9000),
            Appearance::Dark,
            Color::Gray,
            true,
            true,
        );
        assert_eq!(late.len(), 1);
        assert!(line_text(&late[0]).contains("source line 9000"));
        assert!(line_text(&late[0]).contains("first 64 KiB"));
    }

    #[test]
    fn unicode_byte_cap_is_reported_only_past_exact_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let exact = "é".repeat(PREVIEW_BYTES / "é".len());
        let path = dir.path().join("unicode.txt");
        std::fs::write(&path, &exact).unwrap();
        let lines = preview_text(preview_payload(&request(&path)));
        assert!(lines.iter().all(|line| !line.contains("64 KiB")));
        assert!(lines.iter().any(|line| line.contains('é')));

        std::fs::write(&path, format!("{exact}é")).unwrap();
        let lines = preview_text(preview_payload(&request(&path)));
        assert!(lines.iter().any(|line| line.contains("first 64 KiB")));
        assert!(lines.iter().any(|line| line.contains('é')));
    }
}
