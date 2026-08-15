use crate::actions;
use crate::engine::{Engine, Mode};
use crate::highlight::{self, Appearance};
use crate::images;
use crate::matcher::Highlighter;
use crate::theme::Theme;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal, backend::CrosstermBackend, crossterm::execute};
use ratatui_image::picker::Picker;
use ratatui_image::{StatefulImage, protocol::StatefulProtocol};
use std::time::Duration;

const PREVIEW_BYTES: usize = 64 * 1024;

/// Where the preview lives; Tab cycles. Full trades the results list for
/// a much larger canvas — images render at ~3x the cell resolution.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PreviewLayout {
    #[default]
    Side,
    Full,
    Hidden,
}

impl PreviewLayout {
    fn next(self) -> PreviewLayout {
        match self {
            PreviewLayout::Side => PreviewLayout::Full,
            PreviewLayout::Full => PreviewLayout::Hidden,
            PreviewLayout::Hidden => PreviewLayout::Side,
        }
    }
}

/// What Enter does: open the file, or print its path and exit (`--pick`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum UiMode {
    #[default]
    Open,
    Pick,
}

pub struct App {
    pub engine: Engine,
    pub input: String,
    pub selected: usize,
    pub regex_mode: bool,
    pub preview_layout: PreviewLayout,
    pub message: Option<String>,
    pub appearance: Appearance,
    pub picker: Option<Picker>,
    pub theme: Theme,
    pub ui_mode: UiMode,
    pub picked: Option<String>,
    /// Open actions popup: Some(selected entry index).
    pub menu: Option<usize>,
    pub history: Vec<String>,
    history_pos: Option<usize>,
    history_file: Option<std::path::PathBuf>,
    preview_for: Option<(String, Option<u64>)>,
    preview: PreviewContent,
}

enum PreviewContent {
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

impl App {
    pub fn new(engine: Engine) -> App {
        App {
            engine,
            input: String::new(),
            selected: 0,
            regex_mode: false,
            preview_layout: PreviewLayout::Side,
            message: None,
            appearance: Appearance::Dark,
            picker: None,
            theme: crate::theme::resolve("default", None),
            ui_mode: UiMode::Open,
            picked: None,
            menu: None,
            history: Vec::new(),
            history_pos: None,
            history_file: None,
            preview_for: None,
            preview: PreviewContent::Lines(Vec::new()),
        }
    }

    const MENU: [&'static str; 5] = [
        "open",
        "reveal in finder",
        "copy path",
        "quick look",
        "move to trash",
    ];

    fn run_menu_action(&mut self, entry: usize) {
        self.menu = None;
        match entry {
            0 => self.open_selected(),
            1 => self.act(actions::reveal, "revealed"),
            2 => self.act(actions::copy, "copied"),
            3 => self.act(actions::quick_look, "quick look"),
            4 => self.act(actions::trash, "trashed"),
            _ => {}
        }
    }

    fn history_step(&mut self, back: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_pos, back) {
            (None, true) => Some(self.history.len() - 1),
            (None, false) => return,
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => {
                // stepped past the newest entry: back to a blank query
                self.history_pos = None;
                self.input.clear();
                self.refresh_query();
                return;
            }
        };
        self.history_pos = next;
        if let Some(i) = next {
            self.input = self.history[i].clone();
            self.refresh_query_keep_history();
        }
    }

    fn push_history(&mut self) {
        let q = self.input.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.history.retain(|prev| prev != &q);
        self.history.push(q.clone());
        if let Some(file) = &self.history_file {
            crate::frecency::append_query(file, &q);
        }
    }

