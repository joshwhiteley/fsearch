# Changelog

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
