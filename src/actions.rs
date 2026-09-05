use std::io::Write;
use std::process::{Command, Stdio};

use crate::config::CustomAction;

#[cfg(target_os = "macos")]
pub fn open_args(path: &str) -> (&'static str, Vec<String>) {
    ("open", vec![path.to_string()])
}

#[cfg(not(target_os = "macos"))]
pub fn open_args(path: &str) -> (&'static str, Vec<String>) {
    ("xdg-open", vec![path.to_string()])
}

#[cfg(target_os = "macos")]
pub fn reveal_args(path: &str) -> (&'static str, Vec<String>) {
    ("open", vec!["-R".to_string(), path.to_string()])
}

/// Linux has no standard "reveal in file manager"; open the parent dir.
#[cfg(not(target_os = "macos"))]
pub fn reveal_args(path: &str) -> (&'static str, Vec<String>) {
    let parent = std::path::Path::new(path)
        .parent()
        .map_or_else(|| path.to_string(), |p| p.to_string_lossy().into_owned());
    ("xdg-open", vec![parent])
}

fn run(args: (&'static str, Vec<String>)) -> std::io::Result<()> {
    Command::new(args.0)
        .args(&args.1)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Turns a result path into an absolute path without requiring it to exist.
/// The index normally already stores absolute paths, but filter-like callers
/// and tests may provide relative paths.
pub fn absolute_path(path: &str) -> String {
    let path = std::path::Path::new(path);
    if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(path).to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned())
    }
}

/// Expands custom-action placeholders into argv without shell parsing.
/// `{paths}` is special: its argv element is repeated once per path.
pub fn expand_args(cmd: &[String], path: &str, paths: &[String]) -> Vec<String> {
    expand_args_with_line(cmd, path, paths, None)
}

/// Like `expand_args`, with an optional matched line (`{line}` defaults to 1).
/// Scan only the template: placeholder-like text in filenames stays literal.
pub fn expand_args_with_line(
    cmd: &[String],
    path: &str,
    paths: &[String],
    line: Option<u64>,
) -> Vec<String> {
    let path = absolute_path(path);
    let paths: Vec<String> = paths.iter().map(|path| absolute_path(path)).collect();
    let dir = std::path::Path::new(&path)
        .parent()
        .map_or_else(
            || std::path::PathBuf::from("."),
            std::path::Path::to_path_buf,
        )
        .to_string_lossy()
        .into_owned();
    let line = line.unwrap_or(1).max(1).to_string();
    let expand = |template: &str, path: &str| {
        let mut expanded = String::new();
        let mut rest = template;
        while !rest.is_empty() {
            let replacement = [
                ("{paths}", path),
                ("{path}", path),
                ("{dir}", dir.as_str()),
                ("{line}", line.as_str()),
            ]
            .into_iter()
            .find(|(token, _)| rest.starts_with(token));
            if let Some((token, value)) = replacement {
                expanded.push_str(value);
                rest = &rest[token.len()..];
            } else {
                let ch = rest.chars().next().unwrap();
                expanded.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
        expanded
    };
    let mut args = Vec::new();
    for arg in cmd {
        if arg.contains("{paths}") {
            args.extend(paths.iter().map(|path| expand(arg, path)));
        } else {
            args.push(expand(arg, &path));
        }
    }
    args
}

/// Launches a configured action detached from the TUI, with no shell and no
/// inherited standard streams.
pub fn run_custom(action: &CustomAction, selected: &str, paths: &[String]) -> std::io::Result<()> {
    run_custom_with_line(action, selected, paths, None)
}

/// Additive line-aware variant; batch callers choose the relevant row's line.
pub fn run_custom_with_line(
    action: &CustomAction,
    selected: &str,
    paths: &[String],
    line: Option<u64>,
) -> std::io::Result<()> {
    let args = expand_args_with_line(&action.cmd, selected, paths, line);
    let Some((program, args)) = args.split_first() else {
        return Err(std::io::Error::other("custom action has no command"));
    };
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map(|_| ())
}

pub fn has_paths_placeholder(cmd: &[String]) -> bool {
    cmd.iter().any(|arg| arg.contains("{paths}"))
}

/// Runs a destructive action synchronously so a non-zero exit is reported to
/// the caller instead of being presented as a successful trash operation.
fn run_checked(args: (&'static str, Vec<String>)) -> std::io::Result<()> {
    let status = Command::new(args.0)
        .args(&args.1)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{} exited with {status}",
            args.0
        )))
    }
}

