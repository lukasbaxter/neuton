//! World state: chunk decoding and the containers it produces.
//!
//! Server-play only, so there is no generation and no save format here. Chunks
//! arrive over the wire, get decoded, and are handed to the mesher.

pub mod chunk;
pub mod generated;
pub mod palette;
pub mod mining;
pub mod physics;
pub mod raycast;

pub use chunk::{BlockEntity, Chunk, Heightmap, Section, SectionScratch};
pub use generated::{entities, shapes};
pub use physics::{Aabb, Body, BlockShapes, BlockView, Input};
pub use palette::{BIOME_VOLUME, Palette, PalettedContainer, SECTION_VOLUME, block_index, words_for};
