//! Drawing an item the way it appears in a slot.
//!
//! Two shapes of thing end up in an inventory, and which one an item is comes
//! out of its own definition rather than out of whether it places a block: a
//! torch places a block and is drawn flat, a stone places a block and is drawn
//! as a cube. Most items are a flat 16x16 picture and just need scaling. The
//! rest have a model, and a model in a slot is really rendered -- turned thirty
//! degrees down and two hundred and twenty five round and shrunk to five
//! eighths, which are the game's own numbers out of the model's `display`
//! block, not an isometric projection that happens to look similar.
//!
//! Rendering it properly is what makes the odd ones right. Stairs are two
//! boxes. A slab is half a block high. An anvil is four boxes of different
//! sizes and a rail on a slope is a box turned about its own edge, and all of
//! that falls out of drawing the model the game would draw.

use crate::atlas::{Image, decode};
use crate::models::{BlockModel, Display, Element, ItemGeometry, ModelResolver};
use crate::pack::PackStack;
use std::collections::HashMap;

/// An item picture, ready to hand to the UI.
pub struct Icon {
    pub size: u32,
    /// RGBA, premultiplied by nothing: straight alpha.
    pub pixels: Vec<u8>,
}

/// How big icons are drawn. Twice the resolution the game uses, so the
/// diagonals have somewhere to land instead of stair-stepping.
pub const ICON_SIZE: u32 = 32;

/// Vanilla's grass and foliage colours outside any biome, which is what an
/// inventory slot is.
const GRASS: [f32; 3] = [0.569, 0.741, 0.349];
const FOLIAGE: [f32; 3] = [0.467, 0.671, 0.184];

/// Renders item icons, keeping the textures it has already decoded.
pub struct Icons {
    resolver: ModelResolver,
    textures: HashMap<String, Option<Image>>,
}

impl Default for Icons {
    fn default() -> Self {
        Self::new()
    }
}

impl Icons {
    pub fn new() -> Self {
        Self { resolver: ModelResolver::new(), textures: HashMap::new() }
    }

    /// The resolver, for callers that want a model rather than a picture.
    pub fn models(&mut self) -> &mut ModelResolver {
        &mut self.resolver
    }

    /// Draws one item, by its registry name.
    pub fn render(&mut self, packs: &mut PackStack, name: &str) -> Option<Icon> {
        let resolved = self.resolver.item(packs, name)?;
        match &resolved.geometry {
            ItemGeometry::Sprite(layers) => {
                let layers = layers.clone();
                self.flat(packs, &layers)
            }
            ItemGeometry::Solid(model) => {
                let model = model.clone();
                let display = self.resolver.display(packs, &resolved.path, "gui");
                self.solid(packs, &model, display, name)
            }
        }
    }

    /// A 16x16 sprite, scaled up, with any further layers composited over it.
    ///
    /// Drawn square on, because that is what the model says: `item/generated`
    /// declares no `gui` transform at all, so a flat item is shown flat.
    fn flat(&mut self, packs: &mut PackStack, layers: &[String]) -> Option<Icon> {
        let mut out = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
        let mut drew = false;
        for path in layers {
            let Some(image) = self.texture(packs, path) else { continue };
            let (w, h) = (image.width, image.frame());
            if w == 0 || h == 0 {
                continue;
            }
            for y in 0..ICON_SIZE {
                for x in 0..ICON_SIZE {
                    let sx = x * w / ICON_SIZE;
                    let sy = y * h / ICON_SIZE;
                    let i = ((sy * w + sx) * 4) as usize;
                    let Some(px) = image.rgba.get(i..i + 4) else { continue };
                    over(&mut out, ((y * ICON_SIZE + x) * 4) as usize, px);
                }
            }
            drew = true;
        }
        drew.then_some(Icon { size: ICON_SIZE, pixels: out })
    }

