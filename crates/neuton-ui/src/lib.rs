//! The launcher window.
//!
//! winit and wgpu are driven directly rather than through a framework, because
//! the same window and the same `wgpu::Device` will host the world renderer.
//! egui draws the launcher on top of a surface neuton owns.

pub mod app;
pub mod chat;
pub mod clicks;
pub mod entities;
pub mod block_models;
pub mod entity_render;
pub mod auth_task;
pub mod fonts;
pub mod gpu;
pub mod hand;
pub mod icons;
pub mod ping_task;
pub mod servers;
pub mod settings;
pub mod session;
pub mod inventory;
pub mod offline;
pub mod world_view;
pub mod theme;

use app::Launcher;
use gpu::Gpu;
use neuton_render::{BlockTextures, WorldRenderer};
use session::WorldSession;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};
use world_view::WorldView;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    run_with(None, None)
}

/// When the process started, for measuring how long it takes to be usable.
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Whether this process may take the pointer at all.
///
/// A grab on macOS is desktop-wide, so a run that takes the pointer and then
/// hangs leaves the whole machine without a cursor rather than just this
/// window. A screenshot run has nobody at the keyboard to give it back, so it
/// never asks for it in the first place; `NEUTON_NO_GRAB` says the same thing
/// for a run started by hand.
static GRAB_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The last frame's age in milliseconds, and whether the pointer is held.
///
/// Read by a thread that has no other way to know the frame loop has stopped.
static FRAME_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static POINTER_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn grab_allowed() -> bool {
    GRAB_ALLOWED.load(std::sync::atomic::Ordering::Relaxed)
        && std::env::var_os("NEUTON_NO_GRAB").is_none()
}

fn started() -> Instant {
    *START.get_or_init(Instant::now)
}

/// Opens straight into a world, skipping the launcher.
///
/// For development against a local server, where clicking through the launcher
/// on every rebuild is the slowest part of the loop.
pub fn run_direct(
    host: String,
    port: u16,
    session: neuton_auth::Session,
) -> Result<(), Box<dyn std::error::Error>> {
    run_with(Some(app::PendingJoin { host, port, session }), None)
}

/// Joins, waits for chunks to arrive, writes one frame to `path` and exits.
///
/// The renderer is otherwise only checkable by looking at a window, which is no
/// use from a script or a test.
pub fn run_screenshot(
    host: String,
    port: u16,
    session: neuton_auth::Session,
    path: std::path::PathBuf,
    after: std::time::Duration,
    view: Option<([f32; 3], f32, f32)>,
    bench_frames: u32,
    say: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Before anything can ask for it. Nothing about a screenshot needs the
    // pointer, and a run nobody is watching must not be able to take it.
    GRAB_ALLOWED.store(false, std::sync::atomic::Ordering::Relaxed);
    watchdog(after);
    let mut app = App {
        direct: Some(app::PendingJoin { host, port, session }),
        shot: Some((path, after)),
        view,
        bench: bench_frames,
        say,
        ..Default::default()
    };
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Kills a screenshot run that never finishes.
///
/// The deadline inside the frame loop only fires if frames are still arriving;
/// a hang in a connect, a driver call or a lock stops them. This thread is
/// outside all of that, so an automated run always ends on its own.
fn watchdog(after: std::time::Duration) {
    let grace = std::env::var("NEUTON_SHOT_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(std::time::Duration::from_secs(90));
    let limit = after + grace;
    std::thread::spawn(move || {
        std::thread::sleep(limit);
        eprintln!("neuton: screenshot run gave up after {:.0}s", limit.as_secs_f64());
        std::process::exit(2);
    });
}

/// Ends the process if the frame loop stops while the pointer is held.
///
/// Releasing a grab has to happen on the thread that owns the window, and a
/// hung main thread is exactly when that cannot be done -- so the only way out
/// left is to exit, which hands the pointer back to the desktop. A stall this
/// long is a dead client either way.
fn deadman() {
    const STALL: u64 = 30_000;
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let last = FRAME_TICK.load(std::sync::atomic::Ordering::Relaxed);
            let now = started().elapsed().as_millis() as u64;
            if last > 0
                && now.saturating_sub(last) > STALL
                && POINTER_HELD.load(std::sync::atomic::Ordering::Relaxed)
            {
                eprintln!("neuton: no frame in {STALL} ms with the pointer held; letting go by exiting");
                std::process::exit(3);
            }
        }
    });
}

