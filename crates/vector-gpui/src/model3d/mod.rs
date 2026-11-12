//! 3D model attachments (`model3d/engine.js`): glTF, STL, OBJ and FBX, parsed off the UI thread
//! and drawn by one offscreen GPU device shared by every view. Nothing renders while idle: a
//! frame is drawn on a change, or per tick while an animation plays or the pedestal turns.

mod anim;
mod env;
mod fbx;
mod gltf;
mod gpu;
mod mesh_formats;
mod scene;
pub mod stage;
pub mod view;
pub mod viewer;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};

use glam::Quat;


pub const EXTENSIONS: [&str; 5] = ["glb", "gltf", "stl", "obj", "fbx"];

pub fn supports(ext: &str) -> bool {
    EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

pub enum Src {
    File(PathBuf),
    Held(Arc<dyn vector_core::media::MediaSource>),
}

impl Src {
    fn read(&self) -> Result<Vec<u8>, String> {
        match self {
            Src::File(p) => std::fs::read(p).map_err(|e| e.to_string()),
            Src::Held(s) => {
                let mut buf = vec![0u8; s.len() as usize];
                let mut at = 0;
                while at < buf.len() {
                    let n = s.read_at(at as u64, &mut buf[at..]).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    at += n;
                }
                buf.truncate(at);
                Ok(buf)
            }
        }
    }
}

pub struct Clip {
    pub name: String,
    pub duration: f32,
}

pub enum Out {
    Ready { clips: Vec<Clip>, radius: f32 },
    Failed(String),
    /// No GPU to draw with: the file shows as a file.
    Unavailable,
    Frame { w: u32, h: u32, bgra: Vec<u8> },
}

pub struct Shot {
    pub w: u32,
    pub h: u32,
    pub pivot: Quat,
    pub distance: f32,
    pub clip: Option<(usize, f32)>,
}

enum Job {
    Load { id: u64, src: Src, ext: String, out: Sender<Out>, wake: futures::channel::mpsc::UnboundedSender<()> },
    Parsed { id: u64, scene: Result<scene::Scene, String> },
    Render { id: u64, shot: Shot },
    Drop { id: u64 },
}

fn friendly(err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    if ["draco", "meshopt", "ktx2", "basisu"].iter().any(|k| lower.contains(k)) {
        return "This model is compressed in a way Vector can't open yet.".into();
    }
    if lower.contains("fbx version") || lower.contains("version not supported") {
        return "This FBX is too old to open. Re-export it as FBX 2011 or newer.".into();
    }
    if lower.contains("unsupported") {
        return "Vector can't open this kind of model.".into();
    }
    "This model couldn't be opened.".into()
}

fn parse(bytes: &[u8], ext: &str) -> Result<scene::Scene, String> {
    let mut s = match ext {
        "glb" | "gltf" => gltf::load(bytes),
        "stl" => mesh_formats::stl(bytes),
        "obj" => mesh_formats::obj(bytes),
        "fbx" => fbx::load(bytes),
        _ => Err("Unsupported model format".into()),
    }?;
    // Exporters leave empty takes behind (FBX's "Take 001"): nothing to play.
    s.clips.retain(|c| c.duration > 0.);
    Ok(s)
}

struct Slot {
    out: Sender<Out>,
    wake: futures::channel::mpsc::UnboundedSender<()>,
    model: Option<gpu::Model>,
    target: Option<gpu::Target>,
}

fn engine() -> &'static Sender<Job> {
    static ENGINE: OnceLock<Sender<Job>> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let back = tx.clone();
        std::thread::Builder::new()
            .name("model3d".into())
            .spawn(move || run(rx, back))
            .ok();
        tx
    })
}

