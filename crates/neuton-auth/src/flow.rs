//! The OAuth device-code flow and the Xbox relay behind it.
//!
//! Device code rather than a redirect flow on purpose: it needs no embedded
//! browser, no local HTTP listener and no custom URI scheme registration, so it
//! behaves identically on all three target platforms and adds nothing to the
//! binary.

use crate::session::{now, profile_from_json};
use crate::{
    Error, MC_LOGIN_URL, MC_PROFILE_URL, MS_DEVICE_CODE_URL, MS_TOKEN_URL, Profile, Result, SCOPE,
    Session, XBL_AUTH_URL, XSTS_AUTH_URL,
};
use serde_json::{Value, json};
use std::time::Duration;

/// What the user has to do, and where.
#[derive(Debug, Clone)]
pub struct DeviceCode {
    /// Short code the user types into the browser.
    pub user_code: String,
    /// Page the user opens, usually `https://www.microsoft.com/link`.
    pub verification_uri: String,
    /// Opaque handle we poll with.
    pub device_code: String,
    /// Minimum seconds between polls; polling faster earns a slow_down.
    pub interval: u64,
    /// Unix seconds after which the code stops working.
    pub expires_at: u64,
}

impl DeviceCode {
    pub fn expires_in(&self) -> i64 {
        self.expires_at as i64 - now() as i64
    }
}

pub struct DeviceCodeFlow {
    agent: ureq::Agent,
    client_id: String,
}

impl DeviceCodeFlow {
    pub fn new(client_id: String) -> Self {
        // Non-2xx responses must come back as values, not errors: the whole
        // device-code protocol signals "not yet approved" with a 400 whose body
        // carries the real state.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(concat!("neuton/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent, client_id }
    }

    /// Asks Microsoft for a code to show the user.
    pub fn start(&self) -> Result<DeviceCode> {
        let mut res = self
            .agent
            .post(MS_DEVICE_CODE_URL)
            .send_form([("client_id", self.client_id.as_str()), ("scope", SCOPE)])?;
        let body: Value = res.body_mut().read_json()?;

        if let Some(err) = body.get("error").and_then(|e| e.as_str()) {
            return Err(Error::Upstream {
                stage: "microsoft device code",
                code: err.to_string(),
                message: describe(&body),
            });
        }

        let interval = body.get("interval").and_then(|v| v.as_u64()).unwrap_or(5);
        let expires_in = body.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(900);
        Ok(DeviceCode {
            user_code: string_field(&body, "user_code", "microsoft device code")?,
            verification_uri: string_field(&body, "verification_uri", "microsoft device code")?,
            device_code: string_field(&body, "device_code", "microsoft device code")?,
            // Never poll faster than told, and never slower than makes the UI
            // feel dead.
            interval: interval.clamp(1, 30),
            expires_at: now() + expires_in,
        })
    }

    /// Polls once. `Ok(None)` means the user has not approved it yet.
    pub fn poll(&self, dc: &DeviceCode) -> Result<Option<Session>> {
        let mut res = self.agent.post(MS_TOKEN_URL).send_form([
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", self.client_id.as_str()),
            ("device_code", dc.device_code.as_str()),
        ])?;
        let body: Value = res.body_mut().read_json()?;

        if let Some(err) = body.get("error").and_then(|e| e.as_str()) {
            return match err {
                // Expected while the user is still in the browser.
                "authorization_pending" | "slow_down" => Ok(None),
                "expired_token" | "code_expired" => Err(Error::DeviceCodeExpired),
                _ => Err(Error::Upstream {
                    stage: "microsoft device code",
                    code: err.to_string(),
                    message: describe(&body),
                }),
            };
        }
        self.finish(&body).map(Some)
    }

    /// Polls until the user approves, the code expires, or something breaks.
    ///
    /// `on_wait` is called before each sleep so a caller can animate or check
    /// for cancellation.
    pub fn wait(&self, dc: &DeviceCode, mut on_wait: impl FnMut(i64)) -> Result<Session> {
        let mut interval = dc.interval;
        loop {
            if dc.expires_in() <= 0 {
                return Err(Error::DeviceCodeExpired);
            }
            match self.poll(dc) {
                Ok(Some(session)) => return Ok(session),
                Ok(None) => {}
                // A transient network blip should not throw away a sign-in the
                // user may already have approved; keep polling until the code
                // itself expires.
                Err(Error::Http(_)) => interval = (interval + 2).min(30),
                Err(e) => return Err(e),
            }
            on_wait(dc.expires_in());
            std::thread::sleep(Duration::from_secs(interval));
        }
    }