fn run_with(
    direct: Option<app::PendingJoin>,
    shot: Option<(std::path::PathBuf, std::time::Duration)>,
) -> Result<(), Box<dyn std::error::Error>> {
    started();
    let event_loop = EventLoop::new()?;
    // Wait for input rather than spinning: an idle launcher should use no CPU.
    // A world requests its own redraws continuously.
    event_loop.set_control_flow(ControlFlow::Wait);
    deadman();
    let mut app = App { direct, shot, ..Default::default() };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[derive(Default)]
struct App {
    state: Option<State>,
    launcher: Option<Launcher>,
    /// Present once a world has been joined. The launcher stays alive
    /// underneath so leaving a world is instant.
    world: Option<WorldView>,
    renderer: Option<WorldRenderer>,
    /// Resolved once and shared with every connection thread.
    textures: Option<Arc<BlockTextures>>,
    /// Colormaps, shared with connection threads so they can resolve biomes.
    tints: Option<Arc<neuton_assets::Tints>>,
    last_frame: Option<Instant>,
    /// A join requested on the command line, applied once the window exists.
    direct: Option<app::PendingJoin>,
    /// Where to write a screenshot, and how long to wait for the world first.
    shot: Option<(std::path::PathBuf, std::time::Duration)>,
    /// A camera to force, rather than following the player's spawn. For looking
    /// at a particular thing from a script.
    view: Option<([f32; 3], f32, f32)>,
    /// Frames drawn after the screenshot deadline, before capturing.
    warmup: u32,
    /// Frames to time before capturing, if benchmarking.
    bench: u32,
    /// Lines to send on arrival, for testing the chat path.
    say: Vec<String>,
    started: Option<Instant>,
}

struct State {
    gpu: Gpu,
    egui_ctx: egui::Context,
    egui_winit: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("neuton")
            .with_inner_size(winit::dpi::LogicalSize::new(820.0, 620.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(560.0, 440.0));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("neuton: could not open a window: {e}");
                event_loop.exit();
                return;
            }
        };

        let gpu = match Gpu::new(window.clone()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("neuton: {e}");
                event_loop.exit();
                return;
            }
        };

        let egui_ctx = egui::Context::default();
        theme::apply(&egui_ctx);
        fonts::install(&egui_ctx);

        let egui_winit = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            gpu.format(),
            egui_wgpu::RendererOptions::default(),
        );

        self.launcher = Some(Launcher::new());
        self.state = Some(State { gpu, egui_ctx, egui_winit, egui_renderer });
        if std::env::var_os("NEUTON_TIMING").is_some() {
            eprintln!("startup: window and GPU ready in {:.0} ms", started().elapsed().as_secs_f64() * 1000.0);
        }

        if let Some(pending) = self.direct.take() {
            let Self { state, world, renderer, textures, tints, .. } = self;
            if let Some(state) = state {
                start_world(pending, state, world, renderer, textures, tints);
            }
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        // Raw motion rather than cursor position: once the pointer is locked
        // there is no position to read.
        if let DeviceEvent::MouseMotion { delta } = event
            && let Some(world) = &mut self.world
        {
            world.mouse_moved(delta.0 as f32, delta.1 as f32);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        // Destructured so the launcher, the world and the GPU can all be
        // borrowed at once; they are disjoint fields but the compiler cannot
        // see that through a method call on `self`.
        let Self {
            state, launcher, world, renderer, textures, tints, last_frame, started, ..
        } = self;
        let Some(state) = state else { return };
        let Some(launcher) = launcher else { return };

        // In a world, input belongs to the world. The overlay covers the whole
        // window, so letting egui see events first meant it swallowed every
        // click and the mouse could never be captured.
        let in_world = world.is_some();
        let typing = world.as_ref().is_some_and(|w| w.chat.is_open());
        // Every screen the player is meant to click around in, not just the
        // pause menu. A death screen whose button cannot be pressed is a dead
        // end, and the inventory is not much use if the slots ignore the mouse.
        let screen_open = world
            .as_ref()
            .is_some_and(|w| w.paused || w.dead || w.inventory.open);
        if !in_world || screen_open {
            let response = state.egui_winit.on_window_event(&state.gpu.window, &event);
            if response.repaint {
                state.gpu.window.request_redraw();
            }
            if response.consumed {
                return;
            }
        }
        let _ = typing;

        // Leaving the world is decided on the pause menu, which egui owns.
        if world.as_ref().is_some_and(|w| w.leaving) {
            *world = None;
            if let Some(r) = renderer.as_mut() {
                r.clear();
            }
            state.gpu.window.set_title("neuton");
            state.gpu.window.request_redraw();
            return;
        }

        // A join can only be started here, where the GPU lives.
        if let Some(pending) = launcher.pending_join.take() {
            start_world(pending, state, world, renderer, textures, tints);
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.gpu.resize(size.width, size.height);
                state.gpu.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                FRAME_TICK.store(
                    // Qualified: `started` is a local binding in here.
                    crate::started().elapsed().as_millis() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                let dt = last_frame
                    .replace(Instant::now())
                    .map(|t| t.elapsed().as_secs_f32())
                    .unwrap_or(1.0 / 60.0)
                    .min(0.25);

                if let (Some(w), Some(r)) = (world.as_mut(), renderer.as_mut()) {
                    w.update(dt, r, &state.gpu.device, &state.gpu.queue);
                    w.record_frame(dt);
                    // Dying is not something the player chose, so the mouse has
                    // to come back on its own: a locked pointer cannot press the
                    // button that gets you out of it. Same for opening a screen
                    // the player is meant to click around in. Only ever
                    // releasing here -- taking the pointer back is something
                    // closing the screen does, so this cannot fight the pause
                    // menu for it.
                    if !w.paused && (w.dead || w.inventory.open) && w.captured {
                        set_capture(&state.gpu.window, w, false);
                    }
                    // Settings that live outside the world view.
                    r.min_light = w.settings.min_light();
                    state.gpu.set_present_mode(r.wants_present_mode(w.settings.vsync));
                    let native = state.gpu.window.scale_factor() as f32;
                    state
                        .egui_ctx
                        .set_pixels_per_point(w.settings.effective_gui_scale(native));
                }
                let animating = world.is_some();

                // Screenshot mode: give the world time to stream in, take one
                // frame, then leave.
                if let Some((path, after)) = &self.shot {
                    let elapsed = started.get_or_insert_with(Instant::now).elapsed();
                    if elapsed >= *after {
                        // Anything the caller asked to be said on arrival, for
                        // checking the chat path without a person at the
                        // keyboard.
                        if let Some(w) = world.as_mut() {
                            for line in self.say.drain(..) {
                                match line.strip_prefix('/') {
                                    Some(cmd) if !cmd.is_empty() => w
                                        .session
                                        .send(session::Outgoing::Command(cmd.to_string())),
                                    _ => w.session.send(session::Outgoing::Chat(line)),
                                }
                            }
                        }

                        // Opens a menu for the screenshot, so the interface can
                        // be looked at without a person pressing escape.
                        if let Some(w) = world.as_mut()
                            && let Ok(which) = std::env::var("NEUTON_SHOW_MENU")
                        {
                            w.paused = true;
                            w.settings_open = which == "settings";
                        }

                        if let (Some(w), Some(view)) = (world.as_mut(), self.view) {
                            w.view_override = Some(view);
                        }
                        // A few ordinary frames first. The panel reports the
                        // previous frame's chunk count and frame time, and
                        // capturing straight after placing the camera would put
                        // "0 chunks" and a nonsense frame rate in the file.
                        if self.warmup < 3 {
                            self.warmup += 1;
                            // Through the off-screen path, not the window. A
                            // window that is behind another is reported as
                            // occluded and its frame is skipped, which is right
                            // for a game and useless for a warm-up.
                            let _ = capture_frame(
                                state,
                                launcher,
                                world.as_mut(),
                                renderer.as_mut(),
                                state.gpu.config.width,
                                state.gpu.config.height,
                            );
                            state.gpu.window.request_redraw();
                            return;
                        }
                        // The window's own size, so the capture matches what
                        // is on screen rather than laying the interface out for
                        // one resolution and rendering it at another.
                        let (width, height) = (state.gpu.config.width, state.gpu.config.height);
                        let shot = capture_frame(
                            state,
                            launcher,
                            world.as_mut(),
                            renderer.as_mut(),
                            width,
                            height,
                        );
                        if std::env::var_os("NEUTON_TIMING").is_some()
                            && let Some(w) = world.as_ref()
                        {
                            let ms = |t: Option<Instant>| {
                                t.map(|t| (t - w.joined_at).as_secs_f64() * 1000.0)
                                    .unwrap_or(f64::NAN)
                            };
                            let span = ms(w.last_chunk) - ms(w.first_chunk);
                            eprintln!(
                                "world: first chunk {:.0} ms after connecting, last {:.0} ms, {} columns",
                                ms(w.first_chunk),
                                ms(w.last_chunk),
                                renderer.as_ref().map(|r| r.chunk_count()).unwrap_or(0),
                            );
                            eprintln!(
                                "world: of that {:.0} ms spent meshing ({} meshes) and {:.0} ms waiting on the server",
                                w.timing.meshing_ms,
                                w.timing.meshes,
                                w.timing.waiting_ms,
                            );
                            eprintln!(
                                "world: streaming took {:.1} s, meshing was {:.0}% of it",
                                span / 1000.0,
                                w.timing.meshing_ms / span.max(1.0) * 100.0,
                            );
                        }

                        if self.bench > 0 {
                            let ms = bench(state, launcher, world, renderer, self.bench);
                            let tris = renderer.as_ref().map(|r| r.triangles()).unwrap_or(0);
                            println!(
                                "bench: {ms:.2} ms/frame ({:.0} fps) at {}x{}, {} chunks, {:.2}M triangles",
                                1000.0 / ms,
                                state.gpu.config.width,
                                state.gpu.config.height,
                                renderer.as_ref().map(|r| r.drawn.get()).unwrap_or(0),
                                tris as f64 / 1.0e6,
                            );
                        }

                        match shot {
                            Some(pixels) => {
                                let png =
                                    neuton_render::png::encode_rgba(&pixels, width, height);
                                match std::fs::write(path, png) {
                                    Ok(()) => {
                                        let held = renderer
                                            .as_ref()
                                            .map(|r| (r.drawn.get(), r.chunk_count()))
                                            .unwrap_or((0, 0));
                                        println!(
                                            "wrote {} ({}/{} chunks drawn)",
                                            path.display(),
                                            held.0,
                                            held.1
                                        );
                                    }
                                    Err(e) => eprintln!("neuton: could not write: {e}"),
                                }
                            }
                            None => eprintln!("neuton: capture failed"),
                        }
                        event_loop.exit();
                        return;
                    }
                }

                draw(state, launcher, world.as_mut(), renderer.as_mut());

                // A world animates continuously; the launcher alone repaints
                // only when something happens.
                if animating || self.shot.is_some() {
                    // A frame cap only means anything without vsync, which is
                    // already pacing to the display.
                    if let Some(w) = world.as_ref()
                        && !w.settings.vsync
                        && w.settings.max_fps > 0
                    {
                        let target = std::time::Duration::from_secs_f64(
                            1.0 / w.settings.max_fps as f64,
                        );
                        let spent = last_frame.map(|t| t.elapsed()).unwrap_or_default();
                        if let Some(remaining) = target.checked_sub(spent) {
                            std::thread::sleep(remaining);
                        }
                    }
                    state.gpu.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;

                // Rebinding swallows the next key, so a menu shortcut is not
                // triggered by the key being bound.
                if pressed
                    && let Some(w) = world.as_mut()
                    && let Some(action) = w.rebinding
                    && let PhysicalKey::Code(code) = event.physical_key
                {
                    w.rebinding = None;
                    if code != KeyCode::Escape {
                        w.settings.keys.set(action, code);
                        if let Err(e) = w.settings.save() {
                            eprintln!("neuton: could not save settings: {e}");
                        }
                    }
                    state.gpu.window.request_redraw();
                    return;
                }

                // Typed characters, before key codes: a text field wants what
                // the layout produced, not which physical key was pressed.
                if pressed
                    && let Some(w) = world.as_mut()
                    && w.chat.is_open()
                {
                    if matches!(event.physical_key, PhysicalKey::Code(KeyCode::Backspace)) {
                        w.backspace();
                        state.gpu.window.request_redraw();
                        return;
                    }
                    if let Some(text) = &event.text {
                        w.type_text(text);
                        state.gpu.window.request_redraw();
                    }
                }

                let PhysicalKey::Code(code) = event.physical_key else { return };
                if let Some(w) = world.as_mut() {
                    // Escape while typing cancels the message, not the world.
                    if pressed && code == KeyCode::Escape && w.chat.is_open() {
                        w.key(code, pressed, event.repeat);
                        set_capture(&state.gpu.window, w, true);
                        state.gpu.window.request_redraw();
                        return;
                    }
                    // Escape backs out of settings before it closes the menu.
                    if pressed && code == KeyCode::Escape && w.settings_open {
                        w.settings_open = false;
                        state.gpu.window.request_redraw();
                        return;
                    }
                    // Escape closes the inventory before it reaches the pause
                    // menu, as it does in the game.
                    if pressed && code == KeyCode::Escape && w.inventory.open {
                        w.toggle_inventory();
                        set_capture(&state.gpu.window, w, true);
                        return;
                    }
                    if pressed && code == KeyCode::Escape {
                        // Escape opens the pause menu and releases the mouse,
                        // as it does in the game. Leaving is a choice on that
                        // menu rather than a second press, which used to drop
                        // you out of the world by accident.
                        w.paused = !w.paused;
                        set_capture(&state.gpu.window, w, !w.paused);
                        state.gpu.window.request_redraw();
                        return;
                    }
                    // Opening chat releases the mouse so the pointer comes
                    // back and typing does not also fly the camera.
                    let was_typing = w.chat.is_open();
                    // While paused, the world takes no input.
                    if w.paused {
                        return;
                    }
                    let changed_screen = w.key(code, pressed, event.repeat);
                    if changed_screen || (was_typing && !w.chat.is_open()) {
                        // A key that opened or closed a screen decides who has
                        // the pointer. Asking what is open now, rather than
                        // assuming the key opened something, is what lets the
                        // same key close it again and give the pointer back.
                        let screen = w.chat.is_open() || w.inventory.open || w.dead;
                        set_capture(&state.gpu.window, w, !screen);
                    }
                    state.gpu.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state: button_state, button, .. } => {
                if let Some(w) = world.as_mut() {
                    let pressed = button_state == ElementState::Pressed;
                    if pressed && !w.captured && !w.chat.is_open() && !w.dead
                        && !w.inventory.open
                    {
                        // The first click is what takes the mouse, not an
                        // action in the world. It is also the player asking for
                        // the pointer by hand, which is what clears a grab the
                        // client gave up on.
                        w.allow_capture_again();
                        set_capture(&state.gpu.window, w, true);
                    } else if w.captured && !w.paused {
                        w.mouse_button(button, pressed);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(w) = world.as_mut()
                    && w.captured
                    && !w.paused
                {
                    let steps = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                        winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    w.scroll(steps);
                }
            }
            WindowEvent::Focused(false) => {
                if let Some(w) = world.as_mut() {
                    w.release_all();
                    set_capture(&state.gpu.window, w, false);
                }
            }
            _ => {}
        }
    }
}

/// Builds the atlas if needed, then starts a connection.
fn start_world(
    pending: app::PendingJoin,
    state: &mut State,
    world: &mut Option<WorldView>,
    renderer: &mut Option<WorldRenderer>,
    textures: &mut Option<Arc<BlockTextures>>,
    tints: &mut Option<Arc<neuton_assets::Tints>>,
) {
    {
        // Resolving the atlas takes a few hundred milliseconds and only has to
        // happen once, so it is done on the first join rather than at startup,
        // which keeps the launcher instant.
        if textures.is_none() {
            let mut packs = neuton_assets::PackStack::new();
            match neuton_assets::vanilla_jar("26.2") {
                Some(jar) => {
                    let _ = packs.push(jar);
                }
                None => {
                    eprintln!("neuton: no vanilla 26.2 installation found, cannot load textures");
                    return;
                }
            }
            if let Some(dir) = neuton_assets::resource_pack_dir() {
                for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                    let _ = packs.push(entry.path());
                }
            }
            *tints = Some(Arc::new(neuton_assets::Tints::load(&mut packs)));
            let t = Instant::now();
            *textures = Some(Arc::new(BlockTextures::build(&mut packs)));
            if std::env::var_os("NEUTON_TIMING").is_some() {
                eprintln!(
                    "startup: resource packs resolved and atlas stitched in {:.0} ms",
                    t.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
        let atlas = textures.clone().unwrap();

        if renderer.is_none() {
            *renderer = Some(WorldRenderer::new(
                &state.gpu.device,
                &state.gpu.queue,
                state.gpu.format(),
                state.gpu.config.width,
                state.gpu.config.height,
                &atlas,
            ));
        }

        state
            .gpu
            .window
            .set_title(&format!("neuton - {}:{}", pending.host, pending.port));
        let session = WorldSession::connect(
            pending.host,
            pending.port,
            pending.session,
            atlas.clone(),
            tints.clone().unwrap_or_default(),
        );
        let mut view = WorldView::new(session, atlas.clone());
        set_capture(&state.gpu.window, &mut view, true);
        *world = Some(view);
        state.gpu.window.request_redraw();
    }
}

/// Locks or releases the pointer for mouse look.
fn set_capture(window: &Window, world: &mut WorldView, capture: bool) {
    // Asking for the state we are already in is not a reason to talk to the
    // window system again. Pointer grab and cursor visibility are desktop-wide
    // on macOS, so a caller that runs every frame has to cost nothing.
    if capture == world.captured {
        return;
    }
    if capture {
        if world.capture_gave_up {
            return;
        }
        if !grab_allowed() {
            // Looking around comes from raw device motion, which arrives
            // whether or not the pointer is pinned, so the only thing given up
            // here is the pointer itself.
            world.captured = true;
            return;
        }
        if world.grab_is_runaway(Instant::now()) {
            // Whatever is asking this often is wrong, and the cost of being
            // wrong about the pointer is a cursor no window can get back.
            world.capture_gave_up = true;
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
            world.captured = false;
            POINTER_HELD.store(false, std::sync::atomic::Ordering::Relaxed);
            world
                .chat
                .note("Something kept grabbing the mouse; click to take it back.");
            return;
        }
        // Locked is what a game wants; some platforms only offer Confined.
        let grabbed = window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
            .is_ok();
        window.set_cursor_visible(!grabbed);
        // Looking around is driven by raw device motion, which arrives whether
        // or not the platform would let us pin the pointer. Refusing to look
        // just because the grab failed would be worse than a cursor that
        // wanders off the window.
        world.captured = true;
        POINTER_HELD.store(grabbed, std::sync::atomic::Ordering::Relaxed);
        if !grabbed {
            world.chat.note("Could not lock the mouse pointer; look still works.");
        }
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
        world.captured = false;
        POINTER_HELD.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Somewhere other than the window to draw into.
struct CaptureTarget<'a> {
    view: &'a wgpu::TextureView,
    width: u32,
    height: u32,
}

fn draw(
    state: &mut State,
    launcher: &mut Launcher,
    world: Option<&mut WorldView>,
    renderer: Option<&mut WorldRenderer>,
) {
    draw_into(state, launcher, world, renderer, None);
}

/// Draws one frame, to the window or to `capture`.
///
/// A screenshot goes through here rather than rendering the world on its own,
/// so what lands in the file is the frame a player would see, interface
/// included.
fn draw_into(
    state: &mut State,
    launcher: &mut Launcher,
    mut world: Option<&mut WorldView>,
    renderer: Option<&mut WorldRenderer>,
    capture: Option<CaptureTarget<'_>>,
) {
    use wgpu::CurrentSurfaceTexture as Cst;

    let raw_input = state.egui_winit.take_egui_input(&state.gpu.window);
    let hud = world.as_ref().zip(renderer.as_ref()).map(|(w, r)| Hud {
        debug: w.show_debug.then(|| w.debug_lines(r)),
        chat: w.chat.visible().cloned().collect(),
        input: w.chat.input().map(str::to_string),
        paused: w.paused,
        server: w.session.server.clone(),
        settings: w.settings_open.then(|| w.settings.clone()),
        rebinding: w.rebinding,
        dead: w.dead,
    });
    let mut pause_action = PauseAction::None;
    let mut output = state.egui_ctx.run_ui(raw_input, |ui| match &hud {
        // In a world, egui draws only the overlay.
        Some(hud) => pause_action = overlay(ui, hud, world.as_deref_mut()),
        None => launcher.update(ui),
    });
    if let Some(w) = world.as_mut() {
        match pause_action {
            PauseAction::Resume => {
                w.paused = false;
                w.settings_open = false;
            }
            PauseAction::Leave => w.leaving = true,
            PauseAction::Respawn => {
                w.respawn();
                set_capture(&state.gpu.window, w, true);
            }
            PauseAction::OpenSettings => w.settings_open = true,
            PauseAction::CloseSettings => {
                w.settings_open = false;
                w.rebinding = None;
            }
            PauseAction::Apply(settings) => {
                w.settings = *settings;
                if let Err(e) = w.settings.save() {
                    eprintln!("neuton: could not save settings: {e}");
                }
            }
            PauseAction::Rebind(action) => w.rebinding = Some(action),
            PauseAction::None => {}
        }
    }
    state.egui_winit.handle_platform_output(&state.gpu.window, output.platform_output.clone());

    let pixels_per_point = state.egui_ctx.pixels_per_point();
    let tris = state.egui_ctx.tessellate(output.shapes, pixels_per_point);

    // Texture deltas are applied before the frame is acquired, and the delta
    // set is consumed either way. egui asserts on a dropped delta it never
    // handed to a renderer, so any path that skips a frame must still take
    // them, not just the path that draws.
    for (id, deltas) in &output.textures_delta.set {
        for delta in deltas {
            state.egui_renderer.update_texture(&state.gpu.device, &state.gpu.queue, *id, delta);
        }
    }
    let to_free = std::mem::take(&mut output.textures_delta.free);
    output.textures_delta.clear();

    let free_all = |renderer: &mut egui_wgpu::Renderer| {
        for id in &to_free {
            renderer.free_texture(id);
        }
    };

    // An offscreen target skips the surface entirely: there is nothing to
    // acquire and nothing to present.
    let frame = if capture.is_some() {
        None
    } else {
        Some(match state.gpu.surface.get_current_texture() {
        Cst::Success(f) => f,
        // Usable, but the surface wants reconfiguring before the next frame.
        Cst::Suboptimal(f) => {
            let (w, h) = (state.gpu.config.width, state.gpu.config.height);
            state.gpu.resize(w, h);
            f
        }
        // A resize or display change invalidated the surface. Reconfigure and
        // let the next redraw pick it up.
        Cst::Outdated | Cst::Lost => {
            let (w, h) = (state.gpu.config.width, state.gpu.config.height);
            state.gpu.resize(w, h);
            free_all(&mut state.egui_renderer);
            return;
        }
        // Minimised, or the compositor is busy. Skipping a frame is correct.
        Cst::Timeout | Cst::Occluded => {
            free_all(&mut state.egui_renderer);
            return;
        }
        Cst::Validation => {
            eprintln!("neuton: surface validation error while acquiring a frame");
            free_all(&mut state.egui_renderer);
            return;
        }
        })
    };

    let owned_view = frame
        .as_ref()
        .map(|f| f.texture.create_view(&wgpu::TextureViewDescriptor::default()));
    let view = match (&owned_view, &capture) {
        (Some(v), _) => v,
        (None, Some(c)) => c.view,
        (None, None) => return,
    };
    let (target_width, target_height) = match &capture {
        Some(c) => (c.width, c.height),
        None => (state.gpu.config.width, state.gpu.config.height),
    };
    let mut encoder = state
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });

    // The world goes down first and clears the frame; egui composites over it.
    let world_drawn = match (world, renderer) {
        (Some(w), Some(r)) => {
            r.resize(&state.gpu.device, target_width, target_height);
            let mut camera = w.camera.clone();
            camera.aspect = target_width as f32 / target_height.max(1) as f32;
            r.render(&mut encoder, &state.gpu.queue, view, &camera);
            true
        }
        _ => false,
    };

    let desc = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [target_width, target_height],
        pixels_per_point,
    };
    state.egui_renderer.update_buffers(
        &state.gpu.device,
        &state.gpu.queue,
        &mut encoder,
        &tris,
        &desc,
    );

    {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Only clear when there is no world underneath to preserve.
                    load: if world_drawn {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.031,
                            g: 0.035,
                            b: 0.043,
                            a: 1.0,
                        })
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            ..Default::default()
        });
        state.egui_renderer.render(&mut pass.forget_lifetime(), &tris, &desc);
    }

    state.gpu.queue.submit(Some(encoder.finish()));
    // Freed only after submit. The render pass above still references these
    // textures, and destroying them first fails validation at submit time.
    free_all(&mut state.egui_renderer);
    // Presentation moved onto the queue in wgpu 30.
    if let Some(frame) = frame {
        state.gpu.queue.present(frame);
    }
}

/// What the in-world overlay draws.
struct Hud {
    /// Present when the debug panel is up.
    debug: Option<Vec<String>>,
    chat: Vec<Vec<neuton_net::Span>>,
    /// The line being typed, if chat is open.
    input: Option<String>,
    paused: bool,
    server: String,
    /// Present while the settings screen is open, so it can be edited in place.
    settings: Option<crate::settings::Settings>,
    /// The action waiting for a key press, if the player is rebinding one.
    rebinding: Option<crate::settings::Action>,
    /// Set while the player is dead and waiting to come back.
    dead: bool,
}

/// What the player picked on the pause menu.
enum PauseAction {
    None,
    Resume,
    Leave,
    OpenSettings,
    CloseSettings,
    Respawn,
    /// Settings were changed and should be applied and saved.
    Apply(Box<crate::settings::Settings>),
    Rebind(crate::settings::Action),
}

/// The in-world overlay: a crosshair, and the debug panel when it is up.
fn overlay(ui: &mut egui::Ui, hud: &Hud, world: Option<&mut WorldView>) -> PauseAction {
    let mut action = PauseAction::None;
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            if !hud.paused {
                crosshair(ui);
            }
            if let Some(world) = world {
                // The hotbar stays up while paused, as it does in the game: the
                // pause menu sits over the world, it does not replace it.
                let scale = crate::inventory::interface_scale(ui.clip_rect().size());
                let (health, food, survival) =
                    (world.health, world.food, !world.abilities.instant_build);
                // Two and a bit strides to a full cycle, as the game bobs.
                let bob = world.walked * 2.2;
                let open = world.inventory.open;
                let creative = world.abilities.instant_build;
                let mut clicked = Vec::new();
                {
                    let WorldView { inventory, cursor, art, .. } = world;
                    if open {
                        clicked = crate::inventory::screen(
                            ui, inventory, cursor, art, None, scale, creative,
                        );
                    } else {
                        crate::inventory::held_item(ui, inventory, art, scale, bob);
                    }
                    crate::inventory::hotbar(ui, inventory, art, scale);
                    // Creative shows neither: nothing can hurt you and nothing
                    // makes you hungry.
                    if survival {
                        crate::inventory::vitals(ui, art, scale, health, food);
                    }
                }
                for click in clicked {
                    world.act(click);
                }
            }

            let Some(lines) = &hud.debug else { return };
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                for line in lines {
                    if line.is_empty() {
                        ui.add_space(6.0);
                        continue;
                    }
                    // Each line on its own shaded strip, as the game does, so
                    // text stays readable over both sky and terrain.
                    egui::Frame::new()
                        .fill(egui::Color32::from_black_alpha(130))
                        .inner_margin(egui::Margin::symmetric(4, 1))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(line)
                                    .monospace()
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(0xE8, 0xE8, 0xE8)),
                            );
                        });
                }
            });

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                // Chat sits above the hotbar rather than behind it.
                ui.add_space(24.0 * crate::inventory::interface_scale(ui.clip_rect().size()));
                chat_panel(ui, hud);
            });

            if hud.dead {
                action = death_screen(ui);
            } else if hud.paused {
                action = match &hud.settings {
                    Some(settings) => settings_menu(ui, settings, hud.rebinding),
                    None => pause_menu(ui, hud),
                };
            }
        });
    action
}

