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

/// One face of one element.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceDef {
    /// Pack-relative texture path, ready for the atlas.
    pub texture: String,
    /// Region of the texture this face shows, in the model's 0..16 space as
    /// `[u1, v1, u2, v2]` with v increasing downwards.
    ///
    /// Partial models rely on this: a lantern's sides are a 6x7 patch of its
    /// texture, and stretching the whole image over them instead is why one
    /// looks like a smear rather than a lantern.
    pub uv: [f32; 4],
    /// Whether this face takes the block's biome tint.
    ///
    /// Read from the model's `tintindex`, which is how vanilla marks the faces
    /// that are greyscale on disk and coloured at render time.
    pub tinted: bool,
    /// Which neighbour hides this face, if any. A face with no `cullface` is
    /// always drawn, which is what keeps the inside of a fence visible.
    pub cullface: Option<u8>,
}

/// One box of a model. Most blocks are a single full cube; grass is two.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// Corner in the model's 0..16 space.
    pub from: [f32; 3],
    pub to: [f32; 3],
    /// down, up, north, south, west, east.
    pub faces: [Option<FaceDef>; 6],
}

impl Element {
    /// True if this box fills the whole block.
    pub fn is_full_cube(&self) -> bool {
        self.from == [0.0, 0.0, 0.0] && self.to == [16.0, 16.0, 16.0]
    }
}

/// A block's geometry and textures, resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockModel {
    pub elements: Vec<Element>,
    /// Rotation from the blockstate variant, in degrees.
    pub x_rot: i32,
    pub y_rot: i32,
}

impl BlockModel {
    /// Every distinct texture used, for atlas collection.
    pub fn textures(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .elements
            .iter()
            .flat_map(|e| e.faces.iter().flatten())
            .map(|f| f.texture.as_str())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }
}

/// Face order used throughout: down, up, north, south, west, east.
pub const FACE_NAMES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

