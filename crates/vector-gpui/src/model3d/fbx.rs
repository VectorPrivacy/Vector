//! FBX 7.x, binary or ASCII, read the way three.js's FBXLoader reads it: models with their
//! pivots folded into TRS, polygon meshes fanned into triangles, clusters as skins, and each
//! animation stack as a clip. Textures come only from embedded video content.

use std::collections::HashMap;

use glam::{Mat4, Quat, Vec3};

use super::scene::*;

const TICKS_PER_SECOND: f64 = 46_186_158_000.;

#[derive(Clone)]
enum Val {
    Int(i64),
    Float(f64),
    Str(String),
    Ints(Vec<i64>),
    Floats(Vec<f64>),
    Bin(Vec<u8>),
}

impl Val {
    fn num(&self) -> Option<f64> {
        match self {
            Val::Int(i) => Some(*i as f64),
            Val::Float(f) => Some(*f),
            _ => None,
        }
    }
    fn int(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            Val::Float(f) => Some(*f as i64),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    fn floats(&self) -> Vec<f64> {
        match self {
            Val::Floats(v) => v.clone(),
            Val::Ints(v) => v.iter().map(|&i| i as f64).collect(),
            Val::Int(i) => vec![*i as f64],
            Val::Float(f) => vec![*f],
            _ => Vec::new(),
        }
    }
    fn ints(&self) -> Vec<i64> {
        match self {
            Val::Ints(v) => v.clone(),
            Val::Floats(v) => v.iter().map(|&f| f as i64).collect(),
            Val::Int(i) => vec![*i],
            _ => Vec::new(),
        }
    }
}

#[derive(Default)]
struct FNode {
    name: String,
    attrs: Vec<Val>,
    kids: Vec<FNode>,
}

impl FNode {
    fn kid(&self, name: &str) -> Option<&FNode> {
        self.kids.iter().find(|k| k.name == name)
    }
    fn kids<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a FNode> + 'a {
        self.kids.iter().filter(move |k| k.name == name)
    }
    fn arr_f(&self, name: &str) -> Vec<f64> {
        self.kid(name).and_then(|k| k.attrs.first()).map(|a| a.floats()).unwrap_or_default()
    }
    fn arr_i(&self, name: &str) -> Vec<i64> {
        self.kid(name).and_then(|k| k.attrs.first()).map(|a| a.ints()).unwrap_or_default()
    }
    fn text(&self, name: &str) -> Option<&str> {
        self.kid(name).and_then(|k| k.attrs.first()).and_then(|a| a.str())
    }
}

fn from_cel(n: fbxcel::tree::v7400::NodeHandle) -> FNode {
    use fbxcel::low::v7400::AttributeValue as A;
    FNode {
        name: n.name().to_string(),
        attrs: n
            .attributes()
            .iter()
            .map(|a| match a {
                A::Bool(b) => Val::Int(*b as i64),
                A::I16(v) => Val::Int(*v as i64),
                A::I32(v) => Val::Int(*v as i64),
                A::I64(v) => Val::Int(*v),
                A::F32(v) => Val::Float(*v as f64),
                A::F64(v) => Val::Float(*v),
                A::ArrBool(v) => Val::Ints(v.iter().map(|&b| b as i64).collect()),
                A::ArrI32(v) => Val::Ints(v.iter().map(|&b| b as i64).collect()),
                A::ArrI64(v) => Val::Ints(v.clone()),
                A::ArrF32(v) => Val::Floats(v.iter().map(|&b| b as f64).collect()),
                A::ArrF64(v) => Val::Floats(v.clone()),
                A::String(s) => Val::Str(s.clone()),
                A::Binary(b) => Val::Bin(b.clone()),
            })
            .collect(),
        kids: n.children().map(from_cel).collect(),
    }
}

