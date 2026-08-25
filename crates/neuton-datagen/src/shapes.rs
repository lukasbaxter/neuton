//! The boxes a block occupies, extracted from the jar.
//!
//! Not in the vanilla data reports, and not derivable from the render model
//! either. The game keeps these in code and keeps two of them: a fence is
//! drawn one block tall, outlined one block tall, and walked into one and a
//! half, so that it cannot be jumped. Reading the boxes off the model gets
//! that wrong, and gets thin blocks wrong the other way: an end rod's model
//! narrows to two pixels above its base while the box you collide with stays
//! four pixels the whole way up.

use crate::{Ctx, emit};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

type Err = Box<dyn std::error::Error>;

/// One block's boxes, in 0..1 block space.
type Boxes = Vec<[f64; 6]>;

/// Runs the extractor, compiling it first. Returns the collision boxes and the
/// outline boxes, per block state ID.
fn dump(ctx: &Ctx, java: &Path, classpath: &str) -> Result<Vec<(Boxes, Boxes)>, Err> {
    let source = ctx.repo.join("crates/neuton-datagen/java/DumpCollision.java");
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
    let dumped = ctx.repo.join("target/datagen/shapes.json");
    let status = Command::new(java)
        .arg("-XX:+UseSerialGC")
        .arg("-cp")
        .arg(format!("{}{separator}{classpath}", classes.display()))
        .arg("DumpCollision")
        .arg(&dumped)
        .stdout(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err("the shape extractor failed".into());
    }
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&dumped)?)?;

    let read = |value: &serde_json::Value| -> Boxes {
        value
            .as_array()
            .map(|boxes| {
                boxes
                    .iter()
                    .filter_map(|b| {
                        let n = b.as_array()?;
                        if n.len() != 6 {
                            return None;
                        }
                        let mut out = [0.0; 6];
                        for (slot, value) in out.iter_mut().zip(n) {
                            *slot = value.as_f64()?;
                        }
                        Some(out)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut out = vec![(Boxes::new(), Boxes::new()); entries.len()];
    for entry in &entries {
        let id = entry["id"].as_u64().ok_or("entry with no id")? as usize;
        if id >= out.len() {
            return Err("state id out of range".into());
        }
        out[id] = (read(&entry["collision"]), read(&entry["outline"]));
    }
    Ok(out)
}

/// Collapses the per-state boxes onto the distinct sets, since most of the
/// thirty-two thousand states are one of a few hundred shapes.
fn dedupe(all: impl Iterator<Item = Boxes>) -> (Vec<Boxes>, Vec<u16>) {
    let mut shapes: Vec<Boxes> = Vec::new();
    let mut seen: BTreeMap<String, u16> = BTreeMap::new();
    let mut index = Vec::new();
    for boxes in all {
        let key = format!("{boxes:?}");
        let slot = *seen.entry(key).or_insert_with(|| {
            shapes.push(boxes);
            (shapes.len() - 1) as u16
        });
        index.push(slot);
    }
    (shapes, index)
}

fn write_shapes(out: &mut String, name: &str, doc: &str, shapes: &[Boxes]) {
    let _ = writeln!(out, "{doc}");
    let _ = writeln!(out, "pub static {name}: [&[Aabb]; {}] = [", shapes.len());
    for boxes in shapes {
        let _ = write!(out, "    &[");
        for b in boxes {
            let _ = write!(
                out,
                "Aabb {{ min: [{:?}, {:?}, {:?}], max: [{:?}, {:?}, {:?}] }},",
                b[0], b[1], b[2], b[3], b[4], b[5]
            );
        }
        let _ = writeln!(out, "],");
    }
    let _ = writeln!(out, "];");
}

fn write_index(out: &mut String, name: &str, doc: &str, index: &[u16]) {
    let _ = writeln!(out, "{doc}");
    let _ = writeln!(out, "static {name}: [u16; {}] = [", index.len());
    for chunk in index.chunks(32) {
        let _ = write!(out, "    ");
        for slot in chunk {
            let _ = write!(out, "{slot},");
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "];");
}

pub fn generate(ctx: &Ctx, java: &Path, classpath: &str) -> Result<(), Err> {
    let dumped = dump(ctx, java, classpath)?;
    let (collision, collision_index) = dedupe(dumped.iter().map(|(c, _)| c.clone()));
    let (outline, outline_index) = dedupe(dumped.iter().map(|(_, o)| o.clone()));

    let mut out = String::new();
    let _ = writeln!(
        out,
        "// @generated by neuton-datagen from the vanilla {} jar\n\
         // Do not edit. Run `cargo run -p neuton-datagen` to regenerate.\n\
         //!\n\
         //! The boxes a block occupies. Two sets, because the game keeps two:\n\
         //! what a player walks into, and what the crosshair picks and the\n\
         //! selection box is drawn around. They differ for more than half of\n\
         //! all block states, most visibly for fences and walls, whose\n\
         //! collision stands half a block taller than anything drawn.\n\
         #![allow(dead_code)]\n\
         \n\
         use crate::physics::Aabb;\n\
         use neuton_blocks::StateId;\n",
        ctx.version
    );

    write_shapes(
        &mut out,
        "COLLISION_SHAPES",
        "/// Every distinct set of boxes a player can walk into.",
        &collision,
    );
    let _ = writeln!(out);
    write_shapes(
        &mut out,
        "OUTLINE_SHAPES",
        "/// Every distinct set of boxes the crosshair picks against.",
        &outline,
    );
    let _ = writeln!(out);
    write_index(
        &mut out,
        "STATE_COLLISION",
        "/// Which collision shape each state uses.",
        &collision_index,
    );
    let _ = writeln!(out);
    write_index(
        &mut out,
        "STATE_OUTLINE",
        "/// Which outline shape each state uses.",
        &outline_index,
    );

    let _ = writeln!(
        out,
        "\n/// The boxes a player collides with. Empty for anything walked\n\
         /// through, which is most plants and all air.\n\
         #[inline]\n\
         pub fn collision(state: StateId) -> &'static [Aabb] {{\n\
         \x20   match STATE_COLLISION.get(state.0 as usize) {{\n\
         \x20       Some(&slot) => COLLISION_SHAPES[slot as usize],\n\
         \x20       None => &[],\n\
         \x20   }}\n\
         }}\n\
         \n\
         /// The boxes the crosshair picks against, and the selection box is\n\
         /// drawn around.\n\
         #[inline]\n\
         pub fn outline(state: StateId) -> &'static [Aabb] {{\n\
         \x20   match STATE_OUTLINE.get(state.0 as usize) {{\n\
         \x20       Some(&slot) => OUTLINE_SHAPES[slot as usize],\n\
         \x20       None => &[],\n\
         \x20   }}\n\
         }}"
    );

    emit(&ctx.repo.join("crates/neuton-world/src/generated/shapes.rs"), &out)?;
    Ok(())
}
