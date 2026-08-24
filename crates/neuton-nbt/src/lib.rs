//! NBT, in the "network" framing used from 1.20.2 onward.
//!
//! Two access patterns matter here and they have very different costs:
//!
//! - Registry data during configuration is read once per connection. Convenience
//!   wins, so [`Value`] materialises a tree.
//! - Chunk packets carry NBT (heightmaps, block entities) that the renderer
//!   mostly does not care about, arriving thousands of times a second. Walking
//!   it into a tree just to drop it would be pure waste, so [`skip`] steps over
//!   a tag without allocating anything at all.

use std::borrow::Cow;

mod error;
mod read;
mod value;

pub use error::{Error, Result};
pub use read::{skip, skip_payload};
pub use value::{Value, skip_network};

/// NBT tag discriminants, as they appear on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TagType {
    End = 0,
    Byte = 1,
    Short = 2,
    Int = 3,
    Long = 4,
    Float = 5,
    Double = 6,
    ByteArray = 7,
    String = 8,
    List = 9,
    Compound = 10,
    IntArray = 11,
    LongArray = 12,
}

impl TagType {
    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => TagType::End,
            1 => TagType::Byte,
            2 => TagType::Short,
            3 => TagType::Int,
            4 => TagType::Long,
            5 => TagType::Float,
            6 => TagType::Double,
            7 => TagType::ByteArray,
            8 => TagType::String,
            9 => TagType::List,
            10 => TagType::Compound,
            11 => TagType::IntArray,
            12 => TagType::LongArray,
            _ => return None,
        })
    }

    pub const fn name(self) -> &'static str {
        match self {
            TagType::End => "END",
            TagType::Byte => "BYTE",
            TagType::Short => "SHORT",
            TagType::Int => "INT",
            TagType::Long => "LONG",
            TagType::Float => "FLOAT",
            TagType::Double => "DOUBLE",
            TagType::ByteArray => "BYTE_ARRAY",
            TagType::String => "STRING",
            TagType::List => "LIST",
            TagType::Compound => "COMPOUND",
            TagType::IntArray => "INT_ARRAY",
            TagType::LongArray => "LONG_ARRAY",
        }
    }
}

/// An NBT string.
///
/// NBT uses Java's modified UTF-8, which differs from real UTF-8 only for NUL
/// and for characters outside the BMP. Those are vanishingly rare in practice,
/// so the common case borrows straight out of the packet buffer and only the
/// awkward case allocates.
pub type NbtStr<'a> = Cow<'a, str>;

/// Guard against a hostile or corrupt server making us allocate without bound.
/// Vanilla's own network NBT limit is far below this.
pub const MAX_ELEMENTS: usize = 1 << 24;

/// Deepest nesting we will follow, matching vanilla's limit. Without this a
/// crafted payload of nothing but list-of-list would blow the stack.
pub const MAX_DEPTH: u32 = 512;
