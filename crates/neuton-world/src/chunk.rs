//! Decoding `level_chunk_with_light` into sections.

use crate::palette::{PalettedContainer, SECTION_VOLUME, block_index};
use neuton_blocks::StateId;
use neuton_protocol::{Error, Reader, Result};

/// A 16 x 16 x 16 slice of a chunk.
#[derive(Debug, Clone)]
pub struct Section {
    /// Non-air blocks, as counted by the server.
    ///
    /// Zero means the section is empty and meshing can skip it outright,
    /// without unpacking anything.
    pub block_count: u16,
    /// Blocks holding fluid. Sent since 26.x, and needed by the mesher to know
    /// whether a section has any water surface to build.
    pub fluid_count: u16,
    pub blocks: PalettedContainer,
    pub biomes: PalettedContainer,
}

impl Section {
    pub fn read(r: &mut Reader<'_>) -> Result<Self> {
        let block_count = r.read_i16()? as u16;
        // Two shorts, not one: LevelChunkSection writes nonEmptyBlockCount and
        // then fluidCount. Missing the second shifts everything after it by two
        // bytes, which shows up several sections later as an impossible
        // bits-per-entry rather than as a clean error here.
        let fluid_count = r.read_i16()? as u16;
        let blocks = PalettedContainer::read_blocks(r)?;
        let biomes = PalettedContainer::read_biomes(r)?;
        Ok(Self { block_count, fluid_count, blocks, biomes })
    }

    /// True if nothing in this section can produce geometry.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.block_count == 0
    }

    #[inline]
    pub fn state_at(&self, x: usize, y: usize, z: usize) -> Option<StateId> {
        self.blocks.get(block_index(x, y, z)).map(StateId)
    }

    /// Places a block, keeping the counts the mesher relies on honest.
    ///
    /// `block_count` is what decides whether a section is skipped outright, so
    /// a section that gains its first block has to stop being empty or the
    /// block will never be drawn.
    pub fn set_state(&mut self, x: usize, y: usize, z: usize, state: StateId) {
        let index = block_index(x, y, z);
        let before = self.blocks.get(index).unwrap_or(0);
        if before == state.0 {
            return;
        }
        let was_air = StateId(before).is_air();
        let is_air = state.is_air();
        self.blocks.set(index, state.0);
        match (was_air, is_air) {
            (true, false) => self.block_count = self.block_count.saturating_add(1),
            (false, true) => self.block_count = self.block_count.saturating_sub(1),
            _ => {}
        }
    }
}

/// One heightmap, kept packed.
#[derive(Debug, Clone)]
pub struct Heightmap {
    /// Registry ordinal of the heightmap type.
    pub kind: i32,
    pub data: Vec<u64>,
}

/// A block entity's position and type. The NBT payload is skipped during decode
/// and re-read on demand, since most block entities never need it.
#[derive(Debug, Clone)]
pub struct BlockEntity {
    /// Position within the chunk column.
    pub x: u8,
    pub z: u8,
    pub y: i16,
    /// Registry ID of the block entity type.
    pub kind: i32,
}

/// Sky and block light for a column.
///
/// One nibble per block, so a section is 2048 bytes. Sections the server calls
/// empty carry no array at all, which is most of them: an underground column is
/// almost entirely dark and an open one is almost entirely full daylight.
#[derive(Debug, Clone, Default)]
pub struct Lighting {
    /// Indexed by section index + 1, because the server sends light for one
    /// section below the world and one above it.
    pub sky: Vec<Option<Vec<u8>>>,
    pub block: Vec<Option<Vec<u8>>>,
    /// Lowest section the arrays cover, in world section coordinates.
    pub min_section: i32,
}

impl Lighting {
    /// Sky light at a position, 0 to 15.
    ///
    /// Defaults to full daylight where the server sent nothing, which is what
    /// vanilla assumes for a section above the terrain.
    #[inline]
    pub fn sky_at(&self, x: usize, y: i32, z: usize) -> u8 {
        self.sample(&self.sky, x, y, z).unwrap_or(15)
    }

