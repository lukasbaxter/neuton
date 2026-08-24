//! Turning chunk sections into triangles.
//!
//! One quad per visible face of every box in a block's model, so a slab is half
//! a block tall and a fence is a post. Faces are dropped when the model says a
//! neighbour hides them, which is where nearly all the geometry goes: the
//! inside of the world, and the inside of every ocean, is most of the world.

use crate::textures::{BakedElement, BiomeTints, BlockTextures};
use neuton_world::BIOME_VOLUME;
use neuton_blocks::StateId;
use neuton_world::{Chunk, SECTION_VOLUME, block_index};

/// One corner of a face.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// Position relative to the chunk column's origin.
    pub position: [f32; 3],
    /// Atlas coordinates.
    pub uv: [f32; 2],
    /// Biome tint, multiplied into the texture. White for untinted faces.
    /// The fourth channel carries how opaque the block is.
    pub tint: [f32; 4],
    /// Directional shade, applied per face.
    pub light: f32,
}

/// The six faces, in the order models and the wire both use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Face {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

impl Face {
    pub const ALL: [Face; 6] =
        [Face::Down, Face::Up, Face::North, Face::South, Face::West, Face::East];

    pub const fn from_index(i: u8) -> Face {
        match i {
            0 => Face::Down,
            1 => Face::Up,
            2 => Face::North,
            3 => Face::South,
            4 => Face::West,
            _ => Face::East,
        }
    }

    /// Offset to the neighbouring cell across this face.
    pub const fn offset(self) -> [i32; 3] {
        match self {
            Face::Down => [0, -1, 0],
            Face::Up => [0, 1, 0],
            Face::North => [0, 0, -1],
            Face::South => [0, 0, 1],
            Face::West => [-1, 0, 0],
            Face::East => [1, 0, 0],
        }
    }

    /// Fixed directional shading, matching the values vanilla uses.
    ///
    /// Without it every face of a cube is the same colour and the world reads
    /// as a flat silhouette rather than as geometry.
    pub const fn shade(self) -> f32 {
        match self {
            Face::Up => 1.0,
            Face::Down => 0.5,
            Face::North | Face::South => 0.8,
            Face::West | Face::East => 0.6,
        }
    }

    /// Axes and corner signs used for ambient occlusion.
    ///
    /// `u` and `v` are the two axes in the plane of the face, and each corner
    /// names its position along them. The order matches [`Face::corners`],
    /// which is what lets a corner's occlusion be attached to its vertex.
    const fn ao_basis(self) -> ([i32; 3], usize, usize, [(i32, i32); 4]) {
        match self {
            Face::Down => (
                [0, -1, 0], 0, 2,
                [(-1, -1), (1, -1), (1, 1), (-1, 1)],
            ),
            Face::Up => (
                [0, 1, 0], 0, 2,
                [(-1, 1), (1, 1), (1, -1), (-1, -1)],
            ),
            Face::North => (
                [0, 0, -1], 0, 1,
                [(1, -1), (-1, -1), (-1, 1), (1, 1)],
            ),
            Face::South => (
                [0, 0, 1], 0, 1,
                [(-1, -1), (1, -1), (1, 1), (-1, 1)],
            ),
            Face::West => (
                [-1, 0, 0], 2, 1,
                [(-1, -1), (1, -1), (1, 1), (-1, 1)],
            ),
            Face::East => (
                [1, 0, 0], 2, 1,
                [(1, -1), (-1, -1), (-1, 1), (1, 1)],
            ),
        }
    }

