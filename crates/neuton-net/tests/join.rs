//! End-to-end test of the join sequence against a scripted server.
//!
//! Exercises the parts that only fail when assembled: compression switching
//! mid-login, the configuration exchange, learning the dimension shape from
//! registry data, and then decoding a chunk against that shape. A unit test on
//! any one layer would miss all of it.

use neuton_auth::{Profile, Session};
use neuton_net::{Connection, Event};
use neuton_protocol::{Framed, Reader, Writer, ids};
use std::io;
use std::net::{TcpListener, TcpStream};

/// Nether-shaped so the test fails if the client falls back to overworld
/// defaults instead of reading the registry.
const MIN_Y: i32 = 0;
const HEIGHT: i32 = 256;
const SECTIONS: usize = 16;
const COMPRESSION_THRESHOLD: i32 = 64;

fn nbt_dimension(min_y: i32, height: i32) -> Vec<u8> {
    let mut v = vec![0x0A]; // network NBT root: unnamed compound
    for (name, value) in [("min_y", min_y), ("height", height)] {
        v.push(0x03); // TAG_Int
        v.extend_from_slice(&(name.len() as u16).to_be_bytes());
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(&value.to_be_bytes());
    }
    v.push(0x00); // TAG_End
    v
}

/// One chunk column of `SECTIONS` sections; every third section is stone.
fn chunk_body(x: i32, z: i32) -> Vec<u8> {
    let mut sections = Writer::new();
    for i in 0..SECTIONS {
        let solid = i % 3 == 1;
        sections.write_i16(if solid { 4096 } else { 0 });
        sections.write_i16(0); // fluid count
        sections.write_u8(0); // single-valued block palette
        sections.write_varint(if solid { 1 } else { 0 });
        sections.write_u8(0); // single-valued biome palette
        sections.write_varint(0);
    }

    let mut w = Writer::new();
    w.write_i32(x);
    w.write_i32(z);
    w.write_varint(0); // no heightmaps
    w.write_byte_array(sections.as_slice());
    w.write_varint(0); // no block entities
    // Light data follows in the real packet; the decoder must stop before it.
    w.write_bytes(&[0xEE; 16]);
    w.into_vec()
}

/// Plays the server side of a join, then sends two chunks.
fn serve(stream: TcpStream) -> io::Result<()> {
    let mut f = Framed::new(stream);

    // Handshake and login start.
    let _ = f.read_packet()?;
    let _ = f.read_packet()?;

    // Turn compression on mid-login: the client must switch codecs before its
    // next read or everything after this desyncs.
    let mut w = Writer::new();
    w.write_varint(COMPRESSION_THRESHOLD);
    f.write_packet(ids::login::clientbound::LOGIN_COMPRESSION, &w)?;
    f.set_compression(COMPRESSION_THRESHOLD);

    let mut w = Writer::new();
    w.write_uuid(0x1234);
    w.write_str("neutontest");
    w.write_varint(0); // no profile properties
    w.write_uuid(0xABCD); // session id
    f.write_packet(ids::login::clientbound::LOGIN_FINISHED, &w)?;
    let _ = f.read_packet()?; // login_acknowledged

    // Configuration.
    let _ = f.read_packet()?; // client_information

    let mut w = Writer::new();
    w.write_varint(0);
    f.write_packet(ids::configuration::clientbound::SELECT_KNOWN_PACKS, &w)?;
    let _ = f.read_packet()?; // the client's known packs

    // A decoy registry the client must skip without choking.
    let mut w = Writer::new();
    w.write_str("minecraft:banner_pattern");
    w.write_varint(1);
    w.write_str("minecraft:base");
    w.write_bool(false);
    f.write_packet(ids::configuration::clientbound::REGISTRY_DATA, &w)?;

    // The one that matters. Index 0 is a decoy so the Holder index has to be
    // read correctly rather than defaulting to the first entry.
    let mut w = Writer::new();
    w.write_str("minecraft:dimension_type");
    w.write_varint(2);
    w.write_str("minecraft:overworld");
    w.write_bool(true);
    w.write_bytes(&nbt_dimension(-64, 384));
    w.write_str("minecraft:the_nether");
    w.write_bool(true);
    w.write_bytes(&nbt_dimension(MIN_Y, HEIGHT));
    f.write_packet(ids::configuration::clientbound::REGISTRY_DATA, &w)?;

    f.write_packet(ids::configuration::clientbound::FINISH_CONFIGURATION, &Writer::new())?;
    let _ = f.read_packet()?; // the client's finish_configuration

    // Play: join game, pointing at dimension index 1 (the nether).
    let mut w = Writer::new();
    w.write_i32(4242); // entity id
    w.write_bool(false); // hardcore
    w.write_varint(1);
    w.write_str("minecraft:overworld");
    w.write_varint(20); // max players
    w.write_varint(10); // view distance
    w.write_varint(10); // simulation distance
    w.write_bool(false); // reduced debug
    w.write_bool(true); // show death screen
    w.write_bool(false); // limited crafting
    w.write_varint(2); // Holder: registry index 1, encoded as index + 1
    w.write_str("minecraft:the_nether");
    w.write_i64(0); // seed
    w.write_u8(0); // game type
    f.write_packet(ids::play::clientbound::LOGIN, &w)?;

    for (x, z) in [(0i32, 0i32), (1, -1)] {
        let body = chunk_body(x, z);
        let mut w = Writer::new();
        w.write_bytes(&body);
        f.write_packet(ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT, &w)?;
    }

    // A keep-alive, which the client must answer on its own.
    let mut w = Writer::new();
    w.write_i64(0x5555);
    f.write_packet(ids::play::clientbound::KEEP_ALIVE, &w)?;
    let reply = f.read_packet()?;
    let mut r = Reader::new(reply);
    assert_eq!(r.read_varint().unwrap(), ids::play::serverbound::KEEP_ALIVE);
    assert_eq!(r.read_i64().unwrap(), 0x5555, "keep-alive token must be echoed");

    Ok(())
}

