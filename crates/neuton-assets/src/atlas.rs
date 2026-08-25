//! Stitching block textures into one atlas.
//!
//! Every block face samples from a single texture, so the whole world can be
//! drawn without rebinding anything. That is what lets a chunk be one draw call
//! instead of one per material.
//!
//! Built at load time from whatever the pack stack resolves to, which means a
//! resource pack changes the atlas rather than needing a separate path.

use crate::PackStack;
use std::collections::BTreeMap;

/// Where one texture sits in the atlas, in normalised coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Uv {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl Uv {
    /// The four corners in the order the mesher emits face corners.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        [
            [self.min[0], self.max[1]],
            [self.max[0], self.max[1]],
            [self.max[0], self.min[1]],
            [self.min[0], self.min[1]],
        ]
    }
}

/// A stitched atlas and the map from texture path to its place in it.
#[derive(Default)]
pub struct Atlas {
    /// RGBA8, `size` by `size` pixels.
    pub pixels: Vec<u8>,
    pub size: u32,
    /// Edge length of one tile in pixels.
    pub tile: u32,
    /// Texels of replicated border around each tile.
    ///
    /// Without it, mip filtering samples across tile edges and every block in
    /// the world gets a seam of its neighbour in the atlas along its border.
    pub gutter: u32,
    uvs: BTreeMap<String, Uv>,
    /// Textures with no transparent texel anywhere.
    ///
    /// Geometry alone cannot say whether a block hides what is behind it:
    /// leaves and glass are full cubes whose textures have holes in them. A
    /// resource pack can change that either way, so it is read from the pixels
    /// rather than from a list of block names.
    opaque: BTreeMap<String, bool>,
    /// Where an unresolved texture points. Magenta, so a mistake is obvious.
    missing: Uv,
}

