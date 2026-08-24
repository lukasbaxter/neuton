//! Flying around a live world.

use crate::chat::Chat;
use crate::session::{Outgoing, WorldEvent, WorldSession};
use neuton_world::{Body, Chunk, Input as MoveInput, physics};
use neuton_world::physics::Abilities;
use std::collections::HashMap;
use std::sync::Arc;
use neuton_render::{Camera, WorldRenderer};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use winit::keyboard::KeyCode;

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
    pub chat: Chat,
    /// Block data for collision, keyed by chunk.
    blocks: HashMap<(i32, i32), Arc<Chunk>>,
    /// Physical state. Separate from the camera, which follows it.
    pub body: Body,
    /// Shapes to walk into, shared with the meshing thread.
    shapes: Arc<neuton_render::BlockTextures>,
    /// Time since the last physics tick, so physics runs at a fixed rate
    /// regardless of frame rate.
    accumulator: f64,
    /// Set on the frame the jump key is pressed, for the double-tap to fly.
    last_jump: Option<Instant>,
    /// What the server says the player may do.
    pub abilities: Abilities,
    /// Whether the pause menu is up. The world takes no input while it is.
    pub paused: bool,
    /// Set when the player chooses to disconnect.
    pub leaving: bool,
    /// When the world started and finished arriving, for measuring it.
    pub joined_at: Instant,
    pub first_chunk: Option<Instant>,
    pub last_chunk: Option<Instant>,
    pub timing: crate::session::Timing,
    /// When the last movement update went out, and what it said.
    last_move_sent: Option<Instant>,
    last_position: Option<[f64; 3]>,
    last_rotation: Option<(f32, f32)>,
    /// Ticks since anything about the player changed.
    idle_ticks: u32,
    /// Whether the server has been told the world finished loading.
    reported_loaded: bool,
}

