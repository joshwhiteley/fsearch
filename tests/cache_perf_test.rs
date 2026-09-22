//! Manual warm extracted-text cache benchmark, using only temporary fixtures.
use std::time::Instant;

#[test]
#[ignore = "manual timing benchmark; run alone in release mode with --nocapture"]
fn warm_pdf_cache_hits_do_not_scale_with_directory_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.pdf");
    let cache = dir.path().join("pdftext");
    std::fs::write(&path, fsearch::pdf::minimal_pdf("cached fixture")).unwrap();
    let path = path.to_str().unwrap();
    let expected = fsearch::pdf::extract_cached(path, &cache).unwrap();
    for i in 0..1024 {
        std::fs::write(cache.join(format!("{i:016x}-0-4.txt")), "text").unwrap();
    }
    // Warm metadata/data before measurement. Keep setup and the single parser
    // invocation out of the timed section.
    for _ in 0..5 {
        assert_eq!(
            fsearch::pdf::extract_cached(path, &cache).unwrap(),
            expected
        );
    }
    let mut timings = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..100 {
            assert_eq!(
                fsearch::pdf::extract_cached(path, &cache).unwrap(),
                expected
            );
        }
        timings.push(start.elapsed());
    }
    timings.sort();
    eprintln!(
        "100 warm PDF cache hits with 1025 entries (median): {:?}",
        timings[2]
    );
}
