use super::preview::draw_preview;
use super::rows::draw_results;
use super::{App, PreviewLayout, UiMode};
use crate::engine::Mode;
use crate::theme::{BorderKind, Theme};
use crate::util::human_size;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph};
use std::time::Duration;

const QUERY_HINTS: &[(&str, &str)] = &[
    (">", "grep in files"),
    ("?", "semantic"),
    ("=", "calc"),
    ("'word", "exact"),
    ("ext:pdf", ""),
    ("kind:image", ""),
    ("changed:7d", ""),
    ("larger:100mb", ""),
    ("dir:", "folders"),
    ("ctrl-r", "regex"),
    ("tab", "preview"),
];

/// Below this total height the wrapped query-hint rows are dropped so the
/// results list keeps its space.
const QUERY_HINTS_MIN_HEIGHT: u16 = 12;
/// Below this height even the contextual shortcut footer is dropped.
const ACTION_HINTS_MIN_HEIGHT: u16 = 8;

fn cap_hint_lines(lines: Vec<Line<'static>>, limit: usize) -> Vec<Line<'static>> {
    if lines.len() <= limit {
        return lines;
    }
    if limit == 0 {
        return Vec::new();
    }
    if limit == 1 {
        return vec![lines.last().cloned().expect("non-empty hint lines")];
    }
    let last = lines.last().cloned().expect("non-empty hint lines");
    let mut capped: Vec<Line<'static>> = lines.into_iter().take(limit - 1).collect();
    capped.push(last);
    capped
}

pub(super) fn themed_block(title: &str, theme: &Theme) -> Block<'static> {
    let mut block = Block::default()
        .borders(if theme.borders == BorderKind::None {
            Borders::NONE
        } else {
            Borders::ALL
        })
        .border_style(Style::default().fg(theme.border))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(theme.title),
        ));
    if theme.borders == BorderKind::Rounded {
        block = block.border_type(BorderType::Rounded);
    }
    block
}

/// Selection highlight for lists: the theme's background when it provides
/// one, otherwise today's REVERSED style.
pub(super) fn selection_style(theme: &Theme) -> Style {
    match theme.selection_bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default().add_modifier(Modifier::REVERSED),
    }
}

fn contextual_hints(app: &App) -> Vec<(String, String)> {
    let Some(selected) = app.visible_selected_row() else {
        return Vec::new();
    };
    let open_label = if app.engine.mode() == Mode::Calc {
        "copy result"
    } else if app.engine.is_filter() || app.ui_mode == UiMode::Pick {
        "choose"
    } else {
        "open"
    };
    let mut actions = vec![(crate::keymap::Action::Open, open_label.to_string())];
    if app.engine.mode() != Mode::Calc {
        if !app.engine.is_filter() {
            actions.push((crate::keymap::Action::Reveal, "reveal".to_string()));
        }
        actions.push((crate::keymap::Action::CopyPath, "copy path".to_string()));
        if !app.engine.is_filter() {
            actions.push((crate::keymap::Action::QuickLook, "quick look".to_string()));
            actions.push((crate::keymap::Action::Menu, "actions".to_string()));
            actions.push((crate::keymap::Action::PreviewLayout, "preview".to_string()));
            if app.marking_enabled() {
                let mark_label = if app.marks.contains(&selected.path) {
                    "unmark"
                } else {
                    "mark"
                };
                actions.push((crate::keymap::Action::ToggleMark, mark_label.to_string()));
                if !app.marks.is_empty() {
                    actions.push((
                        crate::keymap::Action::ClearMarks,
                        format!("clear ({} marked)", app.marks.len()),
                    ));
                }
            }
        }
    }
    actions.push((crate::keymap::Action::Help, "help".to_string()));
    actions
        .into_iter()
        .filter_map(|(action, label)| {
            app.keymap
                .shortcut(action)
                .map(|shortcut| (shortcut, label))
        })
        .collect()
}

/// Footer for the empty state: with nothing selected to act on, keep
/// quitting and clearing the query discoverable.
fn minimal_hints(app: &App) -> Vec<(String, String)> {
    [
        (crate::keymap::Action::Quit, "quit"),
        (crate::keymap::Action::ClearQuery, "clear"),
        (crate::keymap::Action::Help, "help"),
    ]
    .into_iter()
    .filter_map(|(action, label)| {
        app.keymap
            .shortcut(action)
            .map(|shortcut| (shortcut, label.to_string()))
    })
    .collect()
}