    /// A model, rendered the way the game renders one into a slot.
    fn solid(
        &mut self,
        packs: &mut PackStack,
        model: &BlockModel,
        display: Display,
        item: &str,
    ) -> Option<Icon> {
        let tint = if item.ends_with("leaves") || item.ends_with("vine") { FOLIAGE } else { GRASS };
        let mut out = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
        let mut depth = vec![f32::NEG_INFINITY; (ICON_SIZE * ICON_SIZE) as usize];
        // Depth per pixel rather than sorting: a model is a handful of boxes,
        // and boxes that pass through one another are common enough -- a
        // lantern's chain, a fence's arms -- that sorting them would be wrong
        // as often as it was right.
        for element in &model.elements {
            self.draw_element(packs, element, &display, tint, &mut out, &mut depth);
        }
        out.chunks_exact(4).any(|p| p[3] > 0).then_some(Icon { size: ICON_SIZE, pixels: out })
    }

    fn draw_element(
        &mut self,
        packs: &mut PackStack,
        element: &Element,
        display: &Display,
        tint: [f32; 3],
        out: &mut [u8],
        depth: &mut [f32],
    ) {
        for face in 0..6u8 {
            let Some(def) = element.faces[face as usize].as_ref() else { continue };
            let Some(image) =
                self.texture(packs, &def.texture).map(|i| (i.rgba.clone(), i.width, i.frame()))
            else {
                continue;
            };

            // The box's own corners, turned by whatever the model asks for,
            // and then placed by the display transform. All of it happens in
            // the model's 0..16 space until the last step, where the game
            // works in blocks.
            let mut corners = face_corners(face, element.from, element.to);
            if let Some(rotation) = &element.rotation {
                for corner in corners.iter_mut() {
                    *corner = rotation.apply(*corner);
                }
            }
            // Shade follows the corners as they ended up, not the face it was
            // declared as: a box turned on its side shows its top as a side.
            let shade = shade_for(&corners);
            let placed = corners.map(|c| {
                display.apply([c[0] / 16.0 - 0.5, c[1] / 16.0 - 0.5, c[2] / 16.0 - 0.5])
            });
            // Away from the viewer: the far side of a solid thing, which the
            // near side covers anyway.
            if facing_away(&placed) {
                continue;
            }

            let uv = def.uv;
            let rotation = def.uv_rotation;
            let colour = if def.tinted { tint } else { [1.0; 3] };
            fill(out, depth, placed, |u, v| {
                // The face shows a patch of its texture, in the model's 0..16
                // space. A lantern's side is a 6x7 corner of one, not the whole.
                let (u, v) = turn_uv(u, v, rotation);
                let tu = (uv[0] + (uv[2] - uv[0]) * u) / 16.0;
                let tv = (uv[1] + (uv[3] - uv[1]) * v) / 16.0;
                let (rgba, w, h) = &image;
                let sx = ((tu * *w as f32) as i32).clamp(0, w.saturating_sub(1) as i32) as u32;
                let sy = ((tv * *h as f32) as i32).clamp(0, h.saturating_sub(1) as i32) as u32;
                let i = ((sy * w + sx) * 4) as usize;
                let p = rgba.get(i..i + 4)?;
                Some([
                    (p[0] as f32 * shade * colour[0]) as u8,
                    (p[1] as f32 * shade * colour[1]) as u8,
                    (p[2] as f32 * shade * colour[2]) as u8,
                    p[3],
                ])
            });
        }
    }

    pub fn texture(&mut self, packs: &mut PackStack, path: &str) -> Option<&Image> {
        let path = path.strip_prefix("minecraft:").unwrap_or(path);
        let full = if path.starts_with("assets/") {
            path.to_string()
        } else {
            format!("assets/minecraft/textures/{path}.png")
        };
        self.textures
            .entry(full.clone())
            .or_insert_with(|| packs.read(&full).as_deref().and_then(decode))
            .as_ref()
    }
}

impl Image {
    /// Animated textures are a vertical strip; an icon shows the first frame.
    pub fn frame(&self) -> u32 {
        self.width.min(self.height)
    }
}

