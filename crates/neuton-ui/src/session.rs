//! A live connection, running off the UI thread.
//!
//! The connection thread does the meshing too. Chunks arrive in bursts of
//! hundreds and meshing one takes about a millisecond, so doing it on the
//! render thread would drop frames for a second solid every time the server
//! sends a batch. What reaches the main thread is geometry ready to upload.

use neuton_net::items::Stack;
use neuton_net::{Connection, Event};
use neuton_render::{Appearance, BiomeTints, BlockTextures, Mesh, Neighbours};
use neuton_world::Chunk;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

/// What the world view needs to know about.
pub enum WorldEvent {
    Joined { entity_id: i32, sections: usize, min_y: i32 },
    /// Geometry for one chunk column, already meshed.
    ///
    /// The blocks come with it: collision needs them, and the meshing thread
    /// already holds them for re-meshing, so sharing costs a pointer.
    Chunk { x: i32, z: i32, mesh: Box<Mesh>, blocks: Arc<Chunk> },
    Forget { x: i32, z: i32 },
    Moved {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        relative: neuton_net::Relatives,
    },
    Chat(Vec<neuton_net::Span>),
    Abilities(neuton_world::physics::Abilities),
    /// A whole container's contents, the player's own inventory included.
    Container {
        window: i32,
        state_id: i32,
        slots: Vec<Option<Stack>>,
        carried: Option<Stack>,
    },
    /// One slot of one container.
    Slot { window: i32, state_id: i32, slot: i32, stack: Option<Stack> },
    /// The server picked a hotbar slot for us.
    HeldSlot(i32),
    Health { health: f32, food: i32 },
    /// A shove from the server: being hit, or thrown by an explosion.
    Knockback([f64; 3]),
    Died,
    /// Where the time went while the world arrived.
    Timing(Timing),
    Disconnected(String),
}

/// Something the player asked the connection to send.
pub enum Outgoing {
    Chat(String),
    Command(String),
    /// What changed since the last update. Both `None` reports only whether
    /// the player is standing on something, which is a change worth sending on
    /// its own.
    Move { position: Option<[f64; 3]>, rotation: Option<(f32, f32)>, on_ground: bool },
    /// Sent once the world has streamed in.
    Loaded,
    /// End of a client tick.
    TickEnd,
    /// The hotbar slot the player selected.
    HeldSlot(i32),
    /// The arm swing everyone else sees.
    Swing,
    /// Start breaking a block.
    StartBreaking { at: [i32; 3], face: u8 },
    /// Give up on a block part way through breaking it.
    AbortBreaking { at: [i32; 3] },
    /// Use what is in hand against a block: placing, opening, flipping.
    UseOn { at: [i32; 3], face: u8, cursor: [f32; 3] },
    /// Use what is in hand with nothing in front of it.
    Use { yaw: f32, pitch: f32 },
    /// Ask to be put back in the world after dying.
    Respawn,
    /// Click a slot in an open container.
    Click { window: i32, state_id: i32, slot: i16, button: u8, mode: i32 },
    /// Close an open container.
    CloseContainer { window: i32 },
}

/// How the world's arrival was spent, for telling network from client.
#[derive(Debug, Default, Clone, Copy)]
pub struct Timing {
    /// Wall time inside the mesher.
    pub meshing_ms: f64,
    /// Meshes produced, including re-meshes when a neighbour arrives.
    pub meshes: u64,
    /// Wall time blocked on the socket, waiting for the server to say
    /// something.
    pub waiting_ms: f64,
}

pub struct WorldSession {
    rx: Receiver<WorldEvent>,
    tx_out: Sender<Outgoing>,
    stop: Arc<AtomicBool>,
    pub server: String,
    /// Chunk meshes produced, including re-meshes when a neighbour arrives.
    pub chunks: u64,
    pub status: String,
}

