//! The byte cursor, modified-UTF-8 decoding, and the allocation-free skipper.

use crate::{Error, MAX_DEPTH, MAX_ELEMENTS, NbtStr, Result, TagType};
use std::borrow::Cow;

/// A cursor over NBT bytes.
///
/// Kept separate from `neuton_protocol::Reader` so this crate stays a leaf with
/// no dependencies; `neuton-protocol` depends on it, not the other way round.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    #[inline]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub const fn position(&self) -> usize {
        self.pos
    }

    #[inline]
    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    #[inline]
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    #[inline]
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len()).ok_or(Error::Eof {
            needed: n,
            remaining: self.remaining(),
        })?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    #[inline]
    pub fn u8(&mut self) -> Result<u8> {
        let b = *self.buf.get(self.pos).ok_or(Error::Eof { needed: 1, remaining: 0 })?;
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    pub fn i8(&mut self) -> Result<i8> {
        self.u8().map(|b| b as i8)
    }

    #[inline]
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    #[inline]
    pub fn i16(&mut self) -> Result<i16> {
        self.u16().map(|v| v as i16)
    }

    #[inline]
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    #[inline]
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    #[inline]
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    #[inline]
    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// Reads a length prefix that must be a sane, non-negative count.
    #[inline]
    pub fn count(&mut self) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 || n as usize > MAX_ELEMENTS {
            return Err(Error::TooLarge { len: n as usize, max: MAX_ELEMENTS });
        }
        Ok(n as usize)
    }

    #[inline]
    pub fn tag_type(&mut self) -> Result<TagType> {
        let b = self.u8()?;
        TagType::from_u8(b).ok_or(Error::BadTagType(b))
    }

    /// Reads a `u16`-prefixed modified-UTF-8 string.
    pub fn nbt_str(&mut self) -> Result<NbtStr<'a>> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        decode_modified_utf8(bytes)
    }

    /// Skips a string without decoding it.
    #[inline]
    pub fn skip_str(&mut self) -> Result<()> {
        let len = self.u16()? as usize;
        self.take(len).map(|_| ())
    }
}

/// Decodes Java's modified UTF-8.
///
/// Modified UTF-8 differs from the real thing in exactly two ways: NUL is
/// encoded as the overlong `C0 80`, and astral characters are written as a
/// CESU-8 surrogate pair. Both forms are *invalid* UTF-8, so if the standard
/// validator accepts the bytes there is nothing unusual in them and we can
/// borrow with no copy. Only the rejected case pays for a decode.
fn decode_modified_utf8(bytes: &[u8]) -> Result<NbtStr<'_>> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok(Cow::Borrowed(s));
    }
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            0x01..=0x7F => {
                out.push(b as char);
                i += 1;
            }
            0xC0..=0xDF => {
                let b1 = *bytes.get(i + 1).ok_or(Error::BadUtf8)?;
                let cp = (((b & 0x1F) as u32) << 6) | (b1 & 0x3F) as u32;
                out.push(char::from_u32(cp).ok_or(Error::BadUtf8)?);
                i += 2;
            }
            0xE0..=0xEF => {
                let b1 = *bytes.get(i + 1).ok_or(Error::BadUtf8)?;
                let b2 = *bytes.get(i + 2).ok_or(Error::BadUtf8)?;
                let cp = (((b & 0x0F) as u32) << 12)
                    | (((b1 & 0x3F) as u32) << 6)
                    | (b2 & 0x3F) as u32;
                // A high surrogate must be followed by its low surrogate,
                // encoded as a second three-byte group.
                if (0xD800..0xDC00).contains(&cp) {
                    let c1 = *bytes.get(i + 3).ok_or(Error::BadUtf8)?;
                    let c2 = *bytes.get(i + 4).ok_or(Error::BadUtf8)?;
                    let c3 = *bytes.get(i + 5).ok_or(Error::BadUtf8)?;
                    if c1 & 0xF0 != 0xE0 {
                        return Err(Error::BadUtf8);
                    }
                    let low = (((c1 & 0x0F) as u32) << 12)
                        | (((c2 & 0x3F) as u32) << 6)
                        | (c3 & 0x3F) as u32;
                    if !(0xDC00..0xE000).contains(&low) {
                        return Err(Error::BadUtf8);
                    }
                    let combined = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                    out.push(char::from_u32(combined).ok_or(Error::BadUtf8)?);
                    i += 6;
                } else {
                    out.push(char::from_u32(cp).ok_or(Error::BadUtf8)?);
                    i += 3;
                }
            }
            // 0x00 never appears on its own, and 0xF0.. is not valid here.
            _ => return Err(Error::BadUtf8),
        }
    }
    Ok(Cow::Owned(out))
}

/// Skips one *named* tag: type byte, name, then payload.
///
/// Returns the tag type that was skipped, or `None` for TAG_End.
pub fn skip(c: &mut Cursor<'_>) -> Result<Option<TagType>> {
    let ty = c.tag_type()?;
    if ty == TagType::End {
        return Ok(None);
    }
    c.skip_str()?;
    skip_payload(c, ty, 0)?;
    Ok(Some(ty))
}

/// Skips a tag payload, the type having already been read.
///
/// This is the hot path for chunk packets, which is why it never allocates and
/// never builds a [`crate::Value`]. Fixed-width payloads become a single
/// pointer bump.
pub fn skip_payload(c: &mut Cursor<'_>, ty: TagType, depth: u32) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::TooDeep);
    }
    match ty {
        TagType::End => {}
        TagType::Byte => drop(c.take(1)?),
        TagType::Short => drop(c.take(2)?),
        TagType::Int | TagType::Float => drop(c.take(4)?),
        TagType::Long | TagType::Double => drop(c.take(8)?),
        TagType::ByteArray => {
            let n = c.count()?;
            c.take(n)?;
        }
        TagType::IntArray => {
            let n = c.count()?;
            c.take(n.checked_mul(4).ok_or(Error::TooLarge { len: n, max: MAX_ELEMENTS })?)?;
        }
        TagType::LongArray => {
            let n = c.count()?;
            c.take(n.checked_mul(8).ok_or(Error::TooLarge { len: n, max: MAX_ELEMENTS })?)?;
        }
        TagType::String => c.skip_str()?,
        TagType::List => {
            let elem = c.tag_type()?;
            let n = c.count()?;
            if n > 0 && elem == TagType::End {
                return Err(Error::BadListType);
            }
            // Uniform width: skip the whole run in one step instead of n steps.
            if let Some(width) = fixed_width(elem) {
                c.take(n.checked_mul(width).ok_or(Error::TooLarge { len: n, max: MAX_ELEMENTS })?)?;
            } else {
                for _ in 0..n {
                    skip_payload(c, elem, depth + 1)?;
                }
            }
        }
        TagType::Compound => loop {
            let ty = c.tag_type()?;
            if ty == TagType::End {
                break;
            }
            c.skip_str()?;
            skip_payload(c, ty, depth + 1)?;
        },
    }
    Ok(())
}

/// Byte width of a tag whose payload is fixed size, else `None`.
#[inline]
const fn fixed_width(ty: TagType) -> Option<usize> {
    Some(match ty {
        TagType::Byte => 1,
        TagType::Short => 2,
        TagType::Int | TagType::Float => 4,
        TagType::Long | TagType::Double => 8,
        _ => return None,
    })
}
