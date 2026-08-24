//! Minecraft: Java Edition wire protocol, version 26.2 (protocol 776).
//!
//! Server-play only: there is no world generation, no save format and no
//! integrated server here. Everything is either a packet on a socket or a table
//! generated from the vanilla jar.

pub mod buf;
pub mod codec;
pub mod crypto;
pub mod error;

mod generated;

pub use buf::{MAX_PACKET_LEN, Reader, Writer, varint_size};
pub use codec::Framed;
pub use error::{Error, Result};
pub use generated::ids;

/// Protocol version this build speaks. Read out of `version.json` in the
/// vanilla 26.2 jar by the datagen step.
pub const PROTOCOL_VERSION: i32 = ids::PROTOCOL_VERSION;

/// Game version string sent in the handshake.
pub const GAME_VERSION: &str = ids::GAME_VERSION;

/// Which phase of the connection we are in. The same packet ID means different
/// things in each, so decoding is always state-qualified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
}

impl State {
    pub const fn name(self) -> &'static str {
        match self {
            State::Handshake => "handshake",
            State::Status => "status",
            State::Login => "login",
            State::Configuration => "configuration",
            State::Play => "play",
        }
    }
}
