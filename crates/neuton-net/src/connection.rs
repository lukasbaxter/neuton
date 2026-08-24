//! The login -> configuration -> play state machine.

use crate::registries::{DimensionShape, Registries};
use neuton_auth::Session;
use neuton_protocol::crypto::{SharedSecret, encrypt_key_exchange, server_hash};
use neuton_protocol::{Framed, PROTOCOL_VERSION, Reader, State, Writer, ids};
use crate::items::Stack;
use neuton_world::Chunk;
use std::io;
use std::net::TcpStream;
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Generous: a busy server can take a while between chunk batches, and a false
/// timeout mid-join is worse than waiting.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Protocol(neuton_protocol::Error),
    Auth(neuton_auth::Error),
    /// The server closed the connection with a reason.
    Disconnected(String),
    /// The server did something the state machine does not allow here.
    Unexpected { state: &'static str, packet: String },
    /// A packet we recognise but could not decode.
    ///
    /// Names the packet, because "unexpected end of buffer" on its own gives no
    /// clue which of a hundred and forty decoders is wrong.
    Decode { packet: &'static str, id: i32, len: usize, source: neuton_protocol::Error },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "connection: {e}"),
            Error::Protocol(e) => write!(f, "protocol: {e}"),
            Error::Auth(e) => write!(f, "{e}"),
            Error::Disconnected(why) => write!(f, "disconnected: {why}"),
            Error::Unexpected { state, packet } => {
                write!(f, "unexpected {packet} during {state}")
            }
            Error::Decode { packet, id, len, source } => {
                write!(f, "could not decode {packet} (id {id}, {len} bytes): {source}")
            }
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<neuton_protocol::Error> for Error {
    fn from(e: neuton_protocol::Error) -> Self {
        Error::Protocol(e)
    }
}
impl From<neuton_auth::Error> for Error {
    fn from(e: neuton_auth::Error) -> Self {
        Error::Auth(e)
    }
}

type Result<T> = std::result::Result<T, Error>;

/// Which fields of a teleport are offsets rather than destinations.
///
/// A bitmask over the game's `Relative` enum, in its declared order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Relatives(pub u32);

impl Relatives {
    const X: u32 = 1 << 0;
    const Y: u32 = 1 << 1;
    const Z: u32 = 1 << 2;
    const YAW: u32 = 1 << 3;
    const PITCH: u32 = 1 << 4;

    pub fn x(self) -> bool {
        self.0 & Self::X != 0
    }
    pub fn y(self) -> bool {
        self.0 & Self::Y != 0
    }
    pub fn z(self) -> bool {
        self.0 & Self::Z != 0
    }
    pub fn yaw(self) -> bool {
        self.0 & Self::YAW != 0
    }
    pub fn pitch(self) -> bool {
        self.0 & Self::PITCH != 0
    }

    /// Applies one field: an offset from `current`, or a destination.
    pub fn resolve(relative: bool, value: f64, current: f64) -> f64 {
        if relative { current + value } else { value }
    }

    /// True if nothing at all is absolute, which is how a server nudges a
    /// player without moving them.
    pub fn all_relative(self) -> bool {
        self.x() && self.y() && self.z()
    }
}

/// Something the caller might want to act on. Packets we do not model yet are
/// reported as [`Event::Ignored`] rather than dropped, so it is visible what a
/// real server actually sends.
#[derive(Debug)]
pub enum Event {
    /// Configuration finished and the world is starting.
    Joined { entity_id: i32, dimension: DimensionShape },
    Chunk(Box<Chunk>),
    ChunkForgotten { x: i32, z: i32 },
    /// The server moved us. Already acknowledged; ignoring these gets us
    /// kicked.
    ///
    /// Each field is either where to go or how far to move, according to
    /// `relative`. A server correcting a small drift sends a relative teleport
    /// of nearly nothing, and reading that as absolute throws the player across
    /// the world.
    Teleported {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        relative: Relatives,
    },
    /// A chat line, already flattened to styled runs.
    Chat(Vec<crate::Span>),
    /// What the server says the player may do. Sent on join and whenever the
    /// game mode changes.
    Abilities(neuton_world::physics::Abilities),
    /// The whole of an open container, the player's own inventory included.
    /// Slot numbering is the container's: for the inventory screen, 0 is the
    /// crafting output and 36 to 44 are the hotbar.
    /// `unread` names the data component that stopped the read, when one did:
    /// the slots before it are good, the ones after it were not attempted.
    Container {
        window: i32,
        slots: Vec<Option<Stack>>,
        carried: Option<Stack>,
        unread: Option<String>,
    },
    /// One slot of one container changed.
    Slot { window: i32, slot: i32, stack: Option<Stack> },
    /// The server moved the hotbar selection, which it does on join and when a
    /// plugin sets it.
    HeldSlot(i32),
    /// Blocks that changed, in world coordinates. One packet may carry many.
    BlocksChanged(Vec<([i32; 3], u32)>),
    /// The player's health and hunger.
    Health { health: f32, food: i32, saturation: f32 },
    /// Motion the server is imposing on the player, in blocks per tick. This is
    /// knockback: being hit, shoved, or thrown by an explosion.
    Knockback([f64; 3]),
    /// The player died and the server is waiting to be told to respawn them.
    Died,
    Disconnect(String),
    /// Answered automatically; surfaced so latency can be tracked.
    KeepAlive,
    Ignored { id: i32, name: &'static str, len: usize },
}

/// Counters for what the connection has actually done.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub packets: u64,
    pub bytes: u64,
    pub chunks: u64,
    pub blocks: u64,
    /// Wall time from TCP connect to the join packet.
    pub join_ms: f64,
}

pub struct Connection {
    framed: Framed<TcpStream>,
    state: State,
    registries: Registries,
    dimension: DimensionShape,
    pub stats: Stats,
    started: Instant,
    /// The last position and rotation the server was told about. Relative
    /// teleports are offsets from this, and so are echoed back against it.
    reported: ([f64; 3], (f32, f32)),
    /// The player's own entity ID, so packets addressed to it can be told apart
    /// from the ones about everything else in the world.
    entity_id: i32,
    /// How fast this client would like chunks. The server waits to be told.
    batches: crate::batches::BatchRate,
    /// Counts the world-changing actions sent, so the server can tell the
    /// client which of its guesses about the world it is confirming.
    sequence: i32,
}

impl Connection {
    /// Connects, authenticates and runs configuration, returning once the
    /// server has put us into the play state.
    pub fn join(host: &str, port: u16, session: &Session) -> Result<Self> {
        let started = Instant::now();
        let stream = connect(host, port)?;
        let mut conn = Self {
            framed: Framed::new(stream),
            state: State::Handshake,
            registries: Registries::default(),
            dimension: DimensionShape::OVERWORLD,
            stats: Stats::default(),
            started,
            reported: ([0.0; 3], (0.0, 0.0)),
            entity_id: 0,
            batches: crate::batches::BatchRate::new(),
            sequence: 0,
        };

        conn.handshake(host, port)?;
        conn.login(session)?;
        conn.configure()?;
        Ok(conn)
    }

    fn handshake(&mut self, host: &str, port: u16) -> Result<()> {
        let mut w = Writer::new();
        w.write_varint(PROTOCOL_VERSION);
        w.write_str(host);
        w.write_u16(port);
        w.write_varint(2); // next state: login
        self.framed.write_packet(ids::handshake::serverbound::INTENTION, &w)?;
        self.state = State::Login;
        Ok(())
    }

    fn login(&mut self, session: &Session) -> Result<()> {
        let mut w = Writer::new();
        w.write_str(&session.profile.name);
        w.write_uuid(session.profile.uuid);
        self.framed.write_packet(ids::login::serverbound::HELLO, &w)?;

        loop {
            let (id, body) = self.read()?;
            let mut r = Reader::new(&body);
            match id {
                ids::login::clientbound::HELLO => {
                    self.key_exchange(&mut r, session)?;
                }
                ids::login::clientbound::LOGIN_COMPRESSION => {
                    // Everything after this frame is compressed, so the codec
                    // must be switched before the next read.
                    let threshold = r.read_varint()?;
                    self.framed.set_compression(threshold);
                }
                ids::login::clientbound::LOGIN_FINISHED => {
                    self.framed
                        .write_packet(ids::login::serverbound::LOGIN_ACKNOWLEDGED, &Writer::new())?;
                    self.state = State::Configuration;
                    return Ok(());
                }
                ids::login::clientbound::LOGIN_DISCONNECT => {
                    return Err(Error::Disconnected(r.read_str().unwrap_or("no reason given").to_string()));
                }
                ids::login::clientbound::COOKIE_REQUEST => {
                    // We keep no cookies; an empty response is a valid answer.
                    let key = r.read_str().unwrap_or_default();
                    let mut w = Writer::new();
                    w.write_str(key);
                    w.write_bool(false);
                    self.framed.write_packet(ids::login::serverbound::COOKIE_RESPONSE, &w)?;
                }
                ids::login::clientbound::CUSTOM_QUERY => {
                    // Modded-server handshakes such as Forge's. Declining is
                    // correct for a vanilla-protocol client.
                    let tx = r.read_varint()?;
                    let mut w = Writer::new();
                    w.write_varint(tx);
                    w.write_bool(false);
                    self.framed
                        .write_packet(ids::login::serverbound::CUSTOM_QUERY_ANSWER, &w)?;
                }
                other => {
                    return Err(Error::Unexpected {
                        state: "login",
                        packet: describe(ids::login::clientbound::name(other), other),
                    });
                }
            }
        }
    }

    /// Handles `ClientboundHelloPacket`: RSA key exchange plus the session
    /// server round trip.
    fn key_exchange(&mut self, r: &mut Reader<'_>, session: &Session) -> Result<()> {
        let server_id = r.read_str()?;
        let public_key = r.read_byte_array()?;
        let challenge = r.read_byte_array()?;
        // Servers in offline mode set this false and skip the Mojang call.
        let should_authenticate = r.read_bool().unwrap_or(true);

        let secret = SharedSecret::generate();
        let (enc_secret, enc_challenge) = encrypt_key_exchange(public_key, &secret, challenge)?;

        if should_authenticate {
            // Must happen before the key packet reaches the server, or the
            // server's own lookup races ahead of ours and fails.
            let hash = server_hash(server_id, &secret, public_key);
            neuton_auth::join_server(&session.access_token, session.profile.uuid, &hash)?;
        }

        let mut w = Writer::new();
        w.write_byte_array(&enc_secret);
        w.write_byte_array(&enc_challenge);
        self.framed.write_packet(ids::login::serverbound::KEY, &w)?;

        // The server's very next byte is encrypted, so the cipher goes on
        // straight after the write and before any further read.
        self.framed.enable_encryption(&secret);
        Ok(())
    }

    fn configure(&mut self) -> Result<()> {
        self.send_client_information()?;

        loop {
            let (id, body) = self.read()?;
            let mut r = Reader::new(&body);
            match id {
                ids::configuration::clientbound::REGISTRY_DATA => {
                    self.absorb_registry(&mut r)?;
                }
                ids::configuration::clientbound::SELECT_KNOWN_PACKS => {
                    // Claiming no known packs makes the server send the full
                    // registry data, including the dimension heights the chunk
                    // decoder needs.
                    let mut w = Writer::new();
                    w.write_varint(0);
                    self.framed
                        .write_packet(ids::configuration::serverbound::SELECT_KNOWN_PACKS, &w)?;
                }
                ids::configuration::clientbound::KEEP_ALIVE => {
                    let token = r.read_i64()?;
                    let mut w = Writer::new();
                    w.write_i64(token);
                    self.framed.write_packet(ids::configuration::serverbound::KEEP_ALIVE, &w)?;
                }
                ids::configuration::clientbound::PING => {
                    let token = r.read_i32()?;
                    let mut w = Writer::new();
                    w.write_i32(token);
                    self.framed.write_packet(ids::configuration::serverbound::PONG, &w)?;
                }
                ids::configuration::clientbound::CODE_OF_CONDUCT => {
                    // Servers gate entry on this; not answering hangs the join.
                    self.framed.write_packet(
                        ids::configuration::serverbound::ACCEPT_CODE_OF_CONDUCT,
                        &Writer::new(),
                    )?;
                }
                ids::configuration::clientbound::RESOURCE_PACK_PUSH => {
                    self.decline_resource_pack(&mut r)?;
                }
                ids::configuration::clientbound::FINISH_CONFIGURATION => {
                    self.framed.write_packet(
                        ids::configuration::serverbound::FINISH_CONFIGURATION,
                        &Writer::new(),
                    )?;
                    self.state = State::Play;
                    return Ok(());
                }
                ids::configuration::clientbound::DISCONNECT => {
                    return Err(Error::Disconnected(disconnect_reason(&mut r)));
                }
                // Tags, feature flags, custom payloads, dialogs and cookies all
                // arrive here. None affect reaching the world.
                _ => {}
            }
        }
    }

    fn send_client_information(&mut self) -> Result<()> {
        let mut w = Writer::new();
        w.write_str("en_us");
        w.write_u8(8); // view distance in chunks
        w.write_varint(0); // chat visibility: full
        w.write_bool(true); // chat colours
        w.write_u8(0x7F); // all skin layers
        w.write_varint(1); // main hand: right
        w.write_bool(false); // text filtering
        w.write_bool(true); // allow server listings
        w.write_varint(0); // particle status: all
        self.framed.write_packet(ids::configuration::serverbound::CLIENT_INFORMATION, &w)?;
        Ok(())
    }

    fn decline_resource_pack(&mut self, r: &mut Reader<'_>) -> Result<()> {
        let uuid = r.read_uuid()?;
        let mut w = Writer::new();
        w.write_uuid(uuid);
        w.write_varint(2); // result: declined
        self.framed.write_packet(ids::configuration::serverbound::RESOURCE_PACK, &w)?;
        Ok(())
    }

    fn absorb_registry(&mut self, r: &mut Reader<'_>) -> Result<()> {
        let registry_id = r.read_str()?;
        // Only two registries matter to the renderer: dimension_type decides
        // how tall a chunk is, and biome decides what colour its grass is.
        // Everything else would be thousands of trees built to be discarded.
        if registry_id != "minecraft:dimension_type" && registry_id != "minecraft:worldgen/biome" {
            return Ok(());
        }
        let count = r.read_varint_len(1 << 16)?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let name = r.read_str()?.to_string();
            let has_data = r.read_bool()?;
            let value = if has_data {
                let (v, used) = neuton_nbt::Value::parse_prefix(r.rest())
                    .map_err(|_| neuton_protocol::Error::BadUtf8)?;
                r.read_bytes(used)?;
                Some(v)
            } else {
                None
            };
            entries.push((name, value));
        }
        self.registries.absorb(registry_id, &entries);
        Ok(())
    }

    /// Reads one play packet, answering the housekeeping ones itself.
    pub fn poll(&mut self) -> Result<Event> {
        loop {
            let (id, body) = self.read()?;
            let mut r = Reader::new(&body);
            let len = body.len();
            let named = |source| Error::Decode {
                packet: ids::play::clientbound::name(id).unwrap_or("unknown"),
                id,
                len,
                source,
            };

            match id {
                ids::play::clientbound::LOGIN => {
                    let entity_id = self.read_join(&mut r).map_err(|e| match e {
                        Error::Protocol(p) => named(p),
                        other => other,
                    })?;
                    self.stats.join_ms = self.started.elapsed().as_secs_f64() * 1000.0;
                    return Ok(Event::Joined {
                        entity_id: {
                            self.entity_id = entity_id;
                            entity_id
                        },
                        dimension: self.dimension.clone(),
                    });
                }
                ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT => {
                    let chunk =
                        Chunk::read(&mut r, self.dimension.section_count(), self.dimension.min_y)
                            .map_err(named)?;
                    self.stats.chunks += 1;
                    self.stats.blocks += chunk.block_count() as u64;
                    return Ok(Event::Chunk(Box::new(chunk)));
                }
                ids::play::clientbound::FORGET_LEVEL_CHUNK => {
                    // Packed as z in the high half, x in the low half.
                    let packed = r.read_i64().map_err(named)?;
                    return Ok(Event::ChunkForgotten {
                        x: packed as i32,
                        z: (packed >> 32) as i32,
                    });
                }
                ids::play::clientbound::PLAYER_POSITION => {
                    let teleport_id = r.read_varint().map_err(named)?;
                    let x = r.read_f64().map_err(named)?;
                    let y = r.read_f64().map_err(named)?;
                    let z = r.read_f64().map_err(named)?;
                    let _dx = r.read_f64().map_err(named)?;
                    let _dy = r.read_f64().map_err(named)?;
                    let _dz = r.read_f64().map_err(named)?;
                    let yaw = r.read_f32().map_err(named)?;
                    let pitch = r.read_f32().map_err(named)?;
                    // A bitmask over the Relative enum, saying which of the
                    // fields above are offsets rather than destinations.
                    let relative = Relatives(r.read_i32().unwrap_or(0) as u32);

                    // Offsets are applied to where the server last heard we
                    // were, which is what it based them on.
                    let last = self.reported;
                    let x = Relatives::resolve(relative.x(), x, last.0[0]);
                    let y = Relatives::resolve(relative.y(), y, last.0[1]);
                    let z = Relatives::resolve(relative.z(), z, last.0[2]);
                    let yaw = Relatives::resolve(relative.yaw(), yaw as f64, last.1.0 as f64) as f32;
                    let pitch =
                        Relatives::resolve(relative.pitch(), pitch as f64, last.1.1 as f64) as f32;

                    if std::env::var_os("NEUTON_TRACE").is_some() {
                        eprintln!("net: teleport #{teleport_id} rel {:#07b}", relative.0);
                    }
                    // Not acknowledging a teleport gets the connection dropped,
                    // so this is answered here rather than left to the caller.
                    let mut w = Writer::new();
                    w.write_varint(teleport_id);
                    self.framed.write_packet(ids::play::serverbound::ACCEPT_TELEPORTATION, &w)?;

                    // A teleport is not settled until the client reports
                    // standing exactly where it was put, and servers compare
                    // that report against the teleport with no tolerance. It
                    // goes out here, before another packet is read, because
                    // anything answered in between -- a transaction, a ping --
                    // makes the server give up waiting for it and teleport
                    // again, forever.
                    self.send_movement(Some([x, y, z]), Some((yaw, pitch)), false)?;

                    return Ok(Event::Teleported { x, y, z, yaw, pitch, relative });
                }
                ids::play::clientbound::CONTAINER_SET_CONTENT => {
                    let window = r.read_varint().map_err(named)?;
                    // A state ID for the server to match acknowledgements
                    // against. Only useful once this client can click a slot.
                    let _state = r.read_varint().map_err(named)?;
                    let count = r.read_varint_len(1024).map_err(named)?;
                    let (slots, stopped) = crate::items::read_stacks(&mut r, count);
                    // A stack this client cannot read costs the rest of the
                    // packet, not the connection.
                    let carried = if stopped.is_none() {
                        crate::items::read_stack(&mut r).ok().flatten()
                    } else {
                        None
                    };
                    return Ok(Event::Container { window, slots, carried, unread: stopped });
                }
                ids::play::clientbound::CONTAINER_SET_SLOT => {
                    let window = r.read_varint().map_err(named)?;
                    let _state = r.read_varint().map_err(named)?;
                    let slot = i32::from(r.read_i16().map_err(named)?);
                    let stack = crate::items::read_stack(&mut r).map_err(named)?;
                    return Ok(Event::Slot { window, slot, stack });
                }
                ids::play::clientbound::SET_HELD_SLOT => {
                    let slot = r.read_varint().map_err(named)?;
                    return Ok(Event::HeldSlot(slot));
                }
                ids::play::clientbound::SET_HEALTH => {
                    let health = r.read_f32().map_err(named)?;
                    let food = r.read_varint().map_err(named)?;
                    let saturation = r.read_f32().map_err(named)?;
                    return Ok(Event::Health { health, food, saturation });
                }
                ids::play::clientbound::SET_ENTITY_MOTION => {
                    let who = r.read_varint().map_err(named)?;
                    if who != self.entity_id {
                        return Ok(Event::Ignored {
                            id: ids::play::clientbound::SET_ENTITY_MOTION,
                            name: "minecraft:set_entity_motion",
                            len,
                        });
                    }
                    // Three shorts at eight thousandths of a block each, which
                    // is all the precision a shove needs.
                    const SCALE: f64 = 8000.0;
                    let axis = |r: &mut Reader<'_>| -> Result<f64> {
                        Ok(f64::from(r.read_i16().map_err(named)?) / SCALE)
                    };
                    let x = axis(&mut r)?;
                    let y = axis(&mut r)?;
                    let z = axis(&mut r)?;
                    return Ok(Event::Knockback([x, y, z]));
                }
                ids::play::clientbound::PLAYER_COMBAT_KILL => {
                    // The player ID and the death message follow, neither of
                    // which changes what has to happen next.
                    return Ok(Event::Died);
                }
                ids::play::clientbound::BLOCK_UPDATE => {
                    let at = decode_block_pos(r.read_i64().map_err(named)?);
                    let state = r.read_varint().map_err(named)? as u32;
                    return Ok(Event::BlocksChanged(vec![(at, state)]));
                }
                ids::play::clientbound::SECTION_BLOCKS_UPDATE => {
                    // A section's origin, then one varlong per block holding the
                    // state and the position within the section together.
                    let packed = r.read_i64().map_err(named)?;
                    let section = [
                        (packed >> 42) as i32,
                        ((packed << 44) >> 44) as i32,
                        ((packed << 22) >> 42) as i32,
                    ];
                    let count = r.read_varint_len(4096).map_err(named)?;
                    let mut out = Vec::with_capacity(count);
                    for _ in 0..count {
                        let entry = r.read_varlong().map_err(named)?;
                        let state = (entry >> 12) as u32;
                        let local = (entry & 0xFFF) as i32;
                        out.push((
                            [
                                section[0] * 16 + ((local >> 8) & 0xF),
                                section[1] * 16 + (local & 0xF),
                                section[2] * 16 + ((local >> 4) & 0xF),
                            ],
                            state,
                        ));
                    }
                    return Ok(Event::BlocksChanged(out));
                }
                ids::play::clientbound::CHUNK_BATCH_START => {
                    self.batches.start();
                    return Ok(Event::Ignored {
                        id: ids::play::clientbound::CHUNK_BATCH_START,
                        name: "minecraft:chunk_batch_start",
                        len,
                    });
                }
                ids::play::clientbound::CHUNK_BATCH_FINISHED => {
                    let chunks = r.read_varint().map_err(named)?;
                    self.batches.finish(chunks);
                    // Until this goes out the server will not send another
                    // batch. Ten unanswered and it stops sending chunks at all.
                    let mut w = Writer::new();
                    w.write_f32(self.batches.desired_per_tick());
                    self.framed
                        .write_packet(ids::play::serverbound::CHUNK_BATCH_RECEIVED, &w)?;
                    if std::env::var_os("NEUTON_TRACE").is_some() {
                        eprintln!(
                            "net: chunk batch of {chunks}, asking for {:.1} a tick",
                            self.batches.desired_per_tick()
                        );
                    }
                    return Ok(Event::Ignored {
                        id: ids::play::clientbound::CHUNK_BATCH_FINISHED,
                        name: "minecraft:chunk_batch_finished",
                        len,
                    });
                }
                ids::play::clientbound::KEEP_ALIVE => {
                    let token = r.read_i64().map_err(named)?;
                    let mut w = Writer::new();
                    w.write_i64(token);
                    self.framed.write_packet(ids::play::serverbound::KEEP_ALIVE, &w)?;
                    return Ok(Event::KeepAlive);
                }
                ids::play::clientbound::PING => {
                    let token = r.read_i32().map_err(named)?;
                    if std::env::var_os("NEUTON_TRACE").is_some() {
                        eprintln!("net: ping {token}");
                    }
                    let mut w = Writer::new();
                    w.write_i32(token);
                    self.framed.write_packet(ids::play::serverbound::PONG, &w)?;
                }
                ids::play::clientbound::PLAYER_ABILITIES => {
                    let flags = r.read_u8().map_err(named)?;
                    let fly_speed = r.read_f32().map_err(named)?;
                    let walk_speed = r.read_f32().map_err(named)?;
                    return Ok(Event::Abilities(neuton_world::physics::Abilities {
                        invulnerable: flags & 0x01 != 0,
                        flying: flags & 0x02 != 0,
                        may_fly: flags & 0x04 != 0,
                        instant_build: flags & 0x08 != 0,
                        fly_speed,
                        walk_speed,
                    }));
                }
                ids::play::clientbound::SYSTEM_CHAT => {
                    // An NBT text component, then a flag for whether it belongs
                    // on the action bar rather than in the chat log.
                    if let Ok(value) = neuton_nbt::Value::parse(r.rest()) {
                        let spans = crate::component::flatten(&value);
                        if !spans.is_empty() {
                            return Ok(Event::Chat(spans));
                        }
                    }
                }
                ids::play::clientbound::DISCONNECT => {
                    return Ok(Event::Disconnect(disconnect_reason(&mut r)));
                }
                ids::play::clientbound::START_CONFIGURATION => {
                    // The server is sending us back to configuration, which
                    // happens on a dimension or resource-pack change.
                    self.framed.write_packet(
                        ids::play::serverbound::CONFIGURATION_ACKNOWLEDGED,
                        &Writer::new(),
                    )?;
                    self.state = State::Configuration;
                    self.configure()?;
                }
                ids::play::clientbound::ENTITY_POSITION_SYNC => {
                    let who = r.read_varint().map_err(named)?;
                    if std::env::var_os("NEUTON_TRACE").is_some_and(|v| v == "2") {
                        let x = r.read_f64().unwrap_or_default();
                        let y = r.read_f64().unwrap_or_default();
                        let z = r.read_f64().unwrap_or_default();
                        eprintln!(
                            "net: entity_position_sync {who}{} to {x:.1} {y:.1} {z:.1}",
                            if who == self.entity_id { " (us)" } else { "" }
                        );
                    }
                    return Ok(Event::Ignored {
                        id: ids::play::clientbound::ENTITY_POSITION_SYNC,
                        name: "minecraft:entity_position_sync",
                        len,
                    });
                }
                other => {
                    let name = ids::play::clientbound::name(other).unwrap_or("unknown");
                    if std::env::var_os("NEUTON_TRACE").is_some_and(|v| v == "2") {
                        eprintln!("net: ignored {name} ({other}) len {len}");
                    }
                    return Ok(Event::Ignored { id: other, name, len });
                }
            }
        }
    }

    /// Reads `ClientboundLoginPacket` far enough to learn the dimension shape.
    ///
    /// Only the prefix is parsed. Everything after the dimension type is player
    /// state the decoder does not need, and parsing fields we ignore is just
    /// more surface to get wrong.
    fn read_join(&mut self, r: &mut Reader<'_>) -> Result<i32> {
        let entity_id = r.read_i32()?;
        let _hardcore = r.read_bool()?;
        let level_count = r.read_varint_len(1 << 12)?;
        for _ in 0..level_count {
            r.read_str()?;
        }
        let _max_players = r.read_varint()?;
        let _view_distance = r.read_varint()?;
        let _simulation_distance = r.read_varint()?;
        let _reduced_debug = r.read_bool()?;
        let _show_death_screen = r.read_bool()?;
        let _limited_crafting = r.read_bool()?;

        // CommonPlayerSpawnInfo begins with a Holder<DimensionType>: a VarInt
        // where 0 means an inline definition follows and anything else is
        // registry index + 1.
        let holder = r.read_varint()?;
        if holder > 0 {
            let index = (holder - 1) as usize;
            if let Some(shape) = self.registries.dimension(index) {
                self.dimension = shape.clone();
            }
        }
        Ok(entity_id)
    }

    fn read(&mut self) -> Result<(i32, Vec<u8>)> {
        let frame = self.framed.read_packet()?;
        self.stats.packets += 1;
        self.stats.bytes += frame.len() as u64;
        let mut r = Reader::new(frame);
        let id = r.read_varint()?;
        // Copied out because `frame` borrows the codec's buffer, and handlers
        // need to write replies through that same codec.
        Ok((id, r.rest().to_vec()))
    }

    pub fn dimension(&self) -> &DimensionShape {
        &self.dimension
    }

    pub fn registries(&self) -> &Registries {
        &self.registries
    }

    /// Tells the server where the player is and which way they are looking.
    ///
    /// Without this the server never learns that we moved, so it keeps sending
    /// chunks around the spawn point and never loads any others. Plugins notice
    /// too: a player who has not moved gets their chat muted as if they had
    /// just joined.
    ///
    /// Which packet goes out depends on what actually changed, exactly as the
    /// game does it. Sending position and rotation every tick regardless is
    /// what anti-cheat calls a duplicate look, and it is right to: a real
    /// client does not claim to have turned when it has not.
    ///
    /// `y` is the feet, not the eyes, which is where the camera sits.
    /// Tells the server which hotbar slot is selected. Without this the server
    /// keeps handing out the first slot's item however the client draws it.
    pub fn send_held_slot(&mut self, slot: i32) -> Result<()> {
        let mut w = Writer::new();
        w.write_i16(slot as i16);
        self.framed.write_packet(ids::play::serverbound::SET_CARRIED_ITEM, &w)?;
        Ok(())
    }

    /// Asks to be put back in the world after dying.
    pub fn send_respawn(&mut self) -> Result<()> {
        let mut w = Writer::new();
        w.write_varint(0); // perform respawn, rather than request stats
        self.framed.write_packet(ids::play::serverbound::CLIENT_COMMAND, &w)?;
        Ok(())
    }

    /// Starts, gives up on, or finishes breaking a block.
    ///
    /// `action` is the game's own ordering: 0 starts, 1 gives up, 2 finishes.
    pub fn send_player_action(&mut self, action: i32, at: [i32; 3], face: u8) -> Result<()> {
        self.sequence += 1;
        let mut w = Writer::new();
        w.write_varint(action);
        w.write_i64(encode_block_pos(at));
        w.write_u8(face);
        w.write_varint(self.sequence);
        self.framed.write_packet(ids::play::serverbound::PLAYER_ACTION, &w)?;
        Ok(())
    }

    /// Uses whatever is in hand against a block: placing, opening, flipping.
    ///
    /// `cursor` is where on the face the player is pointing, from zero to one
    /// across the block. Servers use it to decide which half of a slab you get.
    pub fn send_use_item_on(
        &mut self,
        at: [i32; 3],
        face: u8,
        cursor: [f32; 3],
        inside: bool,
    ) -> Result<()> {
        self.sequence += 1;
        let mut w = Writer::new();
        w.write_varint(0); // main hand
        w.write_i64(encode_block_pos(at));
        w.write_varint(i32::from(face));
        w.write_f32(cursor[0]);
        w.write_f32(cursor[1]);
        w.write_f32(cursor[2]);
        w.write_bool(inside);
        w.write_bool(false); // not a world border hit
        w.write_varint(self.sequence);
        self.framed.write_packet(ids::play::serverbound::USE_ITEM_ON, &w)?;
        Ok(())
    }

    /// Uses what is in hand with nothing in front of it: eating, drawing a bow.
    pub fn send_use_item(&mut self, yaw: f32, pitch: f32) -> Result<()> {
        self.sequence += 1;
        let mut w = Writer::new();
        w.write_varint(0); // main hand
        w.write_varint(self.sequence);
        w.write_f32(yaw);
        w.write_f32(pitch);
        self.framed.write_packet(ids::play::serverbound::USE_ITEM, &w)?;
        Ok(())
    }

    /// The arm swing everyone else sees.
    pub fn send_swing(&mut self) -> Result<()> {
        let mut w = Writer::new();
        w.write_varint(0); // main hand
        self.framed.write_packet(ids::play::serverbound::SWING, &w)?;
        Ok(())
    }

    /// Hits an entity.
    pub fn send_attack(&mut self, entity: i32, sneaking: bool) -> Result<()> {
        let mut w = Writer::new();
        w.write_varint(entity);
        w.write_varint(1); // attack, rather than interact
        w.write_bool(sneaking);
        self.framed.write_packet(ids::play::serverbound::INTERACT, &w)?;
        Ok(())
    }

    pub fn send_movement(
        &mut self,
        position: Option<[f64; 3]>,
        rotation: Option<(f32, f32)>,
        on_ground: bool,
    ) -> Result<()> {
        // Bit 0 is on ground, bit 1 a horizontal collision.
        let flags = u8::from(on_ground);
        let mut w = Writer::new();

        let id = match (position, rotation) {
            (Some(p), Some((yaw, pitch))) => {
                w.write_f64(p[0]);
                w.write_f64(p[1]);
                w.write_f64(p[2]);
                w.write_f32(yaw);
                w.write_f32(pitch);
                w.write_u8(flags);
                ids::play::serverbound::MOVE_PLAYER_POS_ROT
            }
            (Some(p), None) => {
                w.write_f64(p[0]);
                w.write_f64(p[1]);
                w.write_f64(p[2]);
                w.write_u8(flags);
                ids::play::serverbound::MOVE_PLAYER_POS
            }
            (None, Some((yaw, pitch))) => {
                w.write_f32(yaw);
                w.write_f32(pitch);
                w.write_u8(flags);
                ids::play::serverbound::MOVE_PLAYER_ROT
            }
            // Standing still and looking the same way: a keep-alive for
            // movement, which the game still sends but rarely.
            (None, None) => {
                w.write_u8(flags);
                ids::play::serverbound::MOVE_PLAYER_STATUS_ONLY
            }
        };
        if std::env::var_os("NEUTON_TRACE").is_some() {
            eprintln!("net: move {position:?} {rotation:?} ground={on_ground}");
        }
        if let Some(p) = position {
            self.reported.0 = p;
        }
        if let Some(r) = rotation {
            self.reported.1 = r;
        }
        self.framed.write_packet(id, &w)?;
        Ok(())
    }

    /// Marks the end of a client tick.
    ///
    /// Sent once per tick after whatever else the tick produced. Real clients
    /// have done this since 1.21.2 and anti-cheat checks for it, reasonably: a
    /// client that never ends its ticks is not running the game loop.
    pub fn send_tick_end(&mut self) -> Result<()> {
        self.framed
            .write_packet(ids::play::serverbound::CLIENT_TICK_END, &Writer::new())?;
        Ok(())
    }

    /// Tells the server the client has finished loading the world.
    ///
    /// Sent once, after the first teleport is acknowledged. The server holds
    /// some state back until it arrives.
    pub fn send_loaded(&mut self) -> Result<()> {
        if std::env::var_os("NEUTON_TRACE").is_some() {
            eprintln!("net: player_loaded");
        }
        self.framed
            .write_packet(ids::play::serverbound::PLAYER_LOADED, &Writer::new())?;
        Ok(())
    }

    /// Sends a chat message.
    ///
    /// Unsigned. Since 1.19 the game signs chat with a key from the session
    /// server, and a server running `enforce-secure-profile` will refuse this;
    /// most do not, and an offline-mode server cannot check a signature anyway.
    pub fn send_chat(&mut self, message: &str) -> Result<()> {
        let mut w = Writer::new();
        w.write_str(&truncate(message, 256));
        // Timestamp and salt are part of what a signature would cover.
        w.write_i64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        );
        w.write_i64(0); // salt
        w.write_bool(false); // no signature
        // LastSeenMessages update: nothing acknowledged.
        w.write_varint(0);
        w.write_bytes(&[0, 0, 0]); // 20-bit fixed bitset
        w.write_u8(0); // checksum
        self.framed.write_packet(ids::play::serverbound::CHAT, &w)?;
        Ok(())
    }

    /// Sends a command, without the leading slash.
    ///
    /// Its own packet, and unlike chat it carries no signature at all, so it
    /// works on servers that refuse unsigned chat.
    pub fn send_command(&mut self, command: &str) -> Result<()> {
        let mut w = Writer::new();
        w.write_str(&truncate(command, 256));
        self.framed.write_packet(ids::play::serverbound::CHAT_COMMAND, &w)?;
        Ok(())
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_encrypted(&self) -> bool {
        self.framed.is_encrypted()
    }

    /// Whether a packet is already waiting to be read.
    ///
    /// Lets a caller finish draining a burst before doing work that only makes
    /// sense once the burst is over.
    pub fn has_pending(&mut self) -> bool {
        self.framed.has_pending()
    }

    pub fn compression(&self) -> Option<i32> {
        self.framed.compression()
    }
}

fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    use std::net::ToSocketAddrs;
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address resolved"))?;
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    // Movement packets are tiny and latency-critical; Nagle would batch them.
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    Ok(stream)
}

/// Disconnect reasons arrive as NBT text components. Pulling the plain text out
/// is enough to show the player why they were kicked.
fn disconnect_reason(r: &mut Reader<'_>) -> String {
    if let Ok(v) = neuton_nbt::Value::parse(r.rest()) {
        if let Some(s) = v.get("text").and_then(|t| t.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
        if let Some(s) = v.as_str() {
            return s.to_string();
        }
    }
    "no reason given".to_string()
}

/// Cuts a string to a byte budget without splitting a character.
///
/// The server rejects an over-long message outright, and slicing a `String` by
/// bytes would panic mid-character on anything non-ASCII.
#[doc(hidden)]
pub fn truncate_for_test(text: &str, max: usize) -> String {
    truncate(text, max)
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn describe(name: Option<&'static str>, id: i32) -> String {
    match name {
        Some(n) => format!("{n} ({id})"),
        None => format!("packet {id}"),
    }
}

/// Unpacks the game's block position: 26 bits of x, 26 of z, 12 of y.
fn decode_block_pos(packed: i64) -> [i32; 3] {
    [
        (packed >> 38) as i32,
        ((packed << 52) >> 52) as i32,
        ((packed << 26) >> 38) as i32,
    ]
}

fn encode_block_pos(at: [i32; 3]) -> i64 {
    ((at[0] as i64 & 0x3FF_FFFF) << 38)
        | ((at[2] as i64 & 0x3FF_FFFF) << 12)
        | (at[1] as i64 & 0xFFF)
}

#[cfg(test)]
mod position_tests {
    use super::*;

    #[test]
    fn block_positions_survive_a_round_trip() {
        for at in [[0, 0, 0], [1, 2, 3], [-1587, 71, -2111], [-30_000_000, -64, 30_000_000]] {
            assert_eq!(decode_block_pos(encode_block_pos(at)), at, "{at:?}");
        }
    }
}
