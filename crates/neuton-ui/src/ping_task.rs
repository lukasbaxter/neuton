//! Pinging the server list in the background.
//!
//! Every row is pinged on its own thread. A server that is down blocks for the
//! whole connect timeout, and doing that in sequence would leave the list half
//! blank for half a minute, so they all go at once and rows fill in as replies
//! land.

use neuton_net::ServerStatus;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

/// What is known about one row right now.
#[derive(Debug, Clone, Default)]
pub enum Ping {
    #[default]
    Unknown,
    Pending,
    Ok(Box<ServerStatus>),
    Failed(String),
}

impl Ping {
    pub fn is_pending(&self) -> bool {
        matches!(self, Ping::Pending)
    }
}

pub struct Pinger {
    tx: Sender<(u64, Ping)>,
    rx: Receiver<(u64, Ping)>,
    results: HashMap<u64, Ping>,
}

impl Default for Pinger {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, results: HashMap::new() }
    }
}

impl Pinger {
    pub fn state(&self, id: u64) -> &Ping {
        self.results.get(&id).unwrap_or(&Ping::Unknown)
    }

    pub fn any_pending(&self) -> bool {
        self.results.values().any(Ping::is_pending)
    }

    /// Pings one server. Ignored if that row is already in flight.
    pub fn refresh_one(&mut self, id: u64, host: String, port: u16) {
        if self.state(id).is_pending() {
            return;
        }
        self.results.insert(id, Ping::Pending);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = match neuton_net::ping(&host, port) {
                Ok(status) => Ping::Ok(Box::new(status)),
                Err(e) => Ping::Failed(short_reason(&e)),
            };
            // The receiver is gone if the window closed mid-ping, which is fine.
            let _ = tx.send((id, result));
        });
    }

    pub fn refresh_all(&mut self, servers: &[crate::servers::Server]) {
        for s in servers {
            let (host, port) = s.host_port();
            if !host.is_empty() {
                self.refresh_one(s.id, host, port);
            }
        }
    }

    /// Drops state for rows that no longer exist.
    pub fn retain(&mut self, servers: &[crate::servers::Server]) {
        self.results.retain(|id, _| servers.iter().any(|s| s.id == *id));
    }

    /// Drains replies. Returns true if anything changed and a repaint is due.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok((id, state)) => {
                    self.results.insert(id, state);
                    changed = true;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        changed
    }
}

/// Turns an io error into something that fits in a table row.
fn short_reason(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        "timed out".into()
    } else if lower.contains("refused") {
        "connection refused".into()
    } else if lower.contains("nodename") || lower.contains("not known") || lower.contains("resolve")
    {
        "host not found".into()
    } else if lower.contains("unreachable") {
        "unreachable".into()
    } else {
        raw.split('\n').next().unwrap_or(raw).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::ServerList;

    #[test]
    fn unknown_rows_report_unknown() {
        let p = Pinger::default();
        assert!(matches!(p.state(42), Ping::Unknown));
        assert!(!p.any_pending());
    }

    #[test]
    fn a_dead_address_resolves_to_a_short_failure() {
        let mut p = Pinger::default();
        // Port 1 on loopback refuses immediately, so this does not wait on a
        // timeout.
        p.refresh_one(1, "127.0.0.1".into(), 1);
        assert!(p.state(1).is_pending());

        for _ in 0..100 {
            if p.poll() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        match p.state(1) {
            Ping::Failed(why) => assert!(!why.is_empty()),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn results_for_removed_rows_are_dropped() {
        let mut list = ServerList::load(std::env::temp_dir().join("neuton-pinger-test.json"));
        let a = list.add("a", "127.0.0.1");
        let b = list.add("b", "127.0.0.1");
        let mut p = Pinger::default();
        p.results.insert(a, Ping::Unknown);
        p.results.insert(b, Ping::Unknown);

        list.remove(b);
        p.retain(list.entries());
        assert!(p.results.contains_key(&a));
        assert!(!p.results.contains_key(&b));
    }

    #[test]
    fn common_io_errors_become_readable() {
        assert_eq!(short_reason("Connection refused (os error 61)"), "connection refused");
        assert_eq!(short_reason("connection timed out"), "timed out");
        assert_eq!(short_reason("nodename nor servname provided"), "host not found");
    }
}