fn run(rx: Receiver<Job>, back: Sender<Job>) {
    let mut gpu: Option<Result<gpu::Gpu, String>> = None;
    let mut slots: HashMap<u64, Slot> = HashMap::new();
    while let Ok(job) = rx.recv() {
        match job {
            Job::Load { id, src, ext, out, wake } => {
                let g = gpu.get_or_insert_with(|| {
                    let g = gpu::Gpu::new();
                    if let Err(e) = &g {
                        log::warn!("[3D] no GPU for models: {e}");
                    }
                    g
                });
                if g.is_err() {
                    let _ = out.send(Out::Unavailable);
                    let _ = wake.unbounded_send(());
                    continue;
                }
                slots.insert(id, Slot { out, wake, model: None, target: None });
                let back = back.clone();
                let ext = ext.to_ascii_lowercase();
                rayon::spawn(move || {
                    let scene = src.read().and_then(|b| parse(&b, &ext));
                    let _ = back.send(Job::Parsed { id, scene });
                });
            }
            Job::Parsed { id, scene } => {
                let (Some(slot), Some(Ok(g))) = (slots.get_mut(&id), gpu.as_ref()) else { continue };
                match scene {
                    Ok(scene) => {
                        let model = g.upload(scene);
                        let clips = model.scene.clips.iter().enumerate().map(|(i, c)| Clip { name: if c.name.is_empty() { format!("Animation {}", i + 1) } else { c.name.clone() }, duration: c.duration }).collect();
                        let _ = slot.out.send(Out::Ready { clips, radius: model.radius });
                        slot.model = Some(model);
                    }
                    Err(e) => {
                        log::warn!("[3D] model failed: {e}");
                        let _ = slot.out.send(Out::Failed(friendly(&e)));
                    }
                }
                let _ = slot.wake.unbounded_send(());
            }
            Job::Render { id, shot } => {
                let (Some(slot), Some(Ok(g))) = (slots.get_mut(&id), gpu.as_mut()) else { continue };
                let Some(model) = &slot.model else { continue };
                let (w, h) = (shot.w.clamp(1, 4096), shot.h.clamp(1, 4096));
                if slot.target.as_ref().is_none_or(|t| t.size() != (w, h)) {
                    slot.target = Some(g.target(w, h));
                }
                let Some(target) = &slot.target else { continue };
                let clip = shot.clip.and_then(|(i, t)| Some((model.scene.clips.get(i)?, t)));
                let pose = clip.map(|(c, t)| anim::pose(&model.scene.rest(), c, t));
                let morph = clip.map(|(c, t)| anim::weights(model.scene.nodes.len(), c, t));
                match g.render(model, target, &gpu::Shot { pivot: shot.pivot, distance: shot.distance, pose, morph }) {
                    Ok(bgra) => {
                        let _ = slot.out.send(Out::Frame { w, h, bgra });
                    }
                    Err(e) => log::warn!("[3D] frame failed: {e}"),
                }
                let _ = slot.wake.unbounded_send(());
            }
            Job::Drop { id } => {
                slots.remove(&id);
            }
        }
    }
}

fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Fields of view that frame a model: the narrower side of the stage decides.
pub fn fit(radius: f32, aspect: f32) -> f32 {
    let vfov = gpu::FOV.to_radians();
    let fov = vfov.min(2. * ((vfov / 2.).tan() * aspect).atan());
    radius / (fov / 2.).sin() * 1.05
}

#[cfg(test)]
mod tests {
    /// Renders each model in `MODEL3D_DIR` to `<name>.png` beside it, at the chat's 640 px.
    #[test]
    fn renders_sample_models() {
        let Ok(dir) = std::env::var("MODEL3D_DIR") else { return };
        let mut g = super::gpu::Gpu::new().expect("gpu");
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
            if !super::supports(&ext) {
                continue;
            }
            let t = std::time::Instant::now();
            let scene = match super::parse(&std::fs::read(&p).unwrap(), &ext) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{}: {e}", p.display());
                    continue;
                }
            };
            let parsed = t.elapsed();
            let model = g.upload(scene);
            let target = g.target(640, 640);
            let clip = model.scene.clips.first();
            let pose = clip.map(|c| super::anim::pose(&model.scene.rest(), c, c.duration * 0.3));
            let morph = clip.map(|c| super::anim::weights(model.scene.nodes.len(), c, c.duration * 0.3));
            let shot = super::gpu::Shot { pivot: glam::Quat::IDENTITY, distance: super::fit(model.radius, 1.), pose, morph };
            let t2 = std::time::Instant::now();
            let px = g.render(&model, &target, &shot).unwrap();
            eprintln!("{}: parse {parsed:?}, frame {:?}, clips {:?}", p.display(), t2.elapsed(), model.scene.clips.iter().map(|c| (&c.name, c.duration)).collect::<Vec<_>>());
            let mut rgba = px;
            for c in rgba.chunks_exact_mut(4) {
                c.swap(0, 2);
            }
            image::RgbaImage::from_raw(640, 640, rgba).unwrap().save(p.with_extension("png")).unwrap();
        }
    }
}

