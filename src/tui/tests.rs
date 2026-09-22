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
        remember_history: true,
        searches: Default::default(),
        index_apps: false,
        icons: false,
        unified: true,
        quiet: Vec::new(),
        actions: Vec::new(),
        action_warning: None,
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

fn wait_for_transfer(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.transfer_job.is_some() && Instant::now() < deadline {
        app.poll_transfer();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.transfer_job.is_none(), "transfer did not complete");
}

#[test]
fn home_itself_renders_in_rows_and_preview_without_slicing_original_name() {
    let home = dirs::home_dir().unwrap();
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row(&format!("{}/", home.display()))]);
    let mut terminal = Terminal::new(TestBackend::new(50, 10)).unwrap();
    for density in [Density::Comfy, Density::Compact] {
        app.density = density;
        terminal
            .draw(|f| super::rows::draw_results(f, &mut app, Rect::new(0, 0, 50, 10)))
            .unwrap();
        assert!(buffer_text(&terminal).contains("~/"));
    }
    terminal
        .draw(|f| super::preview::draw_preview(f, &mut app, Rect::new(0, 0, 50, 10)))
        .unwrap();
    assert!(buffer_text(&terminal).contains("~/"));
}

#[test]
fn action_popup_captures_all_mouse_events_and_freezes_commands_and_targets() {
    let mut app = mouse_state();
    let mut row = file_row("/original/main.rs");
    row.line_number = Some(29);
    app.engine
        .inject_results_for_test(vec![row, file_row("/other/file.txt")]);
    app.open_menu();
    let entries = app.menu_entries();
    let trash = entries
        .iter()
        .position(|e| e.label == "move to trash")
        .unwrap();
    let nvim = entries
        .iter()
        .position(|e| e.label == "open in nvim")
        .unwrap();
    for (x, y) in [(2, 5), (42, 5), (90, 30)] {
        for kind in [
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollUp,
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
        ] {
            app.handle_mouse(MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            });
            assert_eq!(app.selected, 0);
            assert_eq!(app.preview.scroll, 0);
            assert!(app.menu.is_some());
        }
    }
    // Arrival of a non-source result would remove the nvim entry and change
    // the meaning of every subsequent menu index without a frozen menu.
    app.engine
        .inject_results_for_test(vec![file_row("/new/file.pdf")]);
    assert_eq!(app.menu_entries(), entries);
    assert_eq!(
        app.menu_entries()[trash].command,
        super::MenuCommand::BuiltIn(super::BuiltInAction::Trash)
    );
    assert_eq!(
        app.visible_selected_row().unwrap().path,
        "/original/main.rs"
    );
    app.run_menu_action(nvim);
    assert_eq!(
        app.nvim_request,
        Some(("/original/main.rs".into(), Some(29)))
    );
    assert!(app.menu_snapshot.is_none());
    assert_eq!(app.visible_selected_row().unwrap().path, "/new/file.pdf");
}

#[test]
fn popup_preview_header_metadata_body_and_title_keep_the_frozen_target() {
    let mut app = test_app();
    let mut original = file_row("/original/a.rs");
    original.meta = Some(FileMeta { size: 3, mtime: 0 });
    app.engine.inject_results_for_test(vec![original]);
    let (requests, request_rx) = std::sync::mpsc::channel();
    let (replies, reply_rx) = std::sync::mpsc::channel();
    app.preview.tx = requests;
    app.preview.rx = reply_rx;
    app.open_menu();
    app.load_preview();
    let request = request_rx.try_recv().unwrap();
    let mut replacement = file_row("/replacement/b.txt");
    replacement.meta = Some(FileMeta {
        size: 5000,
        mtime: 0,
    });
    app.engine.inject_results_for_test(vec![replacement]);
    replies
        .send(super::PreviewResult {
            generation: request.generation,
            path: request.path,
            line_number: request.line_number,
            payload: super::PreviewPayload::Lines(vec![Line::from("original body")]),
        })
        .unwrap();
    app.load_preview();
    app.poll_preview();
    assert!(request_rx.try_recv().is_err());
    assert_eq!(app.visible_selected_row().unwrap().path, "/original/a.rs");
    let mut terminal = Terminal::new(TestBackend::new(70, 15)).unwrap();
    terminal
        .draw(|f| super::preview::draw_preview(f, &mut app, Rect::new(0, 0, 70, 15)))
        .unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("/original/a.rs"));
    assert!(text.contains("original body"));
    assert!(text.contains("3 B"));
    assert!(!text.contains("b.txt"));
    terminal
        .draw(|f| super::chrome::draw_menu(f, &mut app, Rect::new(0, 0, 70, 15)))
        .unwrap();
    assert!(buffer_text(&terminal).contains("actions · /original/a.rs"));
    app.close_menu();
    app.load_preview();
    assert_eq!(request_rx.try_recv().unwrap().path, "/replacement/b.txt");
}

#[test]
fn marked_move_uses_popup_targets_not_later_results_or_marks() {
    let mut app = test_app();
    app.engine.inject_results_for_test(vec![
        file_row("/original/a.txt"),
        file_row("/original/b.txt"),
    ]);
    app.marks = ["/original/a.txt".into(), "/original/b.txt".into()].into();
    app.open_menu();
    let entry = app
        .menu_entries()
        .iter()
        .position(|e| e.label == "move marked to…")
        .unwrap();
    app.engine
        .inject_results_for_test(vec![file_row("/new/c.txt")]);
    app.marks = ["/new/c.txt".into()].into();
    app.run_menu_action(entry);
    assert_eq!(
        app.destination_picker.as_ref().unwrap().paths,
        ["/original/a.txt", "/original/b.txt"]
    );
}

