//! Synthetic-only matcher benchmarks; run individually without concurrent builds.
use fsearch::filters::Filters;
use fsearch::index::PathStore;
use fsearch::matcher::{FilenameMode, search_boosted};
use fsearch::quiet::Quiet;
use std::collections::HashMap;
use std::time::Instant;

#[test]
fn bounded_regex_matches_independent_full_sort() {
    // Cross multiple scan chunks, including equal scores, zero boosts,
    // Unicode, directory and metadata filters, and a boosted oldest entry.
    let entries: Vec<_> = (0..35_001)
        .map(|i| {
            let path = match i % 4 {
                0 => format!("/synthetic/docs/report-{i}.txt"),
                1 => format!("/synthetic/Library/REPORT-{i}.md"),
                2 => format!("/synthetic/docs/Résumé-{i}.txt"),
                _ => format!("/synthetic/docs/report-{i}/"),
            };
            (
                path,
                fsearch::walker::FileMeta {
                    mtime: i as i64,
                    size: i as u64,
                },
            )
        })
        .collect();
    let store = PathStore::from_entries(&entries);
    let mut boosts: HashMap<_, _> = (0..store.len())
        .step_by(101)
        .map(|i| (store.get(i).to_owned(), (i % 3) as u32))
        .collect();
    boosts.insert(store.get(store.len() - 1).to_owned(), u32::MAX);
    let none = HashMap::new();
    for filter_query in [
        "",
        "ext:txt path:DOCS",
        "dir:",
        "larger:10000b smaller:30000b",
    ] {
        let (filters, _) = fsearch::filters::parse(filter_query, 0);
        for query in ["report", "REPORT", "résumé", "no-such-match"] {
            let re = regex::RegexBuilder::new(query)
                .case_insensitive(!query.chars().any(char::is_uppercase))
                .build()
                .unwrap();
            for boosts in [&none, &boosts] {
                // Independent pre-optimization oracle: retain every matching
                // index in recency order, then stably sort by descending boost.
                let mut expected: Vec<_> = (0..store.len())
                    .filter(|&i| {
                        filters.matches(store.get(i))
                            && filters.matches_meta(&store.meta(i))
                            && re.is_match(store.get(i))
                    })
                    .collect();
                expected.sort_by_key(|&i| {
                    std::cmp::Reverse(boosts.get(store.get(i)).copied().unwrap_or(0))
                });
                for limit in [0, 1, 17, 500, usize::MAX] {
                    let actual = search_boosted(
                        &store,
                        query,
                        FilenameMode::Regex,
                        limit,
                        boosts,
                        &filters,
                        &Quiet::default(),
                    )
                    .unwrap();
                    assert_eq!(
                        actual.indices,
                        expected[..limit.min(expected.len())],
                        "query={query}, filter={filter_query}, limit={limit}"
                    );
                    assert_eq!(actual.strong, actual.len());
                }
            }
        }
    }
}

#[test]
fn regex_zero_limit_still_validates_and_stale_generations_cancel() {
    use fsearch::matcher::search_boosted_generation;
    use std::sync::atomic::AtomicU64;
    let store = PathStore::from_entries(&[("/synthetic/report.txt".into(), Default::default())]);
    for store in [&store, &PathStore::empty()] {
        let run = |query, limit, generation| {
            search_boosted_generation(
                store,
                query,
                FilenameMode::Regex,
                limit,
                &HashMap::new(),
                &Filters::default(),
                &Quiet::default(),
                generation,
                &AtomicU64::new(1),
            )
        };
        assert!(run("[invalid", 0, 1).is_err());
        assert_eq!(run("report", 0, 1).unwrap().unwrap().len(), 0);
        for limit in [0, 500] {
            assert!(run("report", limit, 2).unwrap().is_none());
            assert!(run("[invalid", limit, 2).unwrap().is_none());
        }
    }
}

/// cargo test --locked --release --test matcher_perf_test -- --ignored --nocapture
#[test]
#[ignore]
fn million_path_ranking_timings() {
    let entries: Vec<_> = (0..1_000_000)
        .map(|i| {
            let dir = if i % 3 == 0 { "Library" } else { "docs" };
            (
                format!("/synthetic/{dir}/sub{}/file-{i}.txt", i % 997),
                Default::default(),
            )
        })
        .collect();
    let store = PathStore::from_entries(&entries);
    let boosts: HashMap<_, _> = (0..store.len())
        .step_by(997)
        .map(|i| (store.get(i).to_owned(), (i % 100 + 1) as u32))
        .collect();
    let none = HashMap::new();
    let quiet = Quiet::default();
    let filters = Filters::default();
    for (label, query, mode, boosts) in [
        ("regex broad", r"file-\d+\.txt$", FilenameMode::Regex, &none),
        (
            "regex broad boosted",
            r"file-\d+\.txt$",
            FilenameMode::Regex,
            &boosts,
        ),
        (
            "regex sparse",
            r"file-\d{3}\.txt$",
            FilenameMode::Regex,
            &none,
        ),
        (
            "regex sparse boosted",
            r"file-\d{3}\.txt$",
            FilenameMode::Regex,
            &boosts,
        ),
        ("fuzzy quiet", "filetxt", FilenameMode::Fuzzy, &none),
    ] {
        let run = || search_boosted(&store, query, mode, 500, boosts, &filters, &quiet).unwrap();
        assert_eq!(run().len(), 500);
        let mut times = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            std::hint::black_box(run());
            times.push(start.elapsed());
        }
        times.sort();
        println!(
            "{label}: median {:?}, range {:?}..{:?}",
            times[3], times[0], times[6]
        );
    }
}
