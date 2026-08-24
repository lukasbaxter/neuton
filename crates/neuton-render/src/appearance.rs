//! Whether a block fills its cell, and whether it hides its own internal faces.
//!
//! Still a heuristic on block names. The real answer is in the model's
//! `elements`, which is the same work that will give stairs and slabs their
//! actual shape; until that lands, an over-inclusive guess here costs a few
//! triangles rather than leaving holes in the world.

use crate::mesh::BlockAppearance;
use neuton_blocks::{BLOCK_COUNT, STATE_COUNT, StateId};

/// Colour and opacity for every block state, resolved once at startup.
///
/// A table rather than a lookup per face: the mesher asks about a block for
/// every one of its six faces, and matching strings there would dominate the
/// meshing cost.
pub struct Appearance {
    /// Indexed by state id.
    opaque: Vec<bool>,
    /// Blocks that hide the faces between two of themselves.
    self_culling: Vec<bool>,
    /// How opaque each state draws. 1 for all but water, ice and stained glass.
    translucent: Vec<f32>,
}

impl Default for Appearance {
    fn default() -> Self {
        Self::new()
    }
}

impl Appearance {
    pub fn new() -> Self {
        let mut opaque = vec![true; STATE_COUNT];
        let mut self_culling = vec![false; STATE_COUNT];
        let mut translucent = vec![1.0f32; STATE_COUNT];

        for i in 0..BLOCK_COUNT {
            let block = neuton_blocks::BlockId(i);
            let name = block.name().trim_start_matches("minecraft:");
            let solid = is_solid(name);
            let culls = hides_its_own_faces(name);
            let alpha = opacity_of(name);
            let b = block.get();
            for s in b.first_state.0..b.first_state.0 + b.state_count {
                opaque[s as usize] = solid;
                self_culling[s as usize] = culls;
                translucent[s as usize] = alpha;
            }
        }
        Self { opaque, self_culling, translucent }
    }
}

impl BlockAppearance for Appearance {
    #[inline]
    fn is_opaque(&self, state: StateId) -> bool {
        self.opaque.get(state.0 as usize).copied().unwrap_or(false)
    }

    #[inline]
    fn alpha(&self, state: StateId) -> f32 {
        self.translucent.get(state.0 as usize).copied().unwrap_or(1.0)
    }

    #[inline]
    fn self_culls(&self, state: StateId) -> bool {
        self.self_culling.get(state.0 as usize).copied().unwrap_or(false)
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

/// How opaque a block is, for the blocks that are see-through without being
/// cutout.
///
/// Leaves and grass are cutout: every texel is either drawn or discarded.
/// Water and glass are not, and drawing them with a hard threshold makes an
/// ocean look like a solid blue floor.
fn opacity_of(name: &str) -> f32 {
    let name = name.trim_start_matches("minecraft:");
    if name == "water" || name == "bubble_column" {
        return 0.72;
    }
    if name == "ice" || name == "frosted_ice" {
        return 0.72;
    }
    if name == "tinted_glass" {
        return 0.55;
    }
    if name.ends_with("stained_glass") || name.ends_with("stained_glass_pane") {
        return 0.62;
    }
    if name == "slime_block" || name == "honey_block" {
        return 0.7;
    }
    1.0
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::BlockAppearance;

    #[test]
    fn every_state_has_an_entry() {
        let a = Appearance::new();
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
    fn water_and_glass_are_translucent_and_stone_is_not() {
        let a = Appearance::new();
        let alpha = |n: &str| a.alpha(neuton_blocks::by_name(n).unwrap().get().default_state);
        assert!(alpha("minecraft:water") < 1.0);
        assert!(alpha("minecraft:ice") < 1.0);
        assert!(alpha("minecraft:blue_stained_glass") < 1.0);
        assert_eq!(alpha("minecraft:stone"), 1.0);
        // Plain glass is cutout, not translucent: it is either there or a hole.
        assert_eq!(alpha("minecraft:glass"), 1.0);
        assert_eq!(alpha("minecraft:oak_leaves"), 1.0);
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

}
