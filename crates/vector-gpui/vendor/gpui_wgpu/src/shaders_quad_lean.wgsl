// The quads vertex stage for GL compilers that refuse `vs_quad`. Naga's GLSL keeps every function
// whose bindings an entry point uses, so `vs_quad` also carries `quad_color`, the whole quad
// fragment stage, as dead code. Reading through a binding of its own keeps that out, and the
// fields are read in place rather than copying the whole Quad.
// Appended after `shaders.wgsl` and `shaders_storage.wgsl`.

@group(1) @binding(0) var<storage, read> b_quads_lean: array<Quad>;

fn quad_stop_color(color: Hsla, color_space: u32) -> vec4<f32> {
    let linear = hsla_to_rgba(color);
    if (color_space == 0u) {
        return linear_to_srgba(linear);
    } else if (color_space == 1u) {
        return linear_srgb_to_oklab(linear);
    }
    return linear;
}

@vertex
fn vs_quad_lean(@builtin(vertex_index) vertex_id: u32, @builtin(instance_index) instance_id: u32) -> QuadVarying {
    let unit_vertex = vec2<f32>(f32(vertex_id & 1u), 0.5 * f32(vertex_id & 2u));
    let bounds = b_quads_lean[instance_id].bounds;
    let tag = b_quads_lean[instance_id].background.tag;

    var out = QuadVarying();
    out.position = to_device_position(unit_vertex, bounds);
    if (tag == 0u || tag == 2u || tag == 3u) {
        out.background_solid = hsla_to_rgba(b_quads_lean[instance_id].background.solid);
    } else if (tag == 1u) {
        let color_space = b_quads_lean[instance_id].background.color_space;
        out.background_color0 = quad_stop_color(b_quads_lean[instance_id].background.colors[0].color, color_space);
        out.background_color1 = quad_stop_color(b_quads_lean[instance_id].background.colors[1].color, color_space);
    }
    out.border_color = hsla_to_rgba(b_quads_lean[instance_id].border_color);
    out.quad_id = instance_id;
    out.clip_distances = distance_from_clip_rect(unit_vertex, bounds, b_quads_lean[instance_id].content_mask);
    out.solid_rect = quad_solid_rect(tag, bounds, b_quads_lean[instance_id].corner_radii,
        b_quads_lean[instance_id].border_widths, b_quads_lean[instance_id].content_mask,
        b_quads_lean[instance_id].content_radii, b_quads_lean[instance_id].content_fade);
    return out;
}

// `vs_quad_lean` without the solid area, for a compiler that refuses even that.
@vertex
fn vs_quad_bare(@builtin(vertex_index) vertex_id: u32, @builtin(instance_index) instance_id: u32) -> QuadVarying {
    let unit_vertex = vec2<f32>(f32(vertex_id & 1u), 0.5 * f32(vertex_id & 2u));
    let bounds = b_quads_lean[instance_id].bounds;
    let tag = b_quads_lean[instance_id].background.tag;

    var out = QuadVarying();
    out.position = to_device_position(unit_vertex, bounds);
    if (tag == 0u || tag == 2u || tag == 3u) {
        out.background_solid = hsla_to_rgba(b_quads_lean[instance_id].background.solid);
    } else if (tag == 1u) {
        let color_space = b_quads_lean[instance_id].background.color_space;
        out.background_color0 = quad_stop_color(b_quads_lean[instance_id].background.colors[0].color, color_space);
        out.background_color1 = quad_stop_color(b_quads_lean[instance_id].background.colors[1].color, color_space);
    }
    out.border_color = hsla_to_rgba(b_quads_lean[instance_id].border_color);
    out.quad_id = instance_id;
    out.clip_distances = distance_from_clip_rect(unit_vertex, bounds, b_quads_lean[instance_id].content_mask);
    out.solid_rect = vec4<f32>(1.0, 1.0, 0.0, 0.0);
    return out;
}

// Probes, compiled only once `vs_quad` is refused, each isolating one thing `vs_quad` does.

struct ProbeVarying {
    @builtin(position) position: vec4<f32>,
}

@group(1) @binding(0) var<storage, read> b_quads_probe: array<Quad>;

@vertex
fn vs_probe_copy(@builtin(instance_index) instance_id: u32) -> ProbeVarying {
    let quad = b_quads_probe[instance_id];
    return ProbeVarying(vec4<f32>(quad.bounds.origin, quad.content_fade.top, 1.0));
}

@vertex
fn vs_probe_gradient(@builtin(instance_index) instance_id: u32) -> ProbeVarying {
    let gradient = prepare_gradient_color(
        b_quads_probe[instance_id].background.tag,
        b_quads_probe[instance_id].background.color_space,
        b_quads_probe[instance_id].background.solid,
        b_quads_probe[instance_id].background.colors
    );
    return ProbeVarying(gradient.solid + gradient.color0 + gradient.color1);
}

// Binds what `vs_quad` binds, so its GLSL keeps `quad_color` as dead code too.
@vertex
fn vs_probe_dead(@builtin(instance_index) instance_id: u32) -> ProbeVarying {
    return ProbeVarying(to_device_position(vec2<f32>(0.0), b_quads[instance_id].bounds));
}

@fragment
fn fs_probe() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}

@fragment
fn fs_probe_mask(input: QuadVarying) -> @location(0) vec4<f32> {
    let quad = load_quad(input.quad_id);
    return vec4<f32>(mask_alpha(input.position.xy, quad.content_mask, quad.content_radii, quad.content_fade));
}

@fragment
fn fs_probe_color(input: QuadVarying) -> @location(0) vec4<f32> {
    return quad_color(input);
}
