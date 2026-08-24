// Procedural infinite grid on the world XY plane at Z=0. A fullscreen
// triangle reconstructs the plane intersection for each pixel, so the grid has
// no finite mesh edge and stays stable while panning or zooming.

struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
    cam_position: vec4<f32>,
    viewport: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
    // xy: viewport rect offset in physical pixels within the render target -
    // `@builtin(position)` is target-relative, not viewport-relative, so this
    // must be subtracted before treating a fragment's position as 0-based.
    viewport_origin: vec4<f32>,
};
@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct GridUniform {
    // xy: absolute world offset of the rebased scene; z: world-Z-zero elevation
    // relative to that scene origin; w: perspective-view flag.
    origin_plane: vec4<f32>,
    // x: fractional base-10 level selected once for the whole view;
    // y: whether grazing-angle suppression is enabled (disabled in Fly mode).
    level_params: vec4<f32>,
    minor_color: vec4<f32>,
    major_color: vec4<f32>,
    x_axis_color: vec4<f32>,
    y_axis_color: vec4<f32>,
};
@group(1) @binding(0)
var<uniform> grid: GridUniform;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covers the viewport without a vertex buffer.
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

fn plane_point(ndc: vec2<f32>) -> vec4<f32> {
    // Reversed-Z: NDC z=1 is the near plane and z=0 is the far plane.
    let near_h = camera.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let far_h = camera.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let near_point = near_h.xyz / near_h.w;
    let far_point = far_h.xyz / far_h.w;
    let ray = far_point - near_point;
    if abs(ray.z) < 1.0e-7 {
        return vec4<f32>(0.0, 0.0, 0.0, -1.0e30);
    }
    let t = (grid.origin_plane.z - near_point.z) / ray.z;
    return vec4<f32>(near_point + ray * t, t);
}

// Anti-aliased coverage for both families of lines at one world-space scale.
fn grid_coverage(scene_xy: vec2<f32>, world_offset: vec2<f32>, spacing: f32) -> f32 {
    // Reduce the large absolute offset to a one-cell phase before adding it
    // to the small rebased coordinate. This preserves sub-metre derivatives
    // at survey/grid coordinates in the millions.
    let scaled_scene = scene_xy / spacing;
    let scaled = scaled_scene + fract(world_offset / spacing);
    let derivative = max(fwidth(scaled_scene), vec2<f32>(1.0e-6));
    let cell_distance = abs(fract(scaled - 0.5) - 0.5) / derivative;
    let pixel_distance = min(cell_distance.x, cell_distance.y);
    return 1.0 - smoothstep(0.30, 0.90, pixel_distance);
}

fn clip_to_screen_homogeneous(clip: vec4<f32>) -> vec3<f32> {
    // Linear homogeneous form of NDC -> framebuffer pixels. Keeping W avoids
    // divisions near the perspective horizon.
    return vec3<f32>(
        camera.viewport.x * 0.5 * (clip.x + clip.w),
        camera.viewport.y * 0.5 * (clip.w - clip.y),
        clip.w,
    );
}

fn projected_axis_coverage(fragment_xy: vec2<f32>, base: vec3<f32>, direction: vec3<f32>) -> f32 {
    // A 3D line projects to a 2D homogeneous line. Transform one point and
    // its direction separately, then take their cross product; unlike fwidth
    // on reconstructed world coordinates, this remains a single stable line
    // when plane intersections approach infinity at the horizon.
    let point_h = clip_to_screen_homogeneous(camera.view_proj * vec4<f32>(base, 1.0));
    let direction_h = clip_to_screen_homogeneous(camera.view_proj * vec4<f32>(direction, 0.0));
    let line = cross(point_h, direction_h);
    let line_length = length(line.xy);
    if line_length < 1.0e-6 {
        return 0.0;
    }
    let pixel_distance = abs(dot(line, vec3<f32>(fragment_xy, 1.0))) / line_length;
    return 1.0 - smoothstep(0.35, 0.95, pixel_distance);
}

fn line_alpha(scene_xy: vec2<f32>, spacing: f32, opacity: f32, world_per_pixel: f32) -> f32 {
    let coverage = grid_coverage(scene_xy, grid.origin_plane.xy, spacing);
    // Do not let sub-pixel lines accumulate into a bright sheet near the
    // horizon. Blender handles the same transition through level alpha and
    // low-alpha stippling; smooth coverage is a better fit for this pipeline.
    let projected_spacing = spacing / world_per_pixel;
    let density_fade = smoothstep(0.75, 2.5, projected_spacing);
    return coverage * opacity * density_fade;
}

