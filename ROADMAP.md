# Roadmap

On main after v0.8.0:

- DOCX/XLSX content search, semantic indexing, and previews
- f16 + mmap semantic vector storage
- Loader mutation tests and a PTY smoke test in CI

Shipped in v0.8.0:

- Smart noise filtering (quiet paths) and the high-contrast `slate` theme

Shipped in v0.7.0:

- Stdin filter mode (`cmd | fsearch`) + ctrl-r history widget
- App launching and `= expr` calculator

Shipped in v0.6.0:

- Rich result rows + density toggle, precision ranking with a
  weaker-match fold, readline editing, mouse support, configurable
  keybindings, preview header/scrolling, background preview loading,
  indexing gauge, action toasts, theme border styles + tokens, and a
  performance round (indexing, content search, semantic search)

Shipped in v0.4.0:

- Metadata filters (kind:/changed:/larger:), `--big`, actions menu,
  Quick Look, query history, themes, arena index cache

Shipped in v0.3.0:

- Query filters (`ext:`, `path:`, `dir:`) across all search modes
- Search inside PDFs (cached text extraction)
- `--pick` mode + shell integration (Ctrl-T, `fcd`)
- Syntax hints under the search bar; size/age in the status line
- Optional chafa renderer for sharper cell-art previews

Shipped in v0.2.0:

- Syntax-highlighted previews (syntect + two-face, dark/light auto-detected)
- Image previews — PNG/JPEG/TIFF/GIF/WebP/BMP + SVG, best-protocol rendering
  with halfblock fallback
- Match-character highlighting in results
- Frecency: files you open rank higher
- Live index updates from filesystem events (no staleness between launches)
- `-p` print mode for scripting
- Linux support (xdg-open, wl-copy/xclip)
- CI, multi-platform release builds, shell installer

Planned:

- crates.io release
- Optional persistent full-text index for instant content search

Ideas welcome — open an issue.
