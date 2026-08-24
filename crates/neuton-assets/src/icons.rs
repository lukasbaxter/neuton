//! Drawing an item the way it appears in a slot.
//!
//! Two shapes of thing end up in an inventory. Most items are a flat 16x16
//! picture and just need scaling. The rest are blocks, and a block in a slot is
//! drawn as a small solid object seen from a corner -- which means actually
//! projecting its model, not pasting its side texture into a square. Stairs
//! look like stairs because they are two boxes, and a slab is half a block
//! high, and both of those fall out of drawing what the model says.

use crate::atlas::{Image, decode};
use crate::models::{Element, ModelResolver};
use crate::pack::PackStack;
use std::collections::HashMap;

/// An item picture, ready to hand to the UI.
pub struct Icon {
    pub size: u32,
    /// RGBA, premultiplied by nothing: straight alpha.
    pub pixels: Vec<u8>,
}

/// How big icons are drawn. Twice the texture resolution, so the isometric
/// diagonals have somewhere to land instead of stair-stepping.
pub const ICON_SIZE: u32 = 32;

/// Vanilla's grass and foliage colours outside any biome, which is what an
/// inventory slot is.
const GRASS: [f32; 3] = [0.569, 0.741, 0.349];
const FOLIAGE: [f32; 3] = [0.467, 0.671, 0.184];

