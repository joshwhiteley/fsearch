# Contributing

Thanks for looking at fsearch! Issues and PRs are welcome.

## Getting started

```sh
cargo test                                 # full suite
cargo clippy --all-targets -- -D warnings  # lint gate (CI enforces this)
cargo fmt                                  # format before committing
cargo run                                  # run against your real home dir
```

Read [ARCHITECTURE.md](ARCHITECTURE.md) first — it explains the moving
parts in ten minutes and will save you an hour of code reading.

## Ground rules

- Tests come with the change. Every module keeps its unit tests inline;
  end-to-end behavior lives in `tests/`.
- Keep commits small, in the existing style: `feat: …`, `bug: …`,
  `add: …`, `docs: …` — lowercase, no scopes.
- New dependencies need a reason; the default build stays free of
  system-library requirements (that's why chafa is an opt-in feature).
- Performance claims need numbers (`tests/perf_test.rs`, hyperfine).

## Good first areas

Check [ROADMAP.md](ROADMAP.md) and issues labeled `good first issue`.
