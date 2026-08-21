struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
    cam_position: vec4<f32>,
    viewport: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
};
@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct PointCloudStyle {
    color: vec4<f32>,
    // x: screen-facing splat width in world units; y: selected flag.
    options: vec4<f32>,
    // Fixed cloud origin relative to the current floating scene origin.
    origin: vec4<f32>,
};
@group(1) @binding(0)
var<uniform> style: PointCloudStyle;

struct ColoredPointInput {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

fn expand_point(pos: vec3<f32>, color: vec4<f32>, vertex_index: u32) -> VertexOutput {
    let right = vertex_index == 1u || vertex_index == 3u;
    let up = vertex_index >= 2u;
    let corner = vec2<f32>(select(-1.0, 1.0, right), select(-1.0, 1.0, up));
    // The inverse view-projection's clip-X/Y directions are the camera's
    // screen-space axes in the same scene coordinates as the point data.
    let world_right = normalize((camera.inv_view_proj * vec4<f32>(1.0, 0.0, 0.0, 0.0)).xyz);
    let world_up = normalize((camera.inv_view_proj * vec4<f32>(0.0, 1.0, 0.0, 0.0)).xyz);
    let half_size = style.options.x * 0.5;
    let world_offset = (corner.x * world_right + corner.y * world_up) * half_size;
    let clip = camera.view_proj * vec4<f32>(pos + style.origin.xyz + world_offset, 1.0);
    var out: VertexOutput;
    out.clip_position = clip;
    out.color = color;
    return out;
}

@vertex
fn vs_colored(point: ColoredPointInput, @builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let color = select(point.color, style.color, style.options.y > 0.5);
    return expand_point(point.pos, color, vertex_index);
}

@vertex
fn vs_uncolored(@location(0) pos: vec3<f32>, @builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    return expand_point(pos, style.color, vertex_index);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
