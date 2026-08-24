# neuton

A from-scratch Minecraft: Java Edition client for **26.2 (protocol 776)**, written in Rust.

Multiplayer only. No world generation, no save format, no integrated server — this
exists to join servers and render them faster than anything else does.

Two goals, in order:

1. **Instant boot.** Click to interactive title screen with no perceptible delay.
2. **Best-in-class frame times**, with shader pack support.

## Why this can be fast

Vanilla's startup cost is mostly work that produces the same answer every single
launch: parsing 1,199 blockstate files and 2,658 block models, building
registries, stitching texture atlases, compiling shaders. neuton moves all of it
to build time.

| Work | Vanilla | neuton |
| --- | --- | --- |
| Registries, block state IDs | parsed at runtime | `static` arrays in `.rodata` |
| Packet ID tables | reflection over class hierarchy | `const i32` |
| Block models | JSON parsed + baked at runtime | baked to quads at build time |
| Texture atlas | stitched at runtime | pre-stitched, GPU-compressed |
| Shaders | compiled at runtime | precompiled pipeline cache |

Startup should be: map a few blobs, create pipelines from cache, draw.

## The unlock: 26.x is unobfuscated

The 26.2 jar ships `net.minecraft.data.Main`, its own data generator, and the
class names are in the clear. So every table neuton depends on comes from the
game itself rather than from reverse engineering or a third-party protocol
library that lags each release:

```
cargo run -p neuton-datagen
```

runs the vanilla generator against your installed 26.2 jar and regenerates the
Rust tables. Currently produces 256 packet IDs across 5 protocol states, and
1,196 blocks / 32,366 block states.

Generated files are committed, so a normal build never needs Java.

## Layout

```
crates/
  neuton-datagen    build tool: vanilla jar -> generated Rust tables
  neuton-protocol   wire types, framing, compression, packet IDs
  neuton-blocks     block + block state tables
  neuton-cli        `neuton ping`, `neuton info`
```

## Status

Working:

- VarInt/VarLong and the full wire type set, zero-copy reads
- Packet framing with zlib compression thresholds
- Generated packet ID and block state tables, with invariants under test
- Live server-list ping at protocol 776

```
$ neuton ping play.notmiji.com
play.notmiji.com:25565
  connect     21.6 ms
  status      29.3 ms
  ping        10.7 ms
  protocol 776
```

Next: encryption + Microsoft auth, the configuration/play state machine, chunk
palette decoding, then the renderer.

## Requirements

- Rust 1.90+
- A vanilla 26.2 install (for `neuton-datagen` and game assets)
- JDK 25+, only to regenerate tables
