//! Server list ping: the MOTD, player counts, and the icon.
//!
//! This is the status handshake, not a login. It opens a socket, asks, and
//! closes. No account is involved and the server records nothing, which is why
//! the launcher can refresh a whole list without asking anyone to sign in.

use neuton_protocol::{Framed, PROTOCOL_VERSION, Reader, Writer, ids};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);

/// One run of colour and style in a MOTD.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    /// RGB, already resolved from a named colour or a `#rrggbb` string.
    pub color: Option<[u8; 3]>,
    pub bold: bool,
    pub italic: bool,
    pub underlined: bool,
    pub strikethrough: bool,
    pub obfuscated: bool,
}

impl Span {
    fn plain(text: String) -> Self {
        Self {
            text,
            color: None,
            bold: false,
            italic: false,
            underlined: false,
            strikethrough: false,
            obfuscated: false,
        }
    }
}

/// What a server said about itself.
#[derive(Debug, Clone, Default)]
pub struct ServerStatus {
    /// MOTD, flattened to styled runs.
    pub motd: Vec<Span>,
    pub players_online: i64,
    pub players_max: i64,
    /// Server software and version string, e.g. "Paper 1.21".
    pub version_name: String,
    pub protocol: i64,
    /// Raw PNG bytes of the 64x64 server icon, if it sent one.
    pub favicon_png: Option<Vec<u8>>,
    pub latency_ms: f64,
}

impl ServerStatus {
    /// MOTD with styling discarded, for tooltips and logs.
    pub fn motd_text(&self) -> String {
        self.motd.iter().map(|s| s.text.as_str()).collect()
    }

    /// True if this build can join the server, going by protocol number alone.
    pub fn compatible(&self) -> bool {
        self.protocol == PROTOCOL_VERSION as i64
    }
}

/// Pings a server and parses its status response.
pub fn ping(host: &str, port: u16) -> Result<ServerStatus, String> {
    let started = Instant::now();
    let mut conn = Framed::connect((host, port), TIMEOUT).map_err(|e| e.to_string())?;

    let mut w = Writer::new();
    w.write_varint(PROTOCOL_VERSION);
    w.write_str(host);
    w.write_u16(port);
    w.write_varint(1); // next state: status
    conn.write_packet(ids::handshake::serverbound::INTENTION, &w)
        .map_err(|e| e.to_string())?;
    conn.write_packet(ids::status::serverbound::STATUS_REQUEST, &Writer::new())
        .map_err(|e| e.to_string())?;

    let frame = conn.read_packet().map_err(|e| e.to_string())?;
    let mut r = Reader::new(frame);
    let id = r.read_varint().map_err(|e| e.to_string())?;
    if id != ids::status::clientbound::STATUS_RESPONSE {
        return Err(format!("expected a status response, got packet {id}"));
    }
    let json = r.read_str().map_err(|e| e.to_string())?;
    let mut status = parse(json)?;

    // Measured over the ping packet rather than the whole exchange, so a large
    // MOTD does not inflate the number.
    let probe = Instant::now();
    let mut w = Writer::new();
    w.write_i64(0x6E65_7574);
    if conn.write_packet(ids::status::serverbound::PING_REQUEST, &w).is_ok()
        && conn.read_packet().is_ok()
    {
        status.latency_ms = probe.elapsed().as_secs_f64() * 1000.0;
    } else {
        status.latency_ms = started.elapsed().as_secs_f64() * 1000.0;
    }
    Ok(status)
}

fn parse(json: &str) -> Result<ServerStatus, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("malformed status json: {e}"))?;

    let mut motd = Vec::new();
    if let Some(desc) = v.get("description") {
        flatten(desc, &Span::plain(String::new()), &mut motd);
    }
    // Collapse runs that ended up identical, which legacy codes produce a lot of.
    motd.retain(|s| !s.text.is_empty());

    let favicon_png = v
        .get("favicon")
        .and_then(|f| f.as_str())
        .and_then(|s| s.strip_prefix("data:image/png;base64,"))
        .and_then(|b64| base64_decode(b64.trim()));

    Ok(ServerStatus {
        motd,
        players_online: v.pointer("/players/online").and_then(|p| p.as_i64()).unwrap_or(-1),
        players_max: v.pointer("/players/max").and_then(|p| p.as_i64()).unwrap_or(-1),
        version_name: v
            .pointer("/version/name")
            .and_then(|p| p.as_str())
            .unwrap_or_default()
            .to_string(),
        protocol: v.pointer("/version/protocol").and_then(|p| p.as_i64()).unwrap_or(-1),
        favicon_png,
        latency_ms: 0.0,
    })
}

