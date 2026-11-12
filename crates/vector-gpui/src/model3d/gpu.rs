//! The offscreen renderer: one device for every view, a scene's meshes and textures uploaded
//! once, each frame drawn multisampled and read back as straight-alpha BGRA for GPUI.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use wgpu::util::DeviceExt;

use super::scene::{Alpha, Scene, TexRef, Wrap};

const SAMPLES: u32 = 4;
const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const OBJ_STRIDE: u64 = 256;
pub const FOV: f32 = 35.;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    nrm: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
    joint: [u16; 4],
    weight: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameU {
    view_proj: [[f32; 4]; 4],
    cam: [f32; 4],
    env: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MatU {
    base: [f32; 4],
    emissive: [f32; 4],
    pbr: [f32; 4],
    misc: [f32; 4],
    uv0: [f32; 4],
    uv1: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ObjU {
    model: [[f32; 4]; 4],
    normal: [[f32; 4]; 4],
    skin: [u32; 4],
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    mat_layout: wgpu::BindGroupLayout,
    obj_layout: wgpu::BindGroupLayout,
    pipelines: HashMap<(bool, bool), wgpu::RenderPipeline>,
    frame_buf: wgpu::Buffer,
    frame_bind: wgpu::BindGroup,
    env_lod: f32,
    white: wgpu::TextureView,
    obj_buf: wgpu::Buffer,
    joint_buf: wgpu::Buffer,
    obj_bind: wgpu::BindGroup,
}

struct Part {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    count: u32,
    material: usize,
    skinned: bool,
    morph: Option<Morph>,
}

/// A morphed part keeps its rest vertices, rewritten into `vbuf` with the weights of each frame.
struct Morph {
    rest: Vec<Vertex>,
    targets: Vec<super::scene::Target>,
}

impl Morph {
    fn apply(&self, weights: &[f32]) -> Vec<Vertex> {
        let mut out = self.rest.clone();
        for (t, &w) in self.targets.iter().zip(weights) {
            if w == 0. {
                continue;
            }
            for (v, d) in out.iter_mut().zip(&t.pos) {
                for k in 0..3 {
                    v.pos[k] += d[k] * w;
                }
            }
            for (v, d) in out.iter_mut().zip(&t.nrm) {
                for k in 0..3 {
                    v.nrm[k] += d[k] * w;
                }
            }
        }
        if self.targets.iter().any(|t| !t.nrm.is_empty()) {
            for v in &mut out {
                v.nrm = Vec3::from(v.nrm).normalize_or(Vec3::Y).to_array();
            }
        }
        out
    }
}

struct GpuMat {
    bind: wgpu::BindGroup,
    blend: bool,
    double: bool,
}

/// A scene on the GPU, with what posing it needs.
pub struct Model {
    pub scene: Scene,
    meshes: Vec<Vec<Part>>,
    materials: Vec<GpuMat>,
    pub center: Vec3,
    pub radius: f32,
}

pub struct Target {
    w: u32,
    h: u32,
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
    resolve: wgpu::Texture,
    readback: wgpu::Buffer,
    stride: u32,
}

impl Target {
    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
}

pub struct Shot {
    pub pivot: Quat,
    pub distance: f32,
    pub pose: Option<Vec<super::scene::Trs>>,
    /// Animated morph weights per node; a node the clip leaves alone keeps its own.
    pub morph: Option<Vec<Option<Vec<f32>>>>,
}

fn mips(w: u32, h: u32, rgba: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    let mut out = vec![(w, h, rgba.to_vec())];
    while let Some(&(w, h, ref px)) = out.last() {
        if w == 1 && h == 1 {
            break;
        }
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let s = |dx: u32, dy: u32| px[((((y * 2 + dy).min(h - 1)) * w + (x * 2 + dx).min(w - 1)) * 4 + c) as usize] as u32;
                    next[((y * nw + x) * 4 + c) as usize] = ((s(0, 0) + s(1, 0) + s(0, 1) + s(1, 1) + 2) / 4) as u8;
                }
            }
        }
        out.push((nw, nh, next));
    }
    out
}