@fragment
fn fs_main(@builtin(position) fragment_position: vec4<f32>) -> FragmentOutput {
    // `fragment_position` is relative to the render target, which for the
    // main window is the whole (toolbar-occluded) surface - rebase it to the
    // viewport sub-rect `camera.viewport` itself describes before using it as
    // a 0-based screen coordinate.
    let screen_xy = fragment_position.xy - camera.viewport_origin.xy;
    let viewport = max(camera.viewport.xy, vec2<f32>(1.0));
    let ndc = vec2<f32>(
        screen_xy.x / viewport.x * 2.0 - 1.0,
        1.0 - screen_xy.y / viewport.y * 2.0,
    );
    let hit = plane_point(ndc);
    if hit.w < -1.0e20 {
        discard;
    }

    let scene_point = hit.xyz;
    let clip = camera.view_proj * vec4<f32>(scene_point, 1.0);
    // Perspective points behind the eye have non-positive W. Keep plane
    // intersections in front of the eye even when they lie between it and
    // the near clip plane; this is an editor construction aid, so its depth
    // is clamped below rather than cutting the foreground off abruptly.
    if clip.w <= 0.0 {
        discard;
    }
    let unclamped_depth = clip.z / clip.w;

    // Suppress the entire grid as the view approaches parallel with its
    // plane. At grazing angles the projected cells collapse into a dense wall
    // of lines, and the coloured axes can stretch across most of the screen.
    // Keep the transition smooth so orbiting never produces a visible pop.
    let grazing_angle_fade = mix(1.0, smoothstep(0.025, 0.17, abs(camera.cam_forward.z)), grid.level_params.y);
    if grazing_angle_fade < 0.001 {
        discard;
    }

    let dx = dpdx(scene_point.xy);
    let dy = dpdy(scene_point.xy);
    let world_per_pixel = max(max(length(dx), length(dy)), 1.0e-8);

    // Use one camera-selected level over the whole plane, matching Blender's
    // viewport grid. Three adjacent powers of ten crossfade without changing
    // scale from pixel to pixel, so perspective lines converge naturally.
    let level_floor = floor(grid.level_params.x) - 1.0;
    let transition = fract(grid.level_params.x);
    let fine_spacing = pow(10.0, level_floor);
    let coarse_spacing = fine_spacing * 10.0;

    let fine_alpha = line_alpha(scene_point.xy, fine_spacing, grid.minor_color.a * (1.0 - transition), world_per_pixel);
    let coarse_alpha = line_alpha(
        scene_point.xy,
        coarse_spacing,
        mix(grid.major_color.a, grid.minor_color.a, transition),
        world_per_pixel,
    );
    let super_alpha = line_alpha(scene_point.xy, coarse_spacing * 10.0, grid.major_color.a, world_per_pixel);
    var alpha = max(max(fine_alpha, coarse_alpha), super_alpha);
    var premultiplied = mix(grid.minor_color.rgb, grid.major_color.rgb, clamp(max(coarse_alpha, super_alpha) / max(alpha, 1.0e-6), 0.0, 1.0)) * alpha;

    // Project the two infinite world axes directly. The arbitrary coordinate
    // along each line is kept at scene zero to avoid large-coordinate loss.
    let x_axis_base = vec3<f32>(0.0, -grid.origin_plane.y, grid.origin_plane.z);
    let y_axis_base = vec3<f32>(-grid.origin_plane.x, 0.0, grid.origin_plane.z);
    let y_axis_alpha = projected_axis_coverage(screen_xy, y_axis_base, vec3<f32>(0.0, 1.0, 0.0)) * grid.y_axis_color.a;
    premultiplied = grid.y_axis_color.rgb * y_axis_alpha + premultiplied * (1.0 - y_axis_alpha);
    alpha = y_axis_alpha + alpha * (1.0 - y_axis_alpha);
    let x_axis_alpha = projected_axis_coverage(screen_xy, x_axis_base, vec3<f32>(1.0, 0.0, 0.0)) * grid.x_axis_color.a;
    premultiplied = grid.x_axis_color.rgb * x_axis_alpha + premultiplied * (1.0 - x_axis_alpha);
    alpha = x_axis_alpha + alpha * (1.0 - x_axis_alpha);

    premultiplied *= grazing_angle_fade;
    alpha *= grazing_angle_fade;

    if alpha < 0.001 {
        discard;
    }

    var out: FragmentOutput;
    out.color = vec4<f32>(premultiplied / alpha, alpha);
    // Keep the construction plane continuous even when the scene-fitted clip
    // range is tighter than the visible floor. At far depth it still remains
    // behind every scene object because this renderer uses reversed-Z.
    out.depth = clamp(unclamped_depth, 0.0, 1.0);
    return out;
}
