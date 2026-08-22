use crate::actions;
use crate::engine::{Engine, Mode};
use crate::highlight::{self, Appearance};
use crate::images;
use crate::matcher::Highlighter;
use crate::theme::{BorderKind, Theme};
use crate::util::human_size;
use crate::walker::FileMeta;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
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

/// One entry of the results list as laid out on screen, for mouse hit
/// testing. `Row(i)` is an engine result; `Header` and `Fold` are the
/// decorative rows (launch sections, weaker-matches divider, fold row).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Slot {
    Header,
    Fold,
    Row(usize),
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
    /// Command keybindings (configurable via `[keys]` in config.toml).
    pub keymap: crate::keymap::Keymap,
    /// Results list scroll state (selection + visual offset); persisted so
    /// its offset reflects real scroll for mouse hit testing.
    pub list_state: ListState,
    /// Display-order hit-test map for the results list: (slot, row height).
    pub slots: Vec<(Slot, u16)>,
    /// Inner rect of the results block, or Rect::default() when hidden.
    pub results_area: Rect,
    /// Inner rect of the preview block, or Rect::default() when hidden.
    pub preview_area: Rect,
    /// Last single click on a result row: (row index, when) for double-click.
    last_click: Option<(usize, Instant)>,
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

impl App {
    pub fn new(engine: Engine) -> App {
        let (preview_tx, preview_rx) = mpsc::channel::<PreviewRequest>();
        let (result_tx, result_rx) = mpsc::channel::<PreviewResult>();
        // one preview worker for the app's lifetime; exits when the app
        // (and thus preview_tx) is dropped. Per-request work is panic-guarded:
        // image decoding, SVG rendering, and syntect highlighting all run in
        // here, and a dead worker would leave previews stuck on "loading..."
        std::thread::spawn(move || {
            while let Ok(req) = preview_rx.recv() {
                let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    preview_payload(&req)
                }))
                .unwrap_or_else(|_| PreviewPayload::Lines(vec![Line::from("(preview failed)")]));
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
            keymap: crate::keymap::Keymap::default(),
            list_state: ListState::default(),
            slots: Vec::new(),
            results_area: Rect::default(),
            preview_area: Rect::default(),
            last_click: None,
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
        // text editing is fixed and handled before the keymap, so those keys
        // can never be rebound (see fsearch::keymap::is_editing_key)
        match key.code {
            KeyCode::Left => self.cursor_left(),
            KeyCode::Right if self.input_cursor < self.input.len() => self.cursor_right(),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_start();
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_end();
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_word_backward();
                self.refresh_query();
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_forward();
                self.refresh_query();
            }
            KeyCode::Backspace => {
                self.delete_backward();
                self.refresh_query();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char(c);
                self.refresh_query();
            }
            _ => {
                if let Some(action) = self.keymap.lookup(key.code, key.modifiers)
                    && !self.run_action(action)
                {
                    return false;
                }
            }
        }
        true
    }

    /// Applies a keymap action; returns false when the app should quit.
    fn run_action(&mut self, action: crate::keymap::Action) -> bool {
        match action {
            crate::keymap::Action::Quit => return false,
            crate::keymap::Action::Open => return self.activate_selected(),
            crate::keymap::Action::Menu => {
                if !self.engine.is_filter() && !self.engine.results().is_empty() {
                    self.menu = Some(0);
                }
            }
            crate::keymap::Action::QuickLook => {
                if !self.engine.is_filter() {
                    self.act(actions::quick_look, "quick look");
                }
            }
            crate::keymap::Action::CopyPath => self.act(actions::copy, "copied"),
            crate::keymap::Action::Reveal => {
                if !self.engine.is_filter() {
                    self.act(actions::reveal, "revealed");
                }
            }
            crate::keymap::Action::ClearQuery => {
                self.input.clear();
                self.input_cursor = 0;
                self.refresh_query();
            }
            crate::keymap::Action::RegexToggle => {
                self.regex_mode = !self.regex_mode;
                self.refresh_query();
            }
            crate::keymap::Action::HistoryPrev => self.history_step(true),
            crate::keymap::Action::HistoryNext => self.history_step(false),
            crate::keymap::Action::MoveUp => self.move_selection(-1),
            crate::keymap::Action::MoveDown => self.move_selection(1),
            crate::keymap::Action::PreviewLayout => {
                self.preview_layout = self.preview_layout.next()
            }
            crate::keymap::Action::DensityToggle => self.density = self.density.toggle(),
            crate::keymap::Action::FoldToggle => {
                self.show_weak = !self.show_weak;
                // folding the weaker tail back up clamps onto the last strong row
                if !self.show_weak {
                    let len = self.visible_len();
                    if self.selected >= len && len > 0 {
                        self.selected = len - 1;
                    }
                }
            }
            crate::keymap::Action::PreviewPageUp => {
                self.preview_scroll = self.preview_scroll.saturating_sub(PREVIEW_SCROLL_PAGE);
            }
            crate::keymap::Action::PreviewPageDown => {
                self.preview_scroll = self.preview_scroll.saturating_add(PREVIEW_SCROLL_PAGE);
            }
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
        // the calculator's "path" is the result — enter copies it
        if self.engine.mode() == Mode::Calc {
            self.message = Some((
                match actions::copy(&path) {
                    Ok(()) => format!("copied: {path}"),
                    Err(e) => format!("error: {e}"),
                },
                Instant::now(),
            ));
            return;
        }
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

    /// The Enter behavior, shared by the Enter key and a double-click:
    /// open the selection (Open mode) or return it and exit (`--pick`).
    /// Returns false when the app should quit.
    fn activate_selected(&mut self) -> bool {
        match self.ui_mode {
            UiMode::Open => {
                self.push_history();
                self.open_selected();
                true
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
                    false
                } else {
                    true
                }
            }
        }
    }

    /// Mouse dispatch. Returns false only to mirror `handle_key`'s quit
    /// contract (a double-click in `--pick` mode returns the selection).
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> bool {
        let point = Position {
            x: ev.column,
            y: ev.row,
        };
        match ev.kind {
            MouseEventKind::ScrollDown => {
                if self.results_area.contains(point) {
                    self.move_selection(1);
                } else if self.preview_area.contains(point) {
                    self.preview_scroll = self.preview_scroll.saturating_add(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if self.results_area.contains(point) {
                    self.move_selection(-1);
                } else if self.preview_area.contains(point) {
                    self.preview_scroll = self.preview_scroll.saturating_sub(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // any left click closes the actions popup
                if self.menu.is_some() {
                    self.menu = None;
                    return true;
                }
                if self.results_area.contains(point) {
                    return self.click_results(ev.row);
                }
            }
            _ => {}
        }
        true
    }

    /// Maps a click row inside the results block onto a slot, walking the
    /// visible slots (from `list_state.offset()`) and accumulating heights.
    /// Returns false when the click activated a selection in `--pick` mode.
    fn click_results(&mut self, row: u16) -> bool {
        let y_rel = row.saturating_sub(self.results_area.y);
        let mut cursor_y = 0u16;
        for (slot, h) in self.slots.iter().skip(self.list_state.offset()).copied() {
            if y_rel < cursor_y + h {
                return match slot {
                    Slot::Row(i) => {
                        let now = Instant::now();
                        let double = self.last_click.is_some_and(|(prev, at)| {
                            prev == i && at.elapsed() < Duration::from_millis(450)
                        });
                        self.last_click = Some((i, now));
                        self.selected = i;
                        if double {
                            self.activate_selected()
                        } else {
                            true
                        }
                    }
                    Slot::Fold => {
                        self.show_weak = !self.show_weak;
                        // folding the weaker tail back up clamps onto the last strong row
                        if !self.show_weak {
                            let len = self.visible_len();
                            if self.selected >= len && len > 0 {
                                self.selected = len - 1;
                            }
                        }
                        true
                    }
                    Slot::Header => true,
                };
            }
            cursor_y += h;
        }
        true
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
        // directories (trailing '/') and macOS .app bundles are both
        // directories without a text preview
        if row.path.ends_with('/') || row.path.to_ascii_lowercase().ends_with(".app") {
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

fn restore_terminal(mouse: bool) {
    if let Ok(mut tty) = open_tty() {
        if mouse {
            let _ = execute!(tty, DisableMouseCapture);
        }
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
    keymap: crate::keymap::Keymap,
    mouse: bool,
) -> anyhow::Result<Option<String>> {
    let (traits, picker) = probe_terminal();
    highlight::preload();
    let mut tty = open_tty()?;
    enable_raw_mode()?;
    if mouse {
        execute!(tty, EnterAlternateScreen, EnableMouseCapture)?;
    } else {
        execute!(tty, EnterAlternateScreen)?;
    }
    // Restore the terminal only for a real crash on the UI thread. Worker
    // threads contain their own panics (a guarded pdf-extract panic is
    // suppressed entirely), and yanking the alternate screen out from under
    // a still-running UI is far worse than a garbled message.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if crate::pdf::in_extract_guard() {
            return;
        }
        if std::thread::current().name() == Some("main") {
            restore_terminal(mouse);
        }
        default_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(tty))?;
    let mut app = App::new(engine);
    app.keymap = keymap;
    app.appearance = traits.appearance;
    app.picker = picker;
    app.ui_mode = ui_mode;
    app.theme = theme;
    if app.engine.is_filter() {
        // filter rows are arbitrary lines, not real files: start with the
        // preview hidden (Tab still cycles, previews of real files work)
        app.preview_layout = PreviewLayout::Hidden;
    }
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
                match event::read() {
                    Ok(Event::Key(key)) if key.is_press() => {
                        if !app.handle_key(key) {
                            break Ok(());
                        }
                    }
                    // only ever fires when mouse capture is on
                    Ok(Event::Mouse(m)) => {
                        if !app.handle_mouse(m) {
                            break Ok(());
                        }
                    }
                    Ok(_) => {}
                    Err(e) => break Err(e.into()),
                }
            }
            Ok(false) => {}
            Err(e) => break Err(e.into()),
        }
    };
    restore_terminal(mouse);
    let _ = std::panic::take_hook(); // drop the restoring hook
    result.map(|_| app.picked)
}

mod chrome;
mod preview;
mod rows;
#[cfg(test)]
mod tests;
#[allow(unused_imports)]
use self::{chrome::*, preview::*, rows::*};
