//! Turning chunk sections into triangles.
//!
//! One quad per visible face of every box in a block's model, so a slab is half
//! a block tall and a fence is a post. Faces are dropped when the model says a
//! neighbour hides them, which is where nearly all the geometry goes: the
//! inside of the world, and the inside of every ocean, is most of the world.

use crate::textures::{BakedElement, BlockTextures};
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
    pub tint: [f32; 3],
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
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Whether a block fills its cell and whether it hides its own internal faces.
pub trait BlockAppearance {
    fn is_opaque(&self, state: StateId) -> bool;

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

/// Builds the mesh for one chunk column.
pub fn build(chunk: &Chunk, look: &dyn BlockAppearance, textures: &BlockTextures) -> Mesh {
    let mut mesh = Mesh::default();
    let sections = chunk.sections.len();

    // The whole column at once, so a face at a section boundary can see the
    // block above or below it. Meshing sections in isolation leaves a seam of
    // wrongly-drawn faces every sixteen blocks.
    let mut column = vec![0u32; sections * SECTION_VOLUME];
    let mut scratch = vec![0u32; SECTION_VOLUME];
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

    let height = (sections * 16) as i32;
    let at = |x: i32, y: i32, z: i32| -> u32 {
        if !(0..16).contains(&x) || !(0..16).contains(&z) || !(0..height).contains(&y) {
            // Outside the column. Treated as empty, which draws faces at the
            // chunk border a neighbour may cover; harmless, and fixed once
            // neighbours are meshed together.
            return 0;
        }
        column[(y as usize / 16) * SECTION_VOLUME
            + block_index(x as usize, y as usize % 16, z as usize)]
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
                let base = [x as f32, (y + chunk.min_y) as f32, z as f32];
                for element in &model.elements {
                    push_element(&mut mesh, base, element, state, look, &at, x, y, z);
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
    x: i32,
    y: i32,
    z: i32,
) {
    for index in 0..6u8 {
        let Some(face) = element.faces[index as usize] else { continue };

        // Only a face the model marked with a cullface can be hidden. Anything
        // else is interior geometry that stays visible whatever is next to it.
        if let Some(cull) = face.cullface {
            let [dx, dy, dz] = Face::from_index(cull).offset();
            let neighbour = StateId(at(x + dx, y + dy, z + dz));
            if look.hides_face(state, neighbour) {
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
            base[1] + element.to[1],
            base[2] + element.to[2],
        ];

        let start = mesh.vertices.len() as u32;
        let light = dir.shade();
        for (corner, uv) in dir.corners(from, to).iter().zip(face.uv.corners()) {
            mesh.vertices.push(Vertex {
                position: *corner,
                uv,
                tint: face.tint,
                light,
            });
        }
        mesh.indices.extend_from_slice(&[
            start,
            start + 1,
            start + 2,
            start,
            start + 2,
            start + 3,
        ]);
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
    fn a_single_solid_section_only_draws_its_shell() {
        let Some(t) = textures() else { return };
        let mesh = build(&chunk(1, stone(), |_| true), &Solid, &t);
        assert_eq!(mesh.triangles(), 6 * 256 * 2);
    }

    #[test]
    fn stacked_sections_do_not_draw_the_seam_between_them() {
        let Some(t) = textures() else { return };
        let mesh = build(&chunk(2, stone(), |_| true), &Solid, &t);
        // Top and bottom stay 256 each; the four sides become 16x32.
        assert_eq!(mesh.triangles(), (2 * 256 + 4 * 16 * 32) * 2);
    }

    #[test]
    fn faces_carry_their_directional_shade() {
        let Some(t) = textures() else { return };
        let mesh = build(&chunk(1, stone(), |_| true), &Solid, &t);
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
        assert!(mesh.vertices.iter().all(|v| v.tint == [1.0, 1.0, 1.0]));
    }

    #[test]
    fn geometry_stays_inside_the_column() {
        let Some(t) = textures() else { return };
        let mesh = build(&chunk(2, stone(), |i| i == 1), &Solid, &t);
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
