//! Renders item icons to PNGs, for looking at them.
//!
//! Usage: cargo run -p neuton-assets --example icons -- <out-dir> <item>...

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("out dir");
    let names: Vec<String> = args.collect();
    std::fs::create_dir_all(&out).unwrap();

    let mut packs = neuton_assets::PackStack::new();
    packs.push(neuton_assets::vanilla_jar("26.2").expect("vanilla jar")).unwrap();
    let mut icons = neuton_assets::Icons::new();

    let sheet = neuton_assets::ICON_SIZE;
    for name in &names {
        let block = neuton_blocks::items::ITEMS
            .iter()
            .find(|i| i.name == name)
            .and_then(|i| i.block_state.map(|_| name.as_str()));
        match icons.render(&mut packs, name, block) {
            Some(icon) => {
                let path = format!("{out}/{name}.png");
                let file = std::fs::File::create(&path).unwrap();
                let mut enc = png::Encoder::new(std::io::BufWriter::new(file), sheet, sheet);
                enc.set_color(png::ColorType::Rgba);
                enc.write_header().unwrap().write_image_data(&icon.pixels).unwrap();
                println!("{name}: {path}");
            }
            None => println!("{name}: no icon"),
        }
    }
}
