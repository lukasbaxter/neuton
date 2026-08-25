//! Renders the inventory portrait to a PNG, for looking at it.
//!
//! Usage: cargo run -p neuton-ui --example portrait -- <out.png> [dx dy]

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("out path");
    let dx: f32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(0.0);
    let dy: f32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(0.0);

    let mut packs = neuton_assets::PackStack::new();
    packs.push(neuton_assets::vanilla_jar("26.2").expect("vanilla jar")).unwrap();
    let bytes = packs
        .read(&format!("assets/minecraft/textures/{}", neuton_ui::hand::SKIN))
        .expect("skin");
    let image = neuton_ui::icons::decode(&bytes).expect("decode");
    let skin = neuton_ui::portrait::Skin {
        rgba: image.pixels.iter().flat_map(|p| p.to_array()).collect(),
        width: image.size[0] as u32,
        height: image.size[1] as u32,
    };

    let (w, h) = (49 * 8, 70 * 8);
    let look = neuton_ui::portrait::Look::at(dx, dy);
    let rendered = neuton_ui::portrait::render(&skin, look, w, h);
    let rgba: Vec<u8> = rendered.pixels.iter().flat_map(|p| p.to_array()).collect();
    let file = std::fs::File::create(&out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.write_header().unwrap().write_image_data(&rgba).unwrap();
    println!("wrote {out}");
}
