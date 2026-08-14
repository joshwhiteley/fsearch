use fsearch::config::Config;
use fsearch::engine::{Engine, Mode};
use std::time::{Duration, Instant};

fn make_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("docs")).unwrap();
    std::fs::create_dir_all(p.join("node_modules")).unwrap();
    std::fs::write(p.join("docs/meeting-notes.md"), "agenda\nfind the needle here\n").unwrap();
    std::fs::write(p.join("docs/todo.txt"), "buy milk\n").unwrap();
    std::fs::write(p.join("node_modules/junk.js"), "needle\n").unwrap();
    dir
}

fn config_for(root: &std::path::Path) -> Config {
    Config {
        roots: vec![root.to_path_buf()],
        excludes: vec!["node_modules".to_string()],
        max_content_filesize: 1024 * 1024,
    }
}

fn wait_until(engine: &mut Engine, deadline: Duration, pred: impl Fn(&Engine) -> bool) {
    let start = Instant::now();
    while start.elapsed() < deadline {
        engine.tick();
        if pred(engine) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("condition not met within {deadline:?}");
}

#[test]
fn end_to_end_filename_and_content_search() {
    let tree = make_tree();
    let cache_dir = tempfile::tempdir().unwrap();
    let cache = cache_dir.path().join("index.bin");
    let mut engine = Engine::new(config_for(tree.path()), cache.clone());

    // index builds in the background (no cache on first run)
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().indexed == 2
    });

    // fuzzy filename search
    engine.set_query("notes", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().iter().any(|r| r.path.ends_with("meeting-notes.md"))
    });
    assert_eq!(engine.mode(), Mode::Fuzzy);

    // regex filename search
    engine.set_query(r"todo\.txt$", true);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 1 && e.results()[0].path.ends_with("todo.txt")
    });

    // content search (debounced), excluded dirs stay excluded
    engine.set_query("> needle", false);
    wait_until(&mut engine, Duration::from_secs(5), |e| !e.results().is_empty());
    assert_eq!(engine.mode(), Mode::Content);
    let rows = engine.results();
    assert!(rows.iter().all(|r| !r.path.contains("node_modules")));
    let hit = rows.iter().find(|r| r.path.ends_with("meeting-notes.md")).unwrap();
    assert_eq!(hit.line_number, Some(2));
    assert!(hit.line.as_deref().unwrap().contains("needle"));

    // invalid regex → error surfaced, no crash
    engine.set_query("[bad", true);
    wait_until(&mut engine, Duration::from_secs(2), |e| e.status().error.is_some());

    // cache was written; a second engine loads it instantly
    wait_until(&mut engine, Duration::from_secs(5), |_| cache.exists());
    let mut engine2 = Engine::new(config_for(tree.path()), cache);
    wait_until(&mut engine2, Duration::from_secs(2), |e| e.status().indexed == 2);
}

#[test]
fn content_search_typed_char_by_char() {
    let tree = make_tree();
    let cache_dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(config_for(tree.path()), cache_dir.path().join("index.bin"));
    wait_until(&mut engine, Duration::from_secs(5), |e| !e.status().indexing);

    // simulate the TUI: one set_query per keystroke, ticking in between
    let input = "> needle";
    for end in 1..=input.len() {
        engine.set_query(&input[..end], false);
        engine.tick();
        std::thread::sleep(Duration::from_millis(30));
        engine.tick();
    }
    wait_until(&mut engine, Duration::from_secs(5), |e| !e.results().is_empty());
}
