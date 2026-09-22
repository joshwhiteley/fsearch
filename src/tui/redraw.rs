//! Keep polling workers, but only format/render frames after visible changes.
use super::*;

pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(50);
const TOAST_LIFETIME: Duration = Duration::from_millis(2500);

pub(super) struct Redraw {
    dirty: bool,
    transfer: Option<(usize, bool)>,
    ages: Vec<String>,
}

impl Redraw {
    pub(super) fn new() -> Self {
        Self {
            dirty: true, // Always paint the first frame, including an empty index.
            transfer: None,
            ages: Vec::new(),
        }
    }

    pub(super) fn mark(&mut self, changed: bool) {
        self.dirty |= changed;
    }

    pub(super) fn take_dirty(&mut self, app: &mut App, now: Instant) -> bool {
        self.mark(app.expire_message(now));
        // These values can change without channel messages or input events.
        let transfer = app.transfer_job.as_ref().map(|job| {
            (
                job.done.load(Ordering::Relaxed),
                job.cancel.load(Ordering::Relaxed),
            )
        });
        let ages = visible_ages(app, std::time::SystemTime::now());
        self.mark(self.transfer != transfer || self.ages != ages);
        self.transfer = transfer;
        self.ages = ages;
        std::mem::take(&mut self.dirty)
    }
}

// Only inspect whole rows in the last painted viewport, never the whole
// result set. Their relative timestamps and the selected preview/status age
// can cross a minute/hour/day boundary without any worker notification.
fn visible_ages(app: &App, now: std::time::SystemTime) -> Vec<String> {
    let mut ages = Vec::new();
    if let Some((_, _, Some(modified))) = app.status.meta {
        ages.push(chrome::human_age_at(modified, now));
    }
    if !app.engine.is_filter() && app.engine.mode() != Mode::Calc {
        let row_age = |row: &ResultRow| {
            row.meta.filter(|meta| meta.mtime > 0).map(|meta| {
                chrome::human_age_at(
                    std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(meta.mtime as u64),
                    now,
                )
            })
        };
        if app.preview_layout != PreviewLayout::Hidden
            && let Some(age) = app.visible_selected_row().and_then(row_age)
        {
            ages.push(age);
        }
        let mut used = 0;
        for &(slot, height) in app.hit_test.slots.iter().skip(app.list_state.offset()) {
            used += u32::from(height);
            if used > u32::from(app.hit_test.results_area.height) {
                break;
            }
            if let Slot::Row(index) = slot
                && let Some(age) = app.engine.results().get(index).and_then(row_age)
            {
                ages.push(age);
            }
        }
    }
    ages
}

// Mouse movement/release/drag is inert in handle_mouse. Do not turn terminal
// pointer motion into a new unconditional frame stream.
pub(super) fn mouse_changes_ui(kind: MouseEventKind) -> bool {
    matches!(
        kind,
        MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollUp
    )
}

impl App {
    pub(super) fn expire_message(&mut self, now: Instant) -> bool {
        if self
            .message
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= TOAST_LIFETIME)
        {
            self.message = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn app() -> App {
        // No index, watcher, saved history, real paths, or filesystem loads.
        App::new(Engine::from_lines(Vec::new()))
    }

    #[test]
    fn idle_minute_renders_once_instead_of_1200_times() {
        for count in [0, 2000] {
            let mut app = app();
            app.preview_layout = PreviewLayout::Hidden;
            app.show_weak = true;
            app.engine.inject_results_for_test(
                (0..count)
                    .map(|i| ResultRow {
                        path: format!("synthetic-{i}"),
                        line_number: None,
                        line: None,
                        recent_open: false,
                        meta: None,
                        score: None,
                    })
                    .collect(),
            );
            let mut redraw = Redraw::new();
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            let start = Instant::now();
            let mut frames = 0;
            for tick in 0..1200 {
                redraw.mark(false);
                if redraw.take_dirty(&mut app, start + POLL_INTERVAL * tick) {
                    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                    frames += 1;
                }
            }
            assert_eq!(frames, 1, "unchanged 50ms polls must not render");
            eprintln!(
                "synthetic idle minute ({count} rows): {frames} frame vs 1200 unconditional frames"
            );
        }
    }

    #[test]
    fn toast_expiry_invalidates_once_without_an_intervening_draw() {
        let mut app = app();
        let mut redraw = Redraw::new();
        let start = Instant::now();
        app.message = Some(("synthetic toast".into(), start));
        assert!(redraw.take_dirty(&mut app, start));
        assert!(!redraw.take_dirty(&mut app, start + TOAST_LIFETIME - Duration::from_nanos(1)));
        assert!(app.message.is_some());
        assert!(redraw.take_dirty(&mut app, start + TOAST_LIFETIME));
        assert!(app.message.is_none());
        assert!(!redraw.take_dirty(&mut app, start + TOAST_LIFETIME + POLL_INTERVAL));
    }

    #[test]
    fn age_tracking_is_viewport_bounded_and_uses_displayed_time_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::Config {
            roots: Vec::new(),
            index_apps: false,
            ..Default::default()
        };
        let mut app = App::new(Engine::new(
            config,
            dir.path().join("index"),
            dir.path().join("history"),
        ));
        app.preview_layout = PreviewLayout::Hidden;
        app.show_weak = true;
        app.engine.inject_results_for_test(
            (0..1000)
                .map(|i| ResultRow {
                    path: format!("synthetic-{i}"),
                    line_number: None,
                    line: None,
                    recent_open: false,
                    score: None,
                    meta: Some(crate::walker::FileMeta {
                        mtime: if i == 10 { 1000 } else { 1 },
                        ..Default::default()
                    }),
                })
                .collect(),
        );
        app.hit_test.slots = (0..1000).map(|i| (Slot::Row(i), 2)).collect();
        app.list_state = ListState::default().with_offset(10);
        // One whole two-line row; the spare cell must not track the next row.
        app.hit_test.results_area = Rect::new(0, 0, 80, 3);
        let epoch = std::time::SystemTime::UNIX_EPOCH;
        assert_eq!(
            visible_ages(&app, epoch + Duration::from_secs(1059)),
            ["just now"]
        );
        assert_eq!(
            visible_ages(&app, epoch + Duration::from_secs(1060)),
            ["1m ago"]
        );
        assert_eq!(
            visible_ages(&app, epoch + Duration::from_secs(1119)),
            ["1m ago"]
        );
        // Status/preview age is tracked even when no results pane is visible.
        app.hit_test.results_area = Rect::default();
        app.status.meta = Some((true, 1, Some(epoch + Duration::from_secs(1000))));
        assert_eq!(
            visible_ages(&app, epoch + Duration::from_secs(1060)),
            ["1m ago"]
        );
    }

