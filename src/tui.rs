use crate::actions;
use crate::engine::{Engine, Mode};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use std::time::Duration;

const PREVIEW_BYTES: usize = 64 * 1024;

pub struct App {
    pub engine: Engine,
    pub input: String,
    pub selected: usize,
    pub regex_mode: bool,
    pub show_preview: bool,
    pub message: Option<String>,
    preview_for: Option<(String, Option<u64>)>,
    preview_lines: Vec<String>,
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
            preview_for: None,
            preview_lines: Vec::new(),
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
            (KeyCode::Enter, _) => self.act(actions::open, "opened"),
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
            self.preview_lines = vec!["no selection".to_string()];
            return;
        };
        let key = (row.path.clone(), row.line_number);
        if self.preview_for.as_ref() == Some(&key) {
            return;
        }
        self.preview_for = Some(key);
        self.preview_lines = match std::fs::read(&row.path) {
            Ok(bytes) if bytes.contains(&0) => vec!["(binary file)".to_string()],
            Ok(mut bytes) => {
                bytes.truncate(PREVIEW_BYTES);
                let text = String::from_utf8_lossy(&bytes);
                let all: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                match row.line_number {
                    // center the preview on the matching line
                    Some(n) => {
                        let start = (n as usize).saturating_sub(6);
                        all.into_iter()
                            .enumerate()
                            .skip(start)
                            .take(40)
                            .map(|(i, l)| format!("{:>5} {l}", i + 1))
                            .collect()
                    }
                    None => all.into_iter().take(100).collect(),
                }
            }
            Err(e) => vec![format!("(unreadable: {e})")],
        };
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

fn draw_results(frame: &mut Frame, app: &App, area: Rect) {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let items: Vec<ListItem> = app
        .engine
        .results()
        .iter()
        .map(|r| {
            let shown = match &home {
                Some(h) if r.path.starts_with(h) => format!("~{}", &r.path[h.len()..]),
                _ => r.path.clone(),
            };
            match (r.line_number, &r.line) {
                (Some(n), Some(line)) => ListItem::new(Line::from(vec![
                    Span::styled(format!("{shown}:{n} "), Style::default().fg(Color::Cyan)),
                    Span::raw(line.clone()),
                ])),
                _ => ListItem::new(shown),
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
    let text: Vec<Line> = app
        .preview_lines
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();
    let preview =
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("preview"));
    frame.render_widget(preview, area);
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

pub fn run(engine: Engine) -> anyhow::Result<()> {
    let mut terminal = ratatui::init(); // installs a terminal-restoring panic hook
    let mut app = App::new(engine);
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
        let engine = Engine::new(config, dir.path().join("index.bin"));
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
    fn ctrl_r_toggles_regex_mode() {
        let mut app = test_app();
        assert!(!app.regex_mode);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.regex_mode);
    }
}