    /// Block light at a position, 0 to 15. Absent means unlit.
    #[inline]
    pub fn block_at(&self, x: usize, y: i32, z: usize) -> u8 {
        self.sample(&self.block, x, y, z).unwrap_or(0)
    }

    fn sample(&self, arrays: &[Option<Vec<u8>>], x: usize, y: i32, z: usize) -> Option<u8> {
        let section = y.div_euclid(16) - self.min_section;
        let array = arrays.get(usize::try_from(section).ok()?)?.as_ref()?;
        let index = block_index(x & 15, y.rem_euclid(16) as usize, z & 15);
        // Two blocks per byte: low nibble first.
        let byte = *array.get(index / 2)?;
        Some(if index % 2 == 0 { byte & 0x0F } else { byte >> 4 })
    }
}

/// A decoded chunk column.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    /// Lowest world Y this column covers, from the dimension type.
    pub min_y: i32,
    /// Bottom to top. Sections the server marked empty are still present.
    pub sections: Vec<Section>,
    pub heightmaps: Vec<Heightmap>,
    pub block_entities: Vec<BlockEntity>,
    pub lighting: Lighting,
}

impl Chunk {
    /// Decodes the body of `level_chunk_with_light`, packet ID already consumed.
    ///
    /// `section_count` and `min_y` come from the dimension type sent during
    /// configuration; they are not in the packet, and guessing them is the usual
    /// reason a chunk decoder reads garbage on a non-overworld dimension.
    pub fn read(r: &mut Reader<'_>, section_count: usize, min_y: i32) -> Result<Self> {
        let x = r.read_i32()?;
        let z = r.read_i32()?;

        // Heightmaps: a map of type ordinal -> packed long array. This replaced
        // the older NBT compound.
        let hm_count = r.read_varint_len(64)?;
        let mut heightmaps = Vec::with_capacity(hm_count);
        for _ in 0..hm_count {
            let kind = r.read_varint()?;
            let words = r.read_varint_len(1 << 16)?;
            let mut data = Vec::with_capacity(words.min(512));
            for _ in 0..words {
                data.push(r.read_u64()?);
            }
            heightmaps.push(Heightmap { kind, data });
        }

        // Section data arrives as one length-prefixed blob. Decoding it through
        // a sub-reader means a malformed section cannot run off into the light
        // data that follows.
        let body = r.read_byte_array()?;
        let mut sr = Reader::new(body);
        let mut sections = Vec::with_capacity(section_count);
        for i in 0..section_count {
            sections.push(Section::read(&mut sr).map_err(|e| {
                // Name the section, otherwise a height mismatch looks like a
                // generic EOF and is very hard to place.
                if matches!(e, Error::Eof { .. }) {
                    Error::Eof { needed: i, remaining: sr.remaining() }
                } else {
                    e
                }
            })?);
        }

        let be_count = r.read_varint_len(1 << 16)?;
        let mut block_entities = Vec::with_capacity(be_count.min(256));
        for _ in 0..be_count {
            let packed = r.read_u8()?;
            let y = r.read_i16()?;
            let kind = r.read_varint()?;
            // Step over the NBT without building it.
            let consumed = neuton_nbt::skip_network(r.rest())
                .map_err(|_| Error::Eof { needed: 0, remaining: r.remaining() })?;
            r.read_bytes(consumed)?;
            block_entities.push(BlockEntity { x: packed >> 4, z: packed & 0x0F, y, kind });
        }

        // Light follows the chunk in the same packet. Skipping it is what makes
        // a world look uniformly lit at noon with no way to tell inside from
        // outside.
        let lighting = read_lighting(r, min_y).unwrap_or_default();

        Ok(Self { x, z, min_y, sections, heightmaps, block_entities, lighting })
    }