#[test]
fn action_worker_is_bounded_responsive_and_reports_completion_and_failure() {
    let mut app = test_app();
    let ui = std::thread::current().id();
    let (started, started_rx) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    app.start_action(move |_| {
        assert_ne!(ui, std::thread::current().id());
        started.send(()).unwrap();
        release_rx.recv().unwrap();
        "error: injected backend failure".into()
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    app.start_action(|_| panic!("a second action must not be queued"));
    assert!(app.message.as_ref().unwrap().0.contains("already running"));
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
    assert_eq!(app.editor.input, "x");
    app.poll_action();
    assert!(app.action_job.is_some());
    release.send(()).unwrap();
    wait_for_action(&mut app);
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "error: injected backend failure"
    );
    app.start_action(|_| "copied: test path".into());
    wait_for_action(&mut app);
    assert_eq!(app.message.as_ref().unwrap().0, "copied: test path");
    app.start_action(|_| panic!("injected worker panic"));
    wait_for_action(&mut app);
    assert!(app.message.as_ref().unwrap().0.contains("worker stopped"));
}

fn wait_for_action(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.action_job.is_some() {
        app.poll_action();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(unix)]
#[test]
fn action_shutdown_kills_and_reaps_an_actual_blocked_child_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let mut app = test_app();
    let worker_path = pid_file.clone();
    app.start_action(move |cancel| {
        let result = crate::actions::checked_command_cancellable(
            std::process::Command::new("sh")
                .args(["-c", "echo $$ > \"$1\"; exec sleep 10", "sh"])
                .arg(worker_path),
            None,
            Duration::from_secs(10),
            cancel,
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
        "cancelled".into()
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = text.trim().parse::<libc::pid_t>()
        {
            break pid;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    let start = Instant::now();
    drop(app.action_job.take());
    assert!(start.elapsed() < Duration::from_secs(1));
    // Observing process absence cannot accidentally reap it on our behalf.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
fn action_shutdown_requests_cancellation_and_joins_current_worker() {
    let mut app = test_app();
    let (finished, finished_rx) = std::sync::mpsc::channel();
    app.start_action(move |cancel| {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cancel.load(std::sync::atomic::Ordering::Relaxed) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        finished.send(()).unwrap();
        "cancelled".into()
    });
    drop(app.action_job.take());
    finished_rx.try_recv().unwrap();
}

#[test]
fn selection_metadata_loads_with_previews_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("selected");
    std::fs::write(&file, b"abc").unwrap();
    let mut app = test_app();
    app.preview_layout = PreviewLayout::Hidden;
    app.engine
        .inject_results_for_test(vec![file_row(file.to_str().unwrap())]);
    app.load_preview();
    assert!(app.preview.for_key.is_none());
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.status.meta.is_none() {
        app.refresh_status();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(app.status.meta.unwrap().1, 3);
    app.engine.inject_results_for_test(Vec::new());
    app.refresh_status();
    assert!(app.status.meta.is_none());
}

#[test]
fn query_cursor_uses_terminal_cells_and_reserves_an_insertion_cell() {
    for (input, width, scroll, cursor_x) in [
        ("界a", 12, 0, 4),
        ("e\u{301}x", 12, 0, 3),
        ("abcdef", 8, 1, 6), // six text cells fill the six-cell viewport
        ("界界界", 8, 1, 6),
        ("e\u{301}abcde", 8, 1, 6),
    ] {
        let mut app = test_app();
        app.editor.input = input.into();
        app.editor.input_cursor = input.len();
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(app.editor.input_scroll, scroll, "{input}");
        assert_eq!(cursor.x, cursor_x, "{input}");
        assert_eq!(
            terminal.backend().buffer()[(cursor.x, cursor.y)].symbol(),
            " ",
            "cursor must sit after the text: {input}"
        );
    }
}

#[test]
fn content_highlight_cache_survives_repeated_frames() {
    let mut app = test_app();
    app.preview_layout = PreviewLayout::Hidden;
    app.editor.input = ">needle".into();
    app.engine.set_query(&app.editor.input, false);
    let mut row = file_row("/a/b.txt");
    row.line = Some("a needle here".into());
    row.line_number = Some(7);
    app.engine.inject_results_for_test(vec![row]);
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let first = terminal.backend().buffer().clone();
    assert!(app.highlights.content.is_some());
    for _ in 0..3 {
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(app.highlights.content.is_some());
        assert_eq!(terminal.backend().buffer(), &first);
    }
}

#[test]
fn filter_and_failed_exits_do_not_replace_ordinary_session_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.toml");
    crate::session::save(&path, "full", "compact");
    let original = std::fs::read(&path).unwrap();
    let app = test_filter_app();
    app.save_session(&path, true, true);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let app = test_app();
    app.save_session(&path, true, false);
    app.save_session(&path, false, true);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    app.save_session(&path, true, true);
    let state = crate::session::load(&path);
    assert_eq!(state.preview_layout.as_deref(), Some("side"));
    assert_eq!(state.density.as_deref(), Some("comfy"));
}

#[test]
fn terminal_guard_restores_on_early_error_after_partial_setup() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut output = tempfile::tempfile().unwrap();
    let result: std::io::Result<()> = (|| {
        let mut guard = super::TerminalGuard::new(output.try_clone()?, true);
        // Model setup succeeding only through alternate-screen entry. Do not
        // change the test runner's real terminal modes or global panic hook.
        guard.active = true;
        guard.tty.write_all(b"\x1b[?1049h")?;
        Err(std::io::Error::other("later setup/Terminal::new failed"))
    })();
    assert!(result.is_err());
    output.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = String::new();
    output.read_to_string(&mut bytes).unwrap();
    assert!(bytes.contains("\x1b[?1000l"), "mouse capture disabled");
    assert!(bytes.contains("\x1b[?1049l"), "alternate screen left");
    assert!(bytes.contains("\x1b[?25h"), "cursor restored");
}

#[test]
fn terminal_cleanup_continues_after_a_write_failure() {
    #[derive(Default)]
    struct FailFirstWrite {
        writes: usize,
        bytes: Vec<u8>,
    }
    impl std::io::Write for FailFirstWrite {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            if self.writes == 1 {
                return Err(std::io::Error::other("injected failure"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = FailFirstWrite::default();
    super::cleanup_terminal(&mut output, true);
    let bytes = String::from_utf8(output.bytes).unwrap();
    assert!(bytes.contains("\x1b[?1049l"));
    assert!(bytes.contains("\x1b[?25h"));
}

#[test]
fn private_history_policy_clears_and_disables_memory_and_disk_history() {
    let mut app = test_app();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queries");
    std::fs::write(&path, "existing\n").unwrap();
    app.history.file = Some(path.clone());
    app.history.entries = vec!["existing".into()];
    app.configure_history(false);
    app.editor.input = "private needle".into();
    app.push_history();
    app.history_step(true);
    assert!(app.history.entries.is_empty());
    assert!(app.history.file.is_none());
    assert_eq!(app.editor.input, "private needle");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "existing\n");
}

#[test]
fn hidden_previews_do_not_queue_and_hide_invalidates_pending_work() {
    let mut app = test_app();
    let (tx, rx) = std::sync::mpsc::channel();
    app.preview.tx = tx;
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/directory/")]);
    app.preview_layout = PreviewLayout::Hidden;
    app.load_preview();
    assert!(rx.try_recv().is_err());
    app.preview_layout = PreviewLayout::Side;
    app.load_preview();
    let req = rx.try_recv().unwrap();
    assert_eq!(req.path, "/tmp/directory/");
    assert!(
        matches!(&app.preview.content, PreviewContent::Lines(lines) if lines[0] == Line::from("loading..."))
    );
    app.preview_layout = PreviewLayout::Hidden;
    app.load_preview();
    assert!(app.preview.generation > req.generation);
    assert!(app.preview.for_key.is_none());
    assert!(rx.try_recv().is_err());
}

#[test]
fn latest_queued_preview_wins() {
    let (tx, rx) = std::sync::mpsc::channel();
    for generation in 1..=100 {
        tx.send(super::PreviewRequest {
            generation,
            path: generation.to_string(),
            line_number: None,
            appearance: crate::highlight::Appearance::Dark,
            gutter: Color::Gray,
        })
        .unwrap();
    }
    let request = super::latest_preview_request(&rx).unwrap();
    assert_eq!(request.generation, 100);
    assert!(rx.try_recv().is_err());
}

#[test]
fn nvim_matched_line_is_before_option_terminator_and_path() {
    assert_eq!(
        super::nvim_args("/tmp/-danger.rs", Some(27)),
        ["+27", "--", "/tmp/-danger.rs"]
    );
    assert_eq!(
        super::nvim_args("/tmp/plain.rs", None),
        ["--", "/tmp/plain.rs"]
    );
    let mut app = test_app();
    let mut row = file_row("/tmp/main.rs");
    row.line_number = Some(27);
    app.engine.inject_results_for_test(vec![row]);
    let entry = app
        .menu_entries()
        .iter()
        .position(|entry| entry.label == "open in nvim")
        .unwrap();
    app.run_menu_action(entry);
    assert_eq!(app.nvim_request, Some(("/tmp/main.rs".into(), Some(27))));
    assert_eq!(app.matched_line("/tmp/main.rs"), Some(27));
}

#[test]
fn transfer_busy_state_blocks_duplicate_jobs_and_esc_cancels() {
    // Keep completion pending without relying on file size or timing.
    let mut app = test_app();
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    app.transfer_job = Some(super::TransferJob {
        kind: crate::actions::TransferKind::Copy,
        total: 3,
        done: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        cancel: cancel.clone(),
        rx,
        worker: None,
    });
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/marked.rs")]);
    app.marks.insert("/tmp/marked.rs".into());
    app.enter_destination_picker(crate::actions::TransferKind::Move);
    assert!(app.destination_picker.is_none());
    app.start_transfer(
        Vec::new(),
        "/tmp".into(),
        crate::actions::TransferKind::Move,
    );
    assert_eq!(app.transfer_job.as_ref().unwrap().total, 3);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains("transferring 1/3 files"));
    assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
    tx.send(crate::actions::TransferOutcome {
        cancelled: 2,
        ..Default::default()
    })
    .unwrap();
    app.poll_transfer();
    assert!(app.transfer_job.is_none());
    assert!(app.message.as_ref().unwrap().0.contains("cancelled 2"));
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
    app.editor.input = "missing-result".into();
    app.editor.input_cursor = app.editor.input.len();
    app.refresh_query();
    // Wait for both the initial walk and current query to settle.
    // Under a loaded runner the initial walk can exceed tick_until's budget.
    for _ in 0..2000 {
        app.engine.tick();
        let status = app.engine.status();
        if !status.indexing && !status.searching {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!app.engine.status().indexing && !app.engine.status().searching);
    app.engine.inject_results_for_test(Vec::<ResultRow>::new());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("No matches"), "empty-state message missing");
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
    // and walks the declaration order, wrapping back to default after forge
    for expected in ["gruvbox", "nord", "tokyonight", "slate", "forge", "default"] {
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

#[test]
fn selection_anchor_survives_result_reranking() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![test_row("/a"), test_row("/b")]);
    app.move_selection(1);
    app.engine
        .inject_results_for_test(vec![test_row("/b"), test_row("/a")]);
    app.restore_selection_anchor();
    assert_eq!(app.selected, 0);
    assert_eq!(app.engine.results()[app.selected].path, "/b");
}

#[test]
fn second_content_hit_stays_selected_and_opens_at_its_line() {
    let mut app = test_app();
    app.engine.set_query(">needle", false);
    assert_eq!(app.engine.mode(), crate::engine::Mode::Content);
    let mut first = file_row("/tmp/main.rs");
    first.line_number = Some(1);
    let mut second = first.clone();
    second.line_number = Some(20);
    app.engine.inject_results_for_test(vec![first, second]);
    app.move_selection(1);
    app.engine.tick();
    app.restore_selection_anchor();
    assert_eq!(app.selected, 1);
    assert!(app.selection_anchor.is_none());
    assert_eq!(app.matched_line("/tmp/main.rs"), Some(20));
    let entry = app
        .menu_entries()
        .iter()
        .position(|entry| entry.label == "open in nvim")
        .unwrap();
    app.run_menu_action(entry);
    assert_eq!(app.nvim_request, Some(("/tmp/main.rs".into(), Some(20))));
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
#[ignore = "synthetic redraw timing; run explicitly with --nocapture"]
fn result_redraw_benchmark() {
    let mut app = test_app();
    app.preview_layout = PreviewLayout::Hidden;
    app.editor.input = "synthetic".into();
    app.show_weak = true;
    for count in [500, 1000] {
        app.engine.inject_results_for_test(
            (0..count)
                .map(|i| test_row(&format!("/synthetic/project/src/module_{i:04}.rs")))
                .collect(),
        );
        assert_eq!(app.visible_len(), count);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let started = Instant::now();
        for _ in 0..100 {
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        }
        eprintln!("{count} rows: {:?}/frame", started.elapsed() / 100);
    }
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
fn mouse_wheel_stops_at_results_edges_but_keys_still_wrap() {
    let mut app = mouse_state();
    assert!(app.handle_mouse(mouse(MouseEventKind::ScrollUp, 10)));
    assert_eq!(app.selected, 0);
    assert_eq!(app.selection_anchor.as_deref(), Some("/a"));
    app.move_selection(-1);
    assert_eq!(app.selected, 2, "keyboard navigation still wraps");
    assert!(app.handle_mouse(mouse(MouseEventKind::ScrollDown, 10)));
    assert_eq!(app.selected, 2);
    assert_eq!(app.selection_anchor.as_deref(), Some("/c"));
    app.move_selection(1);
    assert_eq!(app.selected, 0);

    app.engine.inject_results_for_test(vec![]);
    for kind in [MouseEventKind::ScrollDown, MouseEventKind::ScrollUp] {
        assert!(app.handle_mouse(mouse(kind, 10)));
        assert_eq!(app.selected, 0);
        assert!(app.selection_anchor.is_none());
    }
}

#[test]
fn result_viewport_preserves_global_selection_and_mouse_indices() {
    for density in [Density::Compact, Density::Comfy] {
        let mut app = test_app();
        app.preview_layout = PreviewLayout::Hidden;
        app.density = density;
        app.engine.inject_results_for_test(
            (0..1000)
                .map(|i| {
                    let mut row = test_row(&format!("/synthetic/row_{i:04}.rs"));
                    row.recent_open = i < 5;
                    row
                })
                .collect(),
        );
        let mut terminal = Terminal::new(TestBackend::new(90, 18)).unwrap();
        for selected in [0, 4, 5, 500, 999, 0] {
            app.selected = selected;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(buffer_text(&terminal).contains(&format!("row_{selected:04}.rs")));
            let display_selected = app.list_state.selected().unwrap();
            assert_eq!(app.hit_test.slots[display_selected].0, Slot::Row(selected));
            assert_eq!(
                display_selected,
                selected + if selected < 5 { 1 } else { 2 }
            );
            let offset = app.list_state.offset();
            let (local, index) = app.hit_test.slots[offset..]
                .iter()
                .enumerate()
                .find_map(|(local, (slot, _))| match slot {
                    Slot::Row(index) => Some((local, *index)),
                    _ => None,
                })
                .unwrap();
            let y = app.hit_test.results_area.y
                + app.hit_test.slots[offset..offset + local]
                    .iter()
                    .map(|(_, height)| *height)
                    .sum::<u16>();
            // A single click must select the global row, not the viewport-local index.
            app.hit_test.last_click = None;
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: app.hit_test.results_area.x,
                row: y,
                modifiers: KeyModifiers::NONE,
            });
            assert_eq!(app.selected, index);
        }
    }
}

#[test]
fn result_viewport_preserves_fold_rows_and_content_hits() {
    let mut app = test_filter_app();
    app.editor.input = "alpha".into();
    app.refresh_query();
    tick_until(&mut app, |app| !app.engine.status().searching);
    assert_eq!(app.engine.strong_count(), 1);
    app.engine.inject_results_for_test(
        (0..1000)
            .map(|i| test_row(&format!("alpha_{i:04}")))
            .collect(),
    );
    let mut terminal = Terminal::new(TestBackend::new(90, 18)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.hit_test.slots, vec![(Slot::Row(0), 1), (Slot::Fold, 1)]);
    assert!(buffer_text(&terminal).contains("999 weaker matches hidden"));
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: app.hit_test.results_area.x,
        row: app.hit_test.results_area.y + 1,
        modifiers: KeyModifiers::NONE,
    });
    assert!(app.show_weak);
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.hit_test.slots[1], (Slot::Header, 1));
    assert!(buffer_text(&terminal).contains("WEAKER MATCHES"));
    app.selected = 999;
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.list_state.selected(), Some(1000));
    assert!(buffer_text(&terminal).contains("alpha_0999"));
    app.run_action(crate::keymap::Action::FoldToggle);
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.selected, 0);
    assert_eq!(app.list_state.offset(), 0);
    assert!(buffer_text(&terminal).contains("999 weaker matches hidden"));
    app.handle_mouse(mouse(
        MouseEventKind::ScrollDown,
        app.hit_test.results_area.y,
    ));
    assert_eq!(app.selected, 0, "wheel cannot enter the folded tail");

    let mut app = test_app();
    app.engine.set_query(">needle", false);
    app.editor.input = ">needle".into();
    app.engine.inject_results_for_test(
        (0..1000)
            .map(|i| {
                let mut row = test_row("/synthetic/hits.rs");
                row.line_number = Some(i + 1);
                row.line = Some(format!("needle hit {i:04}"));
                row
            })
            .collect(),
    );
    app.move_selection(999);
    for density in [Density::Comfy, Density::Compact] {
        app.density = density;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(buffer_text(&terminal).contains("needle hit 0999"));
        assert_eq!(app.list_state.selected(), Some(999));
        app.restore_selection_anchor();
        assert_eq!(app.matched_line("/synthetic/hits.rs"), Some(1000));
    }
}

#[test]
fn result_viewport_blank_tail_is_not_an_invisible_mouse_target() {
    let mut app = mouse_state();
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
    terminal
        .draw(|frame| super::rows::draw_results(frame, &mut app, Rect::new(0, 0, 60, 5)))
        .unwrap();
    assert_eq!(app.hit_test.results_area.height, 3);
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: app.hit_test.results_area.x,
        row: app.hit_test.results_area.y + 2,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.selected, 0, "the next two-line row is not on screen");
    assert!(app.hit_test.last_click.is_none());
}

