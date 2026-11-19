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
            Src::File(p) => crate::pic::read(p).map_err(|e| e.to_string()),
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
    /// The scene while the GPU is let go, uploaded again by the next frame asked for.
    parked: Option<scene::Scene>,
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
    loop {
        // A GPU device holds tens of MB of driver memory; it goes after a while with no frame asked for.
        let job = match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(job) => job,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if gpu.as_ref().is_some_and(|g| g.is_ok()) {
                    for slot in slots.values_mut() {
                        slot.target = None;
                        if let Some(model) = slot.model.take() {
                            slot.parked = Some(model.scene);
                        }
                    }
                    gpu = None;
                    crate::trim::soon();
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match job {
            Job::Load { id, src, ext, out, wake } => {
                if bring_up(&mut gpu).is_err() {
                    let _ = out.send(Out::Unavailable);
                    let _ = wake.unbounded_send(());
                    continue;
                }
                slots.insert(id, Slot { out, wake, model: None, target: None, parked: None });
                let back = back.clone();
                let ext = ext.to_ascii_lowercase();
                rayon::spawn(move || {
                    let scene = src.read().and_then(|b| {
                        std::panic::catch_unwind(|| parse(&b, &ext)).unwrap_or_else(|_| Err("damaged model".into()))
                    });
                    let _ = back.send(Job::Parsed { id, scene });
                });
            }
            Job::Parsed { id, scene } => {
                let Some(slot) = slots.get_mut(&id) else { continue };
                let Ok(g) = bring_up(&mut gpu) else { continue };
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
                let Some(slot) = slots.get_mut(&id) else { continue };
                let Ok(g) = bring_up(&mut gpu) else { continue };
                if slot.model.is_none()
                    && let Some(scene) = slot.parked.take()
                {
                    slot.model = Some(g.upload(scene));
                }
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

fn bring_up(gpu: &mut Option<Result<gpu::Gpu, String>>) -> Result<&mut gpu::Gpu, ()> {
    gpu.get_or_insert_with(|| {
        let g = gpu::Gpu::new();
        if let Err(e) = &g {
            log::warn!("[3D] no GPU for models: {e}");
        }
        g
    })
    .as_mut()
    .map_err(|_| ())
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
    fn footprint_mb() -> f64 {
        #[cfg(target_os = "macos")]
        {
            let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
            let ok = unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, &mut info as *mut _ as *mut libc::rusage_info_t) };
            if ok == 0 { info.ri_phys_footprint as f64 / 1048576. } else { -1. }
        }
        #[cfg(not(target_os = "macos"))]
        {
            std::fs::read_to_string("/proc/self/smaps_rollup")
                .ok()
                .and_then(|s| s.lines().filter(|l| l.starts_with("Private_")).map(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()).sum::<Option<f64>>())
                .map_or(-1., |kb| kb / 1024.)
        }
    }

    /// Footprint after each stage of showing `MODEL3D_PROBE`, at the chat's 640 px.
    #[test]
    fn footprint_probe() {
        let Ok(path) = std::env::var("MODEL3D_PROBE") else { return };
        let mark = |stage: &str| eprintln!("probe {stage:<14} {:>7.1} MB", footprint_mb());
        mark("start");
        let ext = std::path::Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let scene = super::parse(&std::fs::read(&path).unwrap(), &ext).unwrap();
        mark("parsed");
        let mut g = super::gpu::Gpu::new().expect("gpu");
        mark("device");
        let model = g.upload(scene);
        mark("uploaded");
        let target = g.target(640, 640);
        mark("target");
        if std::env::var_os("MODEL3D_STEPS").is_some() {
            g.probe_steps(&target, &mark);
        }
        let mut shot = super::gpu::Shot { pivot: glam::Quat::IDENTITY, distance: super::fit(model.radius, 1.), pose: None, morph: None };
        g.render(&model, &target, &shot).unwrap();
        mark("first frame");
        for i in 0..90 {
            shot.pivot = glam::Quat::from_rotation_y(i as f32 * 0.05);
            g.render(&model, &target, &shot).unwrap();
        }
        mark("90 frames");
        // Long enough for any pool the first frames held, and the frame loop doesn't use, to drain.
        if let Some(secs) = std::env::var("MODEL3D_SPIN").ok().and_then(|v| v.parse::<u64>().ok()) {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            let mut i = 0;
            while std::time::Instant::now() < until {
                shot.pivot = glam::Quat::from_rotation_y(i as f32 * 0.05);
                g.render(&model, &target, &shot).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(33));
                i += 1;
            }
            mark("spun");
        }
        drop((target, model));
        mark("model dropped");
        drop(g);
        mark("device dropped");
    }

    /// Damaged models from anyone: parsing, upload and a posed frame must not panic.
    /// `MODEL3D_FUZZ_DIR` adds sample files as seeds; `MODEL3D_FUZZ_ITERS` runs longer.
    #[test]
    fn no_damaged_model_panics() {
        let mut stl = vec![0u8; 80];
        stl.extend_from_slice(&2u32.to_le_bytes());
        for tri in [[0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0.], [0., 0., 1., 1., 1., 0., 0., 1., 0., 1., 0., 0.]] {
            for f in tri {
                stl.extend_from_slice(&f32::to_le_bytes(f));
            }
            stl.extend_from_slice(&[0, 0]);
        }
        let mut seeds: Vec<(Vec<u8>, String)> = vec![
            (stl, "stl".into()),
            (b"solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n".to_vec(), "stl".into()),
            (b"v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nvt 0 0\nvt 1 0\nvn 0 0 1\nf 1/1/1 2/2/1 3/1/1\nf 2 4 3\n".to_vec(), "obj".into()),
        ];
        let fox = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("reference/media/fox.glb");
        seeds.extend(std::fs::read(&fox).ok().map(|b| (b, "glb".to_string())));
        if let Ok(dir) = std::env::var("MODEL3D_FUZZ_DIR") {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or_default().to_ascii_lowercase();
                if super::supports(&ext) {
                    seeds.push((std::fs::read(e.path()).unwrap(), ext));
                }
            }
        }
        let mut g = super::gpu::Gpu::new().ok();
        if let Ok(file) = std::env::var("MODEL3D_FUZZ_REPLAY") {
            let ext = file.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
            if let Ok(scene) = super::parse(&std::fs::read(&file).unwrap(), &ext)
                && let Some(g) = g.as_mut()
            {
                let model = g.upload(scene);
                let target = g.target(32, 32);
                let clip = model.scene.clips.first();
                let pose = clip.map(|c| super::anim::pose(&model.scene.rest(), c, c.duration * 0.3));
                let morph = clip.map(|c| super::anim::weights(model.scene.nodes.len(), c, c.duration * 0.3));
                let _ = g.render(&model, &target, &super::gpu::Shot { pivot: glam::Quat::IDENTITY, distance: super::fit(model.radius, 1.), pose, morph });
            }
            return;
        }
        let iters: usize = std::env::var("MODEL3D_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
        let mut seed: u64 = std::env::var("MODEL3D_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x9e37_79b9_7f4a_7c15);
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        const NUMS: [&[u8]; 9] = [b"-1", b"0", b"99999999", b"1e39", b"nan", b"-2147483648", b"4294967296", b"-3", b"65535"];
        // Overflow inside a dependency panics only with debug assertions; release wraps it.
        static WRAPS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
            let msg = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_default();
            if msg.contains("with overflow") && at.contains("/.cargo/registry/") {
                WRAPS.lock().unwrap().push(at);
            } else {
                default_hook(info);
            }
        }));
        let mut failed = Vec::new();
        for i in 0..iters {
            let (mut b, ext) = seeds[i % seeds.len()].clone();
            let text = b.iter().take(256).all(|c| c.is_ascii());
            for _ in 0..1 + next() % 6 {
                let at = (next() % b.len().max(1) as u64) as usize;
                match next() % if text { 7 } else { 6 } {
                    0 => b[at] = next() as u8,
                    1 => b[at] = 0xff,
                    2 => b[at] = 0,
                    3 => b.truncate(at.max(8)),
                    4 => b[at..].iter_mut().take(4).for_each(|x| *x = 0xff),
                    5 => b[at] ^= 1 << (next() % 8),
                    _ => {
                        let n = NUMS[(next() % NUMS.len() as u64) as usize];
                        let end = (at..b.len()).find(|&j| !b[j].is_ascii_digit()).unwrap_or(b.len());
                        b.splice(at..end, n.iter().copied());
                    }
                }
            }
            let wraps_before = WRAPS.lock().unwrap().len();
            let t = std::time::Instant::now();
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Ok(scene) = super::parse(&b, &ext) else { return };
                let Some(g) = g.as_mut() else { return };
                let model = g.upload(scene);
                let target = g.target(32, 32);
                let clip = model.scene.clips.first();
                let pose = clip.map(|c| super::anim::pose(&model.scene.rest(), c, c.duration * 0.3));
                let morph = clip.map(|c| super::anim::weights(model.scene.nodes.len(), c, c.duration * 0.3));
                let shot = super::gpu::Shot { pivot: glam::Quat::IDENTITY, distance: super::fit(model.radius, 1.), pose, morph };
                let _ = g.render(&model, &target, &shot);
            }));
            let wrapped = run.is_err() && WRAPS.lock().unwrap().len() > wraps_before;
            if (run.is_err() && !wrapped) || t.elapsed() > std::time::Duration::from_secs(5) {
                eprintln!("case {i} ({ext}, {} bytes): {}", b.len(), if run.is_err() { "panicked" } else { "slow" });
                failed.push((ext, b));
                if failed.len() >= 3 {
                    break;
                }
            }
        }
        if let Ok(out) = std::env::var("MODEL3D_FUZZ_OUT") {
            for (i, (ext, b)) in failed.iter().enumerate() {
                std::fs::write(format!("{out}/bad-{i}.{ext}"), b).unwrap();
            }
        }
        let _ = std::panic::take_hook();
        let mut wraps = WRAPS.lock().unwrap().clone();
        wraps.sort();
        wraps.dedup();
        if !wraps.is_empty() {
            eprintln!("dependency overflow (wraps in release): {wraps:?}");
        }
        assert!(failed.is_empty(), "{} damaged models panicked or stalled", failed.len());
    }

    /// Renders each model in `MODEL3D_DIR` to `<name>.png` beside it, at the chat's 640 px.
    #[test]
    fn renders_sample_models() {
        let Ok(dir) = std::env::var("MODEL3D_DIR") else { return };
        let mut g = super::gpu::Gpu::new().expect("gpu");
        eprintln!("direct path: {}", g.is_direct());
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
            if !super::supports(&ext) {
                continue;
            }
            let t = std::time::Instant::now();
            let scene = match super::parse(&crate::pic::read(&p).unwrap(), &ext) {
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

