// Upscales the low-resolution block-model volume raycast target over the
// scene. The raycaster renders into the top-left `params.scaled_dims` sub-rect
// of a full-size target (so the scale can change per frame without
// reallocating the texture); this pass stretches that sub-rect back to full
// screen. Nearest sampling keeps it cheap and avoids bleeding in the cleared
// region past the sub-rect edge — blocky during motion is acceptable.

struct Params {
    // xy: the sub-rect actually rendered this frame, in pixels.
    scaled_dims: vec4<f32>,
};
@group(0) @binding(0)
var volume_low_res: texture_2d<f32>;
@group(0) @binding(1)
var<uniform> params: Params;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    let pos = positions[vertex_index];
    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = pos * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let dims = max(params.scaled_dims.xy, vec2<f32>(1.0));
    let px = clamp(
        vec2<i32>(in.uv * dims),
        vec2<i32>(0, 0),
        vec2<i32>(dims) - vec2<i32>(1, 1),
    );
    // Premultiplied (colour, alpha); blended over the scene by the pipeline's
    // One / OneMinusSrcAlpha state, matching the direct volume pass it
    // replaces.
    return textureLoad(volume_low_res, px, 0);
}
