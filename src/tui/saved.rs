//! Saved queries stay separate from the live editor until explicitly applied.
use super::{App, Editor};
use crate::keymap::Action;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};

pub(super) struct SavedPicker {
    pub(super) editor: Editor,
    pub(super) matches: Vec<usize>,
    pub(super) selected: usize,
    pub(super) list_state: ListState,
}

impl SavedPicker {
    fn new(count: usize) -> Self {
        Self {
            editor: Editor {
                input: String::new(),
                input_cursor: 0,
                input_scroll: 0,
            },
            matches: (0..count).collect(),
            selected: 0,
            list_state: ListState::default(),
        }
    }

    fn filter(&mut self, entries: &[(String, String)]) {
        let needle = self.editor.input.to_lowercase();
        self.matches = entries
            .iter()
            .enumerate()
            .filter(|(_, (name, query))| {
                name.to_lowercase().contains(&needle) || query.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
        self.list_state = ListState::default();
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if !self.matches.is_empty() {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.matches.len() as isize) as usize;
        }
    }
}

impl App {
    pub(super) fn configure_saved_searches(
        &mut self,
        searches: std::collections::HashMap<String, String>,
    ) {
        self.saved_searches = searches.into_iter().collect();
        self.saved_searches.sort_by(|a, b| a.0.cmp(&b.0));
    }

    pub(super) fn saved_searches_enabled(&self) -> bool {
        !self.engine.is_filter() && self.destination_picker.is_none() && self.transfer_job.is_none()
    }

    pub(super) fn open_saved_searches(&mut self) {
        if !self.saved_searches_enabled() {
            return;
        }
        self.menu = None;
        self.help.open = false;
        self.help.scroll = 0;
        self.hit_test.last_click = None;
        self.saved_picker = Some(SavedPicker::new(self.saved_searches.len()));
    }

    fn apply_saved_search(&mut self) {
        let query = self.saved_picker.as_ref().and_then(|picker| {
            picker
                .matches
                .get(picker.selected)
                .and_then(|&index| self.saved_searches.get(index))
                .map(|(_, query)| query.clone())
        });
        let Some(query) = query else { return };
        self.saved_picker = None;
        self.editor.input = query;
        self.editor.input_cursor = self.editor.input.len();
        self.editor.input_scroll = 0;
        self.regex_mode = false;
        self.refresh_query();
    }

    pub(super) fn handle_saved_key(&mut self, key: KeyEvent) {
        let action = self.keymap.lookup(key.code, key.modifiers);
        let Some(picker) = &mut self.saved_picker else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.saved_picker = None,
            KeyCode::Enter => self.apply_saved_search(),
            KeyCode::Up => picker.move_selection(-1),
            KeyCode::Down => picker.move_selection(1),
            KeyCode::Left => picker.editor.cursor_left(),
            KeyCode::Right => picker.editor.cursor_right(),
            KeyCode::Home => picker.editor.cursor_start(),
            KeyCode::End => picker.editor.cursor_end(),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.editor.cursor_start();
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.editor.cursor_end();
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.editor.delete_word_backward();
                picker.filter(&self.saved_searches);
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.editor.delete_forward();
                picker.filter(&self.saved_searches);
            }
            KeyCode::Backspace | KeyCode::Delete => {
                if key.code == KeyCode::Backspace {
                    picker.editor.delete_backward();
                } else {
                    picker.editor.delete_forward();
                }
                picker.filter(&self.saved_searches);
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                picker.editor.insert_char(c);
                picker.filter(&self.saved_searches);
            }
            _ => match action {
                Some(Action::MoveUp) => picker.move_selection(-1),
                Some(Action::MoveDown) => picker.move_selection(1),
                Some(Action::Open) => self.apply_saved_search(),
                Some(Action::Quit) => self.saved_picker = None,
                Some(Action::ClearQuery) => {
                    picker.editor.clear();
                    picker.filter(&self.saved_searches);
                }
                _ => {}
            },
        }
    }
}

pub(super) fn draw_saved(frame: &mut Frame, app: &mut App, screen: Rect) {
    let Some(picker) = &mut app.saved_picker else {
        return;
    };
    let width = screen.width.min(88);
    let height = screen.height.min(18);
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let title = format!(
        "saved searches ({}/{})",
        picker.matches.len(),
        app.saved_searches.len()
    );
    let block = super::chrome::themed_block(&title, &app.theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(u16::from(inner.height >= 4)),
    ])
    .split(inner);
    // Slice on Unicode char boundaries, measuring terminal cells rather than
    // bytes. Keep the cursor visible even for wide characters and long filters.
    let editor = &picker.editor;
    let before = &editor.input[..editor.input_cursor];
    let mut start = 0;
    let available = parts[0].width.saturating_sub(1) as usize;
    while Line::from(&before[start..]).width() > available {
        start += before[start..].chars().next().map_or(0, char::len_utf8);
    }
    let cursor = Line::from(&before[start..]).width() as u16;
    frame.render_widget(Paragraph::new(&editor.input[start..]), parts[0]);
    if !parts[0].is_empty() {
        frame.set_cursor_position((parts[0].x + cursor, parts[0].y));
    }
    let dim = Style::default().fg(app.theme.dim);
    if app.saved_searches.is_empty() {
        frame.render_widget(
            Paragraph::new("No saved searches. Add entries to [searches] in config.toml, e.g. recent = \"changed:7d\"")
                .style(dim).wrap(Wrap { trim: false }),
            parts[1],
        );
    } else if picker.matches.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching saved searches").style(dim),
            parts[1],
        );
    } else {
        let items: Vec<ListItem> = picker
            .matches
            .iter()
            .map(|&index| {
                let (name, query) = &app.saved_searches[index];
                ListItem::new(Line::from(vec![
                    Span::styled(name.clone(), Style::default().fg(app.theme.accent)),
                    Span::styled(format!(" — {query}"), dim),
                ]))
            })
            .collect();
        picker.list_state.select(Some(picker.selected));
        frame.render_stateful_widget(
            List::new(items).highlight_style(super::chrome::selection_style(&app.theme)),
            parts[1],
            &mut picker.list_state,
        );
    }
    let open = app
        .keymap
        .shortcut(Action::Open)
        .unwrap_or_else(|| "enter".into());
    frame.render_widget(
        Paragraph::new(format!(
            "type to filter · ↑/↓ select · {open} apply · esc cancel"
        ))
        .style(dim),
        parts[2],
    );
}