/// Shown when the player has died, in place of the pause menu.
fn death_screen(ui: &mut egui::Ui) -> PauseAction {
    let mut action = PauseAction::None;
    // The game tints the world red rather than dimming it.
    ui.painter().rect_filled(
        ui.clip_rect(),
        0.0,
        egui::Color32::from_rgba_unmultiplied(0x7F, 0x00, 0x00, 0xB0),
    );
    egui::Window::new("died")
        .title_bar(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(egui::Frame::NONE)
        .show(ui.ctx(), |ui| {
            ui.set_width(260.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("You died")
                        .size(34.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                );
                ui.add_space(20.0);
                let wide = egui::vec2(ui.available_width(), 34.0);
                if ui.add_sized(wide, egui::Button::new("Respawn")).clicked() {
                    action = PauseAction::Respawn;
                }
                ui.add_space(6.0);
                if ui.add_sized(wide, egui::Button::new("Disconnect")).clicked() {
                    action = PauseAction::Leave;
                }
            });
        });
    action
}

/// The pause menu.
fn pause_menu(ui: &mut egui::Ui, hud: &Hud) -> PauseAction {
    let mut action = PauseAction::None;
    // Dimmed, so it is obvious the world is not taking input.
    ui.painter().rect_filled(
        ui.clip_rect(),
        0.0,
        egui::Color32::from_black_alpha(140),
    );

    egui::Window::new("Paused")
        .title_bar(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(
            egui::Frame::new()
                .fill(theme::RAISE)
                .stroke(egui::Stroke::new(1.0, theme::LINE2))
                .corner_radius(12)
                .inner_margin(egui::Margin::same(22)),
        )
        .show(ui.ctx(), |ui| {
            ui.set_width(260.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("Paused").size(20.0).strong().color(theme::FG));
                ui.label(
                    egui::RichText::new(&hud.server)
                        .monospace()
                        .size(12.0)
                        .color(theme::DIM),
                );
                ui.add_space(16.0);

                let wide = egui::vec2(ui.available_width(), 32.0);
                if ui.add_sized(wide, egui::Button::new("Back to game")).clicked() {
                    action = PauseAction::Resume;
                }
                ui.add_space(6.0);
                if ui.add_sized(wide, egui::Button::new("Settings")).clicked() {
                    action = PauseAction::OpenSettings;
                }
                ui.add_space(6.0);
                if ui.add_sized(wide, egui::Button::new("Disconnect")).clicked() {
                    action = PauseAction::Leave;
                }
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("escape to go back")
                        .monospace()
                        .size(11.0)
                        .color(theme::DIM),
                );
            });
        });
    action
}

