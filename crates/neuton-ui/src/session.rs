//! A live connection, running off the UI thread.
//!
//! The connection thread does the meshing too. Chunks arrive in bursts of
//! hundreds and meshing one takes about a millisecond, so doing it on the
//! render thread would drop frames for a second solid every time the server
//! sends a batch. What reaches the main thread is geometry ready to upload.

use neuton_net::{Connection, Event};
use neuton_render::{Appearance, BiomeTints, BlockTextures, Mesh, Neighbours};
use neuton_world::Chunk;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    Moved { x: f64, y: f64, z: f64, yaw: f32, pitch: f32 },
    Chat(Vec<neuton_net::Span>),
    Abilities(neuton_world::physics::Abilities),
    /// Where the time went while the world arrived.
    Timing(Timing),
    Disconnected(String),
}

/// Something the player asked the connection to send.
pub enum Outgoing {
    Chat(String),
    Command(String),
    /// What changed since the last update. Both `None` is a movement
    /// keep-alive.
    Move { position: Option<[f64; 3]>, rotation: Option<(f32, f32)> },
    /// Sent once the world has streamed in.
    Loaded,
    /// End of a client tick.
    TickEnd,
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
                        Outgoing::Move { position, rotation } => {
                            conn.send_movement(*position, *rotation, false)
                        }
                        Outgoing::Loaded => conn.send_loaded(),
                        Outgoing::TickEnd => conn.send_tick_end(),
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
                if !dirty.is_empty() && !conn.has_pending() {
                    let batch: Vec<(i32, i32)> = dirty.drain().collect();
                    for (x, z) in batch {
                        let Some(chunk) = world.get(&(x, z)) else { continue };
                        let present = neighbours_present(x, z, &world);
                        if meshed_with.get(&(x, z)).is_some_and(|had| *had >= present) {
                            continue;
                        }
                        meshed_with.insert((x, z), present);
                        let t = std::time::Instant::now();
                        let mesh =
                            mesh_chunk(chunk, &world, &appearance, &textures, &biome_tints);
                        timing.meshing_ms += t.elapsed().as_secs_f64() * 1000.0;
                        timing.meshes += 1;
                        let blocks = chunk.clone();
                        if tx
                            .send(WorldEvent::Chunk { x, z, mesh: Box::new(mesh), blocks })
                            .is_err()
                        {
                            return;
                        }
                    }
                }

                let waited = std::time::Instant::now();
                let polled = conn.poll();
                timing.waiting_ms += waited.elapsed().as_secs_f64() * 1000.0;
                if last_report.elapsed().as_millis() > 250 {
                    last_report = std::time::Instant::now();
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
                        let chunk = world.get(&(x, z)).expect("just inserted").clone();
                        let chunk = &chunk;
                        meshed_with.insert((x, z), neighbours_present(x, z, &world));
                        let t = std::time::Instant::now();
                        let mesh =
                            mesh_chunk(chunk, &world, &appearance, &textures, &biome_tints);
                        timing.meshing_ms += t.elapsed().as_secs_f64() * 1000.0;
                        timing.meshes += 1;
                        let blocks = chunk.clone();
                        if tx
                            .send(WorldEvent::Chunk { x, z, mesh: Box::new(mesh), blocks })
                            .is_err()
                        {
                            return; // the window closed
                        }
                        // Its neighbours now know something they did not.
                        for (nx, nz) in [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                            if world.contains_key(&(nx, nz)) {
                                dirty.insert((nx, nz));
                            }
                        }
                    }
                    Ok(Event::ChunkForgotten { x, z }) => {
                        world.remove(&(x, z));
                        dirty.remove(&(x, z));
                        meshed_with.remove(&(x, z));
                        let _ = tx.send(WorldEvent::Forget { x, z });
                    }
                    Ok(Event::Teleported { x, y, z, yaw, pitch }) => {
                        let _ = tx.send(WorldEvent::Moved { x, y, z, yaw, pitch });
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
fn mesh_chunk(
    chunk: &Chunk,
    world: &HashMap<(i32, i32), Arc<Chunk>>,
    appearance: &Appearance,
    textures: &BlockTextures,
    biomes: &BiomeTints,
) -> Mesh {
    let (x, z) = (chunk.x, chunk.z);
    let get = |dx: i32, dz: i32| world.get(&(x + dx, z + dz)).map(|c| c.as_ref());
    let neighbours = Neighbours {
        west: get(-1, 0),
        east: get(1, 0),
        north: get(0, -1),
        south: get(0, 1),
    };
    neuton_render::build_full(chunk, neighbours, appearance, textures, biomes, 1.0)
}
