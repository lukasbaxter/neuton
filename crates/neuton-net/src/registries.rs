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

/// Dimension types in registry order, so a `Holder` ID indexes straight in.
#[derive(Debug, Default, Clone)]
pub struct Registries {
    pub dimension_types: Vec<DimensionShape>,
}

impl Registries {
    /// Absorbs one `registry_data` packet.
    ///
    /// Entries whose payload is absent fall back to the vanilla overworld
    /// shape: the server omits data only for entries the client is expected to
    /// already know.
    pub fn absorb(&mut self, registry_id: &str, entries: &[(String, Option<Value<'_>>)]) {
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
