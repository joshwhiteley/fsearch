use std::process::Command;

fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let config = temp.path().join("config");
    let cache = temp.path().join("cache");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("known.txt"), "known content").unwrap();
    std::fs::create_dir_all(config.join("fsearch")).unwrap();
    std::fs::create_dir_all(cache.join("fsearch")).unwrap();
    std::fs::write(
        config.join("fsearch/config.toml"),
        format!(
            "roots = [{:?}, {:?}]\nindex_apps = false\n",
            root.to_str().unwrap(),
            temp.path().join("missing-root").to_str().unwrap()
        ),
    )
    .unwrap();
    (temp, root, config, cache)
}

#[test]
fn explicit_rebuilds_preserve_existing_indexes_after_incomplete_walks() {
    let (_temp, root, config, cache) = fixture();
    let index = cache.join("fsearch/index.bin");
    fsearch::index::save(
        &[(
            root.join("known.txt").to_string_lossy().into_owned(),
            fsearch::walker::FileMeta::default(),
        )],
        &index,
    )
    .unwrap();
    let original = std::fs::read(&index).unwrap();
    let semantic = cache.join("fsearch/semantic.bin");
    std::fs::write(&semantic, b"do not replace").unwrap();
    for arg in ["--reindex", "--index-semantic"] {
        let output = Command::new(env!("CARGO_BIN_EXE_fsearch"))
            .arg(arg)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_CACHE_HOME", &cache)
            .env("FSEARCH_SEM_FAKE", "1")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("walk incomplete"));
        assert_eq!(std::fs::read(&index).unwrap(), original);
        assert_eq!(std::fs::read(&semantic).unwrap(), b"do not replace");
    }
}

#[test]
fn cold_headless_search_warns_and_uses_partial_results_without_caching() {
    let (_temp, _root, config, cache) = fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_fsearch"))
        .args(["-p", "known"])
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("known.txt"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("walk incomplete"));
    assert!(!cache.join("fsearch/index.bin").exists());
}
