//! A model as every loader leaves it: a node tree of TRS transforms, triangle meshes, metal-rough
//! materials, skins and keyframed clips. The renderer and the animator read nothing else.

use glam::{Mat4, Quat, Vec3};

#[derive(Default)]
pub struct Scene {
    pub nodes: Vec<Node>,
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    pub images: Vec<Image>,
    pub skins: Vec<Skin>,
    pub clips: Vec<Clip>,
}

#[derive(Clone)]
pub struct Node {
    pub parent: Option<usize>,
    pub t: Vec3,
    pub r: Quat,
    pub s: Vec3,
    pub mesh: Option<usize>,
    pub skin: Option<usize>,
    /// Morph weights set on the node, over its mesh's own.
    pub weights: Option<Vec<f32>>,
}

impl Default for Node {
    fn default() -> Self {
        Self { parent: None, t: Vec3::ZERO, r: Quat::IDENTITY, s: Vec3::ONE, mesh: None, skin: None, weights: None }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Trs {
    pub t: Vec3,
    pub r: Quat,
    pub s: Vec3,
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.s, self.r, self.t)
    }
}

#[derive(Default)]
pub struct Mesh {
    pub prims: Vec<Prim>,
    pub weights: Vec<f32>,
}

/// One morph target's offsets; `nrm` is empty when the target moves positions only.
#[derive(Clone, Default)]
pub struct Target {
    pub pos: Vec<[f32; 3]>,
    pub nrm: Vec<[f32; 3]>,
}

#[derive(Default)]
pub struct Prim {
    pub pos: Vec<[f32; 3]>,
    pub nrm: Vec<[f32; 3]>,
    pub uv: Vec<[f32; 2]>,
    pub color: Option<Vec<[f32; 4]>>,
    pub joints: Option<Vec<[u16; 4]>>,
    pub weights: Option<Vec<[f32; 4]>>,
    pub targets: Vec<Target>,
    pub idx: Vec<u32>,
    pub material: Option<usize>,
}

