use super::chrome::{human_age, selection_style, themed_block};
use super::{App, Density, Slot};
use crate::engine::{EngineStatus, Mode};
use crate::matcher::Highlighter;
use crate::util::human_size;
use crate::walker::FileMeta;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use std::ops::Range;
use std::time::{Duration, SystemTime};

pub(super) fn spans_with_styles(
    shown: &str,
    positions: &[u32],
    plain: Style,
    highlight: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_highlighted = false;
    let mut next = positions.iter().peekable();
    for (i, ch) in shown.chars().enumerate() {
        while next.next_if(|&&p| (p as usize) < i).is_some() {}
        let highlighted = next.peek().is_some_and(|&&p| p as usize == i);
        if highlighted != run_highlighted && !run.is_empty() {
            let text = std::mem::take(&mut run);
            spans.push(Span::styled(
                text,
                if run_highlighted { highlight } else { plain },
            ));
        }
        run_highlighted = highlighted;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(
            run,
            if run_highlighted { highlight } else { plain },
        ));
    }
    spans
}

/// The first regex match in `line`, split into plain spans around an
/// accent-styled match span.
pub(super) fn highlight_first_match(
    line: &str,
    re: &regex::Regex,
    accent: Style,
) -> Vec<Span<'static>> {
    let Some(m) = re.find(line) else {
        return vec![Span::raw(line.to_string())];
    };
    let (start, end) = (m.start(), m.end());
    let mut spans = Vec::new();
    if start > 0 {
        spans.push(Span::raw(line[..start].to_string()));
    }
    spans.push(Span::styled(line[start..end].to_string(), accent));
    if end < line.len() {
        spans.push(Span::raw(line[end..].to_string()));
    }
    spans
}

/// (label, color) for the little kind badge in front of a row; `badges` is
/// the theme's [image, video/audio, doc, code, archive, other] palette and
/// `accent` colors directory badges.
pub(super) fn badge_for(path: &str, badges: [Color; 6], accent: Color) -> (String, Color) {
    if path.ends_with('/') {
        return ("DIR".to_string(), accent);
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext.is_empty() {
        return ("FILE".to_string(), badges[5]);
    }
    let label: String = ext.chars().take(4).collect::<String>().to_uppercase();
    let color = match crate::filters::kind_for_ext(ext) {
        Some("image") => badges[0],
        Some("video") | Some("audio") => badges[1],
        Some("doc") => badges[2],
        Some("code") => badges[3],
        Some("archive") => badges[4],
        Some("app") => accent,
        _ => badges[5],
    };
    (label, color)
}

/// The badge span (`" PDF "` on its kind color) plus the gap space after it,
/// and the total visual width of both (used to indent second lines).
pub(super) fn badge_spans(
    path: &str,
    badges: [Color; 6],
    accent: Color,
) -> (Vec<Span<'static>>, usize) {
    let (label, color) = badge_for(path, badges, accent);
    let label_span = Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    );
    let width = label_span.width() + Span::raw(" ").width();
    (vec![label_span, Span::raw(" ")], width)
}

/// Nerd-font glyph for a path's kind; the same buckets as [`badge_for`].
pub(super) fn icon_glyph(path: &str) -> &'static str {
    if path.ends_with('/') {
        return "\u{f07b}"; // folder
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match crate::filters::kind_for_ext(ext) {
        Some("image") => "\u{f1c5}",
        Some("video") => "\u{f1c8}",
        Some("audio") => "\u{f1c7}",
        Some("doc") => "\u{f15c}",
        Some("code") => "\u{f121}",
        Some("archive") => "\u{f1c6}",
        Some("app") => "\u{f135}", // launch
        _ => "\u{f15b}",           // plain file
    }
}

/// The icon span ("glyph + gap", on the badge's kind color) plus its total
/// visual width; empty when icons are off, keeping rendering identical.
pub(super) fn icon_spans(
    path: &str,
    badges: [Color; 6],
    accent: Color,
    enabled: bool,
) -> (Vec<Span<'static>>, usize) {
    if !enabled {
        return (Vec::new(), 0);
    }
    let glyph = icon_glyph(path);
    let (_, color) = badge_for(path, badges, accent);
    let content = format!("{glyph} ");
    let width = Span::raw(content.clone()).width();
    (
        vec![Span::styled(content, Style::default().fg(color))],
        width,
    )
}

