use super::*;

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
    if app.engine.results().get(app.selected).is_none() {
        return Vec::new();
    }
    let open_label = if app.engine.mode() == Mode::Calc {
        "copy result"
    } else if app.engine.is_filter() || app.ui_mode == UiMode::Pick {
        "choose"
    } else {
        "open"
    };
    let mut actions = vec![(crate::keymap::Action::Open, open_label)];
    if app.engine.mode() != Mode::Calc {
        if !app.engine.is_filter() {
            actions.push((crate::keymap::Action::Reveal, "reveal"));
        }
        actions.push((crate::keymap::Action::CopyPath, "copy path"));
        if !app.engine.is_filter() {
            actions.push((crate::keymap::Action::QuickLook, "quick look"));
            actions.push((crate::keymap::Action::Menu, "actions"));
            actions.push((crate::keymap::Action::PreviewLayout, "preview"));
        }
    }
    actions
        .into_iter()
        .filter_map(|(action, label)| {
            app.keymap
                .shortcut(action)
                .map(|shortcut| (shortcut, label.to_string()))
        })
        .collect()
}

fn help_lines(items: &[(String, String)], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    if items.is_empty() || width == 0 {
        return Vec::new();
    }
    let width = width as usize;
    let mut rows: Vec<Vec<(String, String)>> = Vec::new();
    let mut row = Vec::new();
    let mut used = 1usize; // left padding
    for (key, label) in items {
        let label_width = if label.is_empty() {
            0
        } else {
            1 + label.chars().count()
        };
        let item_width = key.chars().count() + label_width;
        let separator = if row.is_empty() { 0 } else { 3 }; // " · "
        if !row.is_empty() && used + separator + item_width > width {
            rows.push(std::mem::take(&mut row));
            used = 1;
        }
        used += (if row.is_empty() { 0 } else { 3 }) + item_width;
        row.push((key.clone(), label.clone()));
    }
    if !row.is_empty() {
        rows.push(row);
    }

    let key_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(theme.dim);
    rows.into_iter()
        .map(|row| {
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
        })
        .collect()
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let query_items: Vec<(String, String)> = QUERY_HINTS
        .iter()
        .map(|(key, label)| ((*key).to_string(), (*label).to_string()))
        .collect();
    let query_help = if app.input.is_empty() {
        help_lines(&query_items, frame.area().width, &app.theme)
    } else {
        Vec::new()
    };
    let action_help = help_lines(&contextual_hints(app), frame.area().width, &app.theme);
    // without borders the search bar only needs a title row + the text row
    let input_height = if app.theme.borders == BorderKind::None {
        2
    } else {
        3
    };
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
            app.results_area = Rect::default();
            draw_preview(frame, app, body);
        }
        PreviewLayout::Hidden => {
            app.preview_area = Rect::default();
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
    if let Some(selected) = app.menu {
        draw_menu(frame, selected, body, &app.theme);
    }
}

/// Small actions popup anchored inside the body area.
pub(super) fn draw_menu(frame: &mut Frame, selected: usize, body: Rect, theme: &Theme) {
    let width = 24u16.min(body.width);
    let height = (App::MENU.len() as u16 + 2).min(body.height);
    let area = Rect {
        x: body.x + (body.width.saturating_sub(width)) / 2,
        y: body.y + (body.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, area);
    let items: Vec<ListItem> = App::MENU
        .iter()
        .map(|label| ListItem::new(format!(" {label}")))
        .collect();
    let list = List::new(items)
        .block(themed_block("actions", theme))
        .highlight_style(selection_style(theme));
    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
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

pub(super) fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
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
    let input = Paragraph::new(Line::from(query_spans(&app.input, app.theme.accent))).block(block);
    frame.render_widget(input, area);
    frame.set_cursor_position((
        inner.x + app.input[..app.input_cursor].chars().count() as u16,
        inner.y,
    ));
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
    let mut spans = vec![Span::raw(format!("{} indexed", s.indexed))];
    spans.push(Span::raw(format!(" · {} matches", s.matches)));
    // stat the selected path only when the selection changes; the cached
    // triple is reused while the same row stays selected
    if let Some(row) = app.engine.results().get(app.selected) {
        if app.status_path != row.path {
            app.status_path = row.path.clone();
            app.status_meta = std::fs::metadata(&row.path)
                .ok()
                .map(|meta| (meta.is_file(), meta.len(), meta.modified().ok()));
        }
        if let Some((is_file, len, modified)) = app.status_meta {
            if is_file {
                spans.push(Span::raw(format!(" · {}", human_size(len))));
            }
            if let Some(modified) = modified {
                spans.push(Span::raw(format!(" · {}", human_age(modified))));
            }
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
