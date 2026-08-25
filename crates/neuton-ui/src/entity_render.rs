//! Every entity in the world, built into one mesh from the game's own models.
//!
//! The models are the client's, read out of the jar: a tree of named parts,
//! each holding boxes measured in the sixteen-to-a-block units a texture is
//! drawn in. What the jar cannot give us is movement, because how a part swings
//! is a method rather than data, so the poses here are ours: a head that
//! follows where the entity is looking, and legs and arms that swing with how
//! fast it is going.

use crate::entities::{Entities, Entity};
use neuton_render::generated::entity_models::{Cube, Model, Part, look, model};
use neuton_render::renderer::{MAX_ENTITY_VERTICES, MAX_ENTITY_INDICES};
use neuton_render::{EntityBatch, Vertex};
use std::collections::BTreeMap;

/// Model units to blocks.
const UNIT: f32 = 1.0 / 16.0;

/// How far the model's own origin sits above the feet.
///
/// Model space is measured downwards from about the neck and drawn upside
/// down, which is why this is a lift rather than a drop. The extra thousandth
/// is the game's own, and keeps a model's sole out of the surface it stands on.
const LIFT: f32 = 1.501;

/// Something standing in the world at a fixed place, drawn from a model: a
/// chest, which has no block shape of its own.
#[derive(Clone, Copy)]
pub struct Placed {
    /// The block's own corner.
    pub at: [f32; 3],
    /// How far round it faces, in degrees, as the game measures a facing.
    pub yaw: f32,
    pub model: &'static Model,
    pub texture: &'static str,
}

/// One frame's worth of entity geometry.
#[derive(Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub batches: Vec<EntityBatch>,
}

impl Mesh {
    fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.batches.clear();
    }
}

/// What an entity is drawn as.
///
/// Almost all of this is generated, but a player is not: their texture is a
/// skin rather than a file in the jar, and until one has been fetched they wear
/// the one the game itself falls back to.
pub fn appearance(kind: &str) -> Option<(&'static Model, &'static str)> {
    match kind {
        "minecraft:player" => {
            Some((model("minecraft:player#main")?, "entity/player/wide/steve.png"))
        }
        _ => look(kind),
    }
}

/// An affine transform: a rotation and scale, then a move.
#[derive(Clone, Copy)]
struct Xform {
    basis: [[f32; 3]; 3],
    at: [f32; 3],
}

impl Xform {
    fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.basis;
        [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2] + self.at[0],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2] + self.at[1],
            m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2] + self.at[2],
        ]
    }

    /// `self` applied to whatever `inner` produces.
    fn then(&self, inner: &Xform) -> Xform {
        let mut basis = [[0.0f32; 3]; 3];
        for (row, out) in basis.iter_mut().enumerate() {
            for (column, cell) in out.iter_mut().enumerate() {
                *cell = (0..3).map(|k| self.basis[row][k] * inner.basis[k][column]).sum();
            }
        }
        Xform { basis, at: self.apply(inner.at) }
    }
}

fn multiply(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (row, cells) in out.iter_mut().enumerate() {
        for (column, cell) in cells.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[row][k] * b[k][column]).sum();
        }
    }
    out
}

