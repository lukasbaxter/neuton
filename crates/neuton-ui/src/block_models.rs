//! Blocks that are drawn from a model rather than from block geometry.
//!
//! A chest has no block model at all: its shape is a block entity, built out of
//! the same named parts a mob is, and the block itself is invisible. Nothing in
//! the world mesh can stand in for it, so these are found while a chunk is
//! being taken in and drawn with the entities.

use crate::entity_render::Placed;
use neuton_blocks::StateId;
use neuton_render::generated::entity_models::model;
use neuton_world::Chunk;
use neuton_world::palette::Palette;
use std::collections::HashMap;
use std::sync::OnceLock;

/// A chest, and the three textures it wears depending on what it is joined to.
struct Chest {
    block: &'static str,
    single: &'static str,
    left: &'static str,
    right: &'static str,
}

/// Every chest in the game, and there are rather a lot of them now that copper
/// weathers. An ender chest is never double, so its halves are never asked for.
const CHESTS: &[Chest] = &[
    Chest {
        block: "minecraft:chest",
        single: "entity/chest/normal.png",
        left: "entity/chest/normal_left.png",
        right: "entity/chest/normal_right.png",
    },
    Chest {
        block: "minecraft:trapped_chest",
        single: "entity/chest/trapped.png",
        left: "entity/chest/trapped_left.png",
        right: "entity/chest/trapped_right.png",
    },
    Chest {
        block: "minecraft:ender_chest",
        single: "entity/chest/ender.png",
        left: "entity/chest/ender.png",
        right: "entity/chest/ender.png",
    },
    Chest {
        block: "minecraft:copper_chest",
        single: "entity/chest/copper.png",
        left: "entity/chest/copper_left.png",
        right: "entity/chest/copper_right.png",
    },
    Chest {
        block: "minecraft:exposed_copper_chest",
        single: "entity/chest/copper_exposed.png",
        left: "entity/chest/copper_exposed_left.png",
        right: "entity/chest/copper_exposed_right.png",
    },
    Chest {
        block: "minecraft:weathered_copper_chest",
        single: "entity/chest/copper_weathered.png",
        left: "entity/chest/copper_weathered_left.png",
        right: "entity/chest/copper_weathered_right.png",
    },
    Chest {
        block: "minecraft:oxidized_copper_chest",
        single: "entity/chest/copper_oxidized.png",
        left: "entity/chest/copper_oxidized_left.png",
        right: "entity/chest/copper_oxidized_right.png",
    },
    // Waxing changes nothing anyone can see, so a waxed chest wears the
    // texture of the stage it was waxed at.
    Chest {
        block: "minecraft:waxed_copper_chest",
        single: "entity/chest/copper.png",
        left: "entity/chest/copper_left.png",
        right: "entity/chest/copper_right.png",
    },
    Chest {
        block: "minecraft:waxed_exposed_copper_chest",
        single: "entity/chest/copper_exposed.png",
        left: "entity/chest/copper_exposed_left.png",
        right: "entity/chest/copper_exposed_right.png",
    },
    Chest {
        block: "minecraft:waxed_weathered_copper_chest",
        single: "entity/chest/copper_weathered.png",
        left: "entity/chest/copper_weathered_left.png",
        right: "entity/chest/copper_weathered_right.png",
    },
    Chest {
        block: "minecraft:waxed_oxidized_copper_chest",
        single: "entity/chest/copper_oxidized.png",
        left: "entity/chest/copper_oxidized_left.png",
        right: "entity/chest/copper_oxidized_right.png",
    },
];

/// What one state is drawn as: a model, a texture, and how far round it faces.
#[derive(Clone, Copy)]
struct Look {
    model: &'static str,
    texture: &'static str,
    yaw: f32,
}

