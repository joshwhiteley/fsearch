# fsearch

Alfred-style instant file search for the macOS terminal. `fsearch` keeps a
persisted index of every file under your home directory and filters it as you
type — fuzzy or regex over filenames, regex over file contents. Matching a
million paths takes about 15 ms, so results feel instantaneous.

## Install

```sh
cargo install --path .
```

or build a standalone binary at `target/release/fsearch`:

```sh
cargo build --release
```

## Usage

```
fsearch              launch the interactive search ui
fsearch --config     open the config file in $EDITOR (or print its path)
fsearch --reindex    rebuild the file index now
fsearch --help       show help
fsearch --version    print the version
```

Run `fsearch` and start typing:

- **Type plainly** — fuzzy match against file names and paths (like fzf).
- **`Ctrl-R`** — toggle regex mode; the pattern matches the full path.
- **`> pattern`** — content mode: the rest of the query is a regex searched
  inside your files, streaming results as `path:line`.

Both filename and content search are smart-case: all-lowercase queries are
case-insensitive, any uppercase makes them case-sensitive.

Results favor recency: an empty query and regex matches list files newest
first (by modification time), and fuzzy matches use recency to break ranking
ties.

The first launch walks your home directory and builds the index; later
launches load the cached index instantly and refresh it in the background.

## Keys

| Key | Action |
|---|---|
| `↑` / `↓` or `Ctrl-K` / `Ctrl-J` | move selection |
| `Enter` | open with default app |
| `Ctrl-F` | reveal in Finder |
| `Ctrl-Y` | copy path to clipboard |
| `Ctrl-R` | toggle fuzzy / regex mode |
| `Ctrl-U` | clear query |
| `Tab` | toggle preview pane |
| `Esc` / `Ctrl-C` | quit |

## Configuration

`~/.config/fsearch/config.toml` (created with defaults on first run):

```toml
# directories to index (~ expands to your home directory)
roots = ["~"]

# directory or file names/paths never indexed
excludes = [".git", "node_modules", "target", ".bun", ".cache", ".cargo",
            ".npm", ".Trash", ".venv", "__pycache__", "Library/Caches",
            "Library/Containers", "Library/Application Support/MobileSync",
            "Library/Mobile Documents", "Library/Cloud Storage"]

# content search skips files larger than this (bytes)
max_content_filesize = 2097152
```

Hidden files are indexed; `.gitignore` files are deliberately ignored — if
it's on disk, you can find it.

Cloud-synced trees are excluded by default so the walker never touches
placeholder files. To search them, delete `"Library/Mobile Documents"`
(iCloud Drive) or `"Library/Cloud Storage"` (Box, Dropbox, Google Drive,
OneDrive) from `excludes`.

## How it works

The index is a flat list of paths cached at `~/.cache/fsearch/index.bin`;
launch loads it in milliseconds while a background thread re-walks the roots
and swaps in a fresh copy. Filename queries run per keystroke over the
in-memory index (nucleo fuzzy matching or the regex crate, parallelized).
Content queries run ripgrep's engine crates across the indexed files on
demand, skipping binaries and oversized files, streaming hits into the UI.

## License

MIT
