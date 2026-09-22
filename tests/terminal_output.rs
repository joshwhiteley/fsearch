#![cfg(unix)]

use std::fs::File;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::process::{Command, Stdio};

fn terminal_stdout(mut command: Command) -> Vec<u8> {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: both fd outputs are valid pointers. No optional name/termios/
    // winsize pointers are supplied. The returned fds are each owned once.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    // Drain concurrently: Darwin may discard unread PTY output when the last
    // slave closes, and waiting first can deadlock on larger outputs anywhere.
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("reading PTY: {error}"),
            }
        }
        bytes
    });
    let result = command
        .stdin(Stdio::null())
        .stdout(slave)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    drop(command); // Close the parent's slave so the reader sees EOF.
    let bytes = reader.join().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    bytes
}

#[test]
fn cli_escapes_terminal_controls_only_when_stdout_is_a_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("files");
    let config = temp.path().join("config");
    let cache = temp.path().join("cache");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(config.join("fsearch")).unwrap();
    let name = "needle\x1b]52;c;payload\x07.txt";
    std::fs::write(root.join(name), "example").unwrap();
    std::fs::write(
        config.join("fsearch/config.toml"),
        format!(
            "roots = [{:?}]\nindex_apps = false\nremember_history = false\n",
            root.to_str().unwrap()
        ),
    )
    .unwrap();
    let command = |options: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fsearch"));
        command
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_CACHE_HOME", &cache)
            .args(options)
            .args(["-p", "needle"]);
        command
    };
    let tty = terminal_stdout(command(&[]));
    assert!(!tty.contains(&0x1b) && !tty.contains(&0x07));
    let text = String::from_utf8(tty).unwrap();
    assert!(
        text.contains(&fsearch::output::escape_controls(name)),
        "unexpected TTY output: {text:?}"
    );
    let pipe = command(&[]).output().unwrap();
    assert!(pipe.status.success());
    assert!(String::from_utf8(pipe.stdout).unwrap().contains(name));
    let nul = terminal_stdout(command(&["--print0"]));
    assert!(nul.contains(&0x1b) && nul.ends_with(&[0]));
    let json = terminal_stdout(command(&["--json"]));
    let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert!(value["path"].as_str().unwrap().ends_with(name));
}
