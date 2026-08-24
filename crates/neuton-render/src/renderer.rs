//! The world pipeline: one shader, one atlas, one draw call per chunk.

use crate::camera::{Camera, Frustum, Mat4};
use crate::mesh::{Mesh, Vertex};
use crate::textures::BlockTextures;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

/// Uniforms shared by every draw.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_projection: Mat4,
    fog_color: [f32; 4],
    fog: [f32; 4],
}

/// One chunk's geometry, uploaded.
struct ChunkBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    /// Water and glass, drawn in a second pass.
    translucent: Option<wgpu::Buffer>,
    translucent_count: u32,
    /// World-space bounds, for frustum culling. Taken from the geometry rather
    /// than assumed, so a column holding one layer of bedrock does not claim to
    /// be 384 blocks tall.
    min: [f32; 3],
    max: [f32; 3],
}

/// Draws the world.
pub struct WorldRenderer {
    pipeline: wgpu::RenderPipeline,
    translucent_pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    atlas_bind_group: wgpu::BindGroup,
    depth: wgpu::TextureView,
    depth_size: (u32, u32),
    /// The surface format the pipeline was built for. A capture has to match it
    /// or the render pass is rejected.
    format: wgpu::TextureFormat,
    chunks: HashMap<(i32, i32), ChunkBuffers>,
    /// Chunks drawn on the last frame, against chunks held.
    pub drawn: std::cell::Cell<usize>,
    pub sky_color: [f32; 4],
    /// Fog is derived from how much world is actually loaded rather than fixed.
    /// Too near and the world is permanently hazy; too far and it ends at a
    /// visible wall where the chunks run out.
    pub fog_scale: f32,
    /// The lowest light any surface is drawn at. One is fullbright.
    pub min_light: f32,
    outline_pipeline: wgpu::RenderPipeline,
    outline_buffer: wgpu::Buffer,
    /// Line ends in the outline buffer this frame.
    outline_vertices: u32,
    /// Geometry for the cracks over a block being broken.
    breaking_vertices: wgpu::Buffer,
    breaking_indices: wgpu::Buffer,
    breaking_count: u32,
}

