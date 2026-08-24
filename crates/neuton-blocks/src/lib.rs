//! Block and block-state tables for Minecraft 26.2, generated from the vanilla
//! jar's own data reports.
//!
//! Chunk sections come off the wire as palettes of global state IDs. Resolving
//! one to a block happens millions of times per chunk batch, so it is a single
//! array index here — no hashing, no parsing, no allocation.

mod generated;

pub use generated::blocks::{BLOCK_COUNT, BLOCKS, STATE_COUNT, STATE_TO_BLOCK, block};

/// Index into [`BLOCKS`]. Not the same thing as a state ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub usize);

/// A global block state ID, as it appears in chunk palettes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateId(pub u32);

/// One entry of the block registry.
#[derive(Debug, Clone, Copy)]
pub struct Block {
    /// Namespaced registry name, e.g. `minecraft:oak_stairs`.
    pub name: &'static str,
    /// Lowest state ID belonging to this block.
    pub first_state: StateId,
    /// How many consecutive states this block owns.
    pub state_count: u32,
    /// The state a freshly placed block takes.
    pub default_state: StateId,
}

impl BlockId {
    #[inline]
    pub fn get(self) -> &'static Block {
        &BLOCKS[self.0]
    }

    #[inline]
    pub fn name(self) -> &'static str {
        self.get().name
    }
}

impl StateId {
    /// Which block this state belongs to.
    ///
    /// One bounds-checked array read. Returns `None` only for a state ID the
    /// server should never have sent.
    #[inline]
    pub fn block(self) -> Option<BlockId> {
        STATE_TO_BLOCK.get(self.0 as usize).map(|&i| BlockId(i as usize))
    }

    /// Index of this state within its own block, i.e. which property
    /// combination it is.
    #[inline]
    pub fn variant_index(self) -> Option<u32> {
        Some(self.0 - self.block()?.get().first_state.0)
    }

    /// Air is state 0 and by far the most common state in any chunk, so the
    /// meshing fast path tests it before doing anything else.
    #[inline]
    pub const fn is_air(self) -> bool {
        self.0 == 0
    }
}

/// Looks a block up by registry name. Linear; intended for tooling and config,
/// not for hot paths.
pub fn by_name(name: &str) -> Option<BlockId> {
    BLOCKS.iter().position(|b| b.name == name).map(BlockId)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_table_covers_every_state_exactly_once() {
        assert_eq!(STATE_TO_BLOCK.len(), STATE_COUNT);
        let mut seen = vec![0usize; BLOCK_COUNT];
        for &b in STATE_TO_BLOCK.iter() {
            seen[b as usize] += 1;
        }
        for (i, &count) in seen.iter().enumerate() {
            assert_eq!(count as u32, BLOCKS[i].state_count, "block {} state count", BLOCKS[i].name);
        }
    }

    #[test]
    fn every_block_resolves_from_its_own_states() {
        for (i, b) in BLOCKS.iter().enumerate() {
            for s in b.first_state.0..b.first_state.0 + b.state_count {
                assert_eq!(StateId(s).block(), Some(BlockId(i)), "state {s} of {}", b.name);
            }
            assert_eq!(b.default_state.block(), Some(BlockId(i)));
        }
    }

    #[test]
    fn state_zero_is_air() {
        assert_eq!(StateId(0).block().unwrap().name(), "minecraft:air");
        assert!(StateId(0).is_air());
    }

    #[test]
    fn out_of_range_state_is_rejected_rather_than_panicking() {
        assert_eq!(StateId(STATE_COUNT as u32).block(), None);
        assert_eq!(StateId(u32::MAX).block(), None);
    }

    #[test]
    fn named_constants_agree_with_the_registry() {
        assert_eq!(block::OAK_STAIRS.name(), "minecraft:oak_stairs");
        assert_eq!(by_name("minecraft:oak_stairs"), Some(block::OAK_STAIRS));
        // Stairs: 4 facings x 2 halves x 5 shapes x 2 waterlogged.
        assert_eq!(block::OAK_STAIRS.get().state_count, 80);
    }
}