#[test]
fn result_viewport_handles_tiny_panes_and_resize_without_spilling() {
    let mut app = test_app();
    app.engine.inject_results_for_test(
        (0..1000)
            .map(|i| test_row(&format!("/synthetic/row_{i:04}.rs")))
            .collect(),
    );
    app.selected = 999;
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    for height in [15, 3, 2, 1, 0, 8, 18] {
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("neighbor"),
                    Rect::new(0, 19, 60, 1),
                );
                super::rows::draw_results(frame, &mut app, Rect::new(0, 0, 60, height));
            })
            .unwrap();
        assert!(buffer_text(&terminal).contains("neighbor"));
        if height >= 4 {
            assert!(buffer_text(&terminal).contains("row_0999.rs"));
            assert_eq!(
                app.hit_test.slots[app.list_state.selected().unwrap()].0,
                Slot::Row(999)
            );
        }
    }
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
fn preview_header_shows_name_and_preview_row_count() {
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
    assert!(
        text.contains("100 preview rows"),
        "preview row count missing"
    );
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
    assert!(
        text.contains("1–11 / 100 preview rows"),
        "position indicator missing"
    );
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
fn help_overlay_opens_via_key_and_lists_configured_bindings() {
    let mut app = test_app();
    // remap copy_path so the overlay must reflect configuration, not defaults
    let mut keys = std::collections::HashMap::new();
    keys.insert("copy_path".to_string(), vec!["alt-c".to_string()]);
    app.keymap = crate::keymap::Keymap::from_config(&keys);

    app.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    assert!(app.help.open, "f1 must open the help overlay");
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(app.help.open, "ctrl-o must open the help overlay");

    // Leave room for the full action listing and fixed-editing note.
    let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("help"), "overlay title missing");
    assert!(text.contains("navigation"), "group heading missing");
    assert!(text.contains("query modes"), "group heading missing");
    assert!(text.contains("copy path"), "action row missing");
    assert!(text.contains("alt-c"), "remapped binding missing");
    assert!(text.contains("enter"), "default open binding missing");
    assert!(text.contains("cycle theme"), "theme cycle action missing");
    assert!(text.contains("ctrl-g"), "theme cycle binding missing");
    assert!(text.contains("toggle mark"), "mark action missing");
    assert!(text.contains("ctrl-s"), "mark binding missing");
    assert!(text.contains("clear marks"), "clear-mark action missing");
    assert!(text.contains("alt-s"), "clear-mark binding missing");
    // multiple default bindings are listed together
    assert!(
        text.contains("esc, ctrl-c"),
        "multi-binding row missing: {text}"
    );
    assert!(
        text.contains("editing is fixed"),
        "fixed-editing note missing"
    );

    // esc closes the overlay instead of quitting; a second esc quits
    assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(!app.help.open);
    assert!(!app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
}

#[test]
fn help_overlay_is_centered_and_uses_theme_chrome() {
    let mut app = test_app();
    app.help.open = true;
    app.theme = crate::theme::resolve("catppuccin", None);
    app.theme.borders = BorderKind::Rounded;
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let area = app.help.area;
    assert_eq!(area.x, (120 - area.width) / 2);
    assert_eq!(area.y, (40 - area.height) / 2);
    let top_left = terminal
        .backend()
        .buffer()
        .cell((area.x, area.y))
        .expect("modal is inside the terminal");
    assert_eq!(top_left.symbol(), "╭");
    assert_eq!(top_left.fg, app.theme.border);
}

#[test]
fn any_other_key_closes_help_without_reaching_the_app() {
    let mut app = test_app();
    app.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    // a plain char closes the overlay and must not land in the query
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
    assert!(!app.help.open);
    assert_eq!(app.editor.input, "", "typing while help is open leaked in");
}

#[test]
fn page_keys_scroll_the_help_overlay() {
    let mut app = test_app();
    app.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    assert_eq!(app.help.scroll, 8);
    app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    assert_eq!(app.help.scroll, 16);
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(app.help.scroll, 8);
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(app.help.scroll, 0, "scroll saturates at the top");
    // draw-time clamping keeps scroll inside the content on a tiny screen
    let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
    for _ in 0..20 {
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    }
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let clamped = app.help.scroll;
    // scrolling further past the end cannot move past the clamped offset
    app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.help.scroll, clamped, "scroll must clamp at the bottom");

    // Narrow terminals wrap long action rows instead of clipping their keys.
    app.help.scroll = 0;
    let mut narrow = Terminal::new(TestBackend::new(20, 8)).unwrap();
    narrow.draw(|f| draw(f, &mut app)).unwrap();
    let top = buffer_text(&narrow);
    assert!(top.contains("navigation"));
    let mut seen = top;
    for _ in 0..20 {
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        narrow.draw(|f| draw(f, &mut app)).unwrap();
        seen.push_str(&buffer_text(&narrow));
    }
    assert!(
        seen.contains("cycle theme"),
        "theme action was clipped: {seen:?}"
    );
    assert!(seen.contains("f1"), "help binding was clipped: {seen:?}");
}

