//! How long each block takes to break, extracted from the jar.
//!
//! None of this is in the vanilla data reports. Hardness lives in code, so a
//! small Java program is compiled against the jar and asked; the tool a block
//! wants is a tag, and the built-in tags ship inside the jar as JSON.

use crate::{Ctx, emit};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

type Err = Box<dyn std::error::Error>;

/// Runs the extractor, compiling it first. Returns hardness and whether a tool
/// is required, per block state ID.
fn hardness(ctx: &Ctx, java: &Path, classpath: &str) -> Result<Vec<(f32, bool)>, Err> {
    let source = ctx.repo.join("crates/neuton-datagen/java/DumpHardness.java");
    let classes = ctx.repo.join("target/datagen/classes");
    std::fs::create_dir_all(&classes)?;

    let javac = java.with_file_name("javac");
    let status = Command::new(&javac)
        .args(["-nowarn", "-cp", classpath, "-d"])
        .arg(&classes)
        .arg(&source)
        .status()?;
    if !status.success() {
        return Err(format!("compiling {} failed", source.display()).into());
    }

    let separator = if cfg!(windows) { ';' } else { ':' };
    let dumped = ctx.repo.join("target/datagen/hardness.json");
    let status = Command::new(java)
        .arg("-XX:+UseSerialGC")
        .arg("-cp")
        .arg(format!("{}{separator}{classpath}", classes.display()))
        .arg("DumpHardness")
        .arg(&dumped)
        .stdout(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err("the hardness extractor failed".into());
    }
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&dumped)?)?;

    let mut out = vec![(0.0f32, false); entries.len()];
    for entry in &entries {
        let id = entry["id"].as_u64().ok_or("entry with no id")? as usize;
        if id >= out.len() {
            return Err("state id out of range".into());
        }
        out[id] = (
            entry["hardness"].as_f64().unwrap_or(0.0) as f32,
            entry["needs_tool"].as_bool().unwrap_or(false),
        );
    }
    Ok(out)
}

/// Resolves one block tag out of the jar's built-in data pack, following the
/// `#other/tag` references it may contain.
fn tag(jar: &mut zip::ZipArchive<std::fs::File>, name: &str, seen: &mut BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if !seen.insert(name.to_string()) {
        return out;
    }
    let path = format!("data/minecraft/tags/block/{name}.json");
    let Ok(mut file) = jar.by_name(&path) else { return out };
    let mut text = String::new();
    if std::io::Read::read_to_string(&mut file, &mut text).is_err() {
        return out;
    }
    drop(file);
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else { return out };
    let Some(values) = doc.get("values").and_then(|v| v.as_array()) else { return out };
    for value in values {
        // An entry is either a block, or a reference to another tag, and may be
        // wrapped in an object when it is allowed to be missing.
        let id = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        };
        if let Some(nested) = id.strip_prefix('#') {
            let nested = nested.strip_prefix("minecraft:").unwrap_or(nested);
            out.extend(tag(jar, nested, seen));
        } else {
            out.insert(id.strip_prefix("minecraft:").unwrap_or(&id).to_string());
        }
    }
    out
}

pub fn generate(ctx: &Ctx, java: &Path, classpath: &str, jar_path: &Path) -> Result<(), Err> {
    let hardness = hardness(ctx, java, classpath)?;

    let mut jar = zip::ZipArchive::new(std::fs::File::open(jar_path)?)?;
    let mut tool_of: BTreeMap<String, &'static str> = BTreeMap::new();
    for (name, tool) in [
        ("mineable/pickaxe", "Pickaxe"),
        ("mineable/axe", "Axe"),
        ("mineable/shovel", "Shovel"),
        ("mineable/hoe", "Hoe"),
    ] {
        for block in tag(&mut jar, name, &mut BTreeSet::new()) {
            tool_of.insert(block, tool);
        }
    }

    // Blocks are laid out in consecutive state runs, so one lookup per block
    // covers all of its states.
    let raw = std::fs::read_to_string(ctx.reports.join("blocks.json"))?;
    let blocks: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw)?;
    let mut tool_by_state = vec!["None"; hardness.len()];
    for (name, value) in &blocks {
        let short = name.strip_prefix("minecraft:").unwrap_or(name);
        let Some(tool) = tool_of.get(short) else { continue };
        for state in value["states"].as_array().into_iter().flatten() {
            if let Some(id) = state["id"].as_u64()
                && (id as usize) < tool_by_state.len()
            {
                tool_by_state[id as usize] = tool;
            }
        }
    }

    let mut out = String::with_capacity(1 << 20);
    writeln!(
        out,
        "//! Generated by neuton-datagen from Minecraft {}. Do not edit.\n",
        ctx.version
    )?;
    out.push_str(
        "/// The kind of tool a block gives way to fastest.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         pub enum Tool {\n    None,\n    Pickaxe,\n    Axe,\n    Shovel,\n    Hoe,\n}\n\n\
         /// What it takes to break one block state.\n\
         #[derive(Debug, Clone, Copy)]\n\
         pub struct Breaking {\n    \
             /// Seconds to break bare handed with nothing helping. Negative for\n    \
             /// blocks that cannot be broken at all.\n    \
             pub hardness: f32,\n    \
             /// Whether the right tool is needed for the block to drop anything,\n    \
             /// and to break at the faster rate.\n    \
             pub needs_tool: bool,\n    \
             pub tool: Tool,\n\
         }\n\n",
    );
    writeln!(out, "pub const BREAKING: &[Breaking] = &[")?;
    for (id, (hard, needs)) in hardness.iter().enumerate() {
        writeln!(
            out,
            "    Breaking {{ hardness: {hard:?}, needs_tool: {needs}, tool: Tool::{} }},",
            tool_by_state[id]
        )?;
    }
    writeln!(out, "];\n")?;
    out.push_str(
        "/// What it takes to break this state, or an unbreakable stand-in for an\n\
         /// ID the tables do not know.\n\
         pub fn breaking(state: u32) -> Breaking {\n    \
             match BREAKING.get(state as usize) {\n        \
                 Some(b) => *b,\n        \
                 None => Breaking { hardness: -1.0, needs_tool: true, tool: Tool::None },\n    \
             }\n\
         }\n",
    );

    println!("  breaking      : {} states, {} blocks want a tool", hardness.len(), tool_of.len());
    emit(&ctx.repo.join("crates/neuton-blocks/src/generated/breaking.rs"), &out)?;
    Ok(())
}
