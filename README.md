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
actually open. The interactive app watches for filesystem changes and
refreshes the index in the background. Cached snapshots can be stale;
`--reindex` refreshes paths and `--index-semantic` refreshes document vectors.
Works on macOS and Linux.

## Install

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/joshwhiteley/fsearch/releases/latest/download/fsearch-installer.sh | sh
```

From a source checkout (Rust 1.90 or newer):

```sh
cargo install --path .
```

## Use

Run `fsearch` and start typing.

- plain text — fuzzy search on file names, matches highlighted; matching
  project directories can appear too, and nearby path segments help rank
  files inside a project
- with an existing semantic index, bare fuzzy queries blend filename and
  semantic rankings; `unified = false` disables this best-effort blending
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
- `enter` opens · `→` opens an actions menu (including foreground Neovim
  for source files, reveal, copy, quick look, and trash) · `ctrl-space` Quick
  Look · `ctrl-y` copies the path · `ctrl-s` toggles a mark · `alt-s` clears
  marks · the actions menu batches open/copy/trash over visible marked rows ·
  `ctrl-p`/`ctrl-n` recall query history ·
  `ctrl-g` cycles theme presets · `f1`/`ctrl-o` opens help ·
  `tab` cycles the preview (side → full-window → hidden) · `esc` quits

Piped input flips fsearch into an fzf-style filter: `git ls-files |
fsearch` (or `… | fsearch --filter`) fuzzy-filters the lines and prints
your selection. Lines ending in `/` remain ordinary input records.

Scripting: `fsearch --big` lists the largest files in the index (a quick
"what's eating my disk"), and `fsearch -p QUERY` prints matches to stdout
(exit 1 when none),
and `fsearch --pick` runs the full UI but prints your selection instead of
opening it — so `vim "$(fsearch --pick)"` works. `fsearch --help` lists
everything else.

### Script output

Put options **before** the command. After `-p`, `--pick`, or `--filter`,
all remaining arguments are query text.

```sh
fsearch --json -p '> ext:md TODO'
fsearch --json --big 10
fsearch --json --status
fsearch --print0 -p 'ext:pdf report' | xargs -0 ls -l
git ls-files -z | fsearch --read0 --print0 --filter
```

`--json` emits newline-delimited JSON (NDJSON), not a JSON array. Result
records have a `type` field:

| Type | Fields |
|---|---|
| `filename` | `path` |
| `content` | `path`, `line_number`, `text` |
| `semantic` | `path`, `line_number`, `score` |
| `file` (`--big`) | `path`, `size` (bytes), `mtime` (Unix seconds) |
| `selection` (`--pick` / `--filter`) | `value` |

`--json --status` emits one health object, not a result record.
`--print0` emits NUL-terminated paths or selections. For content and semantic
searches it emits only the path **per hit**, without line or score fields;
repeated content hits can repeat a path. `--json` and `--print0` cannot be
combined. Use them instead of newline-delimited text for paths containing
newlines.

`--read0` accepts NUL-separated stdin records in filter mode. Stdin must be
valid UTF-8, even with NUL delimiters. Input is limited to 64 MiB total and
1 MiB per record; exceeding either limit is an error. At 500,000 records,
input is truncated with a warning. NUL mode does not add arbitrary-byte
filename support.

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
`gruvbox`, `nord`, `tokyonight`, the higher-contrast `slate`, and the
copper-toned `forge`) and an optional `accent = "#7aa2f7"` override.

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
    help = ["f1", "ctrl-o"]
    toggle_mark = "ctrl-b"
    clear_marks = "alt-b"

`ctrl-g` cycles the theme presets live (session-only; config wins next run).
`ctrl-s`/`alt-s` mark and clear files in open mode (marking advances to the
next row, fzf-style); filter and `--pick` modes
keep marking disabled. Batch actions use only visible marked rows.
`icons = true` prefixes result rows with nerd-font glyphs — it needs a nerd
font, so it defaults to off. `remember_session = false` disables restoring
preview layout and row density between runs. `remember_history = false`
separately disables loading and saving open/query history and ranking boosts.
For one run, `fsearch --no-history` disables both history and remembered
layout. It does **not** disable index or extracted-text caches.

Named searches and scopes are configured as strings:

```toml
[searches]
recent_docs = "kind:doc changed:7d"
todos = "> ext:md TODO"
```

Use `fsearch --searches` to list them. `fsearch --saved recent_docs` opens
that query; `fsearch --saved recent_docs -p report` adds `report` to its scope.
`--saved` works with interactive search, print, pick, and filter commands.
It is a CLI feature, not an in-app saved-search menu. Conflicting mode
prefixes in the saved and supplied query are rejected.

### Actions and file transfers

Source files offer **open in nvim**. Neovim runs in the foreground and
fsearch restores the search session when it exits. Content and semantic
hits open at their matched line. With no custom actions, installed Cursor,
VS Code, Zed, and Sublime Text editors also get menu entries for code files.

Add `[[actions]]` tables to replace these detected-editor defaults:

```toml
[[actions]]
name = "open in code at line"
cmd = ["code", "--goto", "{path}:{line}"]
kind = "code"
enter = true
```

Commands are argument arrays, not shell scripts. `{path}` is the selected
path, `{dir}` its parent, and `{line}` the selected hit's line (1 when absent).
A standalone `{paths}` argument expands to all batch paths. Optional `ext`
(an array, such as `["md", "txt"]`) and `kind` filters restrict the action;
`enter = true` makes the first matching action the default opener. Invalid
actions are skipped with a warning. Custom actions do not apply to directory
rows marked with a trailing `/`.

Mark files with `ctrl-s`, then use **move marked to…** or **copy marked to…**
in the actions menu. The destination picker starts a directory-only search;
choose a folder from its results. Transfers run in a worker with progress;
Escape cancels after the
current file. Existing destinations, including dangling symlinks, are never
replaced. Directories are skipped; source symlinks and special files are
rejected.

On macOS and Linux, same-filesystem moves use native no-replace rename and
preserve the file's inode and metadata. Copies and cross-filesystem moves
stage data at the destination before publication. They preserve bytes and
permissions, **not timestamps, ownership, ACLs, or extended attributes**.
A cross-filesystem move removes the source only after publication; a failed
source removal leaves both copies and reports an error. If a destination
filesystem does not support native atomic no-replace publication, the
transfer fails safely and leaves the source untouched. Do not modify or
replace source paths concurrently during a transfer.

ZIP, TAR, TAR.GZ and TGZ previews list archive contents without extracting
files. Listings are bounded; corrupt archives show an error preview.

### Terminal preferences

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
fsearch --index-semantic   # build; run again to refresh documents
```

