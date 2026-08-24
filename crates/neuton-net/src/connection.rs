//! The login -> configuration -> play state machine.

use crate::registries::{DimensionShape, Registries};
use neuton_auth::Session;
use neuton_protocol::crypto::{SharedSecret, encrypt_key_exchange, server_hash};
use neuton_protocol::{Framed, PROTOCOL_VERSION, Reader, State, Writer, ids};
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

/// Something the caller might want to act on. Packets we do not model yet are
/// reported as [`Event::Ignored`] rather than dropped, so it is visible what a
/// real server actually sends.
#[derive(Debug)]
pub enum Event {
    /// Configuration finished and the world is starting.
    Joined { entity_id: i32, dimension: DimensionShape },
    Chunk(Box<Chunk>),
    ChunkForgotten { x: i32, z: i32 },
    /// The server moved us. Already acknowledged; ignoring these gets us kicked.
    Teleported { x: f64, y: f64, z: f64, yaw: f32, pitch: f32 },
    Chat(String),
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
                        entity_id,
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

                    // Not acknowledging a teleport gets the connection dropped,
                    // so this is answered here rather than left to the caller.
                    let mut w = Writer::new();
                    w.write_varint(teleport_id);
                    self.framed.write_packet(ids::play::serverbound::ACCEPT_TELEPORTATION, &w)?;
                    return Ok(Event::Teleported { x, y, z, yaw, pitch });
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
                    let mut w = Writer::new();
                    w.write_i32(token);
                    self.framed.write_packet(ids::play::serverbound::PONG, &w)?;
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
                other => {
                    return Ok(Event::Ignored {
                        id: other,
                        name: ids::play::clientbound::name(other).unwrap_or("unknown"),
                        len,
                    });
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

fn describe(name: Option<&'static str>, id: i32) -> String {
    match name {
        Some(n) => format!("{n} ({id})"),
        None => format!("packet {id}"),
    }
}
