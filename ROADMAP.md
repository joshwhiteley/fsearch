# Roadmap

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
- PDF and Office text extraction for content search
- Configurable keybindings
- Per-file-type filters (`ext:pdf`, `kind:image`)
- Optional persistent full-text index for instant content search

Ideas welcome — open an issue.