fn render_hint_row(row: Vec<(String, String)>, key_style: Style, dim: Style) -> Line<'static> {
    let mut spans = vec![Span::styled(" ", dim)];
    for (index, (key, label)) in row.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(key, key_style));
        if !label.is_empty() {
            spans.push(Span::styled(format!(" {label}"), dim));
        }
    }
    Line::from(spans)
}

fn help_lines(items: &[(String, String)], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    if items.is_empty() || width == 0 {
        return Vec::new();
    }
    let width = width as usize;
    let key_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(theme.dim);
    let mut lines = Vec::new();
    let mut row = Vec::new();
    let mut used = 1usize; // left padding
    for (key, label) in items {
        let label_width = if label.is_empty() {
            0
        } else {
            1 + Span::raw(label.clone()).width()
        };
        let item_width = Span::raw(key.clone()).width() + label_width;
        if item_width + 1 > width {
            if !row.is_empty() {
                lines.push(render_hint_row(std::mem::take(&mut row), key_style, dim));
                used = 1;
            }
            let text = if label.is_empty() {
                format!(" {key}")
            } else {
                format!(" {key} {label}")
            };
            lines.extend(wrapped_lines(&text, width, dim));
            continue;
        }
        let separator = if row.is_empty() { 0 } else { 3 }; // " · "
        if !row.is_empty() && used + separator + item_width > width {
            lines.push(render_hint_row(std::mem::take(&mut row), key_style, dim));
            used = 1;
        }
        used += (if row.is_empty() { 0 } else { 3 }) + item_width;
        row.push((key.clone(), label.clone()));
    }
    if !row.is_empty() {
        lines.push(render_hint_row(row, key_style, dim));
    }
    lines
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let screen = frame.area();
    let query_items: Vec<(String, String)> = QUERY_HINTS
        .iter()
        .map(|(key, label)| ((*key).to_string(), (*label).to_string()))
        .collect();
    // on short terminals hint rows are dropped before they starve the body
    let input_height = {
        let requested = if app.theme.borders == BorderKind::None {
            2
        } else {
            3
        };
        requested.min(screen.height.saturating_sub(2).max(1))
    };
    let query_help = if app.editor.input.is_empty() && screen.height >= QUERY_HINTS_MIN_HEIGHT {
        help_lines(&query_items, screen.width, &app.theme)
    } else {
        Vec::new()
    };
    let mut action_items = contextual_hints(app);
    if action_items.is_empty() && app.engine.results().is_empty() {
        // empty state: keep quit/clear discoverable without results
        action_items = minimal_hints(app);
    }
    let action_help = if screen.height >= ACTION_HINTS_MIN_HEIGHT {
        help_lines(&action_items, screen.width, &app.theme)
    } else {
        Vec::new()
    };
    // Keep one body row and the status row even when wrapped hints are tall.
    // The contextual footer wins over the optional query hints, and retaining
    // the last footer row keeps the help shortcut discoverable when clipped.
    let hint_capacity = screen.height.saturating_sub(input_height.saturating_add(2)) as usize;
    let action_help = cap_hint_lines(action_help, hint_capacity);
    let query_help = cap_hint_lines(query_help, hint_capacity.saturating_sub(action_help.len()));
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(input_height),
            Constraint::Length(query_help.len() as u16),
            Constraint::Min(1),
            Constraint::Length(action_help.len() as u16),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_input(frame, app, outer[0]);
    if !query_help.is_empty() {
        frame.render_widget(Paragraph::new(Text::from(query_help)), outer[1]);
    }
    let body = outer[2];
    let actions_area = outer[3];
    let status_area = outer[4];

    // the calculator's single result row has nothing to preview
    let layout = if app.engine.mode() == Mode::Calc {
        PreviewLayout::Hidden
    } else {
        app.preview_layout
    };
    match layout {
        PreviewLayout::Side => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(body);
            draw_results(frame, app, cols[0]);
            draw_preview(frame, app, cols[1]);
        }
        PreviewLayout::Full => {
            app.hit_test.results_area = Rect::default();
            draw_preview(frame, app, body);
        }
        PreviewLayout::Hidden => {
            app.hit_test.preview_area = Rect::default();
            draw_results(frame, app, body);
        }
    }

    if !action_help.is_empty() {
        frame.render_widget(Paragraph::new(Text::from(action_help)), actions_area);
    }
    draw_status(frame, app, status_area);
    // floating toast under the menu popup so the menu stays on top
    match &app.message {
        Some((_, at)) if at.elapsed() < Duration::from_millis(2500) => {
            draw_toast(frame, app, body);
        }
        Some(_) => app.message = None, // expired
        None => {}
    }
    if app.menu.is_some() {
        draw_menu(frame, app, body);
    }
    // the help overlay renders last so it floats above everything
    if app.help.open {
        draw_help(frame, app, screen);
    }
}

