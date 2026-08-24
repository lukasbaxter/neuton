//! World state: chunk decoding and the containers it produces.
//!
//! Server-play only, so there is no generation and no save format here. Chunks
//! arrive over the wire, get decoded, and are handed to the mesher.

pub mod chunk;
pub mod palette;

pub use chunk::{BlockEntity, Chunk, Heightmap, Section, SectionScratch};
pub use palette::{BIOME_VOLUME, PalettedContainer, Palette, SECTION_VOLUME, block_index};
