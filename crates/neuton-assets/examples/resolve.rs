//! Resolves every block's textures against the installed vanilla jar and
//! stitches the atlas, writing it out so the result can be inspected.
fn main() {
    let jar = neuton_assets::vanilla_jar("26.2").expect("no vanilla 26.2 jar");
    let mut packs = neuton_assets::PackStack::new();
    packs.push(&jar).unwrap();
    // Any resource pack the user has installed layers on top.
    if let Some(dir) = neuton_assets::resource_pack_dir() {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let _ = packs.push(e.path());
        }
    }
    println!("packs: {:?}", packs.names());

    let mut r = neuton_assets::ModelResolver::new();
    let mut wanted = std::collections::BTreeSet::new();
    let t = std::time::Instant::now();
    let mut resolved = 0usize;
    for i in 0..neuton_blocks::BLOCK_COUNT {
        let id = neuton_blocks::BlockId(i);
        if let Some(ft) = r.textures(&mut packs, id.name(), id.get().default_state.variant_key()) {
            resolved += 1;
            for t in ft.distinct() { wanted.insert(t.to_string()); }
        }
    }
    let resolve_ms = t.elapsed().as_secs_f64() * 1000.0;

    let paths: Vec<String> = wanted.into_iter().collect();
    let t = std::time::Instant::now();
    let atlas = neuton_assets::Atlas::stitch(&mut packs, &paths);
    let stitch_ms = t.elapsed().as_secs_f64() * 1000.0;

    println!("resolved {resolved}/{} blocks in {resolve_ms:.0} ms", neuton_blocks::BLOCK_COUNT);
    println!("atlas    {}x{} px, {} px tiles, {} textures, stitched in {stitch_ms:.0} ms",
        atlas.size, atlas.size, atlas.tile, atlas.len());
    println!("memory   {:.1} MiB", atlas.pixels.len() as f64 / 1048576.0);
    println!("dropped  {} textures failed to load", paths.len() - atlas.len());

    let out = std::env::args().nth(1).unwrap_or_default();
    if !out.is_empty() {
        write_png(&out, &atlas.pixels, atlas.size);
        println!("wrote    {out}");
    }
}

/// Minimal PNG writer, only for inspecting the atlas by eye.
fn write_png(path: &str, rgba: &[u8], size: u32) {
    let mut raw = Vec::with_capacity(rgba.len() + size as usize);
    for y in 0..size {
        raw.push(0);
        let row = (y * size * 4) as usize;
        raw.extend_from_slice(&rgba[row..row + (size * 4) as usize]);
    }
    let mut z = vec![0x78u8, 0x01];
    for (i, block) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(if last { 1 } else { 0 });
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw { a = (a + byte as u32) % 65521; b = (b + a) % 65521; }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &byte in data {
            c ^= byte as u32;
            for _ in 0..8 { c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 }; }
        }
        c ^ 0xFFFF_FFFF
    }
    fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data].concat()).to_be_bytes());
        out
    }
    let mut ihdr = size.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &z));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    std::fs::write(path, out).unwrap();
}