    /// The four corners of this face on a box, counter-clockwise seen from
    /// outside so back-face culling keeps the right ones.
    fn corners(self, from: [f32; 3], to: [f32; 3]) -> [[f32; 3]; 4] {
        let (x0, y0, z0) = (from[0], from[1], from[2]);
        let (x1, y1, z1) = (to[0], to[1], to[2]);
        match self {
            Face::Down => [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
            Face::Up => [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
            Face::North => [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
            Face::South => [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
            Face::West => [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
            Face::East => [[x1, y0, z1], [x1, y0, z0], [x1, y1, z0], [x1, y1, z1]],
        }
    }
}

/// Geometry for one chunk column.
#[derive(Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    /// Cutout geometry, drawn first with depth writes on.
    pub indices: Vec<u32>,
    /// Water, ice and stained glass, drawn afterwards with blending on and
    /// depth writes off so they do not hide each other.
    pub translucent: Vec<u32>,
}

impl Mesh {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty() && self.translucent.is_empty()
    }

    pub fn triangles(&self) -> usize {
        (self.indices.len() + self.translucent.len()) / 3
    }
}

/// How much light each occlusion level keeps.
///
/// Level 3 is an open corner, 0 is one wedged between two blocks and a
/// diagonal. Vanilla's smooth lighting is the same idea, and it is most of what
/// makes a Minecraft world read as solid rather than as flat coloured planes.
const AO_LEVELS: [f32; 4] = [0.46, 0.66, 0.84, 1.0];

/// Brightness of a light level, on vanilla's curve.
///
/// Deliberately not linear: level 7 is about a fifth as bright as level 15, not
/// half, which is why a torch lights a small bright pool rather than a large
/// dim one.
#[inline]
fn light_brightness(level: f32) -> f32 {
    let f = (level / 15.0).clamp(0.0, 1.0);
    // A floor, so an unlit cave is very dark but not featureless black.
    (f / (4.0 - 3.0 * f)).max(0.05)
}

/// Combines sky and block light the way the game does: whichever is brighter
/// wins, rather than the two adding up.
#[inline]
fn combine_light(sky: f32, block: f32, daylight: f32) -> f32 {
    (sky * daylight).max(block)
}

/// Occlusion for one corner, from the three blocks around it.
///
/// Two sides touching means the corner is fully enclosed however the diagonal
/// falls, which is why that case short-circuits.
#[inline]
fn corner_ao(side1: bool, side2: bool, corner: bool) -> usize {
    if side1 && side2 {
        return 0;
    }
    3 - (side1 as usize + side2 as usize + corner as usize)
}

/// Whether a block fills its cell and whether it hides its own internal faces.
pub trait BlockAppearance {
    fn is_opaque(&self, state: StateId) -> bool;

    /// How opaque a block draws, 1 for everything that is not water, ice or
    /// stained glass.
    fn alpha(&self, _state: StateId) -> f32 {
        1.0
    }

    /// True if this block hides the faces between two of itself, as fluids and
    /// glass do. An ocean is millions of blocks; without this it is drawn as a
    /// volume rather than a surface.
    fn self_culls(&self, _state: StateId) -> bool {
        false
    }

    /// Whether `neighbour` hides the face of `own` that touches it.
    fn hides_face(&self, own: StateId, neighbour: StateId) -> bool {
        self.is_opaque(neighbour)
            || (own.block() == neighbour.block()
                && !neighbour.is_air()
                && self.self_culls(own))
    }
}

/// The four columns around a chunk, where they are loaded.
///
/// Without them a chunk cannot tell whether a block at its edge is against open
/// air or against its neighbour's stone. Guessing either way is visibly wrong:
/// assume air and every column draws its full depth at all four borders, assume
/// solid and a cliff at a chunk boundary has a hole in it.
#[derive(Default, Clone, Copy)]
pub struct Neighbours<'a> {
    pub west: Option<&'a Chunk>,
    pub east: Option<&'a Chunk>,
    pub north: Option<&'a Chunk>,
    pub south: Option<&'a Chunk>,
}

impl Neighbours<'_> {
    /// How many of the four are present.
    pub fn count(&self) -> usize {
        [self.west, self.east, self.north, self.south].iter().filter(|n| n.is_some()).count()
    }
}

/// Builds the mesh for one chunk column, with no neighbours loaded.
pub fn build(chunk: &Chunk, look: &dyn BlockAppearance, textures: &BlockTextures) -> Mesh {
    build_at(chunk, Neighbours::default(), look, textures, 1.0)
}

/// Builds a chunk's mesh at a given daylight level, 0 for night and 1 for noon.
pub fn build_at(
    chunk: &Chunk,
    neighbours: Neighbours<'_>,
    look: &dyn BlockAppearance,
    textures: &BlockTextures,
    daylight: f32,
) -> Mesh {
    build_full(chunk, neighbours, look, textures, &BiomeTints::default(), daylight)
}

/// Builds a chunk's mesh, colouring tinted faces by the biome they sit in.
pub fn build_full(
    chunk: &Chunk,
    neighbours: Neighbours<'_>,
    look: &dyn BlockAppearance,
    textures: &BlockTextures,
    biomes: &BiomeTints,
    daylight: f32,
) -> Mesh {
    let mut mesh = Mesh::default();
    let sections = chunk.sections.len();

    // The whole column at once, so a face at a section boundary can see the
    // block above or below it. Meshing sections in isolation leaves a seam of
    // wrongly-drawn faces every sixteen blocks.
    let mut column = vec![0u32; sections * SECTION_VOLUME];
    let mut scratch = vec![0u32; SECTION_VOLUME];
    // Biomes are stored per 4x4x4 cell, so a section holds 64 of them.
    let mut biome_ids = vec![0u32; sections * BIOME_VOLUME];
    let mut biome_scratch = vec![0u32; BIOME_VOLUME];
    for (i, section) in chunk.sections.iter().enumerate() {
        let target = &mut column[i * SECTION_VOLUME..(i + 1) * SECTION_VOLUME];
        if section.is_empty() {
            target.fill(0);
            continue;
        }
        if section.blocks.unpack_into(&mut scratch) {
            target.copy_from_slice(&scratch);
        } else {
            target.fill(0);
        }
    }
    for (i, section) in chunk.sections.iter().enumerate() {
        let target = &mut biome_ids[i * BIOME_VOLUME..(i + 1) * BIOME_VOLUME];
        if section.biomes.unpack_into(&mut biome_scratch) {
            target.copy_from_slice(&biome_scratch);
        }
    }

    let height = (sections * 16) as i32;
    let inside = |x: i32, y: i32, z: i32| {
        (0..16).contains(&x) && (0..16).contains(&z) && (0..height).contains(&y)
    };
    let at = |x: i32, y: i32, z: i32| -> u32 {
        if !inside(x, y, z) {
            return 0;
        }
        column[(y as usize / 16) * SECTION_VOLUME
            + block_index(x as usize, y as usize % 16, z as usize)]
    };

    /// Which neighbouring column a position falls into, and where in it.
    fn across<'a>(
        n: &Neighbours<'a>,
        x: i32,
        z: i32,
    ) -> Option<(&'a Chunk, usize, usize)> {
        // Only one step out is ever asked for, so a diagonal is not a case.
        if x < 0 {
            return n.west.map(|c| (c, 15usize, z as usize));
        }
        if x > 15 {
            return n.east.map(|c| (c, 0usize, z as usize));
        }
        if z < 0 {
            return n.north.map(|c| (c, x as usize, 15usize));
        }
        if z > 15 {
            return n.south.map(|c| (c, x as usize, 0usize));
        }
        None
    }

