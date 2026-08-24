//! The launcher window.
//!
//! winit and wgpu are driven directly rather than through a framework, because
//! the same window and the same `wgpu::Device` will host the world renderer.
//! egui draws the launcher on top of a surface neuton owns.

pub mod app;
pub mod auth_task;
pub mod gpu;
pub mod theme;

use app::Launcher;
use gpu::Gpu;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    // Wait for input rather than spinning: an idle launcher should use no CPU.
    // The world renderer will switch this to Poll when it takes over.
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[derive(Default)]
struct App {
    state: Option<State>,
    launcher: Option<Launcher>,
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
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = &mut self.state else { return };
        let Some(launcher) = &mut self.launcher else { return };

        let response = state.egui_winit.on_window_event(&state.gpu.window, &event);
        if response.repaint {
            state.gpu.window.request_redraw();
        }
        if response.consumed {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.gpu.resize(size.width, size.height);
                state.gpu.window.request_redraw();
            }
            WindowEvent::RedrawRequested => draw(state, launcher),
            _ => {}
        }
    }
}

fn draw(state: &mut State, launcher: &mut Launcher) {
    use wgpu::CurrentSurfaceTexture as Cst;

    let raw_input = state.egui_winit.take_egui_input(&state.gpu.window);
    let mut output = state.egui_ctx.run_ui(raw_input, |ui| launcher.update(ui));
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

    let mut free_all = |renderer: &mut egui_wgpu::Renderer| {
        for id in &to_free {
            renderer.free_texture(id);
        }
    };

    let frame = match state.gpu.surface.get_current_texture() {
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
    };

    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = state
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("launcher") });

    let desc = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [state.gpu.config.width, state.gpu.config.height],
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
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.031,
                        g: 0.035,
                        b: 0.043,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            ..Default::default()
        });
        state.egui_renderer.render(&mut pass.forget_lifetime(), &tris, &desc);
    }

    free_all(&mut state.egui_renderer);

    state.gpu.queue.submit(Some(encoder.finish()));
    // Presentation moved onto the queue in wgpu 30.
    state.gpu.queue.present(frame);
}
