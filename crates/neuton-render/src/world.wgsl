// Block geometry. One pipeline draws the whole world: every face samples the
// same atlas, so material never changes and a chunk is one draw call.

struct Globals {
    view_projection: mat4x4<f32>,
    fog_color: vec4<f32>,
    // x: fog start, y: fog end, z: lowest light any surface is drawn at,
    // w unused.
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

    // Fullbright raises the floor rather than discarding the lighting, so
    // shape and the directional shade still read.
    let lit = texel.rgb * in.tint.rgb * max(in.light, globals.fog.z);
    return vec4<f32>(apply_fog(lit, in.view_distance), 1.0);
}

/// Water, ice and stained glass. Same geometry path, blended rather than cut
/// out, so what is behind them still shows through.
@fragment
fn fs_translucent(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    let lit = texel.rgb * in.tint.rgb * max(in.light, globals.fog.z);
    return vec4<f32>(apply_fog(lit, in.view_distance), texel.a * in.tint.a);
}

/// The cracks over a block being broken.
///
/// Vanilla multiplies these into the block already on screen rather than
/// drawing on top of it: the crumbling pipeline blends with DST_COLOR and
/// SRC_COLOR, so the result is 2*src*dst, and mid grey leaves the block's own
/// colour alone while the dark lines of the crack darken it. Painting the
/// texture over the block instead is what turns a broken stone block grey.
///
/// Nothing here is lit or fogged. Whatever is underneath already is, and doing
/// it a second time would darken a block just for being looked at.
@fragment
fn fs_crumbling(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    // The stage textures are a palette with one transparent entry, and those
    // texels are the gaps between the cracks. They have to miss the block
    // entirely: multiplied in, transparent white would double its brightness.
    if (texel.a < 0.1) {
        discard;
    }
    return vec4<f32>(texel.rgb, 1.0);
}

/// A player, drawn from a skin rather than from the block atlas.
///
/// Cut out, not blended: the outer layer of a skin is all or nothing, and
/// blending its edges leaves a halo around every hat and sleeve. Lit and
/// fogged like everything else, so a player at range sits in the same haze as
/// the ground they are standing on.
@fragment
fn fs_entity(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    if (texel.a < 0.5) {
        discard;
    }
    let lit = texel.rgb * in.tint.rgb * max(in.light, globals.fog.z);
    return vec4<f32>(apply_fog(lit, in.view_distance), 1.0);
}
