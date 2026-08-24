//! Mapping every block state to the six atlas rectangles that cover it.
//!
//! Built once from the pack stack, then read from the mesher with no locking
//! and no allocation. Resolving models during meshing would put JSON parsing on
//! the hot path.

use neuton_assets::{Atlas, ModelResolver, PackStack, Uv};
use neuton_blocks::{BLOCK_COUNT, STATE_COUNT, StateId};

/// The six faces of a block, as atlas coordinates.
pub type FaceUvs = [Uv; 6];

/// Atlas rectangles for every block state.
pub struct BlockTextures {
    /// Index into `entries`, one per state.
    ///
    /// Deduplicated: most states of a block share their textures, and most
    /// blocks are one plain cube, so the table collapses hard.
    index: Vec<u16>,
    entries: Vec<FaceUvs>,
    pub atlas: Atlas,
}

impl crate::mesh::FaceUvSource for BlockTextures {
    #[inline]
    fn face_uvs(&self, state: StateId) -> &FaceUvs {
        BlockTextures::faces(self, state)
    }
}

impl BlockTextures {
    /// Resolves every block state and stitches the atlas it needs.
    pub fn build(packs: &mut PackStack) -> Self {
        let mut resolver = ModelResolver::new();

        // Two passes. The first learns which textures exist so the atlas can be
        // sized and stitched; the second turns each state into atlas
        // coordinates, which cannot happen before the atlas exists.
        let mut wanted = std::collections::BTreeSet::new();
        let mut per_state: Vec<Option<[String; 6]>> = vec![None; STATE_COUNT];

        for i in 0..BLOCK_COUNT {
            let block = neuton_blocks::BlockId(i);
            let b = block.get();
            for raw in b.first_state.0..b.first_state.0 + b.state_count {
                let state = StateId(raw);
                if let Some(ft) = resolver.textures(packs, block.name(), state.variant_key()) {
                    for path in ft.distinct() {
                        wanted.insert(path.to_string());
                    }
                    per_state[raw as usize] = Some(ft.faces);
                }
            }
        }

        let paths: Vec<String> = wanted.into_iter().collect();
        let atlas = Atlas::stitch(packs, &paths);

        let missing = atlas.uv("");
        let mut entries: Vec<FaceUvs> = vec![[missing; 6]];
        let mut lookup: std::collections::HashMap<[u32; 6], u16> = Default::default();
        let mut index = vec![0u16; STATE_COUNT];

        for (raw, faces) in per_state.iter().enumerate() {
            let Some(faces) = faces else { continue };
            let uvs: FaceUvs = std::array::from_fn(|f| atlas.uv(&faces[f]));
            // Keyed on the bit patterns, since f32 is not hashable and these are
            // copies of the same values rather than computed ones.
            let key: [u32; 6] = std::array::from_fn(|f| {
                (uvs[f].min[0].to_bits() ^ uvs[f].min[1].to_bits().rotate_left(16))
                    .wrapping_mul(0x9E37_79B9)
            });
            let slot = *lookup.entry(key).or_insert_with(|| {
                entries.push(uvs);
                (entries.len() - 1) as u16
            });
            index[raw] = slot;
        }

        Self { index, entries, atlas }
    }

    /// Atlas rectangles for a state, in [`crate::Face`] order.
    #[inline]
    pub fn faces(&self, state: StateId) -> &FaceUvs {
        let slot = self.index.get(state.0 as usize).copied().unwrap_or(0) as usize;
        self.entries.get(slot).unwrap_or(&self.entries[0])
    }

    /// How many distinct face sets the table collapsed to.
    pub fn distinct(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a real installation, so it is skipped when there is not one.
    fn packs() -> Option<PackStack> {
        let jar = neuton_assets::vanilla_jar("26.2")?;
        let mut packs = PackStack::new();
        packs.push(&jar).ok()?;
        Some(packs)
    }

    #[test]
    fn every_state_resolves_against_a_real_installation() {
        let Some(mut packs) = packs() else {
            eprintln!("skipped: no vanilla 26.2 installation");
            return;
        };
        let t = BlockTextures::build(&mut packs);

        assert!(t.atlas.len() > 500, "atlas looks empty: {}", t.atlas.len());
        assert!(t.distinct() > 100, "table collapsed too far: {}", t.distinct());
        assert!(
            t.distinct() < STATE_COUNT,
            "deduplication did nothing: {} of {STATE_COUNT}",
            t.distinct()
        );

        // Grass has a different top, side and bottom; stone is the same all over.
        let grass = neuton_blocks::by_name("minecraft:grass_block").unwrap().get().default_state;
        let g = t.faces(grass);
        assert_ne!(g[1], g[0], "grass top and bottom must differ");
        assert_ne!(g[1], g[2], "grass top and side must differ");

        let stone = neuton_blocks::by_name("minecraft:stone").unwrap().get().default_state;
        let s = t.faces(stone);
        assert!(s.iter().all(|u| *u == s[0]), "stone should be uniform");
        assert_ne!(s[0], g[1], "stone and grass must not share a tile");
    }

    #[test]
    fn an_out_of_range_state_is_safe() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        // Never panics, even for a state the server should not have sent.
        let _ = t.faces(StateId(u32::MAX));
        let _ = t.faces(StateId(STATE_COUNT as u32));
    }
}
