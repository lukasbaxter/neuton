//! Settings, and where they are kept.
//!
//! Written next to the accounts and the server list, in the same plain JSON, so
//! a person can read and edit it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use winit::keyboard::KeyCode;

/// Something the player can bind a key to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Sneak,
    Sprint,
    Chat,
    Command,
    Debug,
}

impl Action {
    /// Every action, in the order the settings screen lists them.
    pub const ALL: [Action; 10] = [
        Action::Forward,
        Action::Back,
        Action::Left,
        Action::Right,
        Action::Jump,
        Action::Sneak,
        Action::Sprint,
        Action::Chat,
        Action::Command,
        Action::Debug,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Action::Forward => "Walk forward",
            Action::Back => "Walk backward",
            Action::Left => "Strafe left",
            Action::Right => "Strafe right",
            Action::Jump => "Jump",
            Action::Sneak => "Sneak",
            Action::Sprint => "Sprint",
            Action::Chat => "Open chat",
            Action::Command => "Open command",
            Action::Debug => "Debug overlay",
        }
    }

    const fn default_key(self) -> KeyCode {
        match self {
            Action::Forward => KeyCode::KeyW,
            Action::Back => KeyCode::KeyS,
            Action::Left => KeyCode::KeyA,
            Action::Right => KeyCode::KeyD,
            Action::Jump => KeyCode::Space,
            Action::Sneak => KeyCode::ShiftLeft,
            Action::Sprint => KeyCode::ControlLeft,
            Action::Chat => KeyCode::KeyT,
            Action::Command => KeyCode::Slash,
            Action::Debug => KeyCode::F3,
        }
    }
}

/// Which key runs which action.
///
/// Stored by name rather than by number: a key code's numeric value is an
/// implementation detail of the windowing library and would silently rebind
/// everything if it ever changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBinds {
    /// Every action, including the ones deliberately left unbound.
    ///
    /// An action missing from the list falls back to its default, so a settings
    /// file written by an older build still works. An action present with no
    /// key is unbound on purpose, and must not quietly revert.
    binds: Vec<(Action, Option<String>)>,
}

impl Default for KeyBinds {
    fn default() -> Self {
        Self {
            binds: Action::ALL
                .iter()
                .map(|a| (*a, Some(key_name(a.default_key()))))
                .collect(),
        }
    }
}

impl KeyBinds {
    pub fn key_for(&self, action: Action) -> Option<KeyCode> {
        match self.binds.iter().find(|(a, _)| *a == action) {
            Some((_, Some(name))) => key_from_name(name),
            // Deliberately unbound.
            Some((_, None)) => None,
            // Not in the file at all, which means it was added since it was
            // written.
            None => Some(action.default_key()),
        }
    }

    pub fn action_for(&self, key: KeyCode) -> Option<Action> {
        let name = key_name(key);
        self.binds
            .iter()
            .find(|(_, n)| n.as_deref() == Some(name.as_str()))
            .map(|(a, _)| *a)
            .or_else(|| {
                // Actions the file predates still answer to their default.
                Action::ALL.iter().copied().find(|a| {
                    !self.binds.iter().any(|(b, _)| b == a) && a.default_key() == key
                })
            })
    }

    /// Binds a key, clearing anything else that used it.
    ///
    /// Two actions on one key means one of them silently stops working, which
    /// is worse than the rebind appearing to clear the other.
    pub fn set(&mut self, action: Action, key: KeyCode) {
        let name = key_name(key);
        // Whatever held this key loses it, rather than two actions sharing one
        // and one of them silently not working.
        for (_, bound) in self.binds.iter_mut() {
            if bound.as_deref() == Some(name.as_str()) {
                *bound = None;
            }
        }
        match self.binds.iter_mut().find(|(a, _)| *a == action) {
            Some((_, bound)) => *bound = Some(name),
            None => self.binds.push((action, Some(name))),
        }
    }

    pub fn unbind(&mut self, action: Action) {
        match self.binds.iter_mut().find(|(a, _)| *a == action) {
            Some((_, bound)) => *bound = None,
            None => self.binds.push((action, None)),
        }
    }

    pub fn label(&self, action: Action) -> String {
        self.key_for(action).map(pretty_key).unwrap_or_else(|| "unbound".into())
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Everything the player can change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub fov: f32,
    /// Interface scale. Zero means follow the display.
    pub gui_scale: f32,
    /// Frames per second cap. Zero means uncapped.
    pub max_fps: u32,
    pub vsync: bool,
    /// Ignore the world's light and draw everything at full brightness.
    pub fullbright: bool,
    pub mouse_sensitivity: f32,
    /// Chunks, sent to the server as the view distance to stream.
    pub render_distance: u32,
    pub keys: KeyBinds,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fov: 70.0,
            gui_scale: 0.0,
            max_fps: 0,
            vsync: true,
            fullbright: false,
            mouse_sensitivity: 0.12,
            render_distance: 10,
            keys: KeyBinds::default(),
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        neuton_auth::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("settings.json")
    }

    /// Loads settings, falling back to defaults.
    ///
    /// A broken file is not worth refusing to start over, and the next save
    /// replaces it.
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)
    }

    /// Interface scale to actually use, given what the display reports.
    pub fn effective_gui_scale(&self, native: f32) -> f32 {
        if self.gui_scale <= 0.0 { native } else { self.gui_scale }
    }

    /// The lowest light any surface is drawn at.
    pub fn min_light(&self) -> f32 {
        if self.fullbright { 1.0 } else { 0.0 }
    }
}

