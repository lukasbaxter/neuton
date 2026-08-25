//! What the player's own hand is holding, drawn over the world.
//!
//! The numbers here are the game's, because a hand that sits a few degrees or a
//! few hundredths of a block out of place is the first thing anyone notices: it
//! is on screen the whole time. Everything is worked out in the space the
//! camera looks down -- x to the right, y up, z towards the player -- and turned
//! into world space at the end, so the geometry can go through the same
//! pipeline as everything else.

use crate::entity_render::{Mesh, Xform, multiply, push_part, rotation_x, rotation_y, rotation_z};
use neuton_render::generated::entity_models::model;
use neuton_render::textures::{BakedModel, BlockTextures};
use neuton_render::{ATLAS_BATCH, EntityBatch, Face, Vertex};
use std::f32::consts::PI;

/// The skin a hand is drawn with, until real skins arrive.
pub const SKIN: &str = "entity/player/wide/steve.png";

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// What the hand is holding this frame.
pub enum Holding<'a> {
    /// Nothing, so the arm itself is what is drawn.
    Nothing,
    /// A block, drawn from its own model in the block atlas.
    Block { model: &'a BakedModel, display: neuton_assets::Display },
    /// A flat picture, built into a solid a sixteenth thick: the two faces and
    /// the rim of little quads round the edge of the silhouette. Without the
    /// rim a held item vanishes at the quarter turn where the faces line up.
    Sprite {
        texture: &'a str,
        sides: &'a [neuton_assets::Side],
        display: neuton_assets::Display,
    },
}

/// Where the hand is in its swing and its equip, both from zero to one.
pub struct Motion {
    pub swing: f32,
    /// One when the item is fully raised, zero when it is out of sight below.
    pub equipped: f32,
}

/// Builds the hand into `out`, in world space.
pub fn build(
    eye: [f32; 3],
    yaw_degrees: f32,
    pitch_degrees: f32,
    holding: &Holding<'_>,
    motion: &Motion,
    out: &mut Mesh,
) {
    out.clear();
    // View space to world: x to the right, y up, z back towards the player,
    // which is the space every number below is written in. Built from the axes
    // themselves rather than by composing turns, because the sign of the one
    // that points backwards is exactly the mistake that puts a hand behind the
    // camera where nobody can see it.
    let (yaw, pitch) = (yaw_degrees.to_radians(), pitch_degrees.to_radians());
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    let (sin_pitch, cos_pitch) = pitch.sin_cos();
    // This is the space the game's own numbers are written in: turning the
    // world by the yaw plus half a turn and then the pitch lands here, which is
    // x to the right, y up, z back towards the player.
    let camera = Xform {
        basis: [
            [-cos_yaw, -sin_yaw * sin_pitch, sin_yaw * cos_pitch],
            [0.0, cos_pitch, sin_pitch],
            [-sin_yaw, cos_yaw * sin_pitch, -cos_yaw * cos_pitch],
        ],
        at: eye,
    };

    let swing = motion.swing.clamp(0.0, 1.0);
    // The game measures how far the hand has DROPPED, not how far it is up, and
    // the sign of that is the difference between a hand at the corner of the
    // eye and one below the bottom of the screen.
    let dropped = 1.0 - motion.equipped.clamp(0.0, 1.0);
    // The arm is there whatever is in it. An item drawn on its own reads as
    // floating in front of the player rather than being carried.
    push_arm(&arm_transform(&camera, swing, dropped), out);
    match holding {
        Holding::Nothing => {}
        Holding::Block { model, display } => {
            let held = item_transform(&camera, swing, dropped, display);
            push_block(model, &held, out);
        }
        Holding::Sprite { texture, sides, display } => {
            let held = item_transform(&camera, swing, dropped, display);
            push_sprite(texture, sides, &held, out);
        }
    }
}

/// Where a held item sits: out to the right and low, swung when the button is
/// pressed, and then however that particular model is held.
fn item_transform(
    camera: &Xform,
    swing: f32,
    dropped: f32,
    display: &neuton_assets::Display,
) -> Xform {
    let mut at = Xform { basis: IDENTITY, at: [0.56, -0.52 + dropped * -0.6, -0.72] };
    at = camera.then(&at);

    // The swing: a turn out and back, and a dip, on two different curves so the
    // hand does not simply rock about one axis.
    let square = (swing * swing * PI).sin();
    let root = (swing.sqrt() * PI).sin();
    let swung = multiply(
        multiply(
            rotation_y((45.0 + square * -20.0).to_radians()),
            rotation_z((root * -20.0).to_radians()),
        ),
        multiply(rotation_x((root * -80.0).to_radians()), rotation_y((-45.0f32).to_radians())),
    );
    let at = at.then(&Xform { basis: swung, at: [0.0; 3] });

    // How this kind of model is held.
    let rotation = multiply(
        multiply(
            rotation_x(display.rotation[0].to_radians()),
            rotation_y(display.rotation[1].to_radians()),
        ),
        rotation_z(display.rotation[2].to_radians()),
    );
    // The scale goes on inside the turn, because the model is scaled in its own
    // axes and then turned, not turned and then stretched along the screen's.
    let s = display.scale;
    let scaled = [
        [rotation[0][0] * s[0], rotation[0][1] * s[1], rotation[0][2] * s[2]],
        [rotation[1][0] * s[0], rotation[1][1] * s[1], rotation[1][2] * s[2]],
        [rotation[2][0] * s[0], rotation[2][1] * s[1], rotation[2][2] * s[2]],
    ];
    at.then(&Xform {
        basis: scaled,
        at: [
            display.translation[0] / 16.0,
            display.translation[1] / 16.0,
            display.translation[2] / 16.0,
        ],
    })
}

