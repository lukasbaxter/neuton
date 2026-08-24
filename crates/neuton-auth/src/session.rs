//! The authenticated session, and its on-disk form.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// The player, as Mojang knows them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    /// Account UUID. Stored as the hyphenless hex Mojang sends.
    pub uuid: u128,
    pub name: String,
}

impl Profile {
    /// Parses Mojang's hyphenless UUID form, and tolerates the hyphenated one.
    pub fn parse_uuid(s: &str) -> Option<u128> {
        let mut acc: u128 = 0;
        let mut digits = 0;
        for c in s.chars() {
            if c == '-' {
                continue;
            }
            acc = (acc << 4) | c.to_digit(16)? as u128;
            digits += 1;
            if digits > 32 {
                return None;
            }
        }
        (digits == 32).then_some(acc)
    }

    /// Hyphenated form, for display and for the session-server URL.
    pub fn uuid_hyphenated(&self) -> String {
        let h = format!("{:032x}", self.uuid);
        format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
    }
}

/// A usable Minecraft session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub profile: Profile,
    /// Minecraft services access token. This is the credential the join
    /// handshake signs with.
    pub access_token: String,
    /// Unix seconds at which `access_token` stops working.
    pub expires_at: u64,
    /// Microsoft refresh token, used to mint a new session without a browser.
    pub refresh_token: String,
}

impl Session {
    /// Treats a session as expired slightly early.
    ///
    /// A token that dies mid-handshake surfaces as a confusing "invalid
    /// session" disconnect, so we refresh while there is still margin.
    const SKEW: u64 = 120;

    pub fn is_valid(&self) -> bool {
        now() + Self::SKEW < self.expires_at
    }

    pub fn expires_in(&self) -> i64 {
        self.expires_at as i64 - now() as i64
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Reads `{ "id": "...", "name": "..." }` from the profile endpoint.
pub(crate) fn profile_from_json(v: &serde_json::Value) -> Result<Profile> {
    let id = v.get("id").and_then(|v| v.as_str()).ok_or_else(|| Error::Upstream {
        stage: "minecraft profile",
        code: "missing_id".into(),
        message: "profile response had no id".into(),
    })?;
    let name = v.get("name").and_then(|v| v.as_str()).ok_or_else(|| Error::Upstream {
        stage: "minecraft profile",
        code: "missing_name".into(),
        message: "profile response had no name".into(),
    })?;
    let uuid = Profile::parse_uuid(id).ok_or_else(|| Error::Upstream {
        stage: "minecraft profile",
        code: "bad_uuid".into(),
        message: format!("could not parse uuid {id:?}"),
    })?;
    Ok(Profile { uuid, name: name.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_parses_in_both_forms_and_round_trips() {
        let hyphenless = "069a79f444e94726a5befca90e38aaf5";
        let hyphenated = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
        let a = Profile::parse_uuid(hyphenless).unwrap();
        let b = Profile::parse_uuid(hyphenated).unwrap();
        assert_eq!(a, b);
        let p = Profile { uuid: a, name: "Notch".into() };
        assert_eq!(p.uuid_hyphenated(), hyphenated);
    }

    #[test]
    fn malformed_uuids_are_rejected() {
        assert_eq!(Profile::parse_uuid(""), None);
        assert_eq!(Profile::parse_uuid("069a79f4"), None); // too short
        assert_eq!(Profile::parse_uuid(&"0".repeat(33)), None); // too long
        assert_eq!(Profile::parse_uuid("069a79f444e94726a5befca90e38aazz"), None); // non-hex
    }

    #[test]
    fn a_session_expiring_within_the_skew_counts_as_expired() {
        let base = Session {
            profile: Profile { uuid: 1, name: "x".into() },
            access_token: "t".into(),
            refresh_token: "r".into(),
            expires_at: now() + Session::SKEW / 2,
        };
        assert!(!base.is_valid(), "must refresh before the token actually dies");

        let good = Session { expires_at: now() + 3600, ..base.clone() };
        assert!(good.is_valid());

        let dead = Session { expires_at: now().saturating_sub(1), ..base };
        assert!(!dead.is_valid());
    }
}
