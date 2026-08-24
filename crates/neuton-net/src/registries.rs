//! The registry snapshot the server sends during configuration.
//!
//! Most of it we do not need. One part we absolutely do: `dimension_type`
//! carries `min_y` and `height`, and without them the chunk decoder does not
//! know how many sections a column has. Decoding a chunk with the wrong section
//! count does not fail cleanly, it reads the next field as block data, so this
//! is captured before entering play rather than assumed.

use neuton_nbt::Value;

/// The shape of one dimension, as far as chunk decoding cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DimensionShape {
    pub name: String,
    pub min_y: i32,
    pub height: i32,
}

impl DimensionShape {
    /// Overworld dimensions since 1.18.
    pub const OVERWORLD: Self =
        Self { name: String::new(), min_y: -64, height: 384 };

    pub fn section_count(&self) -> usize {
        (self.height / 16).max(0) as usize
    }
}

/// What a biome does to the colour of grass, leaves and water.
///
/// Vanilla derives most of it from temperature and rainfall through the
/// colormaps, but a biome may also override any of the three outright, which is
/// how swamp water is green and badlands grass is orange regardless of climate.
#[derive(Debug, Clone, PartialEq)]
pub struct BiomeColors {
    pub name: String,
    pub temperature: f32,
    pub downfall: f32,
    /// Explicit overrides, as packed RGB.
    pub grass: Option<u32>,
    pub foliage: Option<u32>,
    pub water: Option<u32>,
    /// Vanilla special-cases two biomes in code rather than in data.
    pub grass_modifier: GrassModifier,
}

/// Adjustments vanilla applies after the colormap lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrassModifier {
    None,
    /// Swamps ignore the colormap and pick between two fixed greens.
    Swamp,
    /// Dark forests blend towards brown.
    DarkForest,
}

impl Default for BiomeColors {
    fn default() -> Self {
        Self {
            name: String::new(),
            temperature: 0.8,
            downfall: 0.4,
            grass: None,
            foliage: None,
            water: None,
            grass_modifier: GrassModifier::None,
        }
    }
}

/// Dimension types in registry order, so a `Holder` ID indexes straight in.
#[derive(Debug, Default, Clone)]
pub struct Registries {
    pub dimension_types: Vec<DimensionShape>,
    /// Biomes in registry order, so a chunk's biome palette indexes straight in.
    pub biomes: Vec<BiomeColors>,
}

impl Registries {
    /// Absorbs one `registry_data` packet.
    ///
    /// Entries whose payload is absent fall back to the vanilla overworld
    /// shape: the server omits data only for entries the client is expected to
    /// already know.
    pub fn absorb(&mut self, registry_id: &str, entries: &[(String, Option<Value<'_>>)]) {
        if registry_id == "minecraft:worldgen/biome" {
            self.biomes = entries.iter().map(|(name, data)| read_biome(name, data.as_ref())).collect();
            return;
        }
        if registry_id != "minecraft:dimension_type" {
            return;
        }
        self.dimension_types = entries
            .iter()
            .map(|(name, data)| {
                let min_y = data
                    .as_ref()
                    .and_then(|d| d.get("min_y"))
                    .and_then(|v| v.as_i32())
                    .unwrap_or(DimensionShape::OVERWORLD.min_y);
                let height = data
                    .as_ref()
                    .and_then(|d| d.get("height"))
                    .and_then(|v| v.as_i32())
                    .unwrap_or(DimensionShape::OVERWORLD.height);
                DimensionShape { name: name.clone(), min_y, height }
            })
            .collect();
    }

    /// Looks a dimension up by its registry index.
    pub fn dimension(&self, id: usize) -> Option<&DimensionShape> {
        self.dimension_types.get(id)
    }

    pub fn is_empty(&self) -> bool {
        self.dimension_types.is_empty()
    }

    /// Colours for a biome by its registry index.
    pub fn biome(&self, id: usize) -> Option<&BiomeColors> {
        self.biomes.get(id)
    }
}

fn read_biome(name: &str, data: Option<&Value<'_>>) -> BiomeColors {
    let mut out = BiomeColors { name: name.to_string(), ..Default::default() };
    let Some(data) = data else { return out };

    if let Some(v) = data.get("temperature").and_then(|v| v.as_f64()) {
        out.temperature = v as f32;
    }
    if let Some(v) = data.get("downfall").and_then(|v| v.as_f64()) {
        out.downfall = v as f32;
    }
    let colour = |key: &str| {
        data.path(&["effects", key]).and_then(|v| v.as_i64()).map(|v| v as u32)
    };
    out.grass = colour("grass_color");
    out.foliage = colour("foliage_color");
    out.water = colour("water_color");

    out.grass_modifier = match data
        .path(&["effects", "grass_color_modifier"])
        .and_then(|v| v.as_str())
    {
        Some("swamp") => GrassModifier::Swamp,
        Some("dark_forest") => GrassModifier::DarkForest,
        _ => GrassModifier::None,
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overworld_shape_has_twenty_four_sections() {
        assert_eq!(DimensionShape::OVERWORLD.section_count(), 24);
    }

    #[test]
    fn nether_shape_has_sixteen() {
        let nether = DimensionShape { name: "the_nether".into(), min_y: 0, height: 256 };
        assert_eq!(nether.section_count(), 16);
    }

    #[test]
    fn entries_without_payloads_fall_back_to_overworld() {
        let mut r = Registries::default();
        r.absorb("minecraft:dimension_type", &[("minecraft:overworld".to_string(), None)]);
        let d = r.dimension(0).unwrap();
        assert_eq!((d.min_y, d.height), (-64, 384));
    }

    #[test]
    fn unrelated_registries_are_ignored() {
        let mut r = Registries::default();
        r.absorb("minecraft:biome", &[("minecraft:plains".to_string(), None)]);
        assert!(r.is_empty());
    }
}