/// Groups shown in the help overlay. The actions inside each group come
/// from the live keymap, so remaps and newly bound actions stay in sync.
const HELP_GROUPS: &[&str] = &["navigation", "open & actions", "view", "query modes"];

/// Text editing keys are handled before the keymap and can never be bound;
/// the overlay says so instead of listing them as actions.
const HELP_EDITING_NOTE: &str = "editing is fixed: typing · backspace · ←/→ · ctrl-a/e/w/d";

fn wrapped_lines(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let width = width.max(1);
    text.chars()
        .collect::<Vec<_>>()
        .chunks(width)
        .map(|chunk| {
            let text: String = chunk.iter().copied().collect();
            Line::from(Span::styled(text, style))
        })
        .collect()
}

/// Content rows of the help overlay: grouped actions, each with every
/// configured binding joined by commas. Long rows wrap to the available modal
/// width so narrow terminals can still reveal every binding by scrolling.
fn help_overlay_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let heading = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.theme.dim);
    let configured = app.keymap.actions();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for title in HELP_GROUPS {
        let rows: Vec<(String, String)> = configured
            .iter()
            .filter(|action| action.help_group() == *title)
            .filter_map(|action| {
                let labels = app.keymap.labels(*action);
                (!labels.is_empty()).then(|| ((*action).label().to_string(), labels.join(", ")))
            })
            .collect();
        let Some(label_width) = rows.iter().map(|(label, _)| label.chars().count()).max() else {
            continue;
        };
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.extend(wrapped_lines(title, width, heading));
        for (label, bindings) in rows {
            let prefix = format!("  {label:<label_width$}   ");
            if prefix.chars().count() + bindings.chars().count() <= width {
                lines.push(Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(bindings, dim),
                ]));
            } else {
                // Keep the compact aligned form on a normal terminal, but
                // split the label and bindings into visible rows when space
                // is tight instead of letting Paragraph clip the keys.
                lines.extend(wrapped_lines(
                    &format!("  {label}"),
                    width,
                    Style::default(),
                ));
                lines.extend(wrapped_lines(&format!("    {bindings}"), width, dim));
            }
        }
    }
    lines.push(Line::from(""));
    lines.extend(wrapped_lines(HELP_EDITING_NOTE, width, dim));
    lines
}

/// Centered themed modal listing every action's configured bindings; records
/// its rect on `app.help` for hit testing.
pub(super) fn draw_help(frame: &mut Frame, app: &mut App, screen: Rect) {
    let bordered = app.theme.borders != BorderKind::None;
    let padding = if bordered { 4usize } else { 2 };
    // Find the natural width first, then wrap again if the screen is narrower.
    let natural_lines = help_overlay_lines(app, usize::MAX);
    let content_w = natural_lines.iter().map(Line::width).max().unwrap_or(0);
    let width = content_w
        .saturating_add(padding)
        .max(20)
        .min(screen.width.max(1) as usize) as u16;
    let inner_width = width.saturating_sub(if bordered { 2 } else { 0 }).max(1) as usize;
    let lines = help_overlay_lines(app, inner_width);
    let height = lines
        .len()
        .saturating_add(if bordered { 2 } else { 1 })
        .max(3)
        .min(screen.height.max(1) as usize) as u16;
    let area = Rect {
        x: screen.x + screen.width.saturating_sub(width) / 2,
        y: screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    };
    app.help.area = area;
    frame.render_widget(ratatui::widgets::Clear, area);
    let block = themed_block("help", &app.theme);
    let inner_rows = block.inner(area).height as usize;
    let max_scroll = lines.len().saturating_sub(inner_rows) as u16;
    app.help.scroll = app.help.scroll.min(max_scroll);
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .scroll((app.help.scroll, 0));
    frame.render_widget(paragraph, area);
}

