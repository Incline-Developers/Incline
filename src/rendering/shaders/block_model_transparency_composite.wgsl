@group(0) @binding(0)
var accum: texture_2d<f32>;

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
    let size = textureDimensions(accum);
    let pixel = vec2<i32>(
        clamp(i32(in.uv.x * f32(size.x)), 0, i32(size.x) - 1),
        clamp(i32(in.uv.y * f32(size.y)), 0, i32(size.y) - 1),
    );
    let sum = textureLoad(accum, pixel, 0);
    if (sum.a <= 0.000001) {
        return vec4<f32>(0.0);
    }
    let rgb = sum.rgb / sum.a;
    let alpha = clamp(1.0 - exp(-sum.a), 0.0, 0.98);
    return vec4<f32>(rgb * alpha, alpha);
}
