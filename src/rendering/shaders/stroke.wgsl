struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
    cam_position: vec4<f32>,
    viewport: vec4<f32>,
};
@group(0) @binding(0) // 1.
var<uniform> camera: CameraUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) other_position: vec3<f32>,
    @location(3) offset_px: vec2<f32>,
    @location(4) screen_space: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(
    model: VertexInput,
) -> VertexOutput {
    var out: VertexOutput;
    out.color = model.color;

    var clip = camera.view_proj * vec4<f32>(model.position, 1.0);
    let other_clip = camera.view_proj * vec4<f32>(model.other_position, 1.0);
    let current_ndc = clip.xy / max(abs(clip.w), 1e-6);
    let other_ndc = other_clip.xy / max(abs(other_clip.w), 1e-6);
    let delta = other_ndc - current_ndc;
    let direction = delta / max(length(delta), 1e-6);
    let normal = vec2<f32>(-direction.y, direction.x);
    let pixel_to_ndc = vec2<f32>(2.0 / camera.viewport.x, 2.0 / camera.viewport.y);
    var pixel_offset = model.offset_px;
    if model.screen_space < 0.5 {
        pixel_offset = normal * model.offset_px.x + direction * model.offset_px.y;
    }
    let adjusted_xy = clip.xy + pixel_offset * pixel_to_ndc * clip.w;
    clip = vec4<f32>(adjusted_xy, clip.z, clip.w);
    out.clip_position = clip;
    
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
