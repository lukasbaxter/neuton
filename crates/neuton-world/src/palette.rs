//! Paletted containers, the format chunk sections are packed in.
//!
//! A section holds 4,096 block states. Storing them as global IDs would be
//! 8 KiB per section; most sections use a handful of distinct blocks, so the
//! server sends a small palette plus tightly packed indices into it.
//!
//! The packed form is kept as-is rather than expanded on arrival. A render
//! distance of 16 is roughly 25,000 sections, and eagerly expanding every one
//! would cost hundreds of megabytes for data the mesher reads once. Unpacking
//! happens per section, into a caller-owned scratch buffer, at mesh time.

use neuton_protocol::{Reader, Result as PResult};

/// Block states per section: 16 x 16 x 16.
pub const SECTION_VOLUME: usize = 4096;
/// Biome cells per section: 4 x 4 x 4.
pub const BIOME_VOLUME: usize = 64;

/// Above this many bits, block containers send global IDs with no palette.
const MAX_INDIRECT_BLOCK_BITS: u8 = 8;
/// Same threshold for biomes, which have a far smaller registry.
const MAX_INDIRECT_BIOME_BITS: u8 = 3;

/// How a container's values are stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Palette {
    /// Every cell holds the same value. Costs nothing to store and lets the
    /// mesher skip an all-air section with a single comparison.
    Single(u32),
    /// Indices into a small lookup table.
    Indirect(Vec<u32>),
    /// Values are registry IDs directly.
    Direct,
}

/// A decoded paletted container: the palette plus the packed index array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalettedContainer {
    pub palette: Palette,
    /// Bits per entry. Zero for [`Palette::Single`].
    pub bits: u8,
    /// Packed entries. Values never straddle a `u64` boundary; the top
    /// `64 % bits` bits of each word are padding.
    pub data: Vec<u64>,
    /// Number of logical cells, 4096 for blocks and 64 for biomes.
    pub len: usize,
}

impl PalettedContainer {
    pub fn read_blocks(r: &mut Reader<'_>) -> PResult<Self> {
        Self::read(r, SECTION_VOLUME, MAX_INDIRECT_BLOCK_BITS)
    }

    pub fn read_biomes(r: &mut Reader<'_>) -> PResult<Self> {
        Self::read(r, BIOME_VOLUME, MAX_INDIRECT_BIOME_BITS)
    }

    fn read(r: &mut Reader<'_>, len: usize, max_indirect: u8) -> PResult<Self> {
        let bits = r.read_u8()?;
        let palette = if bits == 0 {
            Palette::Single(r.read_varint()? as u32)
        } else if bits <= max_indirect {
            let n = r.read_varint_len(1 << max_indirect.min(16))?;
            let mut entries = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                entries.push(r.read_varint()? as u32);
            }
            Palette::Indirect(entries)
        } else {
            Palette::Direct
        };

        // The data array is always length-prefixed, including the zero-length
        // array that follows a single-valued palette.
        let words = r.read_varint_len(1 << 20)?;
        let mut data = Vec::with_capacity(words.min(4096));
        for _ in 0..words {
            data.push(r.read_u64()?);
        }