#[test]
fn help_mouse_wheel_scrolls_and_clicks_outside_close() {
    use crate::keymap::Keymap;
    let mut app = test_app();
    let mut keys = std::collections::HashMap::new();
    // Many valid bindings make the listing overflow even a tall box. Keep
    // Help on its defaults so F1 still opens the modal below.
    let names = [
        "quit",
        "open",
        "menu",
        "quick_look",
        "copy_path",
        "reveal",
        "clear_query",
        "regex_toggle",
        "history_prev",
        "history_next",
        "move_up",
        "move_down",
        "preview_layout",
        "density_toggle",
        "fold_toggle",
        "preview_page_up",
        "preview_page_down",
    ];
    for (index, name) in names.into_iter().enumerate() {
        let modifier = ["alt", "alt+shift", "ctrl+alt"][index / 6];
        let first = index % 6 + 1;
        keys.insert(
            name.to_string(),
            vec![
                format!("{modifier}-f{first}"),
                format!("{modifier}-f{}", first + 6),
            ],
        );
    }
    app.keymap = Keymap::from_config(&keys);
    app.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.help.area.width > 0, "draw must record the hit rect");

    // wheel anywhere scrolls the modal overlay
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(wheel));
    assert_eq!(app.help.scroll, 3);

    // a click inside does nothing; the overlay stays open
    let inside = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: app.help.area.x + 2,
        row: app.help.area.y + 2,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(inside));
    assert!(app.help.open, "click inside must keep the overlay open");
    assert!(app.message.is_none());

    // a click outside closes it
    let outside = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 23,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(outside));
    assert!(!app.help.open);
    assert_eq!(app.help.scroll, 0, "closing resets the scroll");
}

