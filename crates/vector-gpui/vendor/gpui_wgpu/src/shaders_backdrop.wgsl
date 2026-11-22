// Backdrop blur (CSS `backdrop-filter: blur()`): what is drawn so far under a shape is copied
// out, shrunk to a quarter, blurred across then down, and drawn back under the shape. All rects
// are (x, y, width, height): `dst` in the target's pixels, `uv` in the source's 0..1.
// Self-contained, with a uniform block rather than storage, so GLES compiles it as is.

struct BackdropPass {
    dst: vec4<f32>,
    target_size: vec2<f32>,
    texel: vec2<f32>,
    uv: vec4<f32>,
    uv_clamp: vec4<f32>,
    dir: vec2<f32>,
    pad: vec2<f32>,
    weights0: vec4<f32>, // the centre tap, then taps 1..3 out each way
    weights1: vec4<f32>, // taps 4..6
    offsets0: vec4<f32>, // taps 1..4, in texels along `dir`
    offsets1: vec4<f32>, // taps 5..6
    shape: vec4<f32>,
    radii: vec4<f32>, // top left, top right, bottom right, bottom left
    clip: vec4<f32>,
    clip_radii: vec4<f32>,
}

@group(0) @binding(0) var<uniform> pass_: BackdropPass;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;

struct BackdropVarying {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) at: vec2<f32>,
}

// Four vertices as a triangle strip.
@vertex
fn vs_backdrop(@builtin(vertex_index) index: u32) -> BackdropVarying {
    let unit = vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));
    let at = pass_.dst.xy + unit * pass_.dst.zw;
    let ndc = at / pass_.target_size * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    var out: BackdropVarying;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = pass_.uv.xy + unit * pass_.uv.zw;
    out.at = at;
    return out;
}

fn backdrop_tap(uv: vec2<f32>) -> vec4<f32> {
    return textureSample(source, source_sampler, clamp(uv, pass_.uv_clamp.xy, pass_.uv_clamp.zw));
}

// Each tap lands between four source pixels, so four of them average a 4x4 block.
@fragment
fn fs_backdrop_down(input: BackdropVarying) -> @location(0) vec4<f32> {
    let d = pass_.texel;
    return (backdrop_tap(input.uv + vec2<f32>(-d.x, -d.y))
        + backdrop_tap(input.uv + vec2<f32>(d.x, -d.y))
        + backdrop_tap(input.uv + vec2<f32>(-d.x, d.y))
        + backdrop_tap(input.uv + vec2<f32>(d.x, d.y))) * 0.25;
}

fn backdrop_pair(uv: vec2<f32>, step: vec2<f32>, offset: f32, weight: f32) -> vec4<f32> {
    let d = step * offset;
    return (backdrop_tap(uv + d) + backdrop_tap(uv - d)) * weight;
}

@fragment
fn fs_backdrop_blur(input: BackdropVarying) -> @location(0) vec4<f32> {
    let step = pass_.dir * pass_.texel;
    var color = backdrop_tap(input.uv) * pass_.weights0.x;
    color += backdrop_pair(input.uv, step, pass_.offsets0.x, pass_.weights0.y);
    color += backdrop_pair(input.uv, step, pass_.offsets0.y, pass_.weights0.z);
    color += backdrop_pair(input.uv, step, pass_.offsets0.z, pass_.weights0.w);
    color += backdrop_pair(input.uv, step, pass_.offsets0.w, pass_.weights1.x);
    color += backdrop_pair(input.uv, step, pass_.offsets1.x, pass_.weights1.y);
    color += backdrop_pair(input.uv, step, pass_.offsets1.y, pass_.weights1.z);
    return color;
}

fn backdrop_rrect_sdf(p: vec2<f32>, b: vec4<f32>, r: vec4<f32>) -> f32 {
    let half_size = b.zw * 0.5;
    let to = p - (b.xy + half_size);
    var radius = r.y;
    if (to.x < 0.0) {
        radius = select(r.w, r.x, to.y < 0.0);
    } else {
        radius = select(r.z, r.y, to.y < 0.0);
    }
    let q = abs(to) - half_size + radius;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

// Premultiplied: the blur, opaque, covering the shape antialiased within its clip.
@fragment
fn fs_backdrop_composite(input: BackdropVarying) -> @location(0) vec4<f32> {
    let color = backdrop_tap(input.uv);
    let alpha = saturate(0.5 - backdrop_rrect_sdf(input.at, pass_.shape, pass_.radii))
        * saturate(0.5 - backdrop_rrect_sdf(input.at, pass_.clip, pass_.clip_radii));
    return vec4<f32>(color.rgb * alpha, alpha);
}
