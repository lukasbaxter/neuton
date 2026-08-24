//! Microsoft account authentication, the four-legged relay that ends in a
//! Minecraft session token.
//!
//! ```text
//!   device code  ->  Microsoft OAuth  ->  Xbox Live  ->  XSTS  ->  Minecraft
//! ```
//!
//! Only the first launch does any of this. After that the result is cached and
//! a normal start touches the network zero times, which matters because the
//! whole point of this client is that launching it is instant. A cached session
//! that has expired refreshes in one round trip, and only a fully expired
//! refresh token sends the user back to a browser.

mod cache;
mod error;
mod flow;
mod session;

pub use cache::CachePath;
pub use error::{Error, Result};
pub use flow::{DeviceCode, DeviceCodeFlow};
pub use session::{Profile, Session};

/// Azure application (client) ID used for the OAuth flow.
///
/// There is deliberately no default. Every launcher that talks to Microsoft
/// must register its own application; borrowing another project's ID would put
/// this client's users behind someone else's consent screen and rate limits,
/// and gets that ID revoked. See `docs/AUTH.md` for the two-minute setup.
pub fn client_id() -> Result<String> {
    if let Ok(id) = std::env::var("NEUTON_CLIENT_ID")
        && !id.trim().is_empty()
    {
        return Ok(id.trim().to_string());
    }
    if let Some(path) = cache::config_dir() {
        let file = path.join("client_id");
        if let Ok(id) = std::fs::read_to_string(&file)
            && !id.trim().is_empty()
        {
            return Ok(id.trim().to_string());
        }
    }
    Err(Error::NoClientId)
}

/// Scopes we request. `offline_access` is what yields a refresh token, and so
/// what keeps later launches off the network.
pub(crate) const SCOPE: &str = "XboxLive.signin offline_access";

pub(crate) const MS_DEVICE_CODE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
pub(crate) const MS_TOKEN_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
pub(crate) const XBL_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
pub(crate) const XSTS_AUTH_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
pub(crate) const MC_LOGIN_URL: &str =
    "https://api.minecraftservices.com/authentication/login_with_xbox";
pub(crate) const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";
/// Where the client proves to Mojang that it holds the shared secret, before
/// the server checks the same hash.
pub const MC_JOIN_URL: &str = "https://sessionserver.mojang.com/session/minecraft/join";

/// How a session was obtained. Callers report this; the client uses it to keep
/// the interactive path off the startup critical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Read straight from disk and still valid. No network at all.
    Cache,
    /// One round trip to swap a refresh token for a new session.
    Refreshed,
    /// A full browser sign-in happened.
    Interactive,
}

/// Returns a usable session, doing the least work that will produce one.
///
/// The ordering is the whole point: a warm launch hits the `Cache` arm and
/// never opens a socket. `interactive` gates whether we are allowed to block on
/// a human; pass `false` on the startup path so a stale session surfaces as an
/// error to handle in the UI rather than a hang behind a browser prompt.
pub fn authenticate(
    cache: &CachePath,
    interactive: bool,
    mut on_prompt: impl FnMut(&DeviceCode),
) -> Result<(Session, Origin)> {
    if let Some(session) = cache.load() {
        if session.is_valid() {
            return Ok((session, Origin::Cache));
        }
        // Expired, but the refresh token usually outlives it by weeks.
        let flow = DeviceCodeFlow::new(client_id()?);
        match flow.refresh(&session.refresh_token) {
            Ok(fresh) => {
                cache.store(&fresh)?;
                return Ok((fresh, Origin::Refreshed));
            }
            Err(e) if !interactive => return Err(e),
            // Refresh token revoked or too old: fall through to a full sign-in.
            Err(_) => {}
        }
    }

    if !interactive {
        return Err(Error::RefreshRejected);
    }

    let flow = DeviceCodeFlow::new(client_id()?);
    let dc = flow.start()?;
    on_prompt(&dc);
    let session = flow.wait(&dc, |_| {})?;
    cache.store(&session)?;
    Ok((session, Origin::Interactive))
}

/// Tells Mojang we are about to join a server, proving we hold the shared
/// secret.
///
/// The server independently computes the same hash and asks Mojang whether it
/// has seen it. Both calls must happen within a short window, so this is sent
/// between writing the key packet and reading the server's next packet.
///
/// A 204 is success. A 403 means the hash did not match or the session token is
/// stale, which surfaces to the player as "invalid session".
pub fn join_server(access_token: &str, uuid: u128, server_hash: &str) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .user_agent(concat!("neuton/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();

    let profile = Profile { uuid, name: String::new() };
    let mut res = agent.post(MC_JOIN_URL).content_type("application/json").send_json(
        serde_json::json!({
            "accessToken": access_token,
            "selectedProfile": format!("{:032x}", profile.uuid),
            "serverId": server_hash,
        }),
    )?;

    let status = res.status().as_u16();
    if status == 204 || status == 200 {
        return Ok(());
    }
    let body = res.body_mut().read_to_string().unwrap_or_default();
    Err(Error::Upstream {
        stage: "session server join",
        code: status.to_string(),
        message: if body.is_empty() {
            "the session server rejected the join; the session may have expired".into()
        } else {
            body
        },
    })
}