/// Walks a text component into styled runs.
///
/// A component can be a bare string, an array, or an object with `extra`
/// children that inherit the parent's style. Servers also still emit legacy
/// section-sign codes inside `text`, so both forms have to be handled.
fn flatten(v: &serde_json::Value, inherited: &Span, out: &mut Vec<Span>) {
    match v {
        serde_json::Value::String(s) => push_legacy(s, inherited, out),
        serde_json::Value::Array(items) => {
            for item in items {
                flatten(item, inherited, out);
            }
        }
        serde_json::Value::Object(_) => {
            let mut style = inherited.clone();
            style.text.clear();
            if let Some(c) = v.get("color").and_then(|c| c.as_str())
                && let Some(rgb) = color_of(c)
            {
                style.color = Some(rgb);
            }
            for (key, field) in [
                ("bold", &mut style.bold as *mut bool),
                ("italic", &mut style.italic as *mut bool),
                ("underlined", &mut style.underlined as *mut bool),
                ("strikethrough", &mut style.strikethrough as *mut bool),
                ("obfuscated", &mut style.obfuscated as *mut bool),
            ] {
                if let Some(b) = v.get(key).and_then(|b| b.as_bool()) {
                    // Safe: each pointer targets a distinct field of `style`,
                    // which is alive for the whole loop.
                    unsafe { *field = b };
                }
            }
            if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
                push_legacy(text, &style, out);
            }
            if let Some(extra) = v.get("extra") {
                flatten(extra, &style, out);
            }
        }
        _ => {}
    }
}

/// Splits a string on legacy section-sign codes, emitting a run per style change.
fn push_legacy(text: &str, base: &Span, out: &mut Vec<Span>) {
    if !text.contains('\u{a7}') {
        let mut span = base.clone();
        span.text = text.to_string();
        out.push(span);
        return;
    }

    let mut current = base.clone();
    current.text.clear();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{a7}' {
            current.text.push(c);
            continue;
        }
        let Some(code) = chars.next() else { break };
        if !current.text.is_empty() {
            out.push(current.clone());
            current.text.clear();
        }
        match code.to_ascii_lowercase() {
            'k' => current.obfuscated = true,
            'l' => current.bold = true,
            'm' => current.strikethrough = true,
            'n' => current.underlined = true,
            'o' => current.italic = true,
            // Reset drops every style, not just the colour.
            'r' => current = Span::plain(String::new()),
            other => {
                if let Some(rgb) = legacy_color(other) {
                    current.color = Some(rgb);
                }
            }
        }
    }
    if !current.text.is_empty() {
        out.push(current);
    }
}

