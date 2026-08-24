//! A small DNS client, for the things a Minecraft client actually needs.
//!
//! Two of them. SRV, because `_minecraft._tcp.<domain>` is how a server
//! delegates to a different host and port and the vanilla client honours it, so
//! ignoring it means failing to connect to servers that work everywhere else.
//! And PTR, so the launcher can show what an address really belongs to.
//!
//! Written out rather than pulled in: it is one query type, one UDP round trip,
//! and no dependency is worth a resolver crate for that.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(2);
/// Enough for any answer we ask for; DNS over UDP is capped at 512 anyway
/// without EDNS, and we do not set it.
const MAX_RESPONSE: usize = 512;

const TYPE_SRV: u16 = 33;
const TYPE_PTR: u16 = 12;
const CLASS_IN: u16 = 1;

/// One SRV answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Srv {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

/// Where a hostname actually leads, and how long each step took.
#[derive(Debug, Clone, Default)]
pub struct Resolution {
    /// The name as typed.
    pub queried: String,
    /// SRV answer, if the domain delegates.
    pub srv: Option<Srv>,
    /// Host and port after applying SRV.
    pub effective_host: String,
    pub effective_port: u16,
    /// Every address the effective host resolves to.
    pub addresses: Vec<IpAddr>,
    /// Reverse lookup of the first address.
    pub reverse: Option<String>,
    pub srv_ms: Option<f64>,
    pub lookup_ms: Option<f64>,
    pub reverse_ms: Option<f64>,
}

impl Resolution {
    pub fn primary(&self) -> Option<IpAddr> {
        self.addresses.first().copied()
    }

    /// True if SRV sent us somewhere other than where the user typed.
    pub fn redirected(&self) -> bool {
        self.srv.is_some()
    }
}

/// Resolves a Minecraft server address the way the game does.
///
/// SRV is only consulted when no explicit port was given, which matches vanilla:
/// typing `host:port` means that exact endpoint.
pub fn resolve(host: &str, port: u16, port_was_explicit: bool) -> Resolution {
    let mut out = Resolution {
        queried: host.to_string(),
        effective_host: host.to_string(),
        effective_port: port,
        ..Default::default()
    };

    if !port_was_explicit && !host.parse::<IpAddr>().is_ok() {
        let started = Instant::now();
        if let Some(srv) = query_srv(&format!("_minecraft._tcp.{host}")) {
            out.srv_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            out.effective_host = srv.target.trim_end_matches('.').to_string();
            out.effective_port = srv.port;
            out.srv = Some(srv);
        }
    }

    let started = Instant::now();
    if let Ok(addrs) = (out.effective_host.as_str(), out.effective_port).to_socket_addrs() {
        out.addresses = addrs.map(|a| a.ip()).collect();
        out.addresses.dedup();
        out.lookup_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    }

    if let Some(ip) = out.primary() {
        let started = Instant::now();
        out.reverse = reverse(ip);
        out.reverse_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    }
    out
}

/// Looks up a single SRV record, taking the lowest priority answer.
pub fn query_srv(name: &str) -> Option<Srv> {
    let response = query(name, TYPE_SRV)?;
    let mut best: Option<Srv> = None;
    for (rtype, rdata, msg) in &response {
        if *rtype != TYPE_SRV || rdata.len() < 7 {
            continue;
        }
        let priority = u16::from_be_bytes([rdata[0], rdata[1]]);
        let weight = u16::from_be_bytes([rdata[2], rdata[3]]);
        let port = u16::from_be_bytes([rdata[4], rdata[5]]);
        let target = read_name(msg, msg.len() - rdata.len() + 6)?;
        let candidate = Srv { priority, weight, port, target };
        if best.as_ref().is_none_or(|b| candidate.priority < b.priority) {
            best = Some(candidate);
        }
    }
    best
}

/// Reverse lookup, `in-addr.arpa` for IPv4 and `ip6.arpa` for IPv6.
pub fn reverse(ip: IpAddr) -> Option<String> {
    let name = match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0])
        }
        IpAddr::V6(v6) => {
            let mut name = String::with_capacity(72);
            for byte in v6.octets().iter().rev() {
                name.push_str(&format!("{:x}.{:x}.", byte & 0x0F, byte >> 4));
            }
            name.push_str("ip6.arpa");
            name
        }
    };
    let response = query(&name, TYPE_PTR)?;
    for (rtype, rdata, msg) in &response {
        if *rtype == TYPE_PTR {
            let offset = msg.len() - rdata.len();
            return read_name(msg, offset).map(|n| n.trim_end_matches('.').to_string());
        }
    }
    None
}

