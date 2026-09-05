use fsearch::config::Config;
use fsearch::engine::{Engine, Mode};
use std::time::{Duration, Instant};

fn make_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("docs")).unwrap();
    std::fs::create_dir_all(p.join("node_modules")).unwrap();
    std::fs::write(
        p.join("docs/meeting-notes.md"),
        "agenda\nfind the needle here\n",
    )
    .unwrap();
    std::fs::write(p.join("docs/todo.txt"), "buy milk\n").unwrap();
    std::fs::write(p.join("node_modules/junk.js"), "needle\n").unwrap();
    dir
}

fn config_for(root: &std::path::Path) -> Config {
    Config {
        roots: vec![root.to_path_buf()],
        excludes: vec!["node_modules".to_string()],
        max_content_filesize: 1024 * 1024,
        theme: Default::default(),
        keys: Default::default(),
        mouse: true,
        remember_session: true,
        remember_history: true,
        searches: Default::default(),
        index_apps: false,
        icons: false,
        unified: true,
        quiet: Vec::new(),
        actions: Vec::new(),
        action_warning: None,
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
    let mut engine = Engine::new(
        config_for(tree.path()),
        cache.clone(),
        cache_dir.path().join("history"),
    );

    // index builds in the background (no cache on first run);
    // 2 files + the docs/ directory entry
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().indexed == 3
    });

    // fuzzy filename search
    engine.set_query("notes", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results()
            .iter()
            .any(|r| r.path.ends_with("meeting-notes.md"))
    });
    assert_eq!(engine.mode(), Mode::Fuzzy);

    // regex filename search
    engine.set_query(r"todo\.txt$", true);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 1 && e.results()[0].path.ends_with("todo.txt")
    });

    // content search (debounced), excluded dirs stay excluded
    engine.set_query("> needle", false);
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.results().is_empty()
    });
    assert_eq!(engine.mode(), Mode::Content);
    let rows = engine.results();
    assert!(rows.iter().all(|r| !r.path.contains("node_modules")));
    let hit = rows
        .iter()
        .find(|r| r.path.ends_with("meeting-notes.md"))
        .unwrap();
    assert_eq!(hit.line_number, Some(2));
    assert!(hit.line.as_deref().unwrap().contains("needle"));

    // invalid regex → error surfaced, no crash
    engine.set_query("[bad", true);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.status().error.is_some()
    });

    // cache was written; a second engine loads it instantly
    wait_until(&mut engine, Duration::from_secs(5), |_| cache.exists());
    let mut engine2 = Engine::new(config_for(tree.path()), cache, cache_dir.path().join("h2"));
    wait_until(&mut engine2, Duration::from_secs(2), |e| {
        e.status().indexed == 3
    });
}

#[test]
fn content_search_typed_char_by_char() {
    let tree = make_tree();
    let cache_dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(
        config_for(tree.path()),
        cache_dir.path().join("index.bin"),
        cache_dir.path().join("history"),
    );
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing
    });

    // simulate the TUI: one set_query per keystroke, ticking in between
    let input = "> needle";
    for end in 1..=input.len() {
        engine.set_query(&input[..end], false);
        engine.tick();
        std::thread::sleep(Duration::from_millis(30));
        engine.tick();
    }
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.results().is_empty()
    });
}

#[test]
fn results_sorted_by_last_modified_desc() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let hour = Duration::from_secs(3600);
    let now = std::time::SystemTime::now();
    for (name, age) in [("old.txt", 3), ("newest.txt", 0), ("mid.txt", 1)] {
        let path = p.join(name);
        std::fs::write(&path, "x\n").unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(now - age * hour).unwrap();
    }
    let cache_dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(
        config_for(p),
        cache_dir.path().join("index.bin"),
        cache_dir.path().join("history"),
    );
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().indexed == 3
    });

    // empty query: newest first
    engine.set_query("", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 3
    });
    let order: Vec<&str> = engine
        .results()
        .iter()
        .map(|r| r.path.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(order, ["newest.txt", "mid.txt", "old.txt"]);

    // regex matches too
    engine.set_query(r"\.txt$", true);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 3 && e.results()[0].path.ends_with("newest.txt")
    });
    assert!(engine.results()[2].path.ends_with("old.txt"));
}

#[test]
fn opened_files_rank_first() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let hour = Duration::from_secs(3600);
    let now = std::time::SystemTime::now();
    for (name, age) in [("stale-open.txt", 5), ("fresh.txt", 0)] {
        let path = p.join(name);
        std::fs::write(&path, "x\n").unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(now - age * hour).unwrap();
    }
    let aux = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(
        config_for(p),
        aux.path().join("index.bin"),
        aux.path().join("history"),
    );
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().indexed == 2
    });

    // mtime order first: fresh.txt on top
    engine.set_query("", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 2 && e.results()[0].path.ends_with("fresh.txt")
    });

    // open the stale file → it should outrank the fresher one
    let stale = p.join("stale-open.txt").to_string_lossy().into_owned();
    engine.record_open(&stale);
    engine.set_query("", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 2 && e.results()[0].path.ends_with("stale-open.txt")
    });
}

