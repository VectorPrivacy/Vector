//! The quads pipeline on GL. Naga's GLSL for `vs_quad` carries the whole quad fragment stage as
//! dead code, which some GL compilers (PowerVR's) refuse; `shaders_quad_lean.wgsl` leaves it out.

use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) const LEAN_SHADERS: &str = concat!(
    include_str!("shaders.wgsl"),
    include_str!("shaders_storage.wgsl"),
    include_str!("shaders_quad_lean.wgsl"),
);

const STOCK: u8 = 1;
const LEAN: u8 = 2;
const BARE: u8 = 3;

/// What the first renderer found, so a rebuilt one never compiles a refused stage again.
static CHOSEN: AtomicU8 = AtomicU8::new(0);

/// What each probe isolates of `vs_quad`, with its vertex and fragment entry points.
const PROBES: [(&str, &str, &str); 6] = [
    ("the lean vertex stage", "vs_quad_lean", "fs_probe"),
    ("a whole Quad copied from storage", "vs_probe_copy", "fs_probe"),
    ("the gradient prelude", "vs_probe_gradient", "fs_probe"),
    ("quad fragment code, dead in a vertex stage", "vs_probe_dead", "fs_probe"),
    ("the content mask, fragment", "vs_quad_lean", "fs_probe_mask"),
    ("quad_color, fragment", "vs_quad_lean", "fs_probe_color"),
];

/// `build` makes a pipeline from (label, vertex entry, fragment entry, module).
pub(crate) fn pipeline(
    device: &wgpu::Device,
    stock: &wgpu::ShaderModule,
    build: impl Fn(&str, &str, &str, &wgpu::ShaderModule) -> wgpu::RenderPipeline,
) -> wgpu::RenderPipeline {
    match CHOSEN.load(Ordering::Relaxed) {
        STOCK => return build("quads", "vs_quad", "fs_quad", stock),
        LEAN => return build("quads", "vs_quad_lean", "fs_quad", &lean_module(device)),
        BARE => return build("quads", "vs_quad_bare", "fs_quad", &lean_module(device)),
        _ => {}
    }
    if forced_lean() {
        log::warn!("quads: the lean vertex stage, as asked");
        CHOSEN.store(LEAN, Ordering::Relaxed);
        return build("quads", "vs_quad_lean", "fs_quad", &lean_module(device));
    }
    let (pipeline, error) = caught(device, || build("quads", "vs_quad", "fs_quad", stock));
    let Some(error) = error else {
        CHOSEN.store(STOCK, Ordering::Relaxed);
        return pipeline;
    };
    log::warn!("the GPU refused the quads shader, trying a lean vertex stage: {error}");
    let lean = lean_module(device);
    for (what, vs, fs) in PROBES {
        let (_, error) = caught(device, || build("quads_probe", vs, fs, &lean));
        log::warn!("quads probe, {what}: {}", if error.is_some() { "refused" } else { "compiles" });
    }
    let (pipeline, error) = caught(device, || build("quads", "vs_quad_lean", "fs_quad", &lean));
    let Some(error) = error else {
        log::warn!("quads: drawing with the lean vertex stage");
        CHOSEN.store(LEAN, Ordering::Relaxed);
        return pipeline;
    };
    log::warn!("the GPU refused the lean vertex stage too, trying a bare one: {error}");
    let (pipeline, error) = caught(device, || build("quads", "vs_quad_bare", "fs_quad", &lean));
    match error {
        None => log::warn!("quads: drawing with the bare vertex stage"),
        Some(error) => log::error!("quads: the bare vertex stage was refused too: {error}"),
    }
    CHOSEN.store(BARE, Ordering::Relaxed);
    pipeline
}

/// Lets a GPU that takes `vs_quad` reproduce what a refusing one draws: `debug.vector.gpu=gl-lean`
/// on Android, `VECTOR_GL_LEAN=1` elsewhere.
fn forced_lean() -> bool {
    #[cfg(target_os = "android")]
    return crate::wgpu_context::android_property(c"debug.vector.gpu") == "gl-lean";
    #[cfg(not(target_os = "android"))]
    std::env::var_os("VECTOR_GL_LEAN").is_some()
}

fn lean_module(device: &wgpu::Device) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpui_quad_lean_shaders"),
        source: wgpu::ShaderSource::Wgsl(LEAN_SHADERS.into()),
    })
}

/// Captured here, an error never reaches the device's handler, which counts it a failed frame.
fn caught(
    device: &wgpu::Device,
    build: impl FnOnce() -> wgpu::RenderPipeline,
) -> (wgpu::RenderPipeline, Option<wgpu::Error>) {
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline = build();
    let validation = gpui::block_on(validation.pop());
    let internal = gpui::block_on(internal.pop());
    (pipeline, validation.or(internal))
}
