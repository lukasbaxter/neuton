//! The player, drawn into the window the inventory screen keeps for them.
//!
//! The game renders a real entity there, in three dimensions, following the
//! pointer with its eyes. So does this -- the same model out of the jar and the
//! same skin the world uses -- but rasterised on the processor into a small
//! picture rather than drawn through the world pipeline. The portrait is fifty
//! pixels wide and holds about seventy quads; the pipeline would need a second
//! camera, a second pass and a target to render into, all so that a hundredth
//! of the triangles in a frame could take a different projection.
//!
//! It is redrawn only when it would come out different, which while the mouse
//! is still is never.

use crate::entity_render::{Pose, Xform, multiply, push_part, rotation_x, rotation_y};
use neuton_render::Vertex;
use neuton_render::generated::entity_models::model;

/// Model units to blocks.
const UNIT: f32 = 1.0 / 16.0;

/// How far the model's own origin sits above the feet. The same lift the world
/// uses: model space is measured downwards from about the neck.
const LIFT: f32 = 1.501;

/// The game's own numbers for this window: where it sits on the panel, how big
/// it is, and how many pixels tall a block is drawn.
pub const AT: [f32; 2] = [26.0, 8.0];
pub const SIZE: [f32; 2] = [49.0, 70.0];
const PER_BLOCK: f32 = 30.0;

/// Where the middle of the player is put, which is the middle of the window:
/// half their height, plus the sixteenth the game nudges them down by.
const CENTRE_HEIGHT: f32 = 1.8 / 2.0 + 0.0625;

/// How far the pointer has to be from the window before the player is looking
/// as far round as they will go. The game's own number.
const REACH: f32 = 40.0;

/// The picture, and what it was drawn from.
#[derive(Default)]
pub struct Portrait {
    texture: Option<egui::TextureHandle>,
    /// Rounded, so a pointer that has not really moved does not redraw.
    drawn: Option<(i32, i32, u32, u32)>,
    skin: Option<Skin>,
    /// Set once the skin has been looked for and not found, so it is not
    /// looked for again on every frame for the rest of the session.
    tried_skin: bool,
}

pub struct Skin {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Which way the player is turned, worked out from where the pointer is.
///
/// The body turns half as far as the head, which is what makes them look at
/// the pointer rather than swivel towards it.
#[derive(Clone, Copy)]
pub struct Look {
    /// Degrees the body is turned from facing you.
    pub body: f32,
    pub head: f32,
    /// Degrees the head is tilted, positive downwards.
    pub tilt: f32,
}

impl Look {
    /// From the pointer's offset from the middle of the window, in the game's
    /// own interface pixels.
    pub fn at(dx: f32, dy: f32) -> Self {
        let across = (dx / REACH).atan();
        let down = (dy / REACH).atan();
        Self { body: across * 20.0, head: across * 40.0, tilt: -down * 20.0 }
    }
}

impl Default for Look {
    fn default() -> Self {
        Self::at(0.0, 0.0)
    }
}

impl Portrait {
    /// The picture to draw, rendering it first if this look is a new one.
    ///
    /// `size` is in real pixels, so the portrait is drawn at the resolution it
    /// will be shown at rather than scaled up from a guess.
    pub fn texture(
        &mut self,
        ctx: &egui::Context,
        packs: &mut Option<neuton_assets::PackStack>,
        look: Look,
        size: [u32; 2],
    ) -> Option<egui::TextureId> {
        let (width, height) = (size[0].clamp(16, 512), size[1].clamp(16, 512));
        let key = (
            (look.head * 4.0).round() as i32,
            (look.tilt * 4.0).round() as i32,
            width,
            height,
        );
        if self.drawn == Some(key) {
            return self.texture.as_ref().map(|t| t.id());
        }
        self.load_skin(packs);
        let skin = self.skin.as_ref()?;
        let image = render(skin, look, width, height);
        // Replacing the pixels of the texture already on the GPU rather than
        // making a new one: this happens on most frames the pointer moves.
        match self.texture.as_mut() {
            Some(handle) => handle.set(image, nearest()),
            None => self.texture = Some(ctx.load_texture("portrait", image, nearest())),
        }
        self.drawn = Some(key);
        self.texture.as_ref().map(|t| t.id())
    }