#[test]
fn footers_advertise_the_help_binding() {
    let mut app = test_app();
    // empty state: minimal footer carries the hint
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(
        buffer_text(&terminal).contains("f1 help"),
        "empty-state footer"
    );

    // with a selection: contextual footer carries it too
    app.engine
        .inject_results_for_test(vec![test_row("/a/report.pdf")]);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("f1 help"), "contextual footer missing help");
    assert!(text.contains("ctrl-y copy path"));

    // a remapped binding changes what both footers advertise
    let mut keys = std::collections::HashMap::new();
    keys.insert("help".to_string(), vec!["f12".to_string()]);
    app.keymap = crate::keymap::Keymap::from_config(&keys);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains("f12 help"));
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
    assert_eq!(app.hit_test.slots, vec![(Slot::Row(0), 1)]);
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

fn custom_action(
    name: &str,
    cmd: &[&str],
    ext: &[&str],
    kind: Option<&str>,
    enter: bool,
) -> crate::config::CustomAction {
    crate::config::CustomAction {
        name: name.into(),
        cmd: cmd.iter().map(|arg| (*arg).into()).collect(),
        ext: ext.iter().map(|value| (*value).into()).collect(),
        kind: kind.map(str::to_string),
        enter,
    }
}

#[test]
fn mark_key_toggles_a_mark_and_renders_the_gutter_indicator() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/notes.md"), file_row("/b/other.txt")]);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(!text.contains('▌'), "no marks initially");
    assert!(text.contains("ctrl-s mark"), "mark shortcut missing");
    // mark the focused row: the mark lands and the selection advances
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(app.marks.contains("/a/notes.md"));
    assert_eq!(app.selected, 1, "marking advances fzf-style");
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m.contains("batch")),
        "first mark points at the batch-action menu"
    );
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains('▌'), "mark indicator shown");
    assert!(text.contains("1 marked"), "status count");
    // back on the marked row the footer flips to unmark
    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("ctrl-s unmark"), "unmark shortcut missing");
    assert!(
        text.contains("alt-s clear (1 marked)"),
        "clear shortcut count missing"
    );
    // toggling again clears the mark and the indicator disappears
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(app.marks.is_empty());
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(!buffer_text(&terminal).contains('▌'));
}

