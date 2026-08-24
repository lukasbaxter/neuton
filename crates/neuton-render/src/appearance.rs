//! What a block looks like, before textures exist.
//!
//! A stand-in. The real answer comes from baking the block models and stitching
//! the atlas, which is a large piece of work; this exists so the geometry can
//! be seen and judged first. Every colour here is a guess from the block's
//! name, and the whole file is meant to be deleted.

use crate::mesh::BlockAppearance;
use neuton_blocks::{BLOCK_COUNT, STATE_COUNT, StateId};

/// Colour and opacity for every block state, resolved once at startup.
///
/// A table rather than a lookup per face: the mesher asks about a block for
/// every one of its six faces, and matching strings there would dominate the
/// meshing cost.
pub struct Appearance {
    /// Indexed by state id.
    color: Vec<[f32; 3]>,
    opaque: Vec<bool>,
    /// Blocks that hide the faces between two of themselves.
    self_culling: Vec<bool>,
}

impl Default for Appearance {
    fn default() -> Self {
        Self::new()
    }
}

impl Appearance {
    pub fn new() -> Self {
        let mut color = vec![[0.5, 0.5, 0.5]; STATE_COUNT];
        let mut opaque = vec![true; STATE_COUNT];
        let mut self_culling = vec![false; STATE_COUNT];

        for i in 0..BLOCK_COUNT {
            let block = neuton_blocks::BlockId(i);
            let name = block.name().trim_start_matches("minecraft:");
            let rgb = colour_for(name);
            let solid = is_solid(name);
            let culls = hides_its_own_faces(name);
            let b = block.get();
            for s in b.first_state.0..b.first_state.0 + b.state_count {
                color[s as usize] = rgb;
                opaque[s as usize] = solid;
                self_culling[s as usize] = culls;
            }
        }
        Self { color, opaque, self_culling }
    }
}

impl BlockAppearance for Appearance {
    #[inline]
    fn is_opaque(&self, state: StateId) -> bool {
        self.opaque.get(state.0 as usize).copied().unwrap_or(false)
    }

    #[inline]
    fn self_culls(&self, state: StateId) -> bool {
        self.self_culling.get(state.0 as usize).copied().unwrap_or(false)
    }

    #[inline]
    fn color(&self, state: StateId) -> [f32; 3] {
        self.color.get(state.0 as usize).copied().unwrap_or([1.0, 0.0, 1.0])
    }
}

/// Blocks that do not fill their cell, so faces behind them stay visible.
///
/// Deliberately over-inclusive: drawing a face that turns out to be hidden
/// costs a few triangles, while hiding one that should be visible leaves a hole
/// in the world.
fn is_solid(name: &str) -> bool {
    const SEE_THROUGH: &[&str] = &[
        "air", "glass", "water", "lava", "leaves", "ice", "slab", "stairs", "fence", "wall",
        "door", "trapdoor", "pane", "bars", "chain", "torch", "lantern", "sign", "banner",
        "carpet", "rail", "ladder", "vine", "flower", "sapling", "grass", "fern", "mushroom",
        "button", "pressure_plate", "lever", "redstone", "repeater", "comparator", "hopper",
        "cauldron", "campfire", "candle", "bed", "chest", "anvil", "brewing", "enchanting",
        "conduit", "bell", "scaffolding", "amethyst", "pointed", "sculk_vein", "lightning_rod",
        "head", "skull", "pot", "sniffer_egg", "turtle_egg", "cake", "cobweb", "snow",
        "seagrass", "kelp", "coral", "lily", "bamboo", "sugar_cane", "wheat", "carrots",
        "potatoes", "beetroots", "nether_wart", "cocoa", "berry", "vines", "roots", "sprouts",
        "fungus", "dripleaf", "moss_carpet", "azalea", "spore_blossom", "glow_lichen", "tripwire",
    ];
    // "grass_block" and "tall_grass" both contain "grass" but only the first is
    // a full cube, so exact names win over the substring list.
    const SOLID_EXCEPTIONS: &[&str] = &[
        "grass_block", "snow_block", "packed_ice", "blue_ice", "glowstone", "sea_lantern",
        "redstone_block", "redstone_lamp", "coral_block", "chiseled_bookshelf", "bookshelf",
    ];
    if SOLID_EXCEPTIONS.iter().any(|s| name == *s) {
        return true;
    }
    !SEE_THROUGH.iter().any(|s| name.contains(s))
}