    /// Returns false when the app should quit.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.message = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // the actions popup swallows navigation while open
        if let Some(selected) = self.menu {
            match key.code {
                KeyCode::Esc | KeyCode::Left => self.menu = None,
                KeyCode::Down => self.menu = Some((selected + 1) % Self::MENU.len()),
                KeyCode::Up => {
                    self.menu = Some((selected + Self::MENU.len() - 1) % Self::MENU.len());
                }
                KeyCode::Enter => self.run_menu_action(selected),
                _ => {}
            }
            return true;
        }
        match (key.code, ctrl) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => return false,
            (KeyCode::Right, _) if !self.engine.results().is_empty() => {
                self.menu = Some(0);
            }
            (KeyCode::Char('p'), true) => self.history_step(true),
            (KeyCode::Char('n'), true) => self.history_step(false),
            (KeyCode::Char(' '), true) => self.act(actions::quick_look, "quick look"),
            (KeyCode::Char('r'), true) => {
                self.regex_mode = !self.regex_mode;
                self.refresh_query();
            }
            (KeyCode::Char('u'), true) => {
                self.input.clear();
                self.refresh_query();
            }
            (KeyCode::Char('j'), true) | (KeyCode::Down, _) => self.move_selection(1),
            (KeyCode::Char('k'), true) | (KeyCode::Up, _) => self.move_selection(-1),
            (KeyCode::Char('y'), true) => self.act(actions::copy, "copied"),
            (KeyCode::Char('f'), true) => self.act(actions::reveal, "revealed"),
            (KeyCode::Tab, _) => self.preview_layout = self.preview_layout.next(),
            (KeyCode::Enter, _) => match self.ui_mode {
                UiMode::Open => {
                    self.push_history();
                    self.open_selected();
                }
                UiMode::Pick => {
                    let picked = self
                        .engine
                        .results()
                        .get(self.selected)
                        .map(|row| row.path.clone());
                    if let Some(path) = picked {
                        self.push_history();
                        self.picked = Some(path);
                        return false;
                    }
                }
            },
            (KeyCode::Backspace, _) => {
                self.input.pop();
                self.refresh_query();
            }
            (KeyCode::Char(c), false) => {
                self.input.push(c);
                self.refresh_query();
            }
            _ => {}
        }
        true
    }

    fn refresh_query(&mut self) {
        self.history_pos = None;
        self.refresh_query_keep_history();
    }

    fn refresh_query_keep_history(&mut self) {
        self.selected = 0;
        self.engine.set_query(&self.input, self.regex_mode);
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.engine.results().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
    }

    fn open_selected(&mut self) {
        let Some(row) = self.engine.results().get(self.selected) else {
            return;
        };
        let path = row.path.clone();
        self.message = Some(match actions::open(&path) {
            Ok(()) => {
                self.engine.record_open(&path);
                format!("opened: {path}")
            }
            Err(e) => format!("error: {e}"),
        });
    }

    fn act(&mut self, f: impl Fn(&str) -> std::io::Result<()>, verb: &str) {
        let Some(row) = self.engine.results().get(self.selected) else {
            return;
        };
        self.message = Some(match f(&row.path) {
            Ok(()) => format!("{verb}: {}", row.path),
            Err(e) => format!("error: {e}"),
        });
    }

    fn load_preview(&mut self) {
        let Some(row) = self.engine.results().get(self.selected) else {
            self.preview_for = None;
            self.preview = PreviewContent::Lines(vec![Line::from("no selection")]);
            return;
        };
        let key = (row.path.clone(), row.line_number);
        if self.preview_for.as_ref() == Some(&key) {
            return;
        }
        self.preview_for = Some(key);
        if row.path.ends_with('/') {
            self.preview = PreviewContent::Lines(directory_listing(&row.path, self.theme.accent));
            return;
        }
        if crate::pdf::is_pdf_path(&row.path) {
            self.preview = PreviewContent::Lines(
                match crate::pdf::extract_cached(&row.path, &crate::pdf::default_cache_dir()) {
                    Ok(text) => match row.line_number {
                        Some(n) => {
                            let start = (n as usize).saturating_sub(6);
                            let gutter = Style::default().fg(self.theme.dim);
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
                                .collect()
                        }
                        None => text
                            .lines()
                            .take(100)
                            .map(|l| Line::from(l.to_string()))
                            .collect(),
                    },
                    Err(e) => vec![Line::from(format!("(pdf: {e})"))],
                },
            );
            return;
        }
        if images::is_image_path(&row.path) {
            self.preview = match (
                &self.picker,
                images::load(&row.path, images::MAX_IMAGE_BYTES),
            ) {
                (Some(picker), Ok(img)) => {
                    #[cfg(feature = "chafa")]
                    if picker.protocol_type() == ratatui_image::picker::ProtocolType::Halfblocks {
                        self.preview = PreviewContent::CellArt {
                            img,
                            cols: 0,
                            rows: 0,
                            lines: Vec::new(),
                        };
                        return;
                    }
                    PreviewContent::Image(Box::new(picker.new_resize_protocol(img)))
                }
                (None, _) => PreviewContent::Lines(vec![Line::from("(image)")]),
                (_, Err(e)) => PreviewContent::Lines(vec![Line::from(format!("(image: {e})"))]),
            };
            return;
        }
        let lines = match std::fs::read(&row.path) {
            Ok(bytes) if bytes.contains(&0) => vec![Line::from("(binary file)")],
            Ok(mut bytes) => {
                bytes.truncate(PREVIEW_BYTES);
                let text = String::from_utf8_lossy(&bytes);
                match row.line_number {
                    // center the preview on the matching line, with a gutter
                    Some(n) => {
                        let start = (n as usize).saturating_sub(6);
                        let end = start + 40;
                        highlight::highlight(&row.path, &text, self.appearance, end)
                            .into_iter()
                            .enumerate()
                            .skip(start)
                            .map(|(i, line)| {
                                let gutter = Style::default().fg(self.theme.dim);
                                let mut spans =
                                    vec![Span::styled(format!("{:>5} ", i + 1), gutter)];
                                spans.extend(line.spans);
                                Line::from(spans)
                            })
                            .collect()
                    }
                    None => highlight::highlight(&row.path, &text, self.appearance, 100),
                }
            }
            Err(e) => vec![Line::from(format!("(unreadable: {e})"))],
        };
        self.preview = PreviewContent::Lines(lines);
    }
}

