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
  main.rs      CLI dispatch, bounded stdin, saved scopes, semantic refresh
  cli.rs       argv parsing + help text (pure, no I/O)
  output.rs    text / NDJSON / NUL result and selection records
  health.rs    snapshot diagnostics and narrowly scoped cleanup
  config.rs    config.toml: roots, excludes, actions, saved searches, UI options
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
  query.rs     headless filename/content/semantic search for -p
  highlight.rs syntax highlighting (syntect + two-face)
  images.rs    image decoding (image crate + resvg)
  theme.rs     UI color presets + tokens
  actions.rs   open/reveal/trash, custom argv expansion, safe file transfers
  util.rs      private directory/file creation, human sizes, unix time
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
begins, then fold into fresh snapshots afterwards. Access events are ignored
so reading previews does not trigger another walk. Changed paths are
re-statted; replaced and deleted subtrees are pruned. Overlapping events
and roots are coalesced, paths are deduplicated, and snapshots are sorted
by actual mtime with path-order ties. A copied old file is not necessarily
the newest file.

Rescan flags and watcher errors rebuild configured roots with the same
excludes and app policy as startup. Watcher and save errors remain visible
across successful queries. This reduces missed-update windows; it is not a
freshness guarantee. Headless searches use the cached path snapshot when
available. `--reindex` explicitly refreshes it.

**Frecency lives beside the index, not in it.** Opens append to a small
history file; at search time they become per-path score boosts.
`remember_history = false` bypasses both history loading and recording.
Layout persistence is controlled separately by `remember_session`.

**Semantic vectors are f16 and memory-mapped.** Loads parse document metadata
but map the vector tail read-only. Legacy f32 stores migrate without
re-embedding. Embedding calls are capped at 64 chunks, and very large
documents are sampled across at most 256 chunks. `SemStore::query_filtered`
checks document predicates before scoring and truncation; `query` remains
an unfiltered wrapper. Interactive and headless semantic queries use the
filtered API, so restrictive filters do not depend on fixed over-fetching.

Normal `--index-semantic` runs walk roots and refresh path metadata before
reuse decisions. Additional source timestamp checks catch edits newer than
the previous semantic store, including same-second changes. Reuse still
relies on metadata rather than content hashes. Legacy migration is a
separate first invocation: it converts vectors without loading the model,
then exits. A subsequent invocation refreshes documents. The semantic
worker notices store replacements on later queries, but the path watcher
does not rebuild embeddings. ONNX Runtime is selected with safe explicit
`ort::init_from` calls, not a runtime environment mutation.

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
  ├─ content thread (per query, debounced 300 ms, cancel flag) ────────┤
  └─ semantic worker (lazy model, debounced queries, store reload) ────┘