/// Blocks whose internal faces are never visible.
///
/// Overwhelmingly this is water. An ocean is millions of blocks and drawing the
/// faces between them means drawing its whole volume rather than its surface.
fn hides_its_own_faces(name: &str) -> bool {
    const SELF_CULLING: &[&str] = &[
        "water", "lava", "glass", "ice", "bubble_column", "powder_snow",
    ];
    SELF_CULLING.iter().any(|s| name.contains(s))
}

/// A plausible colour for a block, from its name.
fn colour_for(name: &str) -> [f32; 3] {
    const RULES: &[(&str, [f32; 3])] = &[
        ("grass_block", [0.42, 0.62, 0.30]),
        ("podzol", [0.35, 0.24, 0.11]),
        ("mycelium", [0.44, 0.36, 0.38]),
        ("dirt_path", [0.60, 0.49, 0.27]),
        ("coarse_dirt", [0.47, 0.33, 0.21]),
        ("dirt", [0.53, 0.37, 0.24]),
        ("deepslate", [0.31, 0.31, 0.33]),
        ("cobblestone", [0.49, 0.49, 0.49]),
        ("stone_brick", [0.48, 0.48, 0.48]),
        ("stone", [0.50, 0.50, 0.50]),
        ("andesite", [0.53, 0.53, 0.52]),
        ("diorite", [0.78, 0.78, 0.79]),
        ("granite", [0.60, 0.42, 0.34]),
        ("gravel", [0.51, 0.49, 0.48]),
        ("sandstone", [0.85, 0.81, 0.62]),
        ("red_sand", [0.75, 0.40, 0.16]),
        ("sand", [0.87, 0.83, 0.63]),
        ("clay", [0.63, 0.65, 0.70]),
        ("terracotta", [0.60, 0.37, 0.27]),
        ("netherrack", [0.44, 0.20, 0.20]),
        ("basalt", [0.28, 0.28, 0.30]),
        ("blackstone", [0.17, 0.15, 0.18]),
        ("obsidian", [0.09, 0.06, 0.14]),
        ("bedrock", [0.33, 0.33, 0.33]),
        ("end_stone", [0.87, 0.87, 0.66]),
        ("water", [0.25, 0.42, 0.88]),
        ("lava", [0.94, 0.42, 0.10]),
        ("ice", [0.68, 0.81, 0.96]),
        ("snow", [0.95, 0.96, 0.98]),
        ("leaves", [0.27, 0.52, 0.20]),
        ("log", [0.42, 0.33, 0.20]),
        ("wood", [0.42, 0.33, 0.20]),
        ("planks", [0.65, 0.52, 0.33]),
        ("coal", [0.16, 0.16, 0.16]),
        ("iron", [0.79, 0.71, 0.64]),
        ("copper", [0.72, 0.45, 0.31]),
        ("gold", [0.94, 0.78, 0.27]),
        ("diamond", [0.38, 0.85, 0.84]),
        ("emerald", [0.20, 0.76, 0.35]),
        ("lapis", [0.18, 0.32, 0.66]),
        ("redstone", [0.68, 0.13, 0.11]),
        ("quartz", [0.93, 0.91, 0.86]),
        ("amethyst", [0.60, 0.44, 0.80]),
        ("prismarine", [0.38, 0.62, 0.57]),
        ("concrete", [0.45, 0.45, 0.50]),
        ("wool", [0.90, 0.90, 0.90]),
        ("glass", [0.75, 0.85, 0.90]),
        ("beacon", [0.55, 0.85, 0.82]),
        ("nether_brick", [0.18, 0.09, 0.11]),
        ("purpur", [0.66, 0.45, 0.66]),
        ("moss", [0.34, 0.47, 0.20]),
        ("mud", [0.24, 0.20, 0.20]),
        ("sculk", [0.05, 0.10, 0.13]),
        ("brick", [0.58, 0.35, 0.30]),
    ];

    // Colour words override the material, so black_concrete is black rather
    // than concrete grey.
    const TINTS: &[(&str, [f32; 3])] = &[
        ("white", [0.93, 0.94, 0.94]),
        ("light_gray", [0.60, 0.60, 0.57]),
        ("gray", [0.34, 0.36, 0.38]),
        ("black", [0.09, 0.10, 0.11]),
        ("brown", [0.45, 0.29, 0.17]),
        ("red", [0.62, 0.18, 0.16]),
        ("orange", [0.85, 0.45, 0.09]),
        ("yellow", [0.90, 0.75, 0.13]),
        ("lime", [0.45, 0.75, 0.16]),
        ("green", [0.32, 0.42, 0.15]),
        ("cyan", [0.13, 0.45, 0.51]),
        ("light_blue", [0.36, 0.60, 0.83]),
        ("blue", [0.17, 0.22, 0.62]),
        ("purple", [0.45, 0.19, 0.62]),
        ("magenta", [0.66, 0.24, 0.70]),
        ("pink", [0.87, 0.51, 0.62]),
    ];

    if let Some((_, rgb)) = TINTS.iter().find(|(word, _)| name.starts_with(word)) {
        return *rgb;
    }
    if let Some((_, rgb)) = RULES.iter().find(|(word, _)| name.contains(word)) {
        return *rgb;
    }
    [0.55, 0.55, 0.55]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::BlockAppearance;

    #[test]
    fn every_state_has_an_entry() {
        let a = Appearance::new();
        assert_eq!(a.color.len(), STATE_COUNT);
        assert_eq!(a.opaque.len(), STATE_COUNT);
        // Out of range must not panic during meshing.
        assert!(!a.is_opaque(StateId(STATE_COUNT as u32)));
    }

    #[test]
    fn air_is_never_opaque() {
        let a = Appearance::new();
        assert!(!a.is_opaque(StateId(0)));
        for name in ["minecraft:air", "minecraft:cave_air", "minecraft:void_air"] {
            let id = neuton_blocks::by_name(name).unwrap();
            assert!(!a.is_opaque(id.get().default_state), "{name}");
        }
    }

    #[test]
    fn full_cubes_are_opaque_and_partial_blocks_are_not() {
        let a = Appearance::new();
        let opaque = |n: &str| {
            a.is_opaque(neuton_blocks::by_name(n).unwrap().get().default_state)
        };
        assert!(opaque("minecraft:stone"));
        assert!(opaque("minecraft:dirt"));
        // The exception list keeps these solid despite matching a substring.
        assert!(opaque("minecraft:grass_block"));
        assert!(opaque("minecraft:snow_block"));

        assert!(!opaque("minecraft:glass"));
        assert!(!opaque("minecraft:oak_slab"));
        assert!(!opaque("minecraft:oak_stairs"));
        assert!(!opaque("minecraft:water"));
        assert!(!opaque("minecraft:tall_grass"));
    }

    #[test]
    fn fluids_and_glass_hide_their_own_internal_faces() {
        assert!(hides_its_own_faces("water"));
        assert!(hides_its_own_faces("lava"));
        assert!(hides_its_own_faces("glass"));
        assert!(!hides_its_own_faces("stone"));
        assert!(!hides_its_own_faces("oak_leaves"));

        let a = Appearance::new();
        let water = neuton_blocks::by_name("minecraft:water").unwrap().get().default_state;
        let stone = neuton_blocks::by_name("minecraft:stone").unwrap().get().default_state;
        // Water against water is hidden; water against air is not.
        assert!(a.hides_face(water, water));
        assert!(!a.hides_face(water, StateId(0)));
        // And a solid neighbour still hides anything.
        assert!(a.hides_face(water, stone));
    }

    #[test]
    fn colours_follow_the_block_rather_than_a_default() {
        assert_eq!(colour_for("grass_block"), [0.42, 0.62, 0.30]);
        assert_eq!(colour_for("water"), [0.25, 0.42, 0.88]);
        // A colour word wins over the material it is attached to.
        assert_eq!(colour_for("black_concrete"), colour_for("black_wool"));
        assert_ne!(colour_for("black_concrete"), colour_for("concrete"));
    }

    #[test]
    fn every_block_resolves_to_something() {
        // No block should fall through to the magenta error colour at runtime.
        let a = Appearance::new();
        for i in 0..BLOCK_COUNT {
            let state = neuton_blocks::BlockId(i).get().default_state;
            assert_ne!(a.color(state), [1.0, 0.0, 1.0], "{}", neuton_blocks::BlockId(i).name());
        }
    }
}
