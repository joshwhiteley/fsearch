# fsearch

[![ci](https://github.com/joshwhiteley/fsearch/actions/workflows/ci.yml/badge.svg)](https://github.com/joshwhiteley/fsearch/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Fast file search for your Mac, in the terminal. Type to fuzzy-find any file
under your home folder, hit enter to open it. Inspired by Alfred.

![fsearch demo](demo.gif)

fsearch keeps a persisted index of every file under your home directory —
hidden files included — and filters it as you type. Previews are
syntax-highlighted; images (PNG, JPEG, TIFF, GIF, WebP, SVG…) render right
in the terminal. Results are sorted by last modified and by what you
actually open, and the index updates live as files change on disk — no
re-scanning, no staleness. Works on macOS and Linux.

## Install

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/joshwhiteley/fsearch/releases/latest/download/fsearch-installer.sh | sh
```

From a source checkout:

```sh
cargo install --path .
```

## Use

Run `fsearch` and start typing.

- plain text — fuzzy search on file names, matches highlighted
- `ctrl-r` — regex on the full path
- `'word` — exact substring; also `^word` prefix, `word$` suffix, `!word` excludes
- `> pattern` — regex search inside files, streamed as `path:line`;
  PDF, DOCX and XLSX text is extracted and cached
- `? growing tomatoes` — semantic search: notes, PDFs and Office documents
  ranked by meaning, not exact words (an optional build feature — see below)
- `ext:pdf`, `path:term` — narrow any search (content search included,
  so `> ext:md TODO` greps only markdown)
- `kind:image` (also video, audio, doc, code, archive), `changed:7d`,
  `larger:100mb` / `smaller:` — metadata filters that compose with any query
- `dir:` — search folders instead of files (preview lists their contents)
- `= 2*(3+4)` — inline calculator; enter copies the result
- apps are indexed too (macOS): type `safari`, hit enter, Safari launches
  (`index_apps = false` turns this off)
- noisy paths are demoted automatically: app-internal state (`~/Library/…`)
  and hidden-directory churn (logs, dotfiles) sit behind the weaker-matches
  fold and stay off the launch screen. `ctrl-x` reveals them, and typing a
  `/` in a query (or `path:`) searches them at full rank
- `enter` opens · `→` opens an actions menu (reveal, copy, quick look,
  move to trash) · `ctrl-space` Quick Look · `ctrl-y` copies the path ·
  `ctrl-p`/`ctrl-n` recall query history ·
  `tab` cycles the preview (side → full-window → hidden) · `esc` quits

Piped input flips fsearch into an fzf-style filter: `git ls-files |
fsearch` (or `… | fsearch --filter`) fuzzy-filters the lines and prints
your selection.

Scripting: `fsearch --big` lists the largest files in the index (a quick
"what's eating my disk"), and `fsearch -p QUERY` prints matches to stdout
(exit 1 when none),
and `fsearch --pick` runs the full UI but prints your selection instead of
opening it — so `vim "$(fsearch --pick)"` works. `fsearch --help` lists
everything else.

## Shell integration

Source `shell/fsearch.zsh` (or `.bash`) from your rc file to get:

- **Ctrl-T** — pick a file and insert its path at the cursor
- **`fcd`** — fuzzy-pick a directory and `cd` into it
- **Ctrl-R** — filter your shell history through fsearch

The first run indexes your home folder; later launches load the cached
index in milliseconds and refresh it in the background.

## Speed

Measured on an Apple-silicon MacBook (`tests/perf_test.rs`, hyperfine,
and a real 1.22M-entry home index; your numbers will vary):

| operation | time |
|---|---|
| fuzzy match, 1M paths (per keystroke) | ~26 ms |
| regex match, 1M paths (per keystroke) | ~12 ms |
| full re-index of 1.22M entries | ~8.7 s |
| one-shot `fsearch -p query` | ~220 ms |

Honest comparison: `mdfind` (Spotlight) answers one-shot queries in ~17 ms
because its daemon already holds an index in RAM — when it has one. On the
benchmark machine `mdfind -name readme` returned **0 results** while
fsearch found 500: Spotlight doesn't index hidden files or many dev trees,
and its coverage silently depends on per-volume indexing state. fsearch's
index is yours: predictable, inspectable, rebuildable with `--reindex`.

## When to use something else

- **fzf / television** — arbitrary-list filtering is covered
  (`git ls-files | fsearch`, the ctrl-r history widget), but fzf still
  has the deeper plugin ecosystem and preview scripting.
- **Spotlight / Alfred / Raycast** — fsearch launches apps (type the
  name, hit enter) and evaluates `= 7*831`, but Spotlight and Raycast
  still own OS-level integrations: contacts, clipboard history,
  workflows, extensions.
- **ripgrep** — you're grepping a single project tree. fsearch's content
  search is for "somewhere in my home directory".

## Configuration

`~/.config/fsearch/config.toml` (or `fsearch --config`) — search roots,
excluded folders, content-search size limit. Cloud drives (iCloud, Dropbox,
Box…) are skipped by default; remove them from `excludes` to index them.
`fsearch --reindex` rebuilds the index after changes.

Themes: add a `[theme]` section with `preset = "catppuccin"` (also
`gruvbox`, `nord`, `tokyonight`, and the higher-contrast `slate`) and an
optional `accent = "#7aa2f7"` override.

`quiet = ["/Library/", "/."]` lists path substrings demoted in ranking;
set `quiet = []` to disable smart filtering. Border style is
`borders = "sharp"` (default), `"rounded"`, or `"none"`.
`selection_bg`, `match_fg` and `section` accept hex overrides.

Command keys are remappable via a `[keys]` section — each command takes one
spec string or a list of them. Text editing keys (typing, backspace, cursor
movement, ctrl-a/e/w/d) are fixed and can't be rebound.

    [keys]
    quit = "ctrl-q"
    move_up = ["up", "ctrl-k"]

Mouse is on by default: click to select, double-click to open, wheel
scrolls (`mouse = false` in config.toml disables it). While enabled, hold
the shift key for native terminal text selection.

Image previews auto-negotiate the best graphics protocol and fall back to
colored cells elsewhere (multiplexers included — a terminal merely claiming
Kitty support isn't trusted, since some ACK the query but never render).
`fsearch --doctor` prints what was detected; `FSEARCH_IMAGES` overrides it
(`kitty`, `iterm2`, `halfblocks`, or `off`). For much sharper cell-art
fallback, build with the chafa renderer:

```sh
brew install chafa pkgconf
cargo install --path . --features chafa
```

## Semantic search

`? query` finds documents by meaning — `? that essay about patience`
turns up `compounding.md` even when no word matches. Embeddings run
fully locally (all-MiniLM-L6-v2 on ONNX Runtime); nothing leaves your
machine. It's an optional build feature:

```sh
brew install onnxruntime
cargo install --path . --features semantic
fsearch --index-semantic   # one-time; re-runs embed only changed files
```

The ~90 MB model downloads on the first index. Markdown, text, HTML,
LaTeX, PDF, DOCX and XLSX files are indexed. Vectors are stored as f16 and
memory-mapped on load, so the semantic store uses roughly half the previous
disk and memory footprint. The first `--index-semantic` run migrates an
existing store without re-embedding; run it again later to add new or changed
documents. Very large documents are sampled across their full text to keep
indexing bounded.
Queries still answer in milliseconds.

## How it works

See [ARCHITECTURE.md](ARCHITECTURE.md) — the short version: a flat,
mtime-sorted path list persisted at `~/.cache/fsearch/index.bin`, snapshot
swaps instead of locks, generation counters instead of cancellation trees,
and ripgrep's engine for on-demand content search.

## License

MIT
