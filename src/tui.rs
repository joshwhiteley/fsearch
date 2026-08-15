use crate::actions;
use crate::engine::{Engine, Mode};
use crate::highlight::{self, Appearance};
use crate::images;
use crate::matcher::Highlighter;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui_image::picker::Picker;
use ratatui_image::{StatefulImage, protocol::StatefulProtocol};
use std::time::Duration;

const PREVIEW_BYTES: usize = 64 * 1024;

pub struct App {
    pub engine: Engine,
    pub input: String,
    pub selected: usize,
    pub regex_mode: bool,
    pub show_preview: bool,
    pub message: Option<String>,
    pub appearance: Appearance,
    pub picker: Option<Picker>,
    preview_for: Option<(String, Option<u64>)>,
    preview: PreviewContent,
}

enum PreviewContent {
    Lines(Vec<Line<'static>>),
    Image(Box<StatefulProtocol>),
}

impl App {
    pub fn new(engine: Engine) -> App {
        App {
            engine,
            input: String::new(),
            selected: 0,
            regex_mode: false,
            show_preview: true,
            message: None,
            appearance: Appearance::Dark,
            picker: None,
            preview_for: None,
            preview: PreviewContent::Lines(Vec::new()),
        }
    }

    /// Returns false when the app should quit.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.message = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => return false,
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
            (KeyCode::Tab, _) => self.show_preview = !self.show_preview,
            (KeyCode::Enter, _) => self.open_selected(),
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
        if images::is_image_path(&row.path) {
            self.preview = match (
                &self.picker,
                images::load(&row.path, images::MAX_IMAGE_BYTES),
            ) {
                (Some(picker), Ok(img)) => {
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
                                let gutter = Style::default().fg(Color::DarkGray);
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

pub fn draw(frame: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_input(frame, app, outer[0]);

    if app.show_preview {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(outer[1]);
        draw_results(frame, app, cols[0]);
        draw_preview(frame, app, cols[1]);
    } else {
        draw_results(frame, app, outer[1]);
    }

    draw_status(frame, app, outer[2]);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let mode = match (app.engine.mode(), app.regex_mode) {
        (Mode::Content, _) => "content",
        (_, true) => "regex",
        _ => "fuzzy",
    };
    let input = Paragraph::new(app.input.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("fsearch [{mode}]")),
    );
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
        .fg(Color::Cyan)
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
                    Span::styled(format!("{shown}:{n} "), Style::default().fg(Color::Cyan)),
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
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("results"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    app.load_preview();
    let block = Block::default().borders(Borders::ALL).title("preview");
    match &mut app.preview {
        PreviewContent::Lines(lines) => {
            frame.render_widget(Paragraph::new(lines.clone()).block(block), area);
        }
        PreviewContent::Image(protocol) => {
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_stateful_widget(StatefulImage::default(), inner, protocol.as_mut());
        }
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let s = app.engine.status();
    let mut parts = vec![
        format!("{} files", s.indexed),
        format!("{} matches", s.matches),
    ];
    if s.indexing {
        parts.push("indexing…".to_string());
    }
    if let Some(e) = &s.error {
        parts.push(e.clone());
    }
    if let Some(m) = &app.message {
        parts.push(m.clone());
    }
    let status = Paragraph::new(parts.join(" · ")).style(Style::default().fg(Color::DarkGray));
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

/// Asks the terminal directly whether it speaks the Kitty graphics protocol.
/// Some terminals (e.g. the Herdr multiplexer) ACK Kitty graphics but ignore
/// font-size queries, which makes ratatui-image's own detection give up and
/// fall back to halfblocks. Only call on a terminal that answers queries.
fn kitty_ack_probe() -> bool {
    use ratatui::crossterm::terminal;
    use std::io::{Read, Write};
    if terminal::enable_raw_mode().is_err() {
        return false;
    }
    let mut stdout = std::io::stdout();
    // kitty graphics query, then DA1 (which every real terminal answers)
    let _ = stdout.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
    let _ = stdout.flush();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut chunk = [0u8; 256];
        let mut reply: Vec<u8> = Vec::new();
        loop {
            match stdin.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    reply.extend_from_slice(&chunk[..n]);
                    // DA1 terminates the exchange: ESC [ ? ... c
                    if reply.windows(2).any(|w| w == b"[?") && reply.ends_with(b"c") {
                        let _ = tx.send(reply);
                        break;
                    }
                }
            }
        }
    });
    let reply = rx
        .recv_timeout(Duration::from_millis(800))
        .unwrap_or_default();
    let _ = terminal::disable_raw_mode();
    reply.windows(2).any(|w| w == b"_G")
}

/// Runs the full pre-init terminal probe: appearance, responsiveness, and
/// graphics picker selection (honoring `FSEARCH_IMAGES=off|halfblocks|
/// kitty|iterm2`). Performs stdio queries — call before raw mode / the
/// event loop consumes stdin.
pub fn probe_terminal() -> (highlight::TerminalTraits, Option<Picker>) {
    use ratatui_image::picker::ProtocolType;
    let traits = highlight::detect_terminal();
    let picker = match std::env::var("FSEARCH_IMAGES").as_deref() {
        Ok("off") => None,
        Ok("halfblocks") => Some(Picker::halfblocks()),
        Ok("kitty") => Some(forced_picker(ProtocolType::Kitty)),
        Ok("iterm2") => Some(forced_picker(ProtocolType::Iterm2)),
        _ if traits.responsive => {
            let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
            if picker.protocol_type() == ProtocolType::Halfblocks && kitty_ack_probe() {
                Some(forced_picker(ProtocolType::Kitty))
            } else {
                Some(picker)
            }
        }
        _ => Some(Picker::halfblocks()),
    };
    (traits, picker)
}

pub fn run(engine: Engine) -> anyhow::Result<()> {
    let (traits, picker) = probe_terminal();
    highlight::preload();
    let mut terminal = ratatui::init(); // installs a terminal-restoring panic hook
    let mut app = App::new(engine);
    app.appearance = traits.appearance;
    app.picker = picker;
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
    ratatui::restore();
    result
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
    fn typing_updates_input_and_esc_quits() {
        let mut app = test_app();
        assert!(app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert_eq!(app.input, "a");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
        assert_eq!(app.input, "");
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
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
    fn ctrl_r_toggles_regex_mode() {
        let mut app = test_app();
        assert!(!app.regex_mode);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.regex_mode);
    }
}
