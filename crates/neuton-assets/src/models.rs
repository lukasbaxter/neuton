//! Resolving a block state to the six textures that cover it.
//!
//! The path is the one Minecraft itself walks: a blockstate file maps a
//! property string to a model, a model inherits from a parent chain, and the
//! textures it declares are `#references` into that chain until they bottom out
//! at a real texture path.
//!
//! Only full cubes are handled properly. Stairs, slabs and everything with
//! geometry of its own need the model's `elements` baked, which is a later
//! piece of work; for now they take their model's textures on all six faces,
//! which is wrong in shape but right in colour.

use crate::PackStack;
use std::collections::HashMap;

/// Texture paths for the six faces of a block, in [`Face`] order.
///
/// Paths are pack-relative and already have the `minecraft:` namespace and the
/// `.png` suffix applied, so they can be handed straight to the atlas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceTextures {
    /// down, up, north, south, west, east
    pub faces: [String; 6],
}

impl FaceTextures {
    pub fn all(texture: &str) -> Self {
        Self { faces: std::array::from_fn(|_| texture.to_string()) }
    }

    /// Every distinct texture used, for atlas collection.
    pub fn distinct(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.faces.iter().map(String::as_str).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Caches parsed models so a parent chain is walked once, not once per block.
pub struct ModelResolver {
    models: HashMap<String, serde_json::Value>,
    blockstates: HashMap<String, serde_json::Value>,
}

impl Default for ModelResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelResolver {
    pub fn new() -> Self {
        Self { models: HashMap::new(), blockstates: HashMap::new() }
    }

    /// Resolves the textures for one block state.
    ///
    /// `block` is a namespaced name such as `minecraft:oak_log`, `variant` the
    /// property string from the block state, such as `axis=y`.
    pub fn textures(
        &mut self,
        packs: &mut PackStack,
        block: &str,
        variant: &str,
    ) -> Option<FaceTextures> {
        let (namespace, name) = split(block);
        let path = format!("assets/{namespace}/blockstates/{name}.json");
        if !self.blockstates.contains_key(&path) {
            let value = packs.read_json(&path)?;
            self.blockstates.insert(path.clone(), value);
        }
        let state = self.blockstates.get(&path)?.clone();

        let model = pick_model(&state, variant)?;
        let textures = self.resolve_chain(packs, &model)?;
        Some(face_textures(&textures))
    }

    /// Walks a model's parent chain, collecting and resolving its textures.
    ///
    /// A child's declarations win over its parent's, and `#name` references are
    /// followed afterwards so a child can redirect a variable the parent
    /// defined.
    fn resolve_chain(
        &mut self,
        packs: &mut PackStack,
        model: &str,
    ) -> Option<HashMap<String, String>> {
        let mut merged: HashMap<String, String> = HashMap::new();
        let mut current = Some(model.to_string());
        // Bounded: a pack with a cyclic parent chain must not hang the loader.
        for _ in 0..32 {
            let Some(name) = current.take() else { break };
            let (namespace, path) = split(&name);
            let file = format!("assets/{namespace}/models/{path}.json");

            if !self.models.contains_key(&file) {
                let value = packs.read_json(&file)?;
                self.models.insert(file.clone(), value);
            }
            let value = self.models.get(&file)?;

            if let Some(textures) = value.get("textures").and_then(|t| t.as_object()) {
                for (key, v) in textures {
                    if let Some(s) = v.as_str() {
                        // The child was inserted first and must not be
                        // overwritten by the parent's version of the same key.
                        merged.entry(key.clone()).or_insert_with(|| s.to_string());
                    }
                }
            }
            current = value.get("parent").and_then(|p| p.as_str()).map(str::to_string);
        }

        // Follow #references until they land on a real path.
        let keys: Vec<String> = merged.keys().cloned().collect();
        for key in keys {
            let mut value = merged.get(&key).cloned()?;
            for _ in 0..16 {
                let Some(reference) = value.strip_prefix('#') else { break };
                match merged.get(reference) {
                    Some(next) if next != &value => value = next.clone(),
                    // Unresolved or self-referential; leave it and let the
                    // caller fall back.
                    _ => break,
                }
            }
            merged.insert(key, value);
        }
        Some(merged)
    }
}

/// Chooses a model from a blockstate file for the given property string.
///
/// Falls back to any variant rather than nothing: a block whose properties do
/// not match a key still needs to draw as something.
fn pick_model(state: &serde_json::Value, variant: &str) -> Option<String> {
    if let Some(variants) = state.get("variants").and_then(|v| v.as_object()) {
        let chosen = variants
            .get(variant)
            .or_else(|| variants.get(""))
            .or_else(|| variants.values().next())?;
        return model_name(chosen);
    }
    // Multipart blocks (fences, walls, redstone) describe themselves as a set
    // of conditional pieces. Taking the first is wrong in shape but gives the
    // right textures, which is all this stage uses.
    if let Some(parts) = state.get("multipart").and_then(|m| m.as_array()) {
        for part in parts {
            if let Some(name) = part.get("apply").and_then(model_name) {
                return Some(name);
            }
        }
    }
    None
}

/// A variant entry is a model object, or a weighted list of them.
fn model_name(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Array(items) => items.first().and_then(model_name),
        serde_json::Value::Object(_) => {
            value.get("model").and_then(|m| m.as_str()).map(str::to_string)
        }
        _ => None,
    }
}

/// Picks a texture per face from a resolved texture map.
///
/// Cube models name their faces directly. Anything else falls back through the
/// conventional keys and finally to `particle`, which every model has and which
/// vanilla itself uses as the stand-in colour for a block.
fn face_textures(textures: &HashMap<String, String>) -> FaceTextures {
    let get = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| textures.get(*k)).filter(|v| !v.starts_with('#')).cloned()
    };
    let fallback = get(&["particle", "all", "texture", "side", "end", "cross", "top"])
        .unwrap_or_else(|| "minecraft:block/stone".to_string());

    let face = |keys: &[&str]| texture_path(&get(keys).unwrap_or_else(|| fallback.clone()));
    FaceTextures {
        faces: [
            face(&["down", "bottom", "end", "all"]),
            face(&["up", "top", "end", "all"]),
            face(&["north", "side", "all"]),
            face(&["south", "side", "all"]),
            face(&["west", "side", "all"]),
            face(&["east", "side", "all"]),
        ],
    }
}

