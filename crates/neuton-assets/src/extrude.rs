//! Building a solid out of a flat item picture.
//!
//! A sword in the hand is not a sheet of paper. The game takes the sprite and
//! makes a model out of it a sixteenth of a block thick: the picture on the
//! front, the picture on the back, and a rim of little quads wherever the
//! silhouette has an edge. Turn it side on and you see the thickness, and the
//! rim is the whole of why -- without it, a held item disappears at the
//! quarter turn where the two faces line up edge to edge.
//!
//! Only the rim is built here. The front and back are one quad each and the
//! caller already draws them.

/// One quad along the edge of a sprite, in the model's own 0..1 space with the
/// picture facing negative z.
#[derive(Debug, Clone, PartialEq)]
pub struct Side {
    pub corners: [[f32; 3]; 4],
    /// Where to read the colour, in the sprite's own 0..1 space.
    pub uv: [[f32; 2]; 4],
    /// How much light this face keeps, by the same rule the world uses.
    pub shade: f32,
}

/// How thick the solid is: one pixel of the sixteen a block is drawn in.
pub const THICKNESS: f32 = 1.0 / 16.0;

/// A texel counts as part of the shape at this alpha or above, which is the
/// same cut-off the shader uses.
const OPAQUE: u8 = 128;

/// Builds the rim of the solid the game makes out of one sprite.
///
/// Runs of edge along the same line are merged into one quad, so a plain
/// rectangle costs four and a sword costs a few dozen rather than one per
/// pixel of its outline.
pub fn sides(rgba: &[u8], width: u32, height: u32) -> Vec<Side> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let (w, h) = (width as usize, height as usize);
    // An animated texture is a vertical strip; a held item shows one frame.
    let frame = h.min(w);
    let solid = |x: isize, y: isize| -> bool {
        if x < 0 || y < 0 || x >= w as isize || y >= frame as isize {
            return false;
        }
        rgba.get((y as usize * w + x as usize) * 4 + 3).is_some_and(|a| *a >= OPAQUE)
    };

    let (front, back) = (0.5 - THICKNESS * 0.5, 0.5 + THICKNESS * 0.5);
    let (sw, sh) = (w as f32, frame as f32);
    let mut out = Vec::new();

    // Left and right edges: a run down a column of the picture becomes one
    // tall, thin quad.
    for x in 0..w as isize {
        for facing in [-1isize, 1] {
            let mut run: Option<(isize, isize)> = None;
            for y in 0..=frame as isize {
                let edge = y < frame as isize && solid(x, y) && !solid(x + facing, y);
                match (&mut run, edge) {
                    (None, true) => run = Some((y, y)),
                    (Some(span), true) => span.1 = y,
                    (Some(span), false) => {
                        let (y0, y1) = *span;
                        // The plane the quad sits in: the left side of the
                        // column when the gap is to the left, the right when
                        // it is to the right.
                        let plane = (x + if facing < 0 { 0 } else { 1 }) as f32 / sw;
                        let top = 1.0 - y0 as f32 / sh;
                        let bottom = 1.0 - (y1 + 1) as f32 / sh;
                        let u = (x as f32 + 0.5) / sw;
                        out.push(Side {
                            corners: [
                                [plane, bottom, front],
                                [plane, top, front],
                                [plane, top, back],
                                [plane, bottom, back],
                            ],
                            uv: [
                                [u, (y1 + 1) as f32 / sh],
                                [u, y0 as f32 / sh],
                                [u, y0 as f32 / sh],
                                [u, (y1 + 1) as f32 / sh],
                            ],
                            shade: 0.6,
                        });
                        run = None;
                    }
                    (None, false) => {}
                }
            }
        }
    }

    // Top and bottom edges: a run along a row becomes one wide, flat quad.
    for y in 0..frame as isize {
        for facing in [-1isize, 1] {
            let mut run: Option<(isize, isize)> = None;
            for x in 0..=w as isize {
                let edge = x < w as isize && solid(x, y) && !solid(x, y + facing);
                match (&mut run, edge) {
                    (None, true) => run = Some((x, x)),
                    (Some(span), true) => span.1 = x,
                    (Some(span), false) => {
                        let (x0, x1) = *span;
                        // Rows are numbered downwards and the model is not, so
                        // a gap above is a plane at the top of the row.
                        let plane = 1.0 - (y + if facing < 0 { 0 } else { 1 }) as f32 / sh;
                        let left = x0 as f32 / sw;
                        let right = (x1 + 1) as f32 / sw;
                        let v = (y as f32 + 0.5) / sh;
                        out.push(Side {
                            corners: [
                                [left, plane, front],
                                [right, plane, front],
                                [right, plane, back],
                                [left, plane, back],
                            ],
                            uv: [[left, v], [right, v], [right, v], [left, v]],
                            shade: 0.8,
                        });
                        run = None;
                    }
                    (None, false) => {}
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sprite with one solid rectangle in the middle of it.
    fn block(width: u32, height: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<u8> {
        let mut out = vec![0u8; (width * height * 4) as usize];
        for y in y0..y1 {
            for x in x0..x1 {
                let at = ((y * width + x) * 4) as usize;
                out[at..at + 4].copy_from_slice(&[200, 100, 50, 255]);
            }
        }
        out
    }

    #[test]
    fn a_plain_rectangle_costs_four_quads() {
        let pixels = block(16, 16, 4, 4, 12, 12);
        let sides = sides(&pixels, 16, 16);
        assert_eq!(sides.len(), 4, "one run per edge, not one quad per pixel");
    }

    #[test]
    fn nothing_solid_has_no_edges() {
        assert!(sides(&vec![0u8; 16 * 16 * 4], 16, 16).is_empty());
    }

    #[test]
    fn a_full_sprite_still_has_a_rim() {
        let pixels = block(16, 16, 0, 0, 16, 16);
        let sides = sides(&pixels, 16, 16);
        assert_eq!(sides.len(), 4, "the edge of the picture is an edge");
    }

    #[test]
    fn the_rim_is_a_sixteenth_thick_and_stays_inside_the_block() {
        let pixels = block(16, 16, 4, 4, 12, 12);
        for side in sides(&pixels, 16, 16) {
            let z: Vec<f32> = side.corners.iter().map(|c| c[2]).collect();
            let (lo, hi) = (
                z.iter().copied().fold(f32::MAX, f32::min),
                z.iter().copied().fold(f32::MIN, f32::max),
            );
            assert!((hi - lo - THICKNESS).abs() < 1e-6, "thickness was {}", hi - lo);
            for corner in side.corners {
                assert!(
                    (0.0..=1.0).contains(&corner[0]) && (0.0..=1.0).contains(&corner[1]),
                    "a corner left the block: {corner:?}"
                );
            }
        }
    }

    #[test]
    fn a_hole_in_the_middle_has_edges_of_its_own() {
        let mut pixels = block(16, 16, 4, 4, 12, 12);
        // Punch out one texel: it should add four more quads, since the runs
        // either side of it no longer join.
        let at = ((8 * 16 + 8) * 4) as usize;
        pixels[at + 3] = 0;
        let sides = sides(&pixels, 16, 16);
        assert_eq!(sides.len(), 8, "the outside rim plus the rim of the hole");
    }
}