/// A stable name for a key.
fn key_name(key: KeyCode) -> String {
    format!("{key:?}")
}

fn key_from_name(name: &str) -> Option<KeyCode> {
    // Small enough to search, and it avoids a table that has to be kept in step
    // with the windowing library.
    KEYS.iter().find(|k| format!("{k:?}") == name).copied()
}

/// What to show the player.
fn pretty_key(key: KeyCode) -> String {
    let raw = format!("{key:?}");
    let trimmed = raw
        .strip_prefix("Key")
        .or_else(|| raw.strip_prefix("Digit"))
        .unwrap_or(&raw);
    match trimmed {
        "ShiftLeft" => "Left Shift".into(),
        "ShiftRight" => "Right Shift".into(),
        "ControlLeft" => "Left Ctrl".into(),
        "ControlRight" => "Right Ctrl".into(),
        "AltLeft" => "Left Alt".into(),
        "AltRight" => "Right Alt".into(),
        "Space" => "Space".into(),
        other => other.to_string(),
    }
}

/// Keys that can be bound. Enough for movement and menus, not the whole board.
pub const KEYS: &[KeyCode] = &[
    KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE,
    KeyCode::KeyF, KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI, KeyCode::KeyJ,
    KeyCode::KeyK, KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO,
    KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR, KeyCode::KeyS, KeyCode::KeyT,
    KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX, KeyCode::KeyY,
    KeyCode::KeyZ,
    KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3,
    KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7,
    KeyCode::Digit8, KeyCode::Digit9,
    KeyCode::Space, KeyCode::ShiftLeft, KeyCode::ShiftRight,
    KeyCode::ControlLeft, KeyCode::ControlRight, KeyCode::AltLeft, KeyCode::AltRight,
    KeyCode::Tab, KeyCode::Backquote, KeyCode::Slash, KeyCode::Backslash,
    KeyCode::Comma, KeyCode::Period, KeyCode::Semicolon, KeyCode::Quote,
    KeyCode::BracketLeft, KeyCode::BracketRight, KeyCode::Minus, KeyCode::Equal,
    KeyCode::F1, KeyCode::F2, KeyCode::F3, KeyCode::F4, KeyCode::F5, KeyCode::F6,
    KeyCode::F7, KeyCode::F8, KeyCode::F9, KeyCode::F10, KeyCode::F11, KeyCode::F12,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_game() {
        let s = Settings::default();
        assert_eq!(s.keys.key_for(Action::Forward), Some(KeyCode::KeyW));
        assert_eq!(s.keys.key_for(Action::Sneak), Some(KeyCode::ShiftLeft));
        assert_eq!(s.keys.key_for(Action::Sprint), Some(KeyCode::ControlLeft));
        assert_eq!(s.keys.key_for(Action::Debug), Some(KeyCode::F3));
    }

    #[test]
    fn every_bindable_key_survives_a_round_trip() {
        // Binds are stored by name, so a name that does not parse back would
        // silently unbind that key.
        for key in KEYS {
            assert_eq!(key_from_name(&key_name(*key)), Some(*key), "{key:?}");
        }
    }

    #[test]
    fn rebinding_clears_whatever_used_that_key() {
        let mut keys = KeyBinds::default();
        // W is forward; giving it to back must take it from forward, and must
        // not quietly hand it back through the default.
        keys.set(Action::Back, KeyCode::KeyW);
        assert_eq!(keys.action_for(KeyCode::KeyW), Some(Action::Back));
        assert_eq!(keys.key_for(Action::Forward), None, "forward should be unbound");
        assert_eq!(keys.key_for(Action::Back), Some(KeyCode::KeyW));
    }

    #[test]
    fn an_action_the_file_predates_keeps_its_default() {
        // A settings file written before an action existed should not leave it
        // unbound.
        let mut keys = KeyBinds::default();
        keys.binds.retain(|(a, _)| *a != Action::Debug);
        assert_eq!(keys.key_for(Action::Debug), Some(KeyCode::F3));
        assert_eq!(keys.action_for(KeyCode::F3), Some(Action::Debug));
    }

    #[test]
    fn a_lookup_works_in_both_directions() {
        let keys = KeyBinds::default();
        assert_eq!(keys.action_for(KeyCode::KeyW), Some(Action::Forward));
        assert_eq!(keys.action_for(KeyCode::F7), None);
    }

    #[test]
    fn key_labels_are_readable() {
        assert_eq!(pretty_key(KeyCode::KeyW), "W");
        assert_eq!(pretty_key(KeyCode::Digit5), "5");
        assert_eq!(pretty_key(KeyCode::ShiftLeft), "Left Shift");
        assert_eq!(pretty_key(KeyCode::F3), "F3");
    }

    #[test]
    fn gui_scale_zero_follows_the_display() {
        let mut s = Settings::default();
        assert_eq!(s.effective_gui_scale(2.0), 2.0);
        s.gui_scale = 1.5;
        assert_eq!(s.effective_gui_scale(2.0), 1.5);
    }

    #[test]
    fn fullbright_raises_the_floor_to_full() {
        let mut s = Settings::default();
        assert_eq!(s.min_light(), 0.0);
        s.fullbright = true;
        assert_eq!(s.min_light(), 1.0);
    }

    #[test]
    fn settings_round_trip_through_json() {
        let mut s = Settings::default();
        s.fov = 95.0;
        s.fullbright = true;
        s.keys.set(Action::Jump, KeyCode::KeyQ);
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.fov, 95.0);
        assert!(back.fullbright);
        assert_eq!(back.keys.key_for(Action::Jump), Some(KeyCode::KeyQ));
    }
}