fn offline_session() -> Session {
    Session {
        profile: Profile { uuid: 0, name: "neutontest".into() },
        access_token: String::new(),
        refresh_token: String::new(),
        expires_at: u64::MAX,
    }
}

#[test]
fn joins_a_server_and_decodes_its_chunks() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        serve(stream)
    });

    let mut conn = Connection::join(&addr.ip().to_string(), addr.port(), &offline_session())
        .expect("join should succeed");

    assert_eq!(conn.compression(), Some(COMPRESSION_THRESHOLD));
    assert!(!conn.is_encrypted(), "an offline-mode server sends no encryption request");

    let mut chunks = 0;
    let mut joined = false;
    let mut keep_alives = 0;

    while chunks < 2 || keep_alives == 0 {
        match conn.poll().expect("poll") {
            Event::Joined { entity_id, dimension } => {
                assert_eq!(entity_id, 4242);
                // Proof the dimension came from the registry rather than a
                // default: these are the nether's numbers, not the overworld's.
                assert_eq!(dimension.min_y, MIN_Y);
                assert_eq!(dimension.height, HEIGHT);
                assert_eq!(dimension.section_count(), SECTIONS);
                joined = true;
            }
            Event::Chunk(c) => {
                assert!(joined, "chunks must not arrive before the join packet");
                assert_eq!(c.sections.len(), SECTIONS);
                assert_eq!(c.min_y, MIN_Y);
                // Sections 1, 4, 7, 10, 13 are solid: 5 of 16.
                assert_eq!(c.non_empty_sections().count(), 5);
                assert_eq!(c.block_count(), 5 * 4096);
                // Section 1 spans y 16..32 in a min_y=0 dimension.
                assert_eq!(c.state_at(0, 16, 0).map(|s| s.0), Some(1));
                assert_eq!(c.state_at(0, 0, 0).map(|s| s.0), Some(0));
                chunks += 1;
            }
            Event::KeepAlive => keep_alives += 1,
            Event::Disconnect(why) => panic!("unexpected disconnect: {why}"),
            _ => {}
        }
    }

    assert_eq!(chunks, 2);
    assert_eq!(conn.stats.chunks, 2);
    assert_eq!(conn.stats.blocks, 2 * 5 * 4096);
    server.join().expect("server thread").expect("server script");
}

#[test]
fn a_login_disconnect_is_reported_with_its_reason() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut f = Framed::new(stream);
        let _ = f.read_packet();
        let _ = f.read_packet();
        let mut w = Writer::new();
        w.write_str("{\"text\":\"server full\"}");
        let _ = f.write_packet(ids::login::clientbound::LOGIN_DISCONNECT, &w);
    });

    let err = match Connection::join(&addr.ip().to_string(), addr.port(), &offline_session()) {
        Err(e) => e,
        Ok(_) => panic!("join must fail when the server refuses it"),
    };
    assert!(err.to_string().contains("server full"), "got: {err}");
}

#[test]
fn a_long_message_is_cut_at_a_character_boundary() {
    // Not a network test: slicing by bytes would panic mid-character, and a
    // server rejects an over-long message anyway.
    let long = "é".repeat(200); // 400 bytes
    let cut = neuton_net::connection::truncate_for_test(&long, 256);
    assert!(cut.len() <= 256);
    assert!(cut.chars().all(|c| c == 'é'));
    assert_eq!(neuton_net::connection::truncate_for_test("short", 256), "short");
}