impl Gpu {
    pub fn new() -> Result<Self, String> {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                flags: wgpu::InstanceFlags::default(),
                backend_options: wgpu::BackendOptions::default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                display: None,
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                    apply_limit_buckets: false,
                })
                .await
                .map_err(|e| e.to_string())?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("model3d"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()).using_alignment(adapter.limits()),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    default_queue: wgpu::QueueDescriptor::default(),
                })
                .await
                .map_err(|e| e.to_string())?;
            Ok(Self::with(device, queue))
        })
    }

    fn with(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let tex_entry = |b: u32, vis: wgpu::ShaderStages| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: vis,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let uniform = |b: u32, vis: wgpu::ShaderStages, dynamic: bool| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: vis,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: dynamic, min_binding_size: None },
            count: None,
        };
        let sampler = |b: u32| wgpu::BindGroupLayoutEntry { binding: b, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None };
        let vf = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model3d frame"),
            entries: &[uniform(0, vf, false), tex_entry(1, wgpu::ShaderStages::FRAGMENT), sampler(2)],
        });
        let mat_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model3d material"),
            entries: &[
                uniform(0, vf, false),
                tex_entry(1, wgpu::ShaderStages::FRAGMENT),
                tex_entry(2, wgpu::ShaderStages::FRAGMENT),
                tex_entry(3, wgpu::ShaderStages::FRAGMENT),
                tex_entry(4, wgpu::ShaderStages::FRAGMENT),
                tex_entry(5, wgpu::ShaderStages::FRAGMENT),
                sampler(6),
            ],
        });
        let obj_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model3d object"),
            entries: &[
                uniform(0, wgpu::ShaderStages::VERTEX, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("model3d"), source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()) });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("model3d"), bind_group_layouts: &[Some(&frame_layout), Some(&mat_layout), Some(&obj_layout)], immediate_size: 0 });
        let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4, 4 => Uint16x4, 5 => Float32x4];
        let mut pipelines = HashMap::new();
        for blend in [false, true] {
            for double in [false, true] {
                let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("model3d"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs"),
                        buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("fs"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: COLOR,
                            blend: blend.then_some(wgpu::BlendState {
                                color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::SrcAlpha, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
                                alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
                            }),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: (!double).then_some(wgpu::Face::Back),
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: DEPTH,
                        depth_write_enabled: Some(true),
                        depth_compare: Some(wgpu::CompareFunction::LessEqual),
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                    multisample: wgpu::MultisampleState { count: SAMPLES, mask: !0, alpha_to_coverage_enabled: false },
                    multiview_mask: None,
                    cache: None,
                });
                pipelines.insert((blend, double), p);
            }
        }

        let levels = super::env::build();
        let (w0, h0) = (levels[0].0 as u32, levels[0].1 as u32);
        let env_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model3d env"),
            size: wgpu::Extent3d { width: w0, height: h0, depth_or_array_layers: 1 },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgb9e5Ufloat,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (k, (w, h, px)) in levels.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &env_tex, mip_level: k as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                bytemuck::cast_slice(px),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(*w as u32 * 4), rows_per_image: Some(*h as u32) },
                wgpu::Extent3d { width: *w as u32, height: *h as u32, depth_or_array_layers: 1 },
            );
        }
        let env_view = env_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let env_smp = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let frame_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("model3d frame"), size: std::mem::size_of::<FrameU>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let frame_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model3d frame"),
            layout: &frame_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: frame_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&env_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&env_smp) },
            ],
        });
        let white = Self::texture(&device, &queue, 1, 1, &[255; 4], true).create_view(&wgpu::TextureViewDescriptor::default());
        let (obj_buf, joint_buf, obj_bind) = Self::object_buffers(&device, &obj_layout, 64, 256);
        Self { device, queue, mat_layout, obj_layout, pipelines, frame_buf, frame_bind, env_lod: (levels.len() - 1) as f32, white, obj_buf, joint_buf, obj_bind }
    }

    fn object_buffers(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, objects: u64, joints: u64) -> (wgpu::Buffer, wgpu::Buffer, wgpu::BindGroup) {
        let obj = device.create_buffer(&wgpu::BufferDescriptor { label: Some("model3d objects"), size: objects * OBJ_STRIDE, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let joint = device.create_buffer(&wgpu::BufferDescriptor { label: Some("model3d joints"), size: joints * 64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model3d objects"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &obj, offset: 0, size: wgpu::BufferSize::new(std::mem::size_of::<ObjU>() as u64) }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: joint.as_entire_binding() },
            ],
        });
        (obj, joint, bind)
    }

    fn texture(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, rgba: &[u8], srgb: bool) -> wgpu::Texture {
        let max = device.limits().max_texture_dimension_2d;
        let shrunk;
        let (w, h, rgba) = if w > max || h > max {
            let k = max as f32 / w.max(h) as f32;
            let (nw, nh) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
            shrunk = image::RgbaImage::from_raw(w, h, rgba.to_vec())
                .map(|i| image::imageops::resize(&i, nw, nh, image::imageops::FilterType::Triangle).into_raw())
                .unwrap_or_else(|| vec![255; (nw * nh * 4) as usize]);
            (nw, nh, shrunk.as_slice())
        } else {
            (w, h, rgba)
        };
        let chain = mips(w, h, rgba);
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model3d texture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: chain.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm },
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (k, (mw, mh, px)) in chain.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: k as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                px,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(mw * 4), rows_per_image: Some(*mh) },
                wgpu::Extent3d { width: *mw, height: *mh, depth_or_array_layers: 1 },
            );
        }
        tex
    }

    /// Uploads a scene, centred and measured on its rest pose.
    pub fn upload(&self, scene: Scene) -> Model {
        let mut views: HashMap<(usize, bool), wgpu::TextureView> = HashMap::new();
        let mut view = |t: &Option<TexRef>, srgb: bool| -> Option<wgpu::TextureView> {
            let t = (*t)?;
            let img = scene.images.get(t.image)?;
            Some(
                views
                    .entry((t.image, srgb))
                    .or_insert_with(|| Self::texture(&self.device, &self.queue, img.w, img.h, &img.rgba, srgb).create_view(&wgpu::TextureViewDescriptor::default()))
                    .clone(),
            )
        };
        let address = |w: Wrap| match w {
            Wrap::Repeat => wgpu::AddressMode::Repeat,
            Wrap::Mirror => wgpu::AddressMode::MirrorRepeat,
            Wrap::Clamp => wgpu::AddressMode::ClampToEdge,
        };
        let mut materials = Vec::new();
        let fallback = super::scene::Material::default();
        for m in scene.materials.iter().chain(std::iter::once(&fallback)) {
            let first = m.base_tex.or(m.mr_tex).or(m.normal_tex).or(m.emissive_tex);
            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: first.map_or(wgpu::AddressMode::Repeat, |t| address(t.wrap[0])),
                address_mode_v: first.map_or(wgpu::AddressMode::Repeat, |t| address(t.wrap[1])),
                mag_filter: if first.is_some_and(|t| t.nearest) { wgpu::FilterMode::Nearest } else { wgpu::FilterMode::Linear },
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            });
            let (normal, bump) = (m.normal_tex.is_some() && m.bump.is_none(), m.normal_tex.is_some() && m.bump.is_some());
            let flags = (normal as u32) << 2 | (m.vertex_colors as u32) << 5 | (m.unlit as u32) << 6 | (bump as u32) << 7;
            let normal_scale = m.bump.map_or(m.normal_scale, |b| [b, b]);
            let u = MatU {
                base: m.base,
                emissive: [m.emissive[0], m.emissive[1], m.emissive[2], 0.],
                pbr: [m.metallic, m.roughness, m.occlusion, if let Alpha::Mask(c) = m.alpha { c } else { -1. }],
                misc: [normal_scale[0], normal_scale[1], flags as f32, if m.alpha == Alpha::Blend { 0. } else { 1. }],
                uv0: [m.uv_transform[0], m.uv_transform[1], m.uv_transform[2], 0.],
                uv1: [m.uv_transform[3], m.uv_transform[4], 0., 0.],
            };
            let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("model3d material"), contents: bytemuck::bytes_of(&u), usage: wgpu::BufferUsages::UNIFORM });
            let tv = |t: &Option<TexRef>, srgb: bool, view: &mut dyn FnMut(&Option<TexRef>, bool) -> Option<wgpu::TextureView>| view(t, srgb).unwrap_or_else(|| self.white.clone());
            let base = tv(&m.base_tex, true, &mut view);
            let mr = tv(&m.mr_tex, false, &mut view);
            let normal = tv(&m.normal_tex, false, &mut view);
            let occ = tv(&m.occlusion_tex, false, &mut view);
            let emit = tv(&m.emissive_tex, true, &mut view);
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("model3d material"),
                layout: &self.mat_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&base) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&mr) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&normal) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&occ) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&emit) },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&sampler) },
                ],
            });
            materials.push(GpuMat { bind, blend: m.alpha == Alpha::Blend, double: m.double_sided });
        }
        let fallback_mat = materials.len() - 1;
        let skinned_mesh: Vec<bool> = (0..scene.meshes.len()).map(|i| scene.nodes.iter().any(|n| n.mesh == Some(i) && n.skin.is_some())).collect();
        let meshes = scene
            .meshes
            .iter()
            .enumerate()
            .map(|(mi, mesh)| {
                mesh.prims
                    .iter()
                    .filter(|p| !p.idx.is_empty())
                    .map(|p| {
                        let verts: Vec<Vertex> = (0..p.pos.len())
                            .map(|i| Vertex {
                                pos: p.pos[i],
                                nrm: p.nrm.get(i).copied().unwrap_or([0., 1., 0.]),
                                uv: p.uv.get(i).copied().unwrap_or([0.; 2]),
                                color: p.color.as_ref().and_then(|c| c.get(i).copied()).unwrap_or([1.; 4]),
                                joint: p.joints.as_ref().and_then(|c| c.get(i).copied()).unwrap_or([0; 4]),
                                weight: p.weights.as_ref().and_then(|c| c.get(i).copied()).unwrap_or([0.; 4]),
                            })
                            .collect();
                        let morphed = !p.targets.is_empty();
                        let usage = if morphed { wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST } else { wgpu::BufferUsages::VERTEX };
                        Part {
                            vbuf: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("model3d vertices"), contents: bytemuck::cast_slice(&verts), usage }),
                            ibuf: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("model3d indices"), contents: bytemuck::cast_slice(&p.idx), usage: wgpu::BufferUsages::INDEX }),
                            count: p.idx.len() as u32,
                            material: p.material.filter(|&m| m < fallback_mat).unwrap_or(fallback_mat),
                            skinned: skinned_mesh[mi] && p.joints.is_some(),
                            morph: morphed.then(|| Morph { rest: verts, targets: p.targets.clone() }),
                        }
                    })
                    .collect()
            })
            .collect();
        let (lo, hi) = scene.bounds(&scene.rest()).unwrap_or((Vec3::splat(-0.5), Vec3::splat(0.5)));
        let center = (lo + hi) * 0.5;
        let radius = ((hi - lo).length() / 2.).max(1e-3);
        Model { scene, meshes, materials, center, radius }
    }

    pub fn target(&self, w: u32, h: u32) -> Target {
        let tex = |format, samples, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("model3d target"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let stride = (w * 4).div_ceil(256) * 256;
        Target {
            w,
            h,
            msaa: tex(COLOR, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&wgpu::TextureViewDescriptor::default()),
            depth: tex(DEPTH, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&wgpu::TextureViewDescriptor::default()),
            resolve: tex(COLOR, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC),
            readback: self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("model3d readback"), size: (stride * h) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
            stride,
        }
    }

    /// One frame of `model`, BGRA with straight alpha, `w`×`h`.
    pub fn render(&mut self, model: &Model, target: &Target, shot: &Shot) -> Result<Vec<u8>, String> {
        let (w, h) = (target.w, target.h);
        let scene = &model.scene;
        let rest;
        let pose = match &shot.pose {
            Some(p) => p,
            None => {
                rest = scene.rest();
                &rest
            }
        };
        let world = scene.world(pose);
        let palettes = scene.palettes(&world);
        let root = Mat4::from_quat(shot.pivot) * Mat4::from_translation(-model.center);

        let mut joints: Vec<Mat4> = Vec::new();
        let mut offsets = Vec::with_capacity(palettes.len());
        for p in &palettes {
            offsets.push(joints.len() as u32);
            joints.extend(p.iter().copied());
        }
        let eye = Vec3::new(0.6, 0.45, 1.).normalize() * shot.distance;
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let proj = Mat4::perspective_rh(FOV.to_radians(), w as f32 / h.max(1) as f32, shot.distance / 100., shot.distance * 100.);

        struct Draw<'a> {
            part: &'a Part,
            obj: u32,
            depth: f32,
        }
        let mut objs: Vec<ObjU> = Vec::new();
        let mut draws: Vec<Draw> = Vec::new();
        for (ni, node) in scene.nodes.iter().enumerate() {
            let Some(parts) = node.mesh.and_then(|m| model.meshes.get(m)) else { continue };
            let skin = node.skin.filter(|&s| s < palettes.len());
            let weights = shot
                .morph
                .as_ref()
                .and_then(|m| m.get(ni)?.as_deref())
                .or(node.weights.as_deref())
                .or_else(|| node.mesh.map(|m| scene.meshes[m].weights.as_slice()))
                .unwrap_or_default();
            for part in parts {
                if let Some(m) = &part.morph {
                    self.queue.write_buffer(&part.vbuf, 0, bytemuck::cast_slice(&m.apply(weights)));
                }
                let skinned = part.skinned && skin.is_some();
                let m = if skinned { root } else { root * world[ni] };
                let o = objs.len() as u32;
                objs.push(ObjU {
                    model: m.to_cols_array_2d(),
                    normal: m.inverse().transpose().to_cols_array_2d(),
                    skin: [skinned as u32, skin.map_or(0, |s| offsets[s]), 0, 0],
                });
                let depth = (view * m).transform_point3(Vec3::ZERO).z;
                draws.push(Draw { part, obj: o, depth });
            }
        }
        if objs.len() as u64 * OBJ_STRIDE > self.obj_buf.size() || (joints.len().max(1) as u64 * 64) > self.joint_buf.size() {
            let (a, b, c) = Self::object_buffers(&self.device, &self.obj_layout, (objs.len() as u64).next_power_of_two().max(64), (joints.len() as u64).next_power_of_two().max(256));
            self.obj_buf = a;
            self.joint_buf = b;
            self.obj_bind = c;
        }
        let mut obj_bytes = vec![0u8; objs.len() * OBJ_STRIDE as usize];
        for (i, o) in objs.iter().enumerate() {
            let b = bytemuck::bytes_of(o);
            obj_bytes[i * OBJ_STRIDE as usize..i * OBJ_STRIDE as usize + b.len()].copy_from_slice(b);
        }
        if !obj_bytes.is_empty() {
            self.queue.write_buffer(&self.obj_buf, 0, &obj_bytes);
        }
        if !joints.is_empty() {
            let flat: Vec<[f32; 16]> = joints.iter().map(|m| m.to_cols_array()).collect();
            self.queue.write_buffer(&self.joint_buf, 0, bytemuck::cast_slice(&flat));
        }
        let fu = FrameU { view_proj: (proj * view).to_cols_array_2d(), cam: [eye.x, eye.y, eye.z, 1.], env: [self.env_lod, 0., 0., 0.] };
        self.queue.write_buffer(&self.frame_buf, 0, bytemuck::bytes_of(&fu));

        // Opaque first, then the see-through back to front.
        let (mut opaque, mut clear): (Vec<&Draw>, Vec<&Draw>) = draws.iter().partition(|d| !model.materials[d.part.material].blend);
        opaque.sort_by_key(|d| d.part.material);
        clear.sort_by(|a, b| a.depth.total_cmp(&b.depth));
        let resolve = target.resolve.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("model3d") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("model3d"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.msaa,
                    depth_slice: None,
                    resolve_target: Some(&resolve),
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Discard },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_bind_group(0, &self.frame_bind, &[]);
            for d in opaque.iter().chain(clear.iter()) {
                let m = &model.materials[d.part.material];
                pass.set_pipeline(&self.pipelines[&(m.blend, m.double)]);
                pass.set_bind_group(1, &m.bind, &[]);
                pass.set_bind_group(2, &self.obj_bind, &[d.obj * OBJ_STRIDE as u32]);
                pass.set_vertex_buffer(0, d.part.vbuf.slice(..));
                pass.set_index_buffer(d.part.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..d.part.count, 0, 0..1);
            }
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &target.resolve, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &target.readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(target.stride), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let submission = self.queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        target.readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: Some(std::time::Duration::from_secs(10)) })
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        {
            let data = target.readback.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
            for row in data.chunks_exact(target.stride as usize).take(h as usize) {
                out.extend_from_slice(&row[..(w * 4) as usize]);
            }
        }
        target.readback.unmap();
        for px in out.chunks_exact_mut(4) {
            let a = px[3];
            if a != 0 && a != 255 {
                let k = 255. / a as f32;
                for c in &mut px[..3] {
                    *c = ((*c as f32 * k).round()).min(255.) as u8;
                }
            }
        }
        Ok(out)
    }
}
