//! Joining and staying joined to a server.
//!
//! Owns the login handshake, encryption, the configuration exchange and the
//! play loop. Everything here is blocking on a single socket by design: a game
//! client has exactly one connection, and a dedicated thread doing blocking
//! reads has lower and steadier latency than scheduling through an async
//! runtime.

pub mod connection;
pub mod dns;
pub mod registries;
pub mod status;

pub use connection::{Connection, Error, Event, Stats};
pub use registries::{BiomeColors, DimensionShape, GrassModifier, Registries};
pub use dns::{Resolution, Srv};
pub use status::{ServerStatus, Span, ping};
