use std::process::Command;

fn fsearch(args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fsearch"));
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

#[test]
fn version_prints_the_crate_version() {
    let out = fsearch(&["--version"], &[]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        text.trim(),
        format!("fsearch {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_prints_usage() {
    let out = fsearch(&["--help"], &[]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("usage"));
    assert!(text.contains("--reindex"));
}

#[test]
fn unknown_flag_fails_with_help() {
    let out = fsearch(&["--nope"], &[]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("--nope"));
    assert!(err.contains("usage"));
}

#[test]
fn config_runs_the_editor_and_creates_the_file() {
    // /usr/bin/true stands in for an editor: accepts the path arg, exits 0.
    // (The no-editor branch reveals the file in Finder — covered by a unit
    // test on cli::choose_config_open so test runs don't open real windows.)
    let dir = tempfile::tempdir().unwrap();
    let xdg = dir.path().join("xdg");
    let out = fsearch(
        &["--config"],
        &[
            ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
            ("VISUAL", ""),
            ("EDITOR", "/usr/bin/true"),
        ],
    );
    assert!(out.status.success());
    assert!(xdg.join("fsearch").join("config.toml").exists());
}

#[test]
fn config_propagates_editor_failure() {
    let dir = tempfile::tempdir().unwrap();
    let xdg = dir.path().join("xdg");
    let out = fsearch(
        &["--config"],
        &[
            ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
            ("VISUAL", "/usr/bin/false"),
            ("EDITOR", ""),
        ],
    );
    assert!(!out.status.success());
}

#[test]
fn reindex_builds_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("a.txt"), "x").unwrap();
    std::fs::write(tree.join("b.txt"), "y").unwrap();
    let xdg = dir.path().join("xdg");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(xdg.join("fsearch")).unwrap();
    std::fs::write(
        xdg.join("fsearch").join("config.toml"),
        format!("roots = [{:?}]\n", tree.to_str().unwrap()),
    )
    .unwrap();

    let out = fsearch(
        &["--reindex"],
        &[
            ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache.to_str().unwrap()),
        ],
    );
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("indexed 2 files"), "got: {text}");
    let (saved, _metas) = fsearch::index::load(&cache.join("fsearch").join("index.bin")).unwrap();
    assert_eq!(saved.len(), 2);
}

#[test]
fn print_mode_lists_matches_and_content_hits() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("meeting-notes.md"), "find the needle\n").unwrap();
    std::fs::write(tree.join("todo.txt"), "milk\n").unwrap();
    let xdg = dir.path().join("xdg");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(xdg.join("fsearch")).unwrap();
    std::fs::write(
        xdg.join("fsearch").join("config.toml"),
        format!("roots = [{:?}]\n", tree.to_str().unwrap()),
    )
    .unwrap();
    let env: &[(&str, &str)] = &[
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("XDG_CACHE_HOME", cache.to_str().unwrap()),
    ];

    // fuzzy filename match
    let out = fsearch(&["-p", "notes"], env);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("meeting-notes.md"));
    assert!(!text.contains("todo.txt"));

    // content match prints path:line:text
    let out = fsearch(&["-p", "> needle"], env);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("meeting-notes.md:1:find the needle"),
        "got: {text}"
    );

    // no match exits 1
    let out = fsearch(&["-p", "zzzznope"], env);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn print_mode_honors_filters() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("guide.md"), "x\n").unwrap();
    std::fs::write(tree.join("guide.txt"), "x\n").unwrap();
    let xdg = dir.path().join("xdg");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(xdg.join("fsearch")).unwrap();
    std::fs::write(
        xdg.join("fsearch").join("config.toml"),
        format!("roots = [{:?}]\n", tree.to_str().unwrap()),
    )
    .unwrap();
    let env: &[(&str, &str)] = &[
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("XDG_CACHE_HOME", cache.to_str().unwrap()),
    ];
    let out = fsearch(&["-p", "ext:md", "guide"], env);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("guide.md"));
    assert!(!text.contains("guide.txt"));
}

#[test]
fn big_lists_largest_files_first() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("small.txt"), "x").unwrap();
    std::fs::write(tree.join("huge.bin"), vec![0u8; 5000]).unwrap();
    std::fs::write(tree.join("mid.txt"), vec![0u8; 500]).unwrap();
    let xdg = dir.path().join("xdg");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(xdg.join("fsearch")).unwrap();
    std::fs::write(
        xdg.join("fsearch").join("config.toml"),
        format!("roots = [{:?}]\n", tree.to_str().unwrap()),
    )
    .unwrap();
    let env: &[(&str, &str)] = &[
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("XDG_CACHE_HOME", cache.to_str().unwrap()),
    ];
    let out = fsearch(&["--big", "2"], env);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("huge.bin") && lines[0].contains("5.0 KB"));
    assert!(lines[1].contains("mid.txt"));
}
