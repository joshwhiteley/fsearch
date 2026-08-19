# Changelog

## 0.6.0 — 2026-08-19

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
