use core::fmt;

/// Everything that can go wrong decoding or encoding a packet.
///
/// Deliberately small and `Copy`-cheap: decode errors happen on a hot path and
/// must never allocate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Ran off the end of the buffer while reading.
    Eof { needed: usize, remaining: usize },
    /// A VarInt/VarLong used more continuation bytes than its width allows.
    VarIntTooLong,
    /// A length prefix exceeded the protocol's hard cap.
    TooLarge { len: usize, max: usize },
    /// Bytes that should have been UTF-8 were not.
    BadUtf8,
    /// A discriminant did not match any known variant.
    BadEnum { got: i32 },
    /// A packet ID we have no decoder for, in the given state.
    UnknownPacket { state: &'static str, id: i32 },
    /// Key exchange or cipher setup failed.
    Crypto(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Eof { needed, remaining } => {
                write!(f, "unexpected end of buffer: needed {needed}, {remaining} remaining")
            }
            Error::VarIntTooLong => f.write_str("varint too long"),
            Error::TooLarge { len, max } => write!(f, "length {len} exceeds maximum {max}"),
            Error::BadUtf8 => f.write_str("invalid utf-8"),
            Error::BadEnum { got } => write!(f, "invalid enum discriminant {got}"),
            Error::UnknownPacket { state, id } => {
                write!(f, "unknown packet id {id:#04x} in state {state}")
            }
            Error::Crypto(msg) => write!(f, "encryption failed: {msg}"),
        }
    }
}

impl core::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;