    /// What sits across a face when it is not in this column.
    ///
    /// Sideways, the neighbouring chunk does, and its own mesh will draw
    /// whatever belongs there. Assuming air instead makes every column draw its
    /// full depth at all four chunk borders, which for an ocean is most of the
    /// geometry in the world and looks like a grid of walls under the surface.
    ///
    /// Below the world is solid bedrock for these purposes. Above it is sky, so
    /// the top of the tallest block still gets a face.
    #[derive(Clone, Copy, PartialEq)]
    enum Outside {
        Hidden,
        Sky,
    }

    let outside = |y: i32| if y >= height { Outside::Sky } else { Outside::Hidden };

    let hides = |x: i32, y: i32, z: i32, own: StateId| -> bool {
        if inside(x, y, z) {
            return look.hides_face(own, StateId(at(x, y, z)));
        }
        if !(0..height).contains(&y) {
            return outside(y) == Outside::Hidden;
        }
        match across(&neighbours, x, z) {
            // The real block next door.
            Some((chunk, nx, nz)) => {
                let state = chunk.state_at(nx, y + chunk.min_y, nz).unwrap_or(StateId(0));
                look.hides_face(own, state)
            }
            // Not loaded yet. Hiding is the better guess: the neighbour will
            // arrive and draw whatever belongs there, and until it does a
            // missing face is less obvious than a wall.
            None => true,
        }
    };