#[test]
fn marks_survive_moving_the_selection() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/one.md"), file_row("/b/two.md")]);
    // mark row 0 (selection auto-advances), move back: the mark stays
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(app.selected, 1, "marking advances the selection");
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
        !app.menu_entries()
            .iter()
            .any(|entry| entry.label == "open marked"),
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
        "move marked to…",
        "copy marked to…",
        "clear marks",
    ] {
        assert!(
            entries.iter().any(|entry| entry.label == label),
            "{label} missing"
        );
    }
    // batch copy content: visible marked paths in display order,
    // newline-joined — exactly what goes to the clipboard
    assert_eq!(app.visible_marked(), vec!["/a/x.pdf", "/c/z.pdf"]);
    assert_eq!(app.visible_marked().join("\n"), "/a/x.pdf\n/c/z.pdf");
}

#[test]
fn nvim_action_is_source_only_and_queues_the_selected_path() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/main.rs")]);
    let entry = app
        .menu_entries()
        .iter()
        .position(|entry| entry.label == "open in nvim")
        .expect("source files offer nvim");
    app.run_menu_action(entry);
    assert_eq!(app.nvim_request, Some(("/tmp/main.rs".into(), None)));

    app.engine
        .inject_results_for_test(vec![file_row("/tmp/manual.pdf")]);
    assert!(
        !app.menu_entries()
            .iter()
            .any(|entry| entry.label == "open in nvim")
    );
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/project/")]);
    assert!(
        !app.menu_entries()
            .iter()
            .any(|entry| entry.label == "open in nvim")
    );
}

#[test]
fn custom_actions_are_composed_above_builtins_only_for_matching_files() {
    let mut app = test_app();
    app.custom_actions = vec![
        custom_action("code action", &["true", "{path}"], &[], Some("code"), false),
        custom_action("pdf action", &["true"], &["pdf"], None, false),
    ];
    // No selected result means no custom entries, but built-ins remain.
    assert!(
        !app.menu_entries()
            .iter()
            .any(|entry| entry.label == "code action")
    );
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/main.rs")]);
    let entries = app.menu_entries();
    assert_eq!(entries[0].label, "code action");
    assert!(entries.iter().any(|entry| entry.label == "open"));
    assert!(!entries.iter().any(|entry| entry.label == "pdf action"));
    assert_eq!(entries[0].command, super::MenuCommand::Custom(0));
}

#[test]
fn custom_action_runs_once_for_marked_paths_placeholder() {
    let mut app = test_app();
    app.custom_actions = vec![custom_action(
        "batch action",
        &["true", "{paths}"],
        &[],
        None,
        false,
    )];
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/one.rs"), file_row("/tmp/two.txt")]);
    app.marks = ["/tmp/one.rs".into(), "/tmp/two.txt".into()].into();
    app.run_menu_action(0);
    assert_eq!(
        app.message.as_ref().map(|(message, _)| message.as_str()),
        Some("opened 2 in batch action")
    );
}

#[test]
fn custom_action_reports_marked_spawn_failures() {
    let mut app = test_app();
    app.custom_actions = vec![custom_action(
        "broken action",
        &["fsearch-command-that-does-not-exist", "{path}"],
        &[],
        None,
        false,
    )];
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/one.rs"), file_row("/tmp/two.txt")]);
    app.marks = ["/tmp/one.rs".into(), "/tmp/two.txt".into()].into();
    app.run_menu_action(0);
    assert!(app.message.as_ref().is_some_and(|(message, _)| {
        message.contains("error: opened 0/2 in broken action")
            && message.contains("2 failed")
            && message.contains("/tmp/one.rs")
    }));
}

#[test]
fn first_matching_enter_action_overrides_open() {
    let mut app = test_app();
    app.custom_actions = vec![
        custom_action("unmatched", &["true"], &[], Some("doc"), true),
        custom_action("first", &["true"], &[], Some("code"), true),
        custom_action("second", &["true"], &[], Some("code"), true),
    ];
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/main.rs")]);
    assert!(app.activate_selected());
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(message, _)| message == "opened in first: /tmp/main.rs")
    );
}

#[test]
fn destination_picker_preserves_query_selection_and_marks_on_cancel() {
    let mut app = test_app();
    app.engine.inject_results_for_test(vec![
        file_row("/a/notes.md"),
        file_row("/b/other.txt"),
        file_row("/tmp/destination/"),
    ]);
    app.editor.input = "notes".to_string();
    app.editor.input_cursor = app.editor.input.len();
    app.selected = 1;
    app.show_weak = true; // injected rows have no score-floor metadata
    app.marks.insert("/a/notes.md".to_string());

    let move_entry = app
        .menu_entries()
        .iter()
        .position(|entry| entry.label == "move marked to…")
        .unwrap();
    app.run_menu_action(move_entry);

    let picker = app.destination_picker.as_ref().unwrap();
    assert_eq!(picker.paths, vec!["/a/notes.md"]);
    assert_eq!(app.editor.input, "");
    assert_eq!(app.selected, 0);
    assert!(
        app.menu_entries()
            .iter()
            .all(|entry| !entry.label.ends_with("marked"))
    );
    assert!(
        app.menu_entries()
            .iter()
            .all(|entry| !entry.label.contains("marked to"))
    );

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains("move 1 file to…"));

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.destination_picker.is_none());
    assert!(app.show_weak);
    assert_eq!(app.editor.input, "notes");
    assert_eq!(app.selected, 1);
    assert!(app.marks.contains("/a/notes.md"));
}

