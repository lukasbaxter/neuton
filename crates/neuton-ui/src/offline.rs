//! Playing without signing in.
//!
//! A server in offline mode asks for a name and nothing else, which is the
//! only way into a world while Microsoft sign-in is waiting on app review. The
//! name is kept next to the accounts, in the same plain JSON, because typing it
//! on every launch is exactly the friction that makes a launcher not worth
//! opening.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offline {
    pub name: String,
    /// Whether joins should use this name rather than the signed-in account.
    pub active: bool,
}

impl Default for Offline {
    fn default() -> Self {
        Self { name: String::new(), active: false }
    }
}

impl Offline {
    pub fn path() -> PathBuf {
        neuton_auth::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("offline.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(&tmp, &path)
    }

    /// A name a server will accept: what vanilla allows, and no more.
    pub fn name_problem(name: &str) -> Option<&'static str> {
        match name {
            "" => Some("a name is needed"),
            n if n.chars().count() > 16 => Some("at most 16 characters"),
            n if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                Some("letters, digits and underscore only")
            }
            _ => None,
        }
    }

    /// The identity to join with.
    ///
    /// No tokens: an offline server never asks the session service about the
    /// player, and the UUID it files them under is its own. Sending an empty
    /// token is what the client already does from the command line.
    pub fn session(&self) -> Option<neuton_auth::Session> {
        if !self.active || Self::name_problem(&self.name).is_some() {
            return None;
        }
        Some(neuton_auth::Session {
            profile: neuton_auth::Profile { uuid: 0, name: self.name.clone() },
            access_token: String::new(),
            refresh_token: String::new(),
            expires_at: u64::MAX,
        })
    }
}