/// A thin cross at the centre of the screen.
///
/// Drawn as light strokes with a dark backing rather than the game's inverting
/// blend, which egui cannot express; the outline is what keeps it visible
/// against both a bright sky and dark stone.
fn crosshair(ui: &mut egui::Ui) {
    // The panel fills the window, so its clip rect is the screen.
    let rect = ui.clip_rect();
    let centre = rect.center();
    let painter = ui.painter();

    const ARM: f32 = 7.0;
    const THICK: f32 = 1.5;
    let arms = [
        egui::Rect::from_min_max(
            egui::pos2(centre.x - ARM, centre.y - THICK / 2.0),
            egui::pos2(centre.x + ARM, centre.y + THICK / 2.0),
        ),
        egui::Rect::from_min_max(
            egui::pos2(centre.x - THICK / 2.0, centre.y - ARM),
            egui::pos2(centre.x + THICK / 2.0, centre.y + ARM),
        ),
    ];
    for arm in arms {
        painter.rect_filled(arm.expand(1.0), 0.0, egui::Color32::from_black_alpha(90));
    }
    for arm in arms {
        painter.rect_filled(arm, 0.0, egui::Color32::from_white_alpha(220));
    }
}

/// Times how long the renderer takes per frame, off-screen.
///
/// Measured without the read-back a screenshot needs: copying a full frame into
/// a buffer and waiting on the GPU costs more than drawing it, so timing the
/// screenshot path measures the screenshot machinery rather than the renderer.
fn bench(
    state: &mut State,
    launcher: &mut Launcher,
    world: &mut Option<WorldView>,
    renderer: &mut Option<WorldRenderer>,
    frames: u32,
) -> f64 {
    let (width, height) = (state.gpu.config.width, state.gpu.config.height);
    let texture = state.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: state.gpu.format(),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    // One frame first, so shader compilation and buffer warm-up are not
    // counted as rendering.
    draw_into(
        state,
        launcher,
        world.as_mut(),
        renderer.as_mut(),
        Some(CaptureTarget { view: &view, width, height }),
    );
    let _ = state.gpu.device.poll(wgpu::PollType::wait_indefinitely());

    let start = Instant::now();
    for _ in 0..frames {
        draw_into(
            state,
            launcher,
            world.as_mut(),
            renderer.as_mut(),
            Some(CaptureTarget { view: &view, width, height }),
        );
    }
    // Waited on once at the end rather than per frame: syncing every frame
    // would serialise the CPU against the GPU and measure the stall.
    let _ = state.gpu.device.poll(wgpu::PollType::wait_indefinitely());
    start.elapsed().as_secs_f64() * 1000.0 / frames as f64
}

