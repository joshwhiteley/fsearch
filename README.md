# fsearch

Fast file search for your Mac, in the terminal. Type to fuzzy-find any file
under your home folder, hit enter to open it. Inspired by Alfred.

## Install

```sh
cargo install --path .
```

## Use

Run `fsearch` and start typing.

- plain text — fuzzy search on file names
- `ctrl-r` — regex on the full path
- `> pattern` — regex search inside files
- `enter` opens · `ctrl-f` reveals in Finder · `ctrl-y` copies the path ·
  `tab` toggles preview · `esc` quits

The first run indexes your home folder; after that launches are instant and
the index refreshes in the background. Results are sorted by last modified.

## Config

`~/.config/fsearch/config.toml` (or `fsearch --config`) — search roots,
excluded folders, content-search size limit. Cloud drives (iCloud, Dropbox,
Box…) are skipped by default; remove them from `excludes` to index them.
`fsearch --reindex` rebuilds the index after changes.

## License

MIT
