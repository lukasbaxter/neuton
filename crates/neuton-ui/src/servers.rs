//! The saved server list.
//!
//! Kept next to the account store, in the same plain JSON. Nothing here is
//! secret, so unlike `accounts.json` it needs no special permissions.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_PORT: u16 = 25565;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Server {
    /// Stable across edits and reorders, so an in-flight ping result can still
    /// find the row it belongs to after the user renames or moves it.
    pub id: u64,
    pub name: String,
    /// As typed: `host` or `host:port`.
    pub address: String,
}

impl Server {
    /// Splits the address, defaulting the port.
    ///
    /// A bare IPv6 literal is full of colons, so only the bracketed form
    /// `[::1]:25565` is treated as carrying a port. Anything with more than one
    /// colon and no brackets is taken as a host on the default port.
    pub fn host_port(&self) -> (String, u16) {
        let addr = self.address.trim();

        if let Some(rest) = addr.strip_prefix('[') {
            return match rest.split_once(']') {
                Some((host, tail)) => {
                    let port = tail
                        .strip_prefix(':')
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(DEFAULT_PORT);
                    (host.to_string(), port)
                }
                None => (rest.to_string(), DEFAULT_PORT),
            };
        }

        if addr.matches(':').count() > 1 {
            return (addr.to_string(), DEFAULT_PORT);
        }

        match addr.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() && !port.is_empty() => {
                match port.parse() {
                    Ok(p) => (host.to_string(), p),
                    // "example.com:abc" is a typo, not a port. Keep the host so
                    // the row still says something useful.
                    Err(_) => (host.to_string(), DEFAULT_PORT),
                }
            }
            _ => (addr.to_string(), DEFAULT_PORT),
        }
    }

    /// What to show when the entry has no name yet.
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() { self.address.trim() } else { self.name.trim() }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ServerList {
    #[serde(default)]
    servers: Vec<Server>,
    #[serde(default)]
    next_id: u64,
    #[serde(skip)]
    path: PathBuf,
}

impl ServerList {
    pub fn default_path() -> PathBuf {
        neuton_auth::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("servers.json")
    }

    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut list: ServerList = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        // A hand-edited file could repeat or omit ids; make them sane rather
        // than letting two rows share ping results.
        let mut seen = std::collections::HashSet::new();
        let mut next = list.next_id;
        for s in &mut list.servers {
            if s.id == 0 || !seen.insert(s.id) {
                next += 1;
                s.id = next;
                seen.insert(s.id);
            }
            next = next.max(s.id);
        }
        list.next_id = next;
        list.path = path;
        list
    }

    pub fn load_default() -> Self {
        Self::load(Self::default_path())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)
    }

    pub fn entries(&self) -> &[Server] {
        &self.servers
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// Adds a server and returns its id.
    pub fn add(&mut self, name: impl Into<String>, address: impl Into<String>) -> u64 {
        self.next_id += 1;
        self.servers.push(Server {
            id: self.next_id,
            name: name.into(),
            address: address.into(),
        });
        self.next_id
    }

    pub fn get(&self, id: u64) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    /// Applies an edit in place. Returns false if the id is gone.
    pub fn edit(&mut self, id: u64, name: impl Into<String>, address: impl Into<String>) -> bool {
        match self.servers.iter_mut().find(|s| s.id == id) {
            Some(s) => {
                s.name = name.into();
                s.address = address.into();
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.servers.len();
        self.servers.retain(|s| s.id != id);
        self.servers.len() != before
    }

    /// Moves an entry one place towards the top. No-op at the top.
    pub fn move_up(&mut self, id: u64) {
        if let Some(i) = self.servers.iter().position(|s| s.id == id)
            && i > 0
        {
            self.servers.swap(i - 1, i);
        }
    }

    pub fn move_down(&mut self, id: u64) {
        if let Some(i) = self.servers.iter().position(|s| s.id == id)
            && i + 1 < self.servers.len()
        {
            self.servers.swap(i, i + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("neuton-srv-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("servers.json")
    }

    fn entry(address: &str) -> Server {
        Server { id: 1, name: String::new(), address: address.into() }
    }

    #[test]
    fn address_splits_into_host_and_port() {
        assert_eq!(entry("play.example.com").host_port(), ("play.example.com".into(), 25565));
        assert_eq!(entry("play.example.com:25571").host_port(), ("play.example.com".into(), 25571));
        assert_eq!(entry("  10.0.0.5:1234 ").host_port(), ("10.0.0.5".into(), 1234));
    }

    #[test]
    fn ipv6_literals_keep_all_their_colons() {
        // Splitting on the last colon would give host "fe80:" port 1.
        assert_eq!(entry("fe80::1").host_port(), ("fe80::1".into(), 25565));
        assert_eq!(entry("::1").host_port(), ("::1".into(), 25565));
        // The bracketed form does carry a port.
        assert_eq!(entry("[fe80::1]:25571").host_port(), ("fe80::1".into(), 25571));
        assert_eq!(entry("[::1]").host_port(), ("::1".into(), 25565));
    }

    #[test]
    fn a_typo_in_the_port_keeps_the_host() {
        assert_eq!(entry("play.example.com:abc").host_port(), ("play.example.com".into(), 25565));
    }

    #[test]
    fn unnamed_entries_fall_back_to_their_address() {
        assert_eq!(entry("play.example.com").display_name(), "play.example.com");
        let named = Server { id: 1, name: "Miji".into(), address: "x".into() };
        assert_eq!(named.display_name(), "Miji");
    }

    #[test]
    fn add_edit_remove_round_trip_through_disk() {
        let path = temp("crud");
        let mut list = ServerList::load(&path);
        let a = list.add("Miji", "play.notmiji.com");
        let b = list.add("Dev", "192.168.1.85:25571");
        list.save().unwrap();

        let mut back = ServerList::load(&path);
        assert_eq!(back.len(), 2);
        assert_eq!(back.get(a).unwrap().name, "Miji");

        assert!(back.edit(b, "Dev server", "192.168.1.85:25571"));
        assert!(!back.edit(9999, "x", "y"));
        assert!(back.remove(a));
        assert!(!back.remove(a));
        back.save().unwrap();

        let final_list = ServerList::load(&path);
        assert_eq!(final_list.len(), 1);
        assert_eq!(final_list.entries()[0].name, "Dev server");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ids_are_not_reused_after_removal() {
        let mut list = ServerList::load(temp("ids"));
        let a = list.add("a", "a");
        list.remove(a);
        let b = list.add("b", "b");
        assert_ne!(a, b, "a stale ping result must not land on a new row");
    }

    #[test]
    fn reordering_moves_one_place_and_stops_at_the_ends() {
        let mut list = ServerList::load(temp("order"));
        let a = list.add("a", "a");
        let b = list.add("b", "b");
        list.move_up(b);
        assert_eq!(list.entries()[0].id, b);
        list.move_up(b); // already top
        assert_eq!(list.entries()[0].id, b);
        list.move_down(b);
        assert_eq!(list.entries()[0].id, a);
        list.move_down(b); // already bottom
        assert_eq!(list.entries()[1].id, b);
    }

    #[test]
    fn a_hand_edited_file_with_duplicate_ids_is_repaired() {
        let path = temp("dupe");
        std::fs::write(
            &path,
            r#"{"servers":[{"id":5,"name":"a","address":"a"},{"id":5,"name":"b","address":"b"}]}"#,
        )
        .unwrap();
        let list = ServerList::load(&path);
        assert_ne!(list.entries()[0].id, list.entries()[1].id);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
