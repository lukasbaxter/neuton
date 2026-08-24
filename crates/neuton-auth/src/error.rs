use core::fmt;

#[derive(Debug)]
pub enum Error {
    /// No Azure application ID configured.
    NoClientId,
    Http(Box<ureq::Error>),
    Io(std::io::Error),
    Json(serde_json::Error),
    /// Microsoft or Xbox returned a structured failure.
    Upstream { stage: &'static str, code: String, message: String },
    /// The account cannot play: no Xbox profile, a child account, or no game
    /// licence. These need a human, not a retry.
    NotEntitled(String),
    /// The user did not finish the browser step in time.
    DeviceCodeExpired,
    /// Microsoft and Xbox accepted the sign-in, but Mojang has not approved
    /// this Azure application for the Minecraft services API.
    ///
    /// Distinct from the other failures because nothing the user does can fix
    /// it: it is a one-time review of the application itself.
    AppNotApproved,
    /// The cached refresh token is no longer accepted.
    RefreshRejected,
}

impl Error {
    /// Whether retrying could plausibly succeed. Callers use this to decide
    /// between falling back to an interactive login and giving up.
    pub fn is_recoverable(&self) -> bool {
        // AppNotApproved is deliberately absent: retrying it can never help.
        matches!(self, Error::RefreshRejected | Error::DeviceCodeExpired | Error::Http(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Only reachable in a build that was compiled without one, so the
            // audience here is whoever built it, not the player.
            Error::NoClientId => f.write_str(
                "this build has no Microsoft application ID compiled in, so it cannot sign in\n\
                 release builds set NEUTON_CLIENT_ID at build time\n\
                 to use your own: export NEUTON_CLIENT_ID=<id>, or write it to the\n\
                 client_id file in the config directory. see docs/AUTH.md",
            ),
            Error::Http(e) => write!(f, "http: {e}"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Json(e) => write!(f, "malformed json from the auth service: {e}"),
            Error::Upstream { stage, code, message } => {
                write!(f, "{stage} rejected the request ({code}): {message}")
            }
            Error::NotEntitled(why) => write!(f, "{why}"),
            Error::DeviceCodeExpired => f.write_str("sign-in timed out before it was approved"),
            Error::AppNotApproved => f.write_str(
                "Mojang has not approved this application for Minecraft sign-in\n\
                 \n\
                 the Microsoft and Xbox Live steps succeeded; only the final\n\
                 Minecraft services call was refused. third-party launchers must\n\
                 have their Azure application reviewed once before it can be used:\n\
                 \n\
                     https://aka.ms/mce-reviewappid\n\
                 \n\
                 approval applies to the application, not to any account, so this\n\
                 affects every user of this build until it is granted",
            ),
            Error::RefreshRejected => f.write_str("the saved sign-in expired and must be redone"),
        }
    }
}

impl core::error::Error for Error {}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Error::Http(Box::new(e))
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;
