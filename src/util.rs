/// "412 B", "1.3 KB", "2.0 MB", "1.1 GB"
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Current time as whole seconds since the Unix epoch (0 if the clock is
/// set before it).
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Creates an application-owned directory with private permissions. Only the
/// requested directory is tightened; existing ancestors are never chmod'd.
pub fn create_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::fs::{DirBuilder, Permissions};
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "private directory is not a real directory",
        ));
    }
    #[cfg(unix)]
    std::fs::set_permissions(
        path,
        Permissions::from_mode(meta.permissions().mode() & 0o700),
    )?;
    Ok(())
}

/// Exclusively creates a private staging file. Existing files and symlinks
/// are errors, never truncated. Callers publish it using an atomic rename.
pub fn create_private_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Opens an application history file without following a final symlink or
/// blocking on a FIFO. Existing history files are tightened to 0600.
pub fn append_private_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("history is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Reads regular files only. Nonblocking, no-follow opens prevent a stale
/// indexed path replaced by a FIFO or symlink from wedging a worker.
pub fn open_regular_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok(file)
}

#[cfg(all(test, unix))]
mod private_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn private_permissions_do_not_change_ancestors() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = root.path().join("fsearch");
        create_private_dir(&dir).unwrap();
        let path = dir.join("data");
        create_private_file(&path).unwrap();
        assert_eq!(
            std::fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(create_private_file(&path).is_err());
    }

    #[test]
    fn private_files_and_directories_reject_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        let link = root.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(create_private_file(&link).is_err());
        assert!(append_private_file(&link).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        let dirlink = root.path().join("dirlink");
        symlink(root.path(), &dirlink).unwrap();
        assert!(create_private_dir(&dirlink).is_err());
    }
}
