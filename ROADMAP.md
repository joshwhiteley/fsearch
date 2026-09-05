# Roadmap

Implemented for the next release:

- Snapshot health diagnostics (`--status`), typed NDJSON (`--json`) and
  NUL-delimited input/output (`--read0`, `--print0`)
- CLI named queries/scopes (`[searches]`, `--saved`, `--searches`)
- Separate history/layout controls, `--no-history`, private cache/state
  permissions, and explicit cache/history cleanup
- Foreground Neovim and matched-line custom actions; responsive transfer
  progress/cancellation and no-replace moves/copies
- Watcher access-event suppression, rescan/error recovery, sorted unique
  snapshots, empty-query debounce cancellation, and semantic filtering
  before truncation
- Fresh discovery for semantic refresh, safe explicit ONNX Runtime loading,
  and exact Rust 1.90.0 checks for default and semantic builds

Shipped in v0.10.0:

- Unified filename/semantic ranking, configurable with `unified = false`
- Custom argv actions and detected GUI-editor defaults
- Marked-file move/copy with a destination picker
- Bounded ZIP/TAR/TAR.GZ/TGZ content listings in previews
- Project-directory matches and path-segment ranking bonuses
- `forge` theme; marking on `ctrl-s` / `alt-s`

Shipped in v0.9.0:

- DOCX/XLSX content search, semantic indexing, and previews
- f16 + mmap semantic vector storage (half the size, mmap-fast loads,
  migration without re-embedding)
- A robustness round from a 12-lane audit: atomic + fsync'd store writes,
  panic-proof previews, watcher liveness fixes, one shared query path for
  script mode, bash 3.2 shell integration, broken-pipe-safe output
- Multi-select marks with batch open/copy/trash, a keymap-driven help
  overlay (f1), live theme cycling (ctrl-g), Nerd Font icons, and session
  memory for layout/density
- Supply-chain gates (cargo-audit, cargo-deny, Dependabot), MSRV 1.85,
  trimmed image codecs, loader mutation tests, and a PTY smoke test in CI

Shipped in v0.8.0:

- Smart noise filtering (quiet paths) and the high-contrast `slate` theme

Shipped in v0.7.0 (never tagged; first shipped in the v0.8.0 binaries):

- Stdin filter mode (`cmd | fsearch`) + ctrl-r history widget
- App launching and `= expr` calculator

Shipped in v0.6.0 (never tagged; first shipped in the v0.8.0 binaries):

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
- Live index updates from filesystem events (cached snapshots can still be
  stale; explicit reindexing remains available)
- `-p` print mode for scripting
- Linux support (xdg-open, wl-copy/xclip)
- CI, multi-platform release builds, shell installer

Planned:

- crates.io release (blocked: the `fsearch` name is taken by a yanked
  placeholder crate — needs a rename or an ownership transfer first)
- Optional persistent full-text index for instant content search

Ideas welcome — open an issue.