impl WorldRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        textures: &BlockTextures,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world"),
            source: wgpu::ShaderSource::Wgsl(include_str!("world.wgsl").into()),
        });

        let globals_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("globals"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let atlas_view = upload_atlas(device, queue, textures);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            // Nearest magnification keeps pixel art sharp up close; linear
            // minification stops distant blocks shimmering as the camera moves.
            // Nearest magnification keeps pixel art sharp up close. Linear
            // minification and trilinear mip blending stop distant blocks
            // shimmering, which is the most obvious artefact once a world is
            // more than a few chunks deep.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world"),
            // wgpu 30 allows gaps in the layout list, hence the Options.
            bind_group_layouts: &[Some(&globals_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3, // position
                        1 => Float32x2, // uv
                        2 => Float32x4, // biome tint, with opacity in alpha
                        3 => Float32,   // directional shade
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Faces are wound counter-clockwise seen from outside, so the
                // inside of every block is discarded before it is shaded.
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Opaque: the fragment shader discards rather than blends,
                    // so every surviving fragment fully replaces what is under
                    // it and depth ordering stops mattering.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // The selection box: lines, blended, and not written to depth, so it
        // never hides anything behind it.
        let outline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("outline"),
            source: wgpu::ShaderSource::Wgsl(include_str!("outline.wgsl").into()),
        });
        let outline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("outline"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });
        let outline_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("outline"),
            layout: Some(&outline_layout),
            vertex: wgpu::VertexState {
                module: &outline_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                // No depth bias: it is only defined for triangles. An edge
                // flush with a block face is kept out of the depth fight by
                // nudging the box outwards instead, in `set_outline`.
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &outline_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::COLOR,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        // Room for a handful of boxes: a block's outline is rarely more than
        // two or three, and a fence is the worst case.
        const OUTLINE_CAPACITY: u64 = 24 * 12 * 16;
        let outline_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("outline"),
            size: OUTLINE_CAPACITY,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Same geometry and same shader, but blended and without depth writes,
        // so two translucent surfaces do not hide each other depending on which
        // chunk happened to be drawn first.
        let translucent_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("world translucent"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x3,
                            1 => Float32x2,
                            2 => Float32x4,
                            3 => Float32,
                        ],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    // Both sides: from under the surface of an ocean you are
                    // looking at the back of every water face.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_translucent"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });

        Self {
            pipeline,
            translucent_pipeline,
            globals_buffer,
            globals_bind_group,
            outline_pipeline,
            outline_buffer,
            outline_vertices: 0,
            breaking_vertices: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("breaking vertices"),
                size: (std::mem::size_of::<Vertex>() * 6 * 4 * 16) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            breaking_indices: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("breaking indices"),
                size: (std::mem::size_of::<u32>() * 6 * 6 * 16) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            breaking_count: 0,
            atlas_bind_group,
            depth: create_depth(device, width, height),
            depth_size: (width, height),
            format,
            chunks: HashMap::new(),
            drawn: std::cell::Cell::new(0),
            sky_color: [0.62, 0.74, 0.94, 1.0],
            fog_scale: 1.0,
            min_light: 0.0,
        }
    }

    /// Uploads or replaces one chunk's geometry.
    ///
    /// An empty mesh removes the chunk rather than uploading a zero-length
    /// buffer, which some backends reject.
    pub fn upload(&mut self, device: &wgpu::Device, x: i32, z: i32, mesh: &Mesh) {
        if mesh.is_empty() {
            self.chunks.remove(&(x, z));
            return;
        }
        // An index buffer must not be empty, so a chunk that is entirely water
        // still needs a valid opaque buffer even though it draws nothing.
        // Positions are chunk-relative from the mesher, so shift them into the
        // world here rather than paying for a per-chunk uniform and a bind
        // group change on every draw.
        let mut vertices = mesh.vertices.clone();
        let (ox, oz) = ((x * 16) as f32, (z * 16) as f32);
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for v in &mut vertices {
            v.position[0] += ox;
            v.position[2] += oz;
            for axis in 0..3 {
                min[axis] = min[axis].min(v.position[axis]);
                max[axis] = max[axis].max(v.position[axis]);
            }
        }

        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("chunk vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("chunk indices"),
            contents: bytemuck::cast_slice(if mesh.indices.is_empty() {
                &[0u32, 0, 0][..]
            } else {
                &mesh.indices
            }),
            usage: wgpu::BufferUsages::INDEX,
        });
        let translucent = (!mesh.translucent.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("chunk translucent indices"),
                contents: bytemuck::cast_slice(&mesh.translucent),
                usage: wgpu::BufferUsages::INDEX,
            })
        });
        self.chunks.insert(
            (x, z),
            ChunkBuffers {
                vertices,
                indices,
                index_count: mesh.indices.len() as u32,
                translucent,
                translucent_count: mesh.translucent.len() as u32,
                min,
                max,
            },
        );
    }

    /// Sets the box or boxes drawn around the block being pointed at.
    ///
    /// Each is a world-space minimum and maximum. An empty list draws nothing,
    /// which is what pointing at the sky means.
    pub fn set_outline(&mut self, queue: &wgpu::Queue, boxes: &[([f32; 3], [f32; 3])]) {
        let mut lines: Vec<f32> = Vec::with_capacity(boxes.len() * 24 * 3);
        for (min, max) in boxes {
            // Nudged outwards so the wireframe sits just off the surface rather
            // than inside it.
            const OUT: f32 = 0.0035;
            let a = [min[0] - OUT, min[1] - OUT, min[2] - OUT];
            let b = [max[0] + OUT, max[1] + OUT, max[2] + OUT];
            let corner = |i: usize| {
                [
                    if i & 1 == 0 { a[0] } else { b[0] },
                    if i & 2 == 0 { a[1] } else { b[1] },
                    if i & 4 == 0 { a[2] } else { b[2] },
                ]
            };
            // The twelve edges of a box, as pairs of corner indices.
            const EDGES: [(usize, usize); 12] = [
                (0, 1), (2, 3), (4, 5), (6, 7),
                (0, 2), (1, 3), (4, 6), (5, 7),
                (0, 4), (1, 5), (2, 6), (3, 7),
            ];
            for (from, to) in EDGES {
                lines.extend_from_slice(&corner(from));
                lines.extend_from_slice(&corner(to));
            }
        }
        let bytes = bytemuck::cast_slice(&lines);
        let capacity = self.outline_buffer.size() as usize;
        let bytes = &bytes[..bytes.len().min(capacity)];
        queue.write_buffer(&self.outline_buffer, 0, bytes);
        self.outline_vertices = (bytes.len() / 12) as u32;
    }

    /// Sets the cracks drawn over the block being broken.
    ///
    /// `stage` runs from zero to nine as the swing progresses; `None` clears
    /// them. The boxes are the block's own shapes, so cracks appear over a slab
    /// where the slab is rather than over the whole cube it sits in.
    pub fn set_breaking(
        &mut self,
        queue: &wgpu::Queue,
        textures: &BlockTextures,
        boxes: &[([f32; 3], [f32; 3])],
        stage: Option<u32>,
    ) {
        let Some(stage) = stage else {
            self.breaking_count = 0;
            return;
        };
        let uv = textures.atlas.uv(&crate::textures::destroy_stage_texture(stage));
        let corners = uv.corners();

        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for (min, max) in boxes {
            // A hair outside the block, so the cracks sit on the surface rather
            // than fighting it for the same depth.
            const OUT: f32 = 0.002;
            let a = [min[0] - OUT, min[1] - OUT, min[2] - OUT];
            let b = [max[0] + OUT, max[1] + OUT, max[2] + OUT];
            // down, up, north, south, west, east, wound so each is seen from
            // outside the box.
            let faces: [[[f32; 3]; 4]; 6] = [
                [[a[0], a[1], b[2]], [b[0], a[1], b[2]], [b[0], a[1], a[2]], [a[0], a[1], a[2]]],
                [[a[0], b[1], a[2]], [b[0], b[1], a[2]], [b[0], b[1], b[2]], [a[0], b[1], b[2]]],
                [[b[0], b[1], a[2]], [a[0], b[1], a[2]], [a[0], a[1], a[2]], [b[0], a[1], a[2]]],
                [[a[0], b[1], b[2]], [b[0], b[1], b[2]], [b[0], a[1], b[2]], [a[0], a[1], b[2]]],
                [[a[0], b[1], a[2]], [a[0], b[1], b[2]], [a[0], a[1], b[2]], [a[0], a[1], a[2]]],
                [[b[0], b[1], b[2]], [b[0], b[1], a[2]], [b[0], a[1], a[2]], [b[0], a[1], b[2]]],
            ];
            for face in faces {
                let base = vertices.len() as u32;
                for (corner, position) in face.iter().enumerate() {
                    vertices.push(Vertex {
                        position: *position,
                        uv: corners[corner],
                        tint: [1.0, 1.0, 1.0, 1.0],
                        light: 1.0,
                    });
                }
                indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }

        let vertex_bytes: &[u8] = bytemuck::cast_slice(&vertices);
        let index_bytes: &[u8] = bytemuck::cast_slice(&indices);
        if vertex_bytes.len() as u64 > self.breaking_vertices.size()
            || index_bytes.len() as u64 > self.breaking_indices.size()
        {
            // More shapes than the buffer holds: a block with sixteen boxes is
            // not worth growing a buffer for mid-frame.
            self.breaking_count = 0;
            return;
        }
        queue.write_buffer(&self.breaking_vertices, 0, vertex_bytes);
        queue.write_buffer(&self.breaking_indices, 0, index_bytes);
        self.breaking_count = indices.len() as u32;
    }

    pub fn forget(&mut self, x: i32, z: i32) {
        self.chunks.remove(&(x, z));
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// How far the loaded world reaches, in blocks.
    ///
    /// Taken from the number of columns held rather than from the server's view
    /// distance, because what matters is where the geometry actually stops.
    pub fn view_distance(&self) -> f32 {
        if self.chunks.is_empty() {
            return 256.0;
        }
        // The loaded region is roughly a disc, so its radius follows from its
        // area.
        let radius = (self.chunks.len() as f32 / std::f32::consts::PI).sqrt();
        radius * 16.0
    }

    /// Triangles currently held, across every loaded chunk.
    ///
    /// Counted from the buffers rather than accumulated as chunks arrive: a
    /// chunk is re-meshed whenever a neighbour loads, and a running total would
    /// count it again every time.
    pub fn triangles(&self) -> usize {
        self.chunks
            .values()
            .map(|c| (c.index_count + c.translucent_count) as usize / 3)
            .sum()
    }

    /// Changes whether presentation waits for the display.
    ///
    /// Returns whether the surface needs reconfiguring, which only the owner of
    /// the surface can do.
    pub fn wants_present_mode(&self, vsync: bool) -> wgpu::PresentMode {
        if vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if (width, height) != self.depth_size && width > 0 && height > 0 {
            self.depth = create_depth(device, width, height);
            self.depth_size = (width, height);
        }
    }

    /// Draws every loaded chunk.
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        camera: &Camera,
    ) {
        let distance = self.view_distance();
        queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::bytes_of(&Globals {
                view_projection: camera.view_projection(),
                fog_color: self.sky_color,
                // Haze begins around half way out and is complete just short of
                // the edge, so chunks fade rather than appear.
                fog: [
                    distance * 0.55 * self.fog_scale,
                    distance * 0.95 * self.fog_scale,
                    self.min_light,
                    0.0,
                ],
            }),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("world"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: self.sky_color[0] as f64,
                        g: self.sky_color[1] as f64,
                        b: self.sky_color[2] as f64,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        pass.set_bind_group(1, &self.atlas_bind_group, &[]);

        // Most of the world is behind the camera at any moment, and a chunk
        // that fails this test costs a plane comparison instead of a draw call
        // and a few thousand triangles.
        let frustum = Frustum::from_matrix(camera.view_projection());
        let visible: Vec<&ChunkBuffers> = self
            .chunks
            .values()
            .filter(|c| frustum.intersects(c.min, c.max))
            .collect();
        self.drawn.set(visible.len());

        for chunk in &visible {
            if chunk.index_count == 0 {
                continue;
            }
            pass.set_vertex_buffer(0, chunk.vertices.slice(..));
            pass.set_index_buffer(chunk.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..chunk.index_count, 0, 0..1);
        }

        // The selection box, before the translucent pass so glass and water
        // still read as being in front of it.
        if self.outline_vertices > 0 {
            pass.set_pipeline(&self.outline_pipeline);
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            pass.set_vertex_buffer(0, self.outline_buffer.slice(..));
            pass.draw(0..self.outline_vertices, 0..1);
        }

        // Second: water and glass, over everything solid, so what is behind
        // them has already been drawn and can show through.
        pass.set_pipeline(&self.translucent_pipeline);
        // The cracks go in this pass: they are a texture with holes in it laid
        // over a block that has already been drawn.
        if self.breaking_count > 0 {
            pass.set_vertex_buffer(0, self.breaking_vertices.slice(..));
            pass.set_index_buffer(self.breaking_indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.breaking_count, 0, 0..1);
        }
        for chunk in &visible {
            let Some(buffer) = &chunk.translucent else { continue };
            pass.set_vertex_buffer(0, chunk.vertices.slice(..));
            pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..chunk.translucent_count, 0, 0..1);
        }
    }
}

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