impl WorldView {
    pub fn new(session: WorldSession, shapes: Arc<neuton_render::BlockTextures>) -> Self {
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
            chat: Chat::default(),
            blocks: HashMap::new(),
            body: Body::default(),
            shapes,
            accumulator: 0.0,
            last_jump: None,
            abilities: Abilities::default(),
            paused: false,
            leaving: false,
            joined_at: Instant::now(),
            first_chunk: None,
            last_chunk: None,
            timing: Default::default(),
            last_move_sent: None,
            last_position: None,
            last_rotation: None,
            idle_ticks: 0,
            reported_loaded: false,
        }
    }

    /// Handles a key. Returns true if it wants the mouse released, which is
    /// what opening chat does.
    ///
    /// `repeat` marks an auto-repeat from a key being held. Holding space
    /// produces a stream of them, and treating those as separate presses is
    /// what made holding jump toggle flight over and over.
    pub fn key(&mut self, code: KeyCode, pressed: bool, repeat: bool) -> bool {
        // While typing, keys belong to the text field and nothing else.
        if self.chat.is_open() {
            if !pressed {
                return false;
            }
            match code {
                KeyCode::Escape => self.chat.close(),
                KeyCode::Enter | KeyCode::NumpadEnter => self.submit_chat(),
                KeyCode::ArrowUp => self.chat.history_back(),
                KeyCode::ArrowDown => self.chat.history_forward(),
                _ => {}
            }
            return false;
        }

        if pressed {
            match code {
                KeyCode::F3 => {
                    self.show_debug = !self.show_debug;
                    return false;
                }
                // T for chat and slash for a command, as in the game.
                KeyCode::KeyT => {
                    self.chat.open("");
                    self.held.clear();
                    return true;
                }
                KeyCode::Slash => {
                    self.chat.open("/");
                    self.held.clear();
                    return true;
                }
                // Double-tap jump to fly, where the server allows it at all.
                KeyCode::Space if self.abilities.may_fly && !repeat => {
                    let now = Instant::now();
                    if self.last_jump.is_some_and(|t| now.duration_since(t).as_millis() < 300) {
                        self.toggle_fly();
                        self.last_jump = None;
                    } else {
                        self.last_jump = Some(now);
                    }
                }
                _ => {}
            }
            self.held.insert(code);
        } else {
            self.held.remove(&code);
        }
        false
    }

    /// Appends typed text to the input line.
    ///
    /// Driven from the key event's own text rather than from a key code, so
    /// layouts, modifiers and dead keys all resolve to the character the user
    /// actually meant.
    pub fn type_text(&mut self, text: &str) {
        let Some(input) = self.chat.input_mut() else { return };
        for c in text.chars() {
            // Control characters arrive here too; enter and escape are handled
            // as keys and would otherwise be inserted literally.
            if !c.is_control() && input.chars().count() < 256 {
                input.push(c);
            }
        }
    }

    /// Deletes the last character of the input line.
    pub fn backspace(&mut self) {
        if let Some(input) = self.chat.input_mut() {
            input.pop();
        }
    }

    /// Sends whatever is in the input line.
    fn submit_chat(&mut self) {
        let Some(text) = self.chat.submit() else { return };
        match text.strip_prefix('/') {
            // Commands go in their own packet, which carries no signature and
            // so works on servers that refuse unsigned chat.
            Some(command) if !command.is_empty() => {
                self.session.send(Outgoing::Command(command.to_string()));
            }
            _ => self.session.send(Outgoing::Chat(text)),
        }
    }

    /// Releases every key. Called when the window loses focus, so a key held
    /// while alt-tabbing does not stick down forever.
    pub fn release_all(&mut self) {
        self.held.clear();
    }

    pub fn mouse_moved(&mut self, dx: f32, dy: f32) {
        if self.captured {
            // Moving the mouse right turns the view right, and on these axes
            // turning right raises the yaw: facing south, your right is west,
            // which is a larger yaw than south.
            self.camera.turn(dx * SENSITIVITY, dy * SENSITIVITY);
        }
    }

    /// Advances the player and the world for a frame of `dt` seconds.
    pub fn update(&mut self, dt: f32, renderer: &mut WorldRenderer, device: &wgpu::Device) {
        // A paused world holds still, and holding a key while pausing must not
        // leave the player walking into a wall.
        if self.paused {
            self.held.clear();
        }
        self.tick_physics(dt);

        // The server is told where we are about twenty times a second, which is
        // the rate the game itself uses. Less often and chunk loading lags
        // behind the camera; more often is just traffic.
        const MOVE_INTERVAL: Duration = Duration::from_millis(50);
        if self.placed && self.last_move_sent.is_none_or(|t| t.elapsed() >= MOVE_INTERVAL) {
            self.last_move_sent = Some(Instant::now());

            let position = self.body.position;
            let rotation = (self.camera.yaw, self.camera.pitch);

            // Only report what changed. Claiming to have turned every tick when
            // the view has not moved is what anti-cheat flags as a duplicate
            // look, and it is a fair thing to flag.
            let moved = self.last_position.is_none_or(|last| {
                let d = [position[0] - last[0], position[1] - last[1], position[2] - last[2]];
                d[0] * d[0] + d[1] * d[1] + d[2] * d[2] > 4.0e-8
            });
            let turned = self
                .last_rotation
                .is_none_or(|(yaw, pitch)| yaw != rotation.0 || pitch != rotation.1);

            if moved {
                self.last_position = Some(position);
            }
            if turned {
                self.last_rotation = Some(rotation);
            }
            // Standing still and looking the same way needs no packet most
            // ticks. The game sends a bare status update about once a second to
            // show it is still there; sending one every tick is noise that
            // anti-cheat reads as a client not running a real game loop.
            self.idle_ticks = if moved || turned { 0 } else { self.idle_ticks + 1 };
            if moved || turned || self.idle_ticks >= 20 {
                if self.idle_ticks >= 20 {
                    self.idle_ticks = 0;
                }
                self.session.send(Outgoing::Move {
                    position: moved.then_some(position),
                    rotation: turned.then_some(rotation),
                });
            }
            // Every tick ends, whatever happened in it.
            self.session.send(Outgoing::TickEnd);
        }

        for event in self.session.drain() {
            match event {
                WorldEvent::Joined { .. } => {
                    self.session.status = "in world".to_string();
                }
                WorldEvent::Chunk { x, z, mesh, blocks } => {
                    if self.first_chunk.is_none() {
                        self.first_chunk = Some(Instant::now());
                    }
                    self.last_chunk = Some(Instant::now());
                    renderer.upload(device, x, z, &mesh);
                    self.blocks.insert((x, z), blocks);
                }
                WorldEvent::Forget { x, z } => {
                    renderer.forget(x, z);
                    self.blocks.remove(&(x, z));
                }
                WorldEvent::Moved { x, y, z, yaw, pitch } => {
                    if !self.placed {
                        self.placed = true;
                        // The server sends the feet; the camera sits at the eyes.
                        self.body.position = [x, y, z];
                        self.body.velocity = [0.0; 3];
                        // Walking, not flying: the whole point.
                        self.body.flying = false;
                        self.camera.position =
                            [x as f32, (y + neuton_world::physics::EYE_HEIGHT) as f32, z as f32];
                        self.camera.yaw = yaw;
                        self.camera.pitch = pitch;
                        if !self.reported_loaded {
                            self.reported_loaded = true;
                            self.session.send(Outgoing::Loaded);
                        }
                    }
                }
                WorldEvent::Abilities(abilities) => {
                    self.abilities = abilities;
                    // The server has the final say: it can put a player into
                    // flight, and it can take it away mid-air.
                    self.body.flying = abilities.flying;
                    if !abilities.may_fly {
                        self.body.flying = false;
                    }
                }
                WorldEvent::Timing(t) => self.timing = t,
                WorldEvent::Chat(spans) => self.chat.push(spans),
                WorldEvent::Disconnected(why) => {
                    self.chat.note(format!("Disconnected: {why}"));
                }
            }
        }
    }

    /// Runs physics at a fixed rate, whatever the frame rate.
    ///
    /// A variable step changes how far a jump goes and how reliably a collision
    /// is caught, so the simulation runs in fixed slices and the frame just
    /// decides how many of them happen.
    fn tick_physics(&mut self, dt: f32) {
        const TICK: f64 = 1.0 / 60.0;
        let axis = |held: &HashSet<KeyCode>, pos: KeyCode, neg: KeyCode| {
            (held.contains(&pos) as i32 - held.contains(&neg) as i32) as f32
        };
        let input = MoveInput {
            forward: axis(&self.held, KeyCode::KeyW, KeyCode::KeyS),
            strafe: axis(&self.held, KeyCode::KeyD, KeyCode::KeyA),
            jump: self.held.contains(&KeyCode::Space),
            sneak: self.held.contains(&KeyCode::ShiftLeft),
            sprint: self.held.contains(&KeyCode::ControlLeft),
            yaw: self.camera.yaw,
        };

        // Nothing to stand on until the ground has arrived. Chunks stream in
        // over about a second, and a player who starts falling before then is
        // inside the terrain by the time it lands.
        let here = (
            (self.body.position[0].floor() as i32).div_euclid(16),
            (self.body.position[2].floor() as i32).div_euclid(16),
        );
        if !self.blocks.contains_key(&here) {
            self.accumulator = 0.0;
            return;
        }

        self.accumulator += dt as f64;
        // Bounded, so a long stall does not make the player teleport when it
        // ends.
        self.accumulator = self.accumulator.min(0.25);
        let world = ChunkWorld { chunks: &self.blocks };
        while self.accumulator >= TICK {
            self.accumulator -= TICK;
            physics::step(&mut self.body, input, &world, self.shapes.as_ref(), TICK);
        }

        // The camera sits at the eyes.
        self.camera.position = [
            self.body.position[0] as f32,
            (self.body.position[1] + neuton_world::physics::EYE_HEIGHT) as f32,
            self.body.position[2] as f32,
        ];
    }

    /// Switches between walking and flying, where the server allows it.
    fn toggle_fly(&mut self) {
        if !self.abilities.may_fly {
            return;
        }
        self.body.flying = !self.body.flying;
        // Vertical speed is dropped, or letting go of fly at height turns into
        // an instant plummet at whatever the fly speed was.
        self.body.velocity[1] = 0.0;
        self.chat.note(if self.body.flying { "Flying" } else { "Walking" });
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
            format!("Triangles: {:.2}M", renderer.triangles() as f64 / 1.0e6),
            format!(
                "Mode: {}{}",
                if self.abilities.instant_build { "creative" } else { "survival" },
                if self.body.flying { ", flying" } else { "" }
            ),
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


/// Looks blocks up across the loaded chunks.
struct ChunkWorld<'a> {
    chunks: &'a HashMap<(i32, i32), Arc<Chunk>>,
}

impl neuton_world::BlockView for ChunkWorld<'_> {
    fn state_at(&self, x: i32, y: i32, z: i32) -> neuton_blocks::StateId {
        let key = (x.div_euclid(16), z.div_euclid(16));
        // An unloaded chunk reads as air. Treating it as solid would trap the
        // player at the edge of the world; falling through is recoverable and
        // the chunk usually arrives first anyway.
        let Some(chunk) = self.chunks.get(&key) else {
            return neuton_blocks::StateId(0);
        };
        chunk
            .state_at(x.rem_euclid(16) as usize, y, z.rem_euclid(16) as usize)
            .unwrap_or(neuton_blocks::StateId(0))
    }
}