The ~90 MB model downloads on first use. Markdown, text, HTML, LaTeX, PDF,
DOCX and XLSX files are eligible. Each normal `--index-semantic` run walks
configured roots afresh and checks file metadata before reusing vectors;
new, changed and removed files no longer depend on an old path cache.
Reuse is metadata-based, not a content-hash guarantee. Files over 4 MiB are
skipped; large extracted documents are sampled across their full text.

Vectors are stored as f16 and memory-mapped on load. If the existing store
uses the legacy f32 format, the first `--index-semantic` run **only migrates**
it without loading a model or re-embedding. Run the command again to refresh
documents. The interactive worker reloads a replaced semantic store on later
queries; filesystem watching alone does not rebuild embeddings. Path and
metadata filters apply before the semantic result limit.

Set `ORT_DYLIB_PATH` before starting fsearch for a nonstandard ONNX Runtime
installation. Otherwise, fsearch tries common install locations and the
platform's library loader. Runtime selection does not mutate the process
environment.

## Local data and health

`fsearch --status` reports readable roots, cache validity, sizes, ages and
entry counts, plus enabled build features. It also counts PDF/Office
extracted-cache files and bytes (at most 8,192 entries per cache directory;
partial counts are marked). It does not probe the terminal,
start a watcher, build an index, or verify documents against semantic
vectors. A valid or recent snapshot is **not** a freshness guarantee.
Use `--doctor` for terminal/image diagnostics.

Paths and metadata are cached under `~/.cache/fsearch/`; extracted PDF and
Office text and semantic vectors also stay there. Open/query history and
layout live under `~/.local/state/fsearch/`. `XDG_CACHE_HOME` and
`XDG_STATE_HOME` override these base directories. Configuration uses
`XDG_CONFIG_HOME` (default `~/.config`). New app-managed cache/state directories
use mode 0700 and files use 0600 on Unix. These local files are not encrypted.

```sh
fsearch --clear-cache     # index.bin, semantic.bin, pdftext/, officetext/
fsearch --clear-history   # open/query history and remembered layout
```

Close other fsearch instances **before** cleanup to prevent them from
recreating data. Cleanup keeps downloaded models, configuration and original
documents. `--no-history` prevents history/layout use for one search session;
it is not a zero-cache or anonymous mode.

## How it works

See [ARCHITECTURE.md](ARCHITECTURE.md) — the short version: a flat,
mtime-sorted path list persisted at `~/.cache/fsearch/index.bin`, snapshot
swaps instead of locks, generation counters instead of cancellation trees,
and ripgrep's engine for on-demand content search.

## License

MIT