/// Renders one whole frame off-screen and reads it back as RGBA8.
fn capture_frame(
    state: &mut State,
    launcher: &mut Launcher,
    world: Option<&mut WorldView>,
    renderer: Option<&mut WorldRenderer>,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let format = state.gpu.format();
    let texture = state.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("capture"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    draw_into(
        state,
        launcher,
        world,
        renderer,
        Some(CaptureTarget { view: &view, width, height }),
    );

    // Texture copies need rows aligned to 256 bytes, so the buffer is usually
    // wider than the image and is trimmed on the way out.
    let unpadded = width * 4;
    let padded = unpadded.div_ceil(256) * 256;
    let buffer = state.gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("capture readback"),
        size: (padded * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = state
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    state.gpu.queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    state.gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    rx.recv().ok()?.ok()?;

    let mapped = slice.get_mapped_range().ok()?;
    let mut out = Vec::with_capacity((unpadded * height) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        out.extend_from_slice(&mapped[start..start + unpadded as usize]);
    }
    drop(mapped);
    buffer.unmap();

    // Surfaces are usually BGRA; a PNG is RGBA.
    if matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    ) {
        for px in out.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    Some(out)
}

/// The chat log, and the input line when it is open.
fn chat_panel(ui: &mut egui::Ui, hud: &Hud) {
    // Bottom-up layout, so the input sits below the log and the log grows
    // upwards the way it does in the game.
    if let Some(input) = &hud.input {
        egui::Frame::new()
            .fill(egui::Color32::from_black_alpha(170))
            .inner_margin(egui::Margin::symmetric(6, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{input}_"))
                            .monospace()
                            .size(13.0)
                            .color(egui::Color32::WHITE),
                    );
                });
            });
        ui.add_space(2.0);
    }

    for spans in hud.chat.iter().rev() {
        egui::Frame::new()
            .fill(egui::Color32::from_black_alpha(120))
            .inner_margin(egui::Margin::symmetric(6, 1))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for span in spans {
                        let mut text = egui::RichText::new(&span.text).monospace().size(12.5);
                        text = match span.color {
                            Some([r, g, b]) => text.color(egui::Color32::from_rgb(r, g, b)),
                            None => text.color(egui::Color32::from_rgb(0xE8, 0xE8, 0xE8)),
                        };
                        if span.bold {
                            text = text.strong();
                        }
                        if span.italic {
                            text = text.italics();
                        }
                        if span.strikethrough {
                            text = text.strikethrough();
                        }
                        if span.underlined {
                            text = text.underline();
                        }
                        ui.label(text);
                    }
                });
            });
    }
}

