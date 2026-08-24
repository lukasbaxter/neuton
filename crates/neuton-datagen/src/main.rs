//! Build-time extraction of vanilla data into compiled-in Rust tables.
//!
//! The vanilla client spends a large slice of its startup parsing JSON that
//! never changes between runs: registries, block states, packet IDs, block
//! models. We do that work once, here, and commit the result as `.rs` source.
//! At runtime the client just reads static memory.
//!
//! Usage:
//!   cargo run -p neuton-datagen -- [--mc-dir <dir>] [--java <path>]

mod blocks;
mod packets;
mod paths;

use std::path::PathBuf;
use std::process::ExitCode;

pub struct Ctx {
    /// Where `reports/*.json` were written by the vanilla generator.
    pub reports: PathBuf,
    /// Repository root, so generators can write into sibling crates.
    pub repo: PathBuf,
    /// Version id, e.g. "26.2".
    pub version: String,
    /// Protocol number from the jar's `version.json`.
    pub protocol: i64,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("datagen: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut mc_dir = None;
    let mut java = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mc-dir" => {
                mc_dir = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--java" => {
                java = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }

    let env = paths::Env::resolve(mc_dir, java)?;
    println!("  minecraft dir : {}", env.mc_dir.display());
    println!("  version       : {} (protocol {})", env.version, env.protocol);
    println!("  java          : {}", env.java.display());

    let reports = env.run_vanilla_datagen()?;
    println!("  reports       : {}", reports.display());

    let ctx = Ctx {
        reports,
        repo: env.repo.clone(),
        version: env.version.clone(),
        protocol: env.protocol,
    };

    packets::generate(&ctx)?;
    blocks::generate(&ctx)?;

    println!("datagen: done");
    Ok(())
}

/// Writes `contents` to `path`, creating parents, and reports what changed.
pub fn emit(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let changed = std::fs::read_to_string(path).map(|old| old != contents).unwrap_or(true);
    if changed {
        std::fs::write(path, contents)?;
    }
    let rel = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    println!(
        "  generated     : {rel} ({} lines){}",
        contents.lines().count(),
        if changed { "" } else { " [unchanged]" }
    );
    Ok(())
}

/// Turns a registry name into a Rust constant name:
/// `minecraft:oak_stairs` -> `OAK_STAIRS`, `minecraft:debug/block_value` ->
/// `DEBUG_BLOCK_VALUE`.
pub fn const_name(id: &str) -> String {
    let tail = id.rsplit(':').next().unwrap_or(id);
    let mut out: String = tail
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
        .collect();
    // A Rust identifier cannot start with a digit.
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}
