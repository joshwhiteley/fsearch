#[test]
fn ds_store_files_are_excluded_by_the_walker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".DS_Store"), "x").unwrap();
    std::fs::write(dir.path().join("keep.txt"), "x").unwrap();
    let set = fsearch::walker::build_exclude_set(&[".DS_Store".to_string()]).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    fsearch::walker::walk(&[dir.path().to_path_buf()], &set, false, &tx);
    drop(tx);
    let paths: Vec<String> = rx.into_iter().map(|(p, _)| p).collect();
    assert!(paths.iter().any(|p| p.ends_with("keep.txt")));
    assert!(
        !paths.iter().any(|p| p.contains("DS_Store")),
        "got: {paths:?}"
    );
}
