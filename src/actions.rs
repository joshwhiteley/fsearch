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

pub fn open(path: &str) -> std::io::Result<()> {
    run(open_args(path))
}

pub fn reveal(path: &str) -> std::io::Result<()> {
    run(reveal_args(path))
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
                return child.wait().map(|_| ());
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