pub fn open(path: &str) -> std::io::Result<()> {
    run(open_args(path))
}

pub fn reveal(path: &str) -> std::io::Result<()> {
    run(reveal_args(path))
}

#[cfg(target_os = "macos")]
pub fn quick_look_args(path: &str) -> (&'static str, Vec<String>) {
    ("qlmanage", vec!["-p".to_string(), path.to_string()])
}

#[cfg(not(target_os = "macos"))]
pub fn quick_look_args(path: &str) -> (&'static str, Vec<String>) {
    // no Quick Look on Linux; opening is the closest equivalent
    open_args(path)
}

/// Opens the system Quick Look panel (macOS) for the file.
pub fn quick_look(path: &str) -> std::io::Result<()> {
    run(quick_look_args(path))?;
    // qlmanage's panel opens BEHIND the focused terminal, which makes it
    // look like nothing happened. Best-effort raise: needs Accessibility
    // permission for the terminal; fails silently without it.
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("osascript")
            .args([
                "-e",
                "delay 0.3",
                "-e",
                "tell application \"System Events\" to set frontmost of \
                 first application process whose name is \"qlmanage\" to true",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn trash_args(path: &str) -> (&'static str, Vec<String>) {
    // Finder's trash: recoverable, no special entitlements needed
    let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
    (
        "osascript",
        vec![
            "-e".to_string(),
            format!("tell application \"Finder\" to delete POSIX file \"{escaped}\""),
        ],
    )
}

#[cfg(not(target_os = "macos"))]
pub fn trash_args(path: &str) -> (&'static str, Vec<String>) {
    ("gio", vec!["trash".to_string(), path.to_string()])
}

/// Moves the file to the system trash (recoverable).
pub fn trash(path: &str) -> std::io::Result<()> {
    run_checked(trash_args(path))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Move,
    Copy,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TransferOutcome {
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Files not started because cancellation was requested between files.
    pub cancelled: usize,
    pub skipped_exists: usize,
    pub skipped_directories: usize,
    pub succeeded_paths: Vec<String>,
    pub first_error: Option<(String, String)>,
}

fn source_name(path: &str) -> Option<&str> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
}

fn destination_exists(path: &std::path::Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// A destination-local, exclusively-created staging file. Publication uses
/// atomic no-replace rename, which also protects dangling destination symlinks.
/// Dropping this guard removes partial copies; publication disarms cleanup
/// because the temporary name no longer belongs to us after rename.
struct StagedFile(Option<std::path::PathBuf>);

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn stage_copy(
    source: &mut std::fs::File,
    target: &std::path::Path,
    copy: impl FnOnce(&mut std::fs::File, &mut std::fs::File) -> std::io::Result<u64>,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = target
        .parent()
        .ok_or_else(|| std::io::Error::other("no destination directory"))?;
    let mut attempts = 0;
    let (mut stage, mut file) = loop {
        attempts += 1;
        if attempts > 128 {
            return Err(std::io::Error::other(
                "cannot reserve transfer staging file",
            ));
        }
        let path = parent.join(format!(
            ".fsearch-transfer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => break (StagedFile(Some(path)), file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    copy(source, &mut file)?;
    file.set_permissions(source.metadata()?.permissions())?;
    file.sync_all()?;
    // Stage and target share a directory, so publication stays on one
    // filesystem. Unsupported no-replace primitives fail closed; ordinary
    // rename must never be used as a fallback, even after an existence check.
    rename_no_replace(stage.0.as_deref().expect("stage is armed"), target)?;
    stage.0 = None;
    // Persist the published name before a cross-device move can unlink its
    // source. Failure deliberately leaves the published copy AND the source.
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn open_regular_source(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Refuse a raced-in symlink and do not block opening a raced-in FIFO.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "only regular files can be transferred",
        ));
    }
    Ok(file)
}

fn rename_no_replace(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
        let target = std::ffi::CString::new(target.as_os_str().as_bytes())?;
        #[cfg(target_os = "macos")]
        // SAFETY: both C strings remain alive through the call. RENAME_EXCL
        // makes destination creation atomic with the non-existence check.
        let result =
            unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        // SAFETY: both C strings remain alive through the call; AT_FDCWD
        // resolves relative paths against the current working directory.
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (source, target);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace move is not supported on this OS",
        ))
    }
}

/// Cross-device fallback: never remove the source until publication succeeds.
fn copy_then_remove(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    let mut file = open_regular_source(source)?;
    stage_copy(&mut file, target, std::io::copy)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let current = std::fs::symlink_metadata(source)?;
        let opened = file.metadata()?;
        if (current.dev(), current.ino()) != (opened.dev(), opened.ino()) {
            return Err(std::io::Error::other(
                "source changed; published copy kept, source not removed",
            ));
        }
    }
    std::fs::remove_file(source)
}

/// Transfers regular files only; directories are skipped and symlinks/special
/// files are rejected. Local moves use native atomic no-replace rename, keeping
/// inode identity and metadata. Only EXDEV falls back to staged copy + unlink;
/// that fallback preserves bytes/permissions, not timestamps or extended metadata.
/// Source paths must not be concurrently mutated/renamed/replaced by another
/// process: Unix identity is checked before cross-device unlink, but pathname
/// unlink is not atomic with that check. A failed unlink leaves BOTH copies
/// and reports a failure.
pub fn transfer_with_progress(
    paths: &[String],
    destination: &std::path::Path,
    kind: TransferKind,
    cancel: &std::sync::atomic::AtomicBool,
    mut progress: impl FnMut(usize),
) -> TransferOutcome {
    use std::sync::atomic::Ordering;
    let mut outcome = TransferOutcome::default();
    for (index, source) in paths.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = paths.len() - index;
            break;
        }
        transfer_one(source, destination, kind, &mut outcome);
        progress(index + 1);
    }
    outcome
}

