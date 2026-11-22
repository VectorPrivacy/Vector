//! Backdrop blur (`gpui::Backdrop`): the region under a shape is copied out of the frame,
//! shrunk to a quarter, blurred across then down, and drawn back under the shape. Built on
//! first use, so a session that never shows one compiles nothing.

use gpui::{Backdrop, Bounds, ScaledPixels};

const DOWN: f32 = 4.;
/// Uniform offsets must sit on this; 256 is the most any backend asks.
const SLOT: u64 = 256;

/// `BackdropPass` in shaders_backdrop.wgsl, field for field.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Pass {
    dst: [f32; 4],
    target_size: [f32; 2],
    texel: [f32; 2],
    uv: [f32; 4],
    uv_clamp: [f32; 4],
    dir: [f32; 2],
    pad: [f32; 2],
    weights0: [f32; 4],
    weights1: [f32; 4],
    offsets0: [f32; 4],
    offsets1: [f32; 4],
    shape: [f32; 4],
    radii: [f32; 4],
    clip: [f32; 4],
    clip_radii: [f32; 4],
}

struct Scratch {
    width: u32,
    height: u32,
    /// The frame's region, copied out at full size.
    copy: wgpu::Texture,
    copy_view: wgpu::TextureView,
    /// Quarter size: shrunk into `a`, blurred across into `b`, down back into `a`.
    a_view: wgpu::TextureView,
    b_view: wgpu::TextureView,
    quarter: (u32, u32),
}

pub(crate) struct Backdrops {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    down: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    slots: u64,
    scratch: Option<Scratch>,
    /// Held backdrops' blurs, kept while drawn frame after frame.
    held: Vec<Held>,
    /// The latest unheld blur's draw-back.
    last: Option<Composite>,
}

struct Held {
    key: [u32; 13],
    composite: Composite,
    drawn: bool,
}

/// What the main pass draws back once a backdrop's blur is ready.
pub(crate) struct Composite {
    bind_group: wgpu::BindGroup,
}

