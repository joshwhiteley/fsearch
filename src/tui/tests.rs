use super::chrome::{draw, gauge_cells, human_age, query_spans};
use super::rows::{badge_for, icon_glyph, icon_spans, score_bar, score_readout, spans_with_styles};
use super::{App, Density, PreviewContent, PreviewLayout, Slot, UiMode};
use crate::config::Config;
use crate::engine::Engine;
use crate::theme::BorderKind;
use crate::util::human_size;
use crate::walker::FileMeta;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::ListState;
use std::time::{Duration, Instant};

fn test_app() -> App {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        roots: vec![dir.path().to_path_buf()],
        excludes: vec![],
        max_content_filesize: 1024,
        theme: Default::default(),
        keys: Default::default(),
        mouse: true,
        remember_session: true,
        index_apps: false,
        icons: false,
        quiet: Vec::new(),
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

fn test_filter_app() -> App {
    let mut app = App::new(Engine::from_lines(vec![
        "git commit -m fix/thing".into(),
        "cargo build --release".into(),
        "alpha beta".into(),
    ]));
    app.ui_mode = UiMode::Pick;
    app.preview_layout = PreviewLayout::Hidden;
    app
}

/// Ticks the engine until `pred` holds or we time out, so filter tests can
/// wait for the background filename worker to populate results.
fn tick_until(app: &mut App, pred: impl Fn(&App) -> bool) {
    for _ in 0..200 {
        app.engine.tick();
        if pred(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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
    app.editor.input = "notes".to_string();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("notes"));
    assert!(text.contains("fuzzy"));
}

#[test]
fn hints_show_only_while_input_is_empty() {
    let mut app = test_app();
    let mut terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("grep in files"));
    assert!(text.contains("larger:100mb"));
    assert!(text.contains("tab preview"));
    app.editor.input = "x".to_string();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(!buffer_text(&terminal).contains("grep in files"));
}

#[test]
fn short_terminals_drop_query_hints_then_the_footer() {
    let mut app = test_app();
    // tall enough for everything
    let mut terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains("grep in files"));
    // medium: query hints dropped, the shortcut footer stays
    let mut terminal = Terminal::new(TestBackend::new(48, 10)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(!text.contains("grep in files"), "query hints must yield");
    assert!(text.contains("esc quit"), "footer kept at height 10");
    // very short: everything yields to the results pane
    let mut terminal = Terminal::new(TestBackend::new(48, 6)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(!buffer_text(&terminal).contains("esc quit"));
}

#[test]
fn long_input_scrolls_to_keep_the_cursor_visible() {
    let mut app = test_app();
    // 28 chars against a 22-char visible row (24 cols minus borders)
    app.editor.input = "abcdefghijklmnopqrstuvwxyz01".to_string();
    app.editor.input_cursor = app.editor.input.len();
    let mut terminal = Terminal::new(TestBackend::new(24, 12)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("yz01"), "query tail visible");
    assert!(!text.contains("abcdef"), "clipped head scrolled off");
    // the cursor sits inside the frame, one cell inside the right edge
    let pos = terminal.get_cursor_position().unwrap();
    assert_eq!(pos.x, 22);
    assert!(app.editor.input_scroll > 0);
    // moving the edit cursor back toward the start scrolls back into view:
    // the window starts exactly at the cursor column
    app.editor.input_cursor = 2;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.editor.input_scroll, 2);
    let text = buffer_text(&terminal);
    assert!(text.contains("cdefgh"), "window starts at the cursor");
    assert!(!text.contains("ab"), "chars before the cursor stay clipped");
}

#[test]
fn empty_state_shows_no_matches_and_minimal_footer() {
    use crate::engine::ResultRow;
    let mut app = test_app();
    // no injected rows: the launch screen has nothing to list. Wait patiently:
    // under a loaded test runner the initial walk can exceed tick_until's budget.
    for _ in 0..2000 {
        app.engine.tick();
        if !app.engine.status().indexing {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    app.engine.inject_results_for_test(Vec::<ResultRow>::new());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("(no matches)"), "empty-state message missing");
    assert!(text.contains("esc quit"), "minimal footer missing");
    assert!(text.contains("ctrl-u clear"), "minimal footer missing");
}

#[test]
fn ctrl_g_cycles_presets_with_a_toast() {
    let mut app = test_app();
    // an empty preset counts as "default", so the cycle starts at catppuccin
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert_eq!(app.theme_cfg.preset, "catppuccin");
    assert_eq!(
        app.theme.accent,
        crate::theme::resolve("catppuccin", None).accent
    );
    let toast = app
        .message
        .as_ref()
        .expect("cycle raises a toast")
        .0
        .clone();
    assert!(
        toast.contains("catppuccin"),
        "toast names the theme: {toast}"
    );
    // and walks the declaration order, wrapping back to default after slate
    for expected in ["gruvbox", "nord", "tokyonight", "slate", "default"] {
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(app.theme_cfg.preset, expected);
    }
}

#[test]
fn theme_cycle_keeps_hex_overrides() {
    let mut app = test_app();
    app.theme_cfg.accent = Some("#ff0080".into());
    app.theme = crate::theme::resolve_config(&app.theme_cfg);
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert_eq!(app.theme_cfg.preset, "catppuccin");
    // the user's accent override rides along onto the next preset
    assert_eq!(app.theme.accent, Color::Rgb(0xff, 0x00, 0x80));
}

#[test]
fn icons_render_only_when_enabled() {
    use crate::engine::ResultRow;
    let mut app = test_app();
    app.engine.inject_results_for_test(vec![ResultRow {
        path: "/a/b/notes.md".into(),
        line_number: None,
        line: None,
        recent_open: false,
        meta: None,
        score: None,
    }]);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(
        !buffer_text(&terminal).contains('\u{f15c}'),
        "no glyph by default"
    );
    // comfy density shows the doc glyph before the filename
    app.icons = true;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains('\u{f15c}'));
    // compact density too
    app.density = Density::Compact;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains('\u{f15c}'));
}