fn rotation_x(angle: f32) -> [[f32; 3]; 3] {
    let (s, c) = angle.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn rotation_y(angle: f32) -> [[f32; 3]; 3] {
    let (s, c) = angle.sin_cos();
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}

fn rotation_z(angle: f32) -> [[f32; 3]; 3] {
    let (s, c) = angle.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// How an entity is holding itself this frame, in radians.
///
/// Only the parts every model names the same way. Anything with a tail, a wing
/// or a jaw keeps the pose the model was built in until it is worth writing
/// down how that particular animal moves.
struct Pose {
    /// How far the head is turned from the body, and how far up or down.
    head_turn: f32,
    head_tilt: f32,
    /// Where in the walking cycle it is, and how much of the swing to apply.
    stride: f32,
    stride_amount: f32,
}

impl Pose {
    /// Extra rotation for a part, on top of the one the model was built with.
    fn extra(&self, name: &str) -> [f32; 3] {
        // A head carries its hat and its face with it, because they are its
        // children in the model.
        if name == "head" {
            return [self.head_tilt, self.head_turn, 0.0];
        }
        let swing = |phase: f32, amount: f32| {
            [(self.stride + phase).cos() * amount * self.stride_amount, 0.0, 0.0]
        };
        let left = name.starts_with("left");
        // An arm swings against the leg on its own side, and a front leg
        // against the hind leg on its own side, which is what stops an animal
        // from hopping.
        let front = name.contains("front");
        match name {
            _ if name.ends_with("arm") => {
                swing(if left { 0.0 } else { std::f32::consts::PI }, 1.0)
            }
            _ if name.ends_with("leg") => {
                swing(if left != front { std::f32::consts::PI } else { 0.0 }, 1.4)
            }
            _ => [0.0; 3],
        }
    }
}

/// Builds every entity that has a model into one mesh, in runs that share a
/// texture.
pub fn build(entities: &Entities, placed: &[Placed], alpha: f32, out: &mut Mesh) {
    out.clear();
    // Gathered per texture first, because a draw cannot change texture part
    // way through and two zombies should not cost two draws.
    let mut runs: BTreeMap<&'static str, Vec<u32>> = BTreeMap::new();
    for entity in entities.iter() {
        let Some((model, texture)) = appearance(entity.kind.name) else { continue };
        // Stopping on a whole entity rather than part way through one: half a
        // cow is worse than no cow.
        if out.vertices.len() + 4096 > MAX_ENTITY_VERTICES {
            break;
        }
        let indices = runs.entry(texture).or_default();
        push_entity(entity, model, alpha, &mut out.vertices, indices);
    }
    for block in placed {
        if out.vertices.len() + 4096 > MAX_ENTITY_VERTICES {
            break;
        }
        let indices = runs.entry(block.texture).or_default();
        push_placed(block, &mut out.vertices, indices);
    }
    for (texture, run) in runs {
        if run.is_empty() {
            continue;
        }
        out.batches.push(EntityBatch {
            texture: texture.to_string(),
            start: out.indices.len() as u32,
            count: run.len() as u32,
        });
        out.indices.extend_from_slice(&run);
        if out.indices.len() > MAX_ENTITY_INDICES {
            out.indices.truncate(MAX_ENTITY_INDICES);
            out.batches.pop();
            break;
        }
    }
}

fn push_entity(
    entity: &Entity,
    model: &Model,
    alpha: f32,
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let at = entity.drawn_at(alpha);
    // The model is built facing the other way to the axis yaw is measured
    // from, so the body is turned by the difference.
    let body = (180.0 - entity.yaw).to_radians();
    // Upside down and back to front: model space runs down and to the left of
    // where the world does, which the game undoes with a flip rather than by
    // building every model the other way round.
    let flip = [[-UNIT, 0.0, 0.0], [0.0, -UNIT, 0.0], [0.0, 0.0, UNIT]];
    let turn = rotation_y(body);
    let root = Xform {
        basis: multiply(turn, flip),
        at: {
            let lifted = [
                turn[0][1] * LIFT + at[0] as f32,
                turn[1][1] * LIFT + at[1] as f32,
                turn[2][1] * LIFT + at[2] as f32,
            ];
            lifted
        },
    };

    let pose = Pose {
        head_turn: (entity.head_yaw - entity.yaw).to_radians(),
        head_tilt: entity.pitch.to_radians(),
        stride: entity.stride,
        stride_amount: entity.stride_amount,
    };
    push_part(&model.root, &root, &pose, model.texture_size, vertices, indices);
}

/// A block entity, turned about the middle of its own block.
///
/// Nothing is flipped here, unlike an entity: these models are built the way up
/// they are drawn, which is why a chest's lid sits at the top of the numbers
/// rather than the bottom.
fn push_placed(block: &Placed, vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>) {
    let turn = rotation_y((-block.yaw).to_radians());
    let centre = [0.5, 0.0, 0.5];
    let basis = [
        [turn[0][0] * UNIT, turn[0][1] * UNIT, turn[0][2] * UNIT],
        [turn[1][0] * UNIT, turn[1][1] * UNIT, turn[1][2] * UNIT],
        [turn[2][0] * UNIT, turn[2][1] * UNIT, turn[2][2] * UNIT],
    ];
    // Rotating about the middle of the block rather than its corner, which is
    // what keeps a chest inside its own block when it faces east.
    let mut at = [0.0f32; 3];
    for (axis, cell) in at.iter_mut().enumerate() {
        let turned: f32 = (0..3).map(|k| turn[axis][k] * -centre[k]).sum();
        *cell = block.at[axis] + centre[axis] + turned;
    }
    let root = Xform { basis, at };
    let still = Pose { head_turn: 0.0, head_tilt: 0.0, stride: 0.0, stride_amount: 0.0 };
    push_part(&block.model.root, &root, &still, block.model.texture_size, vertices, indices);
}

fn push_part(
    part: &Part,
    parent: &Xform,
    pose: &Pose,
    texture: [f32; 2],
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let [x, y, z, x_rot, y_rot, z_rot, x_scale, y_scale, z_scale] = part.pose;
    let extra = pose.extra(part.name);
    // The order the game turns a part in, and the order matters: a head that
    // is tilted then turned looks in a different direction to one turned then
    // tilted.
    let rotated = multiply(
        multiply(rotation_z(z_rot + extra[2]), rotation_y(y_rot + extra[1])),
        rotation_x(x_rot + extra[0]),
    );
    let scaled = multiply(rotated, [
        [x_scale, 0.0, 0.0],
        [0.0, y_scale, 0.0],
        [0.0, 0.0, z_scale],
    ]);
    let here = parent.then(&Xform { basis: scaled, at: [x, y, z] });

    for cube in part.cubes {
        push_cube(cube, &here, texture, vertices, indices);
    }
    for child in part.children {
        push_part(child, &here, pose, texture, vertices, indices);
    }
}

/// The six faces of one box, unfolded onto the texture the way the game folds
/// them: the two caps side by side along the top, and the four sides in a row
/// beneath, which is why a head's face sits eight pixels across and eight down.
fn push_cube(
    cube: &Cube,
    at: &Xform,
    texture: [f32; 2],
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let (w, h, d) = (cube.size[0], cube.size[1], cube.size[2]);
    let low = [
        cube.at[0] - cube.grow[0],
        cube.at[1] - cube.grow[1],
        cube.at[2] - cube.grow[2],
    ];
    let high = [
        cube.at[0] + w + cube.grow[0],
        cube.at[1] + h + cube.grow[1],
        cube.at[2] + d + cube.grow[2],
    ];
    let (u, v) = (cube.uv[0], cube.uv[1]);
    // A texture can be drawn at a different number of pixels per unit than the
    // model is measured in, which is what the scale is for.
    let (su, sv) = (cube.uv_scale[0].max(1.0), cube.uv_scale[1].max(1.0));

    // A patch of the texture, by pixel, as four corners wound to match a face.
    let patch = |x: f32, y: f32, pw: f32, ph: f32| {
        let (x0, x1) = if cube.mirror {
            // One side of a texture serving both arms: the same patch, read
            // the other way round.
            ((x + pw) / texture[0], x / texture[0])
        } else {
            (x / texture[0], (x + pw) / texture[0])
        };
        let (y0, y1) = (y / texture[1], (y + ph) / texture[1]);
        [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    };
    let (uw, uh, ud) = (w * su, h * sv, d * su);

    let faces = [
        // The two caps.
        (
            [
                [low[0], high[1], low[2]],
                [high[0], high[1], low[2]],
                [high[0], high[1], high[2]],
                [low[0], high[1], high[2]],
            ],
            patch(u + ud, v, uw, ud),
        ),
        (
            [
                [low[0], low[1], high[2]],
                [high[0], low[1], high[2]],
                [high[0], low[1], low[2]],
                [low[0], low[1], low[2]],
            ],
            patch(u + ud + uw, v, uw, ud),
        ),
        // The model's right, its front, its left and its back.
        (
            [
                [low[0], high[1], high[2]],
                [low[0], high[1], low[2]],
                [low[0], low[1], low[2]],
                [low[0], low[1], high[2]],
            ],
            patch(u, v + ud, ud, uh),
        ),
        (
            [
                [low[0], high[1], low[2]],
                [high[0], high[1], low[2]],
                [high[0], low[1], low[2]],
                [low[0], low[1], low[2]],
            ],
            patch(u + ud, v + ud, uw, uh),
        ),
        (
            [
                [high[0], high[1], low[2]],
                [high[0], high[1], high[2]],
                [high[0], low[1], high[2]],
                [high[0], low[1], low[2]],
            ],
            patch(u + ud + uw, v + ud, ud, uh),
        ),
        (
            [
                [high[0], high[1], high[2]],
                [low[0], high[1], high[2]],
                [low[0], low[1], high[2]],
                [high[0], low[1], high[2]],
            ],
            patch(u + ud + uw + ud, v + ud, uw, uh),
        ),
    ];

    for (corners, uvs) in faces {
        let base = vertices.len() as u32;
        for (corner, uv) in corners.iter().zip(uvs) {
            vertices.push(Vertex {
                position: at.apply(*corner),
                uv,
                tint: [1.0, 1.0, 1.0, 1.0],
                // Not lit by the world yet: an entity in a cave is as bright
                // as one in the open until light is sampled where it stands.
                light: 1.0,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(kind: i32) -> Entities {
        let mut all = Entities::default();
        all.add(1, 0, kind, [0.0, 64.0, 0.0], 0.0, 0.0, 0.0, [0.0; 3]);
        all
    }

    /// 156 is the player in registry order.
    #[test]
    fn a_player_stands_on_their_feet() {
        let mut mesh = Mesh::default();
        build(&one(156), &[], 1.0, &mut mesh);
        assert!(!mesh.vertices.is_empty(), "no geometry for a player");
        let lowest = mesh.vertices.iter().map(|v| v.position[1]).fold(f32::MAX, f32::min);
        let highest = mesh.vertices.iter().map(|v| v.position[1]).fold(f32::MIN, f32::max);
        assert!((lowest - 64.0).abs() < 0.03, "feet at {lowest}");
        // The model is two blocks tall, a little more than the hitbox.
        assert!((highest - 66.0).abs() < 0.1, "head at {highest}");
    }

    #[test]
    fn every_texture_coordinate_lands_on_the_texture() {
        let mut mesh = Mesh::default();
        build(&one(156), &[], 1.0, &mut mesh);
        for vertex in &mesh.vertices {
            assert!(
                (0.0..=1.0).contains(&vertex.uv[0]) && (0.0..=1.0).contains(&vertex.uv[1]),
                "uv off the texture: {:?}",
                vertex.uv
            );
        }
    }

    #[test]
    fn one_texture_is_one_run() {
        let mut all = one(156);
        all.add(2, 0, 156, [4.0, 64.0, 0.0], 0.0, 0.0, 0.0, [0.0; 3]);
        let mut mesh = Mesh::default();
        build(&all, &[], 1.0, &mut mesh);
        assert_eq!(mesh.batches.len(), 1, "two players should share a draw");
        assert_eq!(mesh.batches[0].count as usize, mesh.indices.len());
    }

    #[test]
    fn something_with_no_model_is_left_out() {
        // 71 is a dropped item, which is drawn some other way.
        let mut mesh = Mesh::default();
        build(&one(71), &[], 1.0, &mut mesh);
        assert!(mesh.vertices.is_empty() && mesh.batches.is_empty());
    }
}
