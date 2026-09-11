// Procedural grid on the section plane: levels of constant true elevation
// and the world easting or northing lines the cut crosses. A fullscreen
// triangle reconstructs the plane intersection per pixel, as the XY grid
// does, so the grid has no mesh edge and stays stable under orbit and pan.
// Drawn after the opaque geometry with depth on: what is in front of the
// plane hides it, what is behind sits under its lines.

struct SectionGridUniform {
    // x: 1 to rule the world X (easting) lines, 0 for Y (northing);
    // y: spacing of those lines in metres; z: elevation spacing in metres.
    params: vec4<f32>,
    // x, y: one-cell phase of the scene origin along the ruled axis and in
    // elevation, computed in f64 so survey-scale offsets lose nothing;
    // z: line thickness in pixels.
    phase: vec4<f32>,
    color: vec4<f32>,
};
@group(1) @binding(0)
var<uniform> section_grid: SectionGridUniform;

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

// Where the pixel's view ray meets the section plane; w is a sentinel far
// below zero when the ray runs along it.
fn plane_hit(ndc: vec2<f32>) -> vec4<f32> {
    let near_h = camera.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let far_h = camera.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let near_point = near_h.xyz / near_h.w;
    let far_point = far_h.xyz / far_h.w;
    let ray = far_point - near_point;
    let normal = camera.section_normal.xyz;
    let denominator = dot(ray, normal);
    if abs(denominator) < 1.0e-7 {
        return vec4<f32>(0.0, 0.0, 0.0, -1.0e30);
    }
    let t = dot(camera.section_plane.xyz - near_point, normal) / denominator;
    return vec4<f32>(near_point + ray * t, t);
}

// Anti-aliased coverage of one family of lines along a scalar plane coordinate.
fn family_coverage(scene: f32, phase: f32, spacing: f32) -> f32 {
    let scaled_scene = scene / spacing;
    let scaled = scaled_scene + phase;
    let derivative = max(fwidth(scaled_scene), 1.0e-6);
    let cell_distance = abs(fract(scaled - 0.5) - 0.5) / derivative;
    let edge_distance = max(cell_distance - (section_grid.phase.z - 1.0) * 0.5, 0.0);
    return 1.0 - smoothstep(0.30, 0.90, edge_distance);
}

@fragment
fn fs_main(@builtin(position) fragment_position: vec4<f32>) -> FragmentOutput {
    let screen_xy = fragment_position.xy - camera.viewport_origin.xy;
    let viewport = max(camera.viewport.xy, vec2<f32>(1.0));
    let ndc = vec2<f32>(
        screen_xy.x / viewport.x * 2.0 - 1.0,
        1.0 - screen_xy.y / viewport.y * 2.0,
    );
    let hit = plane_hit(ndc);
    if hit.w < -1.0e20 {
        discard;
    }
    let scene_point = hit.xyz;
    let clip = camera.view_proj * vec4<f32>(scene_point, 1.0);
    if clip.w <= 0.0 {
        discard;
    }

    // Seen along the plane the lines would pile into a wall; fade them out.
    let grazing_fade = smoothstep(0.025, 0.17, abs(dot(camera.cam_forward.xyz, camera.section_normal.xyz)));
    if grazing_fade < 0.001 {
        discard;
    }

    let axis_scene = select(scene_point.y, scene_point.x, section_grid.params.x > 0.5);
    let axis_spacing = section_grid.params.y;
    let level_spacing = section_grid.params.z;
    // Each family measures its own pixel size: exaggeration stretches the
    // levels, not the uprights. Sub-pixel spacing must not pile into a sheet.
    let axis_per_pixel = max(length(vec2<f32>(dpdx(axis_scene), dpdy(axis_scene))), 1.0e-8);
    let level_per_pixel = max(length(vec2<f32>(dpdx(scene_point.z), dpdy(scene_point.z))), 1.0e-8);
    let axis_alpha = family_coverage(axis_scene, section_grid.phase.x, axis_spacing) * smoothstep(0.75, 2.5, axis_spacing / axis_per_pixel);
    let level_alpha = family_coverage(scene_point.z, section_grid.phase.y, level_spacing) * smoothstep(0.75, 2.5, level_spacing / level_per_pixel);
    let alpha = max(axis_alpha, level_alpha) * section_grid.color.a * grazing_fade;
    // Faint fringes write depth too, so they are dropped rather than drawn.
    if alpha < 0.02 {
        discard;
    }

    var out: FragmentOutput;
    out.color = vec4<f32>(section_grid.color.rgb, alpha);
    out.depth = clamp(clip.z / clip.w, 0.0, 1.0);
    return out;
}