#[test]
fn live_index_picks_up_created_and_deleted_files() {
    let tree = tempfile::tempdir().unwrap();
    // canonicalize: fs events report resolved paths (/private/var vs /var)
    let root = tree.path().canonicalize().unwrap();
    std::fs::write(root.join("first.txt"), "x\n").unwrap();
    let aux = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(
        config_for(&root),
        aux.path().join("index.bin"),
        aux.path().join("history"),
    );
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().indexed == 1
    });
    engine.set_query("", false);

    // a newly created file shows up without any reindex
    std::fs::write(root.join("brand-new.txt"), "y\n").unwrap();
    wait_until(&mut engine, Duration::from_secs(10), |e| {
        e.results()
            .iter()
            .any(|r| r.path.ends_with("brand-new.txt"))
    });

    // and a deleted file disappears
    std::fs::remove_file(root.join("first.txt")).unwrap();
    wait_until(&mut engine, Duration::from_secs(30), |e| {
        !e.results().iter().any(|r| r.path.ends_with("first.txt"))
    });
}

#[test]
fn filters_scope_filename_and_content_search() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("docs")).unwrap();
    std::fs::create_dir_all(p.join("pics")).unwrap();
    std::fs::write(p.join("docs/guide.md"), "the needle\n").unwrap();
    std::fs::write(p.join("docs/guide.txt"), "the needle\n").unwrap();
    let aux = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(
        config_for(p),
        aux.path().join("index.bin"),
        aux.path().join("history"),
    );
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().indexed == 4
    });

    // ext: narrows fuzzy results
    engine.set_query("ext:md guide", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 1 && e.results()[0].path.ends_with("guide.md")
    });

    // dir: lists directories only
    engine.set_query("dir:", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 2 && e.results().iter().all(|r| r.path.ends_with('/'))
    });

    // filters scope content search too
    engine.set_query("> ext:md needle", false);
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.results().is_empty()
    });
    assert!(
        engine
            .results()
            .iter()
            .all(|r| r.path.ends_with("guide.md"))
    );
}

#[test]
fn changed_and_size_filters_narrow_results() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let now = std::time::SystemTime::now();
    // fresh + big, fresh + small, old + big
    std::fs::write(p.join("fresh-big.bin"), vec![0u8; 4000]).unwrap();
    std::fs::write(p.join("fresh-small.txt"), "x").unwrap();
    std::fs::write(p.join("old-big.bin"), vec![0u8; 4000]).unwrap();
    let f = std::fs::File::options()
        .write(true)
        .open(p.join("old-big.bin"))
        .unwrap();
    f.set_modified(now - std::time::Duration::from_secs(10 * 24 * 3600))
        .unwrap();

    let aux = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(
        config_for(p),
        aux.path().join("index.bin"),
        aux.path().join("history"),
    );
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().indexed == 3
    });

    engine.set_query("changed:1d", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 2 && e.results().iter().all(|r| r.path.contains("fresh"))
    });

    engine.set_query("larger:1kb", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 2 && e.results().iter().all(|r| r.path.contains("big"))
    });

    engine.set_query("changed:1d larger:1kb", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 1 && e.results()[0].path.ends_with("fresh-big.bin")
    });
}

#[test]
fn invalid_excludes_keep_cached_index_and_report_error() {
    let tree = make_tree();
    let cache_dir = tempfile::tempdir().unwrap();
    let cache = cache_dir.path().join("index.bin");
    // seed the cache the way a previous session would have
    let entries = vec![(
        format!("{}/docs/meeting-notes.md", tree.path().display()),
        fsearch::walker::FileMeta::default(),
    )];
    fsearch::index::save(&entries, &cache).unwrap();

    let mut config = config_for(tree.path());
    config.excludes = vec!["node_modules[".to_string()]; // invalid glob
    let mut engine = Engine::new(config, cache, cache_dir.path().join("history"));
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().error.is_some()
    });
    assert!(
        engine
            .status()
            .error
            .unwrap()
            .contains("invalid exclude pattern")
    );
    // the cached snapshot stays searchable instead of being wiped
    assert_eq!(engine.status().indexed, 1);
    engine.set_query("notes", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results()
            .iter()
            .any(|r| r.path.ends_with("meeting-notes.md"))
    });
}