/// Where the bare arm sits, which is not where a held item sits: it comes in
/// from the corner of the screen rather than being held out in front.
///
/// The large steps in the middle are the game's own and look wrong written
/// down -- three and a half blocks forward, five and a half to the side -- but
/// they are separated by turns that bring the arm back to the corner of the
/// eye. Only the last step is in model units.
fn arm_transform(camera: &Xform, swing: f32, dropped: f32) -> Xform {
    let root = swing.sqrt();
    let out = -0.3 * (root * PI).sin();
    let up = 0.4 * (root * PI * 2.0).sin();
    let forward = -0.4 * (swing * PI).sin();
    let mut at = camera.then(&Xform {
        basis: IDENTITY,
        at: [out + 0.64, up - 0.6 + dropped * -0.6, forward - 0.72],
    });

    let square = (swing * swing * PI).sin();
    let curve = (root * PI).sin();
    at = at.then(&Xform {
        basis: multiply(
            rotation_y(45.0f32.to_radians()),
            multiply(
                rotation_y((curve * 70.0).to_radians()),
                rotation_z((square * -20.0).to_radians()),
            ),
        ),
        at: [0.0; 3],
    });
    at = at.then(&Xform { basis: IDENTITY, at: [-1.0, 3.6, 3.5] });
    at = at.then(&Xform {
        basis: multiply(
            rotation_z(120.0f32.to_radians()),
            multiply(rotation_x(200.0f32.to_radians()), rotation_y((-135.0f32).to_radians())),
        ),
        at: [0.0; 3],
    });
    at = at.then(&Xform { basis: IDENTITY, at: [5.6, 0.0, 0.0] });
    // Into the model's own units, sixteen to the block. No flip: the turns
    // above already have the arm the right way up, which is why they are so odd
    // written down.
    at.then(&Xform {
        basis: [
            [1.0 / 16.0, 0.0, 0.0],
            [0.0, 1.0 / 16.0, 0.0],
            [0.0, 0.0, 1.0 / 16.0],
        ],
        at: [0.0; 3],
    })
}

/// The player's own right arm, sleeve and all.
fn push_arm(at: &Xform, out: &mut Mesh) {
    let Some(player) = model("minecraft:player#main") else { return };
    let Some(arm) = player.root.children.iter().find(|p| p.name == "right_arm") else { return };
    let still = crate::entity_render::Pose {
        // A model measured downwards, drawn without being turned over: the one
        // case where the sheet has to be read the other way up.
        sheet_flipped: true,
        head_turn: 0.0,
        head_tilt: 0.0,
        stride: 0.0,
        stride_amount: 0.0,
    };
    let start = out.indices.len() as u32;
    let mut indices = Vec::new();
    push_part(arm, at, &still, player.texture_size, &mut out.vertices, &mut indices);
    out.batches.push(EntityBatch {
        texture: SKIN.to_string(),
        start,
        count: indices.len() as u32,
    });
    out.indices.extend_from_slice(&indices);
}