#[test]
fn icon_glyphs_map_kinds() {
    assert_eq!(icon_glyph("/x/a.rs"), "\u{f121}"); // code
    assert_eq!(icon_glyph("/x/a.py"), "\u{f121}");
    assert_eq!(icon_glyph("/x/report.pdf"), "\u{f15c}"); // doc
    assert_eq!(icon_glyph("/x/notes.md"), "\u{f15c}");
    assert_eq!(icon_glyph("/x/pic.png"), "\u{f1c5}"); // image
    assert_eq!(icon_glyph("/x/movie.mkv"), "\u{f1c8}"); // video
    assert_eq!(icon_glyph("/x/song.flac"), "\u{f1c7}"); // audio
    assert_eq!(icon_glyph("/x/bundle.tgz"), "\u{f1c6}"); // archive
    assert_eq!(icon_glyph("/x/Tools.app"), "\u{f135}"); // launch
    assert_eq!(icon_glyph("/x/folder/"), "\u{f07b}"); // dir-ish
    assert_eq!(icon_glyph("/x/noext"), "\u{f15b}"); // default file
}

#[test]
fn icon_spans_are_empty_when_disabled() {
    let theme = crate::theme::resolve("default", None);
    let (spans, width) = icon_spans("/x/a.rs", theme.badges, theme.accent, false);
    assert!(spans.is_empty());
    assert_eq!(width, 0);
    let (spans, width) = icon_spans("/x/a.rs", theme.badges, theme.accent, true);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content.as_ref(), "\u{f121} ");
    assert_eq!(width, 2);
}

#[test]
fn selected_file_shows_wrapped_contextual_shortcuts() {
    use crate::engine::ResultRow;

    let mut app = test_app();
    app.engine.inject_results_for_test(vec![ResultRow {
        path: "/tmp/report.pdf".to_string(),
        line_number: None,
        line: None,
        recent_open: false,
        meta: None,
        score: None,
    }]);
    let mut terminal = Terminal::new(TestBackend::new(52, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    for expected in [
        "enter open",
        "ctrl-f reveal",
        "ctrl-y copy path",
        "ctrl-space quick look",
        "→ actions",
        "tab preview",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text:?}");
    }
}

#[test]
fn typing_updates_input_and_esc_quits() {
    let mut app = test_app();
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
    assert_eq!(app.editor.input, "a");
    assert!(app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
    assert_eq!(app.editor.input, "");
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
    app.editor.input = "x".to_string();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(!buffer_text(&terminal).contains("RECENT OPENS"));
}

#[test]
fn history_cycles_with_ctrl_p_and_n() {
    let mut app = test_app();
    app.history.entries = vec!["alpha".to_string(), "beta".to_string()];
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input, "beta");
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input, "alpha");
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input, "beta");
    // stepping past the newest clears back to a blank query
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input, "");
    // typing resets the cursor position in history
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input, "beta");
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
fn filter_rows_render_raw_lines_verbatim() {
    let mut app = test_filter_app();
    tick_until(&mut app, |a| a.engine.results().len() >= 3);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    // the full raw line appears; no badge/name split split out an extension
    assert!(text.contains("git commit -m fix/thing"));
    assert!(text.contains("cargo build --release"));
    assert!(!text.contains("THING"), "line was split by a badge");
}

