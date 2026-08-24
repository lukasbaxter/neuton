//! Glyph coverage for MOTDs.
//!
//! Server MOTDs are not plain ASCII. They carry emoji, box-drawing runs, CJK,
//! arrows, and private-use codepoints from a server's own resource pack. egui
//! ships a Latin face plus a monochrome emoji face, and anything outside those
//! draws as a blank box.
//!
//! So the platform's own fonts are pulled in as fallbacks. Nothing is embedded:
//! a font with real Unicode coverage is megabytes, and the point of this client
//! is a small binary that starts instantly.

/// Font files worth trying, best coverage first. Missing files are skipped.
fn candidates() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[
            // Enormous coverage, ships on most macOS installs.
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/System/Library/Fonts/Apple Symbols.ttf",
            // CJK, which shows up in MOTDs more than you would expect.
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/PingFang.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
        ]
    } else if cfg!(windows) {
        &[
            "C:\\Windows\\Fonts\\seguisym.ttf",
            "C:\\Windows\\Fonts\\segoeui.ttf",
            "C:\\Windows\\Fonts\\arial.ttf",
            "C:\\Windows\\Fonts\\msgothic.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ]
    }
}

/// Registers system fallbacks. Returns the names that loaded, for diagnostics.
///
/// Fallbacks go on the end of both families, so the bundled face still wins for
/// Latin and only the gaps reach a system font.
pub fn install(ctx: &egui::Context) -> Vec<String> {
    let mut defs = egui::FontDefinitions::default();
    let mut loaded = Vec::new();

    for path in candidates() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = std::path::Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_string();

        defs.font_data
            .insert(name.clone(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            defs.families.entry(family).or_default().push(name.clone());
        }
        loaded.push(name);
    }

    ctx.set_fonts(defs);
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_list_is_populated_for_this_platform() {
        assert!(!candidates().is_empty());
    }

    #[test]
    fn at_least_one_system_font_exists_on_this_machine() {
        // Not a hard requirement of the client, but if none of these resolve on
        // a normal developer machine the list is probably wrong.
        let found = candidates().iter().filter(|p| std::path::Path::new(p).exists()).count();
        assert!(found > 0, "none of the candidate fonts exist: {:?}", candidates());
    }
}
