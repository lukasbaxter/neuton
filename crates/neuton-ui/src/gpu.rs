//! Window surface and GPU device.
//!
//! Owned directly rather than through a framework. The launcher and the game
//! share one window and one `wgpu::Device`: pressing play swaps what is drawn,
//! it does not tear down a window and build another. That is also why egui is
//! only a layer on top of a surface we control, not the thing that owns the
//! event loop.

use std::sync::Arc;
use winit::window::Window;

pub struct Gpu {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub window: Arc<Window>,
}

impl Gpu {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        pollster::block_on(Self::new_async(window))
    }

    async fn new_async(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        // wgpu 30 wants the descriptor by value and picks backends from the
        // environment, which lets WGPU_BACKEND override the choice for testing.
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());

        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("could not create a surface: {e}"))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("no usable GPU adapter: {e}"))?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("neuton"),
                memory_hints: wgpu::MemoryHints::Performance,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("could not open the GPU device: {e}"))?;

        // Start from the surface's own defaults so new fields in future wgpu
        // releases keep sensible values, then override only what matters here.
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| "this GPU cannot present to the window".to_string())?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer a non-sRGB-converting format so egui and the world renderer
        // agree on colour space without a correction pass between them.
        if let Some(f) = caps.formats.iter().copied().find(|f| !f.is_srgb()) {
            config.format = f;
        }
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);

        Ok(Self { surface, device, queue, config, window })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Switches presentation between waiting for the display and not.
    pub fn set_present_mode(&mut self, mode: wgpu::PresentMode) {
        if self.config.present_mode != mode {
            self.config.present_mode = mode;
            self.surface.configure(&self.device, &self.config);
        }
    }
}
