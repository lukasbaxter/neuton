//! Sign-in, off the UI thread.
//!
//! The device code flow blocks for as long as the user takes to open a browser
//! and type a code, which can be minutes. Running it on the UI thread would
//! freeze the window for that whole time, so it runs on its own thread and
//! reports progress through a channel the UI drains once per frame.

use neuton_auth::{Accounts, DeviceCode, DeviceCodeFlow, Session};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// Progress updates from the sign-in thread.
enum Msg {
    Prompt { code: String, url: String },
    Done(Box<Session>),
    Failed(String),
}

/// What the UI should currently draw for sign-in.
#[derive(Debug, Clone, PartialEq)]
pub enum SignIn {
    /// Nothing in progress.
    Idle,
    /// Asking Microsoft for a code.
    Starting,
    /// Waiting for the user to approve in a browser.
    Waiting { code: String, url: String },
    /// Finished; the account store has already been updated.
    Done { name: String },
    Failed(String),
}

pub struct SignInTask {
    rx: Option<Receiver<Msg>>,
    pub state: SignIn,
}

impl Default for SignInTask {
    fn default() -> Self {
        Self { rx: None, state: SignIn::Idle }
    }
}

impl SignInTask {
    pub fn is_running(&self) -> bool {
        matches!(self.state, SignIn::Starting | SignIn::Waiting { .. })
    }

    /// Starts a sign-in. Does nothing if one is already in flight.
    pub fn start(&mut self, accounts_path: std::path::PathBuf) {
        if self.is_running() {
            return;
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.state = SignIn::Starting;

        std::thread::spawn(move || {
            let client_id = match neuton_auth::client_id() {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.send(Msg::Failed(e.to_string()));
                    return;
                }
            };
            let flow = DeviceCodeFlow::new(client_id);
            let dc: DeviceCode = match flow.start() {
                Ok(dc) => dc,
                Err(e) => {
                    let _ = tx.send(Msg::Failed(e.to_string()));
                    return;
                }
            };
            let _ = tx.send(Msg::Prompt {
                code: dc.user_code.clone(),
                url: dc.verification_uri.clone(),
            });
            neuton_auth::open_browser(&dc.verification_uri);

            match flow.wait(&dc, |_| {}) {
                Ok(session) => {
                    // Re-read the store rather than holding it across the wait:
                    // the user may have removed an account while this ran.
                    let mut store = Accounts::load(accounts_path);
                    store.upsert(session.clone());
                    if let Err(e) = store.save() {
                        let _ = tx.send(Msg::Failed(format!("signed in but could not save: {e}")));
                        return;
                    }
                    let _ = tx.send(Msg::Done(Box::new(session)));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(e.to_string()));
                }
            }
        });
    }

    /// Drains the channel. Returns true if the account store changed and the
    /// UI should reload it.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        let mut reload = false;
        loop {
            match rx.try_recv() {
                Ok(Msg::Prompt { code, url }) => self.state = SignIn::Waiting { code, url },
                Ok(Msg::Done(session)) => {
                    self.state = SignIn::Done { name: session.profile.name.clone() };
                    reload = true;
                }
                Ok(Msg::Failed(why)) => self.state = SignIn::Failed(why),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.rx = None;
                    break;
                }
            }
        }
        reload
    }

    pub fn dismiss(&mut self) {
        if !self.is_running() {
            self.state = SignIn::Idle;
            self.rx = None;
        }
    }
}
