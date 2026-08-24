//! Flying around a live world.

use crate::session::{WorldEvent, WorldSession};
use neuton_render::{Camera, WorldRenderer};
use std::collections::HashSet;
use winit::keyboard::KeyCode;

/// Blocks per second at a walk, and the multiplier while sprinting.
const SPEED: f32 = 14.0;
const SPRINT: f32 = 5.0;
/// Degrees of turn per pixel of mouse movement.
const SENSITIVITY: f32 = 0.12;

pub struct WorldView {
    pub session: WorldSession,
    pub camera: Camera,
    held: HashSet<KeyCode>,
    /// True while the pointer is locked and the mouse turns the camera.
    pub captured: bool,
    /// Set once, so the first teleport places the camera rather than fighting
    /// whatever the user has already done with it.
    placed: bool,
    pub frames: u32,
    pub last_frame_ms: f32,
    /// Whether the debug panel is up. F3, as in the game.
    pub show_debug: bool,
    /// Smoothed frame time, so the number on screen is readable rather than
    /// flickering through every stutter.
    frame_ms_avg: f32,
}

impl WorldView {
    pub fn new(session: WorldSession) -> Self {
        Self {
            session,
            camera: Camera::default(),
            held: HashSet::new(),
            captured: false,
            placed: false,
            frames: 0,
            last_frame_ms: 0.0,
            show_debug: true,
            frame_ms_avg: 0.0,
        }
    }

    pub fn key(&mut self, code: KeyCode, pressed: bool) {
        if pressed && code == KeyCode::F3 {
            self.show_debug = !self.show_debug;
            return;
        }
        if pressed {
            self.held.insert(code);
        } else {
            self.held.remove(&code);
        }
    }

    /// Releases every key. Called when the window loses focus, so a key held
    /// while alt-tabbing does not stick down forever.
    pub fn release_all(&mut self) {
        self.held.clear();
    }

    pub fn mouse_moved(&mut self, dx: f32, dy: f32) {
        if self.captured {
            self.camera.turn(-dx * SENSITIVITY, dy * SENSITIVITY);
        }
    }

    /// Applies held keys for a frame of `dt` seconds.
    pub fn update(&mut self, dt: f32, renderer: &mut WorldRenderer, device: &wgpu::Device) {
        let mut speed = SPEED * dt;
        if self.held.contains(&KeyCode::ShiftLeft) {
            speed *= SPRINT;
        }
        let axis = |pos: KeyCode, neg: KeyCode| {
            (self.held.contains(&pos) as i32 - self.held.contains(&neg) as i32) as f32
        };
        self.camera.fly(
            axis(KeyCode::KeyW, KeyCode::KeyS) * speed,
            axis(KeyCode::KeyD, KeyCode::KeyA) * speed,
            axis(KeyCode::Space, KeyCode::ControlLeft) * speed,
        );

        for event in self.session.drain() {
            match event {
                WorldEvent::Joined { .. } => {
                    self.session.status = "in world".to_string();
                }
                WorldEvent::Chunk { x, z, mesh } => {
                    renderer.upload(device, x, z, &mesh);
                }
                WorldEvent::Forget { x, z } => renderer.forget(x, z),
                WorldEvent::Moved { x, y, z, yaw, pitch } => {
                    if !self.placed {
                        self.placed = true;
                        // Eye height, so the view sits where a player's would.
                        self.camera.position = [x as f32, y as f32 + 1.62, z as f32];
                        self.camera.yaw = yaw;
                        self.camera.pitch = pitch;
                    }
                }
                WorldEvent::Disconnected(_) => {}
            }
        }
    }

    /// Folds this frame's time into the smoothed average.
    pub fn record_frame(&mut self, dt: f32) {
        self.last_frame_ms = dt * 1000.0;
        self.frames += 1;
        // Exponential average: responsive enough to show a real slowdown,
        // steady enough that the number can be read.
        self.frame_ms_avg = if self.frame_ms_avg == 0.0 {
            self.last_frame_ms
        } else {
            self.frame_ms_avg * 0.9 + self.last_frame_ms * 0.1
        };
    }

    pub fn fps(&self) -> f32 {
        if self.frame_ms_avg > 0.0 { 1000.0 / self.frame_ms_avg } else { 0.0 }
    }

    /// The debug panel's contents, in the order they are shown.
    pub fn debug_lines(&self, renderer: &WorldRenderer) -> Vec<String> {
        let [x, y, z] = self.camera.position;
        let (bx, by, bz) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        vec![
            format!("neuton {}  ({} fps)", env!("CARGO_PKG_VERSION"), self.fps().round()),
            format!("{:.1} ms/frame", self.frame_ms_avg),
            String::new(),
            format!("XYZ: {x:.3} / {y:.3} / {z:.3}"),
            format!("Block: {bx} {by} {bz}"),
            format!("Chunk: {} {} in {} {}", bx & 15, bz & 15, bx >> 4, bz >> 4),
            format!("Facing: {} ({:.1} / {:.1})", self.facing(), self.camera.yaw, self.camera.pitch),
            String::new(),
            format!("Chunks: {} drawn / {} loaded", renderer.drawn.get(), renderer.chunk_count()),
            format!("Triangles: {:.2}M", self.session.triangles as f64 / 1.0e6),
            format!("Server: {}", self.session.server),
            format!("Status: {}", self.session.status),
        ]
    }

    /// Compass direction and the axis it faces, the way the game words it.
    fn facing(&self) -> &'static str {
        match self.camera.yaw.rem_euclid(360.0) {
            y if !(45.0..315.0).contains(&y) => "south (+Z)",
            y if y < 135.0 => "west (-X)",
            y if y < 225.0 => "north (-Z)",
            _ => "east (+X)",
        }
    }
}