/// Every state that has to be drawn this way, by state ID.
///
/// Built once. A scan over a section asks this about a handful of palette
/// entries rather than about every block, so the cost of a chunk with no chest
/// in it is a few lookups.
fn drawn_states() -> &'static HashMap<u32, Look> {
    static STATES: OnceLock<HashMap<u32, Look>> = OnceLock::new();
    STATES.get_or_init(|| {
        let mut out = HashMap::new();
        for chest in CHESTS {
            let Some(block) = neuton_blocks::by_name(chest.block) else { continue };
            let block = block.get();
            for offset in 0..block.state_count {
                let state = StateId(block.first_state.0 + offset);
                let variant = state.variant_key();
                let (model, texture) = match property(variant, "type") {
                    Some("left") => ("minecraft:double_chest_left#main", chest.left),
                    Some("right") => ("minecraft:double_chest_right#main", chest.right),
                    _ => ("minecraft:chest#main", chest.single),
                };
                out.insert(
                    state.0,
                    Look { model, texture, yaw: facing_yaw(property(variant, "facing")) },
                );
            }
        }
        out
    })
}

/// One property out of a variant key such as `facing=north,type=single`.
fn property<'a>(variant: &'a str, name: &str) -> Option<&'a str> {
    variant.split(',').find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
}

/// Which way round a facing turns a model, as the game measures it.
fn facing_yaw(facing: Option<&str>) -> f32 {
    match facing {
        Some("west") => 90.0,
        Some("north") => 180.0,
        Some("east") => 270.0,
        // South is zero, and so is anything unexpected.
        _ => 0.0,
    }
}

/// Finds everything in one column that has to be drawn as a model.
///
/// Cheap on the usual column: a section whose palette holds no chest is passed
/// over without looking at a single block, and most sections hold no chest.
pub fn scan(chunk: &Chunk, column: (i32, i32), out: &mut Vec<Placed>) {
    let drawn = drawn_states();
    for (index, section) in chunk.sections.iter().enumerate() {
        if section.is_empty() {
            continue;
        }
        let worth_scanning = match &section.blocks.palette {
            Palette::Single(state) => drawn.contains_key(state),
            Palette::Indirect(states) => states.iter().any(|s| drawn.contains_key(s)),
            // A direct palette says nothing about what is inside it, so the
            // only way to know is to look.
            Palette::Direct => true,
        };
        if !worth_scanning {
            continue;
        }
        let base_y = chunk.min_y + (index as i32) * 16;
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let Some(state) = section.state_at(x, y, z) else { continue };
                    let Some(look) = drawn.get(&state.0) else { continue };
                    let Some(model) = model(look.model) else { continue };
                    out.push(Placed {
                        // The block's own corner. The model is measured from
                        // there, and turned about the middle of the block.
                        at: [
                            (column.0 * 16 + x as i32) as f32,
                            (base_y + y as i32) as f32,
                            (column.1 * 16 + z as i32) as f32,
                        ],
                        yaw: look.yaw,
                        model,
                        texture: look.texture,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chest_knows_which_way_it_faces() {
        let drawn = drawn_states();
        let chest = neuton_blocks::by_name("minecraft:chest").unwrap().get();
        let mut seen = std::collections::HashSet::new();
        for offset in 0..chest.state_count {
            let look = drawn.get(&(chest.first_state.0 + offset)).expect("every chest state");
            seen.insert(look.yaw.to_bits());
        }
        // North, south, east and west, and nothing else.
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn a_double_chest_is_two_halves_with_their_own_textures() {
        let drawn = drawn_states();
        let chest = neuton_blocks::by_name("minecraft:chest").unwrap().get();
        let looks: Vec<Look> = (0..chest.state_count)
            .map(|o| drawn[&(chest.first_state.0 + o)])
            .collect();
        assert!(looks.iter().any(|l| l.model == "minecraft:double_chest_left#main"));
        assert!(looks.iter().any(|l| l.model == "minecraft:double_chest_right#main"));
        assert!(looks.iter().any(|l| l.texture == "entity/chest/normal.png"));
    }

    #[test]
    fn a_property_is_read_out_of_a_variant_key() {
        assert_eq!(property("facing=north,type=left,waterlogged=false", "type"), Some("left"));
        assert_eq!(property("facing=north", "facing"), Some("north"));
        assert_eq!(property("facing=north", "type"), None);
    }
}
