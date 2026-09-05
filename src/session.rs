use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// UI settings carried between runs: the preview layout and the results
/// row density. `None` means "not recorded" — the config default applies.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionState {
    pub preview_layout: Option<String>,
    pub density: Option<String>,
}

pub fn default_state_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".local")
                .join("state")
        });
    base.join("fsearch").join("session.toml")
}

/// Loads the saved settings. A missing or corrupt file falls back silently
/// to an empty state (config defaults apply).
pub fn load(path: &Path) -> SessionState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return SessionState::default();
    };
    #[derive(serde::Deserialize)]
    struct Raw {
        preview_layout: Option<String>,
        density: Option<String>,
    }
    let Ok(raw) = toml::from_str::<Raw>(&text) else {
        return SessionState::default();
    };
    SessionState {
        preview_layout: raw.preview_layout,
        density: raw.density,
    }
}

/// Persists the settings with an atomic replace: pid + counter temp name so
/// concurrent fsearch processes never truncate each other's temp file.
pub fn save(path: &Path, preview_layout: &str, density: &str) {
    let body = format!("preview_layout = \"{preview_layout}\"\ndensity = \"{density}\"\n");
    if let Some(parent) = path.parent()
        && crate::util::create_private_dir(parent).is_err()
    {
        return;
    }
    let nonce = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let Ok(mut f) = crate::util::create_private_file(&tmp) else {
        return;
    };
    let ok = (|| {
        f.write_all(body.as_bytes())?;
        // make the bytes durable before the rename publishes them
        f.sync_all()
    })();
    if ok.is_ok() {
        let _ = std::fs::rename(&tmp, path);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        save(&path, "full", "compact");
        let state = load(&path);
        assert_eq!(state.preview_layout.as_deref(), Some("full"));
        assert_eq!(state.density.as_deref(), Some("compact"));
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&dir.path().join("absent.toml")),
            SessionState::default()
        );
    }

    #[test]
    fn corrupt_file_falls_back_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "preview_layout = not toml at all").unwrap();
        assert_eq!(load(&path), SessionState::default());
    }
}