const HINTS: &str = "> grep in files \u{b7} ext:pdf \u{b7} kind:image \u{b7} changed:7d \u{b7} larger:100mb \u{b7} dir: folders \u{b7} ctrl-r regex \u{b7} tab zoom preview";

fn themed_block(title: &str, theme: &Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(theme.title),
        ))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    // show syntax reminders under the search bar until typing starts
    let hint_rows = if app.input.is_empty() { 1 } else { 0 };
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(hint_rows),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_input(frame, app, outer[0]);
    if hint_rows > 0 {
        let hints = Paragraph::new(format!(" {HINTS}")).style(Style::default().fg(app.theme.dim));
        frame.render_widget(hints, outer[1]);
    }
    let body = outer[2];
    let status_area = outer[3];

    match app.preview_layout {
        PreviewLayout::Side => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(body);
            draw_results(frame, app, cols[0]);
            draw_preview(frame, app, cols[1]);
        }
        PreviewLayout::Full => draw_preview(frame, app, body),
        PreviewLayout::Hidden => draw_results(frame, app, body),
    }

    draw_status(frame, app, status_area);
    if let Some(selected) = app.menu {
        draw_menu(frame, selected, body, &app.theme);
    }
}

/// Small actions popup anchored inside the body area.
fn draw_menu(frame: &mut Frame, selected: usize, body: Rect, theme: &Theme) {
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
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Preview for a directory entry: its children, folders first.
fn directory_listing(path: &str, accent: Color) -> Vec<Line<'static>> {
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

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let mode = match (app.engine.mode(), app.regex_mode) {
        (Mode::Content, _) => "content",
        (_, true) => "regex",
        _ => "fuzzy",
    };
    let input = Paragraph::new(app.input.as_str())
        .block(themed_block(&format!("fsearch [{mode}]"), &app.theme));
    frame.render_widget(input, area);
    frame.set_cursor_position((area.x + 1 + app.input.len() as u16, area.y + 1));
}

/// Splits `shown` into spans, styling the chars at `positions` (char indices).
fn spans_with_positions(shown: &str, positions: &[u32], highlight: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_highlighted = false;
    let mut next = positions.iter().peekable();
    for (i, ch) in shown.chars().enumerate() {
        while next.next_if(|&&p| (p as usize) < i).is_some() {}
        let highlighted = next.peek().is_some_and(|&&p| p as usize == i);
        if highlighted != run_highlighted && !run.is_empty() {
            let text = std::mem::take(&mut run);
            spans.push(if run_highlighted {
                Span::styled(text, highlight)
            } else {
                Span::raw(text)
            });
        }
        run_highlighted = highlighted;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(if run_highlighted {
            Span::styled(run, highlight)
        } else {
            Span::raw(run)
        });
    }
    spans
}

