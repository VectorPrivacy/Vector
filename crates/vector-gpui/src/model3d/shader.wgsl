// three.js's MeshStandardMaterial under an environment map alone: metal-rough PBR with
// multiscatter, derivative tangents, ACES Filmic and sRGB out.

struct Frame {
    view_proj: mat4x4<f32>,
    cam: vec4<f32>,
    env: vec4<f32>,
};

struct Mat {
    base: vec4<f32>,
    emissive: vec4<f32>,
    // metallic, roughness, occlusion strength, alpha cutoff (negative: none)
    pbr: vec4<f32>,
    // normal scale xy, flags, opaque
    misc: vec4<f32>,
    // uv offset xy, rotation, unused
    uv0: vec4<f32>,
    // uv scale xy
    uv1: vec4<f32>,
};

struct Object {
    model: mat4x4<f32>,
    normal: mat4x4<f32>,
    // skinned, joint offset
    skin: vec4<u32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var env_tex: texture_2d<f32>;
@group(0) @binding(2) var env_smp: sampler;

@group(1) @binding(0) var<uniform> mat: Mat;
@group(1) @binding(1) var t_base: texture_2d<f32>;
@group(1) @binding(2) var t_mr: texture_2d<f32>;
@group(1) @binding(3) var t_normal: texture_2d<f32>;
@group(1) @binding(4) var t_occ: texture_2d<f32>;
@group(1) @binding(5) var t_emit: texture_2d<f32>;
@group(1) @binding(6) var smp: sampler;

@group(2) @binding(0) var<uniform> obj: Object;
@group(2) @binding(1) var<storage, read> joints: array<mat4x4<f32>>;

struct In {
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) joint: vec4<u32>,
    @location(5) weight: vec4<f32>,
};

struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
};

@vertex
fn vs(v: In) -> Out {
    var m = obj.model;
    var nm = obj.normal;
    if (obj.skin.x != 0u) {
        let o = obj.skin.y;
        let s = joints[o + v.joint.x] * v.weight.x + joints[o + v.joint.y] * v.weight.y
              + joints[o + v.joint.z] * v.weight.z + joints[o + v.joint.w] * v.weight.w;
        m = obj.model * s;
        nm = m;
    }
    let w = m * vec4<f32>(v.pos, 1.0);
    var out: Out;
    out.clip = frame.view_proj * w;
    out.world = w.xyz;
    out.nrm = (nm * vec4<f32>(v.nrm, 0.0)).xyz;
    let c = cos(mat.uv0.z);
    let s = sin(mat.uv0.z);
    let uv = v.uv * mat.uv1.xy;
    out.uv = vec2<f32>(c * uv.x + s * uv.y, -s * uv.x + c * uv.y) + mat.uv0.xy;
    out.color = v.color;
    return out;
}

const PI: f32 = 3.141592653589793;

fn env_uv(d: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(atan2(d.z, d.x) / (2.0 * PI) + 0.5, acos(clamp(d.y, -1.0, 1.0)) / PI);
}

fn env(d: vec3<f32>, roughness: f32) -> vec3<f32> {
    return textureSampleLevel(env_tex, env_smp, env_uv(normalize(d)), roughness * frame.env.x).rgb;
}

fn rrt_odt(v: vec3<f32>) -> vec3<f32> {
    let a = v * (v + 0.0245786) - 0.000090537;
    let b = v * (0.983729 * v + 0.4329510) + 0.238081;
    return a / b;
}

fn aces(color_in: vec3<f32>) -> vec3<f32> {
    let input = mat3x3<f32>(
        vec3<f32>(0.59719, 0.07600, 0.02840),
        vec3<f32>(0.35458, 0.90834, 0.13383),
        vec3<f32>(0.04823, 0.01566, 0.83777),
    );
    let output = mat3x3<f32>(
        vec3<f32>(1.60475, -0.10208, -0.00327),
        vec3<f32>(-0.53108, 1.10813, -0.07276),
        vec3<f32>(-0.07367, -0.00605, 1.07602),
    );
    var c = color_in / 0.6;
    c = input * c;
    c = rrt_odt(c);
    c = output * c;
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = pow(c, vec3<f32>(0.41666)) * 1.055 - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn dfg(nv: f32, roughness: f32) -> vec2<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * nv)) * r.x + r.y;
    return vec2<f32>(-1.04, 1.04) * a004 + r.zw;
}

