//! Baking every block state into geometry the mesher can emit directly.
//!
//! Resolving models means parsing JSON, and a chunk asks about tens of
//! thousands of blocks. So it happens once, up front, and what the mesher sees
//! is boxes with atlas coordinates and tint colours already attached.

use neuton_assets::models::BlockModel;
use neuton_assets::{Atlas, ModelResolver, PackStack, TintSource, Tints, Uv};
use neuton_blocks::{BLOCK_COUNT, STATE_COUNT, StateId};

/// One face, ready to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BakedFace {
    pub uv: Uv,
    /// Multiplied into the texture. White for anything untinted.
    pub tint: [f32; 3],
    /// Which neighbour hides this face. `None` means always draw it, which is
    /// what keeps the inside of a fence or a stair step visible.
    pub cullface: Option<u8>,
}

/// One box of a block, in 0..1 block space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BakedElement {
    pub from: [f32; 3],
    pub to: [f32; 3],
    pub faces: [Option<BakedFace>; 6],
    /// True when this box fills the cell, so the mesher can take the fast path.
    pub full_cube: bool,
}

/// Everything needed to draw one block state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BakedModel {
    pub elements: Vec<BakedElement>,
}

impl BakedModel {
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }
}

/// Baked models for every block state.
pub struct BlockTextures {
    /// Index into `models`, one per state. Deduplicated hard: most states of a
    /// block share a model and most blocks are one plain cube.
    index: Vec<u16>,
    models: Vec<BakedModel>,
    pub atlas: Atlas,
}

impl BlockTextures {
    /// Resolves every block state, stitches the atlas, and bakes the result.
    pub fn build(packs: &mut PackStack) -> Self {
        let mut resolver = ModelResolver::new();
        let tints = Tints::load(packs);

        // Two passes: the atlas cannot be stitched before the textures it needs
        // are known, and faces cannot be baked before the atlas exists.
        let mut wanted = std::collections::BTreeSet::new();
        let mut raw: Vec<Option<(BlockModel, TintSource)>> = vec![None; STATE_COUNT];

        for i in 0..BLOCK_COUNT {
            let block = neuton_blocks::BlockId(i);
            let tint = neuton_assets::tint::source_for(block.name());
            let b = block.get();
            for id in b.first_state.0..b.first_state.0 + b.state_count {
                let state = StateId(id);
                if let Some(model) = resolver.model(packs, block.name(), state.variant_key()) {
                    for path in model.textures() {
                        wanted.insert(path.to_string());
                    }
                    raw[id as usize] = Some((model, tint));
                }
            }
        }

        let paths: Vec<String> = wanted.into_iter().collect();
        let atlas = Atlas::stitch(packs, &paths);

        let mut models: Vec<BakedModel> = vec![BakedModel::default()];
        let mut lookup: std::collections::HashMap<String, u16> = Default::default();
        let mut index = vec![0u16; STATE_COUNT];

        for (id, entry) in raw.iter().enumerate() {
            let Some((model, tint_source)) = entry else { continue };
            let baked = bake(model, &atlas, tints.get(*tint_source));
            // Keyed on a cheap textual form: the values are copies of the same
            // few shapes, and f32 is not hashable.
            let key = fingerprint(&baked);
            let slot = *lookup.entry(key).or_insert_with(|| {
                models.push(baked);
                (models.len() - 1) as u16
            });
            index[id] = slot;
        }

        Self { index, models, atlas }
    }

    #[inline]
    pub fn model(&self, state: StateId) -> &BakedModel {
        let slot = self.index.get(state.0 as usize).copied().unwrap_or(0) as usize;
        self.models.get(slot).unwrap_or(&self.models[0])
    }

    /// How many distinct models the table collapsed to.
    pub fn distinct(&self) -> usize {
        self.models.len()
    }
}

/// Turns a resolved model into block-space boxes with atlas coordinates.
fn bake(model: &BlockModel, atlas: &Atlas, tint: [f32; 3]) -> BakedModel {
    let elements = model
        .elements
        .iter()
        .map(|element| {
            let mut from = element.from;
            let mut to = element.to;
            let mut faces: [Option<BakedFace>; 6] = Default::default();
            for (i, face) in element.faces.iter().enumerate() {
                let Some(face) = face else { continue };
                faces[i] = Some(BakedFace {
                    uv: atlas.uv(&face.texture),
                    tint: if face.tinted { tint } else { [1.0, 1.0, 1.0] },
                    cullface: face.cullface,
                });
            }

            // Blockstate rotation, applied here so the mesher never has to.
            // Vanilla applies x first, then y.
            for _ in 0..(model.x_rot.rem_euclid(360) / 90) {
                rotate_x(&mut from, &mut to, &mut faces);
            }
            for _ in 0..(model.y_rot.rem_euclid(360) / 90) {
                rotate_y(&mut from, &mut to, &mut faces);
            }

            let full_cube = from == [0.0, 0.0, 0.0] && to == [16.0, 16.0, 16.0];
            BakedElement {
                from: [from[0] / 16.0, from[1] / 16.0, from[2] / 16.0],
                to: [to[0] / 16.0, to[1] / 16.0, to[2] / 16.0],
                faces,
                full_cube,
            }
        })
        .collect();
    BakedModel { elements }
}

/// Rotates a box and its faces 90 degrees about the X axis.
///
/// Sends up to north, which is what turns a vertical column model into a log
/// lying along Z.
fn rotate_x(from: &mut [f32; 3], to: &mut [f32; 3], faces: &mut [Option<BakedFace>; 6]) {
    let map = |p: [f32; 3]| [p[0], p[2], 16.0 - p[1]];
    let (a, b) = (map(*from), map(*to));
    *from = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
    *to = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
    // down=0 up=1 north=2 south=3 west=4 east=5
    permute(faces, [3, 2, 0, 1, 4, 5]);
}

