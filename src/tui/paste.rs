//! Paste text as one editor operation, never as a stream of command keys.
use super::{App, Editor};

const MAX_PASTED_QUERY_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq)]
struct PasteEdit {
    changed: bool,
    truncated: bool,
}

impl Editor {
    fn insert_paste(&mut self, text: &str) -> PasteEdit {
        let available = MAX_PASTED_QUERY_BYTES.saturating_sub(self.input.len());
        let mut inserted = String::with_capacity(text.len().min(available));
        let mut chars = text.chars().peekable();
        let mut truncated = false;
        while let Some(ch) = chars.next() {
            let ch = match ch {
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    ' '
                }
                '\n' | '\t' | '\u{0085}' | '\u{2028}' | '\u{2029}' => ' ',
                ch if ch.is_control() => continue,
                ch => ch,
            };
            if inserted.len() + ch.len_utf8() > available {
                truncated = true;
                break;
            }
            inserted.push(ch);
        }
        let changed = !inserted.is_empty();
        if changed {
            // One insertion avoids repeatedly shifting the suffix and dispatching
            // a search per pasted character. The cursor stays on a UTF-8 boundary.
            self.input.insert_str(self.input_cursor, &inserted);
            self.input_cursor += inserted.len();
        }
        PasteEdit { changed, truncated }
    }
}

impl App {
    /// Returns whether the screen needs a redraw. Newlines never apply a saved
    /// query, activate a result, run an action, or choose a transfer destination.
    pub(super) fn handle_paste(&mut self, text: &str) -> bool {
        if self.help.open || self.menu.is_some() {
            return false;
        }
        let edit = if let Some(picker) = &mut self.saved_picker {
            let edit = picker.editor.insert_paste(text);
            if edit.changed {
                picker.filter(&self.saved_searches);
            }
            edit
        } else {
            let edit = self.editor.insert_paste(text);
            if edit.changed {
                self.refresh_query();
            }
            edit
        };
        if edit.truncated {
            self.set_message(
                "paste truncated: query text is limited to 64 KiB when pasting".into(),
            );
        } else if edit.changed {
            self.message = None;
        }
        edit.changed || edit.truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use ratatui::widgets::ListState;

    fn editor(input: &str, cursor: usize) -> Editor {
        Editor {
            input: input.into(),
            input_cursor: cursor,
            input_scroll: 0,
        }
    }

    #[test]
    fn batch_paste_inserts_unicode_at_cursor_and_never_inserts_controls() {
        let mut editor = editor("ab界", 2);
        assert_eq!(
            editor.insert_paste("é\r\n中\t\0\x1b\x03\u{2028}x"),
            PasteEdit {
                changed: true,
                truncated: false
            }
        );
        assert_eq!(editor.input, "abé 中  x界");
        assert_eq!(editor.input_cursor, "abé 中  x".len());
        assert!(!editor.input.chars().any(char::is_control));
        assert!(editor.input.is_char_boundary(editor.input_cursor));
    }

    #[test]
    fn paste_budget_is_byte_bounded_and_does_not_split_unicode_or_lose_suffix() {
        let original = "a".repeat(MAX_PASTED_QUERY_BYTES - 4);
        let mut editor = editor(&original, 0);
        assert_eq!(
            editor.insert_paste("界éxyz"),
            PasteEdit {
                changed: true,
                truncated: true
            }
        );
        assert!(editor.input.starts_with('界'));
        assert!(editor.input.ends_with(&original));
        assert!(editor.input.len() <= MAX_PASTED_QUERY_BYTES);
        assert_eq!(editor.input_cursor, 3);
        let mut full = self::editor(&"x".repeat(MAX_PASTED_QUERY_BYTES), 0);
        assert_eq!(
            full.insert_paste("a"),
            PasteEdit {
                changed: false,
                truncated: true
            }
        );
        assert_eq!(full.input_cursor, 0);
    }

    #[test]
    fn paste_never_activates_results_and_respects_modal_editors() {
        let mut app = App::new(Engine::from_lines(vec!["meeting notes".into()]));
        assert!(app.handle_paste("meeting\r\nnotes\x03"));
        assert_eq!(app.editor.input, "meeting notes");
        assert!(app.picked.is_none());
        app.help.open = true;
        assert!(!app.handle_paste("ignored"));
        app.help.open = false;
        app.menu = Some(0);
        assert!(!app.handle_paste("ignored"));
        app.menu = None;
        app.configure_saved_searches(std::collections::HashMap::from([
            ("notes".into(), "ext:md meeting".into()),
            ("code".into(), "ext:rs".into()),
        ]));
        app.saved_picker = Some(super::super::saved::SavedPicker {
            editor: editor("", 0),
            matches: vec![0, 1],
            selected: 0,
            list_state: ListState::default(),
        });
        assert!(app.handle_paste("notes"));
        let picker = app.saved_picker.as_ref().unwrap();
        assert_eq!(picker.editor.input, "notes");
        assert_eq!(picker.matches.len(), 1);
        assert_eq!(app.saved_searches[picker.matches[0]].0, "notes");
        assert_eq!(
            app.editor.input, "meeting notes",
            "saved filter must not run the live query"
        );
        assert!(app.picked.is_none());
    }

    #[test]
    fn destination_paste_only_edits_the_filter_and_never_starts_a_transfer() {
        let mut app = App::new(Engine::from_lines(vec!["destination/".into()]));
        app.destination_picker = Some(super::super::DestinationPicker {
            kind: crate::actions::TransferKind::Copy,
            paths: vec!["synthetic-source.txt".into()],
            previous_query: "previous query".into(),
            previous_selected: 0,
            previous_show_weak: false,
        });
        assert!(app.handle_paste("destination\r\n"));
        assert_eq!(app.editor.input, "destination ");
        assert!(app.destination_picker.is_some());
        assert!(app.transfer_job.is_none());
        assert!(app.picked.is_none());
    }

    #[test]
    fn empty_control_only_and_truncated_pastes_have_explicit_outcomes() {
        let mut app = App::new(Engine::from_lines(Vec::new()));
        assert!(!app.handle_paste(""));
        assert!(!app.handle_paste("\0\x1b\x03"));
        assert!(app.handle_paste(&"x".repeat(MAX_PASTED_QUERY_BYTES + 10)));
        assert_eq!(app.editor.input.len(), MAX_PASTED_QUERY_BYTES);
        assert!(app.message.as_ref().unwrap().0.contains("paste truncated"));
    }
}