/// The four corners of one face of a box, wound anticlockwise seen from
/// outside, in the model's own 0..16 space.
///
/// Order is the game's: down, up, north, south, west, east.
fn face_corners(face: u8, from: [f32; 3], to: [f32; 3]) -> [[f32; 3]; 4] {
    let [x0, y0, z0] = from;
    let [x1, y1, z1] = to;
    match face {
        0 => [[x0, y0, z1], [x1, y0, z1], [x1, y0, z0], [x0, y0, z0]],
        1 => [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
        2 => [[x1, y1, z0], [x0, y1, z0], [x0, y0, z0], [x1, y0, z0]],
        3 => [[x0, y1, z1], [x1, y1, z1], [x1, y0, z1], [x0, y0, z1]],
        4 => [[x0, y1, z0], [x0, y1, z1], [x0, y0, z1], [x0, y0, z0]],
        _ => [[x1, y1, z1], [x1, y1, z0], [x1, y0, z0], [x1, y0, z1]],
    }
}

/// The game's own face brightness, picked by where the face ended up pointing.
fn shade_for(corners: &[[f32; 3]; 4]) -> f32 {
    let n = normal(corners);
    if n[1] > 0.5 {
        1.0
    } else if n[1] < -0.5 {
        0.5
    } else if n[2].abs() > n[0].abs() {
        0.8
    } else {
        0.6
    }
}

fn normal(c: &[[f32; 3]; 4]) -> [f32; 3] {
    let a = [c[1][0] - c[0][0], c[1][1] - c[0][1], c[1][2] - c[0][2]];
    let b = [c[3][0] - c[0][0], c[3][1] - c[0][1], c[3][2] - c[0][2]];
    let n = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
    [n[0] / length, n[1] / length, n[2] / length]
}

/// True for a face whose outside is turned away from the viewer, who is out
/// along positive z.
fn facing_away(corners: &[[f32; 3]; 4]) -> bool {
    normal(corners)[2] <= 0.0
}

/// A face's texture can be turned in quarter steps, which is how one texture
/// serves four sides of a thing that is not square.
fn turn_uv(u: f32, v: f32, quarters: u8) -> (f32, f32) {
    match quarters & 3 {
        1 => (v, 1.0 - u),
        2 => (1.0 - u, 1.0 - v),
        3 => (1.0 - v, u),
        _ => (u, v),
    }
}

/// Where a point in block space, already placed by the display transform, lands
/// on the icon.
///
/// Straight down the z axis: the model has already been turned, so there is
/// nothing left for the camera to do but drop a coordinate. Depth is kept so
/// faces do not have to be sorted.
fn project(p: [f32; 3]) -> ([f32; 2], f32) {
    let half = ICON_SIZE as f32 / 2.0;
    ([half + p[0] * ICON_SIZE as f32, half - p[1] * ICON_SIZE as f32], p[2])
}

/// Fills one projected face, asking `sample` for the colour at each point.
///
/// A face is a parallelogram in space and the projection drops an axis, so its
/// image is a parallelogram too and the mapping back into it is exact -- no
/// triangles, no perspective divide, no seam down the middle of every quad.
fn fill(
    out: &mut [u8],
    depth: &mut [f32],
    corners: [[f32; 3]; 4],
    sample: impl Fn(f32, f32) -> Option<[u8; 4]>,
) {
    let projected: Vec<([f32; 2], f32)> = corners.iter().map(|c| project(*c)).collect();
    let origin = projected[0].0;
    let edge_u = [projected[1].0[0] - origin[0], projected[1].0[1] - origin[1]];
    let edge_v = [projected[3].0[0] - origin[0], projected[3].0[1] - origin[1]];
    let det = edge_u[0] * edge_v[1] - edge_u[1] * edge_v[0];
    if det.abs() < 1e-6 {
        return;
    }
    let (near, du, dv) =
        (projected[0].1, projected[1].1 - projected[0].1, projected[3].1 - projected[0].1);

    let xs = projected.iter().map(|p| p.0[0]);
    let ys = projected.iter().map(|p| p.0[1]);
    let min_x = xs.clone().fold(f32::INFINITY, f32::min).floor().max(0.0) as u32;
    let max_x = xs.fold(f32::NEG_INFINITY, f32::max).ceil().clamp(0.0, ICON_SIZE as f32) as u32;
    let min_y = ys.clone().fold(f32::INFINITY, f32::min).floor().max(0.0) as u32;
    let max_y = ys.fold(f32::NEG_INFINITY, f32::max).ceil().clamp(0.0, ICON_SIZE as f32) as u32;

    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = [x as f32 + 0.5 - origin[0], y as f32 + 0.5 - origin[1]];
            let u = (px[0] * edge_v[1] - px[1] * edge_v[0]) / det;
            let v = (edge_u[0] * px[1] - edge_u[1] * px[0]) / det;
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let here = near + du * u + dv * v;
            let slot = (y * ICON_SIZE + x) as usize;
            if depth[slot] > here {
                continue;
            }
            let Some(colour) = sample(u, v) else { continue };
            if colour[3] < 128 {
                // Cut out rather than blended, the way the game draws a model:
                // a half-transparent texel writing depth is what puts a hole in
                // whatever is behind it.
                continue;
            }
            depth[slot] = here;
            let at = slot * 4;
            out[at..at + 4].copy_from_slice(&colour);
            out[at + 3] = 255;
        }
    }
}