    /// Block state at a position relative to this column, `y` in world
    /// coordinates.
    pub fn state_at(&self, x: usize, y: i32, z: usize) -> Option<StateId> {
        let dy = y - self.min_y;
        if dy < 0 {
            return None;
        }
        let section = self.sections.get(dy as usize / 16)?;
        section.state_at(x & 15, dy as usize % 16, z & 15)
    }

    /// Places a block, in coordinates relative to this column with `y` in the
    /// world's own range.
    pub fn set_state(&mut self, x: usize, y: i32, z: usize, state: StateId) {
        let dy = y - self.min_y;
        if dy < 0 {
            return;
        }
        let Some(section) = self.sections.get_mut(dy as usize / 16) else { return };
        section.set_state(x & 15, dy as usize % 16, z & 15, state);
    }

    /// Sections that could contribute geometry.
    pub fn non_empty_sections(&self) -> impl Iterator<Item = (usize, &Section)> {
        self.sections.iter().enumerate().filter(|(_, s)| !s.is_empty())
    }

    /// Total non-air blocks, as reported by the server.
    pub fn block_count(&self) -> u32 {
        self.sections.iter().map(|s| s.block_count as u32).sum()
    }
}

/// Reads the light section of a chunk packet.
///
/// Four bit sets say which sections carry light and which are known empty, then
/// the arrays themselves, sky first. A missing array is not an error: it means
/// the section is uniformly lit or uniformly dark.
fn read_lighting(r: &mut Reader<'_>, min_y: i32) -> Result<Lighting> {
    let sky_mask = read_bitset(r)?;
    let block_mask = read_bitset(r)?;
    let _empty_sky = read_bitset(r)?;
    let _empty_block = read_bitset(r)?;

    let sky = read_light_arrays(r, &sky_mask)?;
    let block = read_light_arrays(r, &block_mask)?;

    Ok(Lighting {
        sky,
        block,
        // The first entry covers the section below the bottom of the world.
        min_section: min_y.div_euclid(16) - 1,
    })
}

fn read_bitset(r: &mut Reader<'_>) -> Result<Vec<u64>> {
    let words = r.read_varint_len(1024)?;
    let mut out = Vec::with_capacity(words.min(64));
    for _ in 0..words {
        out.push(r.read_u64()?);
    }
    Ok(out)
}

/// Reads one array per set bit, in bit order, into a slot-indexed list.
fn read_light_arrays(r: &mut Reader<'_>, mask: &[u64]) -> Result<Vec<Option<Vec<u8>>>> {
    let count = r.read_varint_len(1024)?;
    let mut arrays = Vec::with_capacity(count);
    for _ in 0..count {
        arrays.push(r.read_byte_array()?.to_vec());
    }

    let slots = mask.len() * 64;
    let mut out = vec![None; slots];
    let mut next = arrays.into_iter();
    for slot in 0..slots {
        if mask[slot / 64] >> (slot % 64) & 1 == 1
            && let Some(array) = next.next()
        {
            out[slot] = Some(array);
        }
    }
    Ok(out)
}

/// Scratch space for unpacking a section during meshing.
///
/// Allocated once per meshing thread and reused, so decoding a chunk never
/// allocates on the hot path.
pub struct SectionScratch(pub Box<[u32; SECTION_VOLUME]>);

