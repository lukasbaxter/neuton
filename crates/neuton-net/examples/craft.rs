//! Puts a log into the crafting grid and reports what the server sends back.
//!
//! Crafting is the server's job: the client moves items into the grid and the
//! result arrives as a slot update on slot zero. If nothing turns up, either
//! the click never landed or the answer is not being read.
//!
//! Usage: cargo run -p neuton-net --example craft -- <host:port> <name> <from-slot>

use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let target = args.next().unwrap_or_else(|| "localhost:25599".into());
    let name = args.next().unwrap_or_else(|| "lukas".into());
    let from: i16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(38);
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(25565)),
        None => (target, 25565),
    };

    let session = neuton_auth::Session {
        profile: neuton_auth::Profile { uuid: 0, name },
        access_token: String::new(),
        refresh_token: String::new(),
        expires_at: u64::MAX,
    };
    let mut conn = neuton_net::Connection::join(&host, port, &session).expect("join");

    let mut state_id = 0;
    let mut source = None;
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_secs(3) {
        match conn.poll() {
            Ok(neuton_net::Event::Container { window, state_id: id, slots, .. }) if window == 0 => {
                state_id = id;
                source = slots.get(from as usize).cloned().flatten();
                println!("container: {} slots, slot {from} holds {:?}", slots.len(),
                    source.as_ref().map(|s: &neuton_net::items::Stack| (s.name, s.count)));
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("dropped while settling: {e}");
                std::process::exit(1);
            }
        }
    }
    if source.is_none() {
        eprintln!("slot {from} is empty; nothing to craft with");
        std::process::exit(1);
    }

    // Pick the stack up, then put one of it in the top left of the grid.
    conn.send_container_click(0, state_id, from, 0, 0).expect("pickup");
    conn.send_container_click(0, state_id, 1, 1, 0).expect("place one");

    let waited = Instant::now();
    let mut result = None;
    while waited.elapsed() < Duration::from_secs(3) {
        match conn.poll() {
            Ok(neuton_net::Event::Slot { window, slot, stack, .. }) if window == 0 => {
                println!("slot {slot} -> {:?}", stack.as_ref().map(|s| (s.name, s.count)));
                if slot == 0 {
                    result = stack;
                }
            }
            Ok(neuton_net::Event::Container { window, slots, .. }) if window == 0 => {
                println!(
                    "container resync: output {:?}, grid {:?}",
                    slots.first().and_then(|s| s.as_ref()).map(|s| (s.name, s.count)),
                    slots.get(1).and_then(|s| s.as_ref()).map(|s| (s.name, s.count)),
                );
                if let Some(Some(stack)) = slots.first() {
                    result = Some(stack.clone());
                }
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("dropped: {e}");
                std::process::exit(1);
            }
        }
    }
    match result {
        Some(stack) => println!("crafting output: {} x{}", stack.name, stack.count),
        None => println!("crafting output: nothing"),
    }
}
