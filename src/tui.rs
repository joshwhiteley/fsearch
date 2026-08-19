use crate::actions;
use crate::engine::{Engine, Mode};
use crate::highlight::{self, Appearance};
use crate::images;
use crate::matcher::Highlighter;
use crate::theme::{BorderKind, Theme};
use crate::util::human_size;
use crate::walker::FileMeta;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState,
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend, crossterm::execute};
use ratatui_image::picker::Picker;
use ratatui_image::{StatefulImage, protocol::StatefulProtocol};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const PREVIEW_BYTES: usize = 64 * 1024;
const PREVIEW_SCROLL_PAGE: usize = 20;

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

/// Row layout for the results list; ctrl-t toggles between them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Density {
    #[default]
    Comfy, // two-line rows
    Compact, // single-line rows
}

impl Density {
    fn toggle(self) -> Density {
        match self {
            Density::Comfy => Density::Compact,
            Density::Compact => Density::Comfy,
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
    /// Byte offset of the edit cursor, always on a char boundary.
    pub input_cursor: usize,
    pub selected: usize,
    pub regex_mode: bool,
    pub preview_layout: PreviewLayout,
    pub density: Density,
    /// When false and the fuzzy score floor leaves weaker matches, only the
    /// strong block is shown (weak matches behind the ctrl-x fold row).
    pub show_weak: bool,
    /// Floating confirmation toast: (text, when it was raised); auto-expires
    /// after a couple of seconds and is dismissed by the next keypress.
    pub message: Option<(String, Instant)>,
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
    /// Preview loading runs on a worker thread; these move requests/results.
    preview_tx: mpsc::Sender<PreviewRequest>,
    preview_rx: mpsc::Receiver<PreviewResult>,
    preview_gen: u64,
    /// Cached fuzzy-match highlighter, rebuilt only when input changes.
    highlighter_input: String,
    highlighter: Option<Highlighter>,
    /// Cached first-match regex for content rows, rebuilt only when input
    /// changes (same take/rebuild pattern as the highlighter).
    content_highlight_input: String,
    content_highlight: Option<regex::Regex>,
    /// Cached stat of the status line's selected path (is_file, size, mtime).
    status_path: String,
    status_meta: Option<(bool, u64, Option<SystemTime>)>,
    /// Pixel dimensions of the image preview, from the decode on the worker.
    preview_image_dims: Option<(u32, u32)>,
    /// First line shown in the text preview; reset on selection change.
    preview_scroll: usize,
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

/// One preview load job; everything the worker needs (no Picker/ratatui
/// image types cross the channel — protocol construction stays on the UI
/// thread).
struct PreviewRequest {
    generation: u64,
    path: String,
    line_number: Option<u64>,
    appearance: Appearance,
    gutter: Color,
}

struct PreviewResult {
    generation: u64,
    path: String,
    line_number: Option<u64>,
    payload: PreviewPayload,
}

enum PreviewPayload {
    /// Styled, line-numbered preview lines (text and PDFs).
    Lines(Vec<Line<'static>>),
    /// Decoded image; not yet converted to a ratatui-image protocol.
    Image(image::DynamicImage),
}

/// The expensive half of preview loading — read, syntax-highlight, PDF
/// extract, image decode — runs on this worker thread so the UI thread only
/// applies results. Mirrors the former synchronous load_preview logic.
fn preview_payload(req: &PreviewRequest) -> PreviewPayload {
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
    if images::is_image_path(&req.path) {
        return match images::load(&req.path, images::MAX_IMAGE_BYTES) {
            Ok(img) => PreviewPayload::Image(img),
            Err(e) => PreviewPayload::Lines(vec![Line::from(format!("(image: {e})"))]),
        };
    }
    match std::fs::read(&req.path) {
        Ok(bytes) if bytes.contains(&0) => PreviewPayload::Lines(vec![Line::from("(binary file)")]),
        Ok(mut bytes) => {
            bytes.truncate(PREVIEW_BYTES);
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

impl App {
    pub fn new(engine: Engine) -> App {
        let (preview_tx, preview_rx) = mpsc::channel::<PreviewRequest>();
        let (result_tx, result_rx) = mpsc::channel::<PreviewResult>();
        // one preview worker for the app's lifetime; exits when the app
        // (and thus preview_tx) is dropped
        std::thread::spawn(move || {
            while let Ok(req) = preview_rx.recv() {
                let payload = preview_payload(&req);
                if result_tx
                    .send(PreviewResult {
                        generation: req.generation,
                        path: req.path,
                        line_number: req.line_number,
                        payload,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        App {
            engine,
            input: String::new(),
            input_cursor: 0,
            selected: 0,
            regex_mode: false,
            preview_layout: PreviewLayout::Side,
            density: Density::Comfy,
            show_weak: false,
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
            preview_tx,
            preview_rx: result_rx,
            preview_gen: 0,
            highlighter_input: String::new(),
            highlighter: None,
            content_highlight_input: String::new(),
            content_highlight: None,
            status_path: String::new(),
            status_meta: None,
            preview_image_dims: None,
            preview_scroll: 0,
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
                self.input_cursor = 0;
                self.refresh_query();
                return;
            }
        };
        self.history_pos = next;
        if let Some(i) = next {
            self.input = self.history[i].clone();
            self.input_cursor = self.input.len();
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

    /// Byte offset of the start of the last char in `s[..end]` (0 if none).
    fn prev_char_boundary(s: &str, end: usize) -> usize {
        s[..end].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    fn cursor_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor = Self::prev_char_boundary(&self.input, self.input_cursor);
        }
    }

    fn cursor_right(&mut self) {
        if self.input_cursor < self.input.len() {
            self.input_cursor += self.input[self.input_cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
        }
    }

    fn cursor_start(&mut self) {
        self.input_cursor = 0;
    }

    fn cursor_end(&mut self) {
        self.input_cursor = self.input.len();
    }

    fn delete_backward(&mut self) {
        if self.input_cursor > 0 {
            let start = Self::prev_char_boundary(&self.input, self.input_cursor);
            self.input.drain(start..self.input_cursor);
            self.input_cursor = start;
        }
    }

    fn delete_forward(&mut self) {
        if self.input_cursor < self.input.len() {
            let end = self.input_cursor
                + self.input[self.input_cursor..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
            self.input.drain(self.input_cursor..end);
        }
    }

    /// readline-style ctrl-w: delete trailing whitespace plus the preceding
    /// run of non-whitespace chars before the cursor.
    fn delete_word_backward(&mut self) {
        let mut start = self.input_cursor;
        while start > 0 {
            let prev = Self::prev_char_boundary(&self.input, start);
            if !self.input[prev..start]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace())
            {
                break;
            }
            start = prev;
        }
        while start > 0 {
            let prev = Self::prev_char_boundary(&self.input, start);
            if self.input[prev..start]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace())
            {
                break;
            }
            start = prev;
        }
        self.input.drain(start..self.input_cursor);
        self.input_cursor = start;
    }

    fn insert_char(&mut self, c: char) {
        self.input.insert(self.input_cursor, c);
        self.input_cursor += c.len_utf8();
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
            (KeyCode::Right, _) => {
                if self.input_cursor < self.input.len() {
                    self.cursor_right();
                } else if !self.engine.results().is_empty() {
                    self.menu = Some(0);
                }
            }
            (KeyCode::Left, _) => self.cursor_left(),
            (KeyCode::Char('p'), true) => self.history_step(true),
            (KeyCode::Char('n'), true) => self.history_step(false),
            (KeyCode::Char(' '), true) => self.act(actions::quick_look, "quick look"),
            (KeyCode::Char('r'), true) => {
                self.regex_mode = !self.regex_mode;
                self.refresh_query();
            }
            (KeyCode::Char('x'), true) => {
                self.show_weak = !self.show_weak;
                // folding the weaker tail back up clamps onto the last strong row
                if !self.show_weak {
                    let len = self.visible_len();
                    if self.selected >= len && len > 0 {
                        self.selected = len - 1;
                    }
                }
            }
            (KeyCode::Char('t'), true) => self.density = self.density.toggle(),
            (KeyCode::Char('u'), true) => {
                self.input.clear();
                self.input_cursor = 0;
                self.refresh_query();
            }
            (KeyCode::Char('a'), true) => self.cursor_start(),
            (KeyCode::Char('e'), true) => self.cursor_end(),
            (KeyCode::Char('w'), true) => {
                self.delete_word_backward();
                self.refresh_query();
            }
            (KeyCode::Char('d'), true) => {
                self.delete_forward();
                self.refresh_query();
            }
            (KeyCode::Char('j'), true) | (KeyCode::Down, _) => self.move_selection(1),
            (KeyCode::Char('k'), true) | (KeyCode::Up, _) => self.move_selection(-1),
            (KeyCode::Char('y'), true) => self.act(actions::copy, "copied"),
            (KeyCode::Char('f'), true) => self.act(actions::reveal, "revealed"),
            (KeyCode::Tab, _) => self.preview_layout = self.preview_layout.next(),
            (KeyCode::PageDown, _) => {
                self.preview_scroll = self.preview_scroll.saturating_add(PREVIEW_SCROLL_PAGE);
            }
            (KeyCode::PageUp, _) => {
                self.preview_scroll = self.preview_scroll.saturating_sub(PREVIEW_SCROLL_PAGE);
            }
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
                self.delete_backward();
                self.refresh_query();
            }
            (KeyCode::Char(c), false) => {
                self.insert_char(c);
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
        self.show_weak = false;
        self.engine.set_query(&self.input, self.regex_mode);
    }

    /// Rows currently on screen: the strong block when weaker matches are
    /// folded away, otherwise every result.
    fn visible_len(&self) -> usize {
        if matches!(self.engine.mode(), Mode::Fuzzy)
            && !self.input.is_empty()
            && !self.show_weak
            && self.engine.strong_count() < self.engine.results().len()
        {
            self.engine.strong_count()
        } else {
            self.engine.results().len()
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.visible_len();
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
        self.message = Some((
            match actions::open(&path) {
                Ok(()) => {
                    self.engine.record_open(&path);
                    format!("opened: {path}")
                }
                Err(e) => format!("error: {e}"),
            },
            Instant::now(),
        ));
    }

    fn act(&mut self, f: impl Fn(&str) -> std::io::Result<()>, verb: &str) {
        let Some(row) = self.engine.results().get(self.selected) else {
            return;
        };
        self.message = Some((
            match f(&row.path) {
                Ok(()) => format!("{verb}: {}", row.path),
                Err(e) => format!("error: {e}"),
            },
            Instant::now(),
        ));
    }

    fn load_preview(&mut self) {
        let Some(row) = self.engine.results().get(self.selected) else {
            self.preview_for = None;
            self.preview = PreviewContent::Lines(vec![Line::from("no selection")]);
            self.preview_image_dims = None;
            return;
        };
        let key = (row.path.clone(), row.line_number);
        if self.preview_for.as_ref() == Some(&key) {
            return;
        }
        self.preview_for = Some(key);
        self.preview_scroll = 0;
        self.preview_image_dims = None;
        if row.path.ends_with('/') {
            // cheap; stays on the UI thread
            self.preview = PreviewContent::Lines(directory_listing(&row.path, self.theme.accent));
            return;
        }
        // expensive loading (file read, highlight, PDF/image decode) happens
        // on the preview worker; show a placeholder until poll_preview
        // delivers the result on a later draw
        self.preview = PreviewContent::Lines(vec![Line::from("loading...")]);
        self.preview_gen += 1;
        let _ = self.preview_tx.send(PreviewRequest {
            generation: self.preview_gen,
            path: row.path.clone(),
            line_number: row.line_number,
            appearance: self.appearance,
            gutter: self.theme.dim,
        });
    }

    /// Applies preview results that arrived since the last draw. Stale
    /// generations and superseded selections are dropped.
    fn poll_preview(&mut self) {
        while let Ok(result) = self.preview_rx.try_recv() {
            if result.generation != self.preview_gen {
                continue;
            }
            if !self
                .preview_for
                .as_ref()
                .is_some_and(|(p, n)| p == &result.path && *n == result.line_number)
            {
                continue;
            }
            self.preview = match result.payload {
                PreviewPayload::Lines(lines) => PreviewContent::Lines(lines),
                PreviewPayload::Image(img) => {
                    self.preview_image_dims = Some((img.width(), img.height()));
                    match &self.picker {
                        Some(picker) => {
                            #[cfg(feature = "chafa")]
                            if picker.protocol_type()
                                == ratatui_image::picker::ProtocolType::Halfblocks
                            {
                                PreviewContent::CellArt {
                                    img,
                                    cols: 0,
                                    rows: 0,
                                    lines: Vec::new(),
                                }
                            } else {
                                PreviewContent::Image(Box::new(picker.new_resize_protocol(img)))
                            }
                            #[cfg(not(feature = "chafa"))]
                            PreviewContent::Image(Box::new(picker.new_resize_protocol(img)))
                        }
                        None => PreviewContent::Lines(vec![Line::from("(image)")]),
                    }
                }
            };
        }
    }
}

const HINTS: &str = "> grep in files \u{b7} ? semantic \u{b7} 'word exact \u{b7} ext:pdf \u{b7} kind:image \u{b7} changed:7d \u{b7} larger:100mb \u{b7} dir: folders \u{b7} ctrl-r regex \u{b7} tab zoom preview";

fn themed_block(title: &str, theme: &Theme) -> Block<'static> {
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
fn selection_style(theme: &Theme) -> Style {
    match theme.selection_bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default().add_modifier(Modifier::REVERSED),
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    // show syntax reminders under the search bar until typing starts
    let hint_rows = if app.input.is_empty() { 1 } else { 0 };
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
        .highlight_style(selection_style(theme));
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

/// Styled spans for the query input: a leading `>` / `?` mode prefix lights
/// up in the accent, and tokens the real parser consumes as filters (ext:,
/// kind:, changed:, ...) turn yellow. Concatenating the span contents
/// reproduces `input` exactly, so the cursor math below stays valid.
fn query_spans(input: &str, accent: Color) -> Vec<Span<'static>> {
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

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let mode = match (app.engine.mode(), app.regex_mode) {
        (Mode::Content, _) => "content",
        (Mode::Semantic, _) => "semantic",
        (_, true) => "regex",
        _ => "fuzzy",
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
fn spans_with_styles(
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
fn highlight_first_match(line: &str, re: &regex::Regex, accent: Style) -> Vec<Span<'static>> {
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
fn badge_for(path: &str, badges: [Color; 6], accent: Color) -> (String, Color) {
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
        _ => badges[5],
    };
    (label, color)
}

/// The badge span (`" PDF "` on its kind color) plus the gap space after it,
/// and the total visual width of both (used to indent second lines).
fn badge_spans(path: &str, badges: [Color; 6], accent: Color) -> (Vec<Span<'static>>, usize) {
    let (label, color) = badge_for(path, badges, accent);
    let width = label.chars().count() + 3; // " label " + the gap
    let span = Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    );
    (vec![span, Span::raw(" ")], width)
}

/// Spaces to push the right column flush against the row's right edge; None
/// when there is no room for even one gap (callers then drop the column).
fn right_pad(left: usize, right: usize, inner_width: usize) -> Option<usize> {
    let pad = inner_width.saturating_sub(left + right);
    (pad >= 1).then_some(pad)
}

/// "5m ago" for a row's mtime; None when the meta is missing or bogus
/// (mtime <= 0).
fn row_age(meta: Option<FileMeta>) -> Option<String> {
    meta.filter(|m| m.mtime > 0)
        .map(|m| human_age(SystemTime::UNIX_EPOCH + Duration::from_secs(m.mtime as u64)))
}

/// Filled/empty cells of a 5-cell score bar for a 0..=1 similarity score.
/// Returns the number filled and the bar glyph string.
fn score_bar(s: f32) -> (usize, String) {
    let filled = ((s.clamp(0.0, 1.0) * 5.0).round() as usize).min(5);
    (filled, "▰".repeat(filled) + &"▱".repeat(5 - filled))
}

/// Right-aligned score readout for a semantic row: a styled 5-cell bar plus
/// the percent, returning its cell width and the spans to render.
fn score_readout(s: f32, accent: Color, dim: Style) -> (usize, Vec<Span<'static>>) {
    let (filled, bar) = score_bar(s);
    let (fill, rest) = bar.split_at(filled);
    let pct = format!(" {:.0}%", s * 100.0);
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !fill.is_empty() {
        spans.push(Span::styled(fill.to_string(), Style::default().fg(accent)));
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_string(), dim));
    }
    let width = bar.chars().count() + pct.chars().count();
    spans.push(Span::styled(pct, dim));
    (width, spans)
}

fn draw_results(frame: &mut Frame, app: &mut App, area: Rect) {
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
    let mut highlighter = std::mem::take(&mut app.highlighter);
    if matches!(app.engine.mode(), Mode::Fuzzy) && !app.input.is_empty() {
        if app.highlighter_input != app.input {
            app.highlighter_input = app.input.clone();
            highlighter = Some(Highlighter::new(&app.input));
        }
    } else {
        app.highlighter_input.clear();
        highlighter = None;
    }
    // same take/rebuild cache for the first-match content highlight
    let mut content_re = std::mem::take(&mut app.content_highlight);
    if matches!(app.engine.mode(), Mode::Content) && !app.input.is_empty() {
        if app.content_highlight_input != app.input {
            app.content_highlight_input = app.input.clone();
            let (_, pattern) = crate::engine::parse_query(&app.input, app.regex_mode);
            content_re = regex::RegexBuilder::new(&pattern)
                .case_insensitive(!pattern.chars().any(char::is_uppercase))
                .build()
                .ok();
        }
    } else {
        app.content_highlight_input.clear();
        content_re = None;
    }
    let inner_width = area.width.saturating_sub(2) as usize; // minus the borders
    let name_plain = Style::default().add_modifier(Modifier::BOLD);
    let parent_hl = Style::default().fg(match_color).add_modifier(Modifier::DIM);
    let items: Vec<ListItem> = app
        .engine
        .results()
        .iter()
        .take(app.visible_len())
        .map(|r| {
            let (shown, trimmed_chars) = match &home {
                Some(h) if r.path.starts_with(h.as_str()) => {
                    (format!("~{}", &r.path[h.len()..]), h.chars().count())
                }
                _ => (r.path.clone(), 0),
            };
            // split the shown path at the name boundary: the final component
            // (trailing '/' kept for directories) is the hero, the rest is
            // the dim parent
            let name = {
                let stem = r.path.trim_end_matches('/');
                let last = stem.rsplit('/').next().unwrap_or("");
                if r.path.ends_with('/') {
                    format!("{last}/")
                } else {
                    last.to_string()
                }
            };
            let parent = shown[..shown.len() - name.len()].to_string();
            let name_chars = name.chars().count();
            let parent_chars = shown.chars().count() - name_chars;
            match (r.line_number, &r.line) {
                (Some(n), Some(line)) => {
                    let (badge, badge_width) = badge_spans(&r.path, badges, dir_color);
                    let colon = format!(":{n}");
                    let age = row_age(r.meta);
                    match app.density {
                        Density::Comfy => ListItem::new(Text::from(vec![
                            // badge, bold name, dim :n, age flush right
                            Line::from({
                                let mut spans = badge;
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
                                    let left = badge_width + name_chars + colon.chars().count();
                                    if let Some(pad) = right_pad(left, right_width, inner_width) {
                                        spans.push(Span::raw(" ".repeat(pad)));
                                        spans.extend(right_spans);
                                    }
                                }
                                spans
                            }),
                            // indented matched line text
                            Line::from({
                                let mut spans = vec![Span::raw(" ".repeat(badge_width))];
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
                                    + name_chars
                                    + colon.chars().count()
                                    + 1
                                    + line.chars().count();
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
                    let (badge, badge_width) = badge_spans(&r.path, badges, dir_color);
                    // positions refer to the full path; shift them onto the
                    // `~`-shortened string, then partition them at the name
                    // boundary (the parent starts at index 0 of `shown`)
                    let (in_name, in_parent): (Vec<u32>, Vec<u32>) = match highlighter.as_mut() {
                        Some(hl) => {
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
                            line1.extend(name_spans);
                            if let Some(age) = &age {
                                let left = badge_width + name_chars;
                                if let Some(pad) = right_pad(left, age.chars().count(), inner_width)
                                {
                                    line1.push(Span::raw(" ".repeat(pad)));
                                    line1.push(Span::styled(age.clone(), dim));
                                }
                            }
                            // line 2: indented dim parent (+ size when known)
                            let mut line2 = vec![Span::raw(" ".repeat(badge_width))];
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
                            line1.extend(name_spans);
                            line1.push(Span::styled(" — ".to_string(), dim));
                            line1.extend(spans_with_styles(&parent, &in_parent, dim, parent_hl));
                            if let Some(right) = &right {
                                let left = badge_width + name_chars + 3 + parent.chars().count();
                                if let Some(pad) =
                                    right_pad(left, right.chars().count(), inner_width)
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
    // On the launch screen (empty query), split the list into "recent
    // opens" (frecency) and "recently modified" with dim section headers.
    // Headers are extra list rows, so the selection index shifts past them.
    let rows = app.engine.results();
    let visible = app.visible_len();
    let strong = app.engine.strong_count();
    let hidden = rows.len().saturating_sub(visible);
    let opened = rows.iter().take_while(|r| r.recent_open).count();
    let sectioned = app.input.is_empty() && matches!(app.engine.mode(), Mode::Fuzzy) && opened > 0;
    let mut display_items = items;
    let mut display_selected = app.selected;
    if sectioned {
        let header = |label: &str| {
            ListItem::new(Span::styled(
                format!("─ {label} ────────"),
                Style::default().fg(app.theme.section.unwrap_or(app.theme.dim)),
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
    // The weaker-match fold: a one-line dim fold after the last strong row
    // while weak matches are hidden, or — once revealed — a section header
    // before the first weaker row. Extra non-result rows (like the launch
    // sections above); the trailing fold row never shifts the selection.
    if hidden > 0 || (app.show_weak && strong < rows.len()) {
        let mut out = Vec::with_capacity(display_items.len() + 2);
        if app.show_weak && strong < rows.len() {
            for (i, item) in display_items.into_iter().enumerate() {
                if i == strong {
                    out.push(ListItem::new(Span::styled(
                        "─ WEAKER MATCHES ─",
                        Style::default().fg(app.theme.section.unwrap_or(app.theme.dim)),
                    )));
                }
                out.push(item);
            }
            display_items = out;
            // the header sits just before the first weaker row; a selection
            // at or past it shifts one place
            display_selected += if app.selected < strong { 0 } else { 1 };
        } else {
            out.extend(display_items);
            out.push(ListItem::new(Span::styled(
                format!("▸ {hidden} weaker matches hidden · ctrl-x show"),
                Style::default().fg(app.theme.dim),
            )));
            display_items = out;
        }
    }
    app.highlighter = highlighter;
    let list = List::new(display_items)
        .block(themed_block("results", &app.theme))
        .highlight_style(selection_style(&app.theme));
    let mut state = ListState::default();
    state.select(Some(display_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

/// `path` with a home-directory prefix shortened to `~`; unchanged otherwise.
fn shorten_home(path: &str) -> String {
    match dirs::home_dir() {
        Some(h) => {
            let h = h.to_string_lossy();
            if path.starts_with(h.as_ref()) {
                format!("~{}", &path[h.len()..])
            } else {
                path.to_string()
            }
        }
        None => path.to_string(),
    }
}

/// Final path component, trailing '/' kept for directories.
fn path_name(path: &str) -> String {
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
fn preview_meta(app: &App) -> Option<FileMeta> {
    let row = app.engine.results().get(app.selected)?;
    if let Some(m) = row.meta {
        return Some(m);
    }
    if app.status_path == row.path
        && let Some((_, len, modified)) = app.status_meta
    {
        let mtime = modified
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        return Some(FileMeta { mtime, size: len });
    }
    None
}

/// Kind label for the preview header: uppercased extension, or DIR / FILE.
fn kind_label(path: &str) -> String {
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

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    app.poll_preview();
    app.load_preview();
    let block = themed_block("preview", &app.theme);
    let inner = block.inner(area);
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
        if !matches!(&app.preview, PreviewContent::Lines(_))
            && let Some((w, h)) = app.preview_image_dims
        {
            meta_line.push(Span::styled(format!(" · {w}×{h}"), dim));
        }
        if let Some(meta) = preview_meta(app) {
            meta_line.push(Span::styled(format!(" · {}", human_size(meta.size)), dim));
            if let Some(age) = row_age(Some(meta)) {
                meta_line.push(Span::styled(format!(" · {age}"), dim));
            }
        }
        if let PreviewContent::Lines(lines) = &app.preview
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
    match &mut app.preview {
        PreviewContent::Lines(lines) => {
            let total = lines.len();
            let visible = body.height as usize;
            if total > visible && visible > 1 {
                // the last body row shows the position line, so the content
                // gets one row less
                let content_rows = visible - 1;
                app.preview_scroll = app.preview_scroll.min(total.saturating_sub(content_rows));
                let shown: Vec<Line<'static>> = lines
                    .iter()
                    .skip(app.preview_scroll)
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
                let first = app.preview_scroll + 1;
                let last = (app.preview_scroll + content_rows).min(total);
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
                    .position(app.preview_scroll);
                let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight).style(dim);
                frame.render_stateful_widget(bar, body, &mut bar_state);
            } else {
                app.preview_scroll = 0;
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

fn draw_status(frame: &mut Frame, app: &mut App, area: Rect) {
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
fn gauge_cells(done: usize, total: usize, width: usize) -> usize {
    (done * width / total).min(width)
}

/// `s` clipped to `max` chars, with a trailing "…" when clipped.
fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let clipped: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{clipped}…")
    }
}

/// Floating confirmation toast, top-right of the body.
fn draw_toast(frame: &mut Frame, app: &mut App, body: Rect) {
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
        app.input_cursor = app.input.len();
        app.engine.set_query(&app.input, app.regex_mode);
    }
    let result = loop {
        app.engine.tick();
        let len = app.visible_len();
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
        let spans = spans_with_styles("abcd", &[1, 2], Style::default(), hl);
        let texts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["a", "bc", "d"]);
        assert_eq!(spans[1].style, hl);
        assert_eq!(spans[0].style, Style::default());
        // no positions → single plain span
        assert_eq!(
            spans_with_styles("abcd", &[], Style::default(), hl).len(),
            1
        );
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
                meta: None,
                score: None,
            },
            ResultRow {
                path: "/a/fresh.txt".into(),
                line_number: None,
                line: None,
                recent_open: false,
                meta: None,
                score: None,
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
    fn ctrl_t_toggles_row_density() {
        let mut app = test_app();
        assert_eq!(app.density, Density::Comfy);
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.density, Density::Compact);
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.density, Density::Comfy);
    }

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    #[test]
    fn comfy_rows_render_name_badge_size_and_parent() {
        use crate::engine::ResultRow;
        use crate::walker::FileMeta;
        let mut app = test_app();
        app.engine.inject_results_for_test(vec![ResultRow {
            path: "/a/b/notes.md".into(),
            line_number: None,
            line: None,
            recent_open: false,
            meta: Some(FileMeta {
                mtime: now_secs(),
                size: 2048,
            }),
            score: None,
        }]);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("notes.md"), "name missing");
        assert!(text.contains("MD"), "badge missing");
        assert!(text.contains("2.0 KB"), "size missing");
        assert!(text.contains("/a/b"), "parent missing");
    }

    #[test]
    fn compact_rows_keep_name_and_parent_on_one_line() {
        use crate::engine::ResultRow;
        use crate::walker::FileMeta;
        let mut app = test_app();
        app.density = Density::Compact;
        app.engine.inject_results_for_test(vec![ResultRow {
            path: "/a/b/notes.md".into(),
            line_number: None,
            line: None,
            recent_open: false,
            meta: Some(FileMeta {
                mtime: now_secs(),
                size: 2048,
            }),
            score: None,
        }]);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("notes.md"), "name missing");
        assert!(text.contains("/a/b"), "parent missing");
    }

    #[test]
    fn badges_map_extensions_and_kinds() {
        let theme = crate::theme::resolve("default", None);
        let (pd, jpeg, dir, file, gz) = (
            badge_for("/x/a.pdf", theme.badges, theme.accent),
            badge_for("/x/photo.jpeg", theme.badges, theme.accent),
            badge_for("/x/dir/", theme.badges, theme.accent),
            badge_for("/x/noext", theme.badges, theme.accent),
            badge_for("/x/a.tar.gz", theme.badges, theme.accent),
        );
        assert_eq!(pd, ("PDF".to_string(), Color::Yellow));
        assert_eq!(jpeg, ("JPEG".to_string(), Color::Cyan));
        // directories use the theme accent rather than a hardcoded blue
        assert_eq!(dir, ("DIR".to_string(), theme.accent));
        assert_eq!(file, ("FILE".to_string(), Color::DarkGray));
        assert_eq!(gz, ("GZ".to_string(), Color::Red));
        // a catppuccin palette flows through too
        let cp = crate::theme::resolve("catppuccin", None);
        let (label, color) = badge_for("/x/pic.png", cp.badges, cp.accent);
        assert_eq!(label, "PNG");
        assert_eq!(color, cp.badges[0]);
    }

    #[test]
    fn content_rows_render_line_number_and_text() {
        use crate::engine::ResultRow;
        let mut app = test_app();
        app.engine.inject_results_for_test(vec![ResultRow {
            path: "/a/b/notes.rs".into(),
            line_number: Some(3),
            line: Some("let needle = 1;".into()),
            recent_open: false,
            meta: None,
            score: None,
        }]);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("needle"), "matched line missing");
        assert!(text.contains(":3"), "line number missing");
    }

    #[test]
    fn ctrl_r_toggles_regex_mode() {
        let mut app = test_app();
        assert!(!app.regex_mode);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.regex_mode);
    }

    #[test]
    fn insert_at_cursor_keeps_cursor_after_typed_char() {
        let mut app = test_app();
        app.input = "ab".to_string();
        app.input_cursor = 1;
        app.insert_char('X');
        assert_eq!(app.input, "aXb");
        assert_eq!(app.input_cursor, 2);
    }

    #[test]
    fn cursor_arrows_move_across_multibyte_chars() {
        let mut app = test_app();
        app.input = "héllo".to_string(); // 'é' is two bytes
        app.input_cursor = app.input.len();
        // stepping left crosses the two-byte 'é' exactly once
        for expected in [5usize, 4, 3, 1, 0] {
            app.cursor_left();
            assert_eq!(app.input_cursor, expected, "left step");
        }
        // stepping right from 0 crosses 'é' in one char-width jump
        for expected in [1usize, 3, 4, 5, 6] {
            app.cursor_right();
            assert_eq!(app.input_cursor, expected, "right step");
        }
        // moving left at the start / right at the end is a no-op
        app.cursor_start();
        app.cursor_left();
        assert_eq!(app.input_cursor, 0);
        app.cursor_end();
        app.cursor_right();
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[test]
    fn ctrl_w_deletes_previous_word() {
        let mut app = test_app();
        app.input = "needle   haystack".to_string();
        app.input_cursor = app.input.len();
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "needle   ");
        assert_eq!(app.input_cursor, "needle   ".len());
        // readline ctrl-w also eats the trailing whitespace
        app.delete_word_backward();
        assert_eq!(app.input, "");
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn ctrl_d_deletes_char_under_cursor() {
        let mut app = test_app();
        app.input = "abcd".to_string();
        app.input_cursor = 1;
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "acd");
        assert_eq!(app.input_cursor, 1);
    }

    #[test]
    fn ctrl_a_e_jump_to_ends() {
        let mut app = test_app();
        app.input = "fsearch".to_string();
        app.input_cursor = 3;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(app.input_cursor, 0);
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[test]
    fn page_keys_scroll_preview_text_clamped() {
        let mut app = test_app();
        app.preview_scroll = 5;
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.preview_scroll, 25);
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.preview_scroll, 5);
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.preview_scroll, 0); // saturates at zero
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.preview_scroll, 0);
    }

    #[test]
    fn query_spans_light_up_prefix_and_filter_tokens() {
        let spans = query_spans("> ext:md TODO", Color::Yellow);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "> ext:md TODO");
        assert_eq!(spans.len(), 5);
        // the '>' mode prefix lights up in the accent
        assert_eq!(spans[0].content.as_ref(), ">");
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        // whitespace stays raw, a live filter token turns yellow
        assert_eq!(spans[1].content.as_ref(), " ");
        assert_eq!(spans[2].content.as_ref(), "ext:md");
        assert_eq!(spans[2].style.fg, Some(Color::Yellow));
        // plain tokens stay unstyled
        assert_eq!(spans[3].content.as_ref(), " ");
        assert_eq!(spans[4].content.as_ref(), "TODO");
        assert_eq!(spans[4].style, Style::default());
        // '?' lights up too; typos like changed:soon stay plain; kind:image lives
        let spans = query_spans("? notes changed:soon kind:image", Color::Yellow);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "? notes changed:soon kind:image");
        let fgs: Vec<Option<Color>> = spans.iter().map(|s| s.style.fg).collect();
        assert_eq!(fgs[0], Some(Color::Yellow)); // '?'
        assert_eq!(fgs[2], None); // notes
        assert_eq!(fgs[4], None); // changed:soon is a typo
        assert_eq!(fgs[6], Some(Color::Yellow)); // kind:image
    }

    #[test]
    fn gauge_cells_counts_filled_cells() {
        assert_eq!(gauge_cells(0, 100, 12), 0);
        assert_eq!(gauge_cells(50, 100, 12), 6);
        assert_eq!(gauge_cells(200, 100, 12), 12);
    }

    #[test]
    fn toast_renders_then_auto_expires() {
        let mut app = test_app();
        app.message = Some(("copied: /a/b".into(), Instant::now()));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(buffer_text(&terminal).contains("copied"));
        // an old toast is dropped on draw instead of rendered
        app.message = Some((
            "copied: /a/b".into(),
            Instant::now().checked_sub(Duration::from_secs(3)).unwrap(),
        ));
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(!buffer_text(&terminal).contains("copied"));
        assert!(app.message.is_none());
    }

    #[test]
    fn keypress_dismisses_toast() {
        let mut app = test_app();
        app.message = Some(("copied: /a/b".into(), Instant::now()));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.message.is_none());
    }

    #[test]
    fn toast_error_renders_red_without_checkmark() {
        let mut app = test_app();
        app.message = Some(("error: no such file".into(), Instant::now()));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("error: no such file"));
        assert!(!text.contains("✓"));
    }

    #[test]
    fn preview_header_shows_name_and_line_count() {
        use crate::engine::ResultRow;
        let mut app = test_app();
        app.preview_layout = PreviewLayout::Full; // keeps the buffer header-only
        app.engine.inject_results_for_test(vec![ResultRow {
            path: "/a/b/notes.md".into(),
            line_number: None,
            line: None,
            recent_open: false,
            meta: Some(FileMeta {
                mtime: now_secs(),
                size: 2048,
            }),
            score: None,
        }]);
        let lines: Vec<Line<'static>> =
            (1..=100).map(|i| Line::from(format!("line {i}"))).collect();
        // hand the preview pane ready-made content plus the matching cache key
        // so load_preview keeps it instead of replacing it with "loading..."
        app.preview_for = Some(("/a/b/notes.md".into(), None));
        app.preview = PreviewContent::Lines(lines);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("notes.md"), "header filename missing");
        assert!(text.contains("/a/b/"), "header parent path missing");
        assert!(text.contains("100 lines"), "line count missing");
        assert!(text.contains("2.0 KB"), "size missing");
    }

    #[test]
    fn preview_position_indicator_overflows_short_pane() {
        use crate::engine::ResultRow;
        let mut app = test_app();
        app.preview_layout = PreviewLayout::Full;
        app.engine.inject_results_for_test(vec![ResultRow {
            path: "/a/b/notes.md".into(),
            line_number: None,
            line: None,
            recent_open: false,
            meta: None,
            score: None,
        }]);
        let lines: Vec<Line<'static>> =
            (1..=100).map(|i| Line::from(format!("line {i}"))).collect();
        app.preview_for = Some(("/a/b/notes.md".into(), None));
        app.preview = PreviewContent::Lines(lines);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        // body = 17 inner rows minus 2 header rows = 15; content gets 14,
        // the bottom row shows 1–14 / 100
        assert!(text.contains("1–14 / 100"), "position indicator missing");
    }

    #[test]
    fn borderless_theme_renders_input_title() {
        let mut app = test_app();
        app.theme.borders = BorderKind::None;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("fsearch"),
            "input title missing with borderless chrome"
        );
        // the preview pane still shows its label line
        assert!(text.contains("preview"));
    }

    #[test]
    fn rounded_borders_render_rounded_corners() {
        let mut app = test_app();
        app.theme.borders = BorderKind::Rounded;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("╭"), "rounded corner missing");
        assert!(!text.contains("┌"), "sharp corner still present");
    }

    #[test]
    fn score_bar_fill_counts_round() {
        // 0.5 * 5 = 2.5 rounds half-away-from-zero to 3
        assert_eq!(score_bar(0.0).0, 0);
        assert_eq!(score_bar(0.5).0, 3);
        assert_eq!(score_bar(1.0).0, 5);
        assert_eq!(score_bar(1.5).0, 5); // clamps at 5
        // 0.7 * 5 = 3.5 -> rounds to 4
        assert_eq!(score_bar(0.7).0, 4);
        // every bar is exactly 5 cells
        assert_eq!(score_bar(0.35).1.chars().count(), 5);
    }
}