#[test]
fn choosing_move_destination_removes_only_moved_marks() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    let collision_source = dir.path().join("existing.txt");
    let destination = dir.path().join("destination");
    std::fs::write(&source, "hello").unwrap();
    std::fs::write(&collision_source, "new").unwrap();
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("existing.txt"), "old").unwrap();
    let source = source.to_string_lossy().into_owned();
    let collision_source = collision_source.to_string_lossy().into_owned();
    let destination = format!("{}/", destination.to_string_lossy());

    let mut app = test_app();
    app.engine.inject_results_for_test(vec![
        file_row(&source),
        file_row(&collision_source),
        file_row(&destination),
    ]);
    app.marks = [source.clone(), collision_source.clone()].into();
    let move_entry = app
        .menu_entries()
        .iter()
        .position(|entry| entry.label == "move marked to…")
        .unwrap();
    app.run_menu_action(move_entry);
    // The real search worker will replace these rows asynchronously; inject
    // the selected directory so this test exercises Enter deterministically.
    app.engine
        .inject_results_for_test(vec![file_row(&destination)]);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.destination_picker.is_none());
    assert!(app.transfer_job.is_some());
    wait_for_transfer(&mut app);
    assert_eq!(app.marks, [collision_source.clone()].into());
    assert!(!std::path::Path::new(&source).exists());
    assert!(std::path::Path::new(&collision_source).exists());
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&destination).join("source.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&destination).join("existing.txt")).unwrap(),
        "old"
    );
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(message, _)| message == "moved 1, skipped 1 (exists)")
    );
}

#[test]
fn custom_actions_are_disabled_in_pick_filter_and_directory_modes() {
    let mut app = test_app();
    app.custom_actions = vec![custom_action("always", &["true"], &[], None, true)];
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/dir/")]);
    assert!(
        !app.menu_entries()
            .iter()
            .any(|entry| entry.label == "always")
    );
    app.ui_mode = UiMode::Pick;
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/file.txt")]);
    assert!(
        !app.menu_entries()
            .iter()
            .any(|entry| entry.label == "always")
    );
    let mut filter = test_filter_app();
    filter.custom_actions = vec![custom_action("always", &["true"], &[], None, false)];
    tick_until(&mut filter, |a| a.engine.results().len() >= 3);
    assert!(
        !filter
            .menu_entries()
            .iter()
            .any(|entry| entry.label == "always")
    );
}

#[test]
fn choosing_copy_destination_keeps_marks() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    let destination = dir.path().join("destination");
    std::fs::write(&source, "hello").unwrap();
    std::fs::create_dir(&destination).unwrap();
    let source = source.to_string_lossy().into_owned();
    let destination = format!("{}/", destination.to_string_lossy());

    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row(&source), file_row(&destination)]);
    app.marks.insert(source.clone());
    let copy_entry = app
        .menu_entries()
        .iter()
        .position(|entry| entry.label == "copy marked to…")
        .unwrap();
    app.run_menu_action(copy_entry);
    app.engine
        .inject_results_for_test(vec![file_row(&destination)]);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.destination_picker.is_none());
    assert!(app.transfer_job.is_some());
    wait_for_transfer(&mut app);
    assert_eq!(app.marks, [source.clone()].into());
    assert!(std::path::Path::new(&source).exists());
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&destination).join("source.txt")).unwrap(),
        "hello"
    );
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(message, _)| message == "copied 1 file")
    );
}

#[test]
fn destination_menu_entries_require_visible_marks() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/visible.txt")]);
    app.marks.insert("/hidden/file.txt".to_string());
    let entries = app.menu_entries();
    assert!(!entries.iter().any(|e| e.label == "move marked to…"));
    assert!(!entries.iter().any(|e| e.label == "copy marked to…"));
}

#[test]
fn clear_marks_action_and_menu_entry_clear_the_set() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/notes.md"), file_row("/b/other.txt")]);
    // two mark presses in a row cover both rows thanks to auto-advance
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(app.marks.len(), 2);
    // clear-marks clears everything with a toast
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT));
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

fn saved_app() -> App {
    let mut app = test_app();
    app.preview_layout = PreviewLayout::Hidden;
    app.configure_saved_searches(
        [
            ("Zebra".into(), "kind:doc changed:7d".into()),
            ("Café 東京".into(), "> naïve  spacing ext:rs".into()),
            ("Alpha".into(), "path:PROJECT ext:md".into()),
        ]
        .into(),
    );
    app
}

fn saved_type(app: &mut App, text: &str) {
    for c in text.chars() {
        assert!(app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
    }
}

#[test]
fn saved_cancel_preserves_query_editor_selection_regex_and_history() {
    let mut app = saved_app();
    app.editor.input = "prior.*query".into();
    app.editor.input_cursor = 5;
    app.editor.input_scroll = 2;
    app.regex_mode = true;
    app.refresh_query();
    app.selected = 2;
    app.selection_anchor = Some("/tmp/prior".into());
    app.show_weak = true;
    app.history.pos = Some(1);
    app.list_state = ListState::default().with_offset(3);
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)));
    saved_type(&mut app, "東京");
    assert!(app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
    assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(app.saved_picker.is_none());
    assert_eq!(app.editor.input, "prior.*query");
    assert_eq!(app.editor.input_cursor, 5);
    assert_eq!(app.editor.input_scroll, 2);
    assert_eq!(app.selected, 2);
    assert_eq!(app.selection_anchor.as_deref(), Some("/tmp/prior"));
    assert!(app.show_weak);
    assert!(app.regex_mode);
    assert_eq!(app.engine.mode(), crate::engine::Mode::Regex);
    assert_eq!(app.history.pos, Some(1));
    assert_eq!(app.list_state.offset(), 3);
    app.open_saved_searches();
    assert!(app.saved_picker.as_ref().unwrap().editor.input.is_empty());
}

#[test]
fn saved_apply_replaces_full_query_through_normal_refresh_in_pick_mode() {
    let mut app = saved_app();
    app.ui_mode = UiMode::Pick;
    app.editor.input = "old.*".into();
    app.editor.input_cursor = app.editor.input.len();
    app.regex_mode = true;
    app.refresh_query();
    app.selected = 4;
    app.selection_anchor = Some("old".into());
    app.history.pos = Some(0);
    app.show_weak = true;
    app.open_saved_searches();
    saved_type(&mut app, "CAFÉ");
    assert_eq!(app.saved_picker.as_ref().unwrap().matches, [1]);
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    assert!(app.saved_picker.is_none());
    assert_eq!(app.editor.input, "> naïve  spacing ext:rs");
    assert_eq!(app.editor.input_cursor, app.editor.input.len());
    assert_eq!(app.editor.input_scroll, 0);
    assert!(!app.regex_mode);
    assert_eq!(app.engine.mode(), crate::engine::Mode::Content);
    assert_eq!(app.selected, 0);
    assert!(app.selection_anchor.is_none());
    assert!(app.history.pos.is_none());
    assert!(!app.show_weak);
    assert!(
        app.picked.is_none(),
        "applying a query must not exit --pick"
    );
    app.open_saved_searches();
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    assert_eq!(app.editor.input, "path:PROJECT ext:md");
    assert_eq!(app.engine.mode(), crate::engine::Mode::Fuzzy);
}