fn face_index(name: &str) -> Option<u8> {
    FACE_NAMES.iter().position(|n| *n == name).map(|i| i as u8)
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

    /// Resolves the model for one block state.
    ///
    /// `block` is a namespaced name such as `minecraft:oak_log`, `variant` the
    /// property string from the block state, such as `axis=y`.
    pub fn model(
        &mut self,
        packs: &mut PackStack,
        block: &str,
        variant: &str,
    ) -> Option<BlockModel> {
        let (namespace, name) = split(block);
        let path = format!("assets/{namespace}/blockstates/{name}.json");
        if !self.blockstates.contains_key(&path) {
            let value = packs.read_json(&path)?;
            self.blockstates.insert(path.clone(), value);
        }
        let state = self.blockstates.get(&path)?.clone();

        let (model, x_rot, y_rot) = pick_model(&state, variant)?;
        let textures = self.resolve_chain(packs, &model)?;
        let elements = self.resolve_elements(packs, &model, &textures)?;
        Some(BlockModel { elements, x_rot, y_rot })
    }

    /// Finds the child-most `elements` in the parent chain and resolves its
    /// texture references.
    ///
    /// A model that declares elements replaces its parent's outright, which is
    /// how a slab stops being a cube.
    fn resolve_elements(
        &mut self,
        packs: &mut PackStack,
        model: &str,
        textures: &HashMap<String, String>,
    ) -> Option<Vec<Element>> {
        let mut current = Some(model.to_string());
        for _ in 0..32 {
            let name = current.take()?;
            let (namespace, path) = split(&name);
            let file = format!("assets/{namespace}/models/{path}.json");
            if !self.models.contains_key(&file) {
                let value = packs.read_json(&file)?;
                self.models.insert(file.clone(), value);
            }
            let value = self.models.get(&file)?;

            if let Some(raw) = value.get("elements").and_then(|e| e.as_array()) {
                return Some(parse_elements(raw, textures));
            }
            current = value.get("parent").and_then(|p| p.as_str()).map(str::to_string);
        }
        None
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
/// A blockstate key lists only the properties that change the model, so a
/// stair's variants are keyed on facing, half and shape and say nothing about
/// waterlogged. Comparing whole strings therefore never matches, and silently
/// falls through to whichever variant happens to be first: every stair in the
/// world faces the same way.
///
/// A key matches when every property it names agrees with the block state.
fn pick_model(state: &serde_json::Value, variant: &str) -> Option<(String, i32, i32)> {
    if let Some(variants) = state.get("variants").and_then(|v| v.as_object()) {
        let properties = parse_properties(variant);
        let chosen = variants
            .iter()
            .find(|(key, _)| matches(key, &properties))
            // Nothing matched, which means the blockstate names a property this
            // build does not know about. Drawing something is better than
            // drawing nothing.
            .map(|(_, value)| value)
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

/// Splits `"facing=east,half=bottom"` into pairs.
fn parse_properties(variant: &str) -> Vec<(&str, &str)> {
    variant
        .split(',')
        .filter(|p| !p.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .collect()
}

/// True if every property the key names agrees with the state.
///
/// An empty key matches anything, which is how a block with no variants at all
/// is written.
fn matches(key: &str, properties: &[(&str, &str)]) -> bool {
    parse_properties(key)
        .iter()
        .all(|(name, value)| properties.iter().any(|(n, v)| n == name && v == value))
}

/// A variant entry is a model object, or a weighted list of them.
fn model_name(value: &serde_json::Value) -> Option<(String, i32, i32)> {
    match value {
        serde_json::Value::Array(items) => items.first().and_then(model_name),
        serde_json::Value::Object(_) => {
            let name = value.get("model").and_then(|m| m.as_str())?.to_string();
            let rot = |key| value.get(key).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            Some((name, rot("x"), rot("y")))
        }
        _ => None,
    }
}

/// Turns a model's `elements` array into resolved boxes.
fn parse_elements(
    raw: &[serde_json::Value],
    textures: &HashMap<String, String>,
) -> Vec<Element> {
    let resolve = |reference: &str| -> Option<String> {
        let key = reference.strip_prefix('#').unwrap_or(reference);
        let value = textures.get(key).map(String::as_str).unwrap_or(reference);
        (!value.starts_with('#')).then(|| texture_path(value))
    };

    raw.iter()
        .filter_map(|element| {
            let corner = |key: &str| -> Option<[f32; 3]> {
                let a = element.get(key)?.as_array()?;
                Some([
                    a.first()?.as_f64()? as f32,
                    a.get(1)?.as_f64()? as f32,
                    a.get(2)?.as_f64()? as f32,
                ])
            };
            let from = corner("from")?;
            let to = corner("to")?;

            let mut faces: [Option<FaceDef>; 6] = Default::default();
            for (name, face) in element.get("faces")?.as_object()? {
                let Some(index) = face_index(name) else { continue };
                let Some(texture) = face.get("texture").and_then(|t| t.as_str()) else { continue };
                let Some(texture) = resolve(texture) else { continue };
                let uv = face
                    .get("uv")
                    .and_then(|u| u.as_array())
                    .and_then(|a| {
                        Some([
                            a.first()?.as_f64()? as f32,
                            a.get(1)?.as_f64()? as f32,
                            a.get(2)?.as_f64()? as f32,
                            a.get(3)?.as_f64()? as f32,
                        ])
                    })
                    // Omitted means "derive it from the box", which is what
                    // makes a plain cube show its whole texture.
                    .unwrap_or_else(|| default_uv(index, from, to));

                faces[index as usize] = Some(FaceDef {
                    texture,
                    uv,
                    tinted: face.get("tintindex").is_some(),
                    cullface: face
                        .get("cullface")
                        .and_then(|c| c.as_str())
                        .and_then(face_index),
                });
            }
            Some(Element { from, to, faces })
        })
        .collect()
}

/// The texture region a face shows when the model does not say.
///
/// Taken from where the box sits, so a slab shows the bottom half of its
/// texture on its sides rather than the whole thing squashed.
fn default_uv(face: u8, from: [f32; 3], to: [f32; 3]) -> [f32; 4] {
    let [x0, y0, z0] = from;
    let [x1, y1, z1] = to;
    match face {
        0 => [x0, 16.0 - z1, x1, 16.0 - z0],        // down
        1 => [x0, z0, x1, z1],                      // up
        2 => [16.0 - x1, 16.0 - y1, 16.0 - x0, 16.0 - y0], // north
        3 => [x0, 16.0 - y1, x1, 16.0 - y0],        // south
        4 => [z0, 16.0 - y1, z1, 16.0 - y0],        // west
        _ => [16.0 - z1, 16.0 - y1, 16.0 - z0, 16.0 - y0], // east
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

    /// A full cube whose faces all use `#all`, like `block/cube_all`.
    const CUBE: &str = r##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{
        "down":{"texture":"#all","cullface":"down"},
        "up":{"texture":"#all","cullface":"up"},
        "north":{"texture":"#all","cullface":"north"},
        "south":{"texture":"#all","cullface":"south"},
        "west":{"texture":"#all","cullface":"west"},
        "east":{"texture":"#all","cullface":"east"}}}]}"##;

    #[test]
    fn a_simple_cube_takes_one_texture_on_every_face() {
        let (dir, mut packs) = pack("cube", &[
            ("assets/minecraft/blockstates/stone.json", r##"{"variants":{"":{"model":"minecraft:block/stone"}}}"##),
            ("assets/minecraft/models/block/stone.json", r##"{"parent":"minecraft:block/cube_all","textures":{"all":"minecraft:block/stone"}}"##),
            ("assets/minecraft/models/block/cube_all.json", CUBE),
        ]);
        let mut r = ModelResolver::new();
        let m = r.model(&mut packs, "minecraft:stone", "").unwrap();
        assert_eq!(m.elements.len(), 1);
        assert!(m.elements[0].is_full_cube());
        assert_eq!(m.textures(), vec!["assets/minecraft/textures/block/stone.png"]);
        // Every face of a plain cube can be hidden by its neighbour.
        assert!(m.elements[0].faces.iter().all(|f| f.as_ref().unwrap().cullface.is_some()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tintindex_marks_the_faces_that_take_a_biome_colour() {
        // Leaves are tinted on every face; the texture is greyscale on disk.
        let (dir, mut packs) = pack("tint", &[
            ("assets/minecraft/blockstates/oak_leaves.json", r##"{"variants":{"":{"model":"minecraft:block/oak_leaves"}}}"##),
            ("assets/minecraft/models/block/oak_leaves.json", r##"{"parent":"minecraft:block/leaves","textures":{"all":"minecraft:block/oak_leaves"}}"##),
            ("assets/minecraft/models/block/leaves.json", r##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{
                "up":{"texture":"#all","tintindex":0},"down":{"texture":"#all","tintindex":0}}}]}"##),
        ]);
        let mut r = ModelResolver::new();
        let m = r.model(&mut packs, "minecraft:oak_leaves", "").unwrap();
        assert!(m.elements[0].faces[1].as_ref().unwrap().tinted);
        assert!(m.elements[0].faces[0].as_ref().unwrap().tinted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grass_tints_its_top_but_not_its_sides() {
        // The shape that made tinting worth doing properly: one cube with an
        // untinted side and a tinted top, plus a tinted overlay cube over it.
        let (dir, mut packs) = pack("grass", &[
            ("assets/minecraft/blockstates/grass_block.json", r##"{"variants":{"snowy=false":{"model":"minecraft:block/grass_block"}}}"##),
            ("assets/minecraft/models/block/grass_block.json", r##"{"textures":{
                "bottom":"block/dirt","top":"block/grass_block_top","side":"block/grass_block_side","overlay":"block/grass_block_side_overlay"},
                "elements":[
                {"from":[0,0,0],"to":[16,16,16],"faces":{
                    "down":{"texture":"#bottom","cullface":"down"},
                    "up":{"texture":"#top","cullface":"up","tintindex":0},
                    "north":{"texture":"#side","cullface":"north"}}},
                {"from":[0,0,0],"to":[16,16,16],"faces":{
                    "north":{"texture":"#overlay","tintindex":0,"cullface":"north"}}}]}"##),
        ]);
        let mut r = ModelResolver::new();
        let m = r.model(&mut packs, "minecraft:grass_block", "snowy=false").unwrap();
        assert_eq!(m.elements.len(), 2, "the overlay is a second element");

        let base = &m.elements[0];
        assert!(base.faces[1].as_ref().unwrap().tinted, "top is tinted");
        assert!(!base.faces[2].as_ref().unwrap().tinted, "side is not");
        assert!(!base.faces[0].as_ref().unwrap().tinted, "bottom is dirt");
        assert!(m.elements[1].faces[2].as_ref().unwrap().tinted, "overlay is tinted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_column_gets_different_ends_and_sides() {
        let (dir, mut packs) = pack("column", &[
            ("assets/minecraft/blockstates/oak_log.json", r##"{"variants":{"axis=y":{"model":"minecraft:block/oak_log"},"axis=x":{"model":"minecraft:block/oak_log","x":90,"y":90}}}"##),
            ("assets/minecraft/models/block/oak_log.json", r##"{"parent":"minecraft:block/cube_column","textures":{"end":"minecraft:block/oak_log_top","side":"minecraft:block/oak_log"}}"##),
            ("assets/minecraft/models/block/cube_column.json", r##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{
                "down":{"texture":"#end"},"up":{"texture":"#end"},"north":{"texture":"#side"},"south":{"texture":"#side"},
                "west":{"texture":"#side"},"east":{"texture":"#side"}}}]}"##),
        ]);
        let mut r = ModelResolver::new();
        let m = r.model(&mut packs, "minecraft:oak_log", "axis=y").unwrap();
        let faces = &m.elements[0].faces;
        assert_eq!(faces[0].as_ref().unwrap().texture, "assets/minecraft/textures/block/oak_log_top.png");
        assert_eq!(faces[2].as_ref().unwrap().texture, "assets/minecraft/textures/block/oak_log.png");
        assert_eq!(m.textures().len(), 2);
        assert_eq!((m.x_rot, m.y_rot), (0, 0));

        // The horizontal variant carries the rotation the blockstate asked for.
        let sideways = r.model(&mut packs, "minecraft:oak_log", "axis=x").unwrap();
        assert_eq!((sideways.x_rot, sideways.y_rot), (90, 90));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_partial_element_is_not_a_full_cube() {
        // A bottom slab: half height, and its top face has no cullface so it
        // stays visible with a block above it.
        let (dir, mut packs) = pack("slab", &[
            ("assets/minecraft/blockstates/s.json", r##"{"variants":{"":{"model":"minecraft:block/slab"}}}"##),
            ("assets/minecraft/models/block/slab.json", r##"{"textures":{"all":"block/stone"},"elements":[
                {"from":[0,0,0],"to":[16,8,16],"faces":{"up":{"texture":"#all"},"down":{"texture":"#all","cullface":"down"}}}]}"##),
        ]);
        let mut r = ModelResolver::new();
        let m = r.model(&mut packs, "minecraft:s", "").unwrap();
        assert!(!m.elements[0].is_full_cube());
        assert_eq!(m.elements[0].to, [16.0, 8.0, 16.0]);
        assert!(m.elements[0].faces[1].as_ref().unwrap().cullface.is_none());
        assert!(m.elements[0].faces[0].as_ref().unwrap().cullface.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_overrides_a_texture_its_parent_declared() {
        let (dir, mut packs) = pack("override", &[
            ("assets/minecraft/blockstates/b.json", r##"{"variants":{"":{"model":"minecraft:block/child"}}}"##),
            ("assets/minecraft/models/block/child.json", r##"{"parent":"minecraft:block/base","textures":{"all":"minecraft:block/child_tex"}}"##),
            ("assets/minecraft/models/block/base.json", r##"{"textures":{"all":"minecraft:block/base_tex"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"}}}]}"##),
        ]);
        let mut r = ModelResolver::new();
        let m = r.model(&mut packs, "minecraft:b", "").unwrap();
        assert_eq!(m.textures(), vec!["assets/minecraft/textures/block/child_tex.png"]);
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
        // Terminates rather than hanging; with no elements anywhere in the loop
        // there is nothing to return.
        assert!(r.model(&mut packs, "minecraft:b", "").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multipart_blocks_still_resolve() {
        let (dir, mut packs) = pack("multipart", &[
            ("assets/minecraft/blockstates/fence.json", r##"{"multipart":[{"apply":{"model":"minecraft:block/fence_post"}}]}"##),
            ("assets/minecraft/models/block/fence_post.json", r##"{"textures":{"texture":"minecraft:block/oak_planks"},"elements":[
                {"from":[6,0,6],"to":[10,16,10],"faces":{"up":{"texture":"#texture"}}}]}"##),
        ]);
        let mut r = ModelResolver::new();
        let m = r.model(&mut packs, "minecraft:fence", "").unwrap();
        assert_eq!(m.textures(), vec!["assets/minecraft/textures/block/oak_planks.png"]);
        assert!(!m.elements[0].is_full_cube());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn namespaces_default_to_minecraft() {
        assert_eq!(split("block/stone"), ("minecraft", "block/stone"));
        assert_eq!(split("mypack:block/stone"), ("mypack", "block/stone"));
        assert_eq!(texture_path("mypack:block/x"), "assets/mypack/textures/block/x.png");
    }

    #[test]
    fn a_variant_key_matches_on_the_properties_it_names() {
        let props = parse_properties("facing=east,half=bottom,shape=straight,waterlogged=false");
        // Blockstate files omit properties that do not change the model.
        assert!(matches("facing=east,half=bottom,shape=straight", &props));
        assert!(matches("facing=east", &props));
        assert!(matches("", &props), "an empty key matches anything");
        // And still reject a key that disagrees.
        assert!(!matches("facing=west", &props));
        assert!(!matches("facing=east,half=top", &props));
    }

    #[test]
    fn variants_are_chosen_by_property_not_by_string() {
        // The bug this replaced: keys were compared whole, so a state carrying
        // an extra property matched nothing and fell through to the first
        // variant, pointing every stair in the world the same way.
        let (dir, mut packs) = pack("variants", &[
            ("assets/minecraft/blockstates/s.json", r##"{"variants":{
                "facing=east":{"model":"minecraft:block/e"},
                "facing=west":{"model":"minecraft:block/w","y":180}}}"##),
            ("assets/minecraft/models/block/e.json", r##"{"textures":{"all":"block/e"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"}}}]}"##),
            ("assets/minecraft/models/block/w.json", r##"{"textures":{"all":"block/w"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"}}}]}"##),
        ]);
        let mut r = ModelResolver::new();

        let east = r.model(&mut packs, "minecraft:s", "facing=east,waterlogged=false").unwrap();
        assert_eq!(east.textures(), vec!["assets/minecraft/textures/block/e.png"]);
        assert_eq!(east.y_rot, 0);

        let west = r.model(&mut packs, "minecraft:s", "facing=west,waterlogged=true").unwrap();
        assert_eq!(west.textures(), vec!["assets/minecraft/textures/block/w.png"]);
        assert_eq!(west.y_rot, 180);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn face_names_are_in_the_order_the_renderer_uses() {
        assert_eq!(face_index("down"), Some(0));
        assert_eq!(face_index("up"), Some(1));
        assert_eq!(face_index("east"), Some(5));
        assert_eq!(face_index("nonsense"), None);
    }
}