/// Spaces to push the right column flush against the row's right edge; None
/// when there is no room for even one gap (callers then drop the column).
pub(super) fn right_pad(left: usize, right: usize, inner_width: usize) -> Option<usize> {
    let pad = inner_width.saturating_sub(left + right);
    (pad >= 1).then_some(pad)
}

/// "5m ago" for a row's mtime; None when the meta is missing or bogus
/// (mtime <= 0).
pub(super) fn row_age(meta: Option<FileMeta>) -> Option<String> {
    meta.filter(|m| m.mtime > 0)
        .map(|m| human_age(SystemTime::UNIX_EPOCH + Duration::from_secs(m.mtime as u64)))
}

/// Filled/empty cells of a 5-cell score bar for a 0..=1 similarity score.
/// Returns the number filled and the bar glyph string.
pub(super) fn score_bar(s: f32) -> (usize, String) {
    let filled = ((s.clamp(0.0, 1.0) * 5.0).round() as usize).min(5);
    (filled, "▰".repeat(filled) + &"▱".repeat(5 - filled))
}

/// Right-aligned score readout for a semantic row: a styled 5-cell bar plus
/// the percent, returning its cell width and the spans to render.
pub(super) fn score_readout(s: f32, accent: Color, dim: Style) -> (usize, Vec<Span<'static>>) {
    // build the two segments directly: the bar glyphs are 3 bytes each, so
    // splitting the joined string at the fill COUNT would land mid-char
    let (filled, _) = score_bar(s);
    let fill = "\u{25b0}".repeat(filled);
    let rest = "\u{25b1}".repeat(5 - filled);
    let pct = format!(" {:.0}%", s * 100.0);
    let width = Span::raw(fill.clone()).width()
        + Span::raw(rest.clone()).width()
        + Span::raw(pct.clone()).width();
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !fill.is_empty() {
        spans.push(Span::styled(fill, Style::default().fg(accent)));
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest, dim));
    }
    spans.push(Span::styled(pct, dim));
    (width, spans)
}

