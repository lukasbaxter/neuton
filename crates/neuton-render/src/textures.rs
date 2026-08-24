//! Baking every block state into geometry the mesher can emit directly.
//!
//! Resolving models means parsing JSON, and a chunk asks about tens of
//! thousands of blocks. So it happens once, up front, and what the mesher sees
//! is boxes with atlas coordinates and tint colours already attached.

use neuton_assets::models::BlockModel;
use neuton_assets::{Atlas, ModelResolver, PackStack, TintSource, Tints, Uv};

/// Resolved biome colours, indexed by the ids a chunk's biome palette carries.
///
/// Precomputed rather than sampled per block: a colormap lookup per face would
/// put an image read on the meshing hot path, and there are only a few dozen
/// biomes.
pub struct BiomeTints {
    /// grass, foliage, water, per biome.
    colors: Vec<[[f32; 3]; 3]>,
    /// Used for biome ids the server never described.
    fallback: [[f32; 3]; 3],
}

impl BiomeTints {
    /// Resolves every biome the server sent.
    pub fn build(biomes: &[neuton_net::BiomeColors], tints: &Tints) -> Self {
        let resolve = |b: &neuton_net::BiomeColors| -> [[f32; 3]; 3] {
            let unpack = |hex: u32| {
                [
                    ((hex >> 16) & 0xFF) as f32 / 255.0,
                    ((hex >> 8) & 0xFF) as f32 / 255.0,
                    (hex & 0xFF) as f32 / 255.0,
                ]
            };
            let mut grass = b
                .grass
                .map(unpack)
                .unwrap_or_else(|| tints.sample(TintSource::Grass, b.temperature, b.downfall));

            // Two biomes vanilla adjusts in code rather than in data.
            match b.grass_modifier {
                neuton_net::GrassModifier::Swamp => grass = [0.42, 0.44, 0.22],
                neuton_net::GrassModifier::DarkForest => {
                    // Blended halfway towards a fixed brown.
                    let brown = [0.16, 0.16, 0.06];
                    for i in 0..3 {
                        grass[i] = (grass[i] + brown[i]) / 2.0;
                    }
                }
                neuton_net::GrassModifier::None => {}
            }

            let foliage = b
                .foliage
                .map(unpack)
                .unwrap_or_else(|| tints.sample(TintSource::Foliage, b.temperature, b.downfall));
            let water = b.water.map(unpack).unwrap_or([0.247, 0.463, 0.894]);
            [grass, foliage, water]
        };

        Self {
            colors: biomes.iter().map(resolve).collect(),
            fallback: [
                tints.sample(TintSource::Grass, 0.8, 0.4),
                tints.sample(TintSource::Foliage, 0.8, 0.4),
                [0.247, 0.463, 0.894],
            ],
        }
    }

    /// The colour a face takes in a biome.
    #[inline]
    pub fn get(&self, biome: u32, source: TintSource) -> [f32; 3] {
        let entry = self.colors.get(biome as usize).unwrap_or(&self.fallback);
        match source {
            TintSource::None => [1.0, 1.0, 1.0],
            TintSource::Grass => entry[0],
            TintSource::Foliage | TintSource::DryFoliage => entry[1],
            TintSource::Water => entry[2],
            TintSource::Fixed(hex) => [
                ((hex >> 16) & 0xFF) as f32 / 255.0,
                ((hex >> 8) & 0xFF) as f32 / 255.0,
                (hex & 0xFF) as f32 / 255.0,
            ],
        }
    }
}

impl Default for BiomeTints {
    fn default() -> Self {
        Self { colors: Vec::new(), fallback: [[0.57, 0.74, 0.35], [0.47, 0.67, 0.18], [0.247, 0.463, 0.894]] }
    }
}
use neuton_blocks::{BLOCK_COUNT, STATE_COUNT, StateId};
use neuton_world::Aabb;

/// One face, ready to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BakedFace {
    /// Atlas coordinates for the face's four corners, in the order the mesher
    /// emits them.
    ///
    /// Corners rather than a rectangle, because a model face names a sub-region
    /// of its texture and a rotation reorders which corner is which.
    pub uv: [[f32; 2]; 4],
    /// Where this face's colour comes from.
    ///
    /// Resolved at mesh time rather than baked, because it depends on the biome
    /// the block sits in and one block state appears in every biome there is.
    pub tint: TintSource,
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
    /// Boxes to walk into, in 0..1 block space.
    ///
    /// Taken from the render geometry. The game keeps a separate collision
    /// shape, which differs for a handful of blocks, but for stairs, slabs,
    /// fences and walls the render boxes are the right answer and everything
    /// else is close enough to stand on.
    pub collision: Vec<Aabb>,
    /// True if this block fills its cell and hides what is behind it.
    ///
    /// Taken from the geometry rather than guessed from the name. A block whose
    /// model is a full cube with all six faces occludes; a shelf, a slab or a
    /// lantern does not, whatever it is called.
    pub occludes: bool,
    /// True for water and lava.
    ///
    /// Fluids have no model in the game files at all: `block/water.json`
    /// declares a particle texture and nothing else, because vanilla draws them
    /// with a dedicated renderer that varies their height. Without a special
    /// case an ocean simply does not exist.
    pub fluid: bool,
}

