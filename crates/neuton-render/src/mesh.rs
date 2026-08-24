//! Turning chunk sections into triangles.
//!
//! Naive to begin with: one quad per visible block face, with faces between two
//! solid blocks skipped. That is the bulk of the win, since the inside of the
//! world is the overwhelming majority of it. Merging coplanar quads comes later
//! and lives behind this same interface.

use crate::textures::FaceUvs;
use neuton_blocks::StateId;
use neuton_world::{Chunk, SECTION_VOLUME, block_index};

/// One corner of a face.
///
/// Kept to 28 bytes so a chunk's worth of geometry stays cache-friendly. The
/// colour is per-vertex rather than per-face because face merging will later
/// want to interpolate across a merged quad.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// World position, relative to the chunk column's origin.
    pub position: [f32; 3],
    /// Atlas coordinates.
    pub uv: [f32; 2],
    /// Directional shade, applied per face.
    pub light: f32,
}

/// The six faces, in the order the mesher walks them.
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

    /// The four corners of this face on a unit cube, counter-clockwise seen
    /// from outside, so back-face culling keeps the right ones.
    pub const fn corners(self) -> [[f32; 3]; 4] {
        match self {
            Face::Down => [[0., 0., 0.], [1., 0., 0.], [1., 0., 1.], [0., 0., 1.]],
            Face::Up => [[0., 1., 1.], [1., 1., 1.], [1., 1., 0.], [0., 1., 0.]],
            Face::North => [[1., 0., 0.], [0., 0., 0.], [0., 1., 0.], [1., 1., 0.]],
            Face::South => [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
            Face::West => [[0., 0., 0.], [0., 0., 1.], [0., 1., 1.], [0., 1., 0.]],
            Face::East => [[1., 0., 1.], [1., 0., 0.], [1., 1., 0.], [1., 1., 1.]],
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

    fn push_face(&mut self, base: [f32; 3], face: Face, uv: neuton_assets::Uv) {
        let start = self.vertices.len() as u32;
        let light = face.shade();
        let corners = face.corners();
        let texcoords = uv.corners();
        for (corner, uv) in corners.iter().zip(texcoords) {
            self.vertices.push(Vertex {
                position: [base[0] + corner[0], base[1] + corner[1], base[2] + corner[2]],
                uv,
                light,
            });
        }
        // Two triangles per quad, sharing the diagonal.
        self.indices.extend_from_slice(&[
            start,
            start + 1,
            start + 2,
            start,
            start + 2,
            start + 3,
        ]);
    }
}

/// Decides what a block looks like and which of its faces are worth drawing.
pub trait BlockAppearance {
    /// True if this state fills its cell and blocks the face behind it.
    fn is_opaque(&self, state: StateId) -> bool;

    /// Whether `neighbour` hides the face of `own` that touches it.
    ///
    /// An opaque neighbour always does. The other case is two of the same
    /// see-through block meeting: an ocean is millions of water blocks, and
    /// drawing the faces between them means drawing the whole volume instead of
    /// just its surface. Minecraft culls those, and so does this.
    fn hides_face(&self, own: StateId, neighbour: StateId) -> bool {
        self.is_opaque(neighbour)
            || (own.block() == neighbour.block()
                && !neighbour.is_air()
                && self.self_culls(own))
    }

    /// True if this block hides its own internal faces, as fluids and glass do.
    fn self_culls(&self, _state: StateId) -> bool {
        false
    }

}

/// Supplies the atlas rectangles for a block's six faces.
pub trait FaceUvSource {
    fn face_uvs(&self, state: StateId) -> &FaceUvs;
}

/// Builds the mesh for one chunk column.
///
/// Blocks are unpacked one section at a time into a reused scratch buffer, so
/// meshing a chunk allocates only for the output.
pub fn build(chunk: &Chunk, look: &dyn BlockAppearance, textures: &dyn FaceUvSource) -> Mesh {
    let mut mesh = Mesh::default();
    let sections = chunk.sections.len();

    // The whole column, so a face at a section boundary can see the block above
    // or below it. Meshing sections in isolation leaves a visible seam of
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
            // chunk border that a neighbour may cover. Harmless with back-face
            // culling and fixed once neighbours are meshed together.
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
                let uvs = textures.face_uvs(state);
                let base = [x as f32, (y + chunk.min_y) as f32, z as f32];
                for face in Face::ALL {
                    let [dx, dy, dz] = face.offset();
                    let neighbour = StateId(at(x + dx, y + dy, z + dz));
                    // A face is drawn unless the neighbour hides it. This is
                    // where almost all the geometry disappears: the inside of
                    // the world, and the inside of every ocean, is most of it.
                    if !look.hides_face(state, neighbour) {
                        mesh.push_face(base, face, uvs[face as usize]);
                    }
                }
            }
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything non-air is a solid cube.
    struct Solid;
    impl BlockAppearance for Solid {
        fn is_opaque(&self, state: StateId) -> bool {
            !state.is_air()
        }
    }

    /// One tile covering the whole atlas, so geometry can be checked without
    /// needing a real installation.
    struct OneTile(FaceUvs);
    impl Default for OneTile {
        fn default() -> Self {
            Self([neuton_assets::Uv { min: [0.0, 0.0], max: [1.0, 1.0] }; 6])
        }
    }
    impl FaceUvSource for OneTile {
        fn face_uvs(&self, _state: StateId) -> &FaceUvs {
            &self.0
        }
    }

    /// A column of `sections` sections; `solid` decides which are filled.
    fn chunk(sections: usize, solid: impl Fn(usize) -> bool) -> Chunk {
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
                .map(|i| if solid(i) { make(1, 4096) } else { make(0, 0) })
                .collect(),
            heightmaps: Vec::new(),
            block_entities: Vec::new(),
        }
    }

    #[test]
    fn an_empty_column_produces_nothing() {
        let mesh = build(&chunk(4, |_| false), &Solid, &OneTile::default());
        assert!(mesh.is_empty());
        assert_eq!(mesh.vertices.len(), 0);
    }

    #[test]
    fn a_single_solid_section_only_draws_its_shell() {
        // One 16x16x16 section alone in the column. Interior faces are hidden,
        // so only the outside survives: six sides of 256 faces each.
        let mesh = build(&chunk(1, |_| true), &Solid, &OneTile::default());
        assert_eq!(mesh.triangles(), 6 * 256 * 2);
        assert_eq!(mesh.vertices.len(), 6 * 256 * 4);
    }

    #[test]
    fn stacked_sections_do_not_draw_the_seam_between_them() {
        // Two solid sections stacked. If the mesher looked at sections in
        // isolation it would draw the touching faces and this would be twice
        // the single-section shell.
        let mesh = build(&chunk(2, |_| true), &Solid, &OneTile::default());
        // Top and bottom are still 256 each; the four sides are now 16x32.
        let expected = (2 * 256 + 4 * 16 * 32) * 2;
        assert_eq!(mesh.triangles(), expected);
    }

    #[test]
    fn faces_carry_their_directional_shade() {
        let mesh = build(&chunk(1, |_| true), &Solid, &OneTile::default());
        let lights: Vec<f32> = {
            let mut l: Vec<f32> = mesh.vertices.iter().map(|v| v.light).collect();
            l.sort_by(|a, b| a.partial_cmp(b).unwrap());
            l.dedup();
            l
        };
        assert_eq!(lights, vec![0.5, 0.6, 0.8, 1.0]);
    }

    #[test]
    fn geometry_sits_inside_the_column_bounds() {
        let mesh = build(&chunk(2, |i| i == 1), &Solid, &OneTile::default());
        for v in &mesh.vertices {
            assert!((0.0..=16.0).contains(&v.position[0]), "x out of range: {v:?}");
            assert!((0.0..=32.0).contains(&v.position[1]), "y out of range: {v:?}");
            assert!((0.0..=16.0).contains(&v.position[2]), "z out of range: {v:?}");
        }
        // The filled section is the upper one, so nothing should sit below y=16.
        assert!(mesh.vertices.iter().all(|v| v.position[1] >= 16.0));
    }

    #[test]
    fn min_y_offsets_the_whole_column() {
        let mut c = chunk(1, |_| true);
        c.min_y = -64;
        let mesh = build(&c, &Solid, &OneTile::default());
        assert!(mesh.vertices.iter().all(|v| (-64.0..=-48.0).contains(&v.position[1])));
    }

    /// Water: see-through, but hides its own internal faces.
    struct Fluid;
    impl BlockAppearance for Fluid {
        fn is_opaque(&self, _state: StateId) -> bool {
            false
        }
        fn self_culls(&self, state: StateId) -> bool {
            !state.is_air()
        }
    }

    #[test]
    fn a_body_of_fluid_draws_only_its_surface() {
        // Two solid sections of a single non-opaque, self-culling block. Without
        // same-block culling this would draw every internal face and produce
        // the whole volume instead of the shell.
        let mesh = build(&chunk(2, |_| true), &Fluid, &OneTile::default());
        let expected = (2 * 256 + 4 * 16 * 32) * 2;
        assert_eq!(mesh.triangles(), expected);
    }

    #[test]
    fn every_index_points_at_a_real_vertex() {
        let mesh = build(&chunk(2, |i| i == 0), &Solid, &OneTile::default());
        assert!(!mesh.indices.is_empty());
        let max = mesh.vertices.len() as u32;
        assert!(mesh.indices.iter().all(|&i| i < max));
        assert_eq!(mesh.indices.len() % 3, 0);
    }
}
