//! A sign-in that got as far as Microsoft and no further.
//!
//! Mojang gates its API on a one-time review of the application. Until that is
//! granted, every sign-in dies at the last step -- after the user has already
//! opened a browser and typed a code. Keeping the Microsoft refresh token means
//! the retry, whenever the approval lands, needs none of that.

use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    Some(crate::accounts::config_dir()?.join("pending.json"))
}

/// Remembers a half-finished sign-in. Best effort: failing to write it costs a
/// browser trip later, which is not worth failing the sign-in over.
pub fn save(refresh_token: &str) {
    if refresh_token.is_empty() {
        return;
    }
    let Some(path) = path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, refresh_token).is_ok() {
        let _ = crate::accounts::restrict_permissions(&path);
    }
}

/// The refresh token from a half-finished sign-in, if there is one.
pub fn take() -> Option<String> {
    let path = path()?;
    let token = std::fs::read_to_string(&path).ok()?;
    let token = token.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Forgets it, once a sign-in has actually completed.
pub fn clear() {
    if let Some(path) = path() {
        let _ = std::fs::remove_file(path);
    }
}