        Ok(Self { palette, bits, data, len })
    }

    /// True if this container is uniform, and what value it holds.
    pub fn uniform(&self) -> Option<u32> {
        match &self.palette {
            Palette::Single(v) => Some(*v),
            _ => None,
        }
    }

    /// Reads one cell.
    ///
    /// Fine for probes and block updates. For meshing use [`Self::unpack_into`],
    /// which amortises the per-word arithmetic across the whole section.
    pub fn get(&self, index: usize) -> Option<u32> {
        if index >= self.len {
            return None;
        }
        match &self.palette {
            Palette::Single(v) => Some(*v),
            Palette::Indirect(entries) => {
                let raw = self.raw(index)?;
                entries.get(raw as usize).copied()
            }
            Palette::Direct => self.raw(index),
        }
    }

    #[inline]
    fn raw(&self, index: usize) -> Option<u32> {
        let bits = self.bits as usize;
        if bits == 0 {
            return Some(0);
        }
        let per_word = 64 / bits;
        let word = self.data.get(index / per_word)?;
        let shift = (index % per_word) * bits;
        let mask = (1u64 << bits) - 1;
        Some(((word >> shift) & mask) as u32)
    }

    /// Expands every cell into `out`, resolving the palette.
    ///
    /// This is the hot path into meshing. Values are read word by word so the
    /// division and mask are computed once per `u64` rather than once per cell.
    /// Returns `false` if the container is malformed, leaving `out` filled with
    /// whatever was resolved so far.
    pub fn unpack_into(&self, out: &mut [u32]) -> bool {
        debug_assert!(out.len() >= self.len);
        let out = &mut out[..self.len];

        match &self.palette {
            Palette::Single(v) => {
                out.fill(*v);
                return true;
            }
            Palette::Indirect(entries) => {
                if entries.is_empty() {
                    return false;
                }
                self.for_each_raw(out, |raw| {
                    // A corrupt index resolves to the first palette entry
                    // rather than panicking mid-frame.
                    entries.get(raw as usize).copied().unwrap_or(entries[0])
                })
            }
            Palette::Direct => self.for_each_raw(out, |raw| raw),
        }
    }

    fn for_each_raw(&self, out: &mut [u32], resolve: impl Fn(u32) -> u32) -> bool {
        let bits = self.bits as usize;
        if bits == 0 || bits > 32 {
            return false;
        }
        let per_word = 64 / bits;
        let mask = (1u64 << bits) - 1;
        let needed = self.len.div_ceil(per_word);
        if self.data.len() < needed {
            return false;
        }

        let mut i = 0;
        for &word in &self.data[..needed] {
            let take = per_word.min(self.len - i);
            for slot in 0..take {
                out[i + slot] = resolve(((word >> (slot * bits)) & mask) as u32);
            }
            i += take;
            if i >= self.len {
                break;
            }
        }
        true
    }
}

/// Index of a block within a section, in the order the wire packs them.
///
/// The ordering is y, then z, then x, so x is contiguous. Meshing loops should
/// iterate in that order to walk memory forwards.
#[inline]
pub const fn block_index(x: usize, y: usize, z: usize) -> usize {
    (y << 8) | (z << 4) | x
}

#[cfg(test)]
mod tests {
    use super::*;
    use neuton_protocol::Writer;

    /// Packs `values` at `bits` per entry, the way the server does.
    fn pack(values: &[u32], bits: usize) -> Vec<u64> {
        let per_word = 64 / bits;
        let mut out = vec![0u64; values.len().div_ceil(per_word)];
        for (i, &v) in values.iter().enumerate() {
            out[i / per_word] |= (v as u64) << ((i % per_word) * bits);
        }
        out
    }

    fn encode(bits: u8, palette: Option<&[u32]>, data: &[u64]) -> Vec<u8> {
        let mut w = Writer::new();
        w.write_u8(bits);
        match palette {
            Some(p) if bits == 0 => {
                w.write_varint(p[0] as i32);
            }
            Some(p) => {
                w.write_varint(p.len() as i32);
                for &v in p {
                    w.write_varint(v as i32);
                }
            }
            None => {}
        }
        w.write_varint(data.len() as i32);
        for &d in data {
            w.write_u64(d);
        }
        w.into_vec()
    }

