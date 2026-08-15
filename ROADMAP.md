# Roadmap

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

- Homebrew tap publishing (`brew install joshwhiteley/tap/fsearch`)
- crates.io release
- Office document text extraction (docx, xlsx)
- Configurable keybindings
- Optional persistent full-text index for instant content search

Ideas welcome — open an issue.
