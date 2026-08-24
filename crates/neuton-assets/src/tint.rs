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

/// A decoded colormap, kept so any climate can be looked up in it.
struct ColorMap {
    rgb: Vec<u8>,
    width: u32,
    height: u32,
}

/// Colormaps loaded from the pack stack.
#[derive(Default)]
pub struct Tints {
    grass_map: Option<ColorMap>,
    foliage_map: Option<ColorMap>,
    dry_foliage_map: Option<ColorMap>,
}

impl Tints {
    /// Loads the colormaps out of the pack stack.
    pub fn load(packs: &mut PackStack) -> Self {
        let load = |packs: &mut PackStack, path: &str| decode(&packs.read(path)?);
        Self {
            grass_map: load(packs, "assets/minecraft/textures/colormap/grass.png"),
            foliage_map: load(packs, "assets/minecraft/textures/colormap/foliage.png"),
            dry_foliage_map: load(packs, "assets/minecraft/textures/colormap/dry_foliage.png"),
        }
    }

    /// The colour a source takes in a given climate.
    ///
    /// Temperature runs along one axis of the colormap and rainfall scaled by
    /// temperature along the other, both inverted, which is why a hot dry biome
    /// lands in the corner vanilla fills with desert tan.
    pub fn sample(&self, source: TintSource, temperature: f32, downfall: f32) -> Rgb {
        let map = match source {
            TintSource::Grass => &self.grass_map,
            TintSource::Foliage => &self.foliage_map,
            TintSource::DryFoliage => &self.dry_foliage_map,
            TintSource::Water => return rgb(0x3F76E4),
            TintSource::Fixed(hex) => return rgb(hex),
            TintSource::None => return NO_TINT,
        };
        let fallback = match source {
            TintSource::Grass => 0x91BD59,
            TintSource::Foliage => 0x77AB2F,
            _ => 0xAEA42A,
        };
        let Some(map) = map else { return rgb(fallback) };

        let temperature = temperature.clamp(0.0, 1.0);
        let adjusted = downfall.clamp(0.0, 1.0) * temperature;
        let x = ((1.0 - temperature) * 255.0) as u32;
        let y = ((1.0 - adjusted) * 255.0) as u32;
        map.at(x, y).unwrap_or_else(|| rgb(fallback))
    }

    /// The tint for a temperate biome, for callers with no biome data.
    pub fn get(&self, source: TintSource) -> Rgb {
        self.sample(source, 0.8, 0.4)
    }
}

impl ColorMap {
    fn at(&self, x: u32, y: u32) -> Option<Rgb> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 3) as usize;
        Some([
            self.rgb[i] as f32 / 255.0,
            self.rgb[i + 1] as f32 / 255.0,
            self.rgb[i + 2] as f32 / 255.0,
        ])
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

/// Decodes a colormap to plain RGB.
fn decode(bytes: &[u8]) -> Option<ColorMap> {
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
    let pixels = (info.width * info.height) as usize;
    let mut rgb = Vec::with_capacity(pixels * 3);
    for p in buf[..pixels * channels].chunks_exact(channels) {
        rgb.extend_from_slice(&p[..3]);
    }
    Some(ColorMap { rgb, width: info.width, height: info.height })
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
    fn climate_changes_the_colour() {
        let Some(jar) = crate::vanilla_jar("26.2") else { return };
        let mut packs = PackStack::new();
        packs.push(jar).unwrap();
        let t = Tints::load(&mut packs);

        // Plains against desert: warm and dry should be visibly drier.
        let plains = t.sample(TintSource::Grass, 0.8, 0.4);
        let desert = t.sample(TintSource::Grass, 2.0, 0.0);
        let snowy = t.sample(TintSource::Grass, 0.0, 0.5);
        assert_ne!(plains, desert, "climate should change grass colour");
        assert_ne!(plains, snowy);
        // Hotter and drier means less green and more yellow.
        assert!(desert[0] > plains[0], "desert grass should be warmer: {desert:?}");
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
