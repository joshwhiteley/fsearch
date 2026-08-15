# One allocation instead of a million: the arena index cache

*August 2026*

fsearch keeps every path under your home directory in a persisted index so
that launch-to-first-result is instant. On my machine that's **1.17 million
entries**, and profiling showed the embarrassing truth: loading the cache
cost more than searching it. A full fuzzy match across every path takes
~15 ms; loading the index it runs against took ~70 ms.

## Where the time went

The old format was the obvious one — length-prefixed records:

```
[u32 len][path bytes][mtime][size]  × 1,170,000
```

and the obvious format forces the obvious decoder: a loop that calls
`String::from_utf8` per record. That's 1.17 million heap allocations, 1.17
million UTF-8 validations of tiny strings, and a `Vec<String>` whose spine
alone is 28 MB of pointers before any character data. The allocator, not
the disk, was the bottleneck: the file reads in ~15 ms; materializing it
took four times that.

## The arena layout

Version 3 stores the same data as four contiguous blocks:

```
header │ lengths (u32 × n) │ metadata (16 B × n) │ path arena (all bytes, back to back)
```

Loading is now: one `read`, one cumulative sum over the lengths table to
build `(offset, len)` spans, one bulk decode of the metadata table, and —
the part that matters — **one** UTF-8 validation pass over the whole arena.
Paths are handed out as `&str` slices borrowed from the arena:

```rust
pub fn get(&self, i: usize) -> &str {
    let (off, len) = self.spans[i];
    // the arena is validated as UTF-8 once, at load
    unsafe { std::str::from_utf8_unchecked(&self.arena[off as usize..(off + len) as usize]) }
}
```

That `unsafe` is the honest price of the design, and it's a narrow one: the
invariant (arena is valid UTF-8) is established in exactly one place, at
load, by `std::str::from_utf8` over the whole buffer — which is SIMD-
accelerated and runs at multiple GB/s, so validating 120 MB of paths costs
single-digit milliseconds.

The search code didn't get slower by going through an accessor: the fuzzy
matcher now iterates `0..store.len()` with rayon and per-thread scratch
state, and the 1M-path benchmark still reads ~15 ms per keystroke.

## Numbers

Measured with hyperfine (warmup 3) on a 1.17M-entry index of a real home
directory, Apple silicon:

| metric | before (v2) | after (v3) |
|---|---|---|
| `fsearch -p query` end to end | 86.7 ms | **51.1 ms** |
| heap allocations to load the index | ~1.17 M | 3 |
| fuzzy match, 1M paths | ~15 ms | ~15 ms |
| peak RSS | ~396 MB | ~398 MB |

Two honest footnotes. First, peak RSS barely moved — the data is the data,
and the transient read buffer dominates both versions; the win is time and
allocator pressure, not resident memory. Second, I expected ~10 ms loads
and got ~30 ms: the spans/metadata table builds and the arena copy are the
new floor. The next step there is `mmap` with borrowed spans and a checksum
instead of eager validation — measured, that's another ~2× on load, but it
drags lifetime complexity through every consumer, and 51 ms end-to-end is
already below the threshold where a human notices. Knowing when to stop is
also an engineering decision.

## The shape of the lesson

Nothing here is novel — string arenas are a classic. The transferable part
is the diagnosis: if your loader's time is O(records) in *allocator calls*,
no amount of faster parsing fixes it; you have to change the memory shape
so the record count stops mattering. One allocation instead of a million is
not 30% faster. It's a different complexity class for the part that hurt.