fn create_depth(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn upload_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    textures: &BlockTextures,
) -> wgpu::TextureView {
    let atlas = &textures.atlas;
    let mips = atlas.mips();
    let size = wgpu::Extent3d {
        width: atlas.size,
        height: atlas.size,
        depth_or_array_layers: 1,
    };
    // Deliberately not the sRGB format. The surface is picked as non-sRGB, so
    // an sRGB texture would be decoded to linear on sampling and then written
    // to a surface that treats it as sRGB again, which darkens the whole world.
    // Sampling and presenting in the same space keeps the atlas looking like
    // the PNGs it came from.
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("block atlas"),
        size,
        mip_level_count: mips.len() as u32 + 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // Level 0, then the chain, each half the size of the one before it.
    let mut width = atlas.size;
    for (level, pixels) in std::iter::once(&atlas.pixels).chain(mips.iter()).enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(width),
            },
            wgpu::Extent3d { width, height: width, depth_or_array_layers: 1 },
        );
        width /= 2;
    }
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

impl WorldRenderer {
    /// Renders one frame into an off-screen image and returns it as RGBA8.
    ///
    /// Exists so the renderer can be checked by looking at it, from a script or
    /// a test, rather than only by whether it crashed.
    pub fn capture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        width: u32,
        height: u32,
    ) -> Option<Vec<u8>> {
        let (width, height) = (width.max(1), height.max(1));
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // Copies out of a texture need rows aligned to 256 bytes, so the buffer
        // is usually wider than the image and is trimmed on the way out.
        let unpadded = width * 4;
        let padded = unpadded.div_ceil(256) * 256;

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        self.resize(device, width, height);
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("capture") });
        self.render(&mut encoder, queue, &view, camera);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
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
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
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
            self.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        ) {
            for px in out.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        Some(out)
    }
}