/// The ASCII form: `Name: a, b, c { … }`, arrays as `*N { a: … }`.
fn parse_ascii(text: &str) -> Result<FNode, String> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut stack: Vec<FNode> = vec![FNode::default()];
    let skip_ws = |i: &mut usize| {
        while *i < b.len() {
            match b[*i] {
                b' ' | b'\t' | b'\r' | b'\n' => *i += 1,
                b';' => {
                    while *i < b.len() && b[*i] != b'\n' {
                        *i += 1;
                    }
                }
                _ => break,
            }
        }
    };
    let value = |i: &mut usize| -> Option<Val> {
        if *i >= b.len() {
            return None;
        }
        if b[*i] == b'"' {
            let start = *i + 1;
            let mut j = start;
            while j < b.len() && b[j] != b'"' {
                j += 1;
            }
            *i = (j + 1).min(b.len());
            return Some(Val::Str(String::from_utf8_lossy(&b[start..j.min(b.len())]).into_owned()));
        }
        let start = *i;
        while *i < b.len() && !matches!(b[*i], b',' | b'\n' | b'\r' | b'{' | b'}') {
            *i += 1;
        }
        let tok = std::str::from_utf8(&b[start..*i]).ok()?.trim();
        if tok.is_empty() {
            return None;
        }
        Some(if let Ok(v) = tok.parse::<i64>() {
            Val::Int(v)
        } else if let Ok(v) = tok.parse::<f64>() {
            Val::Float(v)
        } else {
            Val::Str(tok.to_string())
        })
    };
    loop {
        skip_ws(&mut i);
        if i >= b.len() {
            break;
        }
        if b[i] == b'}' {
            i += 1;
            let done = stack.pop().ok_or("unbalanced")?;
            stack.last_mut().ok_or("unbalanced")?.kids.push(done);
            continue;
        }
        let start = i;
        while i < b.len() && b[i] != b':' && b[i] != b'\n' {
            i += 1;
        }
        if i >= b.len() || b[i] != b':' {
            continue;
        }
        let name = String::from_utf8_lossy(&b[start..i]).trim().to_string();
        i += 1;
        let mut node = FNode { name, ..Default::default() };
        // An array: `*N {` then `a: v, v, …` and `}`.
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        if i < b.len() && b[i] == b'*' {
            while i < b.len() && b[i] != b'{' {
                i += 1;
            }
            i += 1;
            skip_ws(&mut i);
            if b.get(i) == Some(&b'a') && b.get(i + 1) == Some(&b':') {
                i += 2;
            }
            let start = i;
            while i < b.len() && b[i] != b'}' {
                i += 1;
            }
            let body = std::str::from_utf8(&b[start..i]).unwrap_or_default();
            i += 1;
            let nums: Vec<&str> = body.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).collect();
            node.attrs.push(if nums.iter().all(|s| s.parse::<i64>().is_ok()) {
                Val::Ints(nums.iter().filter_map(|s| s.parse().ok()).collect())
            } else {
                Val::Floats(nums.iter().filter_map(|s| s.parse().ok()).collect())
            });
            stack.last_mut().ok_or("unbalanced")?.kids.push(node);
            continue;
        }
        loop {
            while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                i += 1;
            }
            if let Some(v) = value(&mut i) {
                node.attrs.push(v);
            }
            while i < b.len() && (b[i] == b' ' || b[i] == b'\t' || b[i] == b'\r') {
                i += 1;
            }
            if i < b.len() && b[i] == b',' {
                i += 1;
                skip_ws(&mut i);
                continue;
            }
            break;
        }
        if i < b.len() && b[i] == b'{' {
            i += 1;
            stack.push(node);
        } else {
            stack.last_mut().ok_or("unbalanced")?.kids.push(node);
        }
    }
    while stack.len() > 1 {
        let done = stack.pop().unwrap_or_default();
        if let Some(top) = stack.last_mut() {
            top.kids.push(done);
        }
    }
    stack.pop().ok_or_else(|| "empty".into())
}

fn root(bytes: &[u8]) -> Result<FNode, String> {
    if bytes.starts_with(b"Kaydara FBX Binary") {
        let tree = fbxcel::tree::any::AnyTree::from_seekable_reader(std::io::Cursor::new(bytes)).map_err(|e| {
            let t = e.to_string();
            if t.to_ascii_lowercase().contains("version") { format!("FBX version not supported: {t}") } else { t }
        })?;
        match tree {
            fbxcel::tree::any::AnyTree::V7400(_, tree, _) => Ok(from_cel(tree.root())),
            _ => Err("FBX version not supported".into()),
        }
    } else {
        let text = String::from_utf8_lossy(bytes);
        if let Some(v) = text.lines().take(5).find_map(|l| l.strip_prefix("; FBX ")).and_then(|l| l.split('.').next()).and_then(|m| m.trim().parse::<u32>().ok())
            && v < 7
        {
            return Err("FBX version not supported".into());
        }
        parse_ascii(&text)
    }
}

