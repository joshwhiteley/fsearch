# Architecture

This document describes how fsearch works internally. It follows the
[matklad ARCHITECTURE.md convention](https://matklad.github.io/2021/02/06/ARCHITECTURE.md.html):
read this before reading the code.

## Bird's eye view

fsearch answers one question fast: *"where is that file?"* It keeps a flat,
persisted list of every file path under your configured roots, ordered
newest-first, and filters it in memory on every keystroke. Content search
(`> pattern`) greps inside the indexed files on demand instead of maintaining
a full-text index.

The core trade: an index of *paths only* is small enough (tens of MB for
millions of files) to load in milliseconds, rebuild in seconds, and search
exhaustively per keystroke — so there is no query planner, no database, and
no daemon.

## Module map

```
src/
  main.rs      CLI entry: dispatch flags, filter mode, --reindex, -p
  cli.rs       argv parsing + help text (pure, no I/O)
  config.rs    config.toml: roots, excludes, theme, keymap, quiet, caps
  walker.rs    parallel walk (ignore crate), excludes, .app bundles, mtimes
  index.rs     versioned binary cache of the path list
  matcher.rs   filename matching: nucleo fuzzy/regex, ranking, quiet demotion
  content.rs   streaming parallel grep (ripgrep's grep-* crates)
  sem.rs       semantic index: chunking, embeddings, f16 + mmap store
  calc.rs      the `= expr` evaluator (recursive descent, no deps)
  quiet.rs     "quiet path" demotion patterns
  frecency.rs  open history → ranking boosts
  session.rs   remembers preview layout + density between runs
  keymap.rs    configurable keybindings (spec parser + action table)
  pdf.rs       PDF text extraction (cached, panic-guarded)
  office.rs    docx/xlsx text extraction (cached)
  engine.rs    orchestration: threads, generations, debounce, result state
  query.rs     headless one-shot search shared by -p / --big script mode
  highlight.rs syntax highlighting (syntect + two-face)
  images.rs    image decoding (image crate + resvg)
  theme.rs     UI color presets + tokens
  actions.rs   open / reveal / copy / trash
  util.rs      tiny shared helpers (human sizes, unix time)
  tui/
    mod.rs     App state, event loop, terminal probe
    rows.rs    result row rendering
    preview.rs preview worker + pane
    chrome.rs  input, wrapped help, status, gauge, toasts, menu
    tests.rs   TUI test suite
```

`main.rs` + `tui/` talk to a real terminal; everything else is a library
(`src/lib.rs`) exercised by the tests in `tests/`.

## Key design decisions

**The index is sorted by mtime, newest first.** This one decision makes
recency ranking free everywhere: an empty query shows the head of the list
(most recently modified files), regex results come back in index order
(newest first), fuzzy score ties break toward lower indices (newer files),
and content search walks candidates roughly newest-first as it streams.
No per-query timestamp lookup or recency sort is needed; mtime and size live
beside each cached path for filters and result metadata.

**Snapshots, not locks.** The path list lives in an `Arc<PathStore>`.
The indexer publishes complete replacement snapshots; searches clone the
`Arc` and run against an immutable arena-backed store.

**Generation counters instead of cancellation trees.** Every query bump
increments a generation. Workers tag their results with the generation they
were started for; the engine drops anything stale on arrival. Content
searches additionally carry an `Arc<AtomicBool>` cancel flag so an obsolete
grep stops burning CPU mid-file.

**Latest-job-wins search worker.** The filename-search worker drains its
queue to the newest job before running it, so typing fast never queues up
redundant searches.

**The watcher is armed before the walk.** Filesystem events (FSEvents /
inotify via the notify crate) start buffering *before* the initial walk
begins, then fold into fresh snapshots afterwards — changed files
front-insert (they're the newest), deletions filter out, and re-statting
each touched path makes replayed events idempotent. Without this ordering
there's an unfixable race between "walk finished" and "stream started".

**Frecency lives beside the index, not in it.** Opens append to a small
history file; at search time they become per-path score boosts.

**Semantic vectors are f16 and memory-mapped.** Loads parse document metadata
but map the vector tail read-only. Legacy f32 stores migrate without
re-embedding. Embedding calls are capped at 64 chunks, and very large
documents are sampled across at most 256 chunks.

**The terminal is probed once, before raw mode.** Background color (for
light/dark preview themes) and the graphics protocol (Kitty/iTerm2/halfblock
image previews) are queried at startup, before ratatui takes the terminal.
A terminal that answers nothing gets no further stdio queries — the
graphics probe leaks a stdin-reading thread when replies never come, which
would silently eat all keyboard input (found the hard way, in a PTY test).

## The engine's threads

```
UI thread (tui/)                   engine.tick() drains msg_rx every frame
  │ set_query(input)
  ▼
Engine ──── job_tx ────▶ search worker (latest job wins) ── msg_tx ──▶ results
  │                                                                     ▲
  ├─ indexer thread: load cache → publish → walk roots → publish fresh ─┤
  │     └─ then folds fs events (notify) into new snapshots, forever    │
  └─ content thread (per query, debounced 300 ms, cancel flag) ────────┘
```

All communication is `std::sync::mpsc`; the UI never blocks on a search.

## Search behavior

- **Fuzzy** (default): nucleo's path-aware scoring over all paths in
  parallel, plus a basename re-score so filename matches dominate, a
  best/2 floor that folds scattered-letter junk behind "weaker matches",
  and a quiet-path penalty that sinks `~/Library/`-style churn below the
  fold (path-intent queries skip the penalty).
- **Regex** (`ctrl-r`): the regex crate against full paths, smart-case,
  recency order.
- **Content** (`> pattern`): ripgrep's engine, binary/oversize-skipped,
  streamed. PDFs, docx and xlsx are searched through cached extracted text.
- **Semantic** (`? query`): brute-force cosine over f16 chunk embeddings,
  scored in parallel from a read-only mmap; unchanged files reuse vectors.
- **Calc** (`= expr`): evaluated synchronously; enter copies the result.
- **Apps**: .app bundles are indexed and launch via `open` on macOS.
- **Filter mode**: piped stdin lines run through fuzzy/regex and print the
  pick (`--filter`).
- **Filters** (`ext:`, `path:`, `dir:`, `kind:`, `changed:`, `larger:`):
  parsed from any query and applied in every mode.

## Previews

Text files are syntax-highlighted (syntect + two-face, dark/light by
terminal background). PDFs, docx and xlsx show extracted text; .app bundles
show their directory contents. Images (PNG/JPEG/TIFF/GIF/WebP/BMP/SVG)
render through ratatui-image's best protocol, with a halfblock fallback.
Preview loading runs on a worker thread so large files never block the UI.

## Testing strategy

- Unit tests live inside each module and cover the pure logic (parsing,
  ranking, cache format, exclude globs, color conversion).
- `tests/engine_test.rs` runs the real engine headlessly against temp
  directory trees — index build, filename/content/semantic search, cache
  reuse, mtime ordering, invalid patterns.
- `tests/cli_test.rs` executes the actual binary (`--version`, `--help`,
  `--config`, `--reindex`, `-p`) and asserts stdout/exit codes.
- `tests/perf_test.rs` (ignored by default) asserts the 1M-path latency
  budget in release mode.
- `tests/load_fuzz.rs` mutates valid index/semantic stores and feeds
  arbitrary bytes to the loaders, asserting they never panic.
- `tests/smoke.exp` drives the real TUI in a PTY (typing, results, clean
  exit) and runs in CI on macOS.