    fn load_skin(&mut self, packs: &mut Option<neuton_assets::PackStack>) {
        if self.tried_skin {
            return;
        }
        self.tried_skin = true;
        if packs.is_none() {
            *packs = neuton_assets::PackStack::discover("26.2");
        }
        let path = format!("assets/minecraft/textures/{}", crate::hand::SKIN);
        let Some(image) = packs
            .as_mut()
            .and_then(|packs| packs.read(&path))
            .and_then(|bytes| crate::icons::decode(&bytes))
        else {
            eprintln!("neuton: no skin at {path}");
            return;
        };
        self.skin = Some(Skin {
            rgba: image.pixels.iter().flat_map(|p| p.to_array()).collect(),
            width: image.size[0] as u32,
            height: image.size[1] as u32,
        });
    }
}

fn nearest() -> egui::TextureOptions {
    egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        ..Default::default()
    }
}

/// Builds the player and rasterises them.
pub fn render(skin: &Skin, look: Look, width: u32, height: u32) -> egui::ColorImage {
    let mut pixels = vec![egui::Color32::TRANSPARENT; (width * height) as usize];
    let mut depth = vec![f32::NEG_INFINITY; (width * height) as usize];

    let Some(player) = model("minecraft:player#main") else {
        return egui::ColorImage { size: [width as usize, height as usize], pixels, source_size: egui::vec2(width as f32, height as f32) };
    };

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    // Facing you, turned by however far the body has come round. Upside down
    // and back to front first, the way every mob model is built.
    //
    // The turn is subtracted, not added: the model is mirrored on x by the
    // flip, so a rotation applied outside it reads the other way round. Added,
    // the body turns away from the pointer instead of towards it.
    let flip = [[-UNIT, 0.0, 0.0], [0.0, -UNIT, 0.0], [0.0, 0.0, UNIT]];
    let turn = rotation_y(std::f32::consts::PI - look.body.to_radians());
    // The whole scene leans with the pointer as well as the head, which is the
    // game tilting its camera rather than the player bending.
    let lean = rotation_x((-look.tilt * 0.5).to_radians());
    let basis = multiply(lean, multiply(turn, flip));
    let lifted = multiply(lean, turn);
    let root = Xform {
        basis,
        at: [lifted[0][1] * LIFT, lifted[1][1] * LIFT, lifted[2][1] * LIFT],
    };
    let pose = Pose {
        // Turned over to stand up, so its sheet is turned over with it. See
        // `push_cube`.
        sheet_flipped: true,
        // Inside the flip, so this turn reads the other way round again --
        // which is why it is the plain difference here and the body's is not.
        // Get the sign wrong and the head cancels the body exactly: it stays
        // pointed straight at you however far the body comes round, which
        // looks like a head that cannot turn at all.
        head_turn: (look.head - look.body).to_radians(),
        head_tilt: look.tilt.to_radians(),
        stride: 0.0,
        stride_amount: 0.0,
    };
    push_part(&player.root, &root, &pose, player.texture_size, &mut vertices, &mut indices);

    // A block is thirty of the game's interface pixels tall, and the window is
    // forty nine of them wide, so this is however many real pixels that came
    // out to.
    let per_block = PER_BLOCK * width as f32 / SIZE[0];
    let centre = [width as f32 / 2.0, height as f32 / 2.0];
    let project = |p: [f32; 3]| -> [f32; 3] {
        [
            centre[0] + p[0] * per_block,
            centre[1] - (p[1] - CENTRE_HEIGHT) * per_block,
            p[2],
        ]
    };

    for triangle in indices.chunks_exact(3) {
        let Some(a) = vertices.get(triangle[0] as usize) else { continue };
        let Some(b) = vertices.get(triangle[1] as usize) else { continue };
        let Some(c) = vertices.get(triangle[2] as usize) else { continue };
        fill(
            &mut pixels,
            &mut depth,
            width,
            height,
            [(project(a.position), a), (project(b.position), b), (project(c.position), c)],
            skin,
        );
    }
    egui::ColorImage {
        size: [width as usize, height as usize],
        pixels,
        source_size: egui::vec2(width as f32, height as f32),
    }
}