/// A block held in the hand, from its own model.
fn push_block(baked: &BakedModel, at: &Xform, out: &mut Mesh) {
    let start = out.indices.len() as u32;
    let mut indices = Vec::new();
    for element in &baked.elements {
        for face in Face::ALL {
            let Some(baked_face) = element.faces[face as usize] else { continue };
            let corners = face.corners(element.from, element.to);
            let base = out.vertices.len() as u32;
            for (corner, uv) in corners.iter().zip(baked_face.uv) {
                // Held about its middle rather than its corner.
                let centred = [corner[0] - 0.5, corner[1] - 0.5, corner[2] - 0.5];
                out.vertices.push(Vertex {
                    position: at.apply(centred),
                    uv,
                    tint: [1.0, 1.0, 1.0, 1.0],
                    light: 1.0,
                });
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    out.batches.push(EntityBatch {
        texture: ATLAS_BATCH.to_string(),
        start,
        count: indices.len() as u32,
    });
    out.indices.extend_from_slice(&indices);
}

/// Anything that is not a block: the picture on both faces, and the rim that
/// gives it a thickness to see when it turns.
fn push_sprite(texture: &str, sides: &[neuton_assets::Side], at: &Xform, out: &mut Mesh) {
    let thick = neuton_assets::extrude::THICKNESS;
    let (front, back) = (0.5 - thick * 0.5, 0.5 + thick * 0.5);
    let start = out.indices.len() as u32;
    let mut indices = Vec::new();
    let mut quad = |corners: [[f32; 3]; 4], uvs: [[f32; 2]; 4], light: f32| {
        let base = out.vertices.len() as u32;
        for (corner, uv) in corners.iter().zip(uvs) {
            out.vertices.push(Vertex {
                position: at.apply([corner[0] - 0.5, corner[1] - 0.5, corner[2] - 0.5]),
                uv,
                tint: [1.0, 1.0, 1.0, 1.0],
                light,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    // Wound so each side is seen from outside, and the picture is the right way
    // round from in front.
    quad(
        [[0.0, 0.0, front], [1.0, 0.0, front], [1.0, 1.0, front], [0.0, 1.0, front]],
        [[1.0, 1.0], [0.0, 1.0], [0.0, 0.0], [1.0, 0.0]],
        1.0,
    );
    quad(
        [[1.0, 0.0, back], [0.0, 0.0, back], [0.0, 1.0, back], [1.0, 1.0, back]],
        [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        1.0,
    );
    // The rim. Its corners are already in the same 0..1 space as the two faces,
    // and its texture coordinates already point at the texel that edge came
    // from, so both go straight through.
    for side in sides {
        quad(side.corners, side.uv, side.shade);
    }
    out.batches.push(EntityBatch {
        texture: texture.to_string(),
        start,
        count: indices.len() as u32,
    });
    out.indices.extend_from_slice(&indices);
}

/// Everything the shader needs to draw a block held in hand, or nothing if this
/// name is not a block.
pub fn block_in_hand<'a>(shapes: &'a BlockTextures, item: &str) -> Option<&'a BakedModel> {
    let block = neuton_blocks::by_name(item)?;
    let baked = shapes.model(block.get().default_state);
    (!baked.is_empty()).then_some(baked)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Looking north from the origin, so view space and world space line up:
    /// x to the right, y up, z back towards the player.
    fn rest() -> Mesh {
        let mut mesh = Mesh::default();
        build(
            [0.0, 0.0, 0.0],
            180.0,
            0.0,
            &Holding::Nothing,
            &Motion { swing: 0.0, equipped: 1.0 },
            &mut mesh,
        );
        mesh
    }

    #[test]
    fn the_arm_is_down_to_the_right_and_in_front() {
        let mesh = rest();
        assert!(!mesh.vertices.is_empty(), "no arm was built");
        let x: Vec<f32> = mesh.vertices.iter().map(|v| v.position[0]).collect();
        let y: Vec<f32> = mesh.vertices.iter().map(|v| v.position[1]).collect();
        let z: Vec<f32> = mesh.vertices.iter().map(|v| v.position[2]).collect();
        let (lo, hi) = |v: &Vec<f32>| -> (f32, f32) {
            (v.iter().copied().fold(f32::MAX, f32::min), v.iter().copied().fold(f32::MIN, f32::max))
        }(&x);
        assert!(lo > 0.0, "the right arm should be to the right, x from {lo} to {hi}");
        let (lo, hi) = (y.iter().copied().fold(f32::MAX, f32::min), y.iter().copied().fold(f32::MIN, f32::max));
        assert!(hi < 0.0, "the arm should be below the eye, y from {lo} to {hi}");
        let (lo, hi) = (z.iter().copied().fold(f32::MAX, f32::min), z.iter().copied().fold(f32::MIN, f32::max));
        assert!(hi < 0.0, "the arm should be in front, z from {lo} to {hi}");
    }

    /// The end of the arm the hand is on has to be the end nearer the middle of
    /// the screen, or the player is looking at their own shoulder.
    #[test]
    fn the_hand_end_is_the_one_you_can_see() {
        let mesh = rest();
        // The wrist cap is the one at the far end of the arm's own y, which is
        // the second of the two caps: four vertices, one face.
        let highest = mesh.vertices.iter().map(|v| v.position[1]).fold(f32::MIN, f32::max);
        let lowest = mesh.vertices.iter().map(|v| v.position[1]).fold(f32::MAX, f32::min);
        println!("arm spans y {lowest:.3} to {highest:.3}");
        for (i, v) in mesh.vertices.iter().enumerate().skip(8).take(8) {
            println!("  vertex {i}: {:.3?} uv {:.3?}", v.position, v.uv);
        }
        assert!(highest > lowest);
    }
}
