# Changelog

## Unreleased

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
