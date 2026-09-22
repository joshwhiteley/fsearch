# Contributing

Thanks for looking at fsearch! Issues and PRs are welcome.

## Getting started

Use Rust 1.90 or newer. CI tests current stable and checks the minimum
supported compiler, exactly 1.90.0, with the committed lockfile.

```sh
cargo test --locked
cargo test --locked --features semantic
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
cargo run --locked -- --status             # inspect caches, not live freshness
```

The semantic tests use a deterministic fake embedder where needed; they do
not need a downloaded model. To reproduce the minimum-version gate:

```sh
rustup toolchain install 1.90.0 --profile minimal
cargo +1.90.0 check --locked --all-targets
cargo +1.90.0 check --locked --all-targets --features semantic
```

`cargo run --locked` starts the UI and indexes your real configured roots.
For CLI experiments, set `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` and
`XDG_STATE_HOME` to temporary directories and configure a small test root.
`--no-history` disables history/layout persistence, not caches. Set test
environment variables on child commands rather than mutating a
multithreaded test process's environment.

Run `cargo fmt` before submitting changes. In a shared working tree, format
only files you own with `rustfmt --edition 2024 path/to/file.rs`. Parallel
worktrees must use separate Cargo target directories: sharing one can reuse
stale same-package artifacts from another checkout. On APFS, a copy-on-write
copy of an existing target directory can seed dependencies without sharing
mutable build outputs; force a rebuild of the local crate afterward.

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
- Keep CLI help and README examples in sync. Options precede commands;
  test NDJSON fields, NUL record boundaries, UTF-8 errors and input limits.
- Test watcher recovery with injected events and debounce with controlled
  timestamps. Keep the real Linux watcher integration test too.
- File-transfer tests must cover destination races, partial copies and
  source preservation. Do not assume a second mounted filesystem: test the
  cross-device fallback directly.

## CI and manual checks

CI runs default tests and PTY smoke tests on macOS and Linux, semantic tests
on Linux, a provisioned ONNX Runtime 1.24.4 job with real inference,
all-feature tests/clippy with chafa on macOS, exact-MSRV checks,
and cargo-audit/cargo-deny gates. Advisory/source checks include optional
features (`cargo deny --all-features check advisories sources`).

To run the PTY smoke test locally, install `expect` (on Debian/Ubuntu:
`sudo apt-get install expect`), then run:

```sh
cargo build --locked --bin fsearch
tests/smoke.exp target/debug/fsearch
```

The smoke script isolates configuration, cache and state directories, and
checks interactive search, bracketed-paste setup/cleanup, single-line query
normalization and a `--no-history --pick` session. Linux CI
also exercises live watcher updates through `tests/engine_test.rs`.
Parser/preview mutation tests must remain bounded and must not open or
alter a contributor's documents.

For optional renderers and the native runtime loader:

```sh
brew install chafa pkgconf onnxruntime
FSEARCH_SEM_FAKE=1 cargo test --locked --all-features
FSEARCH_SEM_FAKE=0 cargo test --locked --all-features --lib native_runtime_initializes_without_environment_mutation -- --ignored
FSEARCH_SEM_FAKE=0 cargo test --locked --features semantic --test native_semantic -- --ignored
```

The ignored native-runtime unit test initializes the installed ONNX Runtime
without downloading a model. The native integration test downloads/loads the
model and checks real, finite, normalized 384-dimensional vectors. CI runs
both explicitly; neither is a relevance benchmark. Set `ORT_DYLIB_PATH` before
launch if the runtime is outside the usual locations. Runtime API 24 or newer
is required by the locked fastembed dependency.

PDF resource regression fixtures run only in bounded child processes. The
three ignored `pdf_process` entry points are internal subprocess test helpers,
not standalone tests. Never remove their child-only guards or move resource
exhaustion probes into the parent process.

Run the million-path performance budget separately:

```sh
cargo test --locked --release --test perf_test -- --ignored --nocapture
```

## Focused performance checks

Run these separately, without competing builds or benchmark processes:

```sh
cargo test --locked --release --test matcher_perf_test -- --ignored --nocapture
cargo test --locked --release --test cache_perf_test -- --ignored --nocapture
cargo test --locked --lib tui::tests::result_redraw_benchmark -- --ignored --exact --nocapture
RAYON_NUM_THREADS=4 cargo test --locked --release --test semantic_perf_test -- --ignored --nocapture --test-threads=1
cargo test --locked --lib tui::redraw::tests::idle_minute_renders_once_instead_of_1200_times -- --exact --nocapture
```

The matcher fixture uses one million synthetic paths with broad/sparse regexes
and boost ties. The cache fixture uses 1,025 small cache entries and times
100 warmed reads, excluding parsing/setup. The redraw fixture uses a 100×30
TestBackend and 100 frames. All fixtures use synthetic data; these timings
are diagnostic reports, not machine-dependent pass/fail thresholds.

Representative local before/after timings on Apple M5 Pro, Rust 1.95.0:

| Fixture | Before | After |
|---|---:|---:|
| Broad regex, release (7-run median) | 18.91 ms | 0.47 ms |
| Broad regex with boosts, release (7-run median) | 65.88 ms | 3.06 ms |
| Sparse regex, release (7-run median) | 15.25 ms | 1.60 ms |
| 100 warm PDF-cache reads, release (5-run median) | 170.91 ms | 2.12 ms |
| 1,000-result redraw, debug (mean over 100 frames) | 13.67 ms | 0.95 ms |

The last result demonstrates viewport-bounded formatting, not a guarantee
for real-terminal/image-protocol performance. Cheap global slot bookkeeping
still scales with result count.

The semantic fixture compares owned f16, mmap f16 and legacy f32 stores with
64/384-dimensional synthetic vectors, broad and 1-in-97 filters, and zero,
small and full result limits. Regular tests check exact score bits and ranking
against an independent scalar oracle, including malformed ranges and nonfinite
values. Repeat timings with fixed Rayon thread counts and back-to-back binaries.

On the same machine, paired release runs for 16,384 documents × 4 chunks,
384 dimensions and top-20 broad queries gave:

| Storage | Threads | Before | After |
|---|---:|---:|---:|
| mmap f16 | 4 | 7.35 ms | 4.79 ms |
| legacy f32 | 4 | 7.25 ms | 4.06 ms |
| mmap f16 | 1 | 26.43 ms | 19.08 ms |
| legacy f32 | 1 | 26.29 ms | 14.43 ms |

Each value averages two process medians (11 samples × 3 queries), ordered
old/new/new/old after warm-up. Unrelated host CPU load remained active. Owned
storage and restrictive-filter timings were mixed; these are mapped broad-query
improvements, not a universal speedup or embedding/model benchmark.

The deterministic idle test advances 1,200 polling timestamps over a synthetic
minute with zero or 2,000 unchanged rows: one frame instead of 1,200. This
checks scheduling, not CPU usage. Separate tests cover timer/worker updates;
real relative-age transitions and other visible changes still redraw.

## Good first areas

Check [ROADMAP.md](ROADMAP.md) and issues labeled `good first issue`.