```

Communication uses `std::sync::mpsc`; the UI does not wait for search
workers. Empty content and semantic queries clear pending debounce jobs.
File transfers use a separate UI-owned worker with progress and cancellation
between files. Foreground Neovim intentionally suspends the TUI until exit.

## Search behavior

- **Fuzzy** (default): nucleo's path-aware scoring over all paths in
  parallel, plus a basename re-score so filename matches dominate, a
  best/2 floor that folds scattered-letter junk behind "weaker matches",
  and a quiet-path penalty that sinks `~/Library/`-style churn below the
  fold (path-intent queries skip the penalty). Project directories can
  match their own names; a final-two-segment bonus helps files inside matching
  projects. When a semantic store exists, bare fuzzy queries also request
  semantic results and merge them using reciprocal rank fusion. Filename
  metadata is retained for shared paths, with semantic context added.
  `unified = false` disables blending.
- **Regex** (`ctrl-r`): the regex crate against full paths, smart-case,
  recency order.
- **Content** (`> pattern`): ripgrep's engine, binary/oversize-skipped,
  streamed. PDFs, docx and xlsx are searched through cached extracted text.
- **Semantic** (`? query`): brute-force cosine over f16 chunk embeddings,
  scored in parallel from a read-only mmap; unchanged files reuse vectors.
- **Calc** (`= expr`): evaluated synchronously; enter copies the result.
- **Apps**: .app bundles are indexed and launch via `open` on macOS.
- **Filter mode**: piped stdin records run through fuzzy/regex and print the
  pick (`--filter`). `matcher::search_lines` keeps arbitrary slash-ending
  records rather than applying default filesystem-directory suppression.
  Explicit filters still apply; `>`/`?`/`=` are ordinary input text.
- **Filters** (`ext:`, `path:`, `dir:`, `kind:`, `changed:`, `larger:`):
  parsed from search queries and applied in filename, content and semantic
  modes. Calculator expressions bypass filter parsing.

## Previews

Text files are syntax-highlighted (syntect + two-face, dark/light by
terminal background). PDFs, docx and xlsx show extracted text; .app bundles
show their directory contents. Images (PNG/JPEG/TIFF/GIF/WebP/BMP/SVG)
render through ratatui-image's best protocol, with a halfblock fallback.
ZIP, TAR and gzip-compressed TAR previews list bounded archive contents
without extracting files. Preview loading runs on a worker thread. Raster previews limit each dimension
to 16,384 pixels, total pixels to 32 Mi pixels, and decoder allocation to
128 MiB. Parsed and displayed text is capped; failures and guarded parser
panics become error previews instead of terminating the UI.

## Actions and transfer boundaries

Custom actions are argv arrays. `{path}`, `{dir}`, `{line}` and standalone
`{paths}` expand from the original template, so placeholder-like text inside
a filename stays literal. No shell is invoked implicitly. Optional extension
and kind checks are shared by menu actions and Enter overrides; directory
rows do not match. Detected GUI-editor actions are defaults only when the
configured action list is empty. Neovim opens content/semantic hits at the
matched line and restores the terminal on exit.

Transfers operate on regular files only. They skip directories and reject
source symlinks and special files. macOS/Linux moves first use native atomic
no-replace rename, preserving inode identity and metadata. Only a
cross-device error falls back to staged copy and source removal. Copies
are written and synced to an exclusive destination-local staging file,
then published with native atomic no-replace rename. If the destination
filesystem does not support this publication operation, the transfer fails
closed and leaves the source untouched.

The copy path preserves data and permission bits, not timestamps, ownership,
ACLs or extended attributes. A failed source removal leaves both copies and
reports failure. Source paths must not be concurrently changed or replaced:
the identity check before cross-device unlink cannot make pathname unlink
atomic with that check. Cancellation takes effect between files.

## CLI records, local state and diagnostics

CLI options precede the command; the rest of `-p`/`--pick`/`--filter` is query
text. `[searches]` supplies named queries/scopes through `--saved NAME`;
`--searches` lists them. There is no interactive saved-search picker.

`output.rs` separates records from display formatting. `--json` emits typed
NDJSON hits/selections (`--big` emits file metadata); `--json --status` emits
one health object. `--print0` emits a NUL-terminated path per hit, including
content hits that share a path. It does not include line/score text.
`--read0` selects NUL-delimited filter input. Stdin is UTF-8 only, bounded
at 64 MiB total, 1 MiB per record and 500,000 records. Byte-limit or UTF-8
violations fail; record-count overflow warns and truncates.

Private helpers create app cache/state directories with Unix mode 0700 and
files with mode 0600. Atomic publication uses exclusive staging files and
fsync; history appends reject final symlinks and non-regular files. Existing
ancestor directories are not chmod'd. This is local permission protection,
not encryption or a guarantee against concurrently hostile path mutation.

`--no-history` disables open/query history and remembered layout for a run,
not index, model or extracted-text caches. `--clear-cache` removes known
path/semantic indexes and extraction caches. `--clear-history` removes
open/query history and layout. Cleanup does not delete models, configuration
or source documents. Other running instances can recreate cleared data.

`health.rs` inspects root readability and persisted cache validity, size,
age and counts without starting an indexer, watcher, model or terminal
probe. Extracted-cache file/byte counts inspect at most 8,192 entries per
directory and report truncation. `--status` reports a snapshot, not live watcher health or semantic
freshness. `--doctor` remains the terminal-probe diagnostic.

## Testing strategy

- Unit tests live inside each module and cover the pure logic (parsing,
  ranking, cache format, exclude globs, color conversion).
- `tests/engine_test.rs` runs the real engine headlessly against temp
  directory trees — index build, filename/content/semantic search, cache
  reuse, mtime ordering, invalid patterns, disabled history and restrictive
  semantic filters beyond 400 documents. Unit tests inject watcher events
  and due debounce timestamps without relying on OS event timing.
- `tests/cli_test.rs` executes the actual binary (`--version`, `--help`,
  `--config`, `--reindex`, `-p`) and asserts stdout/exit codes.
- `tests/perf_test.rs` (ignored by default) asserts the 1M-path latency
  budget in release mode.
- `tests/load_fuzz.rs` mutates valid index/semantic stores and feeds
  arbitrary bytes to the loaders, asserting they never panic.
- `tests/smoke.exp` drives the real TUI in a PTY (typing, results, clean
  exit and `--no-history --pick` state isolation). CI runs it on macOS and
  Linux, alongside the engine's real watcher integration.
- CI checks the locked default and semantic builds with exactly Rust 1.90.0,
  in addition to current-stable tests, clippy, formatting and all-feature
  cargo-deny advisory/source checks.
