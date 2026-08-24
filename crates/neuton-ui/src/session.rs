//! A live connection, running off the UI thread.
//!
//! The connection thread does the meshing too. Chunks arrive in bursts of
//! hundreds and meshing one takes about a millisecond, so doing it on the
//! render thread would drop frames for a second solid every time the server
//! sends a batch. What reaches the main thread is geometry ready to upload.

use neuton_net::{Connection, Event};
use neuton_render::{Appearance, BlockTextures, Mesh};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// What the world view needs to know about.
pub enum WorldEvent {
    Joined { entity_id: i32, sections: usize, min_y: i32 },
    /// Geometry for one chunk column, already meshed.
    Chunk { x: i32, z: i32, mesh: Box<Mesh> },
    Forget { x: i32, z: i32 },
    Moved { x: f64, y: f64, z: f64, yaw: f32, pitch: f32 },
    Disconnected(String),
}

pub struct WorldSession {
    rx: Receiver<WorldEvent>,
    stop: Arc<AtomicBool>,
    pub server: String,
    pub chunks: u64,
    pub triangles: u64,
    pub status: String,
}

impl WorldSession {
    /// Connects in the background. Returns immediately.
    pub fn connect(
        host: String,
        port: u16,
        session: neuton_auth::Session,
        textures: Arc<BlockTextures>,
    ) -> Self {
        let (tx, rx) = channel();
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
            let appearance = Appearance::new();

            loop {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                match conn.poll() {
                    Ok(Event::Joined { entity_id, dimension }) => {
                        let _ = tx.send(WorldEvent::Joined {
                            entity_id,
                            sections: dimension.section_count(),
                            min_y: dimension.min_y,
                        });
                    }
                    Ok(Event::Chunk(chunk)) => {
                        let mesh = neuton_render::build(&chunk, &appearance, textures.as_ref());
                        if tx
                            .send(WorldEvent::Chunk {
                                x: chunk.x,
                                z: chunk.z,
                                mesh: Box::new(mesh),
                            })
                            .is_err()
                        {
                            return; // the window closed
                        }
                    }
                    Ok(Event::ChunkForgotten { x, z }) => {
                        let _ = tx.send(WorldEvent::Forget { x, z });
                    }
                    Ok(Event::Teleported { x, y, z, yaw, pitch }) => {
                        let _ = tx.send(WorldEvent::Moved { x, y, z, yaw, pitch });
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
            stop,
            server,
            chunks: 0,
            triangles: 0,
            status: "connecting...".to_string(),
        }
    }

    /// Takes whatever has arrived. Never blocks.
    pub fn drain(&mut self) -> Vec<WorldEvent> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    if let WorldEvent::Chunk { mesh, .. } = &event {
                        self.chunks += 1;
                        self.triangles += mesh.triangles() as u64;
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
