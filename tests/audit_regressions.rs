use std::process::{Command, Stdio};

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config/fsearch")).unwrap();
        std::fs::create_dir(dir.path().join("root")).unwrap();
        std::fs::write(dir.path().join("config/fsearch/config.toml"), format!(
            "roots = [{:?}]\nexcludes = []\nindex_apps = false\nquiet = []\n[searches]\ndocs = 'ext:md'\ntodos = '> needle'\n", dir.path().join("root").to_str().unwrap())).unwrap();
        Self { dir }
    }
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fsearch"));
        cmd.args(args)
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_CACHE_HOME", self.dir.path().join("cache"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("FSEARCH_SEM_FAKE", "1");
        cmd
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.dir.path().join("root").join(name), text).unwrap();
    }
}

#[test]
fn structured_and_nul_output_preserve_unusual_paths() {
    let f = Fixture::new();
    f.write("note\nquote\".md", "needle\n");
    let out = f.cmd(&["--json", "-p", "note"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(record["type"], "filename");
    assert!(
        record["path"]
            .as_str()
            .unwrap()
            .ends_with("note\nquote\".md")
    );
    let out = f.cmd(&["--print0", "-p", "note"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout.iter().filter(|&&b| b == 0).count(), 1);
    assert_eq!(out.stdout.last(), Some(&0));
    let out = f.cmd(&["--json", "-p", "> needle"]).output().unwrap();
    let record: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(record["line_number"], 1);
    assert_eq!(record["text"], "needle");
}

#[test]
fn saved_queries_compose_with_explicit_modes() {
    let f = Fixture::new();
    f.write("guide.md", "needle");
    f.write("guide.txt", "needle");
    let out = f
        .cmd(&["--saved", "docs", "-p", "> needle"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("guide.md"));
    assert!(!text.contains("guide.txt"));
    let out = f.cmd(&["--searches"]).output().unwrap();
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("docs\text:md")
    );
    assert_eq!(
        f.cmd(&["--saved", "missing", "-p", "guide"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn status_is_headless_and_does_not_build_indexes() {
    let f = Fixture::new();
    let out = f.cmd(&["--json", "--status"]).output().unwrap();
    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["path_index"]["state"], "missing");
    assert_eq!(report["roots"][0]["readable"], true);
    assert!(!f.dir.path().join("cache").exists());
    f.write("a.txt", "a");
    assert!(f.cmd(&["--reindex"]).output().unwrap().status.success());
    let out = f.cmd(&["--json", "--status"]).output().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["path_index"]["entries"], 1);
}

#[test]
fn semantic_refresh_discovers_changes_without_separate_reindex() {
    let f = Fixture::new();
    f.write("old.md", "one original document");
    assert!(
        f.cmd(&["--index-semantic"])
            .output()
            .unwrap()
            .status
            .success()
    );
    f.write("new.md", "brand new document");
    f.write("old.md", "edited existing document with new size");
    let out = f.cmd(&["--index-semantic"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("2 documents"), "{text}");
    assert!(text.contains("2 embedded"), "{text}");
    let path = f.dir.path().join("cache/fsearch/semantic.bin");
    let first = std::fs::read(&path).unwrap();
    f.write("old.md", "edited existing document with OLD size");
    let out = f.cmd(&["--index-semantic"]).output().unwrap();
    assert!(out.status.success());
    assert_ne!(
        std::fs::read(&path).unwrap(),
        first,
        "equal-size edit must not reuse old vectors"
    );
    std::fs::remove_file(f.dir.path().join("root/new.md")).unwrap();
    assert!(
        f.cmd(&["--index-semantic"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fsearch::sem::SemStore::load(&path).unwrap().docs.len(), 1);
}

#[test]
fn unicode_duration_and_editor_arguments_do_not_crash() {
    let f = Fixture::new();
    f.write("guide.md", "x");
    let out = f.cmd(&["-p", "changed:é"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let out = f
        .cmd(&["--config"])
        .env("VISUAL", "/usr/bin/true --")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        f.cmd(&["--config"])
            .env("VISUAL", "'unterminated")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn big_output_survives_a_closed_pipe() {
    let f = Fixture::new();
    f.write("a.txt", "a");
    let mut child = f
        .cmd(&["--big"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}

#[cfg(unix)]
#[test]
fn private_document_cache_and_cleanup_preserve_originals_and_models() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let pdf = f.dir.path().join("root/private.pdf");
    std::fs::write(&pdf, fsearch::pdf::minimal_pdf("private needle")).unwrap();
    std::fs::set_permissions(&pdf, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        f.cmd(&["-p", "> needle"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let root = f.dir.path().join("cache/fsearch");
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for entry in std::fs::read_dir(root.join("pdftext")).unwrap().flatten() {
        assert_eq!(
            entry.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::create_dir(root.join("models")).unwrap();
    assert!(f.cmd(&["--clear-cache"]).output().unwrap().status.success());
    assert!(!root.join("pdftext").exists());
    assert!(root.join("models").exists());
    assert!(pdf.exists());
}

#[cfg(unix)]
#[test]
fn document_and_image_readers_refuse_fifo_replacements() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    for name in ["doc.pdf", "doc.docx", "image.png"] {
        let path = dir.path().join(name);
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: C path points to a unique entry in this test's temp dir.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let path = path.to_str().unwrap();
        match name {
            "doc.pdf" => {
                assert!(fsearch::pdf::extract_cached(path, &dir.path().join("pdftext")).is_err())
            }
            "doc.docx" => assert!(
                fsearch::office::extract_cached(path, &dir.path().join("officetext")).is_err()
            ),
            _ => assert!(fsearch::images::load(path, fsearch::images::MAX_IMAGE_BYTES).is_err()),
        }
    }
}