    /// Trades a stored refresh token for a fresh session, with no user
    /// interaction. This is the path a normal launch takes.
    pub fn refresh(&self, refresh_token: &str) -> Result<Session> {
        let mut res = self.agent.post(MS_TOKEN_URL).send_form([
            ("grant_type", "refresh_token"),
            ("client_id", self.client_id.as_str()),
            ("scope", SCOPE),
            ("refresh_token", refresh_token),
        ])?;
        let body: Value = res.body_mut().read_json()?;
        if body.get("error").is_some() {
            return Err(Error::RefreshRejected);
        }
        self.finish(&body)
    }

    /// Microsoft token in hand, walk the rest of the chain.
    fn finish(&self, ms: &Value) -> Result<Session> {
        let ms_access = string_field(ms, "access_token", "microsoft token")?;
        let refresh_token = ms
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let (xbl_token, user_hash) = self.xbox_live(&ms_access)?;
        let (xsts_token, xsts_hash) = self.xsts(&xbl_token)?;
        // The two stages return the same user hash; prefer XSTS's.
        let hash = if xsts_hash.is_empty() { user_hash } else { xsts_hash };

        let (access_token, expires_at) = match self.minecraft_token(&hash, &xsts_token) {
            Ok(pair) => pair,
            Err(e) => {
                // The Microsoft half of the sign-in succeeded and cost the user
                // a trip to a browser. If only Mojang refused -- which is a
                // gate on the application, not on this person, and can be
                // lifted later -- keep the refresh token so the next attempt
                // costs nothing.
                if matches!(e, Error::AppNotApproved) {
                    crate::pending::save(&refresh_token);
                }
                return Err(e);
            }
        };
        let profile = self.profile(&access_token)?;

        Ok(Session { profile, access_token, expires_at, refresh_token })
    }

    fn xbox_live(&self, ms_access: &str) -> Result<(String, String)> {
        let mut res = self.agent.post(XBL_AUTH_URL).content_type("application/json").send_json(
            json!({
                "Properties": {
                    "AuthMethod": "RPS",
                    "SiteName": "user.auth.xboxlive.com",
                    // The "d=" prefix marks this as a delegated Microsoft token.
                    "RpsTicket": format!("d={ms_access}"),
                },
                "RelyingParty": "http://auth.xboxlive.com",
                "TokenType": "JWT",
            }),
        )?;
        let status = res.status().as_u16();
        let body: Value = res.body_mut().read_json()?;
        if status >= 400 {
            return Err(Error::Upstream {
                stage: "xbox live",
                code: status.to_string(),
                message: describe(&body),
            });
        }
        Ok((string_field(&body, "Token", "xbox live")?, user_hash(&body)))
    }

    fn xsts(&self, xbl_token: &str) -> Result<(String, String)> {
        let mut res = self.agent.post(XSTS_AUTH_URL).content_type("application/json").send_json(
            json!({
                "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl_token] },
                "RelyingParty": "rp://api.minecraftservices.com/",
                "TokenType": "JWT",
            }),
        )?;
        let status = res.status().as_u16();
        let body: Value = res.body_mut().read_json()?;

        // XSTS reports account-level problems as XErr codes. These are the ones
        // a user can actually act on, so name them instead of printing a number.
        if let Some(xerr) = body.get("XErr").and_then(|v| v.as_i64()) {
            let why = match xerr {
                2_148_916_233 => "this Microsoft account has no Xbox profile; create one at xbox.com and try again",
                2_148_916_235 => "Xbox Live is not available in this account's region",
                2_148_916_236 | 2_148_916_237 => "this account needs adult verification",
                2_148_916_238 => "this is a child account and must be added to a family before it can sign in",
                _ => return Err(Error::Upstream {
                    stage: "xsts",
                    code: xerr.to_string(),
                    message: describe(&body),
                }),
            };
            return Err(Error::NotEntitled(why.to_string()));
        }
        if status >= 400 {
            return Err(Error::Upstream {
                stage: "xsts",
                code: status.to_string(),
                message: describe(&body),
            });
        }
        Ok((string_field(&body, "Token", "xsts")?, user_hash(&body)))
    }