    #[test]
    fn single_valued_section_decodes_without_a_data_array() {
        let bytes = encode(0, Some(&[0]), &[]);
        let c = PalettedContainer::read_blocks(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(c.uniform(), Some(0));
        assert_eq!(c.get(0), Some(0));
        assert_eq!(c.get(SECTION_VOLUME - 1), Some(0));
        assert_eq!(c.get(SECTION_VOLUME), None);

        let mut out = vec![9u32; SECTION_VOLUME];
        assert!(c.unpack_into(&mut out));
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn indirect_palette_resolves_through_the_lookup_table() {
        // Four distinct blocks needs 4 bits per entry.
        let palette = [0u32, 1, 9, 32365];
        let indices: Vec<u32> = (0..SECTION_VOLUME).map(|i| (i % 4) as u32).collect();
        let bytes = encode(4, Some(&palette), &pack(&indices, 4));

        let c = PalettedContainer::read_blocks(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(c.uniform(), None);
        for i in 0..SECTION_VOLUME {
            assert_eq!(c.get(i), Some(palette[i % 4]), "cell {i}");
        }

        let mut out = vec![0u32; SECTION_VOLUME];
        assert!(c.unpack_into(&mut out));
        // The bulk path and the single-cell path must agree exactly.
        for i in 0..SECTION_VOLUME {
            assert_eq!(out[i], c.get(i).unwrap(), "unpack disagrees at {i}");
        }
    }

    #[test]
    fn direct_palette_carries_global_state_ids() {
        // 15 bits covers all 32,366 states.
        let bits = 15;
        let values: Vec<u32> = (0..SECTION_VOLUME).map(|i| (i as u32 * 7) % 32366).collect();
        let bytes = encode(bits, None, &pack(&values, bits as usize));

        let c = PalettedContainer::read_blocks(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(c.palette, Palette::Direct);
        let mut out = vec![0u32; SECTION_VOLUME];
        assert!(c.unpack_into(&mut out));
        assert_eq!(out, values);
    }

    #[test]
    fn entries_do_not_straddle_word_boundaries() {
        // 5 bits gives 12 entries per u64 with 4 bits of padding, the case that
        // breaks any decoder that treats the array as one long bit stream.
        let bits = 5usize;
        let values: Vec<u32> = (0..SECTION_VOLUME).map(|i| (i % 32) as u32).collect();
        let packed = pack(&values, bits);
        assert_eq!(packed.len(), SECTION_VOLUME.div_ceil(64 / bits));

        let palette: Vec<u32> = (0..32).collect();
        let bytes = encode(bits as u8, Some(&palette), &packed);
        let c = PalettedContainer::read_blocks(&mut Reader::new(&bytes)).unwrap();
        let mut out = vec![0u32; SECTION_VOLUME];
        assert!(c.unpack_into(&mut out));
        assert_eq!(out, values);
    }

    #[test]
    fn biomes_use_a_smaller_indirect_threshold() {
        // 4 bits is direct for biomes but indirect for blocks. Same bytes must
        // therefore decode differently depending on which reader is used.
        let values: Vec<u32> = (0..BIOME_VOLUME).map(|i| (i % 16) as u32).collect();
        let bytes = encode(4, None, &pack(&values, 4));
        let c = PalettedContainer::read_biomes(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(c.palette, Palette::Direct);
        assert_eq!(c.len, BIOME_VOLUME);
        let mut out = vec![0u32; BIOME_VOLUME];
        assert!(c.unpack_into(&mut out));
        assert_eq!(out, values);
    }

    #[test]
    fn a_truncated_data_array_is_reported_not_panicked() {
        let palette = [0u32, 1];
        // Claims 4 bits per entry but supplies a single word.
        let bytes = encode(4, Some(&palette), &[0u64]);
        let c = PalettedContainer::read_blocks(&mut Reader::new(&bytes)).unwrap();
        let mut out = vec![0u32; SECTION_VOLUME];
        assert!(!c.unpack_into(&mut out), "short data must fail rather than read past the end");
    }

    #[test]
    fn block_index_is_y_then_z_then_x() {
        assert_eq!(block_index(0, 0, 0), 0);
        assert_eq!(block_index(1, 0, 0), 1);
        assert_eq!(block_index(0, 0, 1), 16);
        assert_eq!(block_index(0, 1, 0), 256);
        assert_eq!(block_index(15, 15, 15), SECTION_VOLUME - 1);
    }
}