impl WorldSession {
    /// Connects in the background. Returns immediately.
    pub fn connect(
        host: String,
        port: u16,
        session: neuton_auth::Session,
        textures: Arc<BlockTextures>,
        tints: Arc<neuton_assets::Tints>,
    ) -> Self {
        let (tx, rx) = channel();
        let (tx_out, rx_out) = channel::<Outgoing>();
        let stop = Arc::new(AtomicBool::new(false));
        let server = format!("{host}:{port}");

        let thread_stop = stop.clone();
        std::thread::spawn(move || {
            let mut conn = match Connection::join(&host, port, &session) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(WorldEvent::Disconnected(e.to_string()));
                    return;
                }
            };
            // Occlusion comes from the baked geometry rather than from block names.
            let appearance = Appearance::from_models(&textures);
            // Biome colours are only known once the server has sent its
            // registries, which it does before the first chunk.
            let biome_tints = BiomeTints::build(&conn.registries().biomes, &tints);
            // Chunks are kept, not just meshed and dropped. A column's edge
            // faces depend on what is next to it, so a chunk has to be re-meshed
            // when a neighbour turns up, and that needs the block data back.
            let mut world: HashMap<(i32, i32), Arc<Chunk>> = HashMap::new();
            let mut dirty: HashSet<(i32, i32)> = HashSet::new();
            // How many neighbours each column had when it was last meshed. A
            // chunk only needs re-meshing when that number goes up; without
            // this a column is meshed once per neighbour that arrives, which on
            // a fresh join is nearly three times more work than it needs.
            let mut meshed_with: HashMap<(i32, i32), usize> = HashMap::new();
            let meshers = Meshers::start(
                tx.clone(),
                Arc::new(appearance),
                textures.clone(),
                Arc::new(biome_tints),
                thread_stop.clone(),
            );
            let mut timing = Timing::default();
            let mut last_report = std::time::Instant::now();

            loop {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                // Anything the player typed. Sent before polling, since poll
                // blocks until the server says something.
                while let Ok(out) = rx_out.try_recv() {
                    let result = match &out {
                        Outgoing::Chat(text) => conn.send_chat(text),
                        Outgoing::Command(text) => conn.send_command(text),
                        Outgoing::Move { position, rotation, on_ground } => {
                            conn.send_movement(*position, *rotation, *on_ground)
                        }
                        Outgoing::Loaded => conn.send_loaded(),
                        Outgoing::TickEnd => conn.send_tick_end(),
                        Outgoing::HeldSlot(slot) => conn.send_held_slot(*slot),
                        Outgoing::Swing => conn.send_swing(),
                        Outgoing::StartBreaking { at, face } => {
                            conn.send_player_action(0, *at, *face)
                        }
                        Outgoing::AbortBreaking { at } => {
                            conn.send_player_action(1, *at, 1)
                        }
                        Outgoing::UseOn { at, face, cursor } => {
                            conn.send_use_item_on(*at, *face, *cursor, false)
                        }
                        Outgoing::Use { yaw, pitch } => conn.send_use_item(*yaw, *pitch),
                        Outgoing::Respawn => conn.send_respawn(),
                        Outgoing::Click { window, state_id, slot, button, mode } => {
                            conn.send_container_click(*window, *state_id, *slot, *button, *mode)
                        }
                        Outgoing::CloseContainer { window } => {
                            conn.send_close_container(*window)
                        }
                    };
                    if let Err(e) = result {
                        let _ = tx.send(WorldEvent::Disconnected(e.to_string()));
                        return;
                    }
                }

                // Re-mesh anything a new neighbour invalidated, but only once
                // the incoming burst has been drained: chunks arrive hundreds at
                // a time, and re-meshing on every single one would do the same
                // work four times over.
                // Waiting for the socket to go quiet is what keeps a column
                // from being meshed once per neighbour that arrives: on a join
                // that is four times the work for the same result.
                if !dirty.is_empty() && !conn.has_pending() {
                    let batch: Vec<(i32, i32)> = dirty.drain().collect();
                    for (x, z) in batch {
                        if !world.contains_key(&(x, z)) {
                            continue;
                        }
                        let present = neighbours_present(x, z, &world);
                        if meshed_with.get(&(x, z)).is_some_and(|had| *had >= present) {
                            continue;
                        }
                        meshed_with.insert((x, z), present);
                        if let Some(job) = MeshJob::new(x, z, &world) {
                            meshers.submit(job);
                        }
                    }
                }

                let waited = std::time::Instant::now();
                let polled = conn.poll();
                timing.waiting_ms += waited.elapsed().as_secs_f64() * 1000.0;
                if last_report.elapsed().as_millis() > 250 {
                    last_report = std::time::Instant::now();
                    timing.meshing_ms =
                        meshers.spent.0.load(Ordering::Relaxed) as f64 / 1000.0;
                    timing.meshes = meshers.spent.1.load(Ordering::Relaxed);
                    let _ = tx.send(WorldEvent::Timing(timing));
                }
                match polled {
                    Ok(Event::Joined { entity_id, dimension }) => {
                        let _ = tx.send(WorldEvent::Joined {
                            entity_id,
                            sections: dimension.section_count(),
                            min_y: dimension.min_y,
                        });
                    }
                    Ok(Event::Chunk(chunk)) => {
                        let (x, z) = (chunk.x, chunk.z);
                        world.insert((x, z), Arc::from(*chunk));
                        meshed_with.insert((x, z), neighbours_present(x, z, &world));
                        if let Some(job) = MeshJob::new(x, z, &world) {
                            meshers.submit(job);
                        }
                        // Its neighbours now know something they did not.
                        for (nx, nz) in [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                            if world.contains_key(&(nx, nz)) {
                                dirty.insert((nx, nz));
                            }
                        }
                    }
                    Ok(Event::Health { health, food, .. }) => {
                        let _ = tx.send(WorldEvent::Health { health, food });
                    }
                    Ok(Event::Knockback(v)) => {
                        let _ = tx.send(WorldEvent::Knockback(v));
                    }
                    Ok(Event::Died) => {
                        let _ = tx.send(WorldEvent::Died);
                    }
                    Ok(Event::BlocksChanged(changes)) => {
                        // A change on a column's edge changes what its
                        // neighbour's edge faces look like too.
                        for (at, state) in changes {
                            let column = (at[0].div_euclid(16), at[2].div_euclid(16));
                            let Some(chunk) = world.get_mut(&column) else { continue };
                            Arc::make_mut(chunk).set_state(
                                at[0].rem_euclid(16) as usize,
                                at[1],
                                at[2].rem_euclid(16) as usize,
                                neuton_blocks::StateId(state),
                            );
                            dirty.insert(column);
                            meshed_with.remove(&column);
                            for (nx, nz) in neighbours_of(at, column) {
                                if world.contains_key(&(nx, nz)) {
                                    dirty.insert((nx, nz));
                                    meshed_with.remove(&(nx, nz));
                                }
                            }
                        }
                    }
                    Ok(Event::ChunkForgotten { x, z }) => {
                        world.remove(&(x, z));
                        dirty.remove(&(x, z));
                        meshed_with.remove(&(x, z));
                        let _ = tx.send(WorldEvent::Forget { x, z });
                    }
                    Ok(Event::Teleported { x, y, z, yaw, pitch, relative }) => {
                        let _ = tx.send(WorldEvent::Moved { x, y, z, yaw, pitch, relative });
                    }
                    Ok(Event::Container { window, state_id, slots, carried, unread }) => {
                        if let Some(why) = unread {
                            // Worth saying out loud: it means a slot is missing
                            // from the screen, and it names what to add.
                            eprintln!("inventory: stopped reading slots, {why}");
                        }
                        let _ = tx.send(WorldEvent::Container {
                            window,
                            state_id,
                            slots,
                            carried,
                        });
                    }
                    Ok(Event::Slot { window, state_id, slot, stack }) => {
                        let _ = tx.send(WorldEvent::Slot { window, state_id, slot, stack });
                    }
                    Ok(Event::HeldSlot(slot)) => {
                        let _ = tx.send(WorldEvent::HeldSlot(slot));
                    }
                    Ok(Event::Abilities(abilities)) => {
                        if tx.send(WorldEvent::Abilities(abilities)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Chat(spans)) => {
                        if tx.send(WorldEvent::Chat(spans)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Disconnect(why)) => {
                        let _ = tx.send(WorldEvent::Disconnected(why));
                        return;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let _ = tx.send(WorldEvent::Disconnected(e.to_string()));
                        return;
                    }
                }
            }
        });

        Self {
            rx,
            tx_out,
            stop,
            server,
            chunks: 0,
            status: "connecting...".to_string(),
        }
    }