    fn minecraft_token(&self, user_hash: &str, xsts_token: &str) -> Result<(String, u64)> {
        let mut res = self.agent.post(MC_LOGIN_URL).content_type("application/json").send_json(
            json!({ "identityToken": format!("XBL3.0 x={user_hash};{xsts_token}") }),
        )?;
        let status = res.status().as_u16();
        let body: Value = res.body_mut().read_json()?;
        if status >= 400 {
            let message = describe(&body);
            // Worth having verbatim when an approval is expected but refused:
            // the difference between "not reviewed" and "reviewed, wrong id" is
            // only visible in what Mojang actually said.
            if std::env::var_os("NEUTON_TRACE").is_some() {
                eprintln!("auth: minecraft services {status}: {body}");
            }
            // Mojang gates the Minecraft API on a one-time review of the Azure
            // application. The raw message is four words and a link, which
            // reads like a misconfiguration rather than a policy gate.
            if message.contains("Invalid app registration") {
                return Err(Error::AppNotApproved);
            }
            return Err(Error::Upstream {
                stage: "minecraft services",
                code: status.to_string(),
                message,
            });
        }
        let token = string_field(&body, "access_token", "minecraft services")?;
        let expires_in = body.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(86_400);
        Ok((token, now() + expires_in))
    }

    fn profile(&self, mc_token: &str) -> Result<Profile> {
        let mut res = self
            .agent
            .get(MC_PROFILE_URL)
            .header("Authorization", &format!("Bearer {mc_token}"))
            .call()?;
        let status = res.status().as_u16();
        // A signed-in Microsoft account without a copy of the game reaches this
        // point and then 404s, which is worth saying plainly.
        if status == 404 {
            return Err(Error::NotEntitled(
                "this account does not own Minecraft: Java Edition".to_string(),
            ));
        }
        let body: Value = res.body_mut().read_json()?;
        if status >= 400 {
            return Err(Error::Upstream {
                stage: "minecraft profile",
                code: status.to_string(),
                message: describe(&body),
            });
        }
        profile_from_json(&body)
    }
}

/// Pulls `DisplayClaims.xui[0].uhs`, the user hash both Xbox stages return.
fn user_hash(body: &Value) -> String {
    body.pointer("/DisplayClaims/xui/0/uhs")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn string_field(body: &Value, key: &str, stage: &'static str) -> Result<String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| Error::Upstream {
            stage,
            code: "missing_field".into(),
            message: format!("response had no {key:?}"),
        })
}

/// Best-effort human message out of whatever error shape the service used.
fn describe(body: &Value) -> String {
    for key in ["error_description", "message", "Message", "errorMessage", "error"] {
        if let Some(s) = body.get(key).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    body.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_hash_is_read_from_the_nested_claim() {
        let body = json!({ "DisplayClaims": { "xui": [ { "uhs": "12345" } ] } });
        assert_eq!(user_hash(&body), "12345");
        assert_eq!(user_hash(&json!({})), "");
    }

    #[test]
    fn mojangs_app_registration_refusal_is_recognised() {
        // The exact body Minecraft services returns for an unapproved app.
        let body = json!({
            "path": "/authentication/login_with_xbox",
            "errorMessage": "Invalid app registration, see https://aka.ms/AppRegInfo for more information"
        });
        assert!(describe(&body).contains("Invalid app registration"));
        // And it must not be reported as retryable.
        assert!(!Error::AppNotApproved.is_recoverable());
        assert!(Error::AppNotApproved.to_string().contains("mce-reviewappid"));
    }

    #[test]
    fn describe_prefers_the_human_readable_field() {
        assert_eq!(
            describe(&json!({ "error": "invalid_grant", "error_description": "expired" })),
            "expired"
        );
        assert_eq!(describe(&json!({ "error": "invalid_grant" })), "invalid_grant");
    }

    #[test]
    fn missing_fields_name_the_stage_that_failed() {
        let e = string_field(&json!({}), "Token", "xsts").unwrap_err();
        assert!(e.to_string().contains("xsts"), "{e}");
        assert!(e.to_string().contains("Token"), "{e}");
    }
}
