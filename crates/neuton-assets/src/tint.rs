//! Biome tinting.
//!
//! Grass and leaves ship as greyscale textures and are coloured at render time,
//! which is why a world drawn without tinting looks like it is under snow. The
//! colour comes from `colormap/grass.png` and `colormap/foliage.png`, indexed by
//! the biome's temperature and rainfall.
//!
//! A few blocks ignore the colormap and use a fixed colour instead: birch and
//! spruce leaves are the same shade in every biome.

use crate::PackStack;

/// A colour to multiply a face by. White means no tint.
pub type Rgb = [f32; 3];

pub const NO_TINT: Rgb = [1.0, 1.0, 1.0];

/// Which colour source a block draws its tint from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TintSource {
    None,
    Grass,
    Foliage,
    DryFoliage,
    Water,
    /// A colour that does not vary by biome.
    Fixed(u32),
}

/// Colormaps loaded from the pack stack, and the tints sampled out of them.
pub struct Tints {
    grass: Rgb,
    foliage: Rgb,
    dry_foliage: Rgb,
    water: Rgb,
}

impl Default for Tints {
    fn default() -> Self {
        // Vanilla's own fallbacks, used when a pack ships no colormap.
        Self {
            grass: rgb(0x91BD59),
            foliage: rgb(0x77AB2F),
            dry_foliage: rgb(0xAEA42A),
            water: rgb(0x3F76E4),
        }
    }
}

impl Tints {
    /// Samples the colormaps for a temperate biome.
    ///
    /// One tint for the whole world for now. Per-biome tinting needs the biome
    /// registry resolved and the biome palette read per section, which is a
    /// larger job; sampling at plains conditions is what most of a normal world
    /// looks like anyway.
    pub fn load(packs: &mut PackStack) -> Self {
        let mut out = Self::default();
        // Plains: temperature 0.8, downfall 0.4.
        if let Some(c) = sample(packs, "assets/minecraft/textures/colormap/grass.png", 0.8, 0.4) {
            out.grass = c;
        }
        if let Some(c) = sample(packs, "assets/minecraft/textures/colormap/foliage.png", 0.8, 0.4) {
            out.foliage = c;
        }
        if let Some(c) =
            sample(packs, "assets/minecraft/textures/colormap/dry_foliage.png", 0.8, 0.4)
        {
            out.dry_foliage = c;
        }
        out
    }

    pub fn get(&self, source: TintSource) -> Rgb {
        match source {
            TintSource::None => NO_TINT,
            TintSource::Grass => self.grass,
            TintSource::Foliage => self.foliage,
            TintSource::DryFoliage => self.dry_foliage,
            TintSource::Water => self.water,
            TintSource::Fixed(hex) => rgb(hex),
        }
    }
}

/// Which colormap a block uses, by name.
///
/// Vanilla decides this in code rather than in data, so there is no file to read
/// it from and the list has to be written out.
pub fn source_for(block: &str) -> TintSource {
    let name = block.trim_start_matches("minecraft:");
    match name {
        // Fixed colours, the same in every biome.
        "birch_leaves" => return TintSource::Fixed(0x80A755),
        "spruce_leaves" => return TintSource::Fixed(0x619961),
        "lily_pad" => return TintSource::Fixed(0x208030),
        "water" | "water_cauldron" | "bubble_column" => return TintSource::Water,
        _ => {}
    }
    if name.ends_with("_leaves") || name.ends_with("_vine") || name == "vine" {
        return TintSource::Foliage;
    }
    const GRASS: &[&str] = &[
        "grass_block", "short_grass", "tall_grass", "fern", "large_fern", "sugar_cane",
        "potted_fern", "grass", "seagrass", "tall_seagrass",
    ];
    if GRASS.contains(&name) {
        return TintSource::Grass;
    }
    TintSource::None
}

/// Reads one pixel out of a colormap.
///
/// The maps are 256x256 indexed by temperature along x and by rainfall scaled
/// by temperature along y, both inverted. The bottom-right half is unused, so a
/// hot dry biome lands on a black pixel that vanilla never asks for.
fn sample(packs: &mut PackStack, path: &str, temperature: f32, downfall: f32) -> Option<Rgb> {
    let bytes = packs.read(path)?;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;

    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return None,
    };

    let temperature = temperature.clamp(0.0, 1.0);
    let adjusted = (downfall.clamp(0.0, 1.0)) * temperature;
    let x = ((1.0 - temperature) * 255.0) as u32;
    let y = ((1.0 - adjusted) * 255.0) as u32;
    if x >= info.width || y >= info.height {
        return None;
    }
    let i = ((y * info.width + x) * channels) as usize;
    Some([
        buf[i] as f32 / 255.0,
        buf[i + 1] as f32 / 255.0,
        buf[i + 2] as f32 / 255.0,
    ])
}

fn rgb(hex: u32) -> Rgb {
    [
        ((hex >> 16) & 0xFF) as f32 / 255.0,
        ((hex >> 8) & 0xFF) as f32 / 255.0,
        (hex & 0xFF) as f32 / 255.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_and_grass_pick_the_right_colormap() {
        assert_eq!(source_for("minecraft:oak_leaves"), TintSource::Foliage);
        assert_eq!(source_for("minecraft:jungle_leaves"), TintSource::Foliage);
        assert_eq!(source_for("minecraft:grass_block"), TintSource::Grass);
        assert_eq!(source_for("minecraft:tall_grass"), TintSource::Grass);
        assert_eq!(source_for("minecraft:stone"), TintSource::None);
        assert_eq!(source_for("minecraft:oak_planks"), TintSource::None);
    }

    #[test]
    fn birch_and_spruce_ignore_the_biome() {
        // Both are a fixed shade everywhere, unlike every other leaf.
        assert_eq!(source_for("minecraft:birch_leaves"), TintSource::Fixed(0x80A755));
        assert_eq!(source_for("minecraft:spruce_leaves"), TintSource::Fixed(0x619961));
        assert_ne!(source_for("minecraft:birch_leaves"), source_for("minecraft:oak_leaves"));
    }

    #[test]
    fn the_fallbacks_are_plausible_greens() {
        let t = Tints::default();
        let grass = t.get(TintSource::Grass);
        // Green channel highest, and nothing near white, or tinting would be a
        // no-op and the bug would be invisible.
        assert!(grass[1] > grass[0] && grass[1] > grass[2], "{grass:?}");
        assert!(grass[0] < 0.9, "{grass:?}");
        assert_eq!(t.get(TintSource::None), NO_TINT);
    }

    #[test]
    fn colormaps_load_from_a_real_installation() {
        let Some(jar) = crate::vanilla_jar("26.2") else {
            eprintln!("skipped: no vanilla 26.2 installation");
            return;
        };
        let mut packs = PackStack::new();
        packs.push(jar).unwrap();
        let t = Tints::load(&mut packs);

        // Sampled, not the fallback, and still green.
        let grass = t.get(TintSource::Grass);
        assert!(grass[1] > grass[0] && grass[1] > grass[2], "grass not green: {grass:?}");
        let foliage = t.get(TintSource::Foliage);
        assert!(foliage[1] > foliage[0], "foliage not green: {foliage:?}");
        assert_ne!(grass, foliage, "grass and foliage should differ");
    }

    #[test]
    fn hex_conversion_is_right_way_round() {
        assert_eq!(rgb(0xFF0000), [1.0, 0.0, 0.0]);
        assert_eq!(rgb(0x00FF00), [0.0, 1.0, 0.0]);
        assert_eq!(rgb(0x0000FF), [0.0, 0.0, 1.0]);
    }
}
