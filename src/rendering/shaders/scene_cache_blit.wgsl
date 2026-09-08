// Restores the cached main-scene image into the multisampled colour target so
// the editor overlay can be drawn over a scene that was rendered on an earlier
// frame. The cache is the resolve of that same target at the same size, so one
// texel fetch per pixel reproduces it exactly - no sampler, no filtering. All
// samples of a pixel take the resolved colour, which the pass then resolves
// back out unchanged; only the overlay drawn afterwards adds coverage.

@group(0) @binding(0) var scene_cache: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covering the clip volume, so the fetch below is
    // a straight framebuffer-coordinate lookup.
    let x = f32(i32(index) / 2) * 4.0 - 1.0;
    let y = f32(i32(index) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(scene_cache, vec2<i32>(position.xy), 0);
}