fn props(node: &FNode) -> HashMap<String, Vec<Val>> {
    let mut out = HashMap::new();
    if let Some(p70) = node.kid("Properties70") {
        for p in p70.kids("P") {
            if let Some(name) = p.attrs.first().and_then(|a| a.str()) {
                out.insert(name.to_string(), p.attrs.iter().skip(4).cloned().collect());
            }
        }
    }
    out
}

fn v3(p: &HashMap<String, Vec<Val>>, k: &str) -> Option<Vec3> {
    let v = p.get(k)?;
    Some(Vec3::new(v.first()?.num()? as f32, v.get(1)?.num()? as f32, v.get(2)?.num()? as f32))
}

fn num(p: &HashMap<String, Vec<Val>>, k: &str) -> Option<f64> {
    p.get(k)?.first()?.num()
}

/// three.js's Euler order names for FBX's `RotationOrder`, outer axis first.
fn order(i: i64) -> &'static str {
    ["ZYX", "YZX", "XZY", "ZXY", "YXZ", "XYZ"].get(i as usize).copied().unwrap_or("ZYX")
}

fn euler(deg: Vec3, order: &str) -> Quat {
    let r = deg * (std::f32::consts::PI / 180.);
    order.chars().fold(Quat::IDENTITY, |q, c| {
        q * match c {
            'X' => Quat::from_rotation_x(r.x),
            'Y' => Quat::from_rotation_y(r.y),
            _ => Quat::from_rotation_z(r.z),
        }
    })
}

fn object_name(n: &FNode) -> String {
    let raw = n.attrs.get(1).and_then(|a| a.str()).unwrap_or_default();
    if let Some((name, _)) = raw.split_once("\u{0}\u{1}") {
        return name.to_string();
    }
    raw.rsplit_once("::").map_or(raw, |(_, n)| n).to_string()
}

struct Layer {
    by: u8,
    index: Option<Vec<i64>>,
    data: Vec<f64>,
}

impl Layer {
    fn read(geom: &FNode, elem: &str, data: &str, index: &str) -> Option<Self> {
        let l = geom.kid(elem)?;
        let mapping = l.text("MappingInformationType").unwrap_or("ByPolygonVertex");
        let reference = l.text("ReferenceInformationType").unwrap_or("Direct");
        let by = match mapping {
            "ByPolygonVertex" => 0,
            "ByPolygon" => 1,
            "ByVertice" | "ByVertex" | "ByControlPoint" => 2,
            _ => 3,
        };
        let data = l.arr_f(data);
        let index = matches!(reference, "IndexToDirect" | "Index").then(|| l.arr_i(index)).filter(|v| !v.is_empty());
        Some(Self { by, index, data })
    }
    fn get(&self, pv: usize, poly: usize, cp: usize, w: usize) -> Option<&[f64]> {
        let key = match self.by {
            0 => pv,
            1 => poly,
            2 => cp,
            _ => 0,
        };
        let at = match &self.index {
            Some(ix) => *ix.get(key)? as usize,
            None => key,
        };
        self.data.get(at * w..at * w + w)
    }
}

