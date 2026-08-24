use crate::TagType;
use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Eof { needed: usize, remaining: usize },
    BadTagType(u8),
    /// A list declared a length but its element type was END.
    BadListType,
    BadUtf8,
    TooLarge { len: usize, max: usize },
    TooDeep,
    /// Asked for a field as the wrong type.
    TypeMismatch { expected: TagType, found: TagType },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Eof { needed, remaining } => {
                write!(f, "unexpected end of nbt: needed {needed}, {remaining} remaining")
            }
            Error::BadTagType(t) => write!(f, "unknown nbt tag type {t}"),
            Error::BadListType => f.write_str("non-empty nbt list with element type END"),
            Error::BadUtf8 => f.write_str("invalid modified utf-8 in nbt string"),
            Error::TooLarge { len, max } => write!(f, "nbt length {len} exceeds maximum {max}"),
            Error::TooDeep => f.write_str("nbt nested deeper than the limit"),
            Error::TypeMismatch { expected, found } => {
                write!(f, "expected nbt {} but found {}", expected.name(), found.name())
            }
        }
    }
}

impl core::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;
