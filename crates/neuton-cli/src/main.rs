//! Developer entry point for the neuton client.
//!
//! `neuton ping <host[:port]>` performs a real server-list ping at protocol 776
//! and reports the round trip. It exists to keep the wire layer honest against
//! live servers, including ones behind proxies like TCPShield.

use neuton_auth::{Accounts, Origin};
use neuton_net::{Connection, Event};
use neuton_protocol::{Framed, PROTOCOL_VERSION, Reader, Writer, ids};
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 25565;
const TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ping") if args.len() >= 2 => ping(&args[1]),
        Some("join") if args.len() >= 2 => join(&args[1], offline_name(&args)),
        Some("login") => login(),
        Some("logout") => logout(args.get(1).map(String::as_str)),
        Some("accounts") => accounts(),
        Some("switch") if args.len() >= 2 => switch(&args[1]),
        Some("whoami") => whoami(),
        Some("info") => {
            info();
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: neuton ping <host[:port]>   server list ping\n\
                 \x20      neuton join <host[:port]>   connect and stream the world\n\
                 \x20      neuton login               sign in with a Microsoft account\n\
                 \x20      neuton accounts            list signed-in accounts\n\
                 \x20      neuton switch <name>       choose the active account\n\
                 \x20      neuton logout [name]       sign one account out, or all\n\
                 \x20      neuton whoami              show the active account\n\
                 \x20      neuton info                build and registry summary"
            );
            return std::process::ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn info() {
    println!("neuton {}", env!("CARGO_PKG_VERSION"));
    println!("  target version : {} (protocol {PROTOCOL_VERSION})", neuton_protocol::GAME_VERSION);
    println!(
        "  play packets   : {} clientbound, {} serverbound",
        ids::play::clientbound::COUNT,
        ids::play::serverbound::COUNT
    );
    println!(
        "  block registry : {} blocks, {} states",
        neuton_blocks::BLOCK_COUNT,
        neuton_blocks::STATE_COUNT
    );
}

fn ping(target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (host, port) = split_host_port(target);

    let t_connect = Instant::now();
    let mut conn = Framed::connect((host, port), TIMEOUT)?;
    let connect_ms = t_connect.elapsed().as_secs_f64() * 1000.0;

    // Handshake, then straight into the status state.
    let mut body = Writer::new();
    body.write_varint(PROTOCOL_VERSION);
    body.write_str(host);
    body.write_u16(port);
    body.write_varint(1); // next state: status
    conn.write_packet(ids::handshake::serverbound::INTENTION, &body)?;

    let t_status = Instant::now();
    conn.write_packet(ids::status::serverbound::STATUS_REQUEST, &Writer::new())?;
    let frame = conn.read_packet()?;
    let mut r = Reader::new(frame);
    let id = r.read_varint()?;
    if id != ids::status::clientbound::STATUS_RESPONSE {
        return Err(format!(
            "expected status_response, got {} ({id})",
            ids::status::clientbound::name(id).unwrap_or("unknown")
        )
        .into());
    }
    let json = r.read_str()?.to_string();
    let status_ms = t_status.elapsed().as_secs_f64() * 1000.0;

    // Latency probe: the server echoes our payload verbatim.
    let mut body = Writer::new();
    body.write_i64(0x1234_5678);
    let t_pong = Instant::now();
    conn.write_packet(ids::status::serverbound::PING_REQUEST, &body)?;
    let frame = conn.read_packet()?;
    let pong_ms = t_pong.elapsed().as_secs_f64() * 1000.0;
    let mut r = Reader::new(frame);
    let id = r.read_varint()?;
    if id != ids::status::clientbound::PONG_RESPONSE || r.read_i64()? != 0x1234_5678 {
        return Err("server did not echo the ping payload".into());
    }

    println!("{host}:{port}");
    println!("  connect  {connect_ms:>7.1} ms");
    println!("  status   {status_ms:>7.1} ms");
    println!("  ping     {pong_ms:>7.1} ms");
    for (label, value) in summarize(&json) {
        println!("  {label:<8} {value}");
    }
    Ok(())
}