/// Gutter marker for a marked row: an accent-colored bar in front of the
/// badge. Filter mode never shows marks (rows are arbitrary stdin lines,
/// not files). Returns the spans and their cell width.
pub(super) fn mark_spans(app: &App, path: &str) -> (Vec<Span<'static>>, usize) {
    if !app.marking_enabled() || !app.marks.contains(path) {
        return (Vec::new(), 0);
    }
    let span = Span::styled(
        "▌ ",
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let width = span.width();
    (vec![span], width)
}

/// Compute the same whole-item viewport as a List with no scroll padding,
/// without first formatting every result. A jump goes straight to the selected
/// row, so even wrapping from the first row to the last only examines a screenful.
fn viewport_slots(
    slots: &[(Slot, u16)],
    offset: usize,
    selected: usize,
    height: u16,
) -> Range<usize> {
    if slots.is_empty() {
        return 0..0;
    }
    let height = usize::from(height);
    let selected = selected.min(slots.len() - 1);
    let mut start = offset.min(slots.len() - 1).min(selected);
    let mut end = start;
    let mut used = 0;
    while end < slots.len() && used + usize::from(slots[end].1) <= height {
        used += usize::from(slots[end].1);
        end += 1;
    }
    if selected >= end {
        end = selected + 1;
        start = end;
        used = 0;
        while start > 0 && used + usize::from(slots[start - 1].1) <= height {
            start -= 1;
            used += usize::from(slots[start].1);
        }
        // A two-line row cannot fit in a one-line pane. Keep its offset,
        // but render nothing rather than spilling into neighboring widgets.
        if start > selected {
            return selected..selected;
        }
    }
    start..end
}

pub(super) fn draw_results(frame: &mut Frame, app: &mut App, area: Rect) {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let match_color = app.theme.match_fg.unwrap_or(app.theme.accent);
    let accent = Style::default()
        .fg(match_color)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.theme.dim);
    let badges = app.theme.badges;
    let dir_color = app.theme.accent;
    // take the cached highlighter out so the results borrow doesn't block
    // rebuilding it; it goes back on App before the frame renders
    let mut highlighter = std::mem::take(&mut app.highlights.fuzzy);
    if matches!(app.engine.mode(), Mode::Fuzzy) && !app.editor.input.is_empty() {
        if app.highlights.input != app.editor.input {
            app.highlights.input = app.editor.input.clone();
            highlighter = Some(Highlighter::new(&app.editor.input));
        }
    } else {
        app.highlights.input.clear();
        highlighter = None;
    }
    // same take/rebuild cache for the first-match content highlight
    let mut content_re = std::mem::take(&mut app.highlights.content);
    if matches!(app.engine.mode(), Mode::Content) && !app.editor.input.is_empty() {
        if app.highlights.content_input != app.editor.input {
            app.highlights.content_input = app.editor.input.clone();
            let (_, pattern) = crate::engine::parse_query(&app.editor.input, app.regex_mode);
            content_re = regex::RegexBuilder::new(&pattern)
                .case_insensitive(!pattern.chars().any(char::is_uppercase))
                .build()
                .ok();
        }
    } else {
        app.highlights.content_input.clear();
        content_re = None;
    }
    let block = themed_block("results", &app.theme);
    let inner = block.inner(area);
    let inner_width = inner.width as usize;
    let name_plain = Style::default().add_modifier(Modifier::BOLD);
    let parent_hl = Style::default().fg(match_color).add_modifier(Modifier::DIM);
    let rows = app.engine.results();
    let visible = app.visible_len();
    let strong = app.engine.strong_count();
    let has_rows = !rows.is_empty();
    let hidden = rows.len().saturating_sub(visible);
    let opened = if app.editor.input.is_empty() && app.engine.mode() == Mode::Fuzzy {
        rows.iter().take_while(|r| r.recent_open).count()
    } else {
        0
    };
    let row_height = if app.engine.is_filter()
        || app.engine.mode() == Mode::Calc
        || app.density == Density::Compact
    {
        1
    } else {
        2
    };
    // Keep the cheap global slot map for scrolling and mouse hit testing.
    // Expensive text construction and highlighting below only visit the viewport.
    let mut slots = Vec::with_capacity(visible + 3);
    let mut headers = Vec::with_capacity(3);
    let mut display_selected = 0;
    for i in 0..visible {
        let section = if opened > 0 && i == 0 {
            Some("─ RECENT OPENS ────────")
        } else if opened > 0 && i == opened {
            Some("─ RECENTLY MODIFIED ────────")
        } else {
            None
        };
        if let Some(label) = section {
            headers.push((slots.len(), label));
            slots.push((Slot::Header, 1));
        }
        if app.show_weak && strong < rows.len() && i == strong {
            headers.push((slots.len(), "─ WEAKER MATCHES ─"));
            slots.push((Slot::Header, 1));
        }
        if i == app.selected.min(visible.saturating_sub(1)) {
            display_selected = slots.len();
        }
        slots.push((Slot::Row(i), row_height));
    }
    if hidden > 0 {
        slots.push((Slot::Fold, 1));
    }
    let viewport = viewport_slots(
        &slots,
        app.list_state.offset(),
        display_selected,
        if inner.is_empty() { 0 } else { inner.height },
    );
    let items: Vec<ListItem> = slots[viewport.clone()]
        .iter()
        .enumerate()
        .map(|(local, (slot, _))| {
            let r = match slot {
                Slot::Row(i) => &rows[*i],
                Slot::Header => {
                    let label = headers
                        .iter()
                        .find(|(index, _)| *index == viewport.start + local)
                        .map_or("", |(_, label)| *label);
                    return ListItem::new(Span::styled(
                        label,
                        Style::default().fg(app.theme.section.unwrap_or(app.theme.dim)),
                    ));
                }
                Slot::Fold => {
                    let shortcut = app
                        .keymap
                        .shortcut(crate::keymap::Action::FoldToggle)
                        .map_or_else(String::new, |key| format!(" · {key} show"));
                    return ListItem::new(Span::styled(
                        format!("▸ {hidden} weaker matches hidden{shortcut}"),
                        dim,
                    ));
                }
            };
            if app.engine.mode() == crate::engine::Mode::Calc {
                // the calculator's single row: dim "expr =" then the
                // result in bold accent
                let mut spans = Vec::new();
                if let Some(expr) = &r.line {
                    spans.push(Span::styled(format!("{expr} "), dim));
                }
                spans.push(Span::styled(
                    r.path.clone(),
                    accent.add_modifier(Modifier::BOLD),
                ));
                return ListItem::new(Line::from(spans));
            }
            if app.engine.is_filter() {
                // filter mode: one plain line per row — the raw line text
                // with fuzzy-match highlights; no home shortening, badge,
                // name/parent split, or size/age columns
                let positions: Vec<u32> = match highlighter.as_mut() {
                    Some(hl) => hl.positions(&r.path),
                    None => Vec::new(),
                };
                return ListItem::new(Line::from(spans_with_styles(
                    &r.path,
                    &positions,
                    Style::default(),
                    accent,
                )));
            }
            let (shown, trimmed_chars) = shorten_home_with(&r.path, home.as_deref());
            // split the shown path at the name boundary: the final component
            // (trailing '/' kept for directories) is the hero, the rest is
            // the dim parent
            let (parent, name) = display_path_parts(&shown);
            let name_chars = name.chars().count();
            let name_width = Span::raw(name.clone()).width();
            let parent_chars = shown.chars().count() - name_chars;
            match (r.line_number, &r.line) {
                (Some(n), Some(line)) => {
                    let (mark, mark_width) = mark_spans(app, &r.path);
                    let (badge, badge_width) = badge_spans(&r.path, badges, dir_color);
                    let (icon, icon_width) = icon_spans(&r.path, badges, dir_color, app.icons);
                    let badge: Vec<Span<'static>> = mark.into_iter().chain(badge).collect();
                    let badge_width = badge_width + mark_width;
                    let colon = format!(":{n}");
                    let age = row_age(r.meta);
                    match app.density {
                        Density::Comfy => ListItem::new(Text::from(vec![
                            // badge, bold name, dim :n, age flush right
                            Line::from({
                                let mut spans = badge;
                                spans.extend(icon);
                                spans.push(Span::styled(name.clone(), name_plain));
                                spans.push(Span::styled(colon.clone(), dim));
                                // semantic rows show a score bar; others call out age
                                let right = match r.score {
                                    Some(s) => Some(score_readout(s, app.theme.accent, dim)),
                                    None => age.as_ref().map(|a| {
                                        (a.chars().count(), vec![Span::styled(a.clone(), dim)])
                                    }),
                                };
                                if let Some((right_width, right_spans)) = right {
                                    let left = badge_width
                                        + icon_width
                                        + name_width
                                        + Span::raw(colon.clone()).width();
                                    if let Some(pad) = right_pad(left, right_width, inner_width) {
                                        spans.push(Span::raw(" ".repeat(pad)));
                                        spans.extend(right_spans);
                                    }
                                }
                                spans
                            }),
                            // indented matched line text
                            Line::from({
                                let mut spans =
                                    vec![Span::raw(" ".repeat(badge_width + icon_width))];
                                match &content_re {
                                    Some(re) => {
                                        spans.extend(highlight_first_match(line, re, accent))
                                    }
                                    None => spans.push(Span::raw(line.clone())),
                                }
                                spans
                            }),
                        ])),
                        Density::Compact => ListItem::new(Line::from({
                            let mut spans = badge;
                            spans.extend(icon);
                            spans.push(Span::styled(name.clone(), name_plain));
                            spans.push(Span::styled(format!("{colon} "), dim));
                            match &content_re {
                                Some(re) => spans.extend(highlight_first_match(line, re, accent)),
                                None => spans.push(Span::raw(line.clone())),
                            }
                            // score rows right-align the bar after the snippet
                            if let Some(s) = r.score {
                                let (right_width, right_spans) =
                                    score_readout(s, app.theme.accent, dim);
                                let left = badge_width
                                    + icon_width
                                    + name_width
                                    + Span::raw(colon.clone()).width()
                                    + 1
                                    + Span::raw(line.clone()).width();
                                if let Some(pad) = right_pad(left, right_width, inner_width) {
                                    spans.push(Span::raw(" ".repeat(pad)));
                                    spans.extend(right_spans);
                                }
                            }
                            spans
                        })),
                    }
                }
                _ => {
                    let (mark, mark_width) = mark_spans(app, &r.path);
                    let (badge, badge_width) = badge_spans(&r.path, badges, dir_color);
                    let (icon, icon_width) = icon_spans(&r.path, badges, dir_color, app.icons);
                    let badge: Vec<Span<'static>> = mark.into_iter().chain(badge).collect();
                    let badge_width = badge_width + mark_width;
                    // positions refer to the full path; shift them onto the
                    // `~`-shortened string, then partition them at the name
                    // boundary (the parent starts at index 0 of `shown`)
                    let (in_name, in_parent): (Vec<u32>, Vec<u32>) = match highlighter.as_mut() {
                        Some(hl) => {
                            let replacement_chars = if trimmed_chars > 0 {
                                shown.chars().count() - (r.path.chars().count() - trimmed_chars)
                            } else {
                                0
                            };
                            let positions: Vec<u32> = hl
                                .positions(&r.path)
                                .into_iter()
                                .filter(|&p| p as usize >= trimmed_chars)
                                .map(|p| (p as usize - trimmed_chars + replacement_chars) as u32)
                                .collect();
                            positions
                                .into_iter()
                                .partition(|&p| p as usize >= parent_chars)
                        }
                        None => (Vec::new(), Vec::new()),
                    };
                    let name_positions: Vec<u32> = in_name
                        .into_iter()
                        .map(|p| p - parent_chars as u32)
                        .collect();
                    let name_spans = spans_with_styles(&name, &name_positions, name_plain, accent);
                    match app.density {
                        Density::Comfy => {
                            // line 1: badge, bold name (highlights), age flush right
                            let age = row_age(r.meta);
                            let mut line1 = badge;
                            line1.extend(icon);
                            line1.extend(name_spans);
                            if let Some(age) = &age {
                                let left = badge_width + icon_width + name_width;
                                if let Some(pad) =
                                    right_pad(left, Span::raw(age.clone()).width(), inner_width)
                                {
                                    line1.push(Span::raw(" ".repeat(pad)));
                                    line1.push(Span::styled(age.clone(), dim));
                                }
                            }
                            // line 2: indented dim parent (+ size when known)
                            let mut line2 = vec![Span::raw(" ".repeat(badge_width + icon_width))];
                            line2.extend(spans_with_styles(&parent, &in_parent, dim, parent_hl));
                            if let Some(size) = r
                                .meta
                                .filter(|_| !r.path.ends_with('/'))
                                .map(|m| format!(" · {}", human_size(m.size)))
                            {
                                line2.push(Span::raw(size));
                            }
                            ListItem::new(Text::from(vec![Line::from(line1), Line::from(line2)]))
                        }
                        Density::Compact => {
                            // badge, bold name, dim " — parent", size/age right
                            let right = r.meta.map(|m| {
                                let size = human_size(m.size);
                                match row_age(Some(m)) {
                                    Some(age) => format!("{size} · {age}"),
                                    None => size,
                                }
                            });
                            let mut line1 = badge;
                            line1.extend(icon);
                            line1.extend(name_spans);
                            line1.push(Span::styled(" — ".to_string(), dim));
                            line1.extend(spans_with_styles(&parent, &in_parent, dim, parent_hl));
                            if let Some(right) = &right {
                                let left = badge_width
                                    + icon_width
                                    + name_width
                                    + 3
                                    + Span::raw(parent.clone()).width();
                                if let Some(pad) =
                                    right_pad(left, Span::raw(right.clone()).width(), inner_width)
                                {
                                    line1.push(Span::raw(" ".repeat(pad)));
                                    line1.push(Span::styled(right.clone(), dim));
                                }
                            }
                            ListItem::new(Line::from(line1))
                        }
                    }
                }
            }
        })
        .collect();
    app.highlights.fuzzy = highlighter;
    app.highlights.content = content_re;
    app.hit_test.results_area = inner;
    let list = List::new(items)
        .block(block)
        .highlight_style(selection_style(&app.theme));
    let mut local_state = ListState::default();
    if viewport.contains(&display_selected) {
        local_state.select(Some(display_selected - viewport.start));
    }
    frame.render_stateful_widget(list, area, &mut local_state);
    // List only sees the viewport slice; retain global coordinates for the
    // next draw and click_results, including decorative rows before it.
    if !inner.is_empty() {
        app.list_state
            .select((!slots.is_empty()).then_some(display_selected));
        *app.list_state.offset_mut() = viewport.start;
    }
    app.hit_test.slots = slots;
    if !has_rows {
        draw_empty_state(frame, app, app.hit_test.results_area, &app.engine.status());
    }
}