#[test]
fn filter_mode_right_does_not_open_menu() {
    let mut app = test_filter_app();
    app.editor.input = "x".to_string();
    app.editor.input_cursor = app.editor.input.len(); // cursor at end: Right = Menu action
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.menu, None, "menu must not open in filter mode");
}

#[test]
fn filter_input_title_says_filter() {
    let mut app = test_filter_app();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains("fsearch [filter]"));
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
    app.editor.input = "ab".to_string();
    app.editor.input_cursor = 1;
    app.editor.insert_char('X');
    assert_eq!(app.editor.input, "aXb");
    assert_eq!(app.editor.input_cursor, 2);
}

#[test]
fn cursor_arrows_move_across_multibyte_chars() {
    let mut app = test_app();
    app.editor.input = "héllo".to_string(); // 'é' is two bytes
    app.editor.input_cursor = app.editor.input.len();
    // stepping left crosses the two-byte 'é' exactly once
    for expected in [5usize, 4, 3, 1, 0] {
        app.editor.cursor_left();
        assert_eq!(app.editor.input_cursor, expected, "left step");
    }
    // stepping right from 0 crosses 'é' in one char-width jump
    for expected in [1usize, 3, 4, 5, 6] {
        app.editor.cursor_right();
        assert_eq!(app.editor.input_cursor, expected, "right step");
    }
    // moving left at the start / right at the end is a no-op
    app.editor.cursor_start();
    app.editor.cursor_left();
    assert_eq!(app.editor.input_cursor, 0);
    app.editor.cursor_end();
    app.editor.cursor_right();
    assert_eq!(app.editor.input_cursor, app.editor.input.len());
}

#[test]
fn ctrl_w_deletes_previous_word() {
    let mut app = test_app();
    app.editor.input = "needle   haystack".to_string();
    app.editor.input_cursor = app.editor.input.len();
    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input, "needle   ");
    assert_eq!(app.editor.input_cursor, "needle   ".len());
    // readline ctrl-w also eats the trailing whitespace
    app.editor.delete_word_backward();
    assert_eq!(app.editor.input, "");
    assert_eq!(app.editor.input_cursor, 0);
}

#[test]
fn ctrl_d_deletes_char_under_cursor() {
    let mut app = test_app();
    app.editor.input = "abcd".to_string();
    app.editor.input_cursor = 1;
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input, "acd");
    assert_eq!(app.editor.input_cursor, 1);
}

#[test]
fn ctrl_a_e_jump_to_ends() {
    let mut app = test_app();
    app.editor.input = "fsearch".to_string();
    app.editor.input_cursor = 3;
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input_cursor, 0);
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.input_cursor, app.editor.input.len());
}

#[test]
fn page_keys_scroll_preview_text_clamped() {
    let mut app = test_app();
    app.preview.scroll = 5;
    app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    assert_eq!(app.preview.scroll, 25);
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(app.preview.scroll, 5);
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(app.preview.scroll, 0); // saturates at zero
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(app.preview.scroll, 0);
}

fn test_row(path: &str) -> crate::engine::ResultRow {
    crate::engine::ResultRow {
        path: path.into(),
        line_number: None,
        line: None,
        recent_open: false,
        meta: None,
        score: None,
    }
}

fn mouse(kind: MouseEventKind, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: 5,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn mouse_state() -> App {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![test_row("/a"), test_row("/b"), test_row("/c")]);
    app.hit_test.results_area = Rect::new(0, 3, 40, 20);
    app.hit_test.preview_area = Rect::new(40, 3, 40, 20);
    // comfy heights: rows at y 0-1, 2-3, 4-5
    app.hit_test.slots = vec![(Slot::Row(0), 2), (Slot::Row(1), 2), (Slot::Row(2), 2)];
    app.list_state = ListState::default();
    app
}

