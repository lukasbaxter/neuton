//! A materialised NBT tree, for data that is read once and then queried.

use crate::read::{Cursor, skip_payload};
use crate::{Error, MAX_DEPTH, NbtStr, Result, TagType};

/// One NBT tag with its payload.
///
/// Byte arrays and strings borrow from the source buffer. Int and long arrays
/// cannot: they are big-endian on the wire and need byte-swapping on every
/// platform we target, so they are decoded eagerly.
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(&'a [u8]),
    String(NbtStr<'a>),
    List(Vec<Value<'a>>),
    /// Field order is preserved and lookup is a linear scan.
    ///
    /// Registry compounds have a handful of keys; scanning a small `Vec` beats
    /// hashing, and it avoids pulling a map into the hot configuration path.
    Compound(Vec<(NbtStr<'a>, Value<'a>)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl<'a> Value<'a> {
    /// Parses network NBT: a type byte followed by an *unnamed* payload.
    ///
    /// This is the framing used from 1.20.2 onward. The older named-root form
    /// does not appear on the wire any more.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let mut c = Cursor::new(bytes);
        Self::parse_from(&mut c)
    }

    /// Like [`Value::parse`] but reports how many bytes were consumed, for
    /// packets that carry NBT followed by more fields.
    pub fn parse_prefix(bytes: &'a [u8]) -> Result<(Self, usize)> {
        let mut c = Cursor::new(bytes);
        let v = Self::parse_from(&mut c)?;
        Ok((v, c.position()))
    }

    fn parse_from(c: &mut Cursor<'a>) -> Result<Self> {
        let ty = c.tag_type()?;
        if ty == TagType::End {
            return Ok(Value::Compound(Vec::new()));
        }
        Self::payload(c, ty, 0)
    }

    fn payload(c: &mut Cursor<'a>, ty: TagType, depth: u32) -> Result<Self> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        Ok(match ty {
            TagType::End => Value::Compound(Vec::new()),
            TagType::Byte => Value::Byte(c.i8()?),
            TagType::Short => Value::Short(c.i16()?),
            TagType::Int => Value::Int(c.i32()?),
            TagType::Long => Value::Long(c.i64()?),
            TagType::Float => Value::Float(c.f32()?),
            TagType::Double => Value::Double(c.f64()?),
            TagType::ByteArray => {
                let n = c.count()?;
                Value::ByteArray(c.take(n)?)
            }
            TagType::String => Value::String(c.nbt_str()?),
            TagType::IntArray => {
                let n = c.count()?;
                let raw = c.take(n.checked_mul(4).ok_or(Error::TooLarge { len: n, max: 0 })?)?;
                Value::IntArray(
                    raw.chunks_exact(4).map(|b| i32::from_be_bytes(b.try_into().unwrap())).collect(),
                )
            }
            TagType::LongArray => {
                let n = c.count()?;
                let raw = c.take(n.checked_mul(8).ok_or(Error::TooLarge { len: n, max: 0 })?)?;
                Value::LongArray(
                    raw.chunks_exact(8).map(|b| i64::from_be_bytes(b.try_into().unwrap())).collect(),
                )
            }
            TagType::List => {
                let elem = c.tag_type()?;
                let n = c.count()?;
                if n > 0 && elem == TagType::End {
                    return Err(Error::BadListType);
                }
                // Do not pre-allocate from the wire's own length field: a
                // crafted header could ask for millions of entries in a packet
                // only a few bytes long. Grow as elements actually decode.
                let mut items = Vec::new();
                for _ in 0..n {
                    items.push(Self::payload(c, elem, depth + 1)?);
                }
                Value::List(items)
            }
            TagType::Compound => {
                let mut fields = Vec::new();
                loop {
                    let ty = c.tag_type()?;
                    if ty == TagType::End {
                        break;
                    }
                    let name = c.nbt_str()?;
                    fields.push((name, Self::payload(c, ty, depth + 1)?));
                }
                Value::Compound(fields)
            }
        })
    }

    pub const fn tag_type(&self) -> TagType {
        match self {
            Value::Byte(_) => TagType::Byte,
            Value::Short(_) => TagType::Short,
            Value::Int(_) => TagType::Int,
            Value::Long(_) => TagType::Long,
            Value::Float(_) => TagType::Float,
            Value::Double(_) => TagType::Double,
            Value::ByteArray(_) => TagType::ByteArray,
            Value::String(_) => TagType::String,
            Value::List(_) => TagType::List,
            Value::Compound(_) => TagType::Compound,
            Value::IntArray(_) => TagType::IntArray,
            Value::LongArray(_) => TagType::LongArray,
        }
    }

    /// Looks a field up in a compound. `None` for a non-compound or a missing key.
    pub fn get(&self, key: &str) -> Option<&Value<'a>> {
        match self {
            Value::Compound(fields) => {
                fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            }
            _ => None,
        }
    }

    /// Follows a chain of compound keys.
    pub fn path(&self, keys: &[&str]) -> Option<&Value<'a>> {
        keys.iter().try_fold(self, |v, k| v.get(k))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    /// Any integral tag widened to `i64`, since NBT authors are inconsistent
    /// about whether a small number is a byte, short, int or long.
    pub fn as_i64(&self) -> Option<i64> {
        Some(match self {
            Value::Byte(v) => *v as i64,
            Value::Short(v) => *v as i64,
            Value::Int(v) => *v as i64,
            Value::Long(v) => *v,
            _ => return None,
        })
    }

    pub fn as_i32(&self) -> Option<i32> {
        self.as_i64().and_then(|v| i32::try_from(v).ok())
    }

    pub fn as_f64(&self) -> Option<f64> {
        Some(match self {
            Value::Float(v) => *v as f64,
            Value::Double(v) => *v,
            _ => return None,
        })
    }

    /// NBT has no boolean; vanilla writes a byte and treats non-zero as true.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Byte(v) => Some(*v != 0),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value<'a>]> {
        match self {
            Value::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_compound(&self) -> Option<&[(NbtStr<'a>, Value<'a>)]> {
        match self {
            Value::Compound(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_long_array(&self) -> Option<&[i64]> {
        match self {
            Value::LongArray(v) => Some(v),
            _ => None,
        }
    }
}

/// Skips a network-NBT value and returns how many bytes it occupied.
///
/// Used by packet decoders that must step over NBT they do not need.
pub fn skip_network(bytes: &[u8]) -> Result<usize> {
    let mut c = Cursor::new(bytes);
    let ty = c.tag_type()?;
    if ty != TagType::End {
        skip_payload(&mut c, ty, 0)?;
    }
    Ok(c.position())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds network NBT by hand so the tests do not depend on a writer.
    fn compound(fields: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut v = vec![TagType::Compound as u8];
        for (name, body) in fields {
            v.extend_from_slice(body);
            let _ = name;
        }
        v.push(TagType::End as u8);
        v
    }

    fn named(ty: TagType, name: &str, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![ty as u8];
        v.extend_from_slice(&(name.len() as u16).to_be_bytes());
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_a_flat_compound() {
        let body = compound(&[
            ("a", named(TagType::Int, "a", &42i32.to_be_bytes())),
            ("b", named(TagType::String, "b", &{
                let mut s = (5u16).to_be_bytes().to_vec();
                s.extend_from_slice(b"hello");
                s
            })),
        ]);
        let v = Value::parse(&body).unwrap();
        assert_eq!(v.get("a").unwrap().as_i32(), Some(42));
        assert_eq!(v.get("b").unwrap().as_str(), Some("hello"));
        assert_eq!(v.get("missing"), None);
    }

    #[test]
    fn nested_paths_resolve() {
        let inner = {
            let mut v = vec![TagType::Compound as u8];
            v.extend_from_slice(&(5u16).to_be_bytes());
            v.extend_from_slice(b"inner");
            v.extend_from_slice(&named(TagType::Long, "deep", &7i64.to_be_bytes()));
            v.push(TagType::End as u8);
            v
        };
        let mut body = vec![TagType::Compound as u8];
        body.extend_from_slice(&inner);
        body.push(TagType::End as u8);

        let v = Value::parse(&body).unwrap();
        assert_eq!(v.path(&["inner", "deep"]).unwrap().as_i64(), Some(7));
        assert_eq!(v.path(&["inner", "nope"]), None);
        assert_eq!(v.path(&["nope", "deep"]), None);
    }

    #[test]
    fn skip_covers_exactly_what_parse_consumes() {
        // A compound holding one of every awkward tag, plus trailing bytes that
        // must survive untouched.
        let mut inner = vec![TagType::Compound as u8];
        inner.extend_from_slice(&(1u16).to_be_bytes());
        inner.extend_from_slice(b"c");
        inner.extend_from_slice(&named(TagType::Double, "d", &1.5f64.to_be_bytes()));
        inner.extend_from_slice(&named(TagType::LongArray, "l", &{
            let mut b = 2i32.to_be_bytes().to_vec();
            b.extend_from_slice(&9i64.to_be_bytes());
            b.extend_from_slice(&(-9i64).to_be_bytes());
            b
        }));
        inner.push(TagType::End as u8);

        let mut body = vec![TagType::Compound as u8];
        body.extend_from_slice(&inner);
        body.push(TagType::End as u8);
        let nbt_len = body.len();
        body.extend_from_slice(b"TRAILING");

        let (v, consumed) = Value::parse_prefix(&body).unwrap();
        assert_eq!(consumed, nbt_len);
        assert_eq!(skip_network(&body).unwrap(), nbt_len);
        assert_eq!(v.path(&["c", "d"]).unwrap().as_f64(), Some(1.5));
        assert_eq!(v.path(&["c", "l"]).unwrap().as_long_array(), Some(&[9i64, -9][..]));
        assert_eq!(&body[consumed..], b"TRAILING");
    }

    #[test]
    fn list_of_compounds_round_trips() {
        // TAG_List of 2 compounds, each { id: <int> }.
        let mut list = vec![TagType::List as u8];
        list.extend_from_slice(&(4u16).to_be_bytes());
        list.extend_from_slice(b"list");
        list.push(TagType::Compound as u8);
        list.extend_from_slice(&2i32.to_be_bytes());
        for id in [1i32, 2] {
            list.extend_from_slice(&named(TagType::Int, "id", &id.to_be_bytes()));
            list.push(TagType::End as u8);
        }

        let mut body = vec![TagType::Compound as u8];
        body.extend_from_slice(&list);
        body.push(TagType::End as u8);

        let v = Value::parse(&body).unwrap();
        let items = v.get("list").unwrap().as_list().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].get("id").unwrap().as_i32(), Some(2));
        assert_eq!(skip_network(&body).unwrap(), body.len());
    }

    #[test]
    fn hostile_list_length_does_not_preallocate() {
        // Claims 0x7fffff compounds but the buffer ends immediately.
        let mut body = vec![TagType::List as u8];
        body.extend_from_slice(&(1u16).to_be_bytes());
        body.extend_from_slice(b"x");
        body.push(TagType::Compound as u8);
        body.extend_from_slice(&0x007f_ffffi32.to_be_bytes());
        // Wrap it so parse sees a root compound.
        let mut root = vec![TagType::Compound as u8];
        root.extend_from_slice(&body);
        root.push(TagType::End as u8);
        assert!(matches!(Value::parse(&root), Err(Error::Eof { .. })));
    }

    #[test]
    fn deeply_nested_lists_are_rejected_rather_than_overflowing_the_stack() {
        // Root compound holding one field "x" that is a list of lists of lists,
        // nested past the limit. Each level is `[elem_type=LIST][count=1]`, and
        // the innermost is `[elem_type=END][count=0]`.
        let depth = MAX_DEPTH as usize + 8;
        let mut body = vec![TagType::Compound as u8];
        body.push(TagType::List as u8);
        body.extend_from_slice(&(1u16).to_be_bytes());
        body.push(b'x');
        for _ in 0..depth {
            body.push(TagType::List as u8);
            body.extend_from_slice(&1i32.to_be_bytes());
        }
        body.push(TagType::End as u8);
        body.extend_from_slice(&0i32.to_be_bytes());
        body.push(TagType::End as u8);

        assert_eq!(Value::parse(&body), Err(Error::TooDeep));
        // The allocation-free skipper must refuse it too, not just the parser.
        assert_eq!(skip_network(&body), Err(Error::TooDeep));
    }

    #[test]
    fn modified_utf8_nul_and_astral_chars_decode() {
        // C0 80 is modified UTF-8 for NUL; it is not valid standard UTF-8.
        let mut payload = (2u16).to_be_bytes().to_vec();
        payload.extend_from_slice(&[0xC0, 0x80]);
        let body = {
            let mut v = vec![TagType::Compound as u8];
            v.extend_from_slice(&named(TagType::String, "s", &payload));
            v.push(TagType::End as u8);
            v
        };
        assert_eq!(Value::parse(&body).unwrap().get("s").unwrap().as_str(), Some("\0"));

        // CESU-8 surrogate pair for U+1F600.
        let mut payload = (6u16).to_be_bytes().to_vec();
        payload.extend_from_slice(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]);
        let body = {
            let mut v = vec![TagType::Compound as u8];
            v.extend_from_slice(&named(TagType::String, "s", &payload));
            v.push(TagType::End as u8);
            v
        };
        assert_eq!(Value::parse(&body).unwrap().get("s").unwrap().as_str(), Some("\u{1F600}"));
    }

    #[test]
    fn plain_ascii_strings_are_borrowed_not_copied() {
        let mut payload = (3u16).to_be_bytes().to_vec();
        payload.extend_from_slice(b"abc");
        let body = {
            let mut v = vec![TagType::Compound as u8];
            v.extend_from_slice(&named(TagType::String, "s", &payload));
            v.push(TagType::End as u8);
            v
        };
        let v = Value::parse(&body).unwrap();
        match v.get("s").unwrap() {
            Value::String(std::borrow::Cow::Borrowed(_)) => {}
            other => panic!("expected a borrowed string, got {other:?}"),
        }
    }
}