/// Face brightness, matching the shading the game gives a block in a slot.
const TOP: f32 = 1.0;
const SIDE_Z: f32 = 0.8;
const SIDE_X: f32 = 0.6;

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

    /// Draws one item. `block` is the block state it places, if it places one.
    pub fn render(&mut self, packs: &mut PackStack, name: &str, block: Option<&str>) -> Option<Icon> {
        // A flat picture is both the common case and the cheap one, so it is
        // tried first. Block items mostly have no flat model at all.
        if let Some(icon) = self.flat(packs, name) {
            return Some(icon);
        }
        self.block(packs, block?, name)
    }

    /// A 16x16 sprite, scaled up, with any further layers composited over it.
    fn flat(&mut self, packs: &mut PackStack, name: &str) -> Option<Icon> {
        let model = packs.read_json(&format!("assets/minecraft/models/item/{name}.json"))?;
        let mut out = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
        let mut drew = false;
        for layer in 0..4 {
            let key = format!("layer{layer}");
            let Some(path) = model.get("textures").and_then(|t| t.get(&key)).and_then(|v| v.as_str())
            else {
                break;
            };
            let Some(image) = self.texture(packs, path) else { continue };
            let (w, h) = (image.width, image.frame());
            for y in 0..ICON_SIZE {
                for x in 0..ICON_SIZE {
                    let sx = x * w / ICON_SIZE;
                    let sy = y * h / ICON_SIZE;
                    let i = ((sy * w + sx) * 4) as usize;
                    let Some(px) = image.rgba.get(i..i + 4) else { continue };
                    over(&mut out, ((y * ICON_SIZE + x) * 4) as usize, px, 1.0);
                }
            }
            drew = true;
        }
        drew.then_some(Icon { size: ICON_SIZE, pixels: out })
    }

    /// The block model, projected from a corner.
    fn block(&mut self, packs: &mut PackStack, block: &str, item: &str) -> Option<Icon> {
        let model = self.resolver.model(packs, &format!("minecraft:{block}"), "")?;
        if model.elements.is_empty() {
            return None;
        }
        let tint = if item.ends_with("leaves") || item.ends_with("vine") { FOLIAGE } else { GRASS };

        let mut out = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
        let mut depth = vec![f32::NEG_INFINITY; (ICON_SIZE * ICON_SIZE) as usize];
        // Front to back would need sorting; a depth value per pixel does not,
        // and a model is only ever a handful of boxes.
        for element in &model.elements {
            self.draw_element(packs, element, tint, &mut out, &mut depth);
        }
        out.chunks_exact(4).any(|p| p[3] > 0).then_some(Icon { size: ICON_SIZE, pixels: out })
    }

    fn draw_element(
        &mut self,
        packs: &mut PackStack,
        element: &Element,
        tint: [f32; 3],
        out: &mut [u8],
        depth: &mut [f32],
    ) {
        let (a, b) = (element.from, element.to);
        // Only the three faces a corner view can see. Up, south and east, given
        // an eye off the positive corner of all three axes.
        let faces: [(usize, [[f32; 3]; 4], f32); 3] = [
            (
                1,
                [[a[0], b[1], a[2]], [b[0], b[1], a[2]], [b[0], b[1], b[2]], [a[0], b[1], b[2]]],
                TOP,
            ),
            (
                3,
                [[a[0], b[1], b[2]], [b[0], b[1], b[2]], [b[0], a[1], b[2]], [a[0], a[1], b[2]]],
                SIDE_Z,
            ),
            (
                5,
                [[b[0], b[1], b[2]], [b[0], b[1], a[2]], [b[0], a[1], a[2]], [b[0], a[1], b[2]]],
                SIDE_X,
            ),
        ];

        for (index, corners, shade) in faces {
            let Some(face) = element.faces[index].as_ref() else { continue };
            let Some(image) = self.texture(packs, &face.texture).map(|i| (i.rgba.clone(), i.width, i.frame()))
            else {
                continue;
            };
            let uv = face.uv;
            let colour = if face.tinted { tint } else { [1.0; 3] };
            fill(out, depth, corners, |u, v| {
                // The face shows a patch of its texture, in the model's 0..16
                // space. A lantern's side is a 6x7 corner of one, not the whole.
                let tu = (uv[0] + (uv[2] - uv[0]) * u) / 16.0;
                let tv = (uv[1] + (uv[3] - uv[1]) * v) / 16.0;
                let (rgba, w, h) = &image;
                let sx = ((tu * *w as f32) as u32).min(w.saturating_sub(1));
                let sy = ((tv * *h as f32) as u32).min(h.saturating_sub(1));
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

    fn texture(&mut self, packs: &mut PackStack, path: &str) -> Option<&Image> {
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
    fn frame(&self) -> u32 {
        self.width.min(self.height)
    }
}

/// Projects a point in the model's 0..16 space onto the icon.
///
/// A corner view: x goes right and towards the viewer, z goes left and towards
/// the viewer, y goes up. Depth is how near the viewer a point is, so faces do
/// not need sorting.
fn project(p: [f32; 3]) -> ([f32; 2], f32) {
    const COS30: f32 = 0.866_025_4;
    let sx = (p[0] - p[2]) * COS30;
    let sy = (p[0] + p[2]) * 0.5 - p[1];
    // The projected cube is 32 wide against 27.7 across, so fitting the height
    // and centring the width keeps it square-on rather than stretched.
    let scale = ICON_SIZE as f32 / 32.0;
    ([ICON_SIZE as f32 / 2.0 + sx * scale, ICON_SIZE as f32 / 2.0 + sy * scale], p[0] + p[1] + p[2])
}

/// Fills one projected face, asking `sample` for the colour at each point.
///
/// The face is an axis-aligned rectangle under an affine projection, so its
/// image is a parallelogram and the mapping back into it is exact.
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

    let xs = projected.iter().map(|p| p.0[0]);
    let ys = projected.iter().map(|p| p.0[1]);
    let min_x = xs.clone().fold(f32::INFINITY, f32::min).floor().max(0.0) as u32;
    let max_x = xs.fold(f32::NEG_INFINITY, f32::max).ceil().min(ICON_SIZE as f32) as u32;
    let min_y = ys.clone().fold(f32::INFINITY, f32::min).floor().max(0.0) as u32;
    let max_y = ys.fold(f32::NEG_INFINITY, f32::max).ceil().min(ICON_SIZE as f32) as u32;
    let near = projected.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);

    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = [x as f32 + 0.5 - origin[0], y as f32 + 0.5 - origin[1]];
            let u = (px[0] * edge_v[1] - px[1] * edge_v[0]) / det;
            let v = (edge_u[0] * px[1] - edge_u[1] * px[0]) / det;
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let slot = (y * ICON_SIZE + x) as usize;
            if depth[slot] > near {
                continue;
            }
            let Some(colour) = sample(u, v) else { continue };
            if colour[3] == 0 {
                continue;
            }
            depth[slot] = near;
            over(out, slot * 4, &colour, 1.0);
        }
    }
}

/// Composites one straight-alpha pixel over another.
fn over(out: &mut [u8], at: usize, src: &[u8], scale: f32) {
    let a = src[3] as f32 / 255.0 * scale;
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