fn draw_results(frame: &mut Frame, app: &App, area: Rect) {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let accent = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut highlighter = (matches!(app.engine.mode(), Mode::Fuzzy) && !app.input.is_empty())
        .then(|| Highlighter::new(&app.input));
    let items: Vec<ListItem> = app
        .engine
        .results()
        .iter()
        .map(|r| {
            let (shown, trimmed_chars) = match &home {
                Some(h) if r.path.starts_with(h.as_str()) => {
                    (format!("~{}", &r.path[h.len()..]), h.chars().count())
                }
                _ => (r.path.clone(), 0),
            };
            match (r.line_number, &r.line) {
                (Some(n), Some(line)) => ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{shown}:{n} "),
                        Style::default().fg(app.theme.accent),
                    ),
                    Span::raw(line.clone()),
                ])),
                _ => match highlighter.as_mut() {
                    Some(hl) => {
                        // positions refer to the full path; shift them onto the
                        // `~`-shortened string and drop hits inside the prefix
                        let shift = if trimmed_chars > 0 {
                            trimmed_chars - 1
                        } else {
                            0
                        };
                        let positions: Vec<u32> = hl
                            .positions(&r.path)
                            .into_iter()
                            .filter(|&p| p as usize >= trimmed_chars)
                            .map(|p| (p as usize - shift) as u32)
                            .collect();
                        ListItem::new(Line::from(spans_with_positions(&shown, &positions, accent)))
                    }
                    None => ListItem::new(shown),
                },
            }
        })
        .collect();
    // On the launch screen (empty query), split the list into "recent
    // opens" (frecency) and "recently modified" with dim section headers.
    // Headers are extra list rows, so the selection index shifts past them.
    let rows = app.engine.results();
    let opened = rows.iter().take_while(|r| r.recent_open).count();
    let sectioned = app.input.is_empty() && matches!(app.engine.mode(), Mode::Fuzzy) && opened > 0;
    let mut display_items = items;
    let mut display_selected = app.selected;
    if sectioned {
        let header = |label: &str| {
            ListItem::new(Span::styled(
                format!("─ {label} ────────"),
                Style::default().fg(app.theme.dim),
            ))
        };
        let mut with_headers = Vec::with_capacity(display_items.len() + 2);
        with_headers.push(header("RECENT OPENS"));
        for (i, item) in display_items.into_iter().enumerate() {
            if i == opened {
                with_headers.push(header("RECENTLY MODIFIED"));
            }
            with_headers.push(item);
        }
        display_items = with_headers;
        display_selected += if app.selected < opened { 1 } else { 2 };
    }
    let list = List::new(display_items)
        .block(themed_block("results", &app.theme))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(display_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    app.load_preview();
    let block = themed_block("preview", &app.theme);
    match &mut app.preview {
        PreviewContent::Lines(lines) => {
            frame.render_widget(Paragraph::new(lines.clone()).block(block), area);
        }
        PreviewContent::Image(protocol) => {
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_stateful_widget(StatefulImage::default(), inner, protocol.as_mut());
        }
        #[cfg(feature = "chafa")]
        PreviewContent::CellArt {
            img,
            cols,
            rows,
            lines,
        } => {
            let inner = block.inner(area);
            let (want_cols, want_rows) =
                crate::cellart::fit_cells(img.width(), img.height(), inner.width, inner.height);
            if (*cols, *rows) != (want_cols, want_rows) {
                *lines = crate::cellart::render(img, want_cols, want_rows);
                (*cols, *rows) = (want_cols, want_rows);
            }
            frame.render_widget(Paragraph::new(lines.clone()).block(block), area);
        }
    }
}

/// "412 B", "1.3 KB", "2.0 MB", "1.1 GB"
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// "just now", "5m ago", "3h ago", "12d ago", "2y ago"
fn human_age(modified: std::time::SystemTime) -> String {
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

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let s = app.engine.status();
    let mut parts = vec![
        format!("{} indexed", s.indexed),
        format!("{} matches", s.matches),
    ];
    if let Some(row) = app.engine.results().get(app.selected)
        && let Ok(meta) = std::fs::metadata(&row.path)
    {
        if meta.is_file() {
            parts.push(human_size(meta.len()));
        }
        if let Ok(modified) = meta.modified() {
            parts.push(human_age(modified));
        }
    }
    if s.indexing {
        parts.push("indexing…".to_string());
    }
    if let Some(e) = &s.error {
        parts.push(e.clone());
    }
    if let Some(m) = &app.message {
        parts.push(m.clone());
    }
    let status = Paragraph::new(parts.join(" · ")).style(Style::default().fg(app.theme.dim));
    frame.render_widget(status, area);
}

/// A generous cell-size guess for terminals that support a graphics protocol
/// but answer no font-size query (multiplexers, notably). Kitty/iTerm2 scale
/// the image into the target cell rect, so overshooting just supersamples.
const GUESSED_FONT_SIZE: ratatui_image::FontSize = ratatui_image::FontSize {
    width: 16,
    height: 32,
};

