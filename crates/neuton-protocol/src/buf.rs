//! Zero-copy reading and flat writing of Minecraft's wire types.
//!
//! Every packet in the game funnels through here, so the primitives are written
//! for the common case: a VarInt is one byte ~90% of the time, and strings are
//! borrowed out of the receive buffer rather than copied.

use crate::error::{Error, Result};

/// Protocol cap on any length-prefixed value. Matches vanilla's limit and stops
/// a hostile server from making us allocate gigabytes off a bad VarInt.
pub const MAX_PACKET_LEN: usize = 0x20_0000; // 2 MiB

/// A cursor over a received packet body.
///
/// Borrows its data, so `read_str` hands back a `&str` pointing into the
/// original receive buffer with no allocation.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Bytes not yet consumed.
    #[inline]
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    #[inline]
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Eof {
            needed: n,
            remaining: self.remaining(),
        })?;
        if end > self.buf.len() {
            return Err(Error::Eof { needed: n, remaining: self.remaining() });
        }
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    #[inline]
    pub fn read_u8(&mut self) -> Result<u8> {
        if self.pos >= self.buf.len() {
            return Err(Error::Eof { needed: 1, remaining: 0 });
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    pub fn read_i8(&mut self) -> Result<i8> {
        self.read_u8().map(|b| b as i8)
    }

    #[inline]
    pub fn read_bool(&mut self) -> Result<bool> {
        self.read_u8().map(|b| b != 0)
    }

    /// Reads a VarInt.
    ///
    /// Hand-unrolled rather than looped: the one-byte case is by far the most
    /// common and gets to return without touching a loop counter.
    #[inline]
    pub fn read_varint(&mut self) -> Result<i32> {
        let b = self.read_u8()?;
        if b < 0x80 {
            return Ok(b as i32);
        }
        let mut val = (b & 0x7F) as i32;
        for shift in [7u32, 14, 21, 28] {
            let b = self.read_u8()?;
            val |= ((b & 0x7F) as i32) << shift;
            if b < 0x80 {
                return Ok(val);
            }
        }
        Err(Error::VarIntTooLong)
    }

    /// Reads a VarInt and rejects negatives, for values used as lengths/counts.
    #[inline]
    pub fn read_varint_len(&mut self, max: usize) -> Result<usize> {
        let v = self.read_varint()?;
        let len = v as usize;
        if v < 0 || len > max {
            return Err(Error::TooLarge { len, max });
        }
        Ok(len)
    }

    #[inline]
    pub fn read_varlong(&mut self) -> Result<i64> {
        let mut val: i64 = 0;
        for shift in (0..64).step_by(7) {
            let b = self.read_u8()?;
            val |= ((b & 0x7F) as i64) << shift;
            if b < 0x80 {
                return Ok(val);
            }
        }
        Err(Error::VarIntTooLong)
    }

    #[inline]
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    /// A VarInt-prefixed byte array.
    #[inline]
    pub fn read_byte_array(&mut self) -> Result<&'a [u8]> {
        let n = self.read_varint_len(MAX_PACKET_LEN)?;
        self.take(n)
    }

    /// A VarInt-prefixed UTF-8 string, borrowed from the receive buffer.
    #[inline]
    pub fn read_str(&mut self) -> Result<&'a str> {
        let n = self.read_varint_len(MAX_PACKET_LEN)?;
        let bytes = self.take(n)?;
        core::str::from_utf8(bytes).map_err(|_| Error::BadUtf8)
    }

    #[inline]
    pub fn read_uuid(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.take(16)?.try_into().unwrap()))
    }
}

macro_rules! read_be {
    ($($name:ident -> $ty:ty, $n:literal;)*) => {$(
        impl Reader<'_> {
            #[inline]
            pub fn $name(&mut self) -> Result<$ty> {
                Ok(<$ty>::from_be_bytes(self.take($n)?.try_into().unwrap()))
            }
        }
    )*};
}

read_be! {
    read_u16 -> u16, 2;
    read_i16 -> i16, 2;
    read_u32 -> u32, 4;
    read_i32 -> i32, 4;
    read_u64 -> u64, 8;
    read_i64 -> i64, 8;
    read_f32 -> f32, 4;
    read_f64 -> f64, 8;
}

