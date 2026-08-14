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
    assert_eq!(text.trim(), format!("fsearch {}", env!("CARGO_PKG_VERSION")));
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
fn config_without_editor_prints_the_path_and_creates_it() {
    let dir = tempfile::tempdir().unwrap();
    let xdg = dir.path().join("xdg");
    let out = fsearch(
        &["--config"],
        &[
            ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
            ("VISUAL", ""),
            ("EDITOR", ""),
        ],
    );
    assert!(out.status.success());
    let printed = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let expected = xdg.join("fsearch").join("config.toml");
    assert_eq!(printed, expected.to_str().unwrap());
    assert!(expected.exists());
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
    let saved = fsearch::index::load(&cache.join("fsearch").join("index.bin")).unwrap();
    assert_eq!(saved.len(), 2);
}
