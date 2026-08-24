//! Server icons.
//!
//! Servers send a 64x64 PNG in their status response. Decoded once per row and
//! cached, since the list repaints on every frame it is visible.

use std::collections::HashMap;

/// Decodes a server favicon into an egui image.
///
/// Returns `None` rather than erroring for anything unexpected: a broken icon
/// should leave a blank square, never stop the row from drawing.
pub fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Normalise the many PNG flavours down to 8-bit RGB or RGBA so the match
    // below stays short.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);

    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;

    let (w, h) = (info.width as usize, info.height as usize);
    // A hostile server could claim a huge icon; vanilla only ever sends 64x64.
    if w == 0 || h == 0 || w > 256 || h > 256 {
        return None;
    }

    let pixels: Vec<egui::Color32> = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4]
            .chunks_exact(4)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect(),
        png::ColorType::Rgb => buf[..w * h * 3]
            .chunks_exact(3)
            .map(|p| egui::Color32::from_rgb(p[0], p[1], p[2]))
            .collect(),
        png::ColorType::Grayscale => {
            buf[..w * h].iter().map(|&g| egui::Color32::from_gray(g)).collect()
        }
        png::ColorType::GrayscaleAlpha => buf[..w * h * 2]
            .chunks_exact(2)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[0], p[0], p[1]))
            .collect(),
        png::ColorType::Indexed => return None,
    };
    if pixels.len() != w * h {
        return None;
    }
    Some(egui::ColorImage::new([w, h], pixels))
}

/// Keeps one texture per server row.
#[derive(Default)]
pub struct IconCache {
    textures: HashMap<u64, Option<egui::TextureHandle>>,
}

impl IconCache {
    /// Returns the texture for a row, decoding and uploading on first sight.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        id: u64,
        png_bytes: Option<&[u8]>,
    ) -> Option<&egui::TextureHandle> {
        if !self.textures.contains_key(&id) {
            let handle = png_bytes.and_then(decode).map(|img| {
                ctx.load_texture(
                    format!("server-icon-{id}"),
                    img,
                    // Nearest, so a 64x64 pixel-art icon stays crisp rather
                    // than turning to mush at 40 px.
                    egui::TextureOptions::NEAREST,
                )
            });
            self.textures.insert(id, handle);
        }
        self.textures.get(&id).and_then(|t| t.as_ref())
    }

    /// Forgets a row's icon so the next ping re-decodes it.
    pub fn invalidate(&mut self, id: u64) {
        self.textures.remove(&id);
    }

    pub fn retain(&mut self, ids: &[u64]) {
        self.textures.retain(|id, _| ids.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_bytes_decode_to_nothing_rather_than_panicking() {
        assert!(decode(b"not a png").is_none());
        assert!(decode(&[]).is_none());
        // A valid PNG signature followed by nothing usable.
        assert!(decode(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).is_none());
    }
}