impl Backdrops {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("backdrop_shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders_backdrop.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Pass>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("backdrop_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |label: &str, fragment: &str, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_backdrop"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fragment),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("backdrop_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            format,
            down: pipeline("backdrop_down", "fs_backdrop_down", None),
            blur: pipeline("backdrop_blur", "fs_backdrop_blur", None),
            composite: pipeline(
                "backdrop_composite",
                "fs_backdrop_composite",
                Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            ),
            layout,
            sampler,
            uniforms: Self::uniform_buffer(device, 4),
            slots: 4,
            scratch: None,
            held: Vec::new(),
            last: None,
        }
    }

    fn uniform_buffer(device: &wgpu::Device, slots: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("backdrop_uniforms"),
            size: slots * SLOT,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub(crate) fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Room for `count` backdrops this frame, four passes each.
    pub(crate) fn reserve(&mut self, device: &wgpu::Device, count: usize) {
        let needed = count as u64 * 4;
        if needed > self.slots {
            self.slots = needed.next_power_of_two();
            self.uniforms = Self::uniform_buffer(device, self.slots);
        }
    }

    /// Starts a frame: a held blur not drawn last frame is dropped, so its next showing blurs
    /// afresh; the scratch textures go when nothing this frame may blur.
    pub(crate) fn begin_frame(&mut self, scene: &gpui::Scene) {
        self.held.retain_mut(|h| std::mem::replace(&mut h.drawn, false));
        if scene.backdrops.iter().all(|b| b.held) {
            self.scratch = None;
        }
    }

    /// Whether a blur is kept for `backdrop`, which then counts as drawn this frame.
    pub(crate) fn held(&mut self, backdrop: &Backdrop) -> bool {
        if !backdrop.held {
            return false;
        }
        let key = backdrop.key();
        self.held.iter_mut().find(|h| h.key == key).map(|h| h.drawn = true).is_some()
    }

    fn scratch(&mut self, device: &wgpu::Device, width: u32, height: u32, quarter: (u32, u32)) -> &Scratch {
        let fits = self
            .scratch
            .as_ref()
            .is_some_and(|s| s.width >= width && s.height >= height && s.quarter.0 >= quarter.0 && s.quarter.1 >= quarter.1);
        if !fits {
            let (width, height) = (width.next_multiple_of(64), height.next_multiple_of(64));
            let quarter = (quarter.0.next_multiple_of(16), quarter.1.next_multiple_of(16));
            let make = |label: &str, w: u32, h: u32, usage: wgpu::TextureUsages| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.format,
                    usage,
                    view_formats: &[],
                })
            };
            let copy = make("backdrop_copy", width, height, wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING);
            let target = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
            let a = make("backdrop_a", quarter.0, quarter.1, target);
            let b = make("backdrop_b", quarter.0, quarter.1, target);
            self.scratch = Some(Scratch {
                width,
                height,
                copy_view: copy.create_view(&Default::default()),
                copy,
                a_view: a.create_view(&Default::default()),
                b_view: b.create_view(&Default::default()),
                quarter,
            });
        }
        self.scratch.as_ref().expect("made above")
    }

    fn bind(&self, device: &wgpu::Device, uniforms: &wgpu::Buffer, slot: u64, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop_bind_group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: uniforms,
                        offset: slot * SLOT,
                        size: wgpu::BufferSize::new(std::mem::size_of::<Pass>() as u64),
                    }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    fn run(&self, encoder: &mut wgpu::CommandEncoder, pipeline: &wgpu::RenderPipeline, target: &wgpu::TextureView, bind_group: &wgpu::BindGroup) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("backdrop_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    /// Blurs `frame` under `backdrop` (the `index`th this frame), outside any pass, for
    /// [`Self::composite_latest`] to draw back; false when it covers nothing on screen.
    pub(crate) fn blur(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::Texture,
        backdrop: &Backdrop,
        index: usize,
    ) -> bool {
        let (vw, vh) = (frame.width() as f32, frame.height() as f32);
        let rect = |b: &Bounds<ScaledPixels>| [b.origin.x.0, b.origin.y.0, b.size.width.0, b.size.height.0];
        let shape = backdrop.bounds.intersect(&backdrop.content_mask.bounds);
        let (x0, y0) = (shape.origin.x.0.max(0.), shape.origin.y.0.max(0.));
        let (x1, y1) = ((shape.origin.x.0 + shape.size.width.0).min(vw), (shape.origin.y.0 + shape.size.height.0).min(vh));
        if x1 <= x0 || y1 <= y0 {
            return false;
        }
        // Read past the shape by three deviations, so its edges blur in what lies beside them.
        let reach = (backdrop.blur_radius.0 * 3.).ceil();
        let (rx0, ry0) = ((x0 - reach).max(0.).floor(), (y0 - reach).max(0.).floor());
        let (rx1, ry1) = ((x1 + reach).min(vw).ceil(), (y1 + reach).min(vh).ceil());
        let (rw, rh) = ((rx1 - rx0) as u32, (ry1 - ry0) as u32);
        let (qw, qh) = (((rx1 - rx0) / DOWN).ceil().max(1.), ((ry1 - ry0) / DOWN).ceil().max(1.));

        let held_texture = backdrop.held.then(|| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("backdrop_held"),
                size: wgpu::Extent3d { width: qw as u32, height: qh as u32, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        });
        let scratch = self.scratch(device, rw, rh, (qw as u32, qh as u32));
        let (sw, sh) = (scratch.width as f32, scratch.height as f32);
        let (aw, ah) = (scratch.quarter.0 as f32, scratch.quarter.1 as f32);
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: frame,
                mip_level: 0,
                origin: wgpu::Origin3d { x: rx0 as u32, y: ry0 as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &scratch.copy,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: rw, height: rh, depth_or_array_layers: 1 },
        );

        let sigma = (backdrop.blur_radius.0 / DOWN).max(0.5);
        let spacing = sigma * 3. / 6.;
        let mut weights = [0f32; 7];
        for (i, w) in weights.iter_mut().enumerate() {
            let x = i as f32 * spacing;
            *w = (-(x * x) / (2. * sigma * sigma)).exp();
        }
        let total = weights[0] + 2. * weights[1..].iter().sum::<f32>();
        weights.iter_mut().for_each(|w| *w /= total);
        let o = |i: usize| i as f32 * spacing;
        let radii = |c: &gpui::Corners<ScaledPixels>| [c.top_left.0, c.top_right.0, c.bottom_right.0, c.bottom_left.0];

        let quarter_clamp = [0.5 / aw, 0.5 / ah, (qw - 0.5) / aw, (qh - 0.5) / ah];
        let down = Pass {
            dst: [0., 0., qw, qh],
            target_size: [aw, ah],
            texel: [1. / sw, 1. / sh],
            uv: [0., 0., qw * DOWN / sw, qh * DOWN / sh],
            uv_clamp: [0.5 / sw, 0.5 / sh, (rw as f32 - 0.5) / sw, (rh as f32 - 0.5) / sh],
            weights0: [weights[0], weights[1], weights[2], weights[3]],
            weights1: [weights[4], weights[5], weights[6], 0.],
            offsets0: [o(1), o(2), o(3), o(4)],
            offsets1: [o(5), o(6), 0., 0.],
            shape: rect(&backdrop.bounds),
            radii: radii(&backdrop.corner_radii),
            clip: rect(&backdrop.content_mask.bounds),
            clip_radii: radii(&backdrop.content_mask.corner_radii),
            ..Default::default()
        };
        let across = Pass { texel: [1. / aw, 1. / ah], uv: [0., 0., qw / aw, qh / ah], uv_clamp: quarter_clamp, dir: [1., 0.], ..down };
        let (ow, oh) = held_texture.as_ref().map_or((aw, ah), |t| (t.width() as f32, t.height() as f32));
        let downward = Pass { dir: [0., 1.], target_size: [ow, oh], ..across };
        // Drawn back over the shape: each pixel reads the blur where it sits, a quarter as far.
        let back = Pass {
            dst: [x0, y0, x1 - x0, y1 - y0],
            target_size: [vw, vh],
            uv: [(x0 - rx0) / DOWN / ow, (y0 - ry0) / DOWN / oh, (x1 - x0) / DOWN / ow, (y1 - y0) / DOWN / oh],
            uv_clamp: [0.5 / ow, 0.5 / oh, (qw - 0.5) / ow, (qh - 0.5) / oh],
            ..across
        };
        let base = index as u64 * 4;
        for (i, pass) in [down, across, downward].iter().enumerate() {
            queue.write_buffer(&self.uniforms, (base + i as u64) * SLOT, bytemuck::bytes_of(pass));
        }

        let scratch = self.scratch.as_ref().expect("made above");
        let held_view = held_texture.as_ref().map(|t| t.create_view(&Default::default()));
        let out = held_view.as_ref().unwrap_or(&scratch.a_view);
        let groups = [
            self.bind(device, &self.uniforms, base, &scratch.copy_view),
            self.bind(device, &self.uniforms, base + 1, &scratch.a_view),
            self.bind(device, &self.uniforms, base + 2, &scratch.b_view),
        ];
        self.run(encoder, &self.down, &scratch.a_view, &groups[0]);
        self.run(encoder, &self.blur, &scratch.b_view, &groups[1]);
        self.run(encoder, &self.blur, out, &groups[2]);
        let composite = match &held_view {
            // Its own uniforms: the shared slots are rewritten every frame.
            Some(view) => {
                let uniforms = Self::uniform_buffer(device, 1);
                queue.write_buffer(&uniforms, 0, bytemuck::bytes_of(&back));
                Composite { bind_group: self.bind(device, &uniforms, 0, view) }
            }
            None => {
                queue.write_buffer(&self.uniforms, (base + 3) * SLOT, bytemuck::bytes_of(&back));
                Composite { bind_group: self.bind(device, &self.uniforms, base + 3, &scratch.a_view) }
            }
        };
        if backdrop.held {
            self.held.push(Held { key: backdrop.key(), composite, drawn: true });
        } else {
            self.last = Some(composite);
        }
        true
    }

    /// Whether a backdrop this frame needs what lies under it read: one not held, or held
    /// without a blur kept for it.
    pub(crate) fn must_read(&self, scene: &gpui::Scene) -> bool {
        scene.backdrops.iter().any(|b| !b.held || !self.held.iter().any(|h| h.key == b.key()))
    }

    /// Copies `from`, a whole frame drawn offscreen, over `to`, the surface's, through the
    /// draw-back pipeline at full cover: the uniform slot after `count` backdrops' four each.
    pub(crate) fn copy_over(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        from: &wgpu::TextureView,
        to: &wgpu::TextureView,
        (width, height): (u32, u32),
        count: usize,
    ) {
        let (w, h) = (width as f32, height as f32);
        let pass = Pass {
            dst: [0., 0., w, h],
            target_size: [w, h],
            texel: [1. / w, 1. / h],
            uv: [0., 0., 1., 1.],
            uv_clamp: [0.5 / w, 0.5 / h, 1. - 0.5 / w, 1. - 0.5 / h],
            shape: [0., 0., w, h],
            clip: [0., 0., w, h],
            ..Default::default()
        };
        let slot = count as u64 * 4;
        queue.write_buffer(&self.uniforms, slot * SLOT, bytemuck::bytes_of(&pass));
        let group = self.bind(device, &self.uniforms, slot, from);
        self.run(encoder, &self.composite, to, &group);
    }

    /// Draws back the blur just made for `backdrop`.
    pub(crate) fn composite_latest(&self, backdrop: &Backdrop, pass: &mut wgpu::RenderPass<'_>) {
        let made = if backdrop.held {
            let key = backdrop.key();
            self.held.iter().rev().find(|h| h.key == key).map(|h| &h.composite)
        } else {
            self.last.as_ref()
        };
        if let Some(composite) = made {
            self.composite(composite, pass);
        }
    }

    pub(crate) fn composite(&self, composite: &Composite, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.composite);
        pass.set_bind_group(0, &composite.bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
}
