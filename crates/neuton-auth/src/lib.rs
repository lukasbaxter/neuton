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

mod accounts;
mod error;
mod flow;
mod session;

pub use accounts::{Accounts, config_dir};
pub use error::{Error, Result};
pub use flow::{DeviceCode, DeviceCodeFlow};
pub use session::{Profile, Session};

/// Azure application (client) ID this build signs in with.
///
/// Baked in at compile time from `NEUTON_CLIENT_ID`. Shipping one ID that
/// belongs to the project is how every launcher does this, and it is what makes
/// the client usable by people who are not its developer: they sign in with
/// their own Microsoft account and never touch Azure.
///
/// This is a **public** OAuth client. There is no client secret and none is
/// possible, so the ID is an identifier rather than a credential and is safe in
/// a public repository. It is not a licence check and grants nothing on its
/// own; every user still authenticates as themselves and must own the game.
const BUILT_IN_CLIENT_ID: Option<&str> = option_env!("NEUTON_CLIENT_ID");

/// Resolves the client ID, most specific source first.
///
/// The overrides exist so a contributor can point a local build at their own
/// app registration without rebuilding the released one.
pub fn client_id() -> Result<String> {
    if let Ok(id) = std::env::var("NEUTON_CLIENT_ID")
        && !id.trim().is_empty()
    {
        return Ok(id.trim().to_string());
    }
    if let Some(path) = accounts::config_dir() {
        if let Ok(id) = std::fs::read_to_string(path.join("client_id"))
            && !id.trim().is_empty()
        {
            return Ok(id.trim().to_string());
        }
    }
    match BUILT_IN_CLIENT_ID {
        Some(id) if !id.trim().is_empty() => Ok(id.trim().to_string()),
        _ => Err(Error::NoClientId),
    }
}

/// Whether this build shipped with an ID, i.e. whether a user can just sign in.
pub fn has_built_in_client_id() -> bool {
    BUILT_IN_CLIENT_ID.is_some_and(|id| !id.trim().is_empty())
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

/// Returns a usable session for the active account, doing the least work that
/// will produce one.
///
/// The ordering is the whole point: a warm launch hits the `Cache` arm and
/// never opens a socket. `interactive` gates whether we may block on a human;
/// pass `false` on the startup path so a stale sign-in surfaces as an error the
/// UI can handle rather than a hang behind a browser prompt.
pub fn authenticate(
    accounts: &mut Accounts,
    interactive: bool,
    on_prompt: impl FnMut(&DeviceCode),
) -> Result<(Session, Origin)> {
    if let Some(session) = accounts.active().cloned() {
        if session.is_valid() {
            return Ok((session, Origin::Cache));
        }
        // Expired, but the refresh token usually outlives it by weeks.
        let flow = DeviceCodeFlow::new(client_id()?);
        match flow.refresh(&session.refresh_token) {
            Ok(fresh) => {
                accounts.upsert(fresh.clone());
                accounts.save()?;
                return Ok((fresh, Origin::Refreshed));
            }
            Err(e) if !interactive => return Err(e),
            // Revoked or too old: fall through to a full sign-in.
            Err(_) => {}
        }
    }

    if !interactive {
        return Err(Error::RefreshRejected);
    }
    let session = sign_in(accounts, on_prompt)?;
    Ok((session, Origin::Interactive))
}

/// Runs a full interactive sign-in and adds the result to the store.
///
/// Used directly when adding a second account, where falling back to an already
/// cached one would be wrong.
pub fn sign_in(
    accounts: &mut Accounts,
    mut on_prompt: impl FnMut(&DeviceCode),
) -> Result<Session> {
    let flow = DeviceCodeFlow::new(client_id()?);
    let dc = flow.start()?;
    on_prompt(&dc);
    let session = flow.wait(&dc, |_| {})?;
    accounts.upsert(session.clone());
    accounts.save()?;
    Ok(session)
}

/// Opens the verification page in the user's browser.
///
/// Best effort: the code is always printed as well, because this fails on
/// headless machines and inside some sandboxes, and a sign-in that depends on a
/// browser launching is a sign-in that breaks for someone.
pub fn open_browser(url: &str) -> bool {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(windows) {
        ("cmd", &["/C", "start", ""])
    } else {
        ("xdg-open", &[])
    };
    std::process::Command::new(program)
        .args(args)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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

    let mut res = agent.post(MC_JOIN_URL).content_type("application/json").send_json(
        serde_json::json!({
            "accessToken": access_token,
            "selectedProfile": format!("{uuid:032x}"),
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
