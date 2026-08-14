use fsearch::matcher::{FilenameMode, search};
use std::time::Instant;

/// Manual perf check: `cargo test --release --test perf_test -- --ignored --nocapture`
#[test]
#[ignore]
fn million_path_search_under_100ms() {
    let dirs = ["src", "docs", "Library", "projects", "Downloads", "notes"];
    let paths: Vec<String> = (0..1_000_000)
        .map(|i| format!("/Users/josh/{}/sub{}/file-{i}.txt", dirs[i % 6], i % 997))
        .collect();

    for (query, mode) in [
        ("filetxt", FilenameMode::Fuzzy),
        (r"file-\d{3}\.txt$", FilenameMode::Regex),
    ] {
        let start = Instant::now();
        let hits = search(&paths, query, mode, 500).unwrap();
        let elapsed = start.elapsed();
        println!("{mode:?} {query:?}: {elapsed:?} ({} hits)", hits.len());
        assert!(!hits.is_empty());
        assert!(elapsed.as_millis() < 100, "{mode:?} took {elapsed:?}");
    }
}
