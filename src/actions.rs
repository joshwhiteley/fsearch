use std::io::Write;
use std::process::{Command, Stdio};

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

fn transfer(
    paths: &[String],
    destination: &std::path::Path,
    kind: TransferKind,
) -> TransferOutcome {
    let mut outcome = TransferOutcome::default();
    for source in paths {
        let source_path = std::path::Path::new(source);
        let Some(name) = source_name(source) else {
            record_error(&mut outcome, source, "path has no file name".to_string());
            continue;
        };
        let metadata = match std::fs::symlink_metadata(source_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                record_error(&mut outcome, source, error.to_string());
                continue;
            }
        };
        if metadata.is_dir() {
            outcome.skipped += 1;
            outcome.skipped_directories += 1;
            continue;
        }
        let target = destination.join(name);
        match destination_exists(&target) {
            Ok(true) => {
                outcome.skipped += 1;
                outcome.skipped_exists += 1;
                continue;
            }
            Ok(false) => {}
            Err(error) => {
                record_error(&mut outcome, source, error.to_string());
                continue;
            }
        }
        let result = match kind {
            TransferKind::Copy => std::fs::copy(source_path, &target).map(|_| ()),
            TransferKind::Move => match std::fs::rename(source_path, &target) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                    std::fs::copy(source_path, &target)
                        .and_then(|_| std::fs::remove_file(source_path))
                }
                Err(error) => Err(error),
            },
        };
        match result {
            Ok(()) => {
                outcome.succeeded += 1;
                outcome.succeeded_paths.push(source.clone());
            }
            Err(error) => record_error(&mut outcome, source, error.to_string()),
        }
    }
    outcome
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

        let paths = vec![source.to_string_lossy().into_owned()];
        let outcome = move_files(&paths, &destination);

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