    /// Queues something to send. Dropped silently if the connection is gone,
    /// which the UI already learns about through a disconnect event.
    pub fn send(&self, out: Outgoing) {
        let _ = self.tx_out.send(out);
    }

    /// Takes whatever has arrived. Never blocks.
    pub fn drain(&mut self) -> Vec<WorldEvent> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    if matches!(event, WorldEvent::Chunk { .. }) {
                        self.chunks += 1;
                    }
                    if let WorldEvent::Disconnected(why) = &event {
                        self.status = format!("disconnected: {why}");
                    }
                    out.push(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.status.starts_with("disconnected") {
                        self.status = "connection closed".to_string();
                    }
                    break;
                }
            }
        }
        out
    }
}

impl Drop for WorldSession {
    fn drop(&mut self) {
        // Tells the thread to stop at its next packet. It may block in a read
        // until then, which is fine: it holds nothing the UI needs back.
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// How many of a column's four neighbours are loaded.
fn neighbours_present(x: i32, z: i32, world: &HashMap<(i32, i32), Arc<Chunk>>) -> usize {
    [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)]
        .iter()
        .filter(|key| world.contains_key(key))
        .count()
}

/// Meshes one column against whichever of its neighbours are loaded.
/// One column to mesh, with the neighbours its edge faces depend on.
///
/// The neighbours travel with the job rather than being looked up later,
/// because by the time a worker gets to it the world may have moved on.
struct MeshJob {
    x: i32,
    z: i32,
    chunk: Arc<Chunk>,
    around: [Option<Arc<Chunk>>; 4],
}

impl MeshJob {
    fn new(x: i32, z: i32, world: &HashMap<(i32, i32), Arc<Chunk>>) -> Option<Self> {
        let chunk = world.get(&(x, z))?.clone();
        let get = |dx: i32, dz: i32| world.get(&(x + dx, z + dz)).cloned();
        Some(Self {
            x,
            z,
            chunk,
            around: [get(-1, 0), get(1, 0), get(0, -1), get(0, 1)],
        })
    }
}

/// Meshing is where a joining client actually spends its time, and it is
/// perfectly parallel: one column at a time, sharing nothing but read-only
/// tables. Doing it on the thread that reads the socket also throttles the
/// world's arrival, because the server sizes its chunk batches from how long
/// the last one took to acknowledge.
struct Meshers {
    jobs: Sender<MeshJob>,
    /// Microseconds spent meshing across every worker, and columns produced.
    spent: Arc<(AtomicU64, AtomicU64)>,
}

impl Meshers {
    fn start(
        tx: Sender<WorldEvent>,
        appearance: Arc<Appearance>,
        textures: Arc<BlockTextures>,
        biomes: Arc<BiomeTints>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        let (jobs, rx) = channel::<MeshJob>();
        let rx = Arc::new(std::sync::Mutex::new(rx));
        let spent = Arc::new((AtomicU64::new(0), AtomicU64::new(0)));
        // One short of the machine's parallelism: the thread reading the socket
        // is doing real work too and should not be fighting for a core.
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).clamp(1, 8))
            .unwrap_or(3);
        for _ in 0..workers {
            let (rx, tx) = (rx.clone(), tx.clone());
            let (appearance, textures, biomes) =
                (appearance.clone(), textures.clone(), biomes.clone());
            let (spent, stop) = (spent.clone(), stop.clone());
            std::thread::spawn(move || {
                loop {
                    let job = {
                        let Ok(rx) = rx.lock() else { return };
                        match rx.recv() {
                            Ok(job) => job,
                            Err(_) => return,
                        }
                    };
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let started = std::time::Instant::now();
                    let neighbours = Neighbours {
                        west: job.around[0].as_deref(),
                        east: job.around[1].as_deref(),
                        north: job.around[2].as_deref(),
                        south: job.around[3].as_deref(),
                    };
                    let mesh = neuton_render::build_full(
                        &job.chunk,
                        neighbours,
                        appearance.as_ref(),
                        textures.as_ref(),
                        biomes.as_ref(),
                        1.0,
                    );
                    spent.0.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                    spent.1.fetch_add(1, Ordering::Relaxed);
                    if tx
                        .send(WorldEvent::Chunk {
                            x: job.x,
                            z: job.z,
                            mesh: Box::new(mesh),
                            blocks: job.chunk,
                        })
                        .is_err()
                    {
                        return; // the window closed
                    }
                }
            });
        }
        Self { jobs, spent }
    }

    fn submit(&self, job: MeshJob) {
        let _ = self.jobs.send(job);
    }
}

/// The columns a change at `at` also affects: only the ones it touches the edge
/// of, since an interior block cannot change a neighbour's outward faces.
fn neighbours_of(at: [i32; 3], column: (i32, i32)) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    let (x, z) = (at[0].rem_euclid(16), at[2].rem_euclid(16));
    if x == 0 {
        out.push((column.0 - 1, column.1));
    }
    if x == 15 {
        out.push((column.0 + 1, column.1));
    }
    if z == 0 {
        out.push((column.0, column.1 - 1));
    }
    if z == 15 {
        out.push((column.0, column.1 + 1));
    }
    out
}
