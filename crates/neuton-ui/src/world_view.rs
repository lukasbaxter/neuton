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
        }
    }

    pub fn key(&mut self, code: KeyCode, pressed: bool) {
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

    /// One line of stats for the overlay.
    pub fn stats(&self, renderer: &WorldRenderer) -> String {
        let [x, y, z] = self.camera.position;
        format!(
            "{}  |  {:.0} fps ({:.1} ms)  |  {}/{} chunks drawn, {:.1}M tris  |  {x:.1} {y:.1} {z:.1}  |  {:.0} deg",
            self.session.status,
            if self.last_frame_ms > 0.0 { 1000.0 / self.last_frame_ms } else { 0.0 },
            self.last_frame_ms,
            renderer.drawn.get(),
            renderer.chunk_count(),
            self.session.triangles as f64 / 1.0e6,
            self.camera.yaw,
        )
    }
}