/// Rotates a box and its faces 90 degrees about the Y axis, clockwise seen from
/// above: north becomes east.
fn rotate_y(from: &mut [f32; 3], to: &mut [f32; 3], faces: &mut [Option<BakedFace>; 6]) {
    let map = |p: [f32; 3]| [16.0 - p[2], p[1], p[0]];
    let (a, b) = (map(*from), map(*to));
    *from = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
    *to = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
    permute(faces, [0, 1, 5, 4, 2, 3]);
}

/// `source[i]` is the face that ends up at position `i`.
fn permute(faces: &mut [Option<BakedFace>; 6], source: [usize; 6]) {
    let old = faces.clone();
    for (i, &from) in source.iter().enumerate() {
        faces[i] = old[from];
        // A cullface names a direction, so it moves with the geometry.
        if let Some(face) = &mut faces[i]
            && let Some(cull) = face.cullface
        {
            face.cullface = Some(source.iter().position(|&s| s == cull as usize).unwrap_or(cull as usize) as u8);
        }
    }
}

fn fingerprint(model: &BakedModel) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(model.elements.len() * 96);
    for e in &model.elements {
        let _ = write!(out, "{:?}{:?}{}", e.from, e.to, e.full_cube);
        for face in &e.faces {
            match face {
                Some(f) => {
                    let _ = write!(out, "|{:?}{:?}{:?}", f.uv.min, f.uv.max, f.tint);
                    let _ = write!(out, "{:?}", f.cullface);
                }
                None => out.push('_'),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packs() -> Option<PackStack> {
        let jar = neuton_assets::vanilla_jar("26.2")?;
        let mut packs = PackStack::new();
        packs.push(&jar).ok()?;
        Some(packs)
    }

    #[test]
    fn every_state_bakes_against_a_real_installation() {
        let Some(mut packs) = packs() else {
            eprintln!("skipped: no vanilla 26.2 installation");
            return;
        };
        let t = BlockTextures::build(&mut packs);
        assert!(t.atlas.len() > 500, "atlas looks empty: {}", t.atlas.len());
        assert!(t.distinct() > 100, "collapsed too far: {}", t.distinct());
        assert!(t.distinct() < STATE_COUNT, "deduplication did nothing");

        let stone = neuton_blocks::by_name("minecraft:stone").unwrap().get().default_state;
        let m = t.model(stone);
        assert_eq!(m.elements.len(), 1);
        assert!(m.elements[0].full_cube);
    }

    #[test]
    fn grass_is_tinted_green_on_top_and_not_on_the_bottom() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        let grass = neuton_blocks::by_name("minecraft:grass_block").unwrap().get().default_state;
        let m = t.model(grass);

        let top = m.elements[0].faces[1].expect("grass has a top face");
        assert!(top.tint[1] > top.tint[0] && top.tint[1] > top.tint[2], "top not green: {:?}", top.tint);
        let bottom = m.elements[0].faces[0].expect("grass has a bottom face");
        assert_eq!(bottom.tint, [1.0, 1.0, 1.0], "the dirt underneath is not tinted");
    }

    #[test]
    fn leaves_are_tinted_on_every_face() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        let leaves = neuton_blocks::by_name("minecraft:oak_leaves").unwrap().get().default_state;
        for face in t.model(leaves).elements[0].faces.iter().flatten() {
            assert!(face.tint[1] > face.tint[0], "leaf face not tinted: {:?}", face.tint);
        }
    }

    #[test]
    fn a_slab_bakes_to_half_a_block() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        let id = neuton_blocks::by_name("minecraft:stone_slab").unwrap();
        // The bottom variant occupies the lower half and is not a full cube.
        let state = id.get().default_state;
        let m = t.model(state);
        assert!(!m.elements[0].full_cube, "a slab must not bake as a full cube");
        assert!(m.elements[0].to[1] <= 0.5 + 1e-6, "top at {}", m.elements[0].to[1]);
    }

    #[test]
    fn rotation_sends_a_column_end_where_the_axis_points() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        let log = neuton_blocks::by_name("minecraft:oak_log").unwrap();

        let face_texture = |variant: &str, face: usize| {
            let state = (log.get().first_state.0..log.get().first_state.0 + log.get().state_count)
                .map(StateId)
                .find(|s| s.variant_key() == variant)
                .expect("variant exists");
            t.model(state).elements[0].faces[face].expect("face").uv
        };

        // Upright: the end grain is on top and bottom.
        assert_eq!(face_texture("axis=y", 1), face_texture("axis=y", 0));
        assert_ne!(face_texture("axis=y", 1), face_texture("axis=y", 2));

        // Lying along Z: the ends move to north and south.
        assert_eq!(face_texture("axis=z", 2), face_texture("axis=z", 3));
        assert_eq!(face_texture("axis=z", 2), face_texture("axis=y", 1));

        // Lying along X: the ends move to west and east.
        assert_eq!(face_texture("axis=x", 4), face_texture("axis=x", 5));
        assert_eq!(face_texture("axis=x", 4), face_texture("axis=y", 1));
    }

    #[test]
    fn an_out_of_range_state_is_safe() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        assert!(t.model(StateId(u32::MAX)).is_empty());
        assert!(t.model(StateId(STATE_COUNT as u32)).is_empty());
    }
}