// from_fontsize is deprecated in favor of auto-detection, but this path
// exists precisely because auto-detection fails on terminals that ACK a
// graphics protocol while ignoring font-size queries
#[allow(deprecated)]
fn forced_picker(protocol: ratatui_image::picker::ProtocolType) -> Picker {
    let mut picker = Picker::from_fontsize(GUESSED_FONT_SIZE);
    picker.set_protocol_type(protocol);
    picker
}

/// Runs the full pre-init terminal probe: appearance, responsiveness, and
/// graphics picker selection (honoring `FSEARCH_IMAGES=off|halfblocks|
/// kitty|iterm2`). Performs stdio queries — call before raw mode / the
/// event loop consumes stdin.
pub fn probe_terminal() -> (highlight::TerminalTraits, Option<Picker>) {
    use ratatui_image::picker::ProtocolType;
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        // stdout is a pipe (`--pick` in command substitution): the stdio
        // capability queries would write escape bytes into the pipe and
        // never get answers, so skip them entirely
        return (
            highlight::TerminalTraits {
                appearance: crate::highlight::Appearance::Dark,
                responsive: false,
            },
            Some(Picker::halfblocks()),
        );
    }
    let traits = highlight::detect_terminal();
    let picker = match std::env::var("FSEARCH_IMAGES").as_deref() {
        Ok("off") => None,
        Ok("halfblocks") => Some(Picker::halfblocks()),
        Ok("kitty") => Some(forced_picker(ProtocolType::Kitty)),
        Ok("iterm2") => Some(forced_picker(ProtocolType::Iterm2)),
        // NOTE: no auto-upgrade on a bare capability ACK. Some multiplexers
        // (Herdr, and WezTerm/Konsole per ratatui-image's blacklist) answer
        // the Kitty query but never render the images — a blank pane is
        // worse than halfblocks. Users who know better can force a protocol.
        _ if traits.responsive => {
            Some(Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks()))
        }
        _ => Some(Picker::halfblocks()),
    };
    (traits, picker)
}

fn open_tty() -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).open("/dev/tty")
}

fn restore_terminal() {
    if let Ok(mut tty) = open_tty() {
        let _ = execute!(tty, LeaveAlternateScreen);
    }
    let _ = disable_raw_mode();
}

