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
  main.rs      CLI entry: dispatches flags (--help/--config/--reindex/-p) or the TUI
  cli.rs       argv parsing + help text (pure, no I/O)
  config.rs    ~/.config/fsearch/config.toml: roots, excludes, size caps
  walker.rs    parallel directory walk (ignore crate), exclude globs, mtimes
  index.rs     versioned binary cache of the path list (atomic save, corrupt-safe load)
  matcher.rs   per-keystroke filename matching: nucleo fuzzy / regex, + match positions
  content.rs   streaming parallel grep (ripgrep's grep-searcher/grep-regex crates)
  engine.rs    orchestration: threads, generations, debounce, result state
  highlight.rs syntax highlighting for previews (syntect + two-face)
  images.rs    image decoding for previews (image crate + resvg for SVG)
  actions.rs   open / reveal-in-Finder / copy-path
  tui.rs       ratatui UI: input, results, preview, keybindings
```

`main.rs` + `tui.rs` are the only modules that talk to a real terminal;
everything else is a library (`src/lib.rs`) exercised directly by the tests
in `tests/`.

## Key design decisions

**The index is sorted by mtime, newest first.** This one decision makes
recency ranking free everywhere: an empty query shows the head of the list
(most recently modified files), regex results come back in index order
(newest first), fuzzy score ties break toward lower indices (newer files),
and content search walks candidates roughly newest-first as it streams.
No per-query sorting or timestamp storage is needed at search time — the
cache file doesn't even contain mtimes.

**Snapshots, not locks.** The path list lives in an `Arc<Vec<String>>`.
The indexer publishes complete replacement snapshots; searches clone the
`Arc` (cheap) and run against an immutable list. There is no shared mutable
state between the walker, the search worker, and the UI.

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
history file; at search time they become per-path score boosts — tie-breaks
in fuzzy mode, front-floats in recency-ordered lists.

**The terminal is probed once, before raw mode.** Background color (for
light/dark preview themes) and the graphics protocol (Kitty/iTerm2/halfblock
image previews) are queried at startup, before ratatui takes the terminal.
A terminal that answers nothing gets no further stdio queries — the
graphics probe leaks a stdin-reading thread when replies never come, which
would silently eat all keyboard input (found the hard way, in a PTY test).

## The engine's threads

```
UI thread (tui.rs)                 engine.tick() drains msg_rx every frame
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

- **Fuzzy** (default): nucleo's path-aware scoring across all paths in
  parallel (rayon chunks), smart-case, top-500 by score then recency.
- **Regex** (`ctrl-r`): the regex crate against full paths, smart-case,
  results in recency order.
- **Content** (`> pattern`): ripgrep's engine over indexed files, skipping
  binaries (NUL detection) and files over `max_content_filesize`, capped at
  20 hits per file / 1000 total, streamed into the UI as found.

## Previews

Text files are syntax-highlighted with syntect + two-face themes
(OneHalfDark/OneHalfLight picked by the terminal's background). The style
converter implements bat's alpha-channel color convention so ANSI-palette
themes don't render black-on-black. Images (PNG/JPEG/TIFF/GIF/WebP/BMP, and
SVG via resvg) render through ratatui-image using the best protocol the
terminal supports, falling back to colored half-block cells anywhere.

## Testing strategy

- Unit tests live inside each module and cover the pure logic (parsing,
  ranking, cache format, exclude globs, color conversion).
- `tests/engine_test.rs` runs the real engine headlessly against temp
  directory trees — index build, all three search modes, cache reuse,
  mtime ordering, invalid patterns.
- `tests/cli_test.rs` executes the actual binary (`--version`, `--help`,
  `--config`, `--reindex`, `-p`) and asserts stdout/exit codes.
- `tests/perf_test.rs` (ignored by default) asserts the 1M-path latency
  budget in release mode.
- An `expect`-driven PTY smoke script drives the real TUI end to end
  (typing, previews, image rendering, clean exit); it caught the
  stdin-eating bug above.
