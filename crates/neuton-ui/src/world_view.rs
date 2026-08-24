//! Flying around a live world.

use crate::chat::Chat;
use crate::session::{Outgoing, WorldEvent, WorldSession};
use neuton_world::{Body, Chunk, Input as MoveInput, physics};
use neuton_world::physics::Abilities;
use std::collections::HashMap;
use std::sync::Arc;
use neuton_render::{Camera, WorldRenderer};
use std::collections::HashSet;
use std::time::Instant;
use crate::settings::{Action, Settings};
use winit::keyboard::KeyCode;

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
    /// Where the body was at the previous tick, for interpolating the camera
    /// between them.
    previous: [f64; 3],
    /// How far the player has walked, for bobbing what they are holding.
    pub walked: f32,
    /// Health and hunger, as the server reports them.
    pub health: f32,
    pub food: i32,
    /// Set when the player has died and has not yet asked to come back.
    pub dead: bool,
    /// The block under the crosshair, if any is in reach.
    pub target: Option<neuton_world::raycast::Hit>,
    /// The block being broken, and when the swing at it started.
    mining: Option<([i32; 3], Instant)>,
    /// When the arm last swung, so breaking keeps it moving.
    last_swing: Option<Instant>,
    /// Whether the use button is held, for placing a run of blocks.
    using: bool,
    /// When the last placement went out, so holding the button does not send
    /// one a frame.
    last_use: Option<Instant>,
    /// What the player is carrying, and which slot is in hand.
    pub inventory: crate::inventory::Inventory,
    /// Item pictures and interface sprites, rendered as they are first needed.
    pub art: crate::inventory::ItemArt,
    /// Chunk meshes waiting to go to the GPU, oldest first.
    pending: std::collections::VecDeque<(i32, i32, Box<neuton_render::Mesh>)>,
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
    /// The player's settings. Changes take effect the moment they are made.
    pub settings: Settings,
    /// Whether the settings screen is up, over the pause menu.
    pub settings_open: bool,
    /// The action waiting for a key, while rebinding one.
    pub rebinding: Option<Action>,
    /// What the last movement update said, so only changes are reported.
    last_position: Option<[f64; 3]>,
    /// Ticks since a position last went out. The game restates its position
    /// every twentieth tick however still the player is standing.
    position_reminder: u32,
    /// Whether the last update said we were standing on something.
    last_on_ground: Option<bool>,
    last_rotation: Option<(f32, f32)>,

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
            previous: [0.0; 3],
            walked: 0.0,
            health: 20.0,
            food: 20,
            dead: false,
            target: None,
            mining: None,
            last_swing: None,
            using: false,
            last_use: None,
            inventory: crate::inventory::Inventory::default(),
            art: crate::inventory::ItemArt::new(),
            pending: std::collections::VecDeque::new(),
            last_jump: None,
            abilities: Abilities::default(),
            paused: false,
            leaving: false,
            joined_at: Instant::now(),
            first_chunk: None,
            last_chunk: None,
            timing: Default::default(),
            settings: Settings::load(),
            settings_open: false,
            rebinding: None,
            last_position: None,
            position_reminder: 0,
            last_on_ground: None,
            last_rotation: None,
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

        if self.dead {
            if pressed && matches!(code, KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space) {
                self.respawn();
            }
            return false;
        }

        let action = self.settings.keys.action_for(code);
        if pressed {
            match action {
                Some(Action::Debug) => {
                    self.show_debug = !self.show_debug;
                    return false;
                }
                Some(Action::Chat) => {
                    self.chat.open("");
                    self.held.clear();
                    return true;
                }
                Some(Action::Command) => {
                    self.chat.open("/");
                    self.held.clear();
                    return true;
                }
                // The number row picks a hotbar slot, as it always has.
                None if !repeat && hotbar_digit(code).is_some() => {
                    if let Some(slot) = hotbar_digit(code) {
                        self.select_slot(slot);
                    }
                    return false;
                }
                Some(Action::Inventory) if !repeat => {
                    self.toggle_inventory();
                    return true;
                }
                // Double-tap jump to fly, where the server allows it at all.
                Some(Action::Jump) if self.abilities.may_fly && !repeat => {
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

    /// True if the key bound to an action is currently down.
    fn acting(&self, action: Action) -> bool {
        self.settings
            .keys
            .key_for(action)
            .is_some_and(|key| self.held.contains(&key))
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
            let sensitivity = self.settings.mouse_sensitivity;
            // Moving the mouse right turns the view right, and on these axes
            // turning right raises the yaw: facing south, your right is west,
            // which is a larger yaw than south.
            self.camera.turn(dx * sensitivity, dy * sensitivity);
        }
    }

    /// Advances the player and the world for a frame of `dt` seconds.
    pub fn update(
        &mut self,
        dt: f32,
        renderer: &mut WorldRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        // A paused world holds still, and holding a key while pausing must not
        // leave the player walking into a wall.
        if self.paused {
            self.held.clear();
        }
        // Corrections are applied before the step they correct, so a
        // teleport does not spend a tick being walked away from.
        self.pump_events(renderer, device);
        self.tick_physics(dt);
        self.aim();
        self.show_target(renderer, queue);

        // Holding the use button lays a run of blocks, at the rate the game
        // uses rather than one a frame.
        const USE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
        if self.using
            && !self.paused
            && self.last_use.is_none_or(|t| t.elapsed() >= USE_INTERVAL)
        {
            self.use_item();
        }
        // Looking away from a block gives up on breaking it, and starting on
        // whatever is under the crosshair now is a new swing.
        if let Some((at, _)) = self.mining
            && self.target.is_none_or(|hit| hit.block != at)
        {
            self.stop_breaking();
            if self.target.is_some() {
                self.begin_breaking();
            }
        }
        // The arm keeps moving while a block is being broken.
        const SWING_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
        if self.mining.is_some()
            && self.last_swing.is_none_or(|t| t.elapsed() >= SWING_INTERVAL)
        {
            self.last_swing = Some(Instant::now());
            self.session.send(Outgoing::Swing);
        }
    }

    /// How many chunk meshes go to the GPU in one frame.
    ///
    /// Enough that the world still fills in quickly, few enough that no single
    /// frame runs long.
    const UPLOADS_PER_FRAME: usize = 8;

    /// Applies everything the server has said since the last frame.
    fn pump_events(&mut self, renderer: &mut WorldRenderer, device: &wgpu::Device) {
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
                    // Uploading every mesh that arrived this frame is what
                    // makes joining stutter: a hundred of them land at once and
                    // the frame that takes them is long enough that the game
                    // misses several ticks, which a server reads as a client
                    // that stopped playing. They go up a few at a time instead.
                    self.pending.push_back((x, z, mesh));
                    self.blocks.insert((x, z), blocks);
                }
                WorldEvent::Forget { x, z } => {
                    self.pending.retain(|(px, pz, _)| (*px, *pz) != (x, z));
                    renderer.forget(x, z);
                    self.blocks.remove(&(x, z));
                }
                WorldEvent::Moved { x, y, z, yaw, pitch, relative } => {
                    // Every teleport, not just the first: a player moved by a
                    // command, a portal or a shove has to follow.
                    //
                    // But each field is either a destination or an offset. A
                    // server nudging a player sends an entirely relative
                    // teleport of nearly nothing, and reading that as a
                    // destination sends them back to wherever the numbers
                    // happen to name.
                    let first = !self.placed;
                    self.placed = true;

                    // Offsets were already resolved against what the server
                    // was last told, which is the base it used for them.
                    let position = [x, y, z];
                    let turned_to = (yaw, pitch);

                    self.body.position = position;
                    self.body.velocity = [0.0; 3];
                    // The camera interpolates between the last two ticks, so
                    // without this it spends a tick travelling from wherever
                    // the player used to be to wherever they now are.
                    self.previous = position;

                    // A teleport is only accepted once the client reports
                    // standing exactly where it was put. Anti-cheats compare
                    // the very next position packet against the teleport with
                    // no tolerance at all, so this goes out immediately after
                    // the acknowledgement rather than waiting for the next
                    // tick, by which time physics has already moved us.
                    self.last_position = Some(position);
                    self.last_rotation = Some(turned_to);

                    // A rotation the server did not specify is the player's,
                    // and overwriting it snaps the view every time the server
                    // corrects a position.
                    if first || relative.0 & 0b11000 == 0 {
                        self.camera.yaw = turned_to.0;
                        self.camera.pitch = turned_to.1;
                    }
                    if first {
                        // For looking at the inventory screen without a hand on
                        // the keyboard.
                        self.inventory.open = std::env::var_os("NEUTON_INVENTORY").is_some();
                        self.body.flying = self.abilities.flying;
                        self.session.send(Outgoing::Loaded);
                        self.reported_loaded = true;
                    }
                    self.camera.position = [
                        position[0] as f32,
                        (position[1] + neuton_world::physics::EYE_HEIGHT) as f32,
                        position[2] as f32,
                    ];

                    if std::env::var_os("NEUTON_TIMING").is_some() {
                        eprintln!(
                            "teleport: t={:.2}s to {position:?} yaw {:.1} relative {:#07b}",
                            self.joined_at.elapsed().as_secs_f32(), turned_to.0, relative.0
                        );
                    }
                }
                WorldEvent::Abilities(abilities) => {
                    self.abilities = abilities;
                    self.body.walk_speed = abilities.walk_speed as f64;
                    self.body.fly_speed = abilities.fly_speed as f64;
                    // The server has the final say: it can put a player into
                    // flight, and it can take it away mid-air.
                    self.body.flying = abilities.flying;
                    if !abilities.may_fly {
                        self.body.flying = false;
                    }
                }
                WorldEvent::Timing(t) => self.timing = t,
                WorldEvent::Chat(spans) => self.chat.push(spans),
                WorldEvent::Container { window, state_id, slots, carried } => {
                    // Window zero is the player's own inventory, the one behind
                    // every other screen. Anything else is a chest or a
                    // workbench, which this client does not open yet.
                    if window == 0 {
                        self.inventory.state_id = state_id;
                        self.inventory.replace(slots);
                        self.inventory.set_carried(carried);
                    }
                }
                WorldEvent::Slot { window, state_id, slot, stack } => {
                    if window == 0 {
                        self.inventory.state_id = state_id;
                        // Minus one is the cursor rather than a slot.
                        if slot < 0 {
                            self.inventory.set_carried(stack);
                        } else {
                            self.inventory.set(slot, stack);
                        }
                    }
                }
                WorldEvent::HeldSlot(slot) => {
                    if let Ok(slot) = usize::try_from(slot)
                        && slot < crate::inventory::HOTBAR.len()
                    {
                        self.inventory.selected = slot;
                    }
                }
                WorldEvent::Health { health, food } => {
                    self.health = health;
                    self.food = food;
                    // Death is a health of zero, not a packet. A player who
                    // joins already dead is never sent the death message again,
                    // and a client that waits for one waits forever: the server
                    // holds a dead player out of the world entirely, ignoring
                    // their movement and sending them no chunks, until it is
                    // asked to bring them back.
                    if health <= 0.0 && !self.dead {
                        self.dead = true;
                        self.held.clear();
                        self.chat.note("You died");
                        // A client driving itself has nobody to press the
                        // button for it.
                        if std::env::var_os("NEUTON_AUTOWALK").is_some() {
                            self.respawn();
                        }
                    }
                }
                WorldEvent::Knockback(velocity) => {
                    // Blocks per tick, the same units the body carries, so a
                    // shove simply replaces what the player was doing.
                    self.body.velocity = velocity;
                    self.body.on_ground = false;
                }
                WorldEvent::Died => {
                    if !self.dead {
                        self.dead = true;
                        self.held.clear();
                        self.chat.note("You died");
                    }
                }
                WorldEvent::Disconnected(why) => {
                    self.chat.note(format!("Disconnected: {why}"));
                }
            }
        }

        for _ in 0..Self::UPLOADS_PER_FRAME {
            let Some((x, z, mesh)) = self.pending.pop_front() else { break };
            renderer.upload(device, x, z, &mesh);
        }
    }

    /// Runs physics at the game's own rate, whatever the frame rate.
    ///
    /// Twenty a second is not a rendering choice. It is the rate the server
    /// simulates at, and a client that steps at any other rate walks a
    /// different curve than the server predicts for it -- which servers read,
    /// fairly, as a client that is not running the real game.
    pub const TICK: f64 = physics::TICK;

    fn tick_physics(&mut self, dt: f32) {
        // Walks and turns by itself, so the movement path can be exercised
        // without a person at the keyboard.
        let auto = std::env::var_os("NEUTON_AUTOWALK").is_some();
        if auto {
            self.camera.yaw = (self.camera.yaw + 60.0 * dt).rem_euclid(360.0);
            // Looking down a little, so what is under the crosshair is ground
            // within reach rather than the horizon.
            self.camera.pitch = 40.0;
        }
        let axis = |pos: Action, neg: Action| {
            (self.acting(pos) as i32 - self.acting(neg) as i32) as f32
        };
        let input = MoveInput {
            forward: if auto { 1.0 } else { axis(Action::Forward, Action::Back) },
            strafe: axis(Action::Right, Action::Left),
            jump: self.acting(Action::Jump),
            sneak: self.acting(Action::Sneak),
            sprint: self.acting(Action::Sprint),
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
            // Saying nothing reads better to a server than repeating a position
            // that is not falling when it expects one that is.
            self.accumulator = 0.0;
            self.previous = self.body.position;
            return;
        }
        self.accumulator += dt as f64;

        // A stall is time the player did not get to play. Catching all of it up
        // at once covers ground in one step that no legitimate movement could,
        // so past a few ticks the rest is dropped rather than sprinted through.
        self.accumulator = self.accumulator.min(Self::TICK * 4.0);

        while self.accumulator >= Self::TICK {
            self.accumulator -= Self::TICK;
            self.previous = self.body.position;
            {
                let world = ChunkWorld { chunks: &self.blocks };
                physics::step(&mut self.body, input, &world, self.shapes.as_ref());
            }
            let step = [
                self.body.position[0] - self.previous[0],
                self.body.position[2] - self.previous[2],
            ];
            self.walked += (step[0] * step[0] + step[1] * step[1]).sqrt() as f32;
            self.report_position();
        }

        // Settings that the camera owns.
        self.camera.fov_degrees = self.settings.fov;

        // Twenty steps a second is visibly steppy at a hundred and sixty
        // frames, so the camera sits between the last two of them.
        let alpha = (self.accumulator / Self::TICK) as f32;
        let at = |i: usize| {
            self.previous[i] as f32 + (self.body.position[i] - self.previous[i]) as f32 * alpha
        };
        self.camera.position =
            [at(0), at(1) + neuton_world::physics::EYE_HEIGHT as f32, at(2)];
    }

    /// Tells the server where we are, once per tick, the way the game does.
    fn report_position(&mut self) {
        if !self.placed {
            return;
        }
        let position = self.body.position;
        let rotation = (self.camera.yaw, self.camera.pitch);
        let on_ground = self.body.on_ground;

        // The game's own rule, and worth following exactly. A position goes out
        // when it has changed, and every twentieth tick regardless -- a client
        // that stands still and never restates where it is looks, to a server,
        // like a client whose position packets have stopped meaning anything.
        // Anti-cheat calls that "movement packets without updating position"
        // and stops trusting anything the client says, including its answer to
        // a teleport, which is how standing still ends up freezing a player in
        // place and swallowing every later teleport.
        self.position_reminder += 1;
        let moved = self.last_position.is_none_or(|last| {
            let d = [position[0] - last[0], position[1] - last[1], position[2] - last[2]];
            d[0] * d[0] + d[1] * d[1] + d[2] * d[2] > 4.0e-8
        });
        let send_position = moved || self.position_reminder >= 20;
        // Claiming to have turned every tick when the view has not moved is
        // what anti-cheat flags as a duplicate look, and it is fair to flag.
        let send_rotation = self
            .last_rotation
            .is_none_or(|(yaw, pitch)| yaw != rotation.0 || pitch != rotation.1);

        if send_position || send_rotation {
            self.session.send(Outgoing::Move {
                position: send_position.then_some(position),
                rotation: send_rotation.then_some(rotation),
                on_ground,
            });
        } else if self.last_on_ground != Some(on_ground) {
            // Landing and taking off are worth a packet on their own, and
            // nothing else is.
            self.session.send(Outgoing::Move { position: None, rotation: None, on_ground });
        }

        if send_position {
            self.last_position = Some(position);
            self.position_reminder = 0;
        }
        if send_rotation {
            self.last_rotation = Some(rotation);
        }
        self.last_on_ground = Some(on_ground);

        // Every tick ends, whatever happened in it.
        self.session.send(Outgoing::TickEnd);
    }

    /// Opens or closes the inventory screen, telling the server either way.
    pub fn toggle_inventory(&mut self) {
        self.inventory.open = !self.inventory.open;
        self.held.clear();
        if !self.inventory.open {
            // Closing hands anything left on the cursor back to the server,
            // which drops it or puts it away as the rules require.
            self.session.send(Outgoing::CloseContainer { window: 0 });
        }
    }

    /// Clicks a slot. The server decides what actually happens and says so.
    pub fn click_slot(&mut self, slot: usize, right: bool) {
        self.session.send(Outgoing::Click {
            window: 0,
            state_id: self.inventory.state_id,
            slot: slot as i16,
            button: u8::from(right),
            mode: 0,
        });
    }

    /// Asks the server to put the player back in the world.
    pub fn respawn(&mut self) {
        if !self.dead {
            return;
        }
        self.dead = false;
        self.session.send(Outgoing::Respawn);
    }

    /// Left breaks, right places. The world has to be in reach for either.
    pub fn mouse_button(&mut self, button: winit::event::MouseButton, pressed: bool) {
        use winit::event::MouseButton;
        if self.dead {
            return;
        }
        match button {
            MouseButton::Left => {
                if pressed {
                    self.begin_breaking();
                } else {
                    self.stop_breaking();
                }
            }
            MouseButton::Right => {
                self.using = pressed;
                if pressed {
                    self.last_use = None;
                    self.use_item();
                }
            }
            MouseButton::Middle if pressed => self.pick_block(),
            _ => {}
        }
    }

    /// Starts breaking whatever is under the crosshair.
    ///
    /// The server runs its own timer from here: once it has been told a swing
    /// started, it counts the block down and breaks it itself. So this sends
    /// the start and then keeps quiet, rather than guessing when the block
    /// should give and telling the server so, repeatedly, until it agreed.
    fn begin_breaking(&mut self) {
        let Some(hit) = self.target else {
            // Swinging at nothing is still a swing, and everyone else sees it.
            self.session.send(Outgoing::Swing);
            return;
        };
        self.session.send(Outgoing::Swing);
        self.session.send(Outgoing::StartBreaking { at: hit.block, face: hit.face });
        // In creative the first packet is the whole of it.
        self.mining = (!self.abilities.instant_build).then(|| (hit.block, Instant::now()));
    }

    /// Letting go, or looking away, gives up on the block.
    fn stop_breaking(&mut self) {
        if let Some((at, _)) = self.mining.take() {
            self.session.send(Outgoing::AbortBreaking { at });
        }
    }

    /// Uses what is in hand: placing a block, or using the item on its own.
    fn use_item(&mut self) {
        self.last_use = Some(Instant::now());
        match self.target {
            Some(hit) => {
                self.session.send(Outgoing::UseOn {
                    at: hit.block,
                    face: hit.face,
                    cursor: hit.cursor,
                });
                self.session.send(Outgoing::Swing);
            }
            None => {
                let (yaw, pitch) = (self.camera.yaw, self.camera.pitch);
                self.session.send(Outgoing::Use { yaw, pitch });
            }
        }
    }

    /// Puts the block being looked at into the hand, where the player has one.
    fn pick_block(&mut self) {
        let Some(hit) = self.target else { return };
        let Some(chunk) = self.blocks.get(&(hit.block[0].div_euclid(16), hit.block[2].div_euclid(16)))
        else {
            return;
        };
        let state = chunk
            .state_at(hit.block[0].rem_euclid(16) as usize, hit.block[1], hit.block[2].rem_euclid(16) as usize)
            .unwrap_or(neuton_blocks::StateId(0));
        let name = neuton_blocks::BLOCKS[neuton_blocks::STATE_TO_BLOCK[state.0 as usize] as usize].name;
        // Only if it is already somewhere on the hotbar: asking the server for
        // an item is a creative-mode packet this client does not send yet.
        for slot in 0..crate::inventory::HOTBAR.len() {
            if self
                .inventory
                .slot(crate::inventory::HOTBAR.start + slot)
                .is_some_and(|stack| stack.name == name)
            {
                self.select_slot(slot);
                return;
            }
        }
    }

    /// Draws the box around whatever is being pointed at.
    fn show_target(&self, renderer: &mut WorldRenderer, queue: &wgpu::Queue) {
        let mut boxes: Vec<([f32; 3], [f32; 3])> = Vec::new();
        if let Some(hit) = self.target {
            let column = (hit.block[0].div_euclid(16), hit.block[2].div_euclid(16));
            if let Some(chunk) = self.blocks.get(&column) {
                let state = chunk
                    .state_at(
                        hit.block[0].rem_euclid(16) as usize,
                        hit.block[1],
                        hit.block[2].rem_euclid(16) as usize,
                    )
                    .unwrap_or(neuton_blocks::StateId(0));
                let base = [hit.block[0] as f32, hit.block[1] as f32, hit.block[2] as f32];
                for shape in neuton_world::physics::BlockShapes::collision(
                    self.shapes.as_ref(),
                    state,
                ) {
                    boxes.push((
                        [
                            base[0] + shape.min[0] as f32,
                            base[1] + shape.min[1] as f32,
                            base[2] + shape.min[2] as f32,
                        ],
                        [
                            base[0] + shape.max[0] as f32,
                            base[1] + shape.max[1] as f32,
                            base[2] + shape.max[2] as f32,
                        ],
                    ));
                }
            }
        }
        renderer.set_outline(queue, &boxes);
    }

    /// Works out what the player is pointing at.
    fn aim(&mut self) {
        if !self.placed {
            self.target = None;
            return;
        }
        let eye = [
            self.body.position[0],
            self.body.position[1] + neuton_world::physics::EYE_HEIGHT,
            self.body.position[2],
        ];
        let (yaw, pitch) = (
            (self.camera.yaw as f64).to_radians(),
            (self.camera.pitch as f64).to_radians(),
        );
        let (sin_yaw, cos_yaw) = yaw.sin_cos();
        let (sin_pitch, cos_pitch) = pitch.sin_cos();
        // The camera looks along negative pitch upwards, matching the game.
        let direction = [-sin_yaw * cos_pitch, -sin_pitch, cos_yaw * cos_pitch];

        let world = ChunkWorld { chunks: &self.blocks };
        self.target = neuton_world::raycast::cast(
            eye,
            direction,
            neuton_world::raycast::REACH,
            &world,
            self.shapes.as_ref(),
        );
    }

    /// Picks a hotbar slot and tells the server, which otherwise keeps handing
    /// out whatever was selected before.
    pub fn select_slot(&mut self, slot: usize) {
        if slot >= crate::inventory::HOTBAR.len() || slot == self.inventory.selected {
            return;
        }
        self.inventory.selected = slot;
        self.session.send(Outgoing::HeldSlot(slot as i32));
    }

    /// The scroll wheel walks along the hotbar, wrapping at both ends.
    pub fn scroll(&mut self, delta: f32) {
        if delta == 0.0 {
            return;
        }
        let slot = self.inventory.scroll(delta.signum() as i32);
        self.session.send(Outgoing::HeldSlot(slot as i32));
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

/// The hotbar slot a number key picks, if it is one.
fn hotbar_digit(code: KeyCode) -> Option<usize> {
    Some(match code {
        KeyCode::Digit1 | KeyCode::Numpad1 => 0,
        KeyCode::Digit2 | KeyCode::Numpad2 => 1,
        KeyCode::Digit3 | KeyCode::Numpad3 => 2,
        KeyCode::Digit4 | KeyCode::Numpad4 => 3,
        KeyCode::Digit5 | KeyCode::Numpad5 => 4,
        KeyCode::Digit6 | KeyCode::Numpad6 => 5,
        KeyCode::Digit7 | KeyCode::Numpad7 => 6,
        KeyCode::Digit8 | KeyCode::Numpad8 => 7,
        KeyCode::Digit9 | KeyCode::Numpad9 => 8,
        _ => return None,
    })
}