/// Fills one triangle, reading the skin at each pixel.
///
/// The projection drops an axis rather than dividing by one, so depth and
/// texture coordinates are linear across the triangle and plain barycentric
/// weights are exact. No perspective correction, because there is no
/// perspective.
fn fill(
    out: &mut [egui::Color32],
    depth: &mut [f32],
    width: u32,
    height: u32,
    corners: [([f32; 3], &Vertex); 3],
    skin: &Skin,
) {
    let p = [corners[0].0, corners[1].0, corners[2].0];
    let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
    if area.abs() < 1e-6 {
        return;
    }
    let min_x = p.iter().map(|v| v[0]).fold(f32::INFINITY, f32::min).floor().max(0.0) as u32;
    let max_x = p
        .iter()
        .map(|v| v[0])
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .clamp(0.0, width as f32) as u32;
    let min_y = p.iter().map(|v| v[1]).fold(f32::INFINITY, f32::min).floor().max(0.0) as u32;
    let max_y = p
        .iter()
        .map(|v| v[1])
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .clamp(0.0, height as f32) as u32;

    for y in min_y..max_y {
        for x in min_x..max_x {
            let at = [x as f32 + 0.5, y as f32 + 0.5];
            let w1 = ((at[0] - p[0][0]) * (p[2][1] - p[0][1])
                - (at[1] - p[0][1]) * (p[2][0] - p[0][0]))
                / area;
            let w2 = ((p[1][0] - p[0][0]) * (at[1] - p[0][1])
                - (p[1][1] - p[0][1]) * (at[0] - p[0][0]))
                / area;
            let w0 = 1.0 - w1 - w2;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let here = p[0][2] * w0 + p[1][2] * w1 + p[2][2] * w2;
            let slot = (y * width + x) as usize;
            if depth[slot] > here {
                continue;
            }
            let u = corners[0].1.uv[0] * w0 + corners[1].1.uv[0] * w1 + corners[2].1.uv[0] * w2;
            let v = corners[0].1.uv[1] * w0 + corners[1].1.uv[1] * w1 + corners[2].1.uv[1] * w2;
            let sx = ((u * skin.width as f32) as i32).clamp(0, skin.width as i32 - 1) as u32;
            let sy = ((v * skin.height as f32) as i32).clamp(0, skin.height as i32 - 1) as u32;
            let i = ((sy * skin.width + sx) * 4) as usize;
            let Some(texel) = skin.rgba.get(i..i + 4) else { continue };
            // Cut out, not blended: the outer layer of a skin is all or
            // nothing, and blending its edges leaves a halo round every hat.
            if texel[3] < 128 {
                continue;
            }
            let light = corners[0].1.light * w0 + corners[1].1.light * w1 + corners[2].1.light * w2;
            depth[slot] = here;
            out[slot] = egui::Color32::from_rgb(
                (texel[0] as f32 * light) as u8,
                (texel[1] as f32 * light) as u8,
                (texel[2] as f32 * light) as u8,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_on_the_window_leaves_the_player_facing_you() {
        let look = Look::at(0.0, 0.0);
        assert_eq!(look.body, 0.0);
        assert_eq!(look.head, 0.0);
        assert_eq!(look.tilt, 0.0);
    }

    #[test]
    fn the_head_turns_twice_as_far_as_the_body() {
        let look = Look::at(30.0, 0.0);
        assert!(look.body > 0.0);
        assert!((look.head - look.body * 2.0).abs() < 1e-4);
    }

    #[test]
    fn looking_further_never_runs_away() {
        // An arctangent, so a pointer at the other side of the screen turns
        // the head no further than one at the edge of the panel.
        let near = Look::at(200.0, 0.0);
        let far = Look::at(20_000.0, 0.0);
        assert!(far.head > near.head);
        assert!(far.head < 90.0, "the head cannot come off");
    }

    #[test]
    fn a_pointer_below_tilts_the_head_down() {
        // Screen coordinates run downwards, and the offset is the window minus
        // the pointer, so a pointer below the window is a negative offset.
        let below = Look::at(0.0, -40.0);
        assert!(below.tilt > 0.0, "tilt should be positive downwards, got {}", below.tilt);
    }
}
