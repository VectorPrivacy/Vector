//! glTF 2.0, binary or JSON. Sealed: only the GLB chunk and `data:` URIs are read, never a
//! sibling file or a remote texture.

use base64::Engine as _;
use glam::{Mat4, Quat, Vec3};
use gltf::mesh::Mode;

use super::scene::*;

const COMPRESSED: [&str; 3] = ["KHR_draco_mesh_compression", "EXT_meshopt_compression", "KHR_texture_basisu"];

fn data_uri(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let (head, body) = rest.split_once(',')?;
    if head.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD.decode(body.trim()).ok()
    } else {
        Some(body.as_bytes().to_vec())
    }
}

fn wrap(w: gltf::texture::WrappingMode) -> Wrap {
    match w {
        gltf::texture::WrappingMode::Repeat => Wrap::Repeat,
        gltf::texture::WrappingMode::MirroredRepeat => Wrap::Mirror,
        gltf::texture::WrappingMode::ClampToEdge => Wrap::Clamp,
    }
}

pub fn load(bytes: &[u8]) -> Result<Scene, String> {
    for ext in COMPRESSED {
        let needle = ext.as_bytes();
        if bytes.windows(needle.len()).any(|w| w == needle) {
            // Only a required one refuses; an optional one leaves a plain fallback to read.
            if let Ok(g) = gltf::Gltf::from_slice_without_validation(bytes)
                && g.extensions_required().any(|r| r == ext)
            {
                return Err(format!("unsupported: {ext}"));
            }
        }
    }
    let g = gltf::Gltf::from_slice_without_validation(bytes).map_err(|e| e.to_string())?;
    let blob = g.blob.clone();
    let buffers: Vec<Option<Vec<u8>>> = g
        .buffers()
        .map(|b| match b.source() {
            gltf::buffer::Source::Bin => blob.clone(),
            gltf::buffer::Source::Uri(u) => data_uri(u),
        })
        .collect();
    let get = |b: gltf::Buffer| buffers.get(b.index()).and_then(|d| d.as_deref());

    let mut scene = Scene::default();
    for img in g.images() {
        let data: Option<Vec<u8>> = match img.source() {
            gltf::image::Source::View { view, .. } => get(view.buffer()).and_then(|d| d.get(view.offset()..view.offset() + view.length())).map(|s| s.to_vec()),
            gltf::image::Source::Uri { uri, .. } => data_uri(uri),
        };
        let decoded = data.and_then(|d| image::load_from_memory(&d).ok()).map(|i| i.to_rgba8());
        scene.images.push(match decoded {
            Some(i) => Image { w: i.width(), h: i.height(), rgba: i.into_raw() },
            None => Image { w: 1, h: 1, rgba: vec![255; 4] },
        });
    }
    let tex = |t: gltf::Texture| -> Option<TexRef> {
        let image = t.source().index();
        let s = t.sampler();
        Some(TexRef {
            image,
            wrap: [wrap(s.wrap_s()), wrap(s.wrap_t())],
            nearest: matches!(s.mag_filter(), Some(gltf::texture::MagFilter::Nearest)),
        })
    };
    for m in g.materials() {
        let pbr = m.pbr_metallic_roughness();
        let base_info = pbr.base_color_texture();
        let uv_transform = base_info
            .as_ref()
            .and_then(|i| i.texture_transform())
            .map(|t| [t.offset()[0], t.offset()[1], t.rotation(), t.scale()[0], t.scale()[1]])
            .unwrap_or([0., 0., 0., 1., 1.]);
        let strength = m.emissive_strength().unwrap_or(1.);
        let e = m.emissive_factor();
        let normal = m.normal_texture();
        let occ = m.occlusion_texture();
        let ns = normal.as_ref().map_or(1., |n| n.scale());
        scene.materials.push(Material {
            base: pbr.base_color_factor(),
            base_tex: base_info.and_then(|i| tex(i.texture())),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            mr_tex: pbr.metallic_roughness_texture().and_then(|i| tex(i.texture())),
            // Derivative tangents read green the other way up.
            normal_scale: [ns, -ns],
            normal_tex: normal.and_then(|n| tex(n.texture())),
            occlusion: occ.as_ref().map_or(1., |o| o.strength()),
            occlusion_tex: occ.and_then(|o| tex(o.texture())),
            emissive: [e[0] * strength, e[1] * strength, e[2] * strength],
            emissive_tex: m.emissive_texture().and_then(|i| tex(i.texture())),
            alpha: match m.alpha_mode() {
                gltf::material::AlphaMode::Opaque => Alpha::Opaque,
                gltf::material::AlphaMode::Mask => Alpha::Mask(m.alpha_cutoff().unwrap_or(0.5)),
                gltf::material::AlphaMode::Blend => Alpha::Blend,
            },
            double_sided: m.double_sided(),
            unlit: m.unlit(),
            vertex_colors: true,
            uv_transform,
        });
    }
    for mesh in g.meshes() {
        let mut prims = Vec::new();
        for p in mesh.primitives() {
            let r = p.reader(get);
            let Some(pos) = r.read_positions() else { continue };
            let mut prim = Prim {
                pos: pos.collect(),
                nrm: r.read_normals().map(|n| n.collect()).unwrap_or_default(),
                uv: r.read_tex_coords(0).map(|t| t.into_f32().collect()).unwrap_or_default(),
                color: r.read_colors(0).map(|c| c.into_rgba_f32().collect()),
                joints: r.read_joints(0).map(|j| j.into_u16().collect()),
                weights: r.read_weights(0).map(|w| w.into_f32().collect()),
                idx: Vec::new(),
                material: p.material().index(),
            };
            let idx: Vec<u32> = r.read_indices().map(|i| i.into_u32().collect()).unwrap_or_else(|| (0..prim.pos.len() as u32).collect());
            prim.idx = match p.mode() {
                Mode::Triangles => idx,
                Mode::TriangleStrip => (2..idx.len()).flat_map(|i| if i % 2 == 0 { [idx[i - 2], idx[i - 1], idx[i]] } else { [idx[i - 1], idx[i - 2], idx[i]] }).collect(),
                Mode::TriangleFan => (2..idx.len()).flat_map(|i| [idx[0], idx[i - 1], idx[i]]).collect(),
                _ => continue,
            };
            prim.finish();
            prims.push(prim);
        }
        scene.meshes.push(Mesh { prims });
    }
    for skin in g.skins() {
        let joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
        let ibm: Vec<Mat4> = skin.reader(get).read_inverse_bind_matrices().map(|m| m.map(|c| Mat4::from_cols_array_2d(&c)).collect()).unwrap_or_default();
        let inverse_bind = (0..joints.len()).map(|i| ibm.get(i).copied().unwrap_or(Mat4::IDENTITY)).collect();
        scene.skins.push(Skin { joints, inverse_bind });
    }
    let doc_scene = g.default_scene().or_else(|| g.scenes().next());
    let mut shown = vec![false; g.nodes().count()];
    fn mark(n: gltf::Node, shown: &mut [bool]) {
        shown[n.index()] = true;
        for c in n.children() {
            if !shown[c.index()] {
                mark(c, shown);
            }
        }
    }
    match &doc_scene {
        Some(s) => s.nodes().for_each(|n| mark(n, &mut shown)),
        None => shown.iter_mut().for_each(|s| *s = true),
    }
    scene.nodes = g
        .nodes()
        .map(|n| {
            let (t, r, s) = n.transform().decomposed();
            Node {
                parent: None,
                t: Vec3::from(t),
                r: Quat::from_array(r).normalize(),
                s: Vec3::from(s),
                mesh: n.mesh().map(|m| m.index()).filter(|_| shown[n.index()]),
                skin: n.skin().map(|s| s.index()),
            }
        })
        .collect();
    for n in g.nodes() {
        for c in n.children() {
            scene.nodes[c.index()].parent = Some(n.index());
        }
    }
    for (i, a) in g.animations().enumerate() {
        let mut channels = Vec::new();
        for ch in a.channels() {
            let r = ch.reader(get);
            let Some(times) = r.read_inputs() else { continue };
            let times: Vec<f32> = times.collect();
            let path = match ch.target().property() {
                gltf::animation::Property::Translation => Path::T,
                gltf::animation::Property::Rotation => Path::R,
                gltf::animation::Property::Scale => Path::S,
                gltf::animation::Property::MorphTargetWeights => continue,
            };
            let values: Vec<f32> = match r.read_outputs() {
                Some(gltf::animation::util::ReadOutputs::Translations(v)) => v.flatten().collect(),
                Some(gltf::animation::util::ReadOutputs::Scales(v)) => v.flatten().collect(),
                Some(gltf::animation::util::ReadOutputs::Rotations(v)) => v.into_f32().flatten().collect(),
                _ => continue,
            };
            channels.push(Channel {
                node: ch.target().node().index(),
                path,
                interp: match ch.sampler().interpolation() {
                    gltf::animation::Interpolation::Step => Interp::Step,
                    gltf::animation::Interpolation::Linear => Interp::Linear,
                    gltf::animation::Interpolation::CubicSpline => Interp::Cubic,
                },
                times,
                values,
            });
        }
        let duration = super::anim::duration(&channels);
        scene.clips.push(Clip { name: a.name().map(String::from).unwrap_or_else(|| format!("animation_{i}")), duration, channels });
    }
    Ok(scene)
}
