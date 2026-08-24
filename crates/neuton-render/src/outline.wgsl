// The wireframe box around the block a player is pointing at.
//
// Shares the world's camera so the lines sit exactly on the block, and draws
// with a slight bias towards the viewer so an edge lying flat against a face is
// not swallowed by it.

struct Globals {
    view_projection: mat4x4<f32>,
    fog_color: vec4<f32>,
    fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return globals.view_projection * vec4<f32>(position, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.4);
}
