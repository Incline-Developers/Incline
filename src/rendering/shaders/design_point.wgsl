struct DesignPointStyle {
    color: vec4<f32>,
    // x: marker width in physical pixels.
    options: vec4<f32>,
    // Design-point positions are relative to the floating scene origin.
    origin: vec4<f32>,
};
@group(1) @binding(0)
var<uniform> style: DesignPointStyle;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Distance from the section plane, the same at all four sprite corners, so
    // the marker is kept or dropped whole.
    @location(1) section_offset: f32,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let right = vertex_index == 1u || vertex_index == 3u;
    let up = vertex_index >= 2u;
    let corner = vec2<f32>(select(-1.0, 1.0, right), select(-1.0, 1.0, up));
    let half_size_px = style.options.x * 0.5;
    let pixel_to_ndc = vec2<f32>(2.0 / camera.viewport.x, 2.0 / camera.viewport.y);
    // The marker's centre, before the corner offset below spreads the sprite.
    let world_position = pos + style.origin.xyz;
    var clip = camera.view_proj * vec4<f32>(world_position, 1.0);
    clip.x = clip.x + corner.x * half_size_px * pixel_to_ndc.x * clip.w;
    clip.y = clip.y + corner.y * half_size_px * pixel_to_ndc.y * clip.w;

    var out: VertexOutput;
    out.clip_position = clip;
    out.color = style.color;
    out.section_offset = section_plane_offset(world_position);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if outside_section_slab(in.section_offset) {
        discard;
    }
    return in.color;
}
