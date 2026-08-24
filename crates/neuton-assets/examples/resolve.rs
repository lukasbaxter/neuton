//! Resolves every block's textures against the installed vanilla jar.
fn main() {
    let jar = neuton_assets::vanilla_jar("26.2").expect("no vanilla 26.2 jar");
    let mut packs = neuton_assets::PackStack::new();
    packs.push(&jar).unwrap();
    println!("packs: {:?}", packs.names());

    let mut r = neuton_assets::ModelResolver::new();
    let mut ok = 0usize;
    let mut failed: Vec<&str> = Vec::new();
    let mut textures = std::collections::BTreeSet::new();
    let t = std::time::Instant::now();

    for i in 0..neuton_blocks::BLOCK_COUNT {
        let id = neuton_blocks::BlockId(i);
        let state = id.get().default_state;
        match r.textures(&mut packs, id.name(), state.variant_key()) {
            Some(ft) => {
                ok += 1;
                for t in ft.distinct() { textures.insert(t.to_string()); }
            }
            None => failed.push(id.name()),
        }
    }
    println!("resolved {ok}/{} blocks in {:.0} ms", neuton_blocks::BLOCK_COUNT, t.elapsed().as_secs_f64()*1000.0);
    println!("distinct textures: {}", textures.len());
    if !failed.is_empty() {
        println!("unresolved ({}): {:?}", failed.len(), &failed[..failed.len().min(12)]);
    }
    // Spot-check a few well-known blocks and confirm the files actually exist.
    for (name, variant) in [("minecraft:stone",""),("minecraft:oak_log","axis=y"),("minecraft:grass_block","snowy=false"),("minecraft:water","level=0")] {
        if let Some(ft) = r.textures(&mut packs, name, variant) {
            let present = ft.distinct().iter().all(|p| packs.read(p).is_some());
            println!("  {name:<26} {:?} files_exist={present}", ft.distinct());
        }
    }
}
