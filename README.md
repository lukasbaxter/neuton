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
  neuton-protocol   wire types, framing, compression, encryption, packet IDs
  neuton-nbt        NBT, with an allocation-free skipper for the chunk path
  neuton-blocks     block + block state tables
  neuton-auth       Microsoft / Xbox / Minecraft session auth
  neuton-world      chunk and paletted-container decoding
  neuton-net        login -> configuration -> play state machine
  neuton-cli        ping, login, join
```

## Status

Working, with 57 tests:

- **Wire layer** — VarInt/VarLong and the full type set with zero-copy reads,
  frame compression, AES-128-CFB8 encryption verified against the NIST SP 800-38A
  vector, and a server hash verified against Mojang's published examples
- **Auth** — Microsoft device-code flow through Xbox Live and XSTS to a Minecraft
  session, cached so warm launches touch the network zero times
- **NBT** — network framing, modified UTF-8, and a skipper that steps over tags
  without allocating
- **Chunks** — paletted containers in all three forms, decoded against the
  dimension shape read from registry data
- **Join** — full login, configuration and play sequence, covered end to end by an
  integration test that scripts the server side

```
$ neuton ping play.notmiji.com
  connect     21.6 ms
  status      29.3 ms
  ping        10.7 ms
  protocol 776

$ neuton join play.notmiji.com
auth     cached as <name> (0 ms)
join     encrypted=true compression=256
world    entity 419, 24 sections from y=-64
chunk    #1 at (12, -30)  8214 non-air, 9 sections used
```

Signing in needs an Azure application ID; see [docs/AUTH.md](docs/AUTH.md).

Next: the renderer. Block model baking and atlas stitching move to build time,
then chunk meshing and the first frame.

## Requirements

- Rust 1.90+
- A vanilla 26.2 install (for `neuton-datagen` and game assets)
- JDK 25+, only to regenerate tables