impl Prim {
    /// Missing normals become face normals, as three.js gives a geometry without them; a
    /// shared vertex is split so each face keeps its own.
    pub fn finish(&mut self) {
        let n = self.pos.len();
        if self.idx.is_empty() {
            self.idx = (0..n as u32).collect();
        }
        self.idx.retain(|&i| (i as usize) < n);
        self.idx.truncate(self.idx.len() / 3 * 3);
        if self.uv.len() != n {
            self.uv = vec![[0.; 2]; n];
        }
        if self.color.as_ref().is_some_and(|c| c.len() != n) {
            self.color = None;
        }
        if self.joints.as_ref().is_some_and(|j| j.len() != n) || self.weights.as_ref().is_some_and(|w| w.len() != n) {
            self.joints = None;
            self.weights = None;
        }
        for t in &mut self.targets {
            t.pos.resize(n, [0.; 3]);
            if t.nrm.len() != n {
                t.nrm.clear();
            }
        }
        if self.nrm.len() == n {
            return;
        }
        let idx = std::mem::take(&mut self.idx);
        let pick = |v: &Vec<[f32; 3]>| idx.iter().map(|&i| v[i as usize]).collect::<Vec<_>>();
        let pos = pick(&self.pos);
        self.uv = idx.iter().map(|&i| self.uv[i as usize]).collect();
        self.color = self.color.take().map(|c| idx.iter().map(|&i| c[i as usize]).collect());
        self.joints = self.joints.take().map(|c| idx.iter().map(|&i| c[i as usize]).collect());
        self.weights = self.weights.take().map(|c| idx.iter().map(|&i| c[i as usize]).collect());
        for t in &mut self.targets {
            t.pos = pick(&t.pos);
            t.nrm.clear();
        }
        self.nrm = pos
            .chunks_exact(3)
            .flat_map(|t| {
                let (a, b, c) = (Vec3::from(t[0]), Vec3::from(t[1]), Vec3::from(t[2]));
                let n = (b - a).cross(c - a).normalize_or_zero().to_array();
                [n, n, n]
            })
            .collect();
        self.idx = (0..pos.len() as u32).collect();
        self.pos = pos;
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Alpha {
    Opaque,
    Mask(f32),
    Blend,
}

#[derive(Clone, Copy)]
pub struct TexRef {
    pub image: usize,
    pub wrap: [Wrap; 2],
    pub nearest: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Wrap {
    Repeat,
    Mirror,
    Clamp,
}

#[derive(Clone)]
pub struct Material {
    pub base: [f32; 4],
    pub base_tex: Option<TexRef>,
    pub metallic: f32,
    pub roughness: f32,
    pub mr_tex: Option<TexRef>,
    pub normal_tex: Option<TexRef>,
    pub normal_scale: [f32; 2],
    pub occlusion_tex: Option<TexRef>,
    pub occlusion: f32,
    pub emissive: [f32; 3],
    pub emissive_tex: Option<TexRef>,
    pub alpha: Alpha,
    pub double_sided: bool,
    pub unlit: bool,
    pub vertex_colors: bool,
    /// `KHR_texture_transform` of the base colour map: offset, rotation, scale.
    pub uv_transform: [f32; 5],
}

impl Default for Material {
    fn default() -> Self {
        Self {
            base: [1.; 4],
            base_tex: None,
            metallic: 1.,
            roughness: 1.,
            mr_tex: None,
            normal_tex: None,
            normal_scale: [1., 1.],
            occlusion_tex: None,
            occlusion: 1.,
            emissive: [0.; 3],
            emissive_tex: None,
            alpha: Alpha::Opaque,
            double_sided: false,
            unlit: false,
            vertex_colors: true,
            uv_transform: [0., 0., 0., 1., 1.],
        }
    }
}

impl Material {
    /// three.js's viewer grey, for formats that carry no material of their own.
    pub fn plain() -> Self {
        let lin = |v: f32| srgb_to_linear(v);
        Self { base: [lin(0xb8 as f32 / 255.), lin(0xb8 as f32 / 255.), lin(0xc0 as f32 / 255.), 1.], metallic: 0.1, roughness: 0.55, ..Self::default() }
    }
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c < 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Decoded pixels, RGBA8.
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

pub struct Skin {
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Path {
    T,
    R,
    S,
    /// Morph weights, as many per key as the mesh has targets.
    W,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Interp {
    Step,
    Linear,
    Cubic,
}

pub struct Channel {
    pub node: usize,
    pub path: Path,
    pub interp: Interp,
    pub times: Vec<f32>,
    /// Packed 3 or 4 wide per key (a weight channel, one per target); a cubic key is in-tangent, value, out-tangent.
    pub values: Vec<f32>,
}

pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub channels: Vec<Channel>,
}

impl Scene {
    pub fn rest(&self) -> Vec<Trs> {
        self.nodes.iter().map(|n| Trs { t: n.t, r: n.r, s: n.s }).collect()
    }

    /// World matrices for a pose; parents need not come first.
    pub fn world(&self, pose: &[Trs]) -> Vec<Mat4> {
        let mut out: Vec<Option<Mat4>> = vec![None; self.nodes.len()];
        fn get(s: &Scene, pose: &[Trs], out: &mut [Option<Mat4>], i: usize, depth: u32) -> Mat4 {
            if let Some(m) = out[i] {
                return m;
            }
            let local = pose[i].matrix();
            let m = match s.nodes[i].parent {
                Some(p) if depth < 512 => get(s, pose, out, p, depth + 1) * local,
                _ => local,
            };
            out[i] = Some(m);
            m
        }
        for i in 0..self.nodes.len() {
            get(self, pose, &mut out, i, 0);
        }
        out.into_iter().map(|m| m.unwrap_or(Mat4::IDENTITY)).collect()
    }

    /// Each skin's joint palette for a pose.
    pub fn palettes(&self, world: &[Mat4]) -> Vec<Vec<Mat4>> {
        self.skins
            .iter()
            .map(|s| {
                s.joints
                    .iter()
                    .enumerate()
                    .map(|(k, &j)| world.get(j).copied().unwrap_or(Mat4::IDENTITY) * s.inverse_bind.get(k).copied().unwrap_or(Mat4::IDENTITY))
                    .collect()
            })
            .collect()
    }

    /// The posed bounds, skinned vertices included.
    pub fn bounds(&self, pose: &[Trs]) -> Option<(Vec3, Vec3)> {
        let world = self.world(pose);
        let palettes = self.palettes(&world);
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for (i, n) in self.nodes.iter().enumerate() {
            let Some(mesh) = n.mesh.and_then(|m| self.meshes.get(m)) else { continue };
            let skin = n.skin.and_then(|s| palettes.get(s));
            for p in &mesh.prims {
                for (v, pos) in p.pos.iter().enumerate() {
                    let pos = Vec3::from(*pos);
                    let at = match (skin, &p.joints, &p.weights) {
                        (Some(pal), Some(j), Some(w)) => {
                            let mut m = Mat4::ZERO;
                            let mut sum = 0.;
                            for k in 0..4 {
                                if w[v][k] > 0. {
                                    m += pal.get(j[v][k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[v][k];
                                    sum += w[v][k];
                                }
                            }
                            if sum > 0. { m.transform_point3(pos) } else { world[i].transform_point3(pos) }
                        }
                        _ => world[i].transform_point3(pos),
                    };
                    if at.is_finite() {
                        lo = lo.min(at);
                        hi = hi.max(at);
                    }
                }
            }
        }
        (lo.x <= hi.x).then_some((lo, hi))
    }
}
