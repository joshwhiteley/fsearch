# file-search (`fsearch`) — Design

An open-source, Alfred-inspired file searcher for macOS, as a terminal TUI.
Exceptionally fast filename search (fuzzy or regex) over the home directory,
plus on-demand regex search inside file contents.

## Goals

- Launch-to-first-result in milliseconds via a persisted path index.
- Find any file under configured roots (default `~`), including hidden files.
- Plain fuzzy search by default; full regex when wanted; content grep on demand.
- Single static binary, no daemon, no external dependencies at runtime.

## Non-goals (v1)

- FSEvents live file watching (freshness comes from a background re-walk on launch).
- PDF/Office text extraction; content search covers text files only.
- A persistent full-text content index.
- GUI frontend.

## Stack

Rust. Key crates:

| Concern | Crate |
|---|---|
| TUI | `ratatui` + `crossterm` |
| Parallel walk | `ignore` (WalkBuilder, parallel) |
| Fuzzy matching | `nucleo-matcher` |
| Regex | `regex` |
| Content grep | `grep-searcher` + `grep-regex` |
| Config | `serde` + `toml` |
| Cache dirs | `dirs` |

## Architecture

Single crate, five modules with clear boundaries:

### `config`
- TOML at `~/.config/fsearch/config.toml`, created with defaults on first run.
- Fields: `roots` (default `["~"]`), `excludes` (glob list; defaults include
  `.git`, `node_modules`, `target`, `Library/Caches`, `.Trash`, `.cache`,
  `Library/Containers`, `Library/Application Support/MobileSync`),
  `max_content_filesize` (default 2 MiB).
- Tilde-expansion for roots. Invalid config → error message and exit, never a crash.

### `walker`
- Parallel walk of all roots using the `ignore` crate with gitignore semantics
  **disabled** (we want everything), hidden files **included**, excludes applied.
- Emits paths to a channel; unreadable dirs are skipped and counted.

### `index`
- In-memory `Vec<String>` of paths (files only).
- Persisted cache: `~/.cache/fsearch/index.bin` — version header + length-prefixed
  paths. Corrupt or version-mismatched cache is discarded and rebuilt.
- Lifecycle: on launch, load cache (ms) and show results immediately; a background
  thread re-walks, atomically swaps the fresh index in (via a shared `Arc` swap),
  and rewrites the cache. First run streams the initial walk into the UI with an
  "indexing…" status.

### `matcher`
- Runs on a worker thread; each keystroke cancels the previous search (generation
  counter) and starts a new one.
- Fuzzy mode (default): `nucleo-matcher` over paths, ranked, top-N (500) kept.
- Regex mode: `regex` crate matched against the full path.
- Match on file name primarily; fuzzy scoring favors filename over directory
  components (nucleo path scoring).

### `content`
- Triggered by a `>`-prefixed query: remainder is a regex grepped inside indexed
  files using ripgrep's `grep-searcher`, in parallel, honoring
  `max_content_filesize` and skipping binary files (NUL detection).
- Debounced (300 ms) rather than per-keystroke; results stream into the list as
  `path:line  snippet`. Cancelled when the query changes.

### `tui`
- Layout: input bar (top), results list (left), preview pane (right, toggleable).
- Preview: first ~100 lines of the file; in content mode, the matching line with
  context and the match highlighted.
- Status line: index size, mode (fuzzy/regex/content), match count, skip count.
- Keys:
  - `↑/↓` / `Ctrl-J`/`Ctrl-K` — navigate
  - `Enter` — open with default app (`open`)
  - `Ctrl-F` — reveal in Finder (`open -R`)
  - `Ctrl-Y` — copy path to clipboard (`pbcopy`)
  - `Ctrl-R` — toggle fuzzy/regex mode
  - `Tab` — toggle preview pane
  - `Esc` / `Ctrl-C` — quit
- Terminal is restored on panic (panic hook) and on all exit paths.

## Error handling

- Unreadable files/dirs: skip, count, show count in status line.
- Corrupt cache: delete, rebuild, no user-visible error.
- Invalid regex while typing: show "invalid pattern" in status line, keep last
  good results, never crash.
- Open/reveal/copy failures: status-line message.

## Performance targets

- Cache load + first paint: < 100 ms for ~1M paths.
- Per-keystroke fuzzy or regex match over ~1M paths: < 100 ms.
- Content grep: first results streamed < 500 ms on warm cache for scoped queries.

## Testing

- Unit: config parse/defaults/tilde-expansion; index cache roundtrip + corrupt
  cache recovery; matcher ranking (exact > prefix > scattered fuzzy) and regex
  filtering; walker excludes and hidden-file inclusion (temp dir fixtures).
- Integration: build index over a temp tree, run searches end-to-end (headless,
  no TUI), verify results and content matches.
- Perf sanity: generate ~1M synthetic paths, assert match latency budget
  (marked `#[ignore]`, run manually).

## Repo conventions

- Commits: incremental, plain style — `add: [thing]`, `feat: [feature]`,
  `bug: [fix]`, `docs: [doc]`. Sole author: Josh Whiteley
  <joshwhiteley89@gmail.com>. No co-author trailers.
- MIT license. README with install (cargo), usage, keybindings, config reference.