/// Runs the UI. Draws on /dev/tty (not stdout), so `--pick` works inside
/// command substitution. In [`UiMode::Pick`], Enter returns the selection.
pub fn run(
    engine: Engine,
    ui_mode: UiMode,
    initial_query: &str,
    theme: Theme,
) -> anyhow::Result<Option<String>> {
    let (traits, picker) = probe_terminal();
    highlight::preload();
    let mut tty = open_tty()?;
    enable_raw_mode()?;
    execute!(tty, EnterAlternateScreen)?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(tty))?;
    let mut app = App::new(engine);
    app.appearance = traits.appearance;
    app.picker = picker;
    app.ui_mode = ui_mode;
    app.theme = theme;
    let queries_path = crate::frecency::default_queries_path();
    app.history = crate::frecency::load_queries(&queries_path);
    app.history_file = Some(queries_path);
    if !initial_query.is_empty() {
        app.input = initial_query.to_string();
        app.engine.set_query(&app.input, app.regex_mode);
    }
    let result = loop {
        app.engine.tick();
        let len = app.engine.results().len();
        if app.selected >= len && len > 0 {
            app.selected = len - 1;
        }
        if let Err(e) = terminal.draw(|f| draw(f, &mut app)) {
            break Err(e.into());
        }
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => {
                if let Ok(Event::Key(key)) = event::read()
                    && key.is_press()
                    && !app.handle_key(key)
                {
                    break Ok(());
                }
            }
            Ok(false) => {}
            Err(e) => break Err(e.into()),
        }
    };
    restore_terminal();
    let _ = std::panic::take_hook(); // drop the restoring hook
    result.map(|_| app.picked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::engine::Engine;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn test_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            roots: vec![dir.path().to_path_buf()],
            excludes: vec![],
            max_content_filesize: 1024,
            theme: Default::default(),
        };
        let engine = Engine::new(
            config,
            dir.path().join("index.bin"),
            dir.path().join("history"),
        );
        // keep the tempdir alive for the test's duration by leaking it (test-only)
        std::mem::forget(dir);
        App::new(engine)
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn renders_input_and_status() {
        let mut app = test_app();
        app.input = "notes".to_string();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("notes"));
        assert!(text.contains("fuzzy"));
    }

    #[test]
    fn hints_show_only_while_input_is_empty() {
        let mut app = test_app();
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(buffer_text(&terminal).contains("grep in files"));
        app.input = "x".to_string();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(!buffer_text(&terminal).contains("grep in files"));
    }

    #[test]
    fn typing_updates_input_and_esc_quits() {
        let mut app = test_app();
        assert!(app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert_eq!(app.input, "a");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
        assert_eq!(app.input, "");
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    }

    #[test]
    fn sizes_and_ages_humanize() {
        assert_eq!(human_size(412), "412 B");
        assert_eq!(human_size(1300), "1.3 KB");
        assert_eq!(human_size(2_000_000), "2.0 MB");
        assert_eq!(human_size(1_100_000_000), "1.1 GB");
        let now = std::time::SystemTime::now();
        assert_eq!(human_age(now), "just now");
        assert_eq!(
            human_age(now - std::time::Duration::from_secs(300)),
            "5m ago"
        );
        assert_eq!(
            human_age(now - std::time::Duration::from_secs(7200)),
            "2h ago"
        );
        assert_eq!(
            human_age(now - std::time::Duration::from_secs(3 * 24 * 3600)),
            "3d ago"
        );
    }

    #[test]
    fn spans_split_on_highlight_boundaries() {
        let hl = Style::default().fg(Color::Cyan);
        let spans = spans_with_positions("abcd", &[1, 2], hl);
        let texts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["a", "bc", "d"]);
        assert_eq!(spans[1].style, hl);
        assert_eq!(spans[0].style, Style::default());
        // no positions → single raw span
        assert_eq!(spans_with_positions("abcd", &[], hl).len(), 1);
    }

    #[test]
    fn tab_cycles_preview_layouts() {
        let mut app = test_app();
        assert_eq!(app.preview_layout, PreviewLayout::Side);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.preview_layout, PreviewLayout::Full);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.preview_layout, PreviewLayout::Hidden);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.preview_layout, PreviewLayout::Side);
    }

    #[test]
    fn full_layout_renders_preview_without_results() {
        let mut app = test_app();
        app.preview_layout = PreviewLayout::Full;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("preview"));
        assert!(!text.contains("results"));
    }

    #[test]
    fn sections_render_on_empty_query_with_recent_opens() {
        use crate::engine::ResultRow;
        let mut app = test_app();
        // simulate an engine state with one frecency row and one plain row
        app.engine.inject_results_for_test(vec![
            ResultRow {
                path: "/a/opened.txt".into(),
                line_number: None,
                line: None,
                recent_open: true,
            },
            ResultRow {
                path: "/a/fresh.txt".into(),
                line_number: None,
                line: None,
                recent_open: false,
            },
        ]);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("RECENT OPENS"));
        assert!(text.contains("RECENTLY MODIFIED"));
        // typing hides the sections
        app.input = "x".to_string();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(!buffer_text(&terminal).contains("RECENT OPENS"));
    }

    #[test]
    fn history_cycles_with_ctrl_p_and_n() {
        let mut app = test_app();
        app.history = vec!["alpha".to_string(), "beta".to_string()];
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "beta");
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "alpha");
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "beta");
        // stepping past the newest clears back to a blank query
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "");
        // typing resets the cursor position in history
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "beta");
    }

    #[test]
    fn menu_opens_only_with_results_and_esc_closes_it() {
        let mut app = test_app();
        // no results: Right is a no-op
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.menu, None);
        // force it open: Esc closes the menu without quitting the app
        app.menu = Some(0);
        assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(app.menu, None);
        // and a second Esc quits
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    }

    #[test]
    fn menu_renders_actions() {
        let mut app = test_app();
        app.menu = Some(4);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("move to trash"));
        assert!(text.contains("quick look"));
    }

    #[test]
    fn pick_mode_enter_returns_selection_and_quits() {
        let mut app = test_app();
        app.ui_mode = UiMode::Pick;
        // no results yet: Enter does nothing
        assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.picked.is_none());
    }

    #[test]
    fn ctrl_r_toggles_regex_mode() {
        let mut app = test_app();
        assert!(!app.regex_mode);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.regex_mode);
    }
}
