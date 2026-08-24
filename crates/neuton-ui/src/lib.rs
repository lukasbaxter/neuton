//! The launcher window.
//!
//! winit and wgpu are driven directly rather than through a framework, because
//! the same window and the same `wgpu::Device` will host the world renderer.
//! egui draws the launcher on top of a surface neuton owns.

pub mod app;
pub mod auth_task;
pub mod fonts;
pub mod gpu;
pub mod icons;
pub mod ping_task;
pub mod servers;
pub mod session;
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
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App {
        direct: Some(app::PendingJoin { host, port, session }),
        shot: Some((path, after)),
        view,
        bench: bench_frames,
        ..Default::default()
    };
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn run_with(
    direct: Option<app::PendingJoin>,
    shot: Option<(std::path::PathBuf, std::time::Duration)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    // Wait for input rather than spinning: an idle launcher should use no CPU.
    // A world requests its own redraws continuously.
    event_loop.set_control_flow(ControlFlow::Wait);
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

        if let Some(pending) = self.direct.take() {
            let Self { state, world, renderer, textures, .. } = self;
            if let Some(state) = state {
                start_world(pending, state, world, renderer, textures);
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
            state, launcher, world, renderer, textures, last_frame, started, ..
        } = self;
        let Some(state) = state else { return };
        let Some(launcher) = launcher else { return };

        // While flying, input belongs to the world rather than to egui.
        let flying = world.as_ref().is_some_and(|w| w.captured);
        if !flying {
            let response = state.egui_winit.on_window_event(&state.gpu.window, &event);
            if response.repaint {
                state.gpu.window.request_redraw();
            }
            if response.consumed {
                return;
            }
        }

        // A join can only be started here, where the GPU lives.
        if let Some(pending) = launcher.pending_join.take() {
            start_world(pending, state, world, renderer, textures);
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.gpu.resize(size.width, size.height);
                state.gpu.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let dt = last_frame
                    .replace(Instant::now())
                    .map(|t| t.elapsed().as_secs_f32())
                    .unwrap_or(1.0 / 60.0)
                    .min(0.25);

                if let (Some(w), Some(r)) = (world.as_mut(), renderer.as_mut()) {
                    w.update(dt, r, &state.gpu.device);
                    w.record_frame(dt);
                }
                let animating = world.is_some();

                // Screenshot mode: give the world time to stream in, take one
                // frame, then leave.
                if let Some((path, after)) = &self.shot {
                    let elapsed = started.get_or_insert_with(Instant::now).elapsed();
                    if elapsed >= *after {
                        if let (Some(w), Some((pos, yaw, pitch))) = (world.as_mut(), self.view) {
                            w.camera.position = pos;
                            w.camera.yaw = yaw;
                            w.camera.pitch = pitch;
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
                    state.gpu.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let pressed = event.state == ElementState::Pressed;
                if let Some(w) = world.as_mut() {
                    if pressed && code == KeyCode::Escape {
                        // First Escape releases the mouse, a second leaves the
                        // world. Anything else strands the pointer.
                        if w.captured {
                            set_capture(&state.gpu.window, w, false);
                        } else {
                            *world = None;
                            if let Some(r) = renderer.as_mut() {
                                r.clear();
                            }
                            state.gpu.window.set_title("neuton");
                        }
                        return;
                    }
                    w.key(code, pressed);
                }
            }
            WindowEvent::MouseInput { state: button_state, .. } => {
                if let Some(w) = world.as_mut()
                    && button_state == ElementState::Pressed
                    && !w.captured
                {
                    set_capture(&state.gpu.window, w, true);
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
            *textures = Some(Arc::new(BlockTextures::build(&mut packs)));
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
        let session = WorldSession::connect(pending.host, pending.port, pending.session, atlas);
        let mut view = WorldView::new(session);
        set_capture(&state.gpu.window, &mut view, true);
        *world = Some(view);
        state.gpu.window.request_redraw();
    }
}

/// Locks or releases the pointer for mouse look.
fn set_capture(window: &Window, world: &mut WorldView, capture: bool) {
    if capture {
        // Locked is what a game wants; some platforms only offer Confined.
        let locked = window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
            .is_ok();
        window.set_cursor_visible(!locked);
        world.captured = locked;
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
        world.captured = false;
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
    world: Option<&mut WorldView>,
    renderer: Option<&mut WorldRenderer>,
    capture: Option<CaptureTarget<'_>>,
) {
    use wgpu::CurrentSurfaceTexture as Cst;

    let raw_input = state.egui_winit.take_egui_input(&state.gpu.window);
    let hud = world.as_ref().zip(renderer.as_ref()).map(|(w, r)| Hud {
        debug: w.show_debug.then(|| w.debug_lines(r)),
    });
    let mut output = state.egui_ctx.run_ui(raw_input, |ui| match &hud {
        // In a world, egui draws only the overlay.
        Some(hud) => overlay(ui, hud),
        None => launcher.update(ui),
    });
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
}

/// The in-world overlay: a crosshair, and the debug panel when it is up.
fn overlay(ui: &mut egui::Ui, hud: &Hud) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            crosshair(ui);

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
                ui.label(
                    egui::RichText::new(
                        "WASD move  ·  space/ctrl height  ·  shift sprint  ·  F3 debug  ·  esc release",
                    )
                    .monospace()
                    .size(11.0)
                    .color(egui::Color32::from_white_alpha(120)),
                );
            });
        });
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
