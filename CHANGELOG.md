# Changelog

## Unreleased

- Source files gain an "open in nvim" action. fsearch temporarily leaves its
  terminal screen, runs Neovim in the foreground, then restores the same
  search session when Neovim exits. Content/semantic hits open at their
  matched line; custom action templates also accept `{line}` (default 1)
- `--status` reports root readability, persisted index health and enabled
  build features without probing the terminal or starting a watcher/model.
  Snapshot age is not a path or semantic freshness guarantee
- `--json` emits typed NDJSON search/selection records and structured status;
  `--print0` emits NUL-terminated paths/selections, and `--read0` accepts
  NUL-separated UTF-8 filter input. Options must precede the command.
  Stdin is bounded to 64 MiB total, 1 MiB per record and 500,000 records
- `[searches]` configures CLI named queries/scopes, selected with
  `--saved NAME` and listed with `--searches`
- `remember_history` controls history independently of `remember_session`.
  `--no-history` disables history and remembered layout for one run, not
  search or extraction caches. `--clear-cache` and `--clear-history` remove
  known local data while keeping models, configuration and source documents;
  close concurrent instances first to prevent recreation
- New app-managed cache/state directories and files use private Unix modes
  0700 and 0600. Staging files are created exclusively; history appends reject
  final symlinks and special files
- Watcher reads no longer trigger reindexing. Rescan flags and errors rebuild
  configured roots with excludes/apps; runtime errors remain visible.
  Snapshots preserve mtime order and deduplicate overlapping paths/work
- Clearing a content or semantic query cancels its pending debounce job.
  Restrictive semantic filters now run before truncation in both interactive
  and headless searches; arbitrary stdin lines ending in `/` remain searchable
- Normal semantic refresh walks roots and rechecks metadata before vector
  reuse. Legacy f32 migration remains a separate first invocation.
  ONNX Runtime loads through an explicit safe API without mutating the
  running process environment
- File transfers run in a worker with progress and cancellation between
  files. macOS/Linux same-filesystem moves use native no-replace rename;
  cross-device moves stage a copy before deleting the source. Collisions
  never overwrite destinations, and source symlinks/special files are
  rejected. Copy fallback preserves bytes/permissions, not timestamps or
  extended filesystem metadata
- Custom actions consistently apply extension/kind filters and exclude
  directory rows. Template expansion does not reinterpret placeholders
  embedded in filenames
- Minimum supported Rust is now 1.90. CI checks locked default and semantic
  builds with exactly 1.90.0. Advisory checks now include optional features;
  the unmaintained `paste` exception remains for tokenizers/fastembed.
  Portable PTY smoke coverage now runs on Linux as well as macOS, including
  a no-history pick session

## 0.10.0 — 2026-08-31

- Built-in default actions: with no `[[actions]]` configured, installed GUI
  code editors (Cursor, VS Code, Zed, Sublime Text) get "open in …" menu
  entries for code files; defining your own actions replaces the defaults
- Unified search: bare queries blend filename and semantic results into one
  ranked list (reciprocal rank fusion); semantic-only rows are labeled, and
  `unified = false` restores the old filename-only behavior
- Custom actions: `[[actions]]` entries in config.toml add commands to the
  actions menu with `{path}`/`{paths}`/`{dir}` placeholders, optional
  `ext`/`kind` filters, and `enter = true` to replace the default opener for
  matching files (e.g. code → Cursor, pdf → Preview, docx → Word)
- Move/copy marked files: the actions menu gains "move marked to…" and
  "copy marked to…", with an in-app directory picker; collisions are skipped
  and reported, cross-device moves fall back to copy+delete
- Archive preview: selecting a .zip, .tar, .tar.gz, or .tgz lists its
  contents in the preview pane (bounded, corrupt-safe)
- Project search: well-matching directories now appear in default fuzzy
  results (finding `sage-kc/` by typing "sage kc"), and files inside a
  matching directory get a segment bonus so project contents outrank
  similarly-named decoys
- Script mode hits write through the pipe-safe path again (`fsearch -p q |
  head` no longer panics)
- Marking moved from `ctrl-b`/`alt-b` to `ctrl-s`/`alt-s`: terminal
  multiplexers (tmux, herdr) swallow `ctrl-b` as their prefix key before the
  app ever sees it. Marking now advances to the next row fzf-style, and the
  first mark shows a toast pointing at the batch-actions menu