/// `--offline <name>` runs the join without Microsoft auth, for testing
/// against a development server.
fn offline_name(args: &[String]) -> Option<&str> {
    let i = args.iter().position(|a| a == "--offline")?;
    args.get(i + 1).map(String::as_str)
}

fn split_host_port(target: &str) -> (&str, u16) {
    match target.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(DEFAULT_PORT)),
        None => (target, DEFAULT_PORT),
    }
}

/// Pulls the interesting fields out of the status JSON without pulling in a
/// JSON dependency: the CLI is a diagnostic, not a parser under test.
fn summarize(json: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(v) = scrape(json, "\"name\":\"") {
        out.push(("version", v));
    }
    if let Some(v) = scrape(json, "\"protocol\":") {
        out.push(("protocol", v.trim_end_matches(['}', ',']).to_string()));
    }
    if let Some(v) = scrape(json, "\"online\":") {
        out.push(("online", v.trim_end_matches(['}', ',']).to_string()));
    }
    out.push(("bytes", json.len().to_string()));
    out
}

fn scrape(json: &str, key: &str) -> Option<String> {
    let start = json.find(key)? + key.len();
    let rest = &json[start..];
    let end = rest.find(['"', ',', '}']).unwrap_or(rest.len());
    Some(rest[..end.max(1)].to_string())
}


/// Shows the device code and waits. Deliberately loud: this is the one moment
/// the client asks something of the user, and a code they miss is a sign-in
/// that silently times out.
fn prompt(dc: &neuton_auth::DeviceCode) {
    println!();
    println!("  sign in at   {}", dc.verification_uri);
    println!("  enter code   {}", dc.user_code);
    println!();
    if neuton_auth::open_browser(&dc.verification_uri) {
        println!("  (opened in your browser)");
    }
    println!("  use the Microsoft account that owns Minecraft.");
    println!("  waiting...");
}

fn login() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = Accounts::load_default()?;
    let t = Instant::now();
    // Always a fresh sign-in: `login` on a machine that already has an account
    // means "add another", not "reuse the one I have".
    let session = neuton_auth::sign_in(&mut store, prompt)?;
    println!(
        "\nsigned in as {} ({:.0} ms)",
        session.profile.name,
        t.elapsed().as_secs_f64() * 1000.0
    );
    println!("  uuid      {}", session.profile.uuid_hyphenated());
    println!("  session   valid for {} h", session.expires_in() / 3600);
    if store.list().len() > 1 {
        println!("  accounts  {} signed in, this one is now active", store.list().len());
    }
    Ok(())
}

fn accounts() -> Result<(), Box<dyn std::error::Error>> {
    let store = Accounts::load_default()?;
    if store.is_empty() {
        println!("no accounts signed in; run `neuton login`");
        return Ok(());
    }
    for a in store.list() {
        println!(
            "{} {:<17} {}  {}",
            if store.is_active(a) { "*" } else { " " },
            a.profile.name,
            a.profile.uuid_hyphenated(),
            if a.is_valid() { "valid".to_string() } else { "needs refresh".to_string() }
        );
    }
    println!("\n{}", store.path().display());
    Ok(())
}

fn switch(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = Accounts::load_default()?;
    if !store.set_active(name) {
        return Err(format!("no account named {name:?}; run `neuton accounts`").into());
    }
    store.save()?;
    println!("active account is now {}", store.active().unwrap().profile.name);
    Ok(())
}

fn logout(name: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = Accounts::load_default()?;
    match name {
        Some(name) => {
            if !store.remove(name) {
                return Err(format!("no account named {name:?}").into());
            }
            store.save()?;
            println!("signed {name} out");
            match store.active() {
                Some(a) => println!("active account is now {}", a.profile.name),
                None => println!("no accounts left"),
            }
        }
        None => {
            let n = store.list().len();
            store.clear();
            store.delete_file()?;
            println!("signed out {n} account{}", if n == 1 { "" } else { "s" });
        }
    }
    Ok(())
}