/// The sixteen named colours, plus `#rrggbb` which servers may use directly.
fn color_of(name: &str) -> Option<[u8; 3]> {
    if let Some(hex) = name.strip_prefix('#')
        && hex.len() == 6
    {
        let n = u32::from_str_radix(hex, 16).ok()?;
        return Some([(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    }
    Some(match name {
        "black" => [0x00, 0x00, 0x00],
        "dark_blue" => [0x00, 0x00, 0xAA],
        "dark_green" => [0x00, 0xAA, 0x00],
        "dark_aqua" => [0x00, 0xAA, 0xAA],
        "dark_red" => [0xAA, 0x00, 0x00],
        "dark_purple" => [0xAA, 0x00, 0xAA],
        "gold" => [0xFF, 0xAA, 0x00],
        "gray" => [0xAA, 0xAA, 0xAA],
        "dark_gray" => [0x55, 0x55, 0x55],
        "blue" => [0x55, 0x55, 0xFF],
        "green" => [0x55, 0xFF, 0x55],
        "aqua" => [0x55, 0xFF, 0xFF],
        "red" => [0xFF, 0x55, 0x55],
        "light_purple" => [0xFF, 0x55, 0xFF],
        "yellow" => [0xFF, 0xFF, 0x55],
        "white" => [0xFF, 0xFF, 0xFF],
        _ => return None,
    })
}

fn legacy_color(code: char) -> Option<[u8; 3]> {
    let name = match code {
        '0' => "black",
        '1' => "dark_blue",
        '2' => "dark_green",
        '3' => "dark_aqua",
        '4' => "dark_red",
        '5' => "dark_purple",
        '6' => "gold",
        '7' => "gray",
        '8' => "dark_gray",
        '9' => "blue",
        'a' => "green",
        'b' => "aqua",
        'c' => "red",
        'd' => "light_purple",
        'e' => "yellow",
        'f' => "white",
        _ => return None,
    };
    color_of(name)
}

/// Standard base64, enough for the favicon field.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, &c) in TABLE.iter().enumerate() {
        lookup[c as usize] = i as u8;
    }

    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &byte in s.as_bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let v = lookup[byte as usize];
        if v == 255 {
            return None;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_string_motd() {
        let s = parse(r#"{"description":"A Minecraft Server"}"#).unwrap();
        assert_eq!(s.motd_text(), "A Minecraft Server");
        assert_eq!(s.motd[0].color, None);
    }

    #[test]
    fn nested_components_inherit_parent_style() {
        let json = r#"{"description":{"text":"","bold":true,"color":"gold",
            "extra":[{"text":"Miji"},{"text":"SMP","color":"aqua"}]}}"#;
        let s = parse(json).unwrap();
        assert_eq!(s.motd_text(), "MijiSMP");
        // Both inherit bold, only the second overrides the colour.
        assert!(s.motd[0].bold && s.motd[1].bold);
        assert_eq!(s.motd[0].color, Some([0xFF, 0xAA, 0x00]));
        assert_eq!(s.motd[1].color, Some([0x55, 0xFF, 0xFF]));
    }

    #[test]
    fn legacy_section_codes_split_into_runs() {
        let s = parse("{\"description\":\"\u{a7}cRed\u{a7}lBold\u{a7}rPlain\"}").unwrap();
        assert_eq!(s.motd_text(), "RedBoldPlain");
        assert_eq!(s.motd[0].color, Some([0xFF, 0x55, 0x55]));
        assert!(!s.motd[0].bold);
        // Bold inherits the red that preceded it.
        assert!(s.motd[1].bold);
        assert_eq!(s.motd[1].color, Some([0xFF, 0x55, 0x55]));
        // Reset clears colour as well as style.
        assert!(!s.motd[2].bold);
        assert_eq!(s.motd[2].color, None);
    }

    #[test]
    fn hex_colours_are_accepted() {
        let s = parse(r##"{"description":{"text":"x","color":"#4de6c4"}}"##).unwrap();
        assert_eq!(s.motd[0].color, Some([0x4d, 0xe6, 0xc4]));
    }

    #[test]
    fn players_version_and_protocol_are_read() {
        let json = r#"{"description":"hi","players":{"online":7,"max":100},
            "version":{"name":"Paper 26.2","protocol":776}}"#;
        let s = parse(json).unwrap();
        assert_eq!((s.players_online, s.players_max), (7, 100));
        assert_eq!(s.version_name, "Paper 26.2");
        assert!(s.compatible());
    }

    #[test]
    fn a_mismatched_protocol_is_flagged() {
        let s = parse(r#"{"version":{"protocol":47}}"#).unwrap();
        assert!(!s.compatible());
    }

    #[test]
    fn missing_fields_do_not_fail_the_parse() {
        let s = parse("{}").unwrap();
        assert_eq!(s.players_online, -1);
        assert!(s.motd.is_empty());
        assert!(s.favicon_png.is_none());
    }

    #[test]
    fn favicon_is_decoded_from_the_data_uri() {
        // "PNG!" in base64.
        let s = parse(r#"{"favicon":"data:image/png;base64,UE5HIQ=="}"#).unwrap();
        assert_eq!(s.favicon_png.as_deref(), Some(&b"PNG!"[..]));
    }

    #[test]
    fn base64_round_trips_a_known_vector() {
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert!(base64_decode("not base64!!").is_none());
    }
}
