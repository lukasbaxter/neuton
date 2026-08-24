//! The account store.
//!
//! A client other people install needs to hold more than one sign-in: shared
//! machines, alt accounts, a parent and a kid. So sessions live in a keyed
//! store with one marked active, rather than a single overwritten file.
//!
//! The file is a live credential. Anyone who can read it can play as any
//! account in it until those refresh tokens are revoked. It is written
//! owner-only and replaced atomically.

use crate::{Error, Result, Session};
use serde::{Deserialize, Serialize};
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

/// Every signed-in account on this machine.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Accounts {
    /// In the order they were added.
    #[serde(default)]
    accounts: Vec<Session>,
    /// UUID of the account launches use by default.
    #[serde(default)]
    active: Option<u128>,
    /// Where this was loaded from, so `save` needs no argument.
    #[serde(skip)]
    path: PathBuf,
}

impl Accounts {
    pub fn default_path() -> Result<PathBuf> {
        let dir = config_dir().ok_or_else(|| {
            Error::Io(std::io::Error::other("could not determine a config directory"))
        })?;
        Ok(dir.join("accounts.json"))
    }

    /// Loads the store, returning an empty one if there is not a readable file.
    ///
    /// A corrupt store is not fatal: it means signing in again, not refusing to
    /// launch.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut store: Accounts = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        store.path = path;
        store
    }

    pub fn load_default() -> Result<Self> {
        Ok(Self::load(Self::default_path()?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        restrict_permissions(&tmp)?;
        // Atomic on every platform we target, so an interrupted save leaves the
        // previous store intact rather than a truncated one.
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn list(&self) -> &[Session] {
        &self.accounts
    }

    /// Adds or replaces an account, and makes it active.
    ///
    /// Keyed on UUID rather than name so a rename does not create a duplicate.
    pub fn upsert(&mut self, session: Session) {
        let uuid = session.profile.uuid;
        match self.accounts.iter_mut().find(|a| a.profile.uuid == uuid) {
            Some(existing) => *existing = session,
            None => self.accounts.push(session),
        }
        self.active = Some(uuid);
    }

    /// The account launches should use.
    ///
    /// Falls back to the first account if the active one was removed, so a
    /// stale pointer never leaves a populated store looking empty.
    pub fn active(&self) -> Option<&Session> {
        self.active
            .and_then(|uuid| self.accounts.iter().find(|a| a.profile.uuid == uuid))
            .or_else(|| self.accounts.first())
    }

    pub fn active_mut(&mut self) -> Option<&mut Session> {
        let uuid = self.active.or_else(|| self.accounts.first().map(|a| a.profile.uuid))?;
        self.accounts.iter_mut().find(|a| a.profile.uuid == uuid)
    }

    /// Finds an account by name, case-insensitively.
    pub fn find(&self, name: &str) -> Option<&Session> {
        self.accounts.iter().find(|a| a.profile.name.eq_ignore_ascii_case(name))
    }

    /// Switches the active account. Returns false if no such account exists.
    pub fn set_active(&mut self, name: &str) -> bool {
        match self.find(name).map(|a| a.profile.uuid) {
            Some(uuid) => {
                self.active = Some(uuid);
                true
            }
            None => false,
        }
    }

    pub fn is_active(&self, session: &Session) -> bool {
        self.active() .is_some_and(|a| a.profile.uuid == session.profile.uuid)
    }

    /// Removes one account. Returns whether it was there.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| !a.profile.name.eq_ignore_ascii_case(name));
        let removed = self.accounts.len() != before;
        if removed && self.active.is_some_and(|u| !self.accounts.iter().any(|a| a.profile.uuid == u))
        {
            self.active = self.accounts.first().map(|a| a.profile.uuid);
        }
        removed
    }

    pub fn clear(&mut self) {
        self.accounts.clear();
        self.active = None;
    }

    /// Deletes the file entirely.
    pub fn delete_file(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
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
    use crate::Profile;

    fn session(name: &str, uuid: u128) -> Session {
        Session {
            profile: Profile { uuid, name: name.into() },
            access_token: format!("token-{name}"),
            refresh_token: format!("refresh-{name}"),
            expires_at: 1_900_000_000,
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("neuton-acct-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("accounts.json")
    }

    #[test]
    fn several_accounts_persist_and_the_active_one_survives_a_reload() {
        let path = temp("multi");
        let mut a = Accounts::load(&path);
        a.upsert(session("Miji", 1));
        a.upsert(session("Alt", 2));
        assert!(a.set_active("miji"), "lookup should be case-insensitive");
        a.save().unwrap();

        let b = Accounts::load(&path);
        assert_eq!(b.list().len(), 2);
        assert_eq!(b.active().unwrap().profile.name, "Miji");
        assert_eq!(b.find("ALT").unwrap().access_token, "token-Alt");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn signing_in_again_updates_rather_than_duplicates() {
        let mut a = Accounts::load(temp("dup"));
        a.upsert(session("Miji", 1));
        let mut renamed = session("MijiRenamed", 1);
        renamed.access_token = "fresh".into();
        a.upsert(renamed);

        assert_eq!(a.list().len(), 1, "same uuid must not create a second entry");
        assert_eq!(a.active().unwrap().access_token, "fresh");
        assert_eq!(a.active().unwrap().profile.name, "MijiRenamed");
    }

    #[test]
    fn removing_the_active_account_promotes_another() {
        let mut a = Accounts::load(temp("remove"));
        a.upsert(session("First", 1));
        a.upsert(session("Second", 2));
        assert_eq!(a.active().unwrap().profile.name, "Second");

        assert!(a.remove("second"));
        assert_eq!(a.list().len(), 1);
        assert_eq!(a.active().unwrap().profile.name, "First");

        assert!(!a.remove("nobody"));
        assert!(a.remove("first"));
        assert!(a.active().is_none());
        assert!(a.is_empty());
    }

    #[test]
    fn a_corrupt_store_loads_as_empty_rather_than_failing() {
        let path = temp("corrupt");
        std::fs::write(&path, b"{ not json").unwrap();
        let a = Accounts::load(&path);
        assert!(a.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn the_account_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp("perm");
        let mut a = Accounts::load(&path);
        a.upsert(session("Miji", 1));
        a.save().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "credential file must be owner-only, got {mode:o}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