    for y in 0..height {
        for z in 0..16i32 {
            for x in 0..16i32 {
                let state = StateId(at(x, y, z));
                if state.is_air() {
                    continue;
                }
                let model = textures.model(state);
                if model.is_empty() {
                    continue;
                }
                // One biome per 4x4x4 cell, in the same y, z, x order as blocks.
                let biome = biome_ids[(y as usize / 16) * BIOME_VOLUME
                    + ((y as usize % 16) / 4) * 16
                    + (z as usize / 4) * 4
                    + (x as usize / 4)];
                let base = [x as f32, (y + chunk.min_y) as f32, z as f32];
                // A fluid's surface sits just below the top of its block unless
                // there is more of the same fluid above it, which is what gives
                // an ocean a surface you can see the edge of rather than a flat
                // lid at block height.
                let surface = if model.fluid {
                    let above = StateId(at(x, y + 1, z));
                    if above.block() == state.block() { 1.0 } else { 14.0 / 16.0 }
                } else {
                    1.0
                };
                for element in &model.elements {
                    push_element(
                        &mut mesh, base, element, state, look, &at, &hides, x, y, z, chunk,
                        daylight, surface, biomes, biome,
                    );
                }
            }
        }
    }
    mesh
}

#[allow(clippy::too_many_arguments)]
fn push_element(
    mesh: &mut Mesh,
    base: [f32; 3],
    element: &BakedElement,
    state: StateId,
    look: &dyn BlockAppearance,
    at: &dyn Fn(i32, i32, i32) -> u32,
    hides: &dyn Fn(i32, i32, i32, StateId) -> bool,
    x: i32,
    y: i32,
    z: i32,
    chunk: &Chunk,
    daylight: f32,
    surface: f32,
    biomes: &BiomeTints,
    biome: u32,
) {
    // World Y, since light is indexed in world coordinates while the block loop
    // counts from the bottom of the column.
    let world_y = y + chunk.min_y;
    for index in 0..6u8 {
        let Some(face) = element.faces[index as usize] else { continue };

        // Only a face the model marked with a cullface can be hidden. Anything
        // else is interior geometry that stays visible whatever is next to it.
        if let Some(cull) = face.cullface {
            let [dx, dy, dz] = Face::from_index(cull).offset();
            if hides(x + dx, y + dy, z + dz, state) {
                continue;
            }
        }

        let dir = Face::from_index(index);
        let from = [
            base[0] + element.from[0],
            base[1] + element.from[1],
            base[2] + element.from[2],
        ];
        let to = [
            base[0] + element.to[0],
            base[1] + element.to[1] * surface,
            base[2] + element.to[2],
        ];

        let shade = dir.shade();

        let (normal, u_axis, v_axis, signs) = dir.ao_basis();
        let solid = |offset: [i32; 3]| {
            look.is_opaque(StateId(at(x + offset[0], y + offset[1], z + offset[2])))
        };
        // Light of the cell a face looks into, not of the block itself: a lit
        // surface is lit by what is in front of it.
        let light_of = |offset: [i32; 3]| {
            let (px, py, pz) = (x + offset[0], world_y + offset[1], z + offset[2]);
            if !(0..16).contains(&px) || !(0..16).contains(&pz) {
                // Outside the column, so the neighbour's light is unknown.
                // Daylight is the better guess than darkness, which would draw
                // a black seam down every chunk border.
                return (15.0, 0.0);
            }
            (
                chunk.lighting.sky_at(px as usize, py, pz as usize) as f32,
                chunk.lighting.block_at(px as usize, py, pz as usize) as f32,
            )
        };

        // Ambient occlusion and smooth lighting share the same four cells
        // around each corner: how many are solid decides the occlusion, and
        // their average light decides the brightness. Vanilla does both from
        // the same neighbourhood, which is why they agree at every seam.
        let mut ao = [3usize; 4];
        let mut lit = [1.0f32; 4];
        for (i, (su, sv)) in signs.iter().enumerate() {
            let mut side1 = normal;
            side1[u_axis] += su;
            let mut side2 = normal;
            side2[v_axis] += sv;
            let mut diagonal = normal;
            diagonal[u_axis] += su;
            diagonal[v_axis] += sv;

            if element.full_cube {
                // A partial box sits inside its cell, so the neighbours a
                // corner would sample are not the ones touching it, and the
                // occlusion reads as dirt rather than depth.
                ao[i] = corner_ao(solid(side1), solid(side2), solid(diagonal));
            }

            // Averaged over the cells that are not solid; a solid one has no
            // light of its own to contribute and would drag the corner dark.
            let mut sky_sum = 0.0;
            let mut block_sum = 0.0;
            let mut count = 0.0;
            for offset in [normal, side1, side2, diagonal] {
                if solid(offset) {
                    continue;
                }
                let (sky, block) = light_of(offset);
                sky_sum += sky;
                block_sum += block;
                count += 1.0;
            }
            lit[i] = if count > 0.0 {
                light_brightness(combine_light(sky_sum / count, block_sum / count, daylight))
            } else {
                let (sky, block) = light_of(normal);
                light_brightness(combine_light(sky, block, daylight))
            };
        }

        let alpha = look.alpha(state);
        let tint = biomes.get(biome, face.tint);
        let start = mesh.vertices.len() as u32;
        for (i, (corner, uv)) in dir.corners(from, to).iter().zip(face.uv).enumerate() {
            mesh.vertices.push(Vertex {
                position: *corner,
                uv,
                tint: [tint[0], tint[1], tint[2], alpha],
                light: shade * AO_LEVELS[ao[i]] * lit[i],
            });
        }

        let indices = if alpha < 1.0 { &mut mesh.translucent } else { &mut mesh.indices };

        // A quad is two triangles, and which diagonal they share is visible
        // when the corners are unevenly lit. Splitting along the darker
        // diagonal keeps the gradient symmetric instead of bending it.
        if ao[0] + ao[2] > ao[1] + ao[3] {
            indices.extend_from_slice(&[start + 1, start + 2, start + 3, start + 1, start + 3, start]);
        } else {
            indices.extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neuton_assets::{PackStack, vanilla_jar};

    fn textures() -> Option<BlockTextures> {
        let jar = vanilla_jar("26.2")?;
        let mut packs = PackStack::new();
        packs.push(&jar).ok()?;
        Some(BlockTextures::build(&mut packs))
    }

    struct Solid;
    impl BlockAppearance for Solid {
        fn is_opaque(&self, state: StateId) -> bool {
            !state.is_air()
        }
    }

    /// A column of `sections` sections filled with `state`.
    fn chunk(sections: usize, state: u32, solid: impl Fn(usize) -> bool) -> Chunk {
        use neuton_world::{Palette, PalettedContainer, Section};
        let make = |state: u32, count: u16| Section {
            block_count: count,
            fluid_count: 0,
            blocks: PalettedContainer {
                palette: Palette::Single(state),
                bits: 0,
                data: Vec::new(),
                len: SECTION_VOLUME,
            },
            biomes: PalettedContainer {
                palette: Palette::Single(0),
                bits: 0,
                data: Vec::new(),
                len: 64,
            },
        };
        Chunk {
            x: 0,
            z: 0,
            min_y: 0,
            sections: (0..sections)
                .map(|i| if solid(i) { make(state, 4096) } else { make(0, 0) })
                .collect(),
            heightmaps: Vec::new(),
            block_entities: Vec::new(),
            lighting: Default::default(),
        }
    }

    /// Builds a column from a per-block closure, so a test can make a shape
    /// rather than only a stack of uniform sections.
    fn chunk_from(sections: usize, block: impl Fn(usize, usize, usize) -> u32) -> Chunk {
        use neuton_world::{Palette, PalettedContainer, Section};
        let mut out = Vec::with_capacity(sections);
        for s in 0..sections {
            // Collect the section's states, then build a palette from them.
            let mut cells = vec![0u32; SECTION_VOLUME];
            let mut solid = 0u16;
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        let state = block(x, s * 16 + y, z);
                        cells[block_index(x, y, z)] = state;
                        if state != 0 {
                            solid += 1;
                        }
                    }
                }
            }
            let mut palette: Vec<u32> = cells.clone();
            palette.sort_unstable();
            palette.dedup();

            // Four bits per entry covers any palette these tests build.
            assert!(palette.len() <= 16, "test palette too large");
            let bits = 4usize;
            let per_word = 64 / bits;
            let mut data = vec![0u64; SECTION_VOLUME.div_ceil(per_word)];
            for (i, cell) in cells.iter().enumerate() {
                let index = palette.iter().position(|p| p == cell).unwrap() as u64;
                data[i / per_word] |= index << ((i % per_word) * bits);
            }

            out.push(Section {
                block_count: solid,
                fluid_count: 0,
                blocks: PalettedContainer {
                    palette: Palette::Indirect(palette),
                    bits: bits as u8,
                    data,
                    len: SECTION_VOLUME,
                },
                biomes: PalettedContainer {
                    palette: Palette::Single(0),
                    bits: 0,
                    data: Vec::new(),
                    len: 64,
                },
            });
        }
        Chunk {
            x: 0,
            z: 0,
            min_y: 0,
            sections: out,
            heightmaps: Vec::new(),
            block_entities: Vec::new(),
            lighting: Default::default(),
        }
    }

    fn stone() -> u32 {
        neuton_blocks::by_name("minecraft:stone").unwrap().get().default_state.0
    }

    #[test]
    fn an_empty_column_produces_nothing() {
        let Some(t) = textures() else { return };
        let mesh = build(&chunk(4, stone(), |_| false), &Solid, &t);
        assert!(mesh.is_empty());
    }

    #[test]
    fn a_solid_column_draws_only_its_top() {
        let Some(t) = textures() else { return };
        // The sides abut neighbouring chunks, which draw their own geometry,
        // and the bottom is the floor of the world.
        let mesh = build(&chunk(1, stone(), |_| true), &Solid, &t);
        assert_eq!(mesh.triangles(), 256 * 2);
    }

    #[test]
    fn stacked_sections_do_not_draw_the_seam_between_them() {
        let Some(t) = textures() else { return };
        // Two sections rather than one, and still one surface: the boundary
        // between them is interior, not a face.
        let mesh = build(&chunk(2, stone(), |_| true), &Solid, &t);
        assert_eq!(mesh.triangles(), 256 * 2);
    }

    #[test]
    fn faces_carry_their_directional_shade() {
        let Some(t) = textures() else { return };
        // A single isolated section: its shell has nothing beside it, so every
        // corner is open and the only variation is the face direction.
        // One block alone in the air, so all six faces are drawn and none are
        // occluded.
        let mesh = build(
            &chunk_from(1, |x, y, z| if (x, y, z) == (8, 8, 8) { stone() } else { 0 }),
            &Solid,
            &t,
        );
        let mut lights: Vec<f32> = mesh.vertices.iter().map(|v| v.light).collect();
        lights.sort_by(|a, b| a.partial_cmp(b).unwrap());
        lights.dedup();
        // No light arrays means full daylight, so brightness is 1 and only the
        // face direction varies.
        assert_eq!(lights, vec![0.5, 0.6, 0.8, 1.0]);
    }

    #[test]
    fn light_levels_follow_a_curve_rather_than_a_line() {
        assert!((light_brightness(15.0) - 1.0).abs() < 1e-6);
        // Half-way is much less than half as bright.
        assert!(light_brightness(7.5) < 0.35, "{}", light_brightness(7.5));
        // Never fully black, so an unlit cave still has shape.
        assert!(light_brightness(0.0) > 0.0);
        assert!(light_brightness(-5.0) > 0.0, "clamps below zero");
        assert!(light_brightness(99.0) <= 1.0, "clamps above fifteen");
    }

    #[test]
    fn sky_and_block_light_do_not_add_up() {
        // Two dim sources stay dim; the brighter one wins.
        assert_eq!(combine_light(7.0, 7.0, 1.0), 7.0);
        assert_eq!(combine_light(4.0, 12.0, 1.0), 12.0);
        // At night the sky contributes nothing and torches carry the world.
        assert_eq!(combine_light(15.0, 6.0, 0.0), 6.0);
        assert_eq!(combine_light(15.0, 0.0, 1.0), 15.0);
    }

    #[test]
    fn corner_occlusion_darkens_towards_enclosure() {
        // Open corner keeps everything; two touching sides take the most.
        assert_eq!(corner_ao(false, false, false), 3);
        assert_eq!(corner_ao(true, false, false), 2);
        assert_eq!(corner_ao(true, false, true), 1);
        assert_eq!(corner_ao(true, true, false), 0);
        // Both sides solid is fully enclosed whatever the diagonal does.
        assert_eq!(corner_ao(true, true, true), corner_ao(true, true, false));
        assert!(AO_LEVELS[0] < AO_LEVELS[3]);
    }

    #[test]
    fn an_inside_corner_is_darker_than_a_flat_wall() {
        let Some(t) = textures() else { return };
        let s = stone();

        // A flat floor: every top face is out in the open.
        let flat = build(&chunk_from(1, |_, y, _| if y == 0 { s } else { 0 }), &Solid, &t);
        // The same floor with a wall along one edge, which occludes the corner
        // where the two meet.
        let walled = build(
            &chunk_from(1, |x, y, _| if y == 0 || (x == 0 && y < 4) { s } else { 0 }),
            &Solid,
            &t,
        );

        let darkest = |m: &Mesh| m.vertices.iter().fold(1.0f32, |a, v| a.min(v.light));
        assert!(!flat.is_empty() && !walled.is_empty());
        assert!(
            darkest(&walled) < darkest(&flat),
            "the inside corner should be darker: {} vs {}",
            darkest(&walled),
            darkest(&flat)
        );
    }

    #[test]
    fn occlusion_leaves_an_isolated_block_evenly_lit() {
        let Some(t) = textures() else { return };
        let s = stone();
        // One block alone in the air: nothing touches it, so every corner is
        // open and only the face direction varies.
        let mesh = build(
            &chunk_from(1, |x, y, z| if (x, y, z) == (8, 8, 8) { s } else { 0 }),
            &Solid,
            &t,
        );
        let mut lights: Vec<f32> = mesh.vertices.iter().map(|v| v.light).collect();
        lights.sort_by(|a, b| a.partial_cmp(b).unwrap());
        lights.dedup();
        assert_eq!(lights, vec![0.5, 0.6, 0.8, 1.0]);
    }

    #[test]
    fn a_slab_is_half_as_tall_as_a_block() {
        let Some(t) = textures() else { return };
        let slab = neuton_blocks::by_name("minecraft:stone_slab").unwrap().get().default_state.0;
        let mesh = build(&chunk(1, slab, |_| true), &Solid, &t);
        assert!(!mesh.is_empty());
        // Nothing reaches the top of the block, because a bottom slab does not.
        let highest = mesh.vertices.iter().fold(0.0f32, |m, v| m.max(v.position[1]));
        assert!(highest <= 15.5 + 1e-3, "slab geometry reaches {highest}");
    }

    #[test]
    fn leaves_carry_a_green_tint_and_stone_does_not() {
        let Some(t) = textures() else { return };
        let leaves = neuton_blocks::by_name("minecraft:oak_leaves").unwrap().get().default_state.0;
        let mesh = build(&chunk(1, leaves, |_| true), &Solid, &t);
        assert!(!mesh.is_empty());
        assert!(
            mesh.vertices.iter().all(|v| v.tint[1] > v.tint[0]),
            "leaves should be tinted green"
        );

        let mesh = build(&chunk(1, stone(), |_| true), &Solid, &t);
        assert!(mesh.vertices.iter().all(|v| v.tint[..3] == [1.0, 1.0, 1.0]));
    }

    #[test]
    fn a_loaded_neighbour_decides_the_border() {
        let Some(t) = textures() else { return };
        let s = stone();
        let solid = chunk(1, s, |_| true);
        let empty = chunk(1, s, |_| false);

        // Against open air next door, the whole west face is drawn.
        let exposed = build_at(
            &solid,
            Neighbours { west: Some(&empty), ..Default::default() },
            &Solid,
            &t,
            1.0,
        );
        // Against more stone, it is not.
        let buried = build_at(
            &solid,
            Neighbours { west: Some(&solid), ..Default::default() },
            &Solid,
            &t,
            1.0,
        );
        assert_eq!(
            exposed.triangles() - buried.triangles(),
            16 * 16 * 2,
            "one face of the column should appear"
        );
    }

    #[test]
    fn an_unloaded_neighbour_hides_rather_than_walls() {
        let Some(t) = textures() else { return };
        let s = stone();
        let solid = chunk(1, s, |_| true);
        let alone = build(&solid, &Solid, &t);
        let with_air = build_at(
            &solid,
            Neighbours { west: Some(&chunk(1, s, |_| false)), ..Default::default() },
            &Solid,
            &t,
            1.0,
        );
        assert!(alone.triangles() < with_air.triangles());
    }

    #[test]
    fn chunk_borders_do_not_grow_walls() {
        let Some(t) = textures() else { return };
        let s = stone();
        // A solid column. Its sides touch neighbouring chunks, and assuming air
        // there would draw all four of them: 4 x 16 x 16 faces on a single
        // section, dwarfing the 256 that actually belong on top.
        let mesh = build(&chunk(1, s, |_| true), &Solid, &t);
        assert_eq!(mesh.triangles(), 256 * 2);
    }

    #[test]
    fn the_top_of_the_world_still_gets_a_face() {
        let Some(t) = textures() else { return };
        let s = stone();
        // The highest block has sky above it, which is not another chunk.
        let mesh = build(&chunk_from(1, |_, y, _| if y == 15 { s } else { 0 }), &Solid, &t);
        let highest = mesh.vertices.iter().fold(0.0f32, |m, v| m.max(v.position[1]));
        assert!((highest - 16.0).abs() < 1e-3, "no top face at the world ceiling");
    }

    #[test]
    fn geometry_stays_inside_the_column() {
        let Some(t) = textures() else { return };
        let mesh = build(&chunk(2, stone(), |i| i == 1), &Solid, &t);
        assert!(!mesh.is_empty());
        for v in &mesh.vertices {
            assert!((0.0..=16.0).contains(&v.position[0]), "{v:?}");
            assert!((16.0..=32.0).contains(&v.position[1]), "{v:?}");
            assert!((0.0..=16.0).contains(&v.position[2]), "{v:?}");
        }
    }

    #[test]
    fn every_index_points_at_a_real_vertex() {
        let Some(t) = textures() else { return };
        let mesh = build(&chunk(2, stone(), |i| i == 0), &Solid, &t);
        assert!(!mesh.indices.is_empty());
        assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.vertices.len()));
        assert_eq!(mesh.indices.len() % 3, 0);
    }
}