fn whoami() -> Result<(), Box<dyn std::error::Error>> {
    let store = Accounts::load_default()?;
    match store.active() {
        Some(s) => {
            println!("{} ({})", s.profile.name, s.profile.uuid_hyphenated());
            println!(
                "  session   {}",
                if s.is_valid() {
                    format!("valid for {} h", s.expires_in() / 3600)
                } else {
                    "expired, will refresh on next use".to_string()
                }
            );
            if store.list().len() > 1 {
                println!("  accounts  {} signed in", store.list().len());
            }
        }
        None => println!("not signed in; run `neuton login`"),
    }
    Ok(())
}

/// Joins a server and streams the world until interrupted.
///
/// This is the end-to-end check on the whole stack: auth, encryption,
/// compression, the configuration exchange and chunk decoding all have to be
/// right for a single chunk to arrive.
fn join(target: &str, offline: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let (host, port) = split_host_port(target);

    let session = match offline {
        // Offline mode exists for development against a local server. The
        // server derives the UUID from the name itself and never sends an
        // encryption request, so the token here is never used.
        Some(name) => {
            println!("auth     offline as {name}");
            neuton_auth::Session {
                profile: neuton_auth::Profile { uuid: 0, name: name.to_string() },
                access_token: String::new(),
                refresh_token: String::new(),
                expires_at: u64::MAX,
            }
        }
        None => {
            let mut store = Accounts::load_default()?;
            let t_auth = Instant::now();
            let (session, origin) = neuton_auth::authenticate(&mut store, true, prompt)?;
            println!(
                "auth     {} as {} ({:.0} ms)",
                match origin {
                    Origin::Cache => "cached",
                    Origin::Refreshed => "refreshed",
                    Origin::Interactive => "interactive",
                },
                session.profile.name,
                t_auth.elapsed().as_secs_f64() * 1000.0
            );
            session
        }
    };

    let mut conn = Connection::join(host, port, &session)?;
    println!(
        "join     {host}:{port}  encrypted={}  compression={}",
        conn.is_encrypted(),
        conn.compression().map(|t| t.to_string()).unwrap_or_else(|| "off".into())
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ignored: std::collections::BTreeMap<&'static str, u32> = Default::default();

    while Instant::now() < deadline {
        match conn.poll()? {
            Event::Joined { entity_id, dimension } => {
                println!(
                    "world    entity {entity_id}, {} sections from y={} ({:.0} ms to join)",
                    dimension.section_count(),
                    dimension.min_y,
                    conn.stats.join_ms
                );
            }
            Event::Chunk(c) => {
                if conn.stats.chunks <= 3 || conn.stats.chunks % 100 == 0 {
                    println!(
                        "chunk    #{} at ({}, {})  {} non-air, {} sections used",
                        conn.stats.chunks,
                        c.x,
                        c.z,
                        c.block_count(),
                        c.non_empty_sections().count()
                    );
                }
            }
            Event::Teleported { x, y, z, .. } => {
                println!("teleport {x:.1} {y:.1} {z:.1}");
            }
            Event::Disconnect(why) => {
                println!("kicked   {why}");
                break;
            }
            Event::Ignored { name, .. } => {
                *ignored.entry(name).or_default() += 1;
            }
            _ => {}
        }
    }

    let s = &conn.stats;
    println!("\nsummary");
    println!("  packets  {}", s.packets);
    println!("  bytes    {:.1} KiB", s.bytes as f64 / 1024.0);
    println!("  chunks   {}", s.chunks);
    println!("  blocks   {}", s.blocks);
    if !ignored.is_empty() {
        let mut top: Vec<_> = ignored.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        let list: Vec<String> =
            top.iter().take(8).map(|(n, c)| format!("{n} x{c}")).collect();
        println!("  unhandled {}", list.join(", "));
    }
    Ok(())
}
