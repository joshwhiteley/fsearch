use crate::actions;
use crate::engine::{Engine, Mode, ResultRow};
use crate::highlight::{self, Appearance};
use crate::matcher::Highlighter;
use crate::theme::Theme;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::ListState;
use ratatui::{Terminal, backend::CrosstermBackend, crossterm::execute};
use ratatui_image::picker::Picker;
use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

const PREVIEW_BYTES: usize = 64 * 1024;
const PREVIEW_SCROLL_PAGE: usize = 20;
/// Rows the help overlay jumps per pgup/pgdn while it is open.
const HELP_SCROLL_PAGE: u16 = 8;

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

    /// Stable key for the session state file; [`Self::from_key`] inverts it.
    fn key(self) -> &'static str {
        match self {
            PreviewLayout::Side => "side",
            PreviewLayout::Full => "full",
            PreviewLayout::Hidden => "hidden",
        }
    }

    fn from_key(s: &str) -> Option<PreviewLayout> {
        match s {
            "side" => Some(PreviewLayout::Side),
            "full" => Some(PreviewLayout::Full),
            "hidden" => Some(PreviewLayout::Hidden),
            _ => None,
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

    /// Stable key for the session state file; [`Self::from_key`] inverts it.
    fn key(self) -> &'static str {
        match self {
            Density::Comfy => "comfy",
            Density::Compact => "compact",
        }
    }

    fn from_key(s: &str) -> Option<Density> {
        match s {
            "comfy" => Some(Density::Comfy),
            "compact" => Some(Density::Compact),
            _ => None,
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

/// State saved while the results list is temporarily used to choose a
/// destination directory.
pub struct DestinationPicker {
    pub kind: actions::TransferKind,
    pub paths: Vec<String>,
    pub previous_query: String,
    pub previous_selected: usize,
    pub previous_show_weak: bool,
}

/// At most one transfer runs. Progress is per completed file; Esc requests
/// cancellation before the next file. Shutdown waits for the current file so
/// temporary files are cleaned and moves are not interrupted between steps.
pub struct TransferJob {
    kind: actions::TransferKind,
    total: usize,
    done: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<actions::TransferOutcome>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for TransferJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
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

/// Query text editing state: the input string, its byte-offset cursor, and
/// the horizontal scroll that keeps the cursor visible.
pub struct Editor {
    pub input: String,
    /// Byte offset of the edit cursor, always on a char boundary.
    pub input_cursor: usize,
    /// Char offset of the first visible input character; shifts once the
    /// edit cursor would leave the query row.
    pub input_scroll: usize,
}

impl Editor {
    fn clear(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
        self.input_scroll = 0;
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
}

/// Query history: loaded once at startup, cycled with ctrl-p/ctrl-n, and
/// appended to the history file on each activation.
pub struct History {
    pub entries: Vec<String>,
    /// Position while stepping through `entries`; None sits "past the
    /// newest", i.e. on a blank query.
    pos: Option<usize>,
    file: Option<std::path::PathBuf>,
    enabled: bool,
}

/// The preview pipeline: what is shown, for which row, the worker channel,
/// and per-content display state (scroll, image dimensions).
pub struct Preview {
    /// The row the current content was loaded for: (path, line number).
    pub for_key: Option<(String, Option<u64>)>,
    pub content: PreviewContent,
    /// Preview loading runs on a worker thread; these move requests/results.
    pub tx: mpsc::Sender<PreviewRequest>,
    pub rx: mpsc::Receiver<PreviewResult>,
    pub generation: u64,
    latest_generation: Arc<AtomicU64>,
    /// First line shown in the text preview; reset on selection change.
    pub scroll: usize,
    /// Pixel dimensions of the image preview, from the decode on the worker.
    pub image_dims: Option<(u32, u32)>,
}

/// Geometry recorded by the draw pass so mouse events can hit-test rows,
/// panes, and the fold/header decorations.
pub struct HitTest {
    /// Display-order hit-test map for the results list: (slot, row height).
    pub slots: Vec<(Slot, u16)>,
    /// Inner rect of the results block, or Rect::default() when hidden.
    pub results_area: Rect,
    /// Inner rect of the preview block, or Rect::default() when hidden.
    pub preview_area: Rect,
    /// Last single click on a result row: (row index, when) for double-click.
    last_click: Option<(usize, Instant)>,
}

/// Cached match highlighters for the results list, rebuilt only when the
/// input changes (same take/rebuild pattern for both caches).
pub struct Highlights {
    pub(crate) input: String,
    pub(crate) fuzzy: Option<Highlighter>,
    pub(crate) content_input: String,
    pub(crate) content: Option<regex::Regex>,
}

/// Cached stat of the status line's selected path (is_file, size, mtime),
/// refreshed outside the draw pass.
pub struct StatusCache {
    pub path: String,
    pub meta: Option<(bool, u64, Option<SystemTime>)>,
}

/// Modal help overlay: the open flag plus scroll offset; `area` records
/// where it was last drawn so mouse clicks can hit-test inside vs outside.
pub struct HelpModal {
    pub open: bool,
    /// First visible content row; clamped to the content at draw time.
    pub scroll: u16,
    /// Screen rect of the rendered overlay, Rect::default() while closed.
    pub area: Rect,
}

struct BatchOutcome {
    succeeded: usize,
    first_error: Option<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltInAction {
    Open,
    Nvim,
    Reveal,
    Copy,
    QuickLook,
    Trash,
    OpenMarked,
    CopyMarked,
    TrashMarked,
    MoveMarkedTo,
    CopyMarkedTo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MenuCommand {
    BuiltIn(BuiltInAction),
    Custom(usize),
    ClearMarks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuEntry {
    label: String,
    command: MenuCommand,
}

fn run_batch<F>(paths: &[String], mut action: F) -> BatchOutcome
where
    F: FnMut(&str) -> std::io::Result<()>,
{
    let mut outcome = BatchOutcome {
        succeeded: 0,
        first_error: None,
    };
    for path in paths {
        match action(path) {
            Ok(()) => outcome.succeeded += 1,
            Err(error) => {
                if outcome.first_error.is_none() {
                    outcome.first_error = Some((path.clone(), error.to_string()));
                }
            }
        }
    }
    outcome
}

fn batch_summary(verb: &str, total: usize, outcome: &BatchOutcome) -> String {
    if total == 0 {
        return "no visible marked files".to_string();
    }
    let failed = total - outcome.succeeded;
    match &outcome.first_error {
        Some((path, error)) => {
            format!(
                "error: {verb} {}/{} files; {failed} failed ({path}: {error})",
                outcome.succeeded, total
            )
        }
        None => format!("{verb} {} files", outcome.succeeded),
    }
}

pub struct App {
    pub engine: Engine,
    pub selected: usize,
    /// Path pinned by an explicit user movement; asynchronous result
    /// re-ranking restores this path instead of silently changing selection.
    selection_anchor: Option<String>,
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
    /// The raw `[theme]` config the current theme was resolved from; kept
    /// so ctrl-g can re-resolve each preset with accent/overrides intact.
    pub theme_cfg: crate::config::ThemeConfig,
    /// Nerd-font glyphs before filenames (from `icons = true`).
    pub icons: bool,
    pub ui_mode: UiMode,
    pub picked: Option<String>,
    /// Open actions popup: Some(selected entry index).
    pub menu: Option<usize>,
    /// Command keybindings (configurable via `[keys]` in config.toml).
    pub keymap: crate::keymap::Keymap,
    /// Results list scroll state (selection + visual offset); persisted so
    /// its offset reflects real scroll for mouse hit testing.
    pub list_state: ListState,
    /// Screen rect of the open actions popup, for mouse hit testing;
    /// Rect::default() while closed.
    pub menu_area: Rect,
    /// Actual list content rect and scroll offset for the open actions popup.
    pub menu_inner: Rect,
    pub menu_offset: usize,
    pub editor: Editor,
    pub history: History,
    pub preview: Preview,
    pub hit_test: HitTest,
    pub highlights: Highlights,
    pub status: StatusCache,
    pub help: HelpModal,
    /// Multi-select marks, tracked as PATHS so they survive reordering of
    /// results. The set persists until cleared or quit; batch actions and
    /// the row indicators operate only on currently-visible marked rows.
    /// Never populated in filter mode (rows are arbitrary stdin lines).
    pub marks: HashSet<String>,
    /// User-defined actions loaded from `[[actions]]`.
    pub custom_actions: Vec<crate::config::CustomAction>,
    /// Selected source file waiting to open in foreground Neovim.
    pub nvim_request: Option<(String, Option<u64>)>,
    /// Temporarily replaces the normal query with a directory-only search.
    pub destination_picker: Option<DestinationPicker>,
    pub transfer_job: Option<TransferJob>,
}

impl App {
    pub fn new(engine: Engine) -> App {
        let (preview_tx, preview_rx) = mpsc::channel::<PreviewRequest>();
        let (result_tx, result_rx) = mpsc::channel::<PreviewResult>();
        let latest_generation = Arc::new(AtomicU64::new(0));
        let worker_generation = latest_generation.clone();
        // one preview worker for the app's lifetime; exits when the app
        // (and thus preview_tx) is dropped. Per-request work is panic-guarded:
        // image decoding, SVG rendering, and syntect highlighting all run in
        // here, and a dead worker would leave previews stuck on "loading..."
        std::thread::spawn(move || {
            while let Ok(req) = latest_preview_request(&preview_rx) {
                if req.generation != worker_generation.load(Ordering::Relaxed) {
                    continue;
                }
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
            selected: 0,
            selection_anchor: None,
            regex_mode: false,
            preview_layout: PreviewLayout::Side,
            density: Density::Comfy,
            show_weak: false,
            message: None,
            appearance: Appearance::Dark,
            picker: None,
            theme: crate::theme::resolve("default", None),
            theme_cfg: crate::config::ThemeConfig::default(),
            icons: false,
            ui_mode: UiMode::Open,
            picked: None,
            menu: None,
            keymap: crate::keymap::Keymap::default(),
            list_state: ListState::default(),
            menu_area: Rect::default(),
            menu_inner: Rect::default(),
            menu_offset: 0,
            editor: Editor {
                input: String::new(),
                input_cursor: 0,
                input_scroll: 0,
            },
            history: History {
                entries: Vec::new(),
                pos: None,
                file: None,
                enabled: true,
            },
            preview: Preview {
                for_key: None,
                content: PreviewContent::Lines(Vec::new()),
                tx: preview_tx,
                rx: result_rx,
                generation: 0,
                latest_generation,
                scroll: 0,
                image_dims: None,
            },
            hit_test: HitTest {
                slots: Vec::new(),
                results_area: Rect::default(),
                preview_area: Rect::default(),
                last_click: None,
            },
            highlights: Highlights {
                input: String::new(),
                fuzzy: None,
                content_input: String::new(),
                content: None,
            },
            status: StatusCache {
                path: String::new(),
                meta: None,
            },
            help: HelpModal {
                open: false,
                scroll: 0,
                area: Rect::default(),
            },
            marks: HashSet::new(),
            custom_actions: Vec::new(),
            nvim_request: None,
            destination_picker: None,
            transfer_job: None,
        }
    }

    const MENU: [(BuiltInAction, &'static str); 5] = [
        (BuiltInAction::Open, "open"),
        (BuiltInAction::Reveal, "reveal in finder"),
        (BuiltInAction::Copy, "copy path"),
        (BuiltInAction::QuickLook, "quick look"),
        (BuiltInAction::Trash, "move to trash"),
    ];

    /// Menu entries: the single-selection actions plus batch actions for
    /// visible marks. Clear remains available for marks hidden by a filter or
    /// the weaker-match fold.
    fn menu_entries(&self) -> Vec<MenuEntry> {
        let mut entries = Vec::new();
        if self.custom_actions_enabled()
            && let Some(path) = self.visible_selected_row().map(|row| row.path.as_str())
        {
            entries.extend(
                self.custom_actions
                    .iter()
                    .enumerate()
                    .filter(|(_, action)| action.matches(path))
                    .map(|(index, action)| MenuEntry {
                        label: action.name.clone(),
                        command: MenuCommand::Custom(index),
                    }),
            );
        }
        if self
            .visible_selected_row()
            .is_some_and(|row| is_source_file(&row.path))
            && self.custom_actions_enabled()
        {
            entries.push(MenuEntry {
                label: "open in nvim".into(),
                command: MenuCommand::BuiltIn(BuiltInAction::Nvim),
            });
        }
        entries.extend(Self::MENU.into_iter().map(|(command, label)| MenuEntry {
            label: label.to_string(),
            command: MenuCommand::BuiltIn(command),
        }));
        if self.marking_enabled() && self.visible_marked_count() > 0 {
            entries.extend([
                MenuEntry {
                    label: "open marked".into(),
                    command: MenuCommand::BuiltIn(BuiltInAction::OpenMarked),
                },
                MenuEntry {
                    label: "copy marked paths".into(),
                    command: MenuCommand::BuiltIn(BuiltInAction::CopyMarked),
                },
                MenuEntry {
                    label: "trash marked".into(),
                    command: MenuCommand::BuiltIn(BuiltInAction::TrashMarked),
                },
                MenuEntry {
                    label: "move marked to…".into(),
                    command: MenuCommand::BuiltIn(BuiltInAction::MoveMarkedTo),
                },
                MenuEntry {
                    label: "copy marked to…".into(),
                    command: MenuCommand::BuiltIn(BuiltInAction::CopyMarkedTo),
                },
            ]);
        }
        if self.marking_enabled() && !self.marks.is_empty() {
            entries.push(MenuEntry {
                label: "clear marks".into(),
                command: MenuCommand::ClearMarks,
            });
        }
        entries
    }

    fn custom_actions_enabled(&self) -> bool {
        self.ui_mode == UiMode::Open && !self.engine.is_filter() && self.engine.mode() != Mode::Calc
    }

    /// The focused row only counts when it belongs to the currently visible
    /// result set. A folded weak row must not be actionable.
    fn visible_selected_row(&self) -> Option<&ResultRow> {
        (self.selected < self.visible_len())
            .then(|| self.engine.results().get(self.selected))
            .flatten()
    }

    /// Paths of currently-visible marked rows in display order, deduplicated
    /// because content search can return several hits for one path.
    fn visible_marked(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.engine
            .results()
            .iter()
            .take(self.visible_len())
            .filter(|row| self.marks.contains(&row.path) && seen.insert(&row.path))
            .map(|row| row.path.clone())
            .collect()
    }

    /// Marking is an Open-mode file feature: `--pick`, filter stdin, and the
    /// calculator have no persistent file rows to mark.
    fn marking_enabled(&self) -> bool {
        self.destination_picker.is_none()
            && self.ui_mode == UiMode::Open
            && !self.engine.is_filter()
            && self.engine.mode() != Mode::Calc
    }

    fn visible_marked_count(&self) -> usize {
        self.visible_marked().len()
    }

    fn run_menu_action(&mut self, entry: usize) {
        let Some(action) = self.menu_entries().get(entry).cloned() else {
            self.menu = None;
            return;
        };
        self.menu = None;
        match action.command {
            MenuCommand::BuiltIn(BuiltInAction::Open) => self.open_selected(),
            MenuCommand::BuiltIn(BuiltInAction::Nvim) => {
                self.nvim_request = self
                    .visible_selected_row()
                    .map(|row| (row.path.clone(), row.line_number));
            }
            MenuCommand::BuiltIn(BuiltInAction::Reveal) => self.act(actions::reveal, "revealed"),
            MenuCommand::BuiltIn(BuiltInAction::Copy) => self.act(actions::copy, "copied"),
            MenuCommand::BuiltIn(BuiltInAction::QuickLook) => {
                self.act(actions::quick_look, "quick look")
            }
            MenuCommand::BuiltIn(BuiltInAction::Trash) => self.act(actions::trash, "trashed"),
            MenuCommand::BuiltIn(BuiltInAction::OpenMarked) => self.open_marked(),
            MenuCommand::BuiltIn(BuiltInAction::CopyMarked) => self.copy_marked(),
            MenuCommand::BuiltIn(BuiltInAction::TrashMarked) => self.trash_marked(),
            MenuCommand::BuiltIn(BuiltInAction::MoveMarkedTo) => {
                self.enter_destination_picker(actions::TransferKind::Move)
            }
            MenuCommand::BuiltIn(BuiltInAction::CopyMarkedTo) => {
                self.enter_destination_picker(actions::TransferKind::Copy)
            }
            MenuCommand::Custom(index) => self.run_custom_action(index),
            MenuCommand::ClearMarks => self.clear_marks(),
        }
    }

    fn set_message(&mut self, message: String) {
        self.message = Some((message, Instant::now()));
    }

    /// Reuses the single-file open path, including frecency recording.
    fn open_path(&mut self, path: &str) -> std::io::Result<()> {
        actions::open(path)?;
        self.engine.record_open(path);
        Ok(())
    }

    fn matching_enter_action(&self, path: &str) -> Option<crate::config::CustomAction> {
        self.custom_actions
            .iter()
            .find(|action| action.enter && action.matches(path))
            .cloned()
    }

    fn matched_line(&self, path: &str) -> Option<u64> {
        self.visible_selected_row()
            .filter(|row| row.path == path)
            .or_else(|| {
                self.engine
                    .results()
                    .iter()
                    .take(self.visible_len())
                    .find(|row| row.path == path)
            })
            .and_then(|row| row.line_number)
    }

    fn open_path_with_enter(&mut self, path: &str) -> std::io::Result<()> {
        if let Some(action) = self.matching_enter_action(path) {
            actions::run_custom_with_line(
                &action,
                path,
                &[path.to_string()],
                self.matched_line(path),
            )?;
            self.engine.record_open(path);
            Ok(())
        } else {
            self.open_path(path)
        }
    }

    /// Batch-open every visible marked row, continuing after failures and
    /// reporting a partial result instead of silently dropping errors.
    fn open_marked(&mut self) {
        let paths = self.visible_marked();
        let outcome = run_batch(&paths, |path| self.open_path_with_enter(path));
        self.set_message(batch_summary("opened", paths.len(), &outcome));
    }

    fn custom_marked_paths(&self, action: &crate::config::CustomAction) -> Vec<String> {
        self.visible_marked()
            .into_iter()
            .filter(|path| action.matches(path))
            .collect()
    }

    fn custom_batch_summary(
        &self,
        action: &crate::config::CustomAction,
        paths: &[String],
        outcome: &BatchOutcome,
    ) -> String {
        if paths.is_empty() {
            return format!("no visible marked files matching {}", action.name);
        }
        let failed = paths.len() - outcome.succeeded;
        match &outcome.first_error {
            Some((path, error)) => format!(
                "error: opened {}/{} in {}; {failed} failed ({path}: {error})",
                outcome.succeeded,
                paths.len(),
                action.name
            ),
            None => format!("opened {} in {}", outcome.succeeded, action.name),
        }
    }

    fn run_custom_action(&mut self, index: usize) {
        let Some(action) = self.custom_actions.get(index).cloned() else {
            return;
        };
        let marked = self.visible_marked();
        let paths = if marked.is_empty() {
            self.visible_selected_row()
                .filter(|row| action.matches(&row.path))
                .map(|row| vec![row.path.clone()])
                .unwrap_or_default()
        } else {
            self.custom_marked_paths(&action)
        };
        if paths.is_empty() {
            self.set_message(self.custom_batch_summary(
                &action,
                &paths,
                &BatchOutcome {
                    succeeded: 0,
                    first_error: None,
                },
            ));
            return;
        }
        let outcome = if actions::has_paths_placeholder(&action.cmd) {
            match actions::run_custom_with_line(
                &action,
                &paths[0],
                &paths,
                self.matched_line(&paths[0]),
            ) {
                Ok(()) => BatchOutcome {
                    succeeded: paths.len(),
                    first_error: None,
                },
                Err(error) => BatchOutcome {
                    succeeded: 0,
                    first_error: Some((paths[0].clone(), error.to_string())),
                },
            }
        } else {
            run_batch(&paths, |path| {
                actions::run_custom_with_line(
                    &action,
                    path,
                    &[path.to_string()],
                    self.matched_line(path),
                )
            })
        };
        self.set_message(self.custom_batch_summary(&action, &paths, &outcome));
    }

    /// Copies the visible marked paths to the clipboard, newline-joined.
    fn copy_marked(&mut self) {
        let paths = self.visible_marked();
        let message = if paths.is_empty() {
            "no visible marked files".to_string()
        } else {
            match actions::copy(&paths.join("\n")) {
                Ok(()) => format!("copied {} paths", paths.len()),
                Err(error) => format!("error copying {} paths: {error}", paths.len()),
            }
        };
        self.set_message(message);
    }

    /// Moves every visible marked row to the trash, continuing after failures
    /// and reporting the first failure with the success count.
    fn trash_marked(&mut self) {
        let paths = self.visible_marked();
        let outcome = run_batch(&paths, actions::trash);
        self.set_message(batch_summary("trashed", paths.len(), &outcome));
    }

    fn transfer_summary(verb: &str, outcome: &actions::TransferOutcome, total: usize) -> String {
        if total == 0 {
            return "no visible marked files".to_string();
        }
        let mut summary = format!("{verb} {}", outcome.succeeded);
        if outcome.skipped > 0 {
            let reason = match (outcome.skipped_exists, outcome.skipped_directories) {
                (_, 0) => "exists".to_string(),
                (0, _) => "directory".to_string(),
                (exists, directories) => format!("exists {exists}, directory {directories}"),
            };
            summary.push_str(&format!(", skipped {} ({reason})", outcome.skipped));
        }
        if outcome.cancelled > 0 {
            summary.push_str(&format!(", cancelled {}", outcome.cancelled));
        }
        if outcome.failed > 0 {
            if let Some((path, error)) = &outcome.first_error {
                format!(
                    "error: {summary}; {} failed ({path}: {error})",
                    outcome.failed
                )
            } else {
                format!("error: {summary}; {} failed", outcome.failed)
            }
        } else if outcome.skipped == 0 && outcome.cancelled == 0 {
            summary.push_str(if outcome.succeeded == 1 {
                " file"
            } else {
                " files"
            });
            summary
        } else {
            summary
        }
    }

    fn enter_destination_picker(&mut self, kind: actions::TransferKind) {
        if self.transfer_job.is_some() {
            self.set_message("transfer already running (Esc to cancel)".into());
            return;
        }
        let paths = self.visible_marked();
        if paths.is_empty() {
            return;
        }
        let picker = DestinationPicker {
            kind,
            paths,
            previous_query: self.editor.input.clone(),
            previous_selected: self.selected,
            previous_show_weak: self.show_weak,
        };
        self.destination_picker = Some(picker);
        self.editor.clear();
        self.refresh_query_keep_history();
    }

    fn restore_destination_picker(&mut self, picker: DestinationPicker) {
        self.destination_picker = None;
        self.editor.input = picker.previous_query;
        self.editor.input_cursor = self.editor.input.len();
        self.editor.input_scroll = 0;
        self.refresh_query_keep_history();
        self.selected = picker.previous_selected;
        self.show_weak = picker.previous_show_weak;
    }

    fn cancel_destination_picker(&mut self) {
        if let Some(picker) = self.destination_picker.take() {
            self.restore_destination_picker(picker);
        }
    }

    fn choose_destination(&mut self) {
        if self.transfer_job.is_some() {
            self.set_message("transfer already running (Esc to cancel)".into());
            return;
        }
        let Some(destination) = self
            .visible_selected_row()
            .filter(|row| row.path.ends_with('/'))
            .map(|row| row.path.clone())
        else {
            self.set_message("no directory selected".to_string());
            return;
        };
        let Some(picker) = self.destination_picker.take() else {
            return;
        };
        let paths = picker.paths.clone();
        let kind = picker.kind;
        self.restore_destination_picker(picker);
        self.start_transfer(paths, destination.into(), kind);
    }

    fn start_transfer(
        &mut self,
        paths: Vec<String>,
        destination: std::path::PathBuf,
        kind: actions::TransferKind,
    ) {
        if self.transfer_job.is_some() {
            self.set_message("transfer already running (Esc to cancel)".into());
            return;
        }
        let total = paths.len();
        let done = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let worker_done = done.clone();
        let worker_cancel = cancel.clone();
        match std::thread::Builder::new()
            .name("file-transfer".into())
            .spawn(move || {
                let outcome = actions::transfer_with_progress(
                    &paths,
                    &destination,
                    kind,
                    &worker_cancel,
                    |n| {
                        worker_done.store(n, Ordering::Relaxed);
                    },
                );
                let _ = tx.send(outcome);
            }) {
            Ok(worker) => {
                self.transfer_job = Some(TransferJob {
                    kind,
                    total,
                    done,
                    cancel,
                    rx,
                    worker: Some(worker),
                });
                self.set_message(format!("transferring {total} files (Esc to cancel)"));
            }
            Err(error) => self.set_message(format!("error starting transfer: {error}")),
        }
    }

    fn poll_transfer(&mut self) {
        let Some(job) = &self.transfer_job else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(outcome) => Ok(outcome),
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(()),
        };
        let job = self.transfer_job.take().unwrap();
        match result {
            Ok(outcome) => {
                if job.kind == actions::TransferKind::Move {
                    for path in &outcome.succeeded_paths {
                        self.marks.remove(path);
                    }
                }
                let verb = if job.kind == actions::TransferKind::Move {
                    "moved"
                } else {
                    "copied"
                };
                self.set_message(Self::transfer_summary(verb, &outcome, job.total));
            }
            Err(()) => self.set_message(
                "error: transfer worker stopped; check destination for partial results".into(),
            ),
        }
    }

    fn configure_history(&mut self, enabled: bool) {
        self.history.enabled = enabled;
        self.history.pos = None;
        self.history.entries.clear();
        self.history.file = None;
        if enabled {
            let path = crate::frecency::default_queries_path();
            self.history.entries = crate::frecency::load_queries(&path);
            self.history.file = Some(path);
        }
    }

    fn clear_marks(&mut self) {
        let cleared = self.marks.len();
        self.marks.clear();
        if cleared > 0 {
            self.set_message(format!("cleared {cleared} marks"));
        }
    }

    fn history_step(&mut self, back: bool) {
        if !self.history.enabled || self.history.entries.is_empty() {
            return;
        }
        let next = match (self.history.pos, back) {
            (None, true) => Some(self.history.entries.len() - 1),
            (None, false) => return,
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < self.history.entries.len() => Some(i + 1),
            (Some(_), false) => {
                // stepped past the newest entry: back to a blank query
                self.history.pos = None;
                self.editor.clear();
                self.refresh_query();
                return;
            }
        };
        self.history.pos = next;
        if let Some(i) = next {
            self.editor.input = self.history.entries[i].clone();
            self.editor.input_cursor = self.editor.input.len();
            self.refresh_query_keep_history();
        }
    }

    fn push_history(&mut self) {
        if !self.history.enabled {
            return;
        }
        let q = self.editor.input.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.history.entries.retain(|prev| prev != &q);
        self.history.entries.push(q.clone());
        if let Some(file) = &self.history.file {
            crate::frecency::append_query(file, &q);
        }
    }

    /// Returns false when the app should quit.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.message = None;
        // the help overlay is modal: any key closes it, except the scroll
        // keys, which page through the listing when it overflows the box
        if self.help.open {
            match key.code {
                KeyCode::PageUp => {
                    self.help.scroll = self.help.scroll.saturating_sub(HELP_SCROLL_PAGE);
                }
                KeyCode::PageDown => {
                    self.help.scroll = self.help.scroll.saturating_add(HELP_SCROLL_PAGE);
                }
                _ => {
                    self.help.open = false;
                    self.help.scroll = 0;
                }
            }
            return true;
        }
        // the actions popup swallows navigation while open
        if let Some(selected) = self.menu {
            let entries = self.menu_entries();
            match key.code {
                KeyCode::Esc | KeyCode::Left => self.menu = None,
                KeyCode::Down => self.menu = Some((selected + 1) % entries.len()),
                KeyCode::Up => {
                    self.menu = Some((selected + entries.len() - 1) % entries.len());
                }
                KeyCode::Enter => self.run_menu_action(selected),
                _ => {}
            }
            return true;
        }
        if self.destination_picker.is_some() {
            match key.code {
                KeyCode::Esc => self.cancel_destination_picker(),
                KeyCode::Enter => self.choose_destination(),
                _ => {}
            }
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                return true;
            }
        }
        if key.code == KeyCode::Esc
            && let Some(job) = &self.transfer_job
        {
            job.cancel.store(true, Ordering::Relaxed);
            self.set_message("cancelling transfer after current file".into());
            return true;
        }
        // text editing is fixed and handled before the keymap, so those keys
        // can never be rebound (see fsearch::keymap::is_editing_key)
        match key.code {
            KeyCode::Left => self.editor.cursor_left(),
            KeyCode::Right if self.editor.input_cursor < self.editor.input.len() => {
                self.editor.cursor_right();
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.editor.cursor_start();
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.editor.cursor_end();
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.editor.delete_word_backward();
                self.refresh_query();
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.editor.delete_forward();
                self.refresh_query();
            }
            KeyCode::Backspace => {
                self.editor.delete_backward();
                self.refresh_query();
            }
            // matches is_editing_key: only bare characters (and ctrl-a/e/w/d
            // above) type; ctrl/alt-modified chars go to the keymap
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.editor.insert_char(c);
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
                if !self.engine.is_filter() && self.visible_selected_row().is_some() {
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
                self.editor.clear();
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
                self.preview_layout = self.preview_layout.next();
                if self.preview_layout == PreviewLayout::Hidden {
                    self.load_preview();
                }
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
            crate::keymap::Action::ThemeCycle => self.cycle_theme(),
            crate::keymap::Action::PreviewPageUp => {
                self.preview.scroll = self.preview.scroll.saturating_sub(PREVIEW_SCROLL_PAGE);
            }
            crate::keymap::Action::PreviewPageDown => {
                self.preview.scroll = self.preview.scroll.saturating_add(PREVIEW_SCROLL_PAGE);
            }
            crate::keymap::Action::Help => {
                self.help.open = !self.help.open;
                self.help.scroll = 0;
            }
            crate::keymap::Action::ToggleMark => {
                if self.marking_enabled()
                    && let Some(path) = self.visible_selected_row().map(|row| row.path.clone())
                {
                    if !self.marks.remove(&path) {
                        self.marks.insert(path);
                        if self.marks.len() == 1 {
                            // first mark: point at the batch-action menu
                            let menu = self
                                .keymap
                                .shortcut(crate::keymap::Action::Menu)
                                .unwrap_or_else(|| "menu".into());
                            self.set_message(format!("1 marked · {menu} for batch actions"));
                        }
                    }
                    // fzf-style: advance so a run of files marks quickly
                    self.move_selection(1);
                }
            }
            crate::keymap::Action::ClearMarks => {
                if self.marking_enabled() {
                    self.clear_marks();
                }
            }
        }
        true
    }

    /// ctrl-g: step through the presets live (session-only, never written
    /// back to config). The user's accent/border/hex overrides ride along.
    fn cycle_theme(&mut self) {
        let names = crate::theme::preset_names();
        let next = names[(crate::theme::preset_index(&self.theme_cfg.preset) + 1) % names.len()];
        self.theme_cfg.preset = next.to_string();
        self.theme = crate::theme::resolve_config(&self.theme_cfg);
        self.message = Some((format!("theme: {next}"), Instant::now()));
    }

    fn refresh_query(&mut self) {
        self.history.pos = None;
        self.refresh_query_keep_history();
    }

    fn refresh_query_keep_history(&mut self) {
        self.selected = 0;
        self.selection_anchor = None;
        self.show_weak = false;
        let query = if self.destination_picker.is_some() {
            if self.editor.input.is_empty() {
                "dir:".to_string()
            } else {
                format!("dir: {}", self.editor.input)
            }
        } else {
            self.editor.input.clone()
        };
        self.engine.set_query(&query, self.regex_mode);
    }

    /// Rows currently on screen: the strong block when weaker matches are
    /// folded away, otherwise every result.
    fn visible_len(&self) -> usize {
        if matches!(self.engine.mode(), Mode::Fuzzy)
            && !self.editor.input.is_empty()
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
            self.selection_anchor = None;
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
        self.selection_anchor = self
            .engine
            .results()
            .get(self.selected)
            .map(|row| row.path.clone());
    }

    fn restore_selection_anchor(&mut self) {
        // Content results append without reranking and can contain several
        // lines from one path. Path-only anchoring would jump to its first hit.
        if self.engine.mode() == Mode::Content {
            self.selection_anchor = None;
            return;
        }
        let Some(path) = self.selection_anchor.clone() else {
            return;
        };
        let new_index = self
            .engine
            .results()
            .iter()
            .position(|row| row.path == path);
        match new_index {
            Some(i) if i < self.visible_len() => self.selected = i,
            _ => self.selection_anchor = None,
        }
    }

    fn open_selected(&mut self) {
        self.open_selected_with(false);
    }

    fn open_selected_with_enter(&mut self) {
        self.open_selected_with(true);
    }

    fn open_selected_with(&mut self, use_enter_action: bool) {
        let Some(path) = self.visible_selected_row().map(|row| row.path.clone()) else {
            return;
        };
        // the calculator's "path" is the result — enter copies it
        if self.engine.mode() == Mode::Calc {
            self.set_message(match actions::copy(&path) {
                Ok(()) => format!("copied: {path}"),
                Err(error) => format!("error: {error}"),
            });
            return;
        }
        let enter_action = use_enter_action
            .then(|| self.matching_enter_action(&path))
            .flatten();
        let result = if let Some(action) = &enter_action {
            actions::run_custom_with_line(
                action,
                &path,
                std::slice::from_ref(&path),
                self.matched_line(&path),
            )
            .map(|()| {
                self.engine.record_open(&path);
            })
        } else {
            self.open_path(&path)
        };
        let message = match result {
            Ok(()) => enter_action.map_or_else(
                || format!("opened: {path}"),
                |action| format!("opened in {}: {path}", action.name),
            ),
            Err(error) => enter_action.map_or_else(
                || format!("error opening {path}: {error}"),
                |action| format!("error opening in {} {path}: {error}", action.name),
            ),
        };
        self.set_message(message);
    }

    /// The Enter behavior, shared by the Enter key and a double-click:
    /// open the selection (Open mode) or return it and exit (`--pick`).
    /// Returns false when the app should quit.
    fn activate_selected(&mut self) -> bool {
        if self.destination_picker.is_some() {
            self.choose_destination();
            return true;
        }
        match self.ui_mode {
            UiMode::Open => {
                self.push_history();
                self.open_selected_with_enter();
                true
            }
            UiMode::Pick => {
                let picked = self.visible_selected_row().map(|row| row.path.clone());
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
        // the help overlay captures the mouse while open: the wheel scrolls
        // it, a click on an item does nothing, a click outside closes it
        if self.help.open {
            match ev.kind {
                MouseEventKind::ScrollDown => self.help.scroll = self.help.scroll.saturating_add(3),
                MouseEventKind::ScrollUp => self.help.scroll = self.help.scroll.saturating_sub(3),
                MouseEventKind::Down(MouseButton::Left) if !self.help.area.contains(point) => {
                    self.help.open = false;
                    self.help.scroll = 0;
                }
                _ => {}
            }
            return true;
        }
        match ev.kind {
            MouseEventKind::ScrollDown => {
                if self.hit_test.results_area.contains(point) {
                    self.move_selection(1);
                } else if self.hit_test.preview_area.contains(point) {
                    self.preview.scroll = self.preview.scroll.saturating_add(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if self.hit_test.results_area.contains(point) {
                    self.move_selection(-1);
                } else if self.hit_test.preview_area.contains(point) {
                    self.preview.scroll = self.preview.scroll.saturating_sub(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // a left click outside the popup closes it; one on an entry
                // activates that entry (border rows just close)
                if self.menu.is_some() {
                    if self.menu_inner.contains(point) {
                        let entry = self.menu_offset + (point.y - self.menu_inner.y) as usize;
                        self.run_menu_action(entry);
                    } else {
                        self.menu = None;
                    }
                    return true;
                }
                if self.hit_test.results_area.contains(point) {
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
        let y_rel = row.saturating_sub(self.hit_test.results_area.y);
        let mut cursor_y = 0u16;
        for (slot, h) in self
            .hit_test
            .slots
            .iter()
            .skip(self.list_state.offset())
            .copied()
        {
            if y_rel < cursor_y + h {
                return match slot {
                    Slot::Row(i) => {
                        let now = Instant::now();
                        let double = self.hit_test.last_click.is_some_and(|(prev, at)| {
                            prev == i && at.elapsed() < Duration::from_millis(450)
                        });
                        self.hit_test.last_click = Some((i, now));
                        self.selected = i;
                        self.selection_anchor =
                            self.engine.results().get(i).map(|row| row.path.clone());
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
        let Some(path) = self.visible_selected_row().map(|row| row.path.clone()) else {
            return;
        };
        self.set_message(match f(&path) {
            Ok(()) => format!("{verb}: {path}"),
            Err(error) => format!("error {verb}ing {path}: {error}"),
        });
    }

    fn load_preview(&mut self) {
        if self.preview_layout == PreviewLayout::Hidden || self.engine.mode() == Mode::Calc {
            if self.preview.for_key.take().is_some() {
                self.preview.generation += 1;
                self.preview
                    .latest_generation
                    .store(self.preview.generation, Ordering::Relaxed);
            }
            return;
        }
        let Some((path, line_number)) = self
            .visible_selected_row()
            .map(|row| (row.path.clone(), row.line_number))
        else {
            if self.preview.for_key.take().is_some() {
                self.preview.generation += 1;
                self.preview
                    .latest_generation
                    .store(self.preview.generation, Ordering::Relaxed);
            }
            self.preview.content = PreviewContent::Lines(vec![Line::from("no selection")]);
            self.preview.image_dims = None;
            return;
        };
        let key = (path.clone(), line_number);
        if self.preview.for_key.as_ref() == Some(&key) {
            return;
        }
        self.preview.for_key = Some(key);
        self.preview.scroll = 0;
        self.preview.image_dims = None;
        // expensive loading (file read, highlight, PDF/image decode) happens
        // on the preview worker; show a placeholder until poll_preview
        // delivers the result on a later tick
        self.preview.content = PreviewContent::Lines(vec![Line::from("loading...")]);
        self.preview.generation += 1;
        self.preview
            .latest_generation
            .store(self.preview.generation, Ordering::Relaxed);
        let _ = self.preview.tx.send(PreviewRequest {
            generation: self.preview.generation,
            path,
            line_number,
            appearance: self.appearance,
            gutter: self.theme.dim,
        });
    }

    /// Applies preview results that arrived since the last tick. Stale
    /// generations and superseded selections are dropped.
    fn poll_preview(&mut self) {
        while let Ok(result) = self.preview.rx.try_recv() {
            if self.preview_layout == PreviewLayout::Hidden || self.engine.mode() == Mode::Calc {
                continue;
            }
            if result.generation != self.preview.generation {
                continue;
            }
            if !self
                .preview
                .for_key
                .as_ref()
                .is_some_and(|(p, n)| p == &result.path && *n == result.line_number)
            {
                continue;
            }
            self.preview.content = match result.payload {
                PreviewPayload::Lines(lines) => PreviewContent::Lines(lines),
                PreviewPayload::Image(img) => {
                    self.preview.image_dims = Some((img.width(), img.height()));
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

    /// Refreshes the status line's stat cache when the selection changed.
    /// Runs in the event loop so the draw pass never touches the filesystem.
    fn refresh_status(&mut self) {
        let Some(path) = self.visible_selected_row().map(|row| row.path.clone()) else {
            self.status.path.clear();
            self.status.meta = None;
            return;
        };
        if self.status.path != path {
            self.status.path = path.clone();
            self.status.meta = std::fs::metadata(&path)
                .ok()
                .map(|meta| (meta.is_file(), meta.len(), meta.modified().ok()));
        }
    }
}

/// Coalesce pending work before decoding. Already-running decoding is not
/// interrupted, but stale queued requests never form a long preview backlog.
fn latest_preview_request(
    rx: &mpsc::Receiver<PreviewRequest>,
) -> Result<PreviewRequest, mpsc::RecvError> {
    let mut request = rx.recv()?;
    while let Ok(newer) = rx.try_recv() {
        request = newer;
    }
    Ok(request)
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

fn is_source_file(path: &str) -> bool {
    if path.ends_with('/') {
        return false;
    }
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(crate::filters::kind_for_ext)
        == Some("code")
}

fn open_tty() -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).open("/dev/tty")
}

fn nvim_args(path: &str, line: Option<u64>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(line) = line {
        args.push(format!("+{}", line.max(1)));
    }
    args.extend(["--".into(), actions::absolute_path(path)]);
    args
}

fn run_nvim_foreground(path: &str, line: Option<u64>) -> std::io::Result<()> {
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let stdin = tty.try_clone()?;
    let stdout = tty.try_clone()?;
    let status = Command::new("nvim")
        .args(nvim_args(path, line))
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(tty))
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("nvim exited with {status}")))
    }
}

type PanicHook = dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static;

/// Armed before the first terminal mutation, not after successful setup.
/// Owns a duplicate TTY descriptor so cleanup does not depend on reopening it.
struct TerminalGuard {
    tty: std::fs::File,
    mouse: bool,
    active: bool,
    previous_hook: Option<Arc<PanicHook>>,
}

impl TerminalGuard {
    fn new(tty: std::fs::File, mouse: bool) -> Self {
        Self {
            tty,
            mouse,
            active: false,
            previous_hook: None,
        }
    }

    fn install_hook(&mut self) {
        let previous: Arc<PanicHook> = std::panic::take_hook().into();
        let forward = previous.clone();
        let ui_thread = std::thread::current().id();
        let mouse = self.mouse;
        std::panic::set_hook(Box::new(move |info| {
            if crate::in_parser_guard() {
                return;
            }
            if std::thread::current().id() == ui_thread {
                restore_terminal(mouse);
            }
            forward(info);
        }));
        self.previous_hook = Some(previous);
    }

    fn enter(&mut self) -> std::io::Result<()> {
        self.active = true;
        let result = (|| {
            enable_raw_mode()?;
            execute!(self.tty, EnterAlternateScreen)?;
            if self.mouse {
                execute!(self.tty, EnableMouseCapture)?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.restore();
        }
        result
    }

    fn restore(&mut self) {
        if !self.active {
            return;
        }
        cleanup_terminal(&mut self.tty, self.mouse);
        self.active = false;
    }

    fn resume(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::fs::File>>,
    ) -> std::io::Result<()> {
        self.enter()?;
        if let Err(error) = terminal.clear() {
            self.restore();
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
        // Rust forbids changing panic hooks while unwinding. On normal error
        // returns (including setup/new/resume failure), restore the caller's hook.
        if !std::thread::panicking()
            && let Some(previous) = self.previous_hook.take()
        {
            std::panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

fn open_in_nvim(
    terminal: &mut Terminal<CrosstermBackend<std::fs::File>>,
    guard: &mut TerminalGuard,
    path: &str,
    line: Option<u64>,
) -> std::io::Result<std::io::Result<()>> {
    guard.restore();
    let result = run_nvim_foreground(path, line);
    // An editor failure is recoverable; a resume failure must end the UI.
    guard.resume(terminal)?;
    Ok(result)
}

fn cleanup_terminal(tty: &mut impl std::io::Write, mouse: bool) {
    // Separate commands: a partial failure must not suppress later cleanup.
    if mouse {
        let _ = execute!(tty, DisableMouseCapture);
    }
    let _ = execute!(tty, LeaveAlternateScreen);
    let _ = execute!(tty, ratatui::crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

fn restore_terminal(mouse: bool) {
    if let Ok(mut tty) = open_tty() {
        cleanup_terminal(&mut tty, mouse);
    } else {
        let _ = disable_raw_mode();
    }
}

/// Runs the UI. Draws on /dev/tty (not stdout), so `--pick` works inside
/// command substitution. In [`UiMode::Pick`], Enter returns the selection.
/// The arguments are the independent CLI/runtime knobs used to initialize the
/// App; keeping them explicit avoids hiding mode-specific behavior.
#[allow(clippy::too_many_arguments)]
pub fn run(
    engine: Engine,
    ui_mode: UiMode,
    initial_query: &str,
    theme_cfg: crate::config::ThemeConfig,
    keymap: crate::keymap::Keymap,
    mouse: bool,
    icons: bool,
    remember_session: bool,
    remember_history: bool,
    custom_actions: Vec<crate::config::CustomAction>,
    action_warning: Option<String>,
) -> anyhow::Result<Option<String>> {
    let (traits, picker) = probe_terminal();
    highlight::preload();
    let tty = open_tty()?;
    let mut guard = TerminalGuard::new(tty.try_clone()?, mouse);
    guard.install_hook();
    guard.enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(tty))?;
    let mut app = App::new(engine);
    app.keymap = keymap;
    app.appearance = traits.appearance;
    app.picker = picker;
    app.ui_mode = ui_mode;
    app.custom_actions = custom_actions;
    if let Some(warning) = action_warning {
        app.message = Some((warning, Instant::now()));
    }
    app.theme_cfg = theme_cfg;
    app.theme = crate::theme::resolve_config(&app.theme_cfg);
    app.icons = icons;
    if app.engine.is_filter() {
        // filter rows are arbitrary lines, not real files: start with the
        // preview hidden (Tab still cycles, previews of real files work)
        app.preview_layout = PreviewLayout::Hidden;
    } else if remember_session {
        // restore the layout and density saved by the last clean exit;
        // unknown or missing values keep the config defaults
        let state = crate::session::load(&crate::session::default_state_path());
        if let Some(layout) = state
            .preview_layout
            .as_deref()
            .and_then(PreviewLayout::from_key)
        {
            app.preview_layout = layout;
        }
        if let Some(density) = state.density.as_deref().and_then(Density::from_key) {
            app.density = density;
        }
    }
    app.configure_history(remember_history);
    if !initial_query.is_empty() {
        app.editor.input = initial_query.to_string();
        app.editor.input_cursor = app.editor.input.len();
        app.engine.set_query(&app.editor.input, app.regex_mode);
    }
    let result = loop {
        app.engine.tick();
        app.restore_selection_anchor();
        // side effects stay out of the draw pass: apply worker results,
        // issue new preview loads, and stat the selected path here
        app.poll_transfer();
        app.poll_preview();
        app.load_preview();
        app.refresh_status();
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
        if let Some((path, line)) = app.nvim_request.take() {
            match open_in_nvim(&mut terminal, &mut guard, &path, line) {
                Ok(Ok(())) => {
                    app.engine.record_open(&path);
                    app.set_message("returned from nvim".into());
                }
                Ok(Err(error)) => app.set_message(format!("error opening nvim: {error}")),
                Err(error) => break Err(error.into()),
            }
        }
    };
    // Cancel before dropping the job; join only waits for the current file.
    drop(app.transfer_job.take());
    guard.restore();
    if remember_session && result.is_ok() {
        // only a clean quit updates the saved settings; errors leave them alone
        crate::session::save(
            &crate::session::default_state_path(),
            app.preview_layout.key(),
            app.density.key(),
        );
    }
    result.map(|_| app.picked)
}

mod chrome;
mod preview;
mod rows;
#[cfg(test)]
mod tests;
use self::chrome::draw;
use self::preview::{
    PreviewContent, PreviewPayload, PreviewRequest, PreviewResult, preview_payload,
};
