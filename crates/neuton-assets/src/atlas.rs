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
#[derive(Debug, Clone, Copy, PartialEq)]
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
pub struct Atlas {
    /// RGBA8, `size` by `size` pixels.
    pub pixels: Vec<u8>,
    pub size: u32,
    /// Edge length of one tile in pixels.
    pub tile: u32,
    uvs: BTreeMap<String, Uv>,
    /// Where an unresolved texture points. Magenta, so a mistake is obvious.
    missing: Uv,
}

impl Atlas {
    /// Looks a texture up by its pack-relative path.
    pub fn uv(&self, path: &str) -> Uv {
        self.uvs.get(path).copied().unwrap_or(self.missing)
    }

    pub fn contains(&self, path: &str) -> bool {
        self.uvs.contains_key(path)
    }

    pub fn len(&self) -> usize {
        self.uvs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.uvs.is_empty()
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

        // One extra slot for the missing-texture tile.
        let count = images.len() + 1;
        let per_row = (count as f64).sqrt().ceil() as u32;
        let size = (per_row * tile).next_power_of_two();
        let per_row = size / tile;

        let mut pixels = vec![0u8; (size * size * 4) as usize];
        let mut uvs = BTreeMap::new();

        let blit = |slot: u32, src: &Image, pixels: &mut Vec<u8>| -> Uv {
            let (tx, ty) = (slot % per_row, slot / per_row);
            let (ox, oy) = (tx * tile, ty * tile);
            for y in 0..tile {
                for x in 0..tile {
                    // Nearest-neighbour: sharp edges matter more than smooth
                    // ones on 16-pixel textures.
                    let sx = x * src.frame_size() / tile;
                    let sy = y * src.frame_size() / tile;
                    let s = ((sy * src.width + sx) * 4) as usize;
                    let d = (((oy + y) * size + ox + x) * 4) as usize;
                    pixels[d..d + 4].copy_from_slice(&src.rgba[s..s + 4]);
                }
            }
            // Half a texel in from each edge. Sampling exactly on the boundary
            // bleeds the neighbouring tile in at distance, which shows up as a
            // bright seam along every block edge.
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
        }

        // The last slot is a magenta and black check, the traditional signal
        // that a texture did not resolve.
        let missing_image = missing_tile(tile);
        let missing = blit(images.len() as u32, &missing_image, &mut pixels);

        Atlas { pixels, size, tile, uvs, missing }
    }
}

/// A decoded texture, possibly an animation strip.
struct Image {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

impl Image {
    /// Edge length of one frame.
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

fn decode(bytes: &[u8]) -> Option<Image> {
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
