//! STL and OBJ: geometry only, under the viewer's own grey. A model is one file, so an OBJ's
//! `.mtl` is never looked for.

use super::scene::*;

fn single(prim: Prim, material: Material) -> Scene {
    let mut s = Scene::default();
    s.materials.push(material);
    s.meshes.push(Mesh { prims: vec![Prim { material: Some(0), ..prim }] });
    s.nodes.push(Node { mesh: Some(0), ..Node::default() });
    s
}

pub fn stl(bytes: &[u8]) -> Result<Scene, String> {
    let binary = bytes.len() >= 84 && {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        84 + n * 50 == bytes.len() || !bytes.iter().take(5).copied().eq(*b"solid")
    };
    let mut prim = Prim::default();
    if binary {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if 84 + n * 50 > bytes.len() {
            return Err("truncated STL".into());
        }
        // Magics' "COLOR=" header gives a default; a face's attribute word may carry its own.
        let header = &bytes[..80];
        let default = header.windows(6).position(|w| w == b"COLOR=").filter(|&p| p + 10 <= 80).map(|p| [header[p + 6], header[p + 7], header[p + 8], header[p + 9]]);
        let lin = |v: f32| srgb_to_linear(v);
        let mut colors = default.map(|_| Vec::with_capacity(n * 3));
        for f in 0..n {
            let o = 84 + f * 50;
            let r = |k: usize| f32::from_le_bytes([bytes[o + k], bytes[o + k + 1], bytes[o + k + 2], bytes[o + k + 3]]);
            for v in 0..3 {
                let b = 12 + v * 12;
                prim.pos.push([r(b), r(b + 4), r(b + 8)]);
            }
            if let (Some(cols), Some(d)) = (colors.as_mut(), default) {
                let packed = u16::from_le_bytes([bytes[o + 48], bytes[o + 49]]);
                let c = if packed & 0x8000 == 0 {
                    [lin((packed & 31) as f32 / 31.), lin(((packed >> 5) & 31) as f32 / 31.), lin(((packed >> 10) & 31) as f32 / 31.), 1.]
                } else {
                    [lin(d[0] as f32 / 255.), lin(d[1] as f32 / 255.), lin(d[2] as f32 / 255.), 1.]
                };
                cols.extend([c, c, c]);
            }
        }
        prim.color = colors;
    } else {
        let text = String::from_utf8_lossy(bytes);
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if it.next() == Some("vertex") {
                let v: Vec<f32> = it.take(3).filter_map(|t| t.parse().ok()).collect();
                if v.len() == 3 {
                    prim.pos.push([v[0], v[1], v[2]]);
                }
            }
        }
    }
    if prim.pos.len() < 3 {
        return Err("no geometry".into());
    }
    prim.pos.truncate(prim.pos.len() / 3 * 3);
    prim.finish();
    let mut m = Material::plain();
    m.vertex_colors = prim.color.is_some();
    Ok(single(prim, m))
}

pub fn obj(bytes: &[u8]) -> Result<Scene, String> {
    let opts = tobj::LoadOptions { triangulate: true, single_index: true, ..Default::default() };
    let (models, _) = tobj::load_obj_buf(&mut std::io::Cursor::new(bytes), &opts, |_| Err(tobj::LoadError::OpenFileFailed)).map_err(|e| e.to_string())?;
    let mut s = Scene::default();
    s.materials.push(Material::plain());
    let mut prims = Vec::new();
    for m in models {
        let mesh = m.mesh;
        let n = mesh.positions.len() / 3;
        if n == 0 {
            continue;
        }
        let mut p = Prim {
            pos: mesh.positions.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
            nrm: mesh.normals.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
            uv: mesh.texcoords.chunks_exact(2).map(|c| [c[0], 1. - c[1]]).collect(),
            idx: mesh.indices,
            material: Some(0),
            ..Default::default()
        };
        p.finish();
        prims.push(p);
    }
    if prims.is_empty() {
        return Err("no geometry".into());
    }
    s.meshes.push(Mesh { prims });
    s.nodes.push(Node { mesh: Some(0), ..Node::default() });
    Ok(s)
}