@fragment
fn fs(i: Out, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let base_s = textureSample(t_base, smp, i.uv);
    let mr_s = textureSample(t_mr, smp, i.uv);
    let n_s = textureSample(t_normal, smp, i.uv);
    let occ_s = textureSample(t_occ, smp, i.uv);
    let emit_s = textureSample(t_emit, smp, i.uv);
    let q0 = dpdx(i.world);
    let q1 = -dpdy(i.world);
    let st0 = dpdx(i.uv);
    let st1 = -dpdy(i.uv);
    let hx = textureSample(t_normal, smp, i.uv + dpdx(i.uv)).x;
    let hy = textureSample(t_normal, smp, i.uv + dpdy(i.uv)).x;
    let flags = u32(mat.misc.z);
    let face = select(-1.0, 1.0, front);
    var geo = normalize(i.nrm) * face;
    let dxy = max(abs(dpdx(geo)), abs(dpdy(geo)));
    let geo_rough = max(max(dxy.x, dxy.y), dxy.z);

    var base = mat.base * base_s;
    if ((flags & 32u) != 0u) {
        base = base * i.color;
    }
    if (mat.pbr.w >= 0.0 && base.a < mat.pbr.w) {
        discard;
    }
    if (mat.misc.w > 0.5) {
        base.a = 1.0;
    }
    if ((flags & 64u) != 0u) {
        return vec4<f32>(srgb(aces(base.rgb)), base.a);
    }

    var n = geo;
    if ((flags & 4u) != 0u) {
        let map_n = vec3<f32>((n_s.xy * 2.0 - 1.0) * mat.misc.xy, n_s.z * 2.0 - 1.0);
        let q1perp = cross(q1, geo);
        let q0perp = cross(geo, q0);
        let t = q1perp * st0.x + q0perp * st1.x;
        let b = q1perp * st0.y + q0perp * st1.y;
        let det = max(dot(t, t), dot(b, b));
        let scale = select(face * inverseSqrt(det), 0.0, det == 0.0);
        n = normalize(t * (map_n.x * scale) + b * (map_n.y * scale) + geo * map_n.z);
    } else if ((flags & 128u) != 0u) {
        let dh = vec2<f32>(hx - n_s.x, hy - n_s.x) * mat.misc.x;
        let sx = normalize(dpdx(i.world));
        let sy = normalize(dpdy(i.world));
        let r1 = cross(sy, geo);
        let r2 = cross(geo, sx);
        let det = dot(sx, r1) * face;
        n = normalize(abs(det) * geo - sign(det) * (dh.x * r1 + dh.y * r2));
    }

    let metal = clamp(mat.pbr.x * mr_s.b, 0.0, 1.0);
    let rough = min(max(mat.pbr.y * mr_s.g, 0.0525) + geo_rough, 1.0);
    let ao = (occ_s.r - 1.0) * mat.pbr.z + 1.0;

    let v = normalize(frame.cam.xyz - i.world);
    let nv = clamp(dot(n, v), 0.0, 1.0);
    let diffuse_c = base.rgb * (1.0 - metal);
    let spec_c = mix(vec3<f32>(0.04), base.rgb, metal);

    var r = reflect(-v, n);
    r = normalize(mix(r, n, rough * rough));
    let radiance = env(r, rough);
    let irradiance = env(n, 1.0);

    let fab = dfg(nv, rough);
    let fss_ess = spec_c * fab.x + vec3<f32>(fab.y);
    let ess = fab.x + fab.y;
    let ems = 1.0 - ess;
    let favg = spec_c + (1.0 - spec_c) * 0.047619;
    let fms = fss_ess * favg / (1.0 - ems * favg);
    let multi = fms * ems;
    let total = fss_ess + multi;
    let diffuse = diffuse_c * (1.0 - max(max(total.r, total.g), total.b));

    var light = diffuse * irradiance * ao;
    let spec_ao = clamp(pow(nv + ao, exp2(-16.0 * rough - 1.0)) - 1.0 + ao, 0.0, 1.0);
    light = light + (radiance * fss_ess + multi * irradiance) * spec_ao;
    light = light + mat.emissive.rgb * emit_s.rgb;
    return vec4<f32>(srgb(aces(light)), base.a);
}