fn empty_state_text(app: &App, status: &EngineStatus) -> (String, String) {
    if status.indexing {
        return (
            "Indexing…".into(),
            "Results will appear as files are discovered.".into(),
        );
    }
    if status.searching {
        return (
            "Searching…".into(),
            "Waiting for results from the current query.".into(),
        );
    }
    if let Some(error) = &status.error {
        return ("Search needs attention".into(), error.clone());
    }
    let mode = app.engine.mode();
    let query = if app.engine.is_filter() {
        app.editor.input.clone()
    } else {
        crate::engine::parse_query(&app.editor.input, app.regex_mode).1
    };
    let (filters, pattern) = crate::filters::parse(&query, crate::util::unix_now());
    if pattern.trim().is_empty() {
        match mode {
            Mode::Content => {
                return (
                    "Search inside files".into(),
                    "Type a pattern after >, for example > ext:md TODO.".into(),
                );
            }
            Mode::Semantic => {
                return (
                    "Search by meaning".into(),
                    "Describe a document after ?, for example ? project notes.".into(),
                );
            }
            Mode::Calc => {
                return (
                    "Calculate".into(),
                    "Type an expression after =, for example = 2*(3+4).".into(),
                );
            }
            _ => {}
        }
    }
    if mode == Mode::Calc {
        return (
            "No result".into(),
            "Check the expression and its parentheses.".into(),
        );
    }
    if app.editor.input.is_empty() && app.engine.is_filter() && status.indexed == 0 {
        return (
            "No input records".into(),
            "Pipe some lines into fsearch to filter them.".into(),
        );
    }
    if app.editor.input.is_empty() && !app.engine.is_filter() {
        return if status.indexed == 0 {
            (
                "No indexed files".into(),
                "Check roots and excludes with fsearch --config, then run fsearch --reindex."
                    .into(),
            )
        } else {
            (
                "No recent files to show".into(),
                "Type a name or path to search the index.".into(),
            )
        };
    }
    let mut hint = if !filters.is_empty() {
        "Try removing a filter or broadening the query.".to_string()
    } else if mode == Mode::Regex {
        match app.keymap.shortcut(crate::keymap::Action::RegexToggle) {
            Some(key) => format!("Try a simpler pattern, or {key} for fuzzy search."),
            None => "Try a simpler regular expression.".into(),
        }
    } else {
        "Try a shorter or different query.".to_string()
    };
    if let Some(key) = app.keymap.shortcut(crate::keymap::Action::ClearQuery) {
        hint.push_str(&format!(" {key} clears the query."));
    }
    ("No matches".into(), hint)
}