#[test]
fn unwatchable_root_reports_live_update_failure() {
    let aux = tempfile::tempdir().unwrap();
    let config = config_for(std::path::Path::new("/nonexistent-fsearch-root"));
    let mut engine = Engine::new(
        config,
        aux.path().join("index.bin"),
        aux.path().join("history"),
    );
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.status().error.is_some()
    });
    assert!(
        engine
            .status()
            .error
            .unwrap()
            .contains("live updates unavailable")
    );
}

#[test]
fn filter_lines_ending_in_slashes_survive_empty_regex_and_fuzzy_queries() {
    let mut engine = Engine::from_lines(vec!["alpha/nested/".into(), "beta/".into()]);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 2
    });
    engine.set_query("alpha", true);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 1 && e.results()[0].path == "alpha/nested/"
    });
    engine.set_query("beta", false);
    wait_until(&mut engine, Duration::from_secs(2), |e| {
        e.results().len() == 1 && e.results()[0].path == "beta/"
    });
}

#[test]
fn disabled_history_does_not_load_boosts_or_write_opens() {
    let tree = tempfile::tempdir().unwrap();
    std::fs::write(tree.path().join("file.txt"), "x").unwrap();
    let aux = tempfile::tempdir().unwrap();
    let history = aux.path().join("history");
    let path = tree.path().join("file.txt").to_string_lossy().into_owned();
    let mut frecency = fsearch::frecency::Frecency::load(history.clone());
    frecency.record(&path);
    drop(frecency);
    let before = std::fs::read(&history).unwrap();
    let mut config = config_for(tree.path());
    config.remember_history = false;
    let mut engine = Engine::new(config, aux.path().join("index.bin"), history.clone());
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing && e.results().len() == 1
    });
    assert!(!engine.results()[0].recent_open);
    engine.record_open(&path);
    assert_eq!(std::fs::read(history).unwrap(), before);
}

const RESTRICTIVE_QUERY: &str = "? ext:md path:wanted changed:1d larger:1kb needle";

#[test]
fn semantic_restrictive_filters_reach_beyond_400_docs_in_engine_and_headless() {
    use fsearch::sem::{Embedder, HashEmbedder, SemStore};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tree");
    let cache = dir.path().join("cache");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(config.join("fsearch")).unwrap();
    std::fs::write(
        config.join("fsearch/config.toml"),
        format!(
            "roots = [{}]\nindex_apps = false\n",
            toml::Value::String(root.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let mut embedder = HashEmbedder { dim: 64 };
    let vector = embedder.embed(&["needle".to_string()]).unwrap().remove(0);
    let lower = vector.iter().map(|v| v * 0.5).collect();
    let now = fsearch::util::unix_now();
    let mut store = SemStore::new(64);
    for i in 0..501 {
        store.push_doc(
            &format!("/noise/{i}.txt"),
            now,
            5_000,
            &[(1, vector.clone())],
        );
    }
    store.push_doc("/wanted/old.md", 0, 5_000, &[(1, vector.clone())]);
    store.push_doc("/wanted/small.md", now, 1, &[(1, vector)]);
    store.push_doc("/wanted/answer.md", now, 5_000, &[(7, lower)]);
    store.save(&cache.join("fsearch/semantic.bin")).unwrap();

    // Configure only child processes: no unsafe process-global environment mutation.
    let configure = |cmd: &mut std::process::Command| {
        cmd.env("XDG_CACHE_HOME", &cache)
            .env("XDG_CONFIG_HOME", &config)
            .env("FSEARCH_SEM_FAKE", "1");
    };
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    configure(&mut child);
    let output = child
        .args([
            "--exact",
            "semantic_restrictive_filter_child",
            "--nocapture",
        ])
        .env("FSEARCH_RESTRICTIVE_TEST_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut cli = std::process::Command::new(env!("CARGO_BIN_EXE_fsearch"));
    configure(&mut cli);
    let output = cli.args(["--print", RESTRICTIVE_QUERY]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(stdout.contains("/wanted/answer.md:7:"), "{stdout}");
}

#[test]
fn semantic_restrictive_filter_child() {
    let Some(root) = std::env::var_os("FSEARCH_RESTRICTIVE_TEST_ROOT") else {
        return;
    };
    let aux = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(
        config_for(std::path::Path::new(&root)),
        aux.path().join("index.bin"),
        aux.path().join("history"),
    );
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.status().indexing
    });
    engine.set_query(RESTRICTIVE_QUERY, false);
    wait_until(&mut engine, Duration::from_secs(5), |e| {
        !e.results().is_empty()
    });
    assert_eq!(engine.results().len(), 1);
    assert_eq!(engine.results()[0].path, "/wanted/answer.md");
    assert_eq!(engine.results()[0].line_number, Some(7));
}
