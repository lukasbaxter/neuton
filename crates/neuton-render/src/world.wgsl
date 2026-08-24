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
    @location(2) tint: vec4<f32>,
    @location(3) light: f32,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
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

/// Distance haze, so the edge of the loaded world fades into the sky rather
/// than ending at a wall.
fn apply_fog(rgb: vec3<f32>, distance: f32) -> vec3<f32> {
    let fog = clamp(
        (distance - globals.fog.x) / max(globals.fog.y - globals.fog.x, 0.001),
        0.0,
        1.0,
    );
    return mix(rgb, globals.fog_color.rgb, fog);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);

    // Cutout, not blending. Leaves and grass have soft edges in the texture,
    // and blending them leaves a halo of whatever was behind: against the sky
    // that reads as a white fringe around every tree. A hard threshold is what
    // the game does for foliage, and it survives mipmapping, where a blended
    // edge only gets softer and paler with distance.
    if (texel.a < 0.5) {
        discard;
    }

    let lit = texel.rgb * in.tint.rgb * in.light;
    return vec4<f32>(apply_fog(lit, in.view_distance), 1.0);
}

/// Water, ice and stained glass. Same geometry path, blended rather than cut
/// out, so what is behind them still shows through.
@fragment
fn fs_translucent(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    let lit = texel.rgb * in.tint.rgb * in.light;
    return vec4<f32>(apply_fog(lit, in.view_distance), texel.a * in.tint.a);
}