- Quick Look's panel opened behind the focused terminal, which made
  `ctrl-space` look like a no-op; fsearch now raises it (needs the terminal
  to have Accessibility permission; silently skipped otherwise)
- `forge` theme preset: warm copper/wheat/steel on near-black with rounded
  borders, matching the pi "forge" terminal theme

## 0.9.0 — 2026-08-23

- TUI: cycle themes with `ctrl-g`, open the keymap-driven help overlay with
  `f1`/`ctrl-o`, and mark visible files with `ctrl-b`/`alt-b` for batch actions;
  Nerd Font icons, row density, session restore, and configurable keys remain
  integrated.
- Fixed quadratic line counting in semantic chunking, so indexing large
  documents is linear in size instead of stalling
- Semantic, path-index, and cache writes now use unique temp names and fsync
  before rename; concurrent fsearch processes can no longer corrupt each
  other's stores, and a full disk can no longer publish truncated extraction
  caches
- The preview worker survives panics from malformed images or SVGs, preview
  reads are capped at the 64 KiB window instead of loading whole files, and a
  docx/xlsx parser panic no longer prints a backtrace into the TUI
- Semantic search picks up reindexing done in another terminal without a
  restart, watcher updates handle directory/file swaps without leaving ghost
  entries, and failed exclude globs or watch roots surface an error instead of
  silently blanking results
- Script mode (`-p`, `--big`) shares one query API with the TUI: metadata
  filters like `larger:` and `changed:` now apply to `?` searches too
- Piping output to `head` exits cleanly instead of panicking on a broken
  pipe; stdin filter mode warns when it truncates at 500k lines; cancelled
  filter/pick runs exit 1 while real errors exit 2
- TUI: long queries scroll horizontally with the cursor kept visible, hint
  rows yield space on short terminals, the actions menu responds to clicks,
  and empty result sets show a "(no matches)" state with a minimal footer
- Shell integration works with macOS system bash 3.2 again
- Dependencies: dropped unused OpenEXR/AV1 image codecs (faster builds,
  smaller binary), declared MSRV 1.85, added Dependabot, cargo-audit/
  cargo-deny gating, `cargo test --features semantic` in CI, and locked CI
  builds
- Search hints now wrap instead of clipping; highlighted results show the
  configured shortcuts for open, reveal, copy path, Quick Look, actions, and
  preview
- DOCX and XLSX text extraction for content search, semantic indexing,
  snippets, and previews; extraction is bounded and cached
- Semantic vectors now use f16 on disk and a read-only mmap at query time,
  cutting store size roughly in half. The first `--index-semantic` run
  migrates an existing store without re-embedding; run it again when you want
  to add new or changed documents
- Semantic indexing caps inference batches and samples across very large
  documents, preventing giant PDFs and spreadsheets from creating multi-GB
  model batches
- Corrupt path and semantic stores are covered by deterministic mutation
  tests; CI now drives the real TUI through a PTY

## 0.8.0 — 2026-08-21

- Smart noise filtering: app-internal and hidden-directory paths (logs,
  `~/Library/` state, dotfile churn) are demoted behind the weaker-matches
  fold and hidden from the launch screen — `ctrl-x` reveals them, and a
  `/` in a query or a `path:` filter searches them at full rank.
  Configurable via `quiet = [...]` (empty list disables)
- `slate` theme preset: higher-contrast dark theme with a selection tint
  instead of reverse video
- .app bundles preview their directory contents instead of "(unreadable)"

## 0.7.0 — 2026-08-20 (never tagged; first shipped in the v0.8.0 binaries)

- Filter mode: `git ls-files | fsearch` (or `… | fsearch --filter`)
  fuzzy-filters piped stdin lines in the full TUI and prints the
  selection — plus a ctrl-r shell-history widget in the zsh/bash
  integration
- App launching (macOS): /Applications bundles are indexed — type
  `safari`, hit enter, Safari opens; `kind:app` narrows to apps and
  `index_apps = false` opts out
- `= 2*(3+4)` inline calculator; enter copies the result
- Split the tui into focused modules
- Fixed: a panicking PDF (exotic encodings) no longer wrecks the
  terminal during content search, and extraction failures are cached
  so broken PDFs are attempted once, not per keystroke
