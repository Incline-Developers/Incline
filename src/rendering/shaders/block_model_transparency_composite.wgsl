@group(0) @binding(0)
var accum: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) fragment_position: vec4<f32>) -> @location(0) vec4<f32> {
    // `accum` is sized and positioned to match the full render target this
    // pass draws into (both this pass and the one that filled `accum` set the
    // same viewport), so the fragment's own absolute position already indexes
    // it correctly - no UV rescale needed.
    let size = textureDimensions(accum);
    let pixel = vec2<i32>(
        clamp(i32(fragment_position.x), 0, i32(size.x) - 1),
        clamp(i32(fragment_position.y), 0, i32(size.y) - 1),
    );
    let sum = textureLoad(accum, pixel, 0);
    if (sum.a <= 0.000001) {
        return vec4<f32>(0.0);
    }
    let rgb = sum.rgb / sum.a;
    let alpha = clamp(1.0 - exp(-sum.a), 0.0, 0.98);
    return vec4<f32>(rgb * alpha, alpha);
}