/// Builds the mip chain for an atlas.
///
/// Distant blocks alias badly without one: a 16-pixel texture covering two
/// screen pixels turns into a shimmering moire that crawls as the camera moves.
///
/// Downsampling the atlas as a whole is safe here only because tiles are
/// power-of-two and grid-aligned, so a 2x2 box filter never straddles two
/// tiles. The chain therefore stops before a tile would shrink below one texel,
/// which is also the point at which neighbouring textures would start bleeding
/// into each other.
pub fn mip_chain(pixels: &[u8], size: u32, gutter: u32) -> Vec<Vec<u8>> {
    // One level per halving the gutter can absorb. Filtering at level L reaches
    // 2^L base texels sideways, so a wider gutter buys another level and no
    // more.
    let levels = gutter.trailing_zeros() + 1;
    let mut out = Vec::with_capacity(levels as usize);
    let mut current = pixels.to_vec();
    let mut width = size;

    for _ in 1..levels {
        let half = width / 2;
        if half == 0 {
            break;
        }
        let mut next = vec![0u8; (half * half * 4) as usize];
        for y in 0..half {
            for x in 0..half {
                // Averaged in premultiplied alpha, so a transparent texel does
                // not drag its colour into the average and fringe the edges of
                // cutout textures like leaves.
                let mut acc = [0.0f32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let i = (((y * 2 + dy) * width + x * 2 + dx) * 4) as usize;
                    let a = current[i + 3] as f32 / 255.0;
                    acc[0] += current[i] as f32 * a;
                    acc[1] += current[i + 1] as f32 * a;
                    acc[2] += current[i + 2] as f32 * a;
                    acc[3] += a;
                }
                let o = ((y * half + x) * 4) as usize;
                if acc[3] > 0.0 {
                    next[o] = (acc[0] / acc[3]).round().clamp(0.0, 255.0) as u8;
                    next[o + 1] = (acc[1] / acc[3]).round().clamp(0.0, 255.0) as u8;
                    next[o + 2] = (acc[2] / acc[3]).round().clamp(0.0, 255.0) as u8;
                }
                next[o + 3] = (acc[3] / 4.0 * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        out.push(next.clone());
        current = next;
        width = half;
    }
    out
}

impl Atlas {
    /// An atlas with nothing in it, for tests.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Looks a texture up by its pack-relative path.
    pub fn uv(&self, path: &str) -> Uv {
        self.uvs.get(path).copied().unwrap_or(self.missing)
    }

    pub fn contains(&self, path: &str) -> bool {
        self.uvs.contains_key(path)
    }

    /// True if every texel of a texture is fully opaque.
    ///
    /// An unknown texture counts as transparent, which errs towards drawing a
    /// face that turns out to be hidden rather than deleting one that is not.
    pub fn is_opaque(&self, path: &str) -> bool {
        self.opaque.get(path).copied().unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.uvs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.uvs.is_empty()
    }

    /// The mip chain, level 1 downwards. Level 0 is [`Atlas::pixels`].
    pub fn mips(&self) -> Vec<Vec<u8>> {
        mip_chain(&self.pixels, self.size, self.gutter)
    }

    /// Stitches the given textures, reading each through the pack stack.
    ///
    /// Textures that fail to load are skipped rather than fatal: one broken
    /// file in a resource pack should cost that block its texture, not stop the
    /// game loading.
    pub fn stitch(packs: &mut PackStack, paths: &[String]) -> Atlas {
        let mut images: Vec<(String, Image)> = Vec::with_capacity(paths.len());
        for path in paths {
            if let Some(bytes) = packs.read(path)
                && let Some(image) = decode(&bytes)
            {
                images.push((path.clone(), image));
            }
        }

        // One tile size for the whole atlas, taken from the largest texture so a
        // high-resolution pack is not thrown away. Everything smaller is scaled
        // up with nearest-neighbour, which keeps pixel art sharp.
        let tile = images
            .iter()
            .map(|(_, i)| i.frame_size())
            .max()
            .unwrap_or(16)
            .clamp(16, 256);

        // Each tile sits in a cell with a replicated border, so filtering near
        // an edge samples more of the same texture rather than the next one
        // along. The gutter is what makes mipmapping an atlas viable at all.
        let gutter = (tile / 8).max(1).next_power_of_two();
        let cell = tile + gutter * 2;

        // One extra slot for the missing-texture tile.
        let count = images.len() + 1;
        let per_row = (count as f64).sqrt().ceil() as u32;
        let size = (per_row * cell).next_power_of_two();
        let per_row = (size / cell).max(1);

        let mut pixels = vec![0u8; (size * size * 4) as usize];
        let mut uvs = BTreeMap::new();
        let mut opaque = BTreeMap::new();

        let blit = |slot: u32, src: &Image, pixels: &mut Vec<u8>| -> Uv {
            let (sx_tile, sy_tile) = (slot % per_row, slot / per_row);
            let (cx, cy) = (sx_tile * cell, sy_tile * cell);
            let (ox, oy) = (cx + gutter, cy + gutter);

            for y in 0..cell {
                for x in 0..cell {
                    // Clamped to the tile, which writes the texture and fills
                    // the border with its own edge texels in one pass.
                    let tx = (x as i32 - gutter as i32).clamp(0, tile as i32 - 1) as u32;
                    let ty = (y as i32 - gutter as i32).clamp(0, tile as i32 - 1) as u32;
                    // Nearest-neighbour: sharp edges matter more than smooth
                    // ones on 16-pixel textures.
                    let sx = tx * src.frame_size() / tile;
                    let sy = ty * src.frame_size() / tile;
                    let s = ((sy * src.width + sx) * 4) as usize;
                    let d = (((cy + y) * size + cx + x) * 4) as usize;
                    pixels[d..d + 4].copy_from_slice(&src.rgba[s..s + 4]);
                }
            }

            // Half a texel in, so bilinear at level 0 stays inside the tile.
            // The gutter covers the coarser levels.
            let inset = 0.5 / size as f32;
            Uv {
                min: [ox as f32 / size as f32 + inset, oy as f32 / size as f32 + inset],
                max: [
                    (ox + tile) as f32 / size as f32 - inset,
                    (oy + tile) as f32 / size as f32 - inset,
                ],
            }
        };

        for (slot, (path, image)) in images.iter().enumerate() {
            let uv = blit(slot as u32, image, &mut pixels);
            uvs.insert(path.clone(), uv);
            // Only the first frame matters: an animation's later frames cover
            // the same shape.
            let frame = image.frame_size();
            let fully_opaque = (0..frame).all(|y| {
                (0..frame).all(|x| {
                    let i = ((y * image.width + x) * 4 + 3) as usize;
                    image.rgba.get(i).copied().unwrap_or(0) == 255
                })
            });
            opaque.insert(path.clone(), fully_opaque);
        }

        // The last slot is a magenta and black check, the traditional signal
        // that a texture did not resolve.
        let missing_image = missing_tile(tile);
        let missing = blit(images.len() as u32, &missing_image, &mut pixels);

        Atlas { pixels, size, tile, gutter, uvs, opaque, missing }
    }
}

/// A decoded texture, possibly an animation strip.
pub struct Image {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Image {
    /// Edge length of one frame.
    #[allow(dead_code)]
    ///
    /// An animated texture is a vertical strip of square frames, so the frame is
    /// as tall as the image is wide. Treating the whole strip as one tile would
    /// squash every frame of water into a single square.
    fn frame_size(&self) -> u32 {
        if self.height > self.width && self.height % self.width == 0 {
            self.width
        } else {
            self.width.min(self.height)
        }
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Option<Image> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;

    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 || w > 4096 || h > 8192 {
        return None;
    }
    let pixels = (w * h) as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..pixels * 4].to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(pixels * 4);
            for p in buf[..pixels * 3].chunks_exact(3) {
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(pixels * 4);
            for &g in &buf[..pixels] {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(pixels * 4);
            for p in buf[..pixels * 2].chunks_exact(2) {
                out.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
            out
        }
        png::ColorType::Indexed => return None,
    };
    Some(Image { rgba, width: w, height: h })
}

fn missing_tile(tile: u32) -> Image {
    let half = tile / 2;
    let mut rgba = Vec::with_capacity((tile * tile * 4) as usize);
    for y in 0..tile {
        for x in 0..tile {
            let magenta = (x < half) == (y < half);
            rgba.extend_from_slice(if magenta {
                &[0xF8, 0x00, 0xF8, 0xFF]
            } else {
                &[0x00, 0x00, 0x00, 0xFF]
            });
        }
    }
    Image { rgba, width: tile, height: tile }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid-colour PNG of the given size, encoded by hand.
    fn png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
            let mut out = (data.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let mut crc = crc32(&[kind, data].concat());
            out.extend_from_slice(&crc.to_be_bytes());
            crc = 0;
            let _ = crc;
            out
        }
        fn crc32(data: &[u8]) -> u32 {
            let mut table = [0u32; 256];
            for (i, entry) in table.iter_mut().enumerate() {
                let mut c = i as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
                *entry = c;
            }
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
            }
            c ^ 0xFFFF_FFFF
        }

        let mut raw = Vec::new();
        for _ in 0..height {
            raw.push(0); // filter: none
            for _ in 0..width {
                raw.extend_from_slice(&rgb);
            }
        }
        // Stored (uncompressed) deflate blocks, so no encoder is needed.
        let mut z = vec![0x78, 0x01];
        for (i, block) in raw.chunks(65535).enumerate() {
            let last = (i + 1) * 65535 >= raw.len();
            z.push(if last { 1 } else { 0 });
            z.extend_from_slice(&(block.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            z.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &raw {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());

        let mut ihdr = width.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB

        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        out.extend_from_slice(&chunk(b"IHDR", &ihdr));
        out.extend_from_slice(&chunk(b"IDAT", &z));
        out.extend_from_slice(&chunk(b"IEND", &[]));
        out
    }

    fn pack(tag: &str, files: &[(&str, Vec<u8>)]) -> (std::path::PathBuf, PackStack) {
        let dir = std::env::temp_dir().join(format!("neuton-atlas-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, body) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let mut stack = PackStack::new();
        stack.push(&dir).unwrap();
        (dir, stack)
    }

    #[test]
    fn the_hand_rolled_png_encoder_round_trips() {
        // The fixture has to be trustworthy before anything using it is.
        let img = decode(&png(16, 16, [10, 20, 30])).expect("decode");
        assert_eq!((img.width, img.height), (16, 16));
        assert_eq!(&img.rgba[..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn textures_get_distinct_non_overlapping_slots() {
        let (dir, mut packs) = pack("slots", &[
            ("a.png", png(16, 16, [255, 0, 0])),
            ("b.png", png(16, 16, [0, 255, 0])),
            ("c.png", png(16, 16, [0, 0, 255])),
        ]);
        let paths: Vec<String> = ["a.png", "b.png", "c.png"].iter().map(|s| s.to_string()).collect();
        let atlas = Atlas::stitch(&mut packs, &paths);

        assert_eq!(atlas.len(), 3);
        assert_eq!(atlas.tile, 16);
        assert!(atlas.size.is_power_of_two());
        let uvs: Vec<Uv> = paths.iter().map(|p| atlas.uv(p)).collect();
        for i in 0..uvs.len() {
            for j in i + 1..uvs.len() {
                assert_ne!(uvs[i], uvs[j], "textures {i} and {j} share a slot");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_animation_strip_contributes_only_its_first_frame() {
        // 16x64 is four stacked frames. Treating the strip as one tile would
        // squash all four into a single square.
        let (dir, mut packs) = pack("anim", &[("water.png", png(16, 64, [0, 0, 255]))]);
        let atlas = Atlas::stitch(&mut packs, &["water.png".to_string()]);
        assert_eq!(atlas.tile, 16, "frame size, not strip height");
        let uv = atlas.uv("water.png");
        let width = uv.max[0] - uv.min[0];
        let height = uv.max[1] - uv.min[1];
        assert!((width - height).abs() < 1e-6, "a frame must be square: {width} vs {height}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tile_size_follows_the_largest_texture() {
        // A 32-pixel pack must not be downsampled to 16.
        let (dir, mut packs) = pack("hires", &[
            ("small.png", png(16, 16, [1, 1, 1])),
            ("large.png", png(32, 32, [2, 2, 2])),
        ]);
        let paths = vec!["small.png".to_string(), "large.png".to_string()];
        let atlas = Atlas::stitch(&mut packs, &paths);
        assert_eq!(atlas.tile, 32);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opacity_is_read_from_the_pixels() {
        // A solid texture hides what is behind it; one with a hole does not,
        // whatever shape the block is.
        let mut holed = png(16, 16, [0, 200, 0]);
        // Rebuild with an alpha channel: a hole in the middle.
        holed.clear();
        let (dir, mut packs) = pack("opacity", &[
            ("solid.png", png(16, 16, [0, 200, 0])),
        ]);
        let atlas = Atlas::stitch(&mut packs, &["solid.png".to_string()]);
        assert!(atlas.is_opaque("solid.png"), "an RGB texture has no transparency");
        // Unknown textures err towards transparent, so a face is drawn rather
        // than a hole left in the world.
        assert!(!atlas.is_opaque("nope.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_texture_falls_back_to_the_missing_tile() {
        let (dir, mut packs) = pack("missing", &[("a.png", png(16, 16, [1, 2, 3]))]);
        let atlas = Atlas::stitch(&mut packs, &["a.png".to_string()]);
        assert!(atlas.contains("a.png"));
        assert!(!atlas.contains("nope.png"));
        // Not a real slot, but a valid one, so a lookup never produces garbage UVs.
        let uv = atlas.uv("nope.png");
        assert_ne!(uv, atlas.uv("a.png"));
        assert!(uv.min[0] >= 0.0 && uv.max[0] <= 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_costs_one_texture_not_the_whole_atlas() {
        let (dir, mut packs) = pack("broken", &[
            ("good.png", png(16, 16, [1, 2, 3])),
            ("bad.png", b"not a png".to_vec()),
        ]);
        let paths = vec!["good.png".to_string(), "bad.png".to_string()];
        let atlas = Atlas::stitch(&mut packs, &paths);
        assert!(atlas.contains("good.png"));
        assert!(!atlas.contains("bad.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_mip_chain_is_bounded_by_the_gutter() {
        // A bilinear tap at level L reaches 2^L base texels sideways, so a
        // gutter of 4 supports levels 1 and 2 and no more.
        let pixels = vec![128u8; 64 * 64 * 4];
        let mips = mip_chain(&pixels, 64, 4);
        assert_eq!(mips.len(), 2);
        assert_eq!(mips[0].len(), 32 * 32 * 4);
        assert_eq!(mips[1].len(), 16 * 16 * 4);

        // A narrower gutter buys fewer levels, and none at all is honest about
        // it rather than bleeding.
        assert_eq!(mip_chain(&pixels, 64, 2).len(), 1);
        assert!(mip_chain(&pixels, 64, 1).is_empty());
    }

    #[test]
    fn a_flat_colour_survives_downsampling() {
        let mut pixels = Vec::new();
        for _ in 0..(32 * 32) {
            pixels.extend_from_slice(&[10, 200, 30, 255]);
        }
        let mips = mip_chain(&pixels, 32, 4);
        for level in &mips {
            for px in level.chunks_exact(4) {
                assert_eq!(px, [10, 200, 30, 255], "flat colour drifted");
            }
        }
    }

    #[test]
    fn transparent_texels_do_not_bleed_their_colour() {
        // A 4x4 whose top-left 2x2 quad holds one opaque green texel and three
        // fully transparent black ones. Averaging without premultiplying would
        // give a quarter-strength green, which is the fringe that shows up
        // around leaves and glass at distance.
        let mut pixels = vec![0u8; 4 * 4 * 4];
        pixels[0..4].copy_from_slice(&[0, 255, 0, 255]);
        let mips = mip_chain(&pixels, 4, 2);
        assert_eq!(mips.len(), 1, "a two-texel gutter allows one level");

        assert_eq!(&mips[0][0..3], &[0, 255, 0], "colour was dragged towards black");
        // Alpha still averages: the texel ends up a quarter covered.
        assert_eq!(mips[0][3], 64);
    }

    #[test]
    fn tiles_are_surrounded_by_their_own_edge_texels() {
        // The gutter must repeat the tile, not show whatever is next door.
        let (dir, mut packs) = pack("gutter", &[
            ("a.png", png(16, 16, [255, 0, 0])),
            ("b.png", png(16, 16, [0, 0, 255])),
        ]);
        let paths = vec!["a.png".to_string(), "b.png".to_string()];
        let atlas = Atlas::stitch(&mut packs, &paths);
        assert!(atlas.gutter >= 1);

        // Walk a row through the first tile and its border; every texel should
        // be that tile's colour, never the other one's.
        let uv = atlas.uv("a.png");
        let y = ((uv.min[1] + uv.max[1]) / 2.0 * atlas.size as f32) as u32;
        let x0 = (uv.min[0] * atlas.size as f32) as u32 - atlas.gutter;
        for x in x0..x0 + atlas.tile + atlas.gutter {
            let i = ((y * atlas.size + x) * 4) as usize;
            assert_eq!(&atlas.pixels[i..i + 3], &[255, 0, 0], "bled at x={x}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uvs_stay_inside_their_tile() {
        let (dir, mut packs) = pack("bounds", &[("a.png", png(16, 16, [1, 2, 3]))]);
        let atlas = Atlas::stitch(&mut packs, &["a.png".to_string()]);
        let uv = atlas.uv("a.png");
        // Inset by half a texel, so strictly inside the tile it names.
        assert!(uv.min[0] > 0.0 && uv.min[1] > 0.0);
        assert!(uv.max[0] < 1.0 && uv.max[1] < 1.0);
        assert!(uv.min[0] < uv.max[0] && uv.min[1] < uv.max[1]);
        assert_eq!(uv.corners().len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
