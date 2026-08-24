// Block geometry. One pipeline draws the whole world: every face samples the
// same atlas, so material never changes and a chunk is one draw call.

struct Globals {
    view_projection: mat4x4<f32>,
    fog_color: vec4<f32>,
    // x: fog start, y: fog end, z and w unused.
    fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    // Biome colour for grass and leaves, which ship greyscale and are tinted at
    // render time. White for everything else.
    @location(2) tint: vec3<f32>,
    @location(3) light: f32,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec3<f32>,
    @location(2) light: f32,
    @location(3) view_distance: f32,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = globals.view_projection * vec4<f32>(in.position, 1.0);
    out.uv = in.uv;
    out.tint = in.tint;
    out.light = in.light;
    // Distance in clip space rather than world space: the vertex shader does
    // not know where the camera is, and w after projection is exactly the
    // distance along the view axis.
    out.view_distance = out.clip.w;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    var color = textureSample(atlas, atlas_sampler, in.uv);

    // Cut out fully transparent texels rather than blending them. Foliage and
    // glass would otherwise write depth over whatever is behind them.
    if (color.a < 0.1) {
        discard;
    }

    color = vec4<f32>(color.rgb * in.tint * in.light, color.a);

    let fog = clamp(
        (in.view_distance - globals.fog.x) / max(globals.fog.y - globals.fog.x, 0.001),
        0.0,
        1.0,
    );
    return vec4<f32>(mix(color.rgb, globals.fog_color.rgb, fog), color.a);
}