/// Turns `minecraft:block/stone` into its pack-relative file path.
fn texture_path(reference: &str) -> String {
    let (namespace, path) = split(reference);
    format!("assets/{namespace}/textures/{path}.png")
}

/// Splits a namespaced name, defaulting the namespace to `minecraft`.
fn split(name: &str) -> (&str, &str) {
    match name.split_once(':') {
        Some((ns, rest)) => (ns, rest),
        None => ("minecraft", name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pack(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PackStack) {
        let dir = std::env::temp_dir().join(format!("neuton-model-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, body) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let mut stack = PackStack::new();
        stack.push(&dir).unwrap();
        (dir, stack)
    }

    #[test]
    fn a_simple_cube_takes_one_texture_on_every_face() {
        let (dir, mut packs) = pack("cube", &[
            ("assets/minecraft/blockstates/stone.json", r##"{"variants":{"":{"model":"minecraft:block/stone"}}}"##),
            ("assets/minecraft/models/block/stone.json", r##"{"parent":"minecraft:block/cube_all","textures":{"all":"minecraft:block/stone"}}"##),
            ("assets/minecraft/models/block/cube_all.json", r##"{"textures":{"particle":"#all","down":"#all","up":"#all","north":"#all","east":"#all","south":"#all","west":"#all"}}"##),
        ]);
        let mut r = ModelResolver::new();
        let t = r.textures(&mut packs, "minecraft:stone", "").unwrap();
        assert_eq!(t.distinct(), vec!["assets/minecraft/textures/block/stone.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_column_gets_different_ends_and_sides() {
        let (dir, mut packs) = pack("column", &[
            ("assets/minecraft/blockstates/oak_log.json", r##"{"variants":{"axis=y":{"model":"minecraft:block/oak_log"},"axis=x":{"model":"minecraft:block/oak_log_horizontal"}}}"##),
            ("assets/minecraft/models/block/oak_log.json", r##"{"parent":"minecraft:block/cube_column","textures":{"end":"minecraft:block/oak_log_top","side":"minecraft:block/oak_log"}}"##),
            ("assets/minecraft/models/block/cube_column.json", r##"{"textures":{"particle":"#side","down":"#end","up":"#end","north":"#side","east":"#side","south":"#side","west":"#side"}}"##),
        ]);
        let mut r = ModelResolver::new();
        let t = r.textures(&mut packs, "minecraft:oak_log", "axis=y").unwrap();
        // down and up are the end texture, the four sides are not.
        assert_eq!(t.faces[0], "assets/minecraft/textures/block/oak_log_top.png");
        assert_eq!(t.faces[1], "assets/minecraft/textures/block/oak_log_top.png");
        assert_eq!(t.faces[2], "assets/minecraft/textures/block/oak_log.png");
        assert_eq!(t.distinct().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_overrides_a_texture_its_parent_declared() {
        let (dir, mut packs) = pack("override", &[
            ("assets/minecraft/blockstates/b.json", r##"{"variants":{"":{"model":"minecraft:block/child"}}}"##),
            ("assets/minecraft/models/block/child.json", r##"{"parent":"minecraft:block/base","textures":{"all":"minecraft:block/child_tex"}}"##),
            ("assets/minecraft/models/block/base.json", r##"{"textures":{"all":"minecraft:block/base_tex","up":"#all","down":"#all","north":"#all","south":"#all","east":"#all","west":"#all"}}"##),
        ]);
        let mut r = ModelResolver::new();
        let t = r.textures(&mut packs, "minecraft:b", "").unwrap();
        assert_eq!(t.distinct(), vec!["assets/minecraft/textures/block/child_tex.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cyclic_parent_chain_terminates() {
        let (dir, mut packs) = pack("cycle", &[
            ("assets/minecraft/blockstates/b.json", r##"{"variants":{"":{"model":"minecraft:block/a"}}}"##),
            ("assets/minecraft/models/block/a.json", r##"{"parent":"minecraft:block/b","textures":{"all":"minecraft:block/x"}}"##),
            ("assets/minecraft/models/block/b.json", r##"{"parent":"minecraft:block/a"}"##),
        ]);
        let mut r = ModelResolver::new();
        let t = r.textures(&mut packs, "minecraft:b", "").unwrap();
        assert_eq!(t.distinct(), vec!["assets/minecraft/textures/block/x.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_weighted_variant_list_takes_the_first() {
        let (dir, mut packs) = pack("weighted", &[
            ("assets/minecraft/blockstates/s.json", r##"{"variants":{"":[{"model":"minecraft:block/one"},{"model":"minecraft:block/two"}]}}"##),
            ("assets/minecraft/models/block/one.json", r##"{"textures":{"all":"minecraft:block/one_tex","up":"#all","down":"#all","north":"#all","south":"#all","east":"#all","west":"#all"}}"##),
        ]);
        let mut r = ModelResolver::new();
        let t = r.textures(&mut packs, "minecraft:s", "").unwrap();
        assert_eq!(t.distinct(), vec!["assets/minecraft/textures/block/one_tex.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multipart_blocks_still_yield_textures() {
        let (dir, mut packs) = pack("multipart", &[
            ("assets/minecraft/blockstates/fence.json", r##"{"multipart":[{"apply":{"model":"minecraft:block/fence_post"}}]}"##),
            ("assets/minecraft/models/block/fence_post.json", r##"{"textures":{"texture":"minecraft:block/oak_planks","particle":"#texture"}}"##),
        ]);
        let mut r = ModelResolver::new();
        let t = r.textures(&mut packs, "minecraft:fence", "").unwrap();
        assert_eq!(t.distinct(), vec!["assets/minecraft/textures/block/oak_planks.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn namespaces_default_to_minecraft() {
        assert_eq!(split("block/stone"), ("minecraft", "block/stone"));
        assert_eq!(split("mypack:block/stone"), ("mypack", "block/stone"));
        assert_eq!(texture_path("mypack:block/x"), "assets/mypack/textures/block/x.png");
    }
}