/// Composites one straight-alpha pixel over another.
fn over(out: &mut [u8], at: usize, src: &[u8]) {
    let a = src[3] as f32 / 255.0;
    if a <= 0.0 {
        return;
    }
    for c in 0..3 {
        let under = out[at + c] as f32;
        out[at + c] = (src[c] as f32 * a + under * (1.0 - a)) as u8;
    }
    let under = out[at + 3] as f32 / 255.0;
    out[at + 3] = ((a + under * (1.0 - a)) * 255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Rotation;

    #[test]
    fn a_face_turned_flat_reads_as_a_top() {
        // A north face turned ninety degrees about x is pointing up.
        let rotation = Rotation { origin: [8.0, 8.0, 8.0], axis: 0, angle: -90.0, rescale: false };
        let mut corners = face_corners(2, [0.0, 0.0, 0.0], [16.0, 16.0, 16.0]);
        for corner in corners.iter_mut() {
            *corner = rotation.apply(*corner);
        }
        assert!((shade_for(&corners) - 1.0).abs() < 1e-3, "it should be lit like a top");
    }

    #[test]
    fn the_gui_transform_turns_a_cube_into_a_corner_view() {
        // The game's own numbers for a block in a slot.
        let display = Display {
            rotation: [30.0, 225.0, 0.0],
            translation: [0.0; 3],
            scale: [0.625; 3],
        };
        // The near top corner of the cube must be nearer than the far one, or
        // the block is being looked at from inside.
        let near = display.apply([0.5, 0.5, 0.5]);
        let far = display.apply([-0.5, 0.5, -0.5]);
        assert!(near[2] != far[2], "the two corners cannot be at the same depth");
        // And the whole thing has to fit the slot it is drawn in.
        for corner in [[-0.5f32, -0.5, -0.5], [0.5, 0.5, 0.5], [-0.5, 0.5, 0.5], [0.5, -0.5, -0.5]]
        {
            let p = display.apply(corner);
            assert!(p[0].abs() <= 0.5 && p[1].abs() <= 0.5, "a corner left the slot: {p:?}");
        }
    }

    #[test]
    fn a_quarter_turn_of_a_texture_comes_back_round() {
        let (u, v) = (0.25f32, 0.75f32);
        let mut turned = (u, v);
        for _ in 0..4 {
            turned = turn_uv(turned.0, turned.1, 1);
        }
        assert!((turned.0 - u).abs() < 1e-6 && (turned.1 - v).abs() < 1e-6);
    }
}