#[test]
fn mouse_wheel_in_results_moves_selection() {
    let mut app = mouse_state();
    assert!(app.handle_mouse(mouse(MouseEventKind::ScrollDown, 10)));
    assert_eq!(app.selected, 1);
    assert!(app.handle_mouse(mouse(MouseEventKind::ScrollUp, 10)));
    assert_eq!(app.selected, 0);
}

#[test]
fn mouse_wheel_in_preview_scrolls_preview() {
    let mut app = mouse_state();
    app.preview.scroll = 5;
    // columns 40..80 land in the preview pane
    let down = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 60,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(down));
    assert_eq!(app.preview.scroll, 8);
    let up = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 60,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(up));
    assert_eq!(app.preview.scroll, 5);
}

#[test]
fn click_selects_row_without_opening() {
    let mut app = mouse_state();
    // row index 1 spans absolute y 5-6 (results_area.y = 3, y_rel 2-3)
    assert!(app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 6)));
    assert_eq!(app.selected, 1);
    assert!(app.message.is_none(), "single click must not open");
    assert!(app.picked.is_none());
}

#[test]
fn click_on_fold_row_toggles_show_weak() {
    let mut app = mouse_state();
    app.hit_test.slots = vec![(Slot::Row(0), 2), (Slot::Fold, 1), (Slot::Row(1), 2)];
    // the fold row sits at y_rel 2 (absolute y 5)
    assert!(app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5)));
    assert!(app.show_weak, "fold click should reveal weaker matches");
    assert!(app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5)));
    assert!(!app.show_weak, "second fold click folds back");
}

#[test]
fn click_while_menu_open_closes_menu() {
    let mut app = mouse_state();
    app.menu = Some(2);
    // a click outside the (not yet rendered) popup area closes the menu
    assert!(app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1)));
    assert_eq!(app.menu, None);
}

#[test]
fn click_on_menu_entry_activates_it() {
    let mut app = mouse_state();
    app.menu = Some(2); // "copy path"
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let area = app.menu_area;
    assert!(area.width > 0, "draw must record the popup hit rect");
    // third entry row: below the top border, two item rows down
    let ev = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 5,
        row: area.y + 3,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(ev));
    assert_eq!(app.menu, None);
    assert!(app.message.is_some(), "the clicked action must have run");
}

#[test]
fn click_on_menu_border_or_outside_closes_without_action() {
    let mut app = mouse_state();
    app.menu = Some(0);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let area = app.menu_area;
    // the title border row closes the popup but runs no action
    let border = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 5,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(border));
    assert_eq!(app.menu, None);
    assert!(app.message.is_none(), "border click must not run an action");
    // a click well away from the popup closes it too
    app.menu = Some(0);
    let outside = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 23,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(outside));
    assert_eq!(app.menu, None);
    assert!(app.message.is_none());
}

#[test]
fn double_click_opens_selection() {
    let mut app = mouse_state();
    app.ui_mode = UiMode::Pick;
    // first click selects, second click (within 450 ms) activates
    assert!(app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 6)));
    assert!(app.picked.is_none());
    assert!(!app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 6)));
    assert_eq!(app.picked.as_deref(), Some("/b"));
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
    let lines: Vec<Line<'static>> = (1..=100).map(|i| Line::from(format!("line {i}"))).collect();
    // hand the preview pane ready-made content plus the matching cache key
    // so load_preview keeps it instead of replacing it with "loading..."
    app.preview.for_key = Some(("/a/b/notes.md".into(), None));
    app.preview.content = PreviewContent::Lines(lines);
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
    let lines: Vec<Line<'static>> = (1..=100).map(|i| Line::from(format!("line {i}"))).collect();
    app.preview.for_key = Some(("/a/b/notes.md".into(), None));
    app.preview.content = PreviewContent::Lines(lines);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    // Wrapped query and contextual help leave 12 preview-body rows;
    // content gets 11 and the bottom row shows its position.
    assert!(text.contains("1–11 / 100"), "position indicator missing");
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

#[test]
fn score_readout_never_splits_mid_char() {
    // regression: split_at(filled) used a char count as a byte index and
    // panicked for every partial fill (the bar glyphs are 3 bytes each)
    for s in [0.0f32, 0.1, 0.2, 0.4, 0.5, 0.7, 0.87, 0.99, 1.0] {
        let (width, spans) = score_readout(s, Color::Cyan, Style::default());
        let text: String = spans.iter().map(|sp| sp.content.as_ref()).collect();
        let bar: String = text.chars().take(5).collect();
        assert_eq!(bar.chars().count(), 5, "bar for {s}");
        assert!(bar.chars().all(|c| c == '\u{25b0}' || c == '\u{25b1}'));
        assert_eq!(width, text.chars().count(), "width for {s}");
    }
}

