struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
    cam_position: vec4<f32>,
    viewport: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: CameraUniform;

struct SegmentInput {
    @location(0) start_radius: vec4<f32>,
    @location(1) end_pad: vec4<f32>,
    @location(2) color_pad: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
};

fn ring(angle: f32) -> vec2<f32> { return vec2<f32>(cos(angle), sin(angle)); }

@vertex
fn vs_main(instance: SegmentInput, @builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let tau = 6.28318530718;
    var radial = vec2<f32>(0.0, 0.0);
    var axial = 0.0;
    var local_normal = vec3<f32>(0.0, 0.0, 0.0);
    if vertex_index < 72u {
        let side = vertex_index / 6u;
        let corner = vertex_index % 6u;
        let a = ring(f32(side) * tau / 12.0);
        let b = ring(f32(side + 1u) * tau / 12.0);
        radial = select(select(a, b, corner == 1u || corner == 2u || corner == 4u), a, corner == 3u || corner == 5u);
        // Two triangles per wall quad:
        // a0, b0, b1 and a0, b1, a1.
        axial = select(0.0, 1.0, corner == 2u || corner == 4u || corner == 5u);
        local_normal = vec3<f32>(normalize(radial), 0.0);
    } else {
        let cap_vertex = vertex_index - 72u;
        let top = (cap_vertex % 6u) >= 3u;
        let corner = cap_vertex % 3u;
        let side = cap_vertex / 6u;
        let a = ring(f32(side) * tau / 12.0);
        let b = ring(f32(side + 1u) * tau / 12.0);
        radial = select(select(a, b, corner == 1u), vec2<f32>(0.0), corner == 0u);
        axial = select(0.0, 1.0, top);
        local_normal = vec3<f32>(0.0, 0.0, select(-1.0, 1.0, top));
    }
    let axis_vector = instance.end_pad.xyz - instance.start_radius.xyz;
    let axis = normalize(axis_vector);
    let helper = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), abs(axis.z) > 0.9);
    let right = normalize(cross(helper, axis));
    let up = cross(axis, right);
    let center = instance.start_radius.xyz + axis_vector * axial;
    let radial_direction = right * radial.x + up * radial.y;
    var radius = instance.start_radius.w;
    if instance.end_pad.w > 0.0 {
        // Use one scale for the whole ring. A radial direction can point
        // exactly into the camera and have a zero screen-space derivative;
        // deriving the radius from that individual vertex would make its
        // minimum world radius unbounded and clip neighbouring triangles.
        // At least one of the two cross-section axes remains visible, so the
        // larger derivative gives the cylinder a stable projected width.
        let center_clip = camera.view_proj * vec4<f32>(center, 1.0);
        let right_clip = camera.view_proj * vec4<f32>(right, 0.0);
        let up_clip = camera.view_proj * vec4<f32>(up, 0.0);
        let safe_w = max(abs(center_clip.w), 1.0e-6);
        let right_ndc_per_world = (right_clip.xy * center_clip.w - center_clip.xy * right_clip.w) / (safe_w * safe_w);
        let up_ndc_per_world = (up_clip.xy * center_clip.w - center_clip.xy * up_clip.w) / (safe_w * safe_w);
        let pixels_per_world = max(
            length(right_ndc_per_world * camera.viewport.xy * 0.5),
            length(up_ndc_per_world * camera.viewport.xy * 0.5),
        );
        let minimum_world_radius = (instance.end_pad.w * 0.5) / max(pixels_per_world, 1.0e-6);
        radius = max(radius, minimum_world_radius);
    }
    let world = center + radial_direction * radius;
    let normal = normalize(right * local_normal.x + up * local_normal.y + axis * local_normal.z);
    var out: VertexOutput;
    out.position = camera.view_proj * vec4<f32>(world, 1.0);
    out.normal = normal;
    out.color = instance.color_pad.xyz;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let light = normalize(vec3<f32>(0.35, 0.45, 0.82));
    let diffuse = 0.38 + 0.62 * abs(dot(normalize(input.normal), light));
    return vec4<f32>(input.color * diffuse, 1.0);
}
