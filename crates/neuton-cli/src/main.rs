//! Developer entry point for the neuton client.
//!
//! `neuton ping <host[:port]>` performs a real server-list ping at protocol 776
//! and reports the round trip. It exists to keep the wire layer honest against
//! live servers, including ones behind proxies like TCPShield.

use neuton_protocol::{Framed, PROTOCOL_VERSION, Reader, Writer, ids};
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 25565;
const TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ping") if args.len() >= 2 => ping(&args[1]),
        Some("info") => {
            info();
            Ok(())
        }
        _ => {
            eprintln!("usage: neuton ping <host[:port]>\n       neuton info");
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
