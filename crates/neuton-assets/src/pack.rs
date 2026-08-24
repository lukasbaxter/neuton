//! Resource packs, layered the way Minecraft layers them.
//!
//! The vanilla jar is the bottom of the stack and every pack the user adds sits
//! on top, so a pack only has to contain the files it changes. A lookup walks
//! the stack from the top down and takes the first hit.
//!
//! Nothing is copied out of the jar and nothing is redistributed: neuton reads
//! the game the user already owns, in place.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// One layer of the stack.
enum Source {
    /// A `.zip` or `.jar`. Held open, since a pack is read from constantly
    /// during atlas stitching and reopening per file is wasteful.
    Archive { path: PathBuf, zip: zip::ZipArchive<File> },
    /// An unpacked pack, which is what people editing textures actually use.
    Directory(PathBuf),
}

/// An ordered set of packs. Later entries win.
pub struct PackStack {
    sources: Vec<Source>,
}

impl PackStack {
    pub fn new() -> Self {
        Self { sources: Vec::new() }
    }

    /// Adds a pack. A directory is used in place; anything else is opened as an
    /// archive.
    pub fn push(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref().to_path_buf();
        if path.is_dir() {
            self.sources.push(Source::Directory(path));
            return Ok(());
        }
        let file = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let zip = zip::ZipArchive::new(file)
            .map_err(|e| format!("{} is not a readable pack: {e}", path.display()))?;
        self.sources.push(Source::Archive { path, zip });
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Names of the loaded packs, bottom first.
    pub fn names(&self) -> Vec<String> {
        self.sources
            .iter()
            .map(|s| {
                let p = match s {
                    Source::Archive { path, .. } => path,
                    Source::Directory(path) => path,
                };
                p.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string()
            })
            .collect()
    }

    /// Reads a file, taking the topmost pack that has it.
    ///
    /// `path` is a pack-relative path such as
    /// `assets/minecraft/textures/block/stone.png`.
    pub fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        // Reversed: the last pack added is the one that overrides.
        for source in self.sources.iter_mut().rev() {
            match source {
                Source::Directory(dir) => {
                    // Reject traversal rather than letting a crafted pack read
                    // outside its own directory.
                    if path.contains("..") {
                        continue;
                    }
                    if let Ok(bytes) = std::fs::read(dir.join(path)) {
                        return Some(bytes);
                    }
                }
                Source::Archive { zip, .. } => {
                    if let Ok(mut entry) = zip.by_name(path) {
                        let mut buf = Vec::with_capacity(entry.size() as usize);
                        if entry.read_to_end(&mut buf).is_ok() {
                            return Some(buf);
                        }
                    }
                }
            }
        }
        None
    }

    /// Reads and parses a JSON file.
    pub fn read_json(&mut self, path: &str) -> Option<serde_json::Value> {
        let bytes = self.read(path)?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Every file under `prefix`, across all packs, deduplicated.
    ///
    /// Used to discover which textures exist so the atlas knows what to stitch.
    pub fn list(&mut self, prefix: &str) -> Vec<String> {
        let mut out = BTreeSet::new();
        for source in self.sources.iter_mut() {
            match source {
                Source::Archive { zip, .. } => {
                    for i in 0..zip.len() {
                        if let Ok(entry) = zip.by_index(i)
                            && let Some(name) = entry.enclosed_name()
                        {
                            let name = name.to_string_lossy().replace('\\', "/");
                            if name.starts_with(prefix) && !name.ends_with('/') {
                                out.insert(name);
                            }
                        }
                    }
                }
                Source::Directory(dir) => {
                    let root = dir.join(prefix);
                    collect(&root, dir, &mut out);
                }
            }
        }
        out.into_iter().collect()
    }
}

fn collect(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, root, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.insert(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

impl Default for PackStack {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the vanilla jar for a version lives in a normal installation.
pub fn vanilla_jar(version: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?);
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/minecraft")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?).join(".minecraft")
    } else {
        home.join(".minecraft")
    };
    let jar = base.join("versions").join(version).join(format!("{version}.jar"));
    jar.is_file().then_some(jar)
}

/// The user's `resourcepacks` folder, where packs are normally kept.
pub fn resource_pack_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?);
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/minecraft")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?).join(".minecraft")
    } else {
        home.join(".minecraft")
    };
    let dir = base.join("resourcepacks");
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("neuton-pack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn a_later_pack_overrides_an_earlier_one() {
        let base = temp("base");
        let over = temp("over");
        write(&base, "assets/minecraft/textures/block/stone.png", "BASE");
        write(&base, "assets/minecraft/textures/block/dirt.png", "BASE-DIRT");
        write(&over, "assets/minecraft/textures/block/stone.png", "OVERRIDE");

        let mut stack = PackStack::new();
        stack.push(&base).unwrap();
        stack.push(&over).unwrap();
        assert_eq!(stack.len(), 2);

        // Overridden by the top pack.
        assert_eq!(stack.read("assets/minecraft/textures/block/stone.png").unwrap(), b"OVERRIDE");
        // Not present in the top pack, so it falls through.
        assert_eq!(stack.read("assets/minecraft/textures/block/dirt.png").unwrap(), b"BASE-DIRT");
        assert!(stack.read("assets/minecraft/textures/block/nope.png").is_none());

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&over);
    }

    #[test]
    fn listing_merges_every_pack() {
        let base = temp("list-base");
        let over = temp("list-over");
        write(&base, "assets/minecraft/textures/block/a.png", "x");
        write(&base, "assets/minecraft/textures/block/b.png", "x");
        write(&over, "assets/minecraft/textures/block/b.png", "y");
        write(&over, "assets/minecraft/textures/block/c.png", "z");

        let mut stack = PackStack::new();
        stack.push(&base).unwrap();
        stack.push(&over).unwrap();
        let found = stack.list("assets/minecraft/textures/block/");
        assert_eq!(found.len(), 3, "b appears in both packs but must be listed once");
        assert!(found.iter().any(|f| f.ends_with("c.png")));

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&over);
    }

    #[test]
    fn a_pack_cannot_read_outside_itself() {
        let base = temp("escape");
        write(&base, "pack.mcmeta", "{}");
        let mut stack = PackStack::new();
        stack.push(&base).unwrap();
        assert!(stack.read("../../../etc/passwd").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn json_parses_through_the_stack() {
        let base = temp("json");
        write(&base, "assets/minecraft/models/block/stone.json", r#"{"parent":"block/cube_all"}"#);
        let mut stack = PackStack::new();
        stack.push(&base).unwrap();
        let v = stack.read_json("assets/minecraft/models/block/stone.json").unwrap();
        assert_eq!(v.get("parent").unwrap().as_str(), Some("block/cube_all"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_pack_is_an_error_not_a_panic() {
        let mut stack = PackStack::new();
        assert!(stack.push("/definitely/not/here.zip").is_err());
        assert!(stack.is_empty());
    }
}
