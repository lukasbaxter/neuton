//! How long a block takes to break, and how far along a swing is.
//!
//! The server keeps its own clock and breaks the block when that clock says so,
//! so nothing here decides anything. It exists to draw the cracks: an estimate
//! that is close is the difference between a block that visibly gives way and
//! one that just vanishes.

use neuton_blocks::StateId;
use neuton_blocks::breaking::{Tool, breaking};

/// How much faster a tool of each material works. The game's own numbers.
fn tool_speed(item: &str) -> f32 {
    match item.split('_').next().unwrap_or_default() {
        "wooden" => 2.0,
        "stone" => 4.0,
        "iron" => 6.0,
        "diamond" => 8.0,
        "netherite" => 9.0,
        "golden" => 12.0,
        _ => 1.0,
    }
}

/// Which kind of tool an item is, if it is one.
fn tool_kind(item: &str) -> Tool {
    match item.rsplit('_').next().unwrap_or_default() {
        "pickaxe" => Tool::Pickaxe,
        "axe" => Tool::Axe,
        "shovel" => Tool::Shovel,
        "hoe" => Tool::Hoe,
        _ => Tool::None,
    }
}

/// Seconds to break `state` holding `item`, or `None` if it cannot be broken.
///
/// `item` is a registry name without its namespace, such as `diamond_pickaxe`;
/// an empty string is a bare hand.
///
/// This does not model tool tiers being too low to harvest a block, which the
/// game expresses as a set of tags. A stone pickaxe on diamond ore therefore
/// looks quicker here than it is. Since the server decides when the block
/// actually gives, the cost is a crack pattern that runs slightly ahead.
pub fn seconds_to_break(state: StateId, item: &str, on_ground: bool) -> Option<f32> {
    let block = breaking(state.0);
    if block.hardness < 0.0 {
        return None;
    }
    if block.hardness == 0.0 {
        return Some(0.0);
    }

    let matches_tool = block.tool != Tool::None && tool_kind(item) == block.tool;
    let mut speed = if matches_tool { tool_speed(item) } else { 1.0 };
    // Swinging in mid-air is five times slower, which is as true of a player
    // falling past a block as of one flying.
    if !on_ground {
        speed /= 5.0;
    }

    // Thirty ticks' worth of progress with the right tool, a hundred without:
    // the penalty for the wrong tool is not being slower, it is being more than
    // three times slower.
    let divisor = if !block.needs_tool || matches_tool { 30.0 } else { 100.0 };
    let per_tick = speed / block.hardness / divisor;
    if per_tick <= 0.0 {
        return None;
    }
    Some((1.0 / per_tick).ceil() * crate::physics::TICK as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use neuton_blocks::by_name;

    fn state(name: &str) -> StateId {
        neuton_blocks::BLOCKS[by_name(&format!("minecraft:{name}")).expect(name).0].default_state
    }

    #[test]
    fn bedrock_never_gives() {
        assert_eq!(seconds_to_break(state("bedrock"), "diamond_pickaxe", true), None);
    }

    #[test]
    fn a_flower_comes_away_instantly() {
        assert_eq!(seconds_to_break(state("dandelion"), "", true), Some(0.0));
    }

    #[test]
    fn stone_takes_the_times_the_game_takes() {
        // The wiki figures: 7.5 seconds bare handed, 1.15 with a wooden
        // pickaxe, 0.4 with an iron one.
        let bare = seconds_to_break(state("stone"), "", true).unwrap();
        let wooden = seconds_to_break(state("stone"), "wooden_pickaxe", true).unwrap();
        let iron = seconds_to_break(state("stone"), "iron_pickaxe", true).unwrap();
        assert!((bare - 7.5).abs() < 0.1, "bare handed took {bare}");
        assert!((wooden - 1.15).abs() < 0.1, "a wooden pickaxe took {wooden}");
        assert!((iron - 0.4).abs() < 0.1, "an iron pickaxe took {iron}");
    }

    #[test]
    fn the_wrong_tool_is_no_better_than_a_hand() {
        let hand = seconds_to_break(state("stone"), "", true);
        let axe = seconds_to_break(state("stone"), "diamond_axe", true);
        assert_eq!(hand, axe);
    }

    #[test]
    fn dirt_does_not_need_a_tool_to_break_quickly() {
        // No tool is required, so a bare hand still gets the fast rate.
        let hand = seconds_to_break(state("dirt"), "", true).unwrap();
        assert!((hand - 0.75).abs() < 0.05, "dirt took {hand} bare handed");
    }

    #[test]
    fn swinging_in_the_air_is_five_times_slower() {
        let grounded = seconds_to_break(state("stone"), "iron_pickaxe", true).unwrap();
        let airborne = seconds_to_break(state("stone"), "iron_pickaxe", false).unwrap();
        assert!(
            (airborne / grounded - 5.0).abs() < 0.3,
            "{airborne} against {grounded}"
        );
    }
}
