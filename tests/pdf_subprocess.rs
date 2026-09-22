//! Exercise the installed-binary path, not the unit-test helper entry point.
#![cfg(any(target_os = "linux", target_os = "macos"))]

#[test]
fn binary_helper_extracts_and_library_uses_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("valid.pdf");
    std::fs::write(&path, fsearch::pdf::minimal_pdf("bounded binary helper")).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fsearch"))
        .arg("--internal-pdf-extract")
        .stdin(std::fs::File::open(&path).unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr.first(), Some(&0));
    assert!(String::from_utf8_lossy(&output.stderr[1..]).contains("bounded binary helper"));
    let text =
        fsearch::pdf::extract_cached(path.to_str().unwrap(), &dir.path().join("cache")).unwrap();
    assert!(text.contains("bounded binary helper"));
}