/// An append-only packet body being built.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    #[inline]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    #[inline]
    pub fn with_capacity(cap: usize) -> Self {
        Self { buf: Vec::with_capacity(cap) }
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    #[inline]
    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    #[inline]
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    #[inline]
    pub fn write_u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    #[inline]
    pub fn write_i8(&mut self, v: i8) -> &mut Self {
        self.write_u8(v as u8)
    }

    #[inline]
    pub fn write_bool(&mut self, v: bool) -> &mut Self {
        self.write_u8(v as u8)
    }

    #[inline]
    pub fn write_varint(&mut self, v: i32) -> &mut Self {
        let mut u = v as u32;
        loop {
            let b = (u & 0x7F) as u8;
            u >>= 7;
            if u == 0 {
                self.buf.push(b);
                return self;
            }
            self.buf.push(b | 0x80);
        }
    }

    #[inline]
    pub fn write_varlong(&mut self, v: i64) -> &mut Self {
        let mut u = v as u64;
        loop {
            let b = (u & 0x7F) as u8;
            u >>= 7;
            if u == 0 {
                self.buf.push(b);
                return self;
            }
            self.buf.push(b | 0x80);
        }
    }

    #[inline]
    pub fn write_bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    #[inline]
    pub fn write_byte_array(&mut self, v: &[u8]) -> &mut Self {
        self.write_varint(v.len() as i32).write_bytes(v)
    }

    #[inline]
    pub fn write_str(&mut self, v: &str) -> &mut Self {
        self.write_byte_array(v.as_bytes())
    }

    #[inline]
    pub fn write_uuid(&mut self, v: u128) -> &mut Self {
        self.write_bytes(&v.to_be_bytes())
    }
}

macro_rules! write_be {
    ($($name:ident($ty:ty);)*) => {$(
        impl Writer {
            #[inline]
            pub fn $name(&mut self, v: $ty) -> &mut Self {
                self.write_bytes(&v.to_be_bytes())
            }
        }
    )*};
}

write_be! {
    write_u16(u16); write_i16(i16);
    write_u32(u32); write_i32(i32);
    write_u64(u64); write_i64(i64);
    write_f32(f32); write_f64(f64);
}

/// Number of bytes `v` occupies as a VarInt. Used to size frame headers without
/// writing them twice.
#[inline]
pub const fn varint_size(v: i32) -> usize {
    match (v as u32).leading_zeros() {
        0..=3 => 5,
        4..=10 => 4,
        11..=17 => 3,
        18..=24 => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip_matches_vanilla_edge_cases() {
        // Values and encodings taken from the protocol's documented test vectors.
        let cases: &[(i32, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (255, &[0xff, 0x01]),
            (25565, &[0xdd, 0xc7, 0x01]),
            (2097151, &[0xff, 0xff, 0x7f]),
            (2147483647, &[0xff, 0xff, 0xff, 0xff, 0x07]),
            (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
            (-2147483648, &[0x80, 0x80, 0x80, 0x80, 0x08]),
        ];
        for &(value, encoded) in cases {
            let mut w = Writer::new();
            w.write_varint(value);
            assert_eq!(w.as_slice(), encoded, "encoding {value}");
            assert_eq!(Reader::new(encoded).read_varint().unwrap(), value, "decoding {value}");
            assert_eq!(varint_size(value), encoded.len(), "size of {value}");
        }
    }

    #[test]
    fn varlong_roundtrip() {
        for v in [0i64, 1, -1, i64::MAX, i64::MIN, 2147483647, -2147483648] {
            let mut w = Writer::new();
            w.write_varlong(v);
            assert_eq!(Reader::new(w.as_slice()).read_varlong().unwrap(), v);
        }
    }

    #[test]
    fn varint_rejects_overlong_encoding() {
        let bad = [0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
        assert_eq!(Reader::new(&bad).read_varint(), Err(Error::VarIntTooLong));
    }

    #[test]
    fn reader_reports_eof_instead_of_panicking() {
        let mut r = Reader::new(&[0x01]);
        assert!(r.read_u8().is_ok());
        assert!(matches!(r.read_u8(), Err(Error::Eof { .. })));
        assert!(matches!(Reader::new(&[]).read_i64(), Err(Error::Eof { .. })));
    }

    #[test]
    fn strings_borrow_from_the_source_buffer() {
        let mut w = Writer::new();
        w.write_str("mijismp");
        let owned = w.into_vec();
        let s = Reader::new(&owned).read_str().unwrap();
        assert_eq!(s, "mijismp");
        // Borrowed, not copied: the &str points into `owned`.
        assert!(s.as_ptr() >= owned.as_ptr());
    }

    #[test]
    fn hostile_length_prefix_is_rejected_before_allocating() {
        // VarInt 0x7fffffff as a string length.
        let bad = [0xff, 0xff, 0xff, 0xff, 0x07];
        assert!(matches!(Reader::new(&bad).read_str(), Err(Error::TooLarge { .. })));
    }
}
