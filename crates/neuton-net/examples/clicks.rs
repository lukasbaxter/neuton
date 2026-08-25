//! Fires one container click of every mode at a real server and reports
//! whether the connection survived each one.
//!
//! A click that the server cannot decode is not an error it replies to: it
//! closes the connection. So the only honest test of the packet's shape is to
//! send it to a server and see whether we are still there afterwards.
//!
//! Usage: cargo run -p neuton-net --example clicks -- <host:port> <name>

use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let target = args.next().unwrap_or_else(|| "localhost:25599".into());
    let name = args.next().unwrap_or_else(|| "clicktest".into());
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
    let mut conn = match neuton_net::Connection::join(&host, port, &session) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not join: {e}");
            std::process::exit(1);
        }
    };

    // Let the world arrive, so the container we are clicking exists.
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_secs(3) {
        if conn.poll().is_err() {
            eprintln!("dropped while settling");
            std::process::exit(1);
        }
    }

    // Every mode, with a button the game would really send for it. Slot nine
    // is the first backpack slot; -999 is away from the window.
    let clicks: [(&str, i16, u8, i32); 10] = [
        ("pickup left", 9, 0, 0),
        ("pickup right", 9, 1, 0),
        ("drop carried", -999, 0, 0),
        ("quick move", 9, 0, 1),
        ("hotbar swap", 9, 2, 2),
        ("clone", 9, 2, 3),
        ("throw one", 9, 0, 4),
        ("drag start", -999, 0, 5),
        ("drag add", 9, 1, 5),
        ("drag end", -999, 2, 5),
    ];
    let mut failed = false;
    for (what, slot, button, mode) in clicks {
        if let Err(e) = conn.send_container_click(0, 0, slot, button, mode) {
            println!("{what:>14}: could not send: {e}");
            failed = true;
            break;
        }
        // Anything the server says back, for long enough that a kick for this
        // click would have arrived.
        let waited = Instant::now();
        let mut alive = true;
        while waited.elapsed() < Duration::from_millis(400) {
            if let Err(e) = conn.poll() {
                println!("{what:>14}: DROPPED -- {e}");
                alive = false;
                failed = true;
                break;
            }
        }
        if !alive {
            break;
        }
        println!("{what:>14}: still connected");
    }
    // Gather is last: it needs the cursor to hold something, and by here it
    // does not, but the packet's shape is what is being checked.
    if !failed {
        let _ = conn.send_container_click(0, 0, 9, 0, 6);
        let waited = Instant::now();
        while waited.elapsed() < Duration::from_millis(400) {
            if let Err(e) = conn.poll() {
                println!("{:>14}: DROPPED -- {e}", "gather");
                std::process::exit(1);
            }
        }
        println!("{:>14}: still connected", "gather");
    }
    if failed {
        std::process::exit(1);
    }
    println!("every click mode decoded");
}
