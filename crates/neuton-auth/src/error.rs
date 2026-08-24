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
    /// The cached refresh token is no longer accepted.
    RefreshRejected,
}

impl Error {
    /// Whether retrying could plausibly succeed. Callers use this to decide
    /// between falling back to an interactive login and giving up.
    pub fn is_recoverable(&self) -> bool {
        matches!(self, Error::RefreshRejected | Error::DeviceCodeExpired | Error::Http(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoClientId => f.write_str(
                "no Azure client ID configured\n\
                 set NEUTON_CLIENT_ID, or write one to the client_id file in the config directory\n\
                 see docs/AUTH.md",
            ),
            Error::Http(e) => write!(f, "http: {e}"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Json(e) => write!(f, "malformed json from the auth service: {e}"),
            Error::Upstream { stage, code, message } => {
                write!(f, "{stage} rejected the request ({code}): {message}")
            }
            Error::NotEntitled(why) => write!(f, "{why}"),
            Error::DeviceCodeExpired => f.write_str("sign-in timed out before it was approved"),
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