impl Default for SectionScratch {
    fn default() -> Self {
        Self(Box::new([0u32; SECTION_VOLUME]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neuton_protocol::Writer;

    /// Builds a synthetic chunk packet body with `n` sections, each a single
    /// block state, plus one heightmap.
    fn build(n: usize, states: &[u32]) -> Vec<u8> {
        let mut sections = Writer::new();
        for i in 0..n {
            let state = states[i % states.len()];
            sections.write_i16(if state == 0 { 0 } else { 4096 });
            sections.write_i16(0); // fluid count
            // Blocks: single-valued palette.
            sections.write_u8(0);
            sections.write_varint(state as i32);
            // Biomes: single-valued palette.
            sections.write_u8(0);
            sections.write_varint(1);
        }

        let mut w = Writer::new();
        w.write_i32(3);
        w.write_i32(-7);
        w.write_varint(1); // one heightmap
        w.write_varint(0); // type ordinal
        w.write_varint(2); // two words
        w.write_u64(0xdead_beef);
        w.write_u64(0);
        w.write_byte_array(sections.as_slice());
        w.write_varint(0); // no block entities
        write_empty_lighting(&mut w);
        w.into_vec()
    }

    /// Four empty bit sets and two empty array lists.
    fn write_empty_lighting(w: &mut Writer) {
        for _ in 0..4 {
            w.write_varint(0);
        }
        w.write_varint(0);
        w.write_varint(0);
    }

    #[test]
    fn decodes_an_overworld_shaped_column() {
        // Overworld: min_y -64, 384 blocks tall, so 24 sections.
        let bytes = build(24, &[0, 1, 9]);
        let c = Chunk::read(&mut Reader::new(&bytes), 24, -64).unwrap();

        assert_eq!((c.x, c.z), (3, -7));
        assert_eq!(c.sections.len(), 24);
        assert_eq!(c.heightmaps.len(), 1);
        assert_eq!(c.heightmaps[0].data, vec![0xdead_beef, 0]);
        assert!(c.block_entities.is_empty());

        // Section 0 is all air and must be skippable.
        assert!(c.sections[0].is_empty());
        assert!(!c.sections[1].is_empty());
        assert_eq!(c.non_empty_sections().count(), 16);
    }

    #[test]
    fn world_y_maps_to_the_right_section() {
        let bytes = build(24, &[0, 1, 9]);
        let c = Chunk::read(&mut Reader::new(&bytes), 24, -64).unwrap();

        // Section index = (y - min_y) / 16, and the cycle is [air, 1, 9].
        assert_eq!(c.state_at(0, -64, 0), Some(StateId(0))); // section 0
        assert_eq!(c.state_at(0, -48, 0), Some(StateId(1))); // section 1
        assert_eq!(c.state_at(0, -32, 0), Some(StateId(9))); // section 2
        assert_eq!(c.state_at(0, -33, 0), Some(StateId(1))); // still section 1

        // Outside the column.
        assert_eq!(c.state_at(0, -65, 0), None);
        assert_eq!(c.state_at(0, 320, 0), None);
    }

    #[test]
    fn a_section_count_mismatch_fails_instead_of_reading_light_data() {
        // Packet built for 24 sections, decoded expecting the nether's 16.
        // The extra bytes must not be silently accepted as something else.
        let bytes = build(16, &[1]);
        assert!(Chunk::read(&mut Reader::new(&bytes), 24, -64).is_err());
    }

    #[test]
    fn block_entity_nbt_is_stepped_over_correctly() {
        let mut sections = Writer::new();
        sections.write_i16(0);
        sections.write_i16(0); // fluid count
        sections.write_u8(0);
        sections.write_varint(0);
        sections.write_u8(0);
        sections.write_varint(1);

        let mut w = Writer::new();
        w.write_i32(0);
        w.write_i32(0);
        w.write_varint(0); // no heightmaps
        w.write_byte_array(sections.as_slice());
        w.write_varint(1);
        w.write_u8(0x5A); // x=5, z=10
        w.write_i16(64);
        w.write_varint(7);
        // An empty NBT compound: type byte then TAG_End.
        w.write_u8(10);
        w.write_u8(0);
        write_empty_lighting(&mut w);
        // Trailing marker proves the skip consumed exactly the NBT and light.
        w.write_i32(0x4321);
        let bytes = w.into_vec();

        let mut r = Reader::new(&bytes);
        let c = Chunk::read(&mut r, 1, 0).unwrap();
        assert_eq!(c.block_entities.len(), 1);
        let be = &c.block_entities[0];
        assert_eq!((be.x, be.z, be.y, be.kind), (5, 10, 64, 7));
        assert_eq!(r.read_i32().unwrap(), 0x4321, "skip consumed the wrong length");
    }
}