#[test]
fn saved_filter_matches_names_queries_and_edits_unicode_safely() {
    let mut app = saved_app();
    app.open_saved_searches();
    assert_eq!(app.saved_searches[0].0, "Alpha", "config order is stable");
    saved_type(&mut app, "project");
    assert_eq!(app.saved_picker.as_ref().unwrap().matches, [0]);
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    saved_type(&mut app, "東x京");
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    let picker = app.saved_picker.as_ref().unwrap();
    assert_eq!(picker.editor.input, "東京");
    assert_eq!(picker.editor.input_cursor, "東".len());
    assert_eq!(picker.matches, [1]);
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(app.saved_picker.as_ref().unwrap().editor.input, "京");
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    saved_type(&mut app, "NAÏVE");
    assert_eq!(app.saved_picker.as_ref().unwrap().matches, [1]);
    assert!(
        app.editor.input.is_empty(),
        "filter never changes live query"
    );
}

#[test]
fn saved_remapped_open_navigation_apply_and_help() {
    let mut app = saved_app();
    app.keymap = crate::keymap::Keymap::from_config(
        &[
            ("saved_searches".into(), vec!["f4".into()]),
            ("move_down".into(), vec!["f5".into()]),
            ("move_up".into(), vec!["f6".into()]),
            ("open".into(), vec!["f7".into()]),
        ]
        .into(),
    );
    app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert!(app.saved_picker.is_none());
    app.help.open = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 60)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("saved searches"));
    assert!(text.contains("f4"));
    app.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
    assert!(!app.help.open);
    app.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
    assert_eq!(app.saved_picker.as_ref().unwrap().selected, 1);
    app.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
    assert_eq!(app.saved_picker.as_ref().unwrap().selected, 0);
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
    assert_eq!(app.editor.input, "> naïve  spacing ext:rs");
    app.menu = Some(0);
    app.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
    assert!(app.menu.is_none());
    assert!(app.saved_picker.is_some());
}

#[test]
fn saved_empty_config_and_no_matches_do_not_apply() {
    let mut app = saved_app();
    app.configure_saved_searches(Default::default());
    app.editor.input = "keep me".into();
    app.editor.input_cursor = app.editor.input.len();
    app.open_saved_searches();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let text = buffer_text(&terminal);
    assert!(text.contains("No saved searches"));
    assert!(text.contains("[searches]"));
    assert!(text.contains("config.toml"));
    for code in [KeyCode::Up, KeyCode::Down, KeyCode::Enter] {
        assert!(app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)));
    }
    assert!(app.saved_picker.is_some());
    assert_eq!(app.editor.input, "keep me");
    let mut app = saved_app();
    app.open_saved_searches();
    saved_type(&mut app, "unmatched");
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains("No matching saved searches"));
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    assert!(app.saved_picker.is_some());
    assert!(app.editor.input.is_empty());
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(app.saved_picker.as_ref().unwrap().matches.len(), 3);
}

#[test]
fn saved_scroll_small_terminals_and_unicode_cursor_are_safe() {
    let mut app = saved_app();
    app.configure_saved_searches(
        (0..100)
            .map(|i| (format!("saved-{i:03}"), format!("query-{i:03}")))
            .collect(),
    );
    app.open_saved_searches();
    for _ in 0..75 {
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    }
    let mut terminal = Terminal::new(TestBackend::new(45, 9)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(buffer_text(&terminal).contains("saved-075 — query-075"));
    assert!(app.saved_picker.as_ref().unwrap().list_state.offset() > 0);
    for border in [BorderKind::Rounded, BorderKind::None] {
        app.theme.borders = border;
        for (width, height) in [(1, 1), (2, 2), (3, 3), (8, 4), (20, 6)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
            saved_type(&mut app, "東京é界".repeat(20).as_str());
            terminal.draw(|f| draw(f, &mut app)).unwrap();
        }
    }
}

#[test]
fn saved_mouse_is_modal_and_never_activates_underlying_pick() {
    let mut app = saved_app();
    app.ui_mode = UiMode::Pick;
    app.engine
        .inject_results_for_test(vec![file_row("/tmp/first"), file_row("/tmp/second")]);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let area = app.hit_test.results_area;
    app.hit_test.last_click = Some((0, Instant::now()));
    app.open_saved_searches();
    assert!(app.hit_test.last_click.is_none());
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::ScrollDown,
    ] {
        assert!(app.handle_mouse(MouseEvent {
            kind,
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE
        }));
    }
    assert_eq!(app.selected, 0);
    assert!(app.picked.is_none());
    assert!(app.saved_picker.is_some());
    assert_eq!(app.saved_picker.as_ref().unwrap().selected, 1);
}

#[test]
fn saved_disabled_in_filter_destination_and_transfer_but_not_pick() {
    let mut filter = test_filter_app();
    filter.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert!(filter.saved_picker.is_none());
    assert!(filter.editor.input.is_empty());
    let mut app = saved_app();
    app.editor.input = "target".into();
    app.editor.input_cursor = app.editor.input.len();
    app.destination_picker = Some(super::DestinationPicker {
        kind: crate::actions::TransferKind::Copy,
        paths: vec!["/tmp/file".into()],
        previous_query: "previous".into(),
        previous_selected: 0,
        previous_show_weak: false,
    });
    app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert!(app.saved_picker.is_none());
    assert!(app.destination_picker.is_some());
    assert_eq!(app.editor.input, "target");
    app.destination_picker = None;
    let (_tx, rx) = std::sync::mpsc::channel();
    app.transfer_job = Some(super::TransferJob {
        kind: crate::actions::TransferKind::Copy,
        total: 1,
        done: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        rx,
        worker: None,
    });
    app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert!(app.saved_picker.is_none());
    assert_eq!(app.editor.input, "target");
    app.transfer_job = None;
    app.ui_mode = UiMode::Pick;
    app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert!(app.saved_picker.is_some());
}

#[test]
fn hidden_selected_rows_are_not_actionable() {
    let mut app = test_app();
    app.engine
        .inject_results_for_test(vec![file_row("/a/visible.txt")]);
    app.selected = 1;
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
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
    assert!(entries.iter().any(|entry| entry.label == "clear marks"));
    assert!(!entries.iter().any(|entry| entry.label == "open marked"));
    assert!(!entries.iter().any(|entry| entry.label == "trash marked"));
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
    // the mark key does nothing in filter mode
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
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
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(app.marks.is_empty(), "no marks in pick mode");
}