pub fn load(bytes: &[u8]) -> Result<Scene, String> {
    let doc = root(bytes)?;
    let objects = doc.kid("Objects").ok_or("no objects")?;
    let mut by_id: HashMap<i64, &FNode> = HashMap::new();
    for o in &objects.kids {
        if let Some(id) = o.attrs.first().and_then(|a| a.int()) {
            by_id.insert(id, o);
        }
    }
    // (child, parent, property)
    let mut links: Vec<(i64, i64, Option<String>)> = Vec::new();
    if let Some(c) = doc.kid("Connections") {
        for l in c.kids("C") {
            let kind = l.attrs.first().and_then(|a| a.str()).unwrap_or_default();
            let (Some(a), Some(b)) = (l.attrs.get(1).and_then(|a| a.int()), l.attrs.get(2).and_then(|a| a.int())) else { continue };
            let prop = (kind == "OP").then(|| l.attrs.get(3).and_then(|a| a.str()).map(String::from)).flatten();
            links.push((a, b, prop));
        }
    }
    let class = |id: i64| by_id.get(&id).map(|n| n.name.as_str());
    let sub = |id: i64| by_id.get(&id).and_then(|n| n.attrs.get(2)).and_then(|a| a.str()).unwrap_or_default().to_string();
    let children_of = |parent: i64, cls: &str| -> Vec<i64> { links.iter().filter(|(c, p, _)| *p == parent && class(*c) == Some(cls)).map(|(c, _, _)| *c).collect() };

    let mut scene = Scene::default();
    let model_ids: Vec<i64> = objects.kids("Model").filter_map(|m| m.attrs.first().and_then(|a| a.int())).collect();
    let index_of: HashMap<i64, usize> = model_ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    struct Rot {
        pre: Quat,
        post_inv: Quat,
        order: &'static str,
    }
    let mut rots = Vec::new();
    for &id in &model_ids {
        let p = props(by_id[&id]);
        let t = v3(&p, "Lcl Translation").unwrap_or(Vec3::ZERO);
        let r = v3(&p, "Lcl Rotation").unwrap_or(Vec3::ZERO);
        let s = v3(&p, "Lcl Scaling").unwrap_or(Vec3::ONE);
        let ord = order(num(&p, "RotationOrder").unwrap_or(0.) as i64);
        let pre = v3(&p, "PreRotation").map_or(Quat::IDENTITY, |v| euler(v, "ZYX"));
        let post_inv = v3(&p, "PostRotation").map_or(Quat::IDENTITY, |v| euler(v, "ZYX").inverse());
        let tr = |k: &str| Mat4::from_translation(v3(&p, k).unwrap_or(Vec3::ZERO));
        let rp = tr("RotationPivot");
        let sp = tr("ScalingPivot");
        let m = Mat4::from_translation(t)
            * tr("RotationOffset")
            * rp
            * Mat4::from_quat(pre * euler(r, ord) * post_inv)
            * rp.inverse()
            * tr("ScalingOffset")
            * sp
            * Mat4::from_scale(s)
            * sp.inverse();
        let (s, r, t) = m.to_scale_rotation_translation();
        let parent = links.iter().find(|(c, p, _)| *c == id && index_of.contains_key(p)).map(|(_, p, _)| index_of[p]);
        scene.nodes.push(Node { parent, t, r, s, ..Node::default() });
        rots.push(Rot { pre, post_inv, order: ord });
    }

    // Images: each texture's embedded video content, decoded once.
    let mut image_of: HashMap<i64, Option<usize>> = HashMap::new();
    let mut texture = |tex_id: i64, scene: &mut Scene| -> Option<TexRef> {
        let slot = image_of.entry(tex_id).or_insert_with(|| {
            let video = links.iter().find(|(c, p, _)| *p == tex_id && class(*c) == Some("Video")).map(|(c, _, _)| *c)?;
            let content = by_id.get(&video)?.kid("Content")?.attrs.first()?;
            let bytes = match content {
                Val::Bin(b) => b.clone(),
                Val::Str(s) => base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s.trim()).ok()?,
                _ => return None,
            };
            let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
            scene.images.push(Image { w: img.width(), h: img.height(), rgba: img.into_raw() });
            Some(scene.images.len() - 1)
        });
        let image = (*slot)?;
        Some(TexRef { image, wrap: [Wrap::Repeat; 2], nearest: false })
    };

    let mut material_of: HashMap<i64, usize> = HashMap::new();
    let mut default_material: Option<usize> = None;
    let mut material = |mid: i64, scene: &mut Scene| -> usize {
        if let Some(&i) = material_of.get(&mid) {
            return i;
        }
        let node = by_id[&mid];
        let p = props(node);
        let shading = node.text("ShadingModel").unwrap_or("phong").to_ascii_lowercase();
        let lin = |c: Vec3| [srgb_to_linear(c.x), srgb_to_linear(c.y), srgb_to_linear(c.z)];
        let color = v3(&p, "DiffuseColor").or_else(|| v3(&p, "Diffuse")).unwrap_or(Vec3::ONE);
        let emissive = v3(&p, "EmissiveColor").or_else(|| v3(&p, "Emissive")).unwrap_or(Vec3::ZERO);
        let ef = num(&p, "EmissiveFactor").unwrap_or(1.) as f32;
        let opacity = num(&p, "Opacity").unwrap_or(1.) as f32;
        let shininess = num(&p, "Shininess").or_else(|| num(&p, "ShininessExponent")).unwrap_or(30.) as f32;
        let roughness = if shading == "lambert" { 0.8 } else { (1. - (shininess / 100.).max(0.).sqrt()).clamp(0.15, 1.) };
        let e = lin(emissive);
        let c = lin(color);
        let mut m = Material {
            base: [c[0], c[1], c[2], opacity],
            metallic: 0.,
            roughness,
            emissive: [e[0] * ef, e[1] * ef, e[2] * ef],
            alpha: if opacity < 1. { Alpha::Blend } else { Alpha::Opaque },
            normal_scale: [1., -1.],
            ..Material::default()
        };
        for (c, _, prop) in links.iter().filter(|(c, p, _)| *p == mid && class(*c) == Some("Texture")) {
            let Some(prop) = prop.as_deref() else { continue };
            let t = texture(*c, scene);
            match prop {
                "DiffuseColor" | "Diffuse" => m.base_tex = t,
                "NormalMap" => m.normal_tex = t,
                "EmissiveColor" | "Emissive" => m.emissive_tex = t,
                _ => {}
            }
        }
        if m.emissive_tex.is_some() && e == [0.; 3] {
            m.emissive = [ef; 3];
        }
        scene.materials.push(m);
        let i = scene.materials.len() - 1;
        material_of.insert(mid, i);
        i
    };

    for g in objects.kids("Geometry") {
        let Some(gid) = g.attrs.first().and_then(|a| a.int()) else { continue };
        if g.attrs.get(2).and_then(|a| a.str()) != Some("Mesh") {
            continue;
        }
        let Some(&mi) = links.iter().find(|(c, p, _)| *c == gid && index_of.contains_key(p)).map(|(_, p, _)| &index_of[p]) else { continue };
        let model_id = model_ids[mi];
        let mp = props(by_id[&model_id]);
        let geo = Mat4::from_scale_rotation_translation(
            v3(&mp, "GeometricScaling").unwrap_or(Vec3::ONE),
            euler(v3(&mp, "GeometricRotation").unwrap_or(Vec3::ZERO), "ZYX"),
            v3(&mp, "GeometricTranslation").unwrap_or(Vec3::ZERO),
        );
        let nmat = glam::Mat3::from_mat4(geo).inverse().transpose();
        let verts = g.arr_f("Vertices");
        let cps: Vec<Vec3> = verts.chunks_exact(3).map(|c| geo.transform_point3(Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32))).collect();
        let pvi = g.arr_i("PolygonVertexIndex");
        let normals = Layer::read(g, "LayerElementNormal", "Normals", "NormalsIndex");
        let uvs = Layer::read(g, "LayerElementUV", "UV", "UVIndex");
        let colors = Layer::read(g, "LayerElementColor", "Colors", "ColorIndex");
        let mats = g.kid("LayerElementMaterial").map(|l| (l.text("MappingInformationType").unwrap_or("AllSame") == "ByPolygon", l.arr_i("Materials")));
        let model_mats: Vec<i64> = children_of(model_id, "Material");
        let slots: Vec<usize> = model_mats.iter().map(|&m| material(m, &mut scene)).collect();

        // Skin: per control point, its four heaviest bones.
        let mut skin_w: Vec<Vec<(u16, f32)>> = Vec::new();
        let mut skin_idx = None;
        if let Some(skin) = links.iter().find(|(c, p, _)| *p == gid && class(*c) == Some("Deformer") && sub(*c) == "Skin").map(|(c, _, _)| *c) {
            let mut joints = Vec::new();
            let mut ibm = Vec::new();
            skin_w = vec![Vec::new(); cps.len()];
            for cl in children_of(skin, "Deformer") {
                let node = by_id[&cl];
                let Some(bone) = links.iter().find(|(_, p, _)| *p == cl).and_then(|(c, _, _)| index_of.get(c)).copied() else { continue };
                let m16 = |k: &str| {
                    let v = node.arr_f(k);
                    (v.len() == 16).then(|| Mat4::from_cols_array(&std::array::from_fn(|i| v[i] as f32)))
                };
                // `Transform` already holds the inverse bind: the link's inverse times the mesh.
                let transform = m16("Transform").or_else(|| m16("TransformLink").map(|l| l.inverse())).unwrap_or(Mat4::IDENTITY);
                let j = joints.len() as u16;
                joints.push(bone);
                ibm.push(transform);
                let (ix, ws) = (node.arr_i("Indexes"), node.arr_f("Weights"));
                for (k, &cp) in ix.iter().enumerate() {
                    if let (Some(slot), Some(&w)) = (skin_w.get_mut(cp as usize), ws.get(k)) {
                        slot.push((j, w as f32));
                    }
                }
            }
            if !joints.is_empty() {
                scene.skins.push(Skin { joints, inverse_bind: ibm });
                skin_idx = Some(scene.skins.len() - 1);
            }
        }

        let mut buckets: HashMap<usize, Prim> = HashMap::new();
        let mut poly: Vec<(usize, usize)> = Vec::new();
        let mut poly_i = 0;
        for (pv, &raw) in pvi.iter().enumerate() {
            let cp = if raw < 0 { (raw ^ -1) as usize } else { raw as usize };
            poly.push((pv, cp));
            if raw >= 0 {
                continue;
            }
            let slot = match &mats {
                Some((true, m)) => m.get(poly_i).copied().unwrap_or(0) as usize,
                Some((false, m)) => m.first().copied().unwrap_or(0) as usize,
                None => 0,
            };
            let mat = match slots.get(slot).or(slots.first()) {
                Some(&m) => m,
                None => *default_material.get_or_insert_with(|| {
                    let lin = srgb_to_linear(0xcc as f32 / 255.);
                    scene.materials.push(Material { base: [lin, lin, lin, 1.], metallic: 0., roughness: 1. - 0.3f32.sqrt(), normal_scale: [1., -1.], ..Material::default() });
                    scene.materials.len() - 1
                }),
            };
            let prim = buckets.entry(mat).or_insert_with(|| Prim { material: Some(mat), ..Default::default() });
            let push = |prim: &mut Prim, &(pv, cp): &(usize, usize)| {
                let Some(&p) = cps.get(cp) else { return };
                prim.pos.push(p.to_array());
                if let Some(n) = normals.as_ref().and_then(|l| l.get(pv, poly_i, cp, 3)) {
                    prim.nrm.push((nmat * Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32)).normalize_or_zero().to_array());
                }
                if let Some(t) = uvs.as_ref().and_then(|l| l.get(pv, poly_i, cp, 2)) {
                    prim.uv.push([t[0] as f32, 1. - t[1] as f32]);
                }
                if let Some(c) = colors.as_ref().and_then(|l| l.get(pv, poly_i, cp, 4)) {
                    prim.color.get_or_insert_with(Vec::new).push([c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]);
                }
                if skin_idx.is_some() {
                    let mut ws = skin_w.get(cp).cloned().unwrap_or_default();
                    ws.sort_by(|a, b| b.1.total_cmp(&a.1));
                    ws.truncate(4);
                    let sum: f32 = ws.iter().map(|w| w.1).sum();
                    let mut j = [0u16; 4];
                    let mut w = [0f32; 4];
                    for (k, (jj, ww)) in ws.iter().enumerate() {
                        j[k] = *jj;
                        w[k] = if sum > 0. { ww / sum } else { 0. };
                    }
                    prim.joints.get_or_insert_with(Vec::new).push(j);
                    prim.weights.get_or_insert_with(Vec::new).push(w);
                }
            };
            for k in 1..poly.len().saturating_sub(1) {
                push(prim, &poly[0]);
                push(prim, &poly[k]);
                push(prim, &poly[k + 1]);
            }
            poly.clear();
            poly_i += 1;
        }
        let mut prims: Vec<Prim> = buckets.into_values().collect();
        prims.sort_by_key(|p| p.material);
        for p in &mut prims {
            if p.nrm.len() != p.pos.len() {
                p.nrm.clear();
            }
            p.finish();
        }
        scene.meshes.push(Mesh { prims, ..Default::default() });
        let mesh = scene.meshes.len() - 1;
        if skin_idx.is_some() || scene.nodes[mi].mesh.is_none() {
            scene.nodes[mi].mesh = Some(mesh);
            scene.nodes[mi].skin = skin_idx;
        } else {
            scene.nodes.push(Node { parent: Some(mi), mesh: Some(mesh), ..Node::default() });
        }
    }

    // Animation: stack → layers → curve nodes → curves.
    for stack in objects.kids("AnimationStack") {
        let Some(sid) = stack.attrs.first().and_then(|a| a.int()) else { continue };
        let mut channels = Vec::new();
        for layer in children_of(sid, "AnimationLayer") {
            for cn in children_of(layer, "AnimationCurveNode") {
                let Some((_, target, Some(prop))) = links.iter().find(|(c, p, prop)| *c == cn && index_of.contains_key(p) && prop.is_some()) else { continue };
                let ni = index_of[target];
                let path = match prop.as_str() {
                    "Lcl Translation" => Path::T,
                    "Lcl Rotation" => Path::R,
                    "Lcl Scaling" => Path::S,
                    _ => continue,
                };
                let defaults = props(by_id[&cn]);
                let mut axes: [Option<(Vec<f64>, Vec<f64>)>; 3] = [None, None, None];
                for (c, _, ax) in links.iter().filter(|(c, p, _)| *p == cn && class(*c) == Some("AnimationCurve")) {
                    let k = match ax.as_deref() {
                        Some("d|X") => 0,
                        Some("d|Y") => 1,
                        Some("d|Z") => 2,
                        _ => continue,
                    };
                    let node = by_id[c];
                    let times: Vec<f64> = node.arr_i("KeyTime").iter().map(|&t| t as f64 / TICKS_PER_SECOND).collect();
                    let values = node.arr_f("KeyValueFloat");
                    if !times.is_empty() && times.len() == values.len() {
                        axes[k] = Some((times, values));
                    }
                }
                if axes.iter().all(|a| a.is_none()) {
                    continue;
                }
                let mut times: Vec<f64> = axes.iter().flatten().flat_map(|(t, _)| t.iter().copied()).collect();
                times.sort_by(f64::total_cmp);
                times.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
                let mp = props(by_id[&model_ids[ni]]);
                let rest = match path {
                    Path::T => v3(&mp, "Lcl Translation").unwrap_or(Vec3::ZERO),
                    Path::R => v3(&mp, "Lcl Rotation").unwrap_or(Vec3::ZERO),
                    Path::S => v3(&mp, "Lcl Scaling").unwrap_or(Vec3::ONE),
                    Path::W => continue,
                };
                let default = |k: usize| num(&defaults, ["d|X", "d|Y", "d|Z"][k]).map_or(rest[k], |v| v as f32);
                let at = |k: usize, t: f64| -> f32 {
                    let Some((ts, vs)) = &axes[k] else { return default(k) };
                    let i = ts.partition_point(|&x| x <= t);
                    if i == 0 {
                        return vs[0] as f32;
                    }
                    if i >= ts.len() {
                        return vs[ts.len() - 1] as f32;
                    }
                    let (t0, t1) = (ts[i - 1], ts[i]);
                    let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0. };
                    (vs[i - 1] + (vs[i] - vs[i - 1]) * f) as f32
                };
                let mut values = Vec::with_capacity(times.len() * 4);
                for &t in &times {
                    let v = Vec3::new(at(0, t), at(1, t), at(2, t));
                    match path {
                        Path::R => {
                            let r = &rots[ni];
                            values.extend_from_slice(&(r.pre * euler(v, r.order) * r.post_inv).to_array());
                        }
                        _ => values.extend_from_slice(&v.to_array()),
                    }
                }
                channels.push(Channel { node: ni, path, interp: Interp::Linear, times: times.iter().map(|&t| t as f32).collect(), values });
            }
        }
        let duration = super::anim::duration(&channels);
        scene.clips.push(Clip { name: object_name(stack), duration, channels });
    }
    if scene.meshes.iter().all(|m| m.prims.is_empty()) {
        return Err("no geometry".into());
    }
    Ok(scene)
}
