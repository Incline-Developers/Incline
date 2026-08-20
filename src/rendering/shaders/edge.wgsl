struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
    cam_position: vec4<f32>,
    viewport: vec4<f32>,
};
@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct EdgeStyle {
    color: vec4<f32>,
    width: f32,
};
@group(1) @binding(0)
var<uniform> edge_style: EdgeStyle;

struct EdgeInput {
    @location(0) start: vec3<f32>,
    @location(1) end: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(edge: EdgeInput, @builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let start_clip = camera.view_proj * vec4<f32>(edge.start, 1.0);
    let end_clip = camera.view_proj * vec4<f32>(edge.end, 1.0);
    let start_ndc = start_clip.xy / max(abs(start_clip.w), 1e-6);
    let end_ndc = end_clip.xy / max(abs(end_clip.w), 1e-6);
    let direction = (end_ndc - start_ndc) / max(length(end_ndc - start_ndc), 1e-6);
    let normal = vec2<f32>(-direction.y, direction.x);
    let pixel_to_ndc = vec2<f32>(2.0 / camera.viewport.x, 2.0 / camera.viewport.y);
    let use_end = vertex_index == 2u || vertex_index == 4u || vertex_index == 5u;
    let positive = vertex_index == 0u || vertex_index == 3u || vertex_index == 5u;
    var clip = select(start_clip, end_clip, use_end);
    let side = select(-1.0, 1.0, positive);
    clip.x = clip.x + normal.x * side * edge_style.width * 0.5 * pixel_to_ndc.x * clip.w;
    clip.y = clip.y + normal.y * side * edge_style.width * 0.5 * pixel_to_ndc.y * clip.w;
    var out: VertexOutput;
    out.clip_position = clip;
    out.color = edge_style.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