fn transfer_one(
    source: &str,
    destination: &std::path::Path,
    kind: TransferKind,
    outcome: &mut TransferOutcome,
) {
    let source_path = std::path::Path::new(source);
    let Some(name) = source_name(source) else {
        record_error(outcome, source, "path has no file name".to_string());
        return;
    };
    let metadata = match std::fs::symlink_metadata(source_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            record_error(outcome, source, error.to_string());
            return;
        }
    };
    if metadata.is_dir() {
        outcome.skipped += 1;
        outcome.skipped_directories += 1;
        return;
    }
    if !metadata.is_file() {
        record_error(
            outcome,
            source,
            "symlinks and special files are not supported".into(),
        );
        return;
    }
    let target = destination.join(name);
    // Optimization only. Native no-replace rename is the authority; another
    // writer can create the target after this check.
    let result = (|| {
        if destination_exists(&target)? {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        }
        if kind == TransferKind::Move {
            match rename_no_replace(source_path, &target) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {}
                Err(error) => return Err(error),
            }
            copy_then_remove(source_path, &target)
        } else {
            let mut file = open_regular_source(source_path)?;
            stage_copy(&mut file, &target, std::io::copy)
        }
    })();
    match result {
        Ok(()) => {
            outcome.succeeded += 1;
            outcome.succeeded_paths.push(source.to_string());
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            outcome.skipped += 1;
            outcome.skipped_exists += 1;
        }
        Err(error) => record_error(outcome, source, error.to_string()),
    }
}