fn draw_empty_state(frame: &mut Frame, app: &App, area: Rect, status: &EngineStatus) {
    if area.is_empty() {
        return;
    }
    let (title, hint) = empty_state_text(app, status);
    let text = Text::from(vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(hint, Style::default().fg(app.theme.dim))),
    ]);
    // Keep all rendering inside the results pane, including tiny terminals.
    let offset = area.height.saturating_sub(4) / 2;
    let rect = Rect {
        y: area.y + offset,
        height: area.height - offset,
        ..area
    };
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        rect,
    );
}

/// `path` with a home-directory prefix shortened to `~`; unchanged otherwise.
pub(super) fn shorten_home(path: &str) -> String {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    shorten_home_with(path, home.as_deref()).0
}

/// Require a complete component, not a sibling sharing the home's prefix.
/// The removed character count maps fuzzy highlights onto the display path.
fn shorten_home_with(path: &str, home: Option<&str>) -> (String, usize) {
    // A root home replaces the leading slash with `~/`, not `~`.
    if home == Some("/") && path.starts_with('/') {
        return (format!("~{path}"), 1);
    }
    if let Some(home) = home
        .map(|h| h.trim_end_matches('/'))
        .filter(|h| !h.is_empty())
        && let Some(rest) = path.strip_prefix(home)
        && (rest.is_empty() || rest.starts_with('/'))
    {
        let shown = if rest.is_empty() {
            "~/".into()
        } else {
            format!("~{rest}")
        };
        return (shown, home.chars().count());
    }
    (path.to_string(), 0)
}

