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
        Some("play") if args.len() >= 2 => play(&args[1], offline_name(&args), shot_path(&args)),
        Some("login") => login(),
        Some("logout") => logout(args.get(1).map(String::as_str)),
        Some("accounts") => accounts(),
        Some("switch") if args.len() >= 2 => switch(&args[1]),
        Some("whoami") => whoami(),
        Some("info") => {
            info();
            Ok(())
        }
        // No arguments is the normal way to start: open the launcher.
        None => neuton_ui::run(),
        _ => {
            eprintln!(
                "usage: neuton                      open the launcher\n\
                 \x20      neuton ping <host[:port]>   server list ping\n\
                 \x20      neuton join <host[:port]>   connect and stream the world\n\
                 \x20      neuton play <host[:port]>   open straight into the world\n\
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

/// Opens the window directly in a world, skipping the launcher.
fn play(
    target: &str,
    offline: Option<&str>,
    shot: Option<(std::path::PathBuf, Duration)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (host, port) = split_host_port(target);
    let session = match offline {
        Some(name) => neuton_auth::Session {
            profile: neuton_auth::Profile { uuid: 0, name: name.to_string() },
            access_token: String::new(),
            refresh_token: String::new(),
            expires_at: u64::MAX,
        },
        None => {
            let mut store = Accounts::load_default()?;
            neuton_auth::authenticate(&mut store, true, prompt)?.0
        }
    };
    match shot {
        Some((path, after)) => {
            neuton_ui::run_screenshot(host.to_string(), port, session, path, after)
        }
        None => neuton_ui::run_direct(host.to_string(), port, session),
    }
}

/// `--offline <name>` runs the join without Microsoft auth, for testing
/// against a development server.
/// `--shot <path> [seconds]` renders one frame and exits.
fn shot_path(args: &[String]) -> Option<(std::path::PathBuf, Duration)> {
    let i = args.iter().position(|a| a == "--shot")?;
    let path = std::path::PathBuf::from(args.get(i + 1)?);
    let secs = args.get(i + 2).and_then(|s| s.parse::<f64>().ok()).unwrap_or(12.0);
    Some((path, Duration::from_secs_f64(secs)))
}

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
    // Where the server put us, and the column of blocks there once its chunk
    // arrives. Reading real terrain back as block names is the only end-to-end
    // check that palettes, state IDs and the generated tables all agree.
    let mut spawn: Option<(f64, f64, f64)> = None;
    let mut column: Vec<String> = Vec::new();
    // Mesh every chunk as it arrives, to measure the real cost on real terrain
    // rather than on a synthetic column of stone.
    let appearance = neuton_render::Appearance::new();
    // Real textures, resolved from the installed game and whatever resource
    // packs are on top of it.
    let t_assets = Instant::now();
    let textures = {
        let mut packs = neuton_assets::PackStack::new();
        if let Some(jar) = neuton_assets::vanilla_jar("26.2") {
            let _ = packs.push(jar);
        }
        if let Some(dir) = neuton_assets::resource_pack_dir() {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let _ = packs.push(e.path());
            }
        }
        println!("assets   packs {:?}", packs.names());
        neuton_render::BlockTextures::build(&mut packs)
    };
    println!(
        "atlas    {}x{} px, {} textures, {} face sets ({:.0} ms)",
        textures.atlas.size,
        textures.atlas.size,
        textures.atlas.len(),
        textures.distinct(),
        t_assets.elapsed().as_secs_f64() * 1000.0
    );
    let mut mesh_time = Duration::ZERO;
    let mut triangles: u64 = 0;
    let mut vertices: u64 = 0;
    let mut histogram: std::collections::HashMap<&'static str, u64> = Default::default();
    let mut lit_samples: Vec<String> = Vec::new();

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
                // Is this the column the player is standing in?
                if let Some((px, py, pz)) = spawn
                    && column.is_empty()
                    && c.x == (px.floor() as i32).div_euclid(16)
                    && c.z == (pz.floor() as i32).div_euclid(16)
                {
                    let (lx, lz) =
                        ((px.floor() as i32).rem_euclid(16) as usize, (pz.floor() as i32).rem_euclid(16) as usize);
                    let feet = py.floor() as i32;
                    for y in (feet - 4..=feet + 2).rev() {
                        let name = c
                            .state_at(lx, y, lz)
                            .and_then(|s| s.block())
                            .map(|b| b.name().trim_start_matches("minecraft:").to_string())
                            .unwrap_or_else(|| "?".into());
                        column.push(format!("    y={y:<4} {name}"));
                    }
                }
                // What is actually in these chunks, so a bad culling rate can be
                // attributed to the world or to the mesher.
                for section in c.sections.iter().filter(|s| !s.is_empty()) {
                    let mut scratch = vec![0u32; neuton_world::SECTION_VOLUME];
                    if section.blocks.unpack_into(&mut scratch) {
                        for raw in &scratch {
                            if *raw != 0
                                && let Some(b) = neuton_blocks::StateId(*raw).block()
                            {
                                *histogram.entry(b.name()).or_default() += 1;
                            }
                        }
                    }
                }

                // Light arrives with the chunk; check it decoded to something
                // that varies rather than a constant.
                if lit_samples.len() < 6 && c.block_count() > 0 {
                    let sky = c.lighting.sky_at(8, 70, 8);
                    let block = c.lighting.block_at(8, 70, 8);
                    let sections_with_sky = c.lighting.sky.iter().filter(|a| a.is_some()).count();
                    let sections_with_block =
                        c.lighting.block.iter().filter(|a| a.is_some()).count();
                    lit_samples.push(format!(
                        "    ({:>4},{:>4})  sky={sky:<2} block={block:<2}  arrays: {sections_with_sky} sky, {sections_with_block} block",
                        c.x, c.z
                    ));
                }

                let t = Instant::now();
                let mesh = neuton_render::build(&c, &appearance, &textures);
                mesh_time += t.elapsed();
                triangles += mesh.triangles() as u64;
                vertices += mesh.vertices.len() as u64;

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
                if spawn.is_none() {
                    println!("teleport {x:.1} {y:.1} {z:.1}");
                }
                spawn = Some((x, y, z));
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

    if !column.is_empty() {
        println!("\nblocks under the player");
        for line in &column {
            println!("{line}");
        }
    }

    if !lit_samples.is_empty() {
        println!("\nlighting");
        for line in &lit_samples {
            println!("{line}");
        }
    }

    let s = &conn.stats;
    println!("\nsummary");
    println!("  packets  {}", s.packets);
    println!("  bytes    {:.1} KiB", s.bytes as f64 / 1024.0);
    println!("  chunks   {}", s.chunks);
    println!("  blocks   {}", s.blocks);
    if s.chunks > 0 {
        println!("\nmeshing");
        println!("  triangles {triangles} ({vertices} vertices)");
        println!(
            "  time      {:.0} ms total, {:.2} ms per chunk",
            mesh_time.as_secs_f64() * 1000.0,
            mesh_time.as_secs_f64() * 1000.0 / s.chunks as f64
        );
        let mut top: Vec<_> = histogram.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        println!("\nmost common blocks");
        for (name, count) in top.iter().take(10) {
            println!("  {:<28} {}", name.trim_start_matches("minecraft:"), count);
        }
        println!(
            "\n  culled    {:.1}% of faces never drawn",
            100.0 - (triangles as f64 / 2.0) / (s.blocks as f64 * 6.0) * 100.0
        );
    }
    if !ignored.is_empty() {
        let mut top: Vec<_> = ignored.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        let list: Vec<String> =
            top.iter().take(8).map(|(n, c)| format!("{n} x{c}")).collect();
        println!("  unhandled {}", list.join(", "));
    }
    Ok(())
}