/// The settings screen.
///
/// Changes apply as they are made rather than on a confirm button: a field that
/// only takes effect later makes it impossible to tell whether it worked.
fn settings_menu(
    ui: &mut egui::Ui,
    current: &crate::settings::Settings,
    rebinding: Option<crate::settings::Action>,
) -> PauseAction {
    use crate::settings::Action;

    let mut action = PauseAction::None;
    let mut settings = current.clone();
    let mut changed = false;

    ui.painter().rect_filled(ui.clip_rect(), 0.0, egui::Color32::from_black_alpha(170));

    egui::Window::new("Settings")
        .title_bar(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(
            egui::Frame::new()
                .fill(theme::RAISE)
                .stroke(egui::Stroke::new(1.0, theme::LINE2))
                .corner_radius(12)
                .inner_margin(egui::Margin::same(20)),
        )
        .show(ui.ctx(), |ui| {
            ui.set_width(430.0);
            ui.label(egui::RichText::new("Settings").size(19.0).strong().color(theme::FG));
            ui.add_space(12.0);

            egui::ScrollArea::vertical().max_height(430.0).show(ui, |ui| {
                section(ui, "VIDEO");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut settings.fov, 30.0..=120.0)
                            .text("Field of view")
                            .suffix("\u{00b0}"),
                    )
                    .changed();
                changed |= ui.checkbox(&mut settings.vsync, "Wait for the display (vsync)").changed();
                if !settings.vsync {
                    let mut cap = settings.max_fps as f32;
                    if ui
                        .add(
                            egui::Slider::new(&mut cap, 0.0..=480.0)
                                .text("Frame rate cap")
                                .step_by(10.0),
                        )
                        .changed()
                    {
                        settings.max_fps = cap as u32;
                        changed = true;
                    }
                    if settings.max_fps == 0 {
                        ui.label(
                            egui::RichText::new("uncapped")
                                .monospace()
                                .size(11.0)
                                .color(theme::DIM),
                        );
                    }
                }
                let mut distance = settings.render_distance as f32;
                if ui
                    .add(egui::Slider::new(&mut distance, 2.0..=32.0).text("Render distance").step_by(1.0))
                    .changed()
                {
                    settings.render_distance = distance as u32;
                    changed = true;
                }
                ui.label(
                    egui::RichText::new("the server decides how much of this it will honour")
                        .size(11.0)
                        .color(theme::DIM),
                );

                ui.add_space(12.0);
                section(ui, "LIGHTING");
                changed |= ui
                    .checkbox(&mut settings.fullbright, "Full brightness")
                    .on_hover_text("Ignores the world's light. Caves are as bright as noon.")
                    .changed();

                ui.add_space(12.0);
                section(ui, "INTERFACE");
                let mut scale = settings.gui_scale;
                if ui
                    .add(
                        egui::Slider::new(&mut scale, 0.0..=4.0)
                            .text("Interface scale")
                            .step_by(0.25),
                    )
                    .changed()
                {
                    settings.gui_scale = scale;
                    changed = true;
                }
                if settings.gui_scale <= 0.0 {
                    ui.label(
                        egui::RichText::new("follows the display")
                            .monospace()
                            .size(11.0)
                            .color(theme::DIM),
                    );
                }
                changed |= ui
                    .add(
                        egui::Slider::new(&mut settings.mouse_sensitivity, 0.02..=0.5)
                            .text("Mouse sensitivity"),
                    )
                    .changed();

                ui.add_space(12.0);
                section(ui, "CONTROLS");
                if rebinding.is_some() {
                    ui.label(
                        egui::RichText::new("press a key, or escape to cancel")
                            .size(12.0)
                            .color(theme::ACCENT),
                    );
                    ui.add_space(4.0);
                }
                egui::Grid::new("keybinds")
                    .num_columns(2)
                    .spacing([14.0, 5.0])
                    .min_col_width(150.0)
                    .show(ui, |ui| {
                        for bind in Action::ALL {
                            ui.label(
                                egui::RichText::new(bind.label()).size(13.0).color(theme::MID),
                            );
                            let waiting = rebinding == Some(bind);
                            let label = if waiting {
                                "...".to_string()
                            } else {
                                settings.keys.label(bind)
                            };
                            let button = egui::Button::new(
                                egui::RichText::new(label).monospace().size(12.5),
                            );
                            if ui
                                .add_sized(egui::vec2(130.0, 24.0), button)
                                .clicked()
                            {
                                action = PauseAction::Rebind(bind);
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                if ui.button("Reset controls to defaults").clicked() {
                    settings.keys.reset();
                    changed = true;
                }
            });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Done").clicked() {
                    action = PauseAction::CloseSettings;
                }
                ui.label(
                    egui::RichText::new("changes apply as you make them")
                        .size(11.0)
                        .color(theme::DIM),
                );
            });
        });

    // A change beats a navigation action: the edit still has to be saved.
    if changed {
        return PauseAction::Apply(Box::new(settings));
    }
    action
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).monospace().size(11.0).color(theme::DIM));
    ui.add_space(4.0);
}
