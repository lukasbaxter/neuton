//! Where the session is stored between launches.
//!
//! This file is a live credential: anyone who reads it can play as the user
//! until the refresh token is revoked. It is written with owner-only
//! permissions, and written via a temporary file so an interrupted save cannot
//! leave a truncated one behind.

use crate::{Error, Result, Session};
use std::path::{Path, PathBuf};

/// Resolves the per-user configuration directory for this client.
pub fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        Some(home?.join("Library/Application Support/neuton"))
    } else if cfg!(windows) {
        Some(PathBuf::from(std::env::var_os("APPDATA")?).join("neuton"))
    } else {
        match std::env::var_os("XDG_CONFIG_HOME") {
            Some(x) if !x.is_empty() => Some(PathBuf::from(x).join("neuton")),
            _ => Some(home?.join(".config/neuton")),
        }
    }
}

/// Location of the cached session.
#[derive(Debug, Clone)]
pub struct CachePath(pub PathBuf);

impl CachePath {
    pub fn default_path() -> Result<Self> {
        let dir = config_dir().ok_or_else(|| {
            Error::Io(std::io::Error::other("could not determine a config directory"))
        })?;
        Ok(Self(dir.join("session.json")))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Loads a cached session, or `None` if there is not a readable one.
    ///
    /// A corrupt or outdated cache is not an error: it just means we sign in
    /// again rather than refusing to start.
    pub fn load(&self) -> Option<Session> {
        let raw = std::fs::read_to_string(&self.0).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn store(&self, session: &Session) -> Result<()> {
        if let Some(parent) = self.0.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.0.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(session)?)?;
        restrict_permissions(&tmp)?;
        // Rename is atomic on every platform we target, so a crash mid-write
        // leaves the previous session intact rather than a half-written file.
        std::fs::rename(&tmp, &self.0)?;
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.0) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    // Windows inherits the user profile's ACL, which is already owner-only.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Profile, Session};

    fn sample() -> Session {
        Session {
            profile: Profile { uuid: 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef, name: "Miji".into() },
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            expires_at: 1_800_000_000,
        }
    }

    #[test]
    fn store_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("neuton-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = CachePath(dir.join("session.json"));

        cache.store(&sample()).unwrap();
        let got = cache.load().expect("cached session should load");
        assert_eq!(got.profile, sample().profile);
        assert_eq!(got.access_token, "access");

        cache.clear().unwrap();
        assert!(cache.load().is_none());
        cache.clear().expect("clearing an absent cache is not an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_cache_reads_as_absent_rather_than_failing() {
        let dir = std::env::temp_dir().join(format!("neuton-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = CachePath(dir.join("session.json"));
        std::fs::write(cache.path(), b"{not json").unwrap();
        assert!(cache.load().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_session_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("neuton-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = CachePath(dir.join("session.json"));
        cache.store(&sample()).unwrap();
        let mode = std::fs::metadata(cache.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "credential file must be owner-only, got {mode:o}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