#[test]
fn semantic_scored_rows_render_without_panicking() {
    use crate::engine::ResultRow;
    let mut app = test_app();
    // 0.4 -> filled = 2, the exact crash case from the field report
    app.engine.inject_results_for_test(vec![ResultRow {
        path: "/docs/essay.md".into(),
        line_number: Some(3),
        line: Some("patience compounds".into()),
        recent_open: false,
        meta: None,
        score: Some(0.4),
    }]);
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("essay.md"));
    assert!(text.contains("40%"));
    // compact density hits the same readout on the single-line path
    app.density = Density::Compact;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
}

#[test]
fn calc_mode_renders_expression_and_result() {
    let mut app = test_app();
    app.editor.input = "= 2*(3+4)".to_string();
    app.editor.input_cursor = app.editor.input.len();
    app.engine.set_query(&app.editor.input, false);
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("calc"), "mode label");
    assert!(text.contains("2*(3+4) ="), "expression");
    assert!(text.contains("14"), "result");
    // an unfinished expression shows no rows and no error
    app.editor.input = "= 2*".to_string();
    app.editor.input_cursor = app.editor.input.len();
    app.engine.set_query(&app.editor.input, false);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.engine.results().len(), 0);
    assert!(app.engine.status().error.is_none());
}

fn file_row(path: &str) -> crate::engine::ResultRow {
    crate::engine::ResultRow {
        path: path.into(),
        line_number: None,
        line: None,
        recent_open: false,
        meta: Some(FileMeta {
            mtime: now_secs(),
            size: 10,
        }),
        score: None,
    }
}

#[test]
fn ctrl_b_toggles_a_mark_and_renders_the_gutter_indicator() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/notes.md"), file_row("/b/other.txt")]);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(!text.contains('▌'), "no marks initially");
    assert!(text.contains("ctrl-b mark"), "mark shortcut missing");
    // mark the focused row
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(app.marks.contains("/a/notes.md"));
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains('▌'), "mark indicator shown");
    assert!(text.contains("1 marked"), "status count");
    assert!(text.contains("ctrl-b unmark"), "unmark shortcut missing");
    assert!(
        text.contains("alt-b clear (1 marked)"),
        "clear shortcut count missing"
    );
    // toggling again clears the mark and the indicator disappears
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(app.marks.is_empty());
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(!buffer_text(&terminal).contains('▌'));
}

#[test]
fn marks_survive_moving_the_selection() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/one.md"), file_row("/b/two.md")]);
    // mark row 0, move down without marking, move back: the mark stays
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.selected, 1);
    assert_eq!(app.marks, ["/a/one.md".to_string()].into());
    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.selected, 0);
    assert!(app.marks.contains("/a/one.md"));
}

#[test]
fn menu_batch_entries_appear_only_with_visible_marks() {
    use crate::engine::ResultRow;
    let mut app = test_app();
    assert!(
        !app.menu_entries().contains(&"open marked"),
        "no batch entries without marks"
    );
    app.engine.inject_results_for_test(vec![
        ResultRow {
            path: "/a/x.pdf".into(),
            line_number: None,
            line: None,
            recent_open: false,
            meta: None,
            score: None,
        },
        ResultRow {
            path: "/b/y.pdf".into(),
            line_number: None,
            line: None,
            recent_open: false,
            meta: None,
            score: None,
        },
        ResultRow {
            path: "/c/z.pdf".into(),
            line_number: None,
            line: None,
            recent_open: false,
            meta: None,
            score: None,
        },
    ]);
    app.marks = ["/a/x.pdf".to_string(), "/c/z.pdf".to_string()].into();
    let entries = app.menu_entries();
    for label in [
        "open marked",
        "copy marked paths",
        "trash marked",
        "clear marks",
    ] {
        assert!(entries.contains(&label), "{label} missing");
    }
    // batch copy content: visible marked paths in display order,
    // newline-joined — exactly what goes to the clipboard
    assert_eq!(app.visible_marked(), vec!["/a/x.pdf", "/c/z.pdf"]);
    assert_eq!(app.visible_marked().join("\n"), "/a/x.pdf\n/c/z.pdf");
}