fn transfer(
    paths: &[String],
    destination: &std::path::Path,
    kind: TransferKind,
) -> TransferOutcome {
    transfer_with_progress(
        paths,
        destination,
        kind,
        &std::sync::atomic::AtomicBool::new(false),
        |_| {},
    )
}

fn record_error(outcome: &mut TransferOutcome, path: &str, error: String) {
    outcome.failed += 1;
    if outcome.first_error.is_none() {
        outcome.first_error = Some((path.to_string(), error));
    }
}

pub fn move_files(paths: &[String], destination: &std::path::Path) -> TransferOutcome {
    transfer(paths, destination, TransferKind::Move)
}

pub fn copy_files(paths: &[String], destination: &std::path::Path) -> TransferOutcome {
    transfer(paths, destination, TransferKind::Copy)
}

#[cfg(target_os = "macos")]
const CLIPBOARD_COMMANDS: &[&[&str]] = &[&["pbcopy"]];

#[cfg(not(target_os = "macos"))]
const CLIPBOARD_COMMANDS: &[&[&str]] = &[
    &["wl-copy"],
    &["xclip", "-selection", "clipboard"],
    &["xsel", "--clipboard", "--input"],
];

pub fn copy(path: &str) -> std::io::Result<()> {
    let mut last_err = std::io::Error::other("no clipboard tool found");
    for cmd in CLIPBOARD_COMMANDS {
        match Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                child
                    .stdin
                    .as_mut()
                    .expect("clipboard stdin is piped")
                    .write_all(path.as_bytes())?;
                let status = child.wait()?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other(format!(
                        "{} exited with {status}",
                        cmd[0]
                    )))
                };
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserted_placeholders_stay_literal_and_line_is_additive() {
        let path = "/tmp/{dir}/{paths}-{path}-{line}.txt";
        let cmd = vec!["{path}:{dir}:{line}".into(), "{paths}:{line}".into()];
        assert_eq!(
            expand_args_with_line(&cmd, path, &[path.into()], Some(42)),
            vec![format!("{path}:/tmp/{{dir}}:42"), format!("{path}:42")]
        );
        assert_eq!(expand_args(&["+{line}".into()], path, &[]), ["+1"]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn native_move_preserves_inode_and_fails_closed_on_collisions() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        std::fs::write(&source, "original").unwrap();
        let inode = std::fs::metadata(&source).unwrap().ino();
        std::fs::write(&target, "existing").unwrap();
        assert_eq!(
            rename_no_replace(&source, &target).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "existing");
        std::fs::remove_file(&target).unwrap();
        symlink(dir.path().join("missing"), &target).unwrap();
        assert_eq!(
            rename_no_replace(&source, &target).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(std::fs::symlink_metadata(&target).unwrap().is_symlink());
        std::fs::remove_file(&target).unwrap();
        rename_no_replace(&source, &target).unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode);
        assert!(!source.exists());
    }

    #[test]
    fn cross_device_fallback_deletes_source_only_after_successful_publication() {
        // Invoke the EXDEV branch directly: CI need not provide two volumes.
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        std::fs::write(&source, "original").unwrap();
        std::fs::write(&target, "existing").unwrap();
        assert_eq!(
            copy_then_remove(&source, &target).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "existing");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        std::fs::remove_file(&target).unwrap();
        copy_then_remove(&source, &target).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn raced_destination_is_never_replaced_and_stage_is_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        std::fs::write(&source, "new").unwrap();
        let mut file = open_regular_source(&source).unwrap();
        let result = stage_copy(&mut file, &target, |input, output| {
            let copied = std::io::copy(input, output)?;
            // Deterministically create a collision AFTER copying, just before
            // publication. A check-then-rename implementation would lose it.
            std::fs::write(&target, "winner")?;
            Ok(copied)
        });
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "winner");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "new");
    }

    #[test]
    fn partial_copy_failure_cleans_stage_and_never_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        std::fs::write(&source, "original").unwrap();
        let mut file = open_regular_source(&source).unwrap();
        let result = stage_copy(&mut file, &target, |_, output| {
            output.write_all(b"partial")?;
            Err(std::io::Error::other("injected read/write failure"))
        });
        assert!(result.is_err());
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");
    }

    #[cfg(unix)]
    #[test]
    fn dangling_destination_links_are_collisions_and_source_links_are_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        let source = dir.path().join("source");
        std::fs::write(&source, "original").unwrap();
        let missing = dir.path().join("missing");
        symlink(&missing, destination.join("source")).unwrap();
        let paths = vec![source.to_string_lossy().into_owned()];
        for kind in [TransferKind::Copy, TransferKind::Move] {
            let result = transfer(&paths, &destination, kind);
            assert_eq!(result.skipped_exists, 1);
            assert_eq!(
                std::fs::read_link(destination.join("source")).unwrap(),
                missing
            );
            assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");
        }
        for (name, target) in [("live-link", &source), ("dead-link", &missing)] {
            let link = dir.path().join(name);
            symlink(target, &link).unwrap();
            for kind in [TransferKind::Copy, TransferKind::Move] {
                let result = transfer(&[link.to_string_lossy().into_owned()], &destination, kind);
                assert_eq!(result.failed, 1);
                assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
                assert!(std::fs::symlink_metadata(destination.join(name)).is_err());
            }
        }
        assert!(!missing.exists());
    }

    #[cfg(unix)]
    #[test]
    fn fifo_source_is_rejected_without_opening_it() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fifo");
        let name = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated path, with a standard mode.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        for kind in [TransferKind::Copy, TransferKind::Move] {
            let result = transfer(&[source.to_string_lossy().into_owned()], dir.path(), kind);
            assert_eq!(result.failed, 1);
        }
        assert!(open_regular_source(&source).is_err());
    }

    #[test]
    fn failed_move_publication_keeps_source_and_cancellation_stops_between_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..3)
            .map(|n| {
                let path = dir.path().join(format!("source-{n}"));
                std::fs::write(&path, "original").unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let outcome = move_files(&paths, &dir.path().join("absent"));
        assert_eq!(outcome.failed, 3);
        assert!(paths.iter().all(|path| std::path::Path::new(path).exists()));
        let destination = dir.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let outcome =
            transfer_with_progress(&paths, &destination, TransferKind::Move, &cancel, |n| {
                assert_eq!(n, 1);
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        assert_eq!(outcome.succeeded, 1);
        assert_eq!(outcome.cancelled, 2);
        assert!(
            paths[1..]
                .iter()
                .all(|path| std::path::Path::new(path).exists())
        );
    }

    fn custom(cmd: &[&str]) -> CustomAction {
        CustomAction {
            name: "test".into(),
            cmd: cmd.iter().map(|arg| (*arg).into()).collect(),
            ext: Vec::new(),
            kind: None,
            enter: false,
        }
    }

    #[test]
    fn custom_placeholders_expand_without_shell_joining() {
        let cmd = vec![
            "editor".to_string(),
            "--file={path}".to_string(),
            "{dir}".to_string(),
            "{paths}".to_string(),
            "--marked={paths}".to_string(),
        ];
        let paths = vec!["/tmp/one file.rs".to_string(), "/tmp/two.rs".to_string()];
        assert_eq!(
            expand_args(&cmd, "/tmp/one file.rs", &paths),
            vec![
                "editor",
                "--file=/tmp/one file.rs",
                "/tmp",
                "/tmp/one file.rs",
                "/tmp/two.rs",
                "--marked=/tmp/one file.rs",
                "--marked=/tmp/two.rs"
            ]
        );
    }

    #[test]
    fn relative_paths_are_made_absolute_for_custom_actions() {
        let args = expand_args(
            &["{path}".into(), "{dir}".into()],
            "relative/file.txt",
            &["relative/file.txt".into()],
        );
        assert!(std::path::Path::new(&args[0]).is_absolute());
        assert_eq!(
            args[1],
            std::path::Path::new(&args[0])
                .parent()
                .unwrap()
                .to_string_lossy()
        );
    }

    #[test]
    fn custom_runner_reports_missing_program() {
        let action = custom(&["fsearch-command-that-does-not-exist"]);
        let error = run_custom(&action, "/tmp/file.txt", &["/tmp/file.txt".into()]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_uses_macos_open() {
        assert_eq!(
            open_args("/a b.txt"),
            ("open", vec!["/a b.txt".to_string()])
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quick_look_uses_qlmanage() {
        assert_eq!(
            quick_look_args("/a.png"),
            ("qlmanage", vec!["-p".to_string(), "/a.png".to_string()])
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn trash_goes_through_finder_with_escaping() {
        let (cmd, args) = trash_args("/a/b \"c\".txt");
        assert_eq!(cmd, "osascript");
        assert!(args[1].contains("POSIX file \"/a/b \\\"c\\\".txt\""));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reveal_uses_open_dash_r() {
        assert_eq!(
            reveal_args("/a.txt"),
            ("open", vec!["-R".to_string(), "/a.txt".to_string()])
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn open_uses_xdg_open() {
        assert_eq!(
            open_args("/a b.txt"),
            ("xdg-open", vec!["/a b.txt".to_string()])
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn reveal_opens_parent_dir() {
        assert_eq!(
            reveal_args("/a/b.txt"),
            ("xdg-open", vec!["/a".to_string()])
        );
    }

    #[test]
    fn move_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        let destination = dir.path().join("destination");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let source = source_dir.join("note.txt");
        std::fs::write(&source, "hello").unwrap();

        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&source).unwrap());
        let paths = vec![source.to_string_lossy().into_owned()];
        let outcome = move_files(&paths, &destination);

        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(
                &std::fs::metadata(destination.join("note.txt")).unwrap()
            ),
            inode
        );
        assert_eq!(outcome.succeeded, 1);
        assert_eq!(outcome.failed, 0);
        assert!(!source.exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("note.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn copy_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("note.txt");
        let destination = dir.path().join("destination");
        std::fs::write(&source, "hello").unwrap();
        std::fs::create_dir(&destination).unwrap();

        let paths = vec![source.to_string_lossy().into_owned()];
        let outcome = copy_files(&paths, &destination);

        assert_eq!(outcome.succeeded, 1);
        assert!(source.exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("note.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn transfer_skips_existing_destination_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("note.txt");
        let destination = dir.path().join("destination");
        std::fs::write(&source, "source").unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("note.txt"), "existing").unwrap();

        let paths = vec![source.to_string_lossy().into_owned()];
        let outcome = move_files(&paths, &destination);

        assert_eq!(outcome.succeeded, 0);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(outcome.skipped_exists, 1);
        assert!(source.exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("note.txt")).unwrap(),
            "existing"
        );

        let outcome = copy_files(&paths, &destination);
        assert_eq!(outcome.succeeded, 0);
        assert_eq!(outcome.skipped_exists, 1);
        assert_eq!(std::fs::read_to_string(source).unwrap(), "source");
    }

    #[test]
    fn copy_files_skips_directories() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        let source_file = dir.path().join("note.txt");
        let destination = dir.path().join("destination");
        std::fs::create_dir(&source_dir).unwrap();
        std::fs::write(&source_file, "hello").unwrap();
        std::fs::create_dir(&destination).unwrap();

        let paths = vec![
            source_dir.to_string_lossy().into_owned(),
            source_file.to_string_lossy().into_owned(),
        ];
        let outcome = copy_files(&paths, &destination);

        assert_eq!(outcome.succeeded, 1);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(outcome.skipped_directories, 1);
        assert!(source_dir.exists());
        assert!(!destination.join("source").is_file());

        let moved_dir = vec![format!("{}/", source_dir.to_string_lossy())];
        let move_outcome = move_files(&moved_dir, &destination);
        assert_eq!(move_outcome.skipped_directories, 1);
        assert!(source_dir.exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("note.txt")).unwrap(),
            "hello"
        );
    }
}
