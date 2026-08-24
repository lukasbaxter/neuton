//! A minimal PNG writer, for screenshots and for looking at what the renderer
//! produced.
//!
//! Stored deflate blocks rather than real compression: the output is a
//! debugging artefact, and an encoder dependency is not worth the few seconds
//! it would save writing one.

/// Encodes RGBA8 pixels as a PNG.
pub fn encode_rgba(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity(pixels.len() + height as usize);
    let stride = (width * 4) as usize;
    for row in 0..height as usize {
        raw.push(0); // filter: none
        raw.extend_from_slice(&pixels[row * stride..(row + 1) * stride]);
    }

    let mut z = vec![0x78u8, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        z.push(u8::from(last));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA

    let mut out = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &z));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
}

fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &byte in data {
        c ^= byte as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    c ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_has_a_png_signature_and_the_right_chunks() {
        let pixels = vec![255u8; 4 * 4 * 4];
        let png = encode_rgba(&pixels, 4, 4);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert!(png.windows(4).any(|w| w == b"IHDR"));
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert!(png.windows(4).any(|w| w == b"IEND"));
    }

    #[test]
    fn it_round_trips_through_a_real_decoder() {
        // Encoding something a decoder rejects would make every screenshot
        // useless, so the check is against an actual PNG reader.
        let mut pixels = Vec::new();
        for y in 0..8u8 {
            for x in 0..8u8 {
                pixels.extend_from_slice(&[x * 32, y * 32, 128, 255]);
            }
        }
        let png = encode_rgba(&pixels, 8, 8);

        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().expect("decodes");
        let mut out = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut out).expect("frame");
        assert_eq!((info.width, info.height), (8, 8));
        assert_eq!(&out[..pixels.len()], &pixels[..]);
    }
}