#[test]
fn clear_marks_action_and_menu_entry_clear_the_set() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/notes.md"), file_row("/b/other.txt")]);
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert_eq!(app.marks.len(), 2);
    // alt-b clears everything with a toast
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
    assert!(app.marks.is_empty());
    assert!(app.message.is_some());
    // and via its menu entry (the last one when marks are visible)
    app.marks = ["/a/notes.md".to_string()].into();
    app.menu = Some(app.menu_entries().len() - 1); // last entry: clear marks
    app.run_menu_action(app.menu.unwrap());
    assert!(app.marks.is_empty());
    assert!(app.marks.is_empty());
}

#[test]
fn mark_indicator_coexists_with_icons_in_both_densities() {
    let mut app = test_app();
    app.icons = true;
    app.engine
        .inject_results_for_test(vec![file_row("/a/notes.rs")]);
    app.marks.insert("/a/notes.rs".to_string());
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(
        text.contains('▌'),
        "mark indicator missing in comfy density"
    );
    assert!(
        text.contains('\u{f121}'),
        "nerd-font icon missing in comfy density"
    );
    app.density = Density::Compact;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(
        text.contains('▌'),
        "mark indicator missing in compact density"
    );
    assert!(
        text.contains('\u{f121}'),
        "nerd-font icon missing in compact density"
    );
}

#[test]
fn batch_runner_reports_partial_failures_and_continues() {
    let paths = vec!["/ok".to_string(), "/bad".to_string(), "/ok2".to_string()];
    let outcome = super::run_batch(&paths, |path| {
        if path == "/bad" {
            Err(std::io::Error::other("permission denied"))
        } else {
            Ok(())
        }
    });
    assert_eq!(outcome.succeeded, 2);
    let Some((path, error)) = &outcome.first_error else {
        panic!("expected the failed path and error");
    };
    assert_eq!(path, "/bad");
    assert_eq!(error, "permission denied");
    let summary = super::batch_summary("trashed", paths.len(), &outcome);
    assert!(summary.contains("trashed 2/3 files"));
    assert!(summary.contains("1 failed"));
    assert!(summary.contains("/bad: permission denied"));
}

#[test]
fn hidden_selected_rows_are_not_actionable() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/visible.txt")]);
    app.selected = 1;
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(app.marks.is_empty());
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(app.menu.is_none(), "hidden selection must not open actions");
}

#[test]
fn hidden_marks_remain_clearable_without_batch_actions() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/visible.txt")]);
    app.marks.insert("/hidden/file.txt".to_string());
    let entries = app.menu_entries();
    assert!(entries.contains(&"clear marks"));
    assert!(!entries.contains(&"open marked"));
    assert!(!entries.contains(&"trash marked"));
}

#[test]
fn duplicate_marked_content_hits_are_processed_once() {
    let mut app = test_app();
    app.engine.inject_results_for_test(vec![
        crate::engine::ResultRow {
            path: "/a/notes.md".into(),
            line_number: Some(1),
            line: Some("first".into()),
            recent_open: false,
            meta: None,
            score: None,
        },
        crate::engine::ResultRow {
            path: "/a/notes.md".into(),
            line_number: Some(2),
            line: Some("second".into()),
            recent_open: false,
            meta: None,
            score: None,
        },
    ]);
    app.marks.insert("/a/notes.md".to_string());
    assert_eq!(app.visible_marked_count(), 1);
    assert_eq!(app.visible_marked(), vec!["/a/notes.md"]);
}

#[test]
fn filter_mode_hides_marking_entirely() {
    let mut app = test_filter_app();
    tick_until(&mut app, |a| a.engine.results().len() >= 3);
    // ctrl-b does nothing in filter mode
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(app.marks.is_empty(), "no marks in filter mode");
    // even a forced mark renders no indicator or status count
    app.marks = ["git commit -m fix/thing".to_string()].into();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(!text.contains('▌'), "filter rows show no mark gutter");
    assert!(!text.contains("marked"), "status hides the mark count");
    // --pick mode hides marking too: toggling does nothing
    let mut app = test_app();
    app.ui_mode = UiMode::Pick;
    app.engine
        .inject_results_for_test(vec![file_row("/a/notes.md")]);
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(app.marks.is_empty(), "no marks in pick mode");
}