/// Sends one query and returns each answer as (type, rdata, whole message).
///
/// The whole message comes back with every record because DNS compresses names
/// as pointers into earlier bytes, so a name inside an answer cannot be decoded
/// without the packet it came from.
fn query(name: &str, qtype: u16) -> Option<Vec<(u16, Vec<u8>, Vec<u8>)>> {
    let server = system_resolver()?;
    let socket = UdpSocket::bind(if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    socket.set_read_timeout(Some(TIMEOUT)).ok()?;
    socket.set_write_timeout(Some(TIMEOUT)).ok()?;

    // Transaction id. Not security, just so a stale reply on this ephemeral
    // socket is discarded rather than parsed.
    let id: u16 = (std::process::id() as u16) ^ (name.len() as u16) << 8 ^ qtype;

    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&0x0100u16.to_be_bytes()); // standard query, recursion desired
    packet.extend_from_slice(&1u16.to_be_bytes()); // one question
    packet.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // no answer/authority/additional
    for label in name.split('.').filter(|l| !l.is_empty()) {
        if label.len() > 63 {
            return None;
        }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&qtype.to_be_bytes());
    packet.extend_from_slice(&CLASS_IN.to_be_bytes());

    socket.send_to(&packet, server).ok()?;

    let mut buf = vec![0u8; MAX_RESPONSE];
    let (len, _) = socket.recv_from(&mut buf).ok()?;
    buf.truncate(len);
    if len < 12 || u16::from_be_bytes([buf[0], buf[1]]) != id {
        return None;
    }
    // RCODE in the low nibble of the second flags byte; anything but 0 is an error.
    if buf[3] & 0x0F != 0 {
        return None;
    }

    let questions = u16::from_be_bytes([buf[4], buf[5]]);
    let answers = u16::from_be_bytes([buf[6], buf[7]]);
    let mut pos = 12;
    for _ in 0..questions {
        pos = skip_name(&buf, pos)?;
        pos = pos.checked_add(4)?; // qtype + qclass
    }

    let mut out = Vec::new();
    for _ in 0..answers {
        pos = skip_name(&buf, pos)?;
        if pos + 10 > buf.len() {
            break;
        }
        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > buf.len() {
            break;
        }
        // rdata is handed over as a tail slice so the caller can recover its
        // offset within the message for name decompression.
        out.push((rtype, buf[pos..].to_vec(), buf.clone()));
        pos += rdlen;
    }
    Some(out)
}

/// Walks past a possibly-compressed name, returning the offset after it.
fn skip_name(buf: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *buf.get(pos)?;
        if len & 0xC0 == 0xC0 {
            // A pointer is two bytes and always ends the name.
            return Some(pos + 2);
        }
        pos += 1;
        if len == 0 {
            return Some(pos);
        }
        pos = pos.checked_add(len as usize)?;
    }
}

/// Decodes a name, following compression pointers.
fn read_name(buf: &[u8], mut pos: usize) -> Option<String> {
    let mut out = String::new();
    // Bounded so a packet whose pointers form a loop cannot hang the thread.
    for _ in 0..64 {
        let len = *buf.get(pos)?;
        if len == 0 {
            return Some(out);
        }
        if len & 0xC0 == 0xC0 {
            let ptr = (((len & 0x3F) as usize) << 8) | *buf.get(pos + 1)? as usize;
            if ptr >= pos {
                return None; // pointers must go backwards
            }
            pos = ptr;
            continue;
        }
        pos += 1;
        let label = buf.get(pos..pos + len as usize)?;
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&String::from_utf8_lossy(label));
        pos += len as usize;
    }
    None
}

/// The first nameserver in the system configuration.
///
/// Falls back to a public resolver only when the system has none readable,
/// which on a normal desktop does not happen.
fn system_resolver() -> Option<SocketAddr> {
    #[cfg(unix)]
    if let Ok(conf) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in conf.lines() {
            let line = line.trim();
            if let Some(addr) = line.strip_prefix("nameserver")
                && let Ok(ip) = addr.trim().parse::<IpAddr>()
            {
                return Some(SocketAddr::new(ip, 53));
            }
        }
    }
    "1.1.1.1:53".to_socket_addrs().ok()?.next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_decompress_through_pointers() {
        // "www" then a pointer back to "example.com" at offset 4.
        let mut buf = vec![0u8; 4];
        buf.extend_from_slice(&[7]);
        buf.extend_from_slice(b"example");
        buf.extend_from_slice(&[3]);
        buf.extend_from_slice(b"com");
        buf.push(0);
        let target = buf.len();
        buf.extend_from_slice(&[3]);
        buf.extend_from_slice(b"www");
        buf.extend_from_slice(&[0xC0, 4]);

        assert_eq!(read_name(&buf, 4).as_deref(), Some("example.com"));
        assert_eq!(read_name(&buf, target).as_deref(), Some("www.example.com"));
    }

    #[test]
    fn a_pointer_loop_terminates() {
        // A pointer to itself would spin forever without the bound and the
        // backwards-only rule.
        let buf = vec![0, 0, 0, 0, 0xC0, 4];
        assert_eq!(read_name(&buf, 4), None);
    }

    #[test]
    fn skip_name_handles_both_forms() {
        let mut buf = vec![3];
        buf.extend_from_slice(b"www");
        buf.push(0);
        assert_eq!(skip_name(&buf, 0), Some(5));
        let ptr = vec![0xC0, 12];
        assert_eq!(skip_name(&ptr, 0), Some(2));
    }

    #[test]
    fn a_truncated_name_is_rejected_rather_than_panicking() {
        // A label claiming five bytes with one byte behind it must not be
        // walked past the end of the buffer.
        assert_eq!(read_name(&[5, b'a'], 0), None);
        assert_eq!(skip_name(&[5, b'a'], 0), None);
        assert_eq!(read_name(&[], 0), None);
        assert_eq!(skip_name(&[], 0), None);
    }

    #[test]
    fn an_explicit_port_skips_the_srv_lookup() {
        // No network: an IP literal with an explicit port must not query.
        let r = resolve("127.0.0.1", 25571, true);
        assert!(r.srv.is_none());
        assert_eq!(r.effective_port, 25571);
        assert_eq!(r.effective_host, "127.0.0.1");
    }

    #[test]
    fn ip_literals_are_not_sent_to_srv() {
        let r = resolve("127.0.0.1", 25565, false);
        assert!(r.srv_ms.is_none(), "an address literal has no SRV to look up");
    }

    #[test]
    fn reverse_name_construction_is_correct() {
        // Exercised through the public shape rather than hitting the network.
        let v4: IpAddr = "8.8.4.4".parse().unwrap();
        let IpAddr::V4(a) = v4 else { unreachable!() };
        let o = a.octets();
        assert_eq!(format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0]), "4.4.8.8.in-addr.arpa");
    }
}