/// Small actions popup anchored inside the body area; records its screen
/// rect on `app` so mouse clicks can hit-test the entries.
pub(super) fn draw_menu(frame: &mut Frame, app: &mut App, body: Rect) {
    let Some(selected) = app.menu else {
        return;
    };
    let entries = app.menu_entries();
    let selected = selected.min(entries.len().saturating_sub(1));
    app.menu = Some(selected);
    let max_label = entries
        .iter()
        .map(|entry| entry.label.chars().count())
        .max()
        .unwrap_or(0);
    let width = max_label.saturating_add(4).min(u16::MAX as usize) as u16;
    let width = width.min(body.width);
    let height = (entries.len() as u16 + 2).min(body.height);
    let area = Rect {
        x: body.x + (body.width.saturating_sub(width)) / 2,
        y: body.y + (body.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, area);
    let items: Vec<ListItem> = entries
        .iter()
        .map(|entry| ListItem::new(format!(" {}", entry.label)))
        .collect();
    let block = themed_block("actions", &app.theme);
    let inner = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(selection_style(&app.theme));
    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
    app.menu_area = area;
    app.menu_inner = inner;
    app.menu_offset = state.offset();
}

/// Preview for a directory entry: its children, folders first.
pub(super) fn query_spans(input: &str, accent: Color) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    // leading whitespace stays raw
    let trimmed = input.trim_start();
    let ws_len = input.len() - trimmed.len();
    if ws_len > 0 {
        spans.push(Span::raw(input[..ws_len].to_string()));
    }
    let mut rest = trimmed;
    // the '>' / '?' mode prefix lights up
    if let Some(p) = rest.chars().next()
        && (p == '>' || p == '?')
    {
        spans.push(Span::styled(
            p.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
        rest = &rest[p.len_utf8()..];
    }
    // walk whitespace-delimited tokens; whitespace runs stay raw
    let mut i = 0usize;
    while i < rest.len() {
        let ws_start = i;
        while let Some(c) = rest[i..].chars().next() {
            if c.is_whitespace() {
                i += c.len_utf8();
            } else {
                break;
            }
        }
        if i > ws_start {
            spans.push(Span::raw(rest[ws_start..i].to_string()));
        }
        let tok_start = i;
        while let Some(c) = rest[i..].chars().next() {
            if !c.is_whitespace() {
                i += c.len_utf8();
            } else {
                break;
            }
        }
        if i > tok_start {
            let token = &rest[tok_start..i];
            let (filters, remaining) = crate::filters::parse(token, 0);
            if remaining.is_empty() && !filters.is_empty() {
                spans.push(Span::styled(
                    token.to_string(),
                    Style::default().fg(Color::Yellow),
                ));
            } else {
                spans.push(Span::raw(token.to_string()));
            }
        }
    }
    spans
}

pub(super) fn draw_input(frame: &mut Frame, app: &mut App, area: Rect) {
    let mode = if app.engine.is_filter() {
        if app.regex_mode { "regex" } else { "filter" }
    } else {
        match (app.engine.mode(), app.regex_mode) {
            (Mode::Content, _) => "content",
            (Mode::Semantic, _) => "semantic",
            (Mode::Calc, _) => "calc",
            (_, true) => "regex",
            _ => "fuzzy",
        }
    };
    let block = themed_block(&format!("fsearch [{mode}]"), &app.theme);
    // the cursor sits in the block's inner rect, so borderless mode (which
    // keeps one row for the title and none for borders) stays in step
    let inner = block.inner(area);
    // readline-style horizontal scroll: once the edit cursor would leave
    // the visible row, shift the query left so the cursor stays one cell
    // inside the right edge
    let cursor_col = app.editor.input[..app.editor.input_cursor].chars().count();
    let width = inner.width as usize;
    if width == 0 {
        app.editor.input_scroll = 0;
    } else {
        if app.editor.input_scroll > cursor_col {
            app.editor.input_scroll = cursor_col;
        }
        if cursor_col >= app.editor.input_scroll + width {
            app.editor.input_scroll = cursor_col + 1 - width;
        }
        let max_skip = app.editor.input.chars().count().saturating_sub(width);
        app.editor.input_scroll = app.editor.input_scroll.min(max_skip);
    }
    let input = Paragraph::new(Line::from(query_spans(&app.editor.input, app.theme.accent)))
        .scroll((0, app.editor.input_scroll as u16))
        .block(block);
    frame.render_widget(input, area);
    let visible_col = (cursor_col - app.editor.input_scroll).min(width.saturating_sub(1));
    frame.set_cursor_position((inner.x + visible_col as u16, inner.y));
}

/// Splits `shown` into spans, styling the chars at `positions` (char
/// indices) with `highlight` and everything else with `plain`.
/// "just now", "5m ago", "3h ago", "12d ago", "2y ago"
pub(super) fn human_age(modified: std::time::SystemTime) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(modified)
        .map_or(0, |d| d.as_secs());
    match secs {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 24 * 3600 => format!("{}h ago", s / 3600),
        s if s < 365 * 24 * 3600 => format!("{}d ago", s / (24 * 3600)),
        s => format!("{}y ago", s / (365 * 24 * 3600)),
    }
}

pub(super) fn draw_status(frame: &mut Frame, app: &mut App, area: Rect) {
    let s = app.engine.status();
    let dim = Style::default().fg(app.theme.dim);
    let marked = if app.marking_enabled() && app.engine.mode() != Mode::Calc {
        let marked = app.visible_marked_count();
        (marked > 0).then_some(marked)
    } else {
        None
    };
    let mut spans = Vec::new();
    if let Some(marked) = marked {
        spans.push(Span::raw(format!("{marked} marked · ")));
    }
    spans.push(Span::raw(format!("{} indexed", s.indexed)));
    spans.push(Span::raw(format!(" · {} matches", s.matches)));
    // the stat cache is refreshed by the event loop (refresh_status), never
    // here: the draw pass stays free of filesystem access
    if app.visible_selected_row().is_some()
        && let Some((is_file, len, modified)) = app.status.meta
    {
        if is_file {
            spans.push(Span::raw(format!(" · {}", human_size(len))));
        }
        if let Some(modified) = modified {
            spans.push(Span::raw(format!(" · {}", human_age(modified))));
        }
    }
    if s.indexing {
        match s.walk {
            Some((done, Some(total))) if total > 0 => {
                // ... indexing ▰▰▰▱▱▱▱▱▱▱▱▱ 33% · 712,401 files
                let filled = gauge_cells(done, total, 12);
                let pct = ((done * 100) / total).min(100);
                let accent = Style::default().fg(app.theme.accent);
                let filled_bar: String = (0..filled).map(|_| '▰').collect();
                let empty_bar: String = (0..12 - filled).map(|_| '▱').collect();
                spans.push(Span::styled(" · indexing ", dim));
                spans.push(Span::styled(filled_bar, accent));
                spans.push(Span::styled(empty_bar, dim));
                spans.push(Span::styled(format!(" {pct}% · {done} files"), dim));
            }
            Some((done, None)) => {
                spans.push(Span::styled(format!(" · indexing… {done} files"), dim));
            }
            Some((_, Some(_))) | None => {
                spans.push(Span::styled(" · indexing…", dim));
            }
        }
    }
    if let Some(e) = &s.error {
        spans.push(Span::raw(format!(" · {e}")));
    }
    let status = Paragraph::new(Line::from(spans)).style(dim);
    frame.render_widget(status, area);
}

/// Cells filled in a `width`-cell progress bar; clamps at `width`.
pub(super) fn gauge_cells(done: usize, total: usize, width: usize) -> usize {
    (done * width / total).min(width)
}

/// `s` clipped to `max` chars, with a trailing "…" when clipped.
pub(super) fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let clipped: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{clipped}…")
    }
}

/// Floating confirmation toast, top-right of the body.
pub(super) fn draw_toast(frame: &mut Frame, app: &mut App, body: Rect) {
    let Some((text, _)) = &app.message else {
        return;
    };
    let (color, content) = if text.starts_with("error") {
        (Color::Red, text.clone())
    } else if text.starts_with("warning") {
        (Color::Yellow, text.clone())
    } else {
        (Color::Green, format!("✓ {text}"))
    };
    let width = (content.chars().count() + 4).min(body.width as usize) as u16;
    let height = 3u16.min(body.height);
    let area = Rect {
        x: body.x + body.width.saturating_sub(width),
        y: body.y,
        width,
        height,
    };
    let shown = clip_chars(&content, width.saturating_sub(2) as usize); // borders
    frame.render_widget(ratatui::widgets::Clear, area);
    let toast = Paragraph::new(Line::from(Span::styled(shown, Style::default().fg(color)))).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color)),
    );
    frame.render_widget(toast, area);
}