    #[test]
    fn transfer_progress_and_cancellation_invalidate_before_completion() {
        let mut app = app();
        let mut redraw = Redraw::new();
        let now = Instant::now();
        let (tx, rx) = mpsc::channel();
        let done = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        app.transfer_job = Some(TransferJob {
            kind: actions::TransferKind::Copy,
            total: 3,
            done: done.clone(),
            cancel: cancel.clone(),
            rx,
            worker: None,
        });
        assert!(redraw.take_dirty(&mut app, now));
        assert!(!app.poll_transfer());
        assert!(!redraw.take_dirty(&mut app, now));
        done.store(1, Ordering::Relaxed);
        assert!(redraw.take_dirty(&mut app, now));
        assert!(!redraw.take_dirty(&mut app, now));
        cancel.store(true, Ordering::Relaxed);
        assert!(redraw.take_dirty(&mut app, now));
        assert!(!redraw.take_dirty(&mut app, now));
        tx.send(actions::TransferOutcome::default()).unwrap();
        redraw.mark(app.poll_transfer());
        assert!(redraw.take_dirty(&mut app, now));
        assert!(app.message.is_some());
        assert!(!app.poll_transfer());
        assert!(!redraw.take_dirty(&mut app, now));
    }

    #[test]
    fn accepted_text_and_image_previews_dirty_but_stale_or_hidden_replies_do_not() {
        let mut app = app();
        let (tx, rx) = mpsc::channel();
        app.preview.rx = rx;
        app.preview.generation = 4;
        app.preview.for_key = Some(("synthetic.png".into(), None));
        app.picker = Some(Picker::halfblocks());
        for (generation, path, visible, expected) in [
            (3, "synthetic.png", true, false),
            (4, "other.png", true, false),
            (4, "synthetic.png", false, false),
            (4, "synthetic.png", true, true),
        ] {
            app.preview_layout = if visible {
                PreviewLayout::Side
            } else {
                PreviewLayout::Hidden
            };
            tx.send(PreviewResult {
                generation,
                path: path.into(),
                line_number: None,
                payload: PreviewPayload::Lines(vec![Line::from("synthetic text")]),
            })
            .unwrap();
            assert_eq!(app.poll_preview(), expected);
        }
        tx.send(PreviewResult {
            generation: 4,
            path: "synthetic.png".into(),
            line_number: None,
            payload: PreviewPayload::Image(image::DynamicImage::new_rgb8(4, 4)),
        })
        .unwrap();
        let mut redraw = Redraw::new();
        let now = Instant::now();
        assert!(redraw.take_dirty(&mut app, now));
        redraw.mark(app.poll_preview());
        assert!(redraw.take_dirty(&mut app, now));
        assert_eq!(app.preview.image_dims, Some((4, 4)));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(!redraw.take_dirty(&mut app, now));
        // A terminal resize must repaint even if the image/query did not change.
        terminal.backend_mut().resize(100, 30);
        redraw.mark(true);
        assert!(redraw.take_dirty(&mut app, now));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(!app.poll_preview());
        assert!(!redraw.take_dirty(&mut app, now));
    }

    #[test]
    fn explicit_invalidations_coalesce_and_pointer_motion_stays_idle() {
        let mut app = app();
        let mut redraw = Redraw::new();
        let now = Instant::now();
        assert!(redraw.take_dirty(&mut app, now));
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            redraw.mark(mouse_changes_ui(kind));
            assert!(!redraw.take_dirty(&mut app, now));
        }
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
        ] {
            redraw.mark(mouse_changes_ui(kind));
            redraw.mark(false);
            assert!(redraw.take_dirty(&mut app, now));
            assert!(!redraw.take_dirty(&mut app, now));
        }
        // Keyboard, resize, focus and foreground-editor return all mark dirty.
        redraw.mark(true);
        redraw.mark(true);
        assert!(redraw.take_dirty(&mut app, now));
        assert!(!redraw.take_dirty(&mut app, now));
    }
}
