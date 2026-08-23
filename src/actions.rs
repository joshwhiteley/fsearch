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
}