/// Split the *displayed* path: the home itself is `~/`, not its original
/// basename. Both sides come from the same string, so UTF-8 slicing is safe.
pub(super) fn display_path_parts(shown: &str) -> (String, String) {
    let name = path_name(shown);
    (shown[..shown.len() - name.len()].to_string(), name)
}

/// Final path component, trailing '/' kept for directories.
pub(super) fn path_name(path: &str) -> String {
    let stem = path.trim_end_matches('/');
    let last = stem.rsplit('/').next().unwrap_or("");
    if path.ends_with('/') {
        format!("{last}/")
    } else {
        last.to_string()
    }
}

/// Metadata for the selected row: its own index meta, else the status-line
/// stat cache when that covers the same path.
pub(super) fn kind_label(path: &str) -> String {
    if path.ends_with('/') {
        "DIR".to_string()
    } else {
        std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("FILE")
            .to_uppercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn viewport_matches_full_list_scrolling_and_pixels() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::StatefulWidget;

        for heights in [
            vec![],
            vec![1],
            vec![2],
            vec![1; 12],
            vec![2; 12],
            vec![1, 2, 2, 1, 2, 2, 1],
        ] {
            let slots: Vec<_> = heights
                .iter()
                .enumerate()
                .map(|(i, h)| (Slot::Row(i), *h))
                .collect();
            let items: Vec<_> = heights
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    ListItem::new(Text::from(
                        (0..*h)
                            .map(|line| Line::raw(format!("{i}:{line}")))
                            .collect::<Vec<_>>(),
                    ))
                })
                .collect();
            let style = Style::default().bg(Color::Blue);
            for height in 1..10 {
                let area = Rect::new(0, 0, 12, height);
                for offset in 0..heights.len() + 2 {
                    for selected in 0..heights.len() + 2 {
                        let mut expected = Buffer::empty(area);
                        let mut state = ListState::default()
                            .with_offset(offset)
                            .with_selected(Some(selected));
                        StatefulWidget::render(
                            List::new(items.clone()).highlight_style(style),
                            area,
                            &mut expected,
                            &mut state,
                        );
                        let range = viewport_slots(&slots, offset, selected, height);
                        assert_eq!(
                            range.start,
                            state.offset(),
                            "heights={heights:?}, offset={offset}, selected={selected}, height={height}"
                        );
                        let mut actual = Buffer::empty(area);
                        let selected = selected.min(heights.len().saturating_sub(1));
                        let local_selection =
                            range.contains(&selected).then(|| selected - range.start);
                        StatefulWidget::render(
                            List::new(items[range.clone()].to_vec()).highlight_style(style),
                            area,
                            &mut actual,
                            &mut ListState::default().with_selected(local_selection),
                        );
                        assert_eq!(actual, expected);
                        assert!(range.len() <= height as usize);
                    }
                }
            }
        }
    }

    #[test]
    fn viewport_work_stays_bounded_when_jumping_through_many_rows() {
        let slots: Vec<_> = (0..100_000).map(|i| (Slot::Row(i), 2)).collect();
        assert_eq!(viewport_slots(&slots, 0, 99_999, 21), 99_990..100_000);
        assert_eq!(viewport_slots(&slots, 99_990, 0, 21), 0..10);
        assert_eq!(viewport_slots(&slots, 50_000, 50_005, 21), 50_000..50_010);
        assert_eq!(viewport_slots(&slots, 0, 99_999, 1), 99_999..99_999);
        assert_eq!(viewport_slots(&slots, 0, 99_999, 0), 99_999..99_999);
        assert_eq!(viewport_slots(&slots, 0, 0, u16::MAX), 0..32_767);
    }

    #[test]
    fn home_shortening_is_component_aware_and_splits_displayed_utf8() {
        for home in ["/Users/al", "/Users/名字", "/home/é"] {
            for path in [home.to_string(), format!("{home}/")] {
                let (shown, removed) = shorten_home_with(&path, Some(home));
                assert_eq!(shown, "~/");
                assert_eq!(removed, home.chars().count());
                assert_eq!(display_path_parts(&shown), ("".into(), "~/".into()));
            }
            for suffix in ["bert/file.txt", "é/名字/", "-backup/é"] {
                let path = format!("{home}{suffix}");
                assert_eq!(shorten_home_with(&path, Some(home)), (path.clone(), 0));
                let (parent, name) = display_path_parts(&path);
                assert_eq!(format!("{parent}{name}"), path);
            }
            let (shown, _) = shorten_home_with(&format!("{home}/目录/é.txt"), Some(home));
            assert_eq!(
                display_path_parts(&shown),
                ("~/目录/".into(), "é.txt".into())
            );
        }
        assert_eq!(display_path_parts("/"), ("".into(), "/".into()));
        assert_eq!(shorten_home_with("/file", Some("/")), ("~/file".into(), 1));
        assert_eq!(shorten_home_with("/", Some("/")), ("~/".into(), 1));
    }

    #[test]
    fn empty_state_distinguishes_busy_errors_and_no_matches() {
        let mut app = App::new(Engine::from_lines(Vec::new()));
        app.editor.input = "missing".into();
        for (status, title) in [
            (
                EngineStatus {
                    indexing: true,
                    ..Default::default()
                },
                "Indexing…",
            ),
            (
                EngineStatus {
                    searching: true,
                    ..Default::default()
                },
                "Searching…",
            ),
            (
                EngineStatus {
                    error: Some("invalid pattern".into()),
                    ..Default::default()
                },
                "Search needs attention",
            ),
            (EngineStatus::default(), "No matches"),
        ] {
            assert_eq!(empty_state_text(&app, &status).0, title);
        }
        app.editor.clear();
        assert_eq!(
            empty_state_text(&app, &EngineStatus::default()).0,
            "No input records"
        );
    }

    #[test]
    fn empty_state_hints_use_configured_keys_and_filters() {
        let mut app = App::new(Engine::from_lines(Vec::new()));
        app.keymap = crate::keymap::Keymap::from_config(
            &[
                ("clear_query".into(), vec!["alt-u".into()]),
                ("regex_toggle".into(), vec!["alt-r".into()]),
            ]
            .into(),
        );
        app.editor.input = "ext:md missing".into();
        let (_, hint) = empty_state_text(&app, &EngineStatus::default());
        assert!(hint.contains("removing a filter"));
        assert!(hint.contains("alt-u"));
        assert!(!hint.contains("ctrl-u"));
        app.editor.input = "missing".into();
        app.engine.set_query("missing", true);
        let (_, hint) = empty_state_text(&app, &EngineStatus::default());
        assert!(hint.contains("alt-r"));
    }

    #[test]
    fn empty_state_whitespace_regex_does_not_claim_stdin_is_empty() {
        let mut app = App::new(Engine::from_lines(vec!["alpha".into()]));
        app.editor.input = " ".into();
        app.engine.set_query(" ", true);
        let status = EngineStatus {
            indexed: 1,
            ..Default::default()
        };
        let (title, hint) = empty_state_text(&app, &status);
        assert_eq!(title, "No matches");
        assert!(hint.contains("fuzzy search"));
        assert!(!hint.contains("Pipe"));
    }

    #[test]
    fn empty_state_explains_mode_prefixes_and_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::Config {
            roots: vec![dir.path().to_path_buf()],
            index_apps: false,
            remember_history: false,
            unified: false,
            ..Default::default()
        };
        let mut app = App::new(Engine::new(
            config,
            dir.path().join("index"),
            dir.path().join("history"),
        ));
        for (input, title) in [
            ("> ext:md", "Search inside files"),
            ("?", "Search by meaning"),
            ("=", "Calculate"),
            ("= 1+", "No result"),
            ("", "No indexed files"),
        ] {
            app.editor.input = input.into();
            app.engine.set_query(input, false);
            assert_eq!(empty_state_text(&app, &EngineStatus::default()).0, title);
        }
    }

    #[test]
    fn empty_state_never_overwrites_neighboring_panes() {
        let app = App::new(Engine::from_lines(Vec::new()));
        for (width, height) in [(0, 0), (1, 1), (2, 3), (8, 4), (32, 10)] {
            let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
            let area = Rect::new(2, 2, width, height);
            terminal
                .draw(|frame| {
                    draw_empty_state(frame, &app, area, &EngineStatus::default());
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..16 {
                for x in 0..40 {
                    if !area.contains((x, y).into()) {
                        assert_eq!(buffer[(x, y)].symbol(), " ", "outside {area:?} at {x},{y}");
                    }
                }
            }
        }
    }
}