- Fixed: semantic score bars sliced a UTF-8 char boundary and crashed

## 0.6.0 — 2026-08-19 (never tagged; first shipped in the v0.8.0 binaries)

- Rich result rows: colored kind badges, bold filenames, dim parent
  path + size, right-aligned age; grep and semantic hits show the
  matched line under the file. `ctrl-t` toggles a compact single-line
  density
- Precision ranking: filename matches outrank letters scattered across
  the path, and the low-scoring tail folds away behind
  "weaker matches" (`ctrl-x` shows them). fzf-style atoms documented:
  `'word` exact, `^word` prefix, `word$` suffix, `!word` excludes
- Semantic results show the matched line and a score bar, and honor
  `changed:` / `larger:` / `smaller:` filters
- Preview: breadcrumb + metadata header, scrollbar with `pgup`/`pgdn`
  paging, image dimensions — and loading runs off the UI thread, so
  big PDFs and images never freeze the app
- Query input: real cursor with readline-style editing
  (ctrl-a/e/w/d, arrows), live coloring of `>`/`?` prefixes and
  filter tokens
- Mouse support: click selects, double-click opens, wheel scrolls
  results and preview (`mouse = false` disables)
- Configurable keybindings: `[keys]` section in config.toml
- Action toasts instead of status-line messages; indexing shows a
  progress gauge
- Themes: `borders = "rounded"` / `"none"`, `selection_bg` /
  `match_fg` / `section` overrides, per-preset badge palettes
- Performance: cold-start indexing is no longer quadratic, content
  search stops cloning the whole path list per query, semantic
  indexing batches embeddings and queries score in parallel, PDF text
  cache writes are atomic and bounded

## 0.5.0 — 2026-08-15

- Semantic search: `? growing tomatoes` ranks notes, docs and PDFs by
  meaning. `fsearch --index-semantic` builds the vector index (re-runs
  embed only changed files); needs the optional `semantic` build
  feature. Everything runs locally — all-MiniLM-L6-v2 over ONNX
  Runtime, nothing leaves the machine
- Launch screen sections: with an empty query, results group under
  "recent opens" and "recently modified"

## 0.4.0 — 2026-08-15

- Metadata filters: `kind:image`, `changed:7d`, `larger:100mb`,
  `smaller:` — composable with every search mode
- `fsearch --big [N]`: largest files in the index
- Actions menu on `→`: open, reveal, copy path, Quick Look, move to trash
- Quick Look on `ctrl-space` (macOS)
- Query history: `ctrl-p` / `ctrl-n`
- Themes: catppuccin, gruvbox, nord, tokyonight presets + accent override
- Arena index cache: one allocation instead of a million; one-shot
  searches 1.7x faster (see docs/blog/arena-cache.md, including a
  measured negative result on incremental narrowing)
- Index format v3 (rebuilds automatically; stores mtime + size)

## 0.3.0 — 2026-08-15

- Query filters: `ext:pdf`, `path:term` narrow any search — including
  content search — and `dir:` searches folders (with directory previews)
- Search inside PDFs: content search greps extracted text, cached by
  path + mtime + size so repeats are instant
- `--pick` mode: full UI, but Enter prints the selection to stdout —
  plus zsh/bash integration (Ctrl-T insert, `fcd`)
- Syntax reminders under the search bar until you start typing
- Selected file's size and age in the status line
- Sharper cell-art image previews via the optional `chafa` build feature
- `--doctor` prints what the terminal probe detected
- Fixed: no Kitty-graphics upgrade on a bare capability ACK (some
  terminals answer the query but never render)

## 0.2.0 — 2026-08-15

- Syntax-highlighted previews (syntect + two-face, terminal-adaptive
  dark/light themes)
- Image previews: PNG, JPEG, TIFF, GIF, WebP, BMP, SVG — Kitty/iTerm2
  protocols with halfblock fallback
- Matched characters highlighted in results
- Frecency: files you open rank higher
- Live index updates from filesystem events
- `-p` print mode for scripting
- Linux support (xdg-open, wl-copy/xclip)
- CI, multi-platform release builds, shell installer

## 0.1.0 — 2026-08-14

- Initial release: persisted home-directory index, fuzzy/regex filename
  search, streaming content search, recency-sorted results, terminal UI
  with previews
