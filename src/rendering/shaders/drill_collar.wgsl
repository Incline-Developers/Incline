struct CollarInstance {
    // xyz: collar position, relative to the scene origin. w: marker radius in
    // world units.
    @location(0) center_radius: vec4<f32>,
    // xyz: outline colour. w: minimum marker diameter in physical pixels.
    @location(1) outline_pixels: vec4<f32>,
    // xyz: fill colour. w: the hole's own rendered radius, used to lift the
    // marker clear of the cylinder it caps.
    @location(2) fill_hole_radius: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    // Unit-disc coordinates, (0,0) at the collar and 1.0 at the rim.
    @location(0) offset: vec2<f32>,
    @location(1) outline: vec3<f32>,
    @location(2) fill: vec3<f32>,
    // Flat because the radius is constant across the quad and the fragment
    // shader measures its ring widths in pixels against it.
    @location(3) @interpolate(flat) radius_pixels: f32,
    // Distance from the section plane, flat: the marker is kept or dropped whole.
    @location(4) @interpolate(flat) section_offset: f32,
};

const OUTLINE_PIXELS: f32 = 1.6;
const EDGE_PIXELS: f32 = 0.8;
// Keep in sync with model::drill_hole::MIN_RENDER_PIXEL_DIAMETER.
const MIN_HOLE_PIXEL_DIAMETER: f32 = 2.0;

@vertex
fn vs_main(instance: CollarInstance, @builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Two triangles: (-1,-1) (1,-1) (1,1), (-1,-1) (1,1) (-1,1).
    let right_corner = vertex_index == 1u || vertex_index == 2u || vertex_index == 4u;
    let top_corner = vertex_index == 2u || vertex_index == 4u || vertex_index == 5u;
    let corner = vec2<f32>(select(-1.0, 1.0, right_corner), select(-1.0, 1.0, top_corner));

    let center = instance.center_radius.xyz;
    let view_direction = normalize(camera.cam_forward.xyz);
    // Any two world axes across the view plane measure the same projected
    // scale, so the pair is built from the view direction alone.
    let helper = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), abs(view_direction.z) > 0.9);
    let right = normalize(cross(helper, view_direction));
    let up = cross(view_direction, right);

    let center_clip = camera.view_proj * vec4<f32>(center, 1.0);
    let right_clip = camera.view_proj * vec4<f32>(right, 0.0);
    let up_clip = camera.view_proj * vec4<f32>(up, 0.0);
    let safe_w = max(abs(center_clip.w), 1.0e-6);
    let right_ndc_per_world = (right_clip.xy * center_clip.w - center_clip.xy * right_clip.w) / (safe_w * safe_w);
    let up_ndc_per_world = (up_clip.xy * center_clip.w - center_clip.xy * up_clip.w) / (safe_w * safe_w);
    let pixels_per_world = max(
        max(length(right_ndc_per_world * camera.viewport.xy * 0.5), length(up_ndc_per_world * camera.viewport.xy * 0.5)),
        1.0e-6,
    );

    // Floor the marker independently of the trace so it can shrink to a dot
    // in overview views while retaining its world-space scale up close.
    let source_marker_radius = max(instance.center_radius.w, 1.0e-6);
    let source_hole_radius = max(instance.fill_hole_radius.w, 1.0e-6);
    let radius_pixels = max(source_marker_radius * pixels_per_world, instance.outline_pixels.w * 0.5);
    // The cylinder starts at the collar and bulges toward the camera by its
    // own rendered radius, so a marker left in the collar plane would be
    // sliced open by it in any side-on view. Lifting the marker clear along
    // the view direction moves it nearer the camera by the same amount under
    // either projection.
    let hole_radius = max(source_hole_radius, MIN_HOLE_PIXEL_DIAMETER * 0.5 / pixels_per_world);
    let lifted = center - view_direction * hole_radius * 1.5;

    var clip = camera.view_proj * vec4<f32>(lifted, 1.0);
    let pixel_to_ndc = vec2<f32>(2.0 / camera.viewport.x, 2.0 / camera.viewport.y);
    clip = vec4<f32>(clip.xy + corner * radius_pixels * pixel_to_ndc * clip.w, clip.zw);

    var out: VertexOutput;
    out.position = clip;
    out.offset = corner;
    out.outline = instance.outline_pixels.xyz;
    out.fill = instance.fill_hole_radius.xyz;
    out.radius_pixels = radius_pixels;
    out.section_offset = section_plane_offset(center);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if outside_section_slab(input.section_offset) {
        discard;
    }
    let distance_pixels = length(input.offset) * input.radius_pixels;
    // Not opacity: the pipeline turns this into a sample-coverage mask, so
    // the rim antialiases against the scene behind it without writing the
    // marker's depth over the samples it does not cover.
    let coverage = 1.0 - smoothstep(input.radius_pixels - EDGE_PIXELS, input.radius_pixels, distance_pixels);
    if coverage <= 0.0 {
        discard;
    }
    // A large marker keeps a proportional ring rather than a hairline one.
    let outline_width = max(OUTLINE_PIXELS, input.radius_pixels * 0.22);
    let inner_radius = max(input.radius_pixels - outline_width, 0.0);
    let ring = smoothstep(inner_radius - EDGE_PIXELS, inner_radius, distance_pixels);
    // At the three-pixel minimum, show only the fill so selection remains
    // legible. Restore the outline gradually, reaching full strength at an
    // eight-pixel diameter where there is room for both colours.
    let outline_strength = smoothstep(1.5, 4.0, input.radius_pixels);
    return vec4<f32>(mix(input.fill, input.outline, ring * outline_strength), coverage);
}
