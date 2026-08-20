struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
};
@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct ColorStop {
    color: vec4<f32>,
    // .x holds the stop's position (0..1); rest is padding.
    pos: vec4<f32>,
};
struct BlockModelStyle {
    fallback_color: vec4<f32>,
    // options.x: has grade colour; options.y: active colour stop count.
    options: vec4<f32>,
    // Local->scene affine for expanding instanced block bounds:
    // scene_pos = rotation * local + translation. `rotation` is a mat3 stored
    // as three column vec4s (.xyz used); `translation.xyz` is the offset.
    rotation_0: vec4<f32>,
    rotation_1: vec4<f32>,
    rotation_2: vec4<f32>,
    translation: vec4<f32>,
    // Slice bounds in the same local instance frame as lower/upper.
    clip_min: vec4<f32>,
    clip_max: vec4<f32>,
    stops: array<ColorStop, 12>,
};
@group(1) @binding(0)
var<uniform> block_style: BlockModelStyle;

const VISIBLE_ALPHA_EPSILON: f32 = 0.004;

// Per-instance block: local-space axis-aligned bounds plus grade. One
// instance draws a full cube via a non-indexed draw of 36 vertices.
struct InstanceInput {
    @location(0) lower: vec3<f32>,
    @location(1) grade: f32,
    @location(2) upper: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) grade: f32,
    @location(1) local_position: vec3<f32>,
};

// Walks the sorted colour-transfer stops as hard cutoffs. Values below the
// first stop are transparent; each stop then remains active until the next.
fn ramp_color(t: f32) -> vec4<f32> {
    let stop_count = max(2, i32(block_style.options.y + 0.5));
    let last_index = stop_count - 1;
    let first = block_style.stops[0];
    let last = block_style.stops[last_index];
    if (t < first.pos.x) {
        return vec4<f32>(0.0);
    }
    if (t >= last.pos.x) {
        return last.color;
    }
    for (var i = 1; i < 12; i++) {
        if (i >= stop_count) {
            break;
        }
        let stop = block_style.stops[i];
        if (t < stop.pos.x) {
            return block_style.stops[i - 1].color;
        }
    }
    return last.color;
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, instance: InstanceInput) -> VertexOutput {
    // Corner index for each of the 36 cube vertices (6 faces x 2 triangles x
    // 3), matching the CPU `QUADS` winding table (corner order 0:(lo,lo,lo) ..
    // 7:(lo,hi,hi)). cull_mode is _None, so winding only affects the derivative
    // normal, which `fs_main` flips toward the camera anyway. Declared as
    // function-local `var`s so the dynamic index is portable across naga.
    var cube_corners = array<u32, 36>(
        0u, 3u, 7u, 0u, 7u, 4u, // -X (quad 0,3,7,4)
        1u, 5u, 6u, 1u, 6u, 2u, // +X (quad 1,5,6,2)
        0u, 4u, 5u, 0u, 5u, 1u, // -Y (quad 0,4,5,1)
        3u, 2u, 6u, 3u, 6u, 7u, // +Y (quad 3,2,6,7)
        0u, 1u, 2u, 0u, 2u, 3u, // -Z (quad 0,1,2,3)
        4u, 7u, 6u, 4u, 6u, 5u, // +Z (quad 4,7,6,5)
    );
    // Per-corner selection of lower(0)/upper(1) along each axis.
    var corner_mask = array<vec3<f32>, 8>(
        vec3<f32>(0.0, 0.0, 0.0),
        vec3<f32>(1.0, 0.0, 0.0),
        vec3<f32>(1.0, 1.0, 0.0),
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(0.0, 0.0, 1.0),
        vec3<f32>(1.0, 0.0, 1.0),
        vec3<f32>(1.0, 1.0, 1.0),
        vec3<f32>(0.0, 1.0, 1.0),
    );

    let corner = cube_corners[vertex_index];
    let mask = corner_mask[corner];
    // Clamp boundary blocks to the requested slice and collapse blocks that
    // sit entirely outside it. A collapsed triangle produces no fragments.
    let clipped_lower = max(instance.lower, block_style.clip_min.xyz);
    let clipped_upper = min(instance.upper, block_style.clip_max.xyz);
    let outside = any(clipped_lower >= clipped_upper);
    let local = select(
        mix(clipped_lower, clipped_upper, mask),
        clipped_lower,
        outside,
    );
    let rotation = mat3x3<f32>(
        block_style.rotation_0.xyz,
        block_style.rotation_1.xyz,
        block_style.rotation_2.xyz,
    );
    let position = rotation * local + block_style.translation.xyz;

    var out: VertexOutput;
    out.grade = instance.grade;
    out.local_position = position;
    out.clip_position = camera.view_proj * vec4<f32>(position, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (in.grade < -1.5) {
        discard;
    }
    var normal = normalize(cross(dpdx(in.local_position), dpdy(in.local_position)));
    if (dot(normal, camera.cam_forward.xyz) > 0.0) {
        normal = -normal;
    }
    let has_grade = block_style.options.x > 0.5 && in.grade >= 0.0;
    let grade_color = ramp_color(in.grade);
    if (has_grade && grade_color.a < 0.004) {
        discard;
    }
    let rgb = select(block_style.fallback_color.rgb, grade_color.rgb, has_grade);
    let alpha = select(block_style.fallback_color.a, grade_color.a, has_grade);
    let key_light = max(dot(normal, normalize(vec3<f32>(-0.60, -0.50, 0.35))), 0.0);
    let fill_light = max(dot(normal, normalize(vec3<f32>(0.45, 0.35, 0.75))), 0.0);
    let view_light = abs(dot(normal, -normalize(camera.cam_forward.xyz)));
    let intensity = 0.28 + 0.18 * view_light + 0.42 * key_light + 0.12 * fill_light;
    return vec4<f32>(rgb * intensity, alpha);
}