impl BakedModel {
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }
}

/// What one state resolved to before the atlas existed.
#[derive(Clone)]
struct Pending {
    model: Option<BlockModel>,
    tint: TintSource,
    /// The still texture, for water and lava.
    fluid: Option<String>,
}

impl neuton_world::BlockShapes for BlockTextures {
    #[inline]
    fn collision(&self, state: StateId) -> &[Aabb] {
        &self.model(state).collision
    }
}

/// Baked models for every block state.
/// How many pictures the game has of a block part way to broken.
pub const DESTROY_STAGES: u32 = 10;

/// Where one of those pictures lives, in the form the atlas keys on.
pub fn destroy_stage_texture(stage: u32) -> String {
    format!(
        "assets/minecraft/textures/block/destroy_stage_{}.png",
        stage.min(DESTROY_STAGES - 1)
    )
}

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

        // Two passes: the atlas cannot be stitched before the textures it needs
        // are known, and faces cannot be baked before the atlas exists.
        let mut wanted = std::collections::BTreeSet::new();
        let mut raw: Vec<Option<Pending>> = vec![None; STATE_COUNT];

        for i in 0..BLOCK_COUNT {
            let block = neuton_blocks::BlockId(i);
            let tint = neuton_assets::tint::source_for(block.name());
            // Checked per block rather than per state: a fluid has no model to
            // resolve, so it would otherwise be dropped before it is ever baked.
            let fluid = fluid_texture(block.name());
            if let Some(texture) = &fluid {
                wanted.insert(texture.clone());
            }

            let b = block.get();
            for id in b.first_state.0..b.first_state.0 + b.state_count {
                let state = StateId(id);
                let model = resolver.model(packs, block.name(), state.variant_key());
                if let Some(model) = &model {
                    for path in model.textures() {
                        wanted.insert(path.to_string());
                    }
                }
                if model.is_some() || fluid.is_some() {
                    raw[id as usize] = Some(Pending { model, tint, fluid: fluid.clone() });
                }
            }
        }

        // The cracks that spread over a block as it is broken. No model refers
        // to them, so they have to be asked for by name or they are not in the
        // atlas to draw with.
        for stage in 0..DESTROY_STAGES {
            wanted.insert(destroy_stage_texture(stage));
        }

        let paths: Vec<String> = wanted.into_iter().collect();
        let atlas = Atlas::stitch(packs, &paths);
        let _ = Tints::load(packs);

        let mut models: Vec<BakedModel> = vec![BakedModel::default()];
        let mut lookup: std::collections::HashMap<String, u16> = Default::default();
        let mut index = vec![0u16; STATE_COUNT];

        for (id, entry) in raw.iter().enumerate() {
            let Some(pending) = entry else { continue };
            let baked = match (&pending.fluid, &pending.model) {
                (Some(texture), _) => bake_fluid(texture, &atlas, pending.tint),
                (None, Some(model)) => bake(model, &atlas, pending.tint),
                (None, None) => continue,
            };
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
fn bake(model: &BlockModel, atlas: &Atlas, tint: TintSource) -> BakedModel {
    let elements: Vec<BakedElement> = model
        .elements
        .iter()
        .map(|element| {
            let mut from = element.from;
            let mut to = element.to;
            let mut faces: [Option<BakedFace>; 6] = Default::default();
            for (i, face) in element.faces.iter().enumerate() {
                let Some(face) = face else { continue };
                faces[i] = Some(BakedFace {
                    uv: map_uv(atlas.uv(&face.texture), face.uv),
                    tint: if face.tinted { tint } else { TintSource::None },
                    cullface: face.cullface,
                });
            }

            // Blockstate rotation, applied here so the mesher never has to,
            // and per element because a multipart block is several parts each
            // turned a different way. Vanilla applies x first, then y.
            for _ in 0..(element.x_rot.rem_euclid(360) / 90) {
                rotate_x(&mut from, &mut to, &mut faces);
            }
            for _ in 0..(element.y_rot.rem_euclid(360) / 90) {
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
    // A block hides what is behind it only if it is a full cube, has all six
    // faces, and none of those faces can be seen through. Geometry alone is not
    // enough: leaves and glass are full cubes with holes in their textures.
    let occludes = model.elements.iter().any(|e| {
        e.is_full_cube()
            && e.faces.iter().all(|f| {
                f.as_ref().is_some_and(|face| atlas.is_opaque(&face.texture))
            })
    });
    let collision = if passes_through(model) {
        Vec::new()
    } else {
        elements
            .iter()
            .map(|e| Aabb::new([e.from[0] as f64, e.from[1] as f64, e.from[2] as f64],
                               [e.to[0] as f64, e.to[1] as f64, e.to[2] as f64]))
            .collect()
    };
    BakedModel { elements, fluid: false, occludes, collision }
}

/// Whether a block can be walked through.
///
/// Not in the data anywhere: the game decides it in code per block. Flowers,
/// torches, rails and signs all have geometry and none of them stop you, and
/// walking into a tulip as though it were a fence post is worse than passing
/// through a block that should have held you.
fn passes_through(model: &BlockModel) -> bool {
    // Model space here is 0..16.
    model.elements.iter().all(|e| {
        let size = [
            e.to[0] - e.from[0],
            e.to[1] - e.from[1],
            e.to[2] - e.from[2],
        ];
        // A plant is a cross: two planes with no thickness at all.
        let flat = size.iter().any(|s| *s <= 0.1);
        // A torch, button or lever is a small prop stuck to a surface. A fence
        // post has the same footprint but runs the full height of the block,
        // and does stop you.
        let small_prop = size[0].max(size[2]) < 8.0 && size[1] < 14.0;
        flat || small_prop
    })
}

/// Places a model face's texture region inside its atlas tile.
///
/// Model space runs 0..16 with v downwards, and the four corners come back in
/// the order the mesher emits its vertices.
fn map_uv(tile: Uv, uv: [f32; 4]) -> [[f32; 2]; 4] {
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * (t / 16.0);
    let u1 = lerp(tile.min[0], tile.max[0], uv[0]);
    let v1 = lerp(tile.min[1], tile.max[1], uv[1]);
    let u2 = lerp(tile.min[0], tile.max[0], uv[2]);
    let v2 = lerp(tile.min[1], tile.max[1], uv[3]);
    [[u1, v2], [u2, v2], [u2, v1], [u1, v1]]
}

/// The still texture a fluid draws with, or `None` for anything else.
fn fluid_texture(block: &str) -> Option<String> {
    let name = block.trim_start_matches("minecraft:");
    let still = match name {
        "water" | "bubble_column" => "water_still",
        "lava" => "lava_still",
        _ => return None,
    };
    Some(format!("assets/minecraft/textures/block/{still}.png"))
}

/// Builds the cube a fluid draws as.
///
/// A full cube here; the mesher lowers the top face when there is air above,
/// which is the only place the neighbour needed to decide that is available.
fn bake_fluid(texture: &str, atlas: &Atlas, tint: TintSource) -> BakedModel {
    let face = Some(BakedFace {
        uv: map_uv(atlas.uv(texture), [0.0, 0.0, 16.0, 16.0]),
        tint,
        // Every face can be hidden, including by another fluid block: an ocean
        // is a surface, not a volume.
        cullface: None,
    });
    let mut faces: [Option<BakedFace>; 6] = Default::default();
    for (i, slot) in faces.iter_mut().enumerate() {
        *slot = face;
        // Only the sides and bottom cull against a neighbour; the top is
        // handled by the mesher, which knows whether this block is submerged.
        if let Some(f) = slot.as_mut() {
            f.cullface = Some(i as u8);
        }
    }
    BakedModel {
        elements: vec![BakedElement {
            from: [0.0, 0.0, 0.0],
            to: [1.0, 1.0, 1.0],
            faces,
            full_cube: true,
        }],
        fluid: true,
        // A fluid is see-through: it must not hide the seabed behind it.
        occludes: false,
        // And you swim through it rather than standing on it.
        collision: Vec::new(),
    }
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
    // The box map sends up to north, north to down, down to south and south to
    // up, so the face that ends up at north is the one that was at up.
    // down=0 up=1 north=2 south=3 west=4 east=5
    permute(faces, [2, 3, 1, 0, 4, 5]);
}

/// Rotates a box and its faces 90 degrees about the Y axis, clockwise seen from
/// above: north becomes east.
fn rotate_y(from: &mut [f32; 3], to: &mut [f32; 3], faces: &mut [Option<BakedFace>; 6]) {
    let map = |p: [f32; 3]| [16.0 - p[2], p[1], p[0]];
    let (a, b) = (map(*from), map(*to));
    *from = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
    *to = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
    // North becomes east, east becomes south, and so on, so the face that ends
    // up at north is the one that was at west.
    permute(faces, [0, 1, 4, 5, 3, 2]);
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
    let _ = write!(out, "{}{}", model.fluid, model.occludes);
    for e in &model.elements {
        let _ = write!(out, "{:?}{:?}{}", e.from, e.to, e.full_cube);
        for face in &e.faces {
            match face {
                Some(f) => {
                    let _ = write!(out, "|{:?}{:?}", f.uv, f.tint);
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
        assert_eq!(top.tint, TintSource::Grass, "the top takes the grass colormap");
        let bottom = m.elements[0].faces[0].expect("grass has a bottom face");
        assert_eq!(bottom.tint, TintSource::None, "the dirt underneath is not tinted");
    }

    #[test]
    fn leaves_are_tinted_on_every_face() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        let leaves = neuton_blocks::by_name("minecraft:oak_leaves").unwrap().get().default_state;
        for face in t.model(leaves).elements[0].faces.iter().flatten() {
            assert_eq!(face.tint, TintSource::Foliage, "leaf face not tinted");
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
    fn fluids_get_geometry_despite_having_no_model() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);

        let water = neuton_blocks::by_name("minecraft:water").unwrap().get().default_state;
        let m = t.model(water);
        assert!(m.fluid, "water must be recognised as a fluid");
        assert_eq!(m.elements.len(), 1, "water has no model, so one is synthesised");
        // Tinted from the biome's water colour, or an ocean comes out grey.
        let face = m.elements[0].faces[1].expect("water has a top face");
        assert_eq!(face.tint, TintSource::Water);

        let lava = neuton_blocks::by_name("minecraft:lava").unwrap().get().default_state;
        assert!(t.model(lava).fluid);

        // And nothing else is claimed as a fluid.
        let stone = neuton_blocks::by_name("minecraft:stone").unwrap().get().default_state;
        assert!(!t.model(stone).fluid);
    }

    #[test]
    fn biome_tints_resolve_and_fall_back() {
        use neuton_net::{BiomeColors, GrassModifier};
        let Some(mut packs) = packs() else { return };
        let tints = Tints::load(&mut packs);

        let biome = |name: &str, temp: f32, rain: f32, m: GrassModifier| BiomeColors {
            name: name.into(),
            temperature: temp,
            downfall: rain,
            grass: None,
            foliage: None,
            water: None,
            grass_modifier: m,
        };
        let table = BiomeTints::build(
            &[
                biome("plains", 0.8, 0.4, GrassModifier::None),
                biome("desert", 2.0, 0.0, GrassModifier::None),
                biome("swamp", 0.8, 0.9, GrassModifier::Swamp),
                BiomeColors {
                    grass: Some(0xFF0000),
                    ..biome("weird", 0.8, 0.4, GrassModifier::None)
                },
            ],
            &tints,
        );

        let grass_of = |i| table.get(i, TintSource::Grass);
        assert_ne!(grass_of(0), grass_of(1), "climate should change the colour");
        assert_ne!(grass_of(0), grass_of(2), "the swamp modifier should apply");
        // An explicit override wins over the colormap entirely.
        assert_eq!(grass_of(3), [1.0, 0.0, 0.0]);
        // An unknown biome id falls back rather than reading out of bounds.
        let fallback = grass_of(999);
        assert!(fallback[1] > fallback[0], "fallback should still be green");
        // Untinted faces are untouched whatever the biome.
        assert_eq!(table.get(1, TintSource::None), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn occlusion_follows_the_geometry_not_the_name() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        let occludes = |name: &str| {
            t.model(neuton_blocks::by_name(name).unwrap().get().default_state).occludes
        };

        assert!(occludes("minecraft:stone"));
        assert!(occludes("minecraft:dirt"));
        assert!(occludes("minecraft:oak_planks"));

        // Partial geometry cannot hide what is behind it, whatever it is
        // called. A shelf is the case that gave this away: nothing in its name
        // says it is not a cube, and treating it as one deleted the top face of
        // the block underneath.
        assert!(!occludes("minecraft:oak_slab"));
        assert!(!occludes("minecraft:oak_stairs"));
        assert!(!occludes("minecraft:lantern"));
        assert!(!occludes("minecraft:oak_fence"));
        assert!(!occludes("minecraft:water"));
        if neuton_blocks::by_name("minecraft:oak_shelf").is_some() {
            assert!(!occludes("minecraft:oak_shelf"));
        }
    }

    #[test]
    fn face_uvs_follow_the_model_not_the_whole_tile() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);

        // A lantern's sides show a 6x7 patch of a 16x16 texture. Using the
        // whole tile stretches the image over the box and looks like a smear.
        let lantern = neuton_blocks::by_name("minecraft:lantern").unwrap().get().default_state;
        let side = t.model(lantern).elements[0].faces[2].expect("lantern has a north face");
        let stone = neuton_blocks::by_name("minecraft:stone").unwrap().get().default_state;
        let whole = t.model(stone).elements[0].faces[2].expect("stone has a north face");

        let span = |uv: [[f32; 2]; 4]| (uv[1][0] - uv[0][0]).abs();
        assert!(
            span(side.uv) < span(whole.uv) * 0.9,
            "lantern face should cover less than a whole tile: {} vs {}",
            span(side.uv),
            span(whole.uv)
        );
    }

    #[test]
    fn a_stair_faces_the_way_its_blockstate_says() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        let stairs = neuton_blocks::by_name("minecraft:oak_stairs").unwrap();
        let b = stairs.get();

        let model_for = |variant: &str| {
            let state = (b.first_state.0..b.first_state.0 + b.state_count)
                .map(StateId)
                .find(|s| s.variant_key() == variant)
                .expect("variant exists");
            t.model(state).clone()
        };

        // The step of a bottom stair is the upper box; where it sits along X or
        // Z is what "facing" means. East is the unrotated model, so its step is
        // at the west end, and the other facings must differ from it and from
        // each other.
        let centre = |m: &BakedModel| {
            let upper = m
                .elements
                .iter()
                .max_by(|a, b| a.from[1].partial_cmp(&b.from[1]).unwrap())
                .expect("stairs have elements");
            [
                (upper.from[0] + upper.to[0]) / 2.0,
                (upper.from[2] + upper.to[2]) / 2.0,
            ]
        };

        let east = centre(&model_for("facing=east,half=bottom,shape=straight,waterlogged=false"));
        let west = centre(&model_for("facing=west,half=bottom,shape=straight,waterlogged=false"));
        let north = centre(&model_for("facing=north,half=bottom,shape=straight,waterlogged=false"));
        let south = centre(&model_for("facing=south,half=bottom,shape=straight,waterlogged=false"));

        // Opposite facings put the step on opposite sides.
        assert!((east[0] - west[0]).abs() > 0.2, "east and west steps coincide: {east:?} {west:?}");
        assert!(
            (north[1] - south[1]).abs() > 0.2,
            "north and south steps coincide: {north:?} {south:?}"
        );
        // And the two axes are genuinely different orientations.
        assert!((east[0] - north[0]).abs() > 0.2 || (east[1] - north[1]).abs() > 0.2);
    }

    #[test]
    fn you_walk_through_plants_and_into_fences() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        let solid = |name: &str| {
            !t.model(neuton_blocks::by_name(name).unwrap().get().default_state)
                .collision
                .is_empty()
        };

        // Things you stand on or bump into.
        assert!(solid("minecraft:stone"));
        assert!(solid("minecraft:oak_slab"));
        assert!(solid("minecraft:oak_stairs"));
        assert!(solid("minecraft:oak_fence"), "a fence post is thin but full height");
        assert!(solid("minecraft:cobblestone_wall"));

        // Things you walk through.
        assert!(!solid("minecraft:poppy"));
        assert!(!solid("minecraft:short_grass"));
        assert!(!solid("minecraft:torch"));
        assert!(!solid("minecraft:water"), "you swim, not stand");
    }

    #[test]
    fn collision_boxes_match_the_shape() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);

        let stone = neuton_blocks::by_name("minecraft:stone").unwrap().get().default_state;
        let full = &t.model(stone).collision;
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].min, [0.0, 0.0, 0.0]);
        assert_eq!(full[0].max, [1.0, 1.0, 1.0]);

        // A bottom slab is half a block tall, so you stand at 0.5.
        let slab = neuton_blocks::by_name("minecraft:stone_slab").unwrap().get().default_state;
        let boxes = &t.model(slab).collision;
        assert_eq!(boxes.len(), 1);
        assert!((boxes[0].max[1] - 0.5).abs() < 1e-6, "slab top at {}", boxes[0].max[1]);
    }

    #[test]
    fn an_out_of_range_state_is_safe() {
        let Some(mut packs) = packs() else { return };
        let t = BlockTextures::build(&mut packs);
        assert!(t.model(StateId(u32::MAX)).is_empty());
        assert!(t.model(StateId(STATE_COUNT as u32)).is_empty());
    }
}
