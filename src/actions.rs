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
    let mut args = Vec::new();
    for arg in cmd {
        if arg.contains("{paths}") {
            args.extend(paths.iter().map(|path| {
                arg.replace("{paths}", path)
                    .replace("{path}", path)
                    .replace("{dir}", &dir)
            }));
        } else {
            args.push(arg.replace("{path}", &path).replace("{dir}", &dir));
        }
    }
    args
}

/// Launches a configured action detached from the TUI, with no shell and no
/// inherited standard streams.
pub fn run_custom(action: &CustomAction, selected: &str, paths: &[String]) -> std::io::Result<()> {
    let args = expand_args(&action.cmd, selected, paths);
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
}
