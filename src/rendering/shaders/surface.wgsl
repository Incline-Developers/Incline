struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
};
@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct SurfaceStyle {
    color: vec4<f32>,
    // x: raster blend opacity; y: hatch pattern (0 clear, 1 slashes, 2 crosses);
    // z, w: scene-Z range to shade across, equal when depth shading is off.
    params: vec4<f32>,
    pattern_color: vec4<f32>,
};
@group(1) @binding(0)
var<uniform> surface_style: SurfaceStyle;

// Scene-origin-relative offset of this chunk's local origin. Vertices are
// stored relative to their chunk's AABB centre so interpolated positions keep
// f32 precision far from the scene origin.
struct SurfaceChunk {
    offset: vec4<f32>,
    pattern_phase: vec4<f32>,
};
@group(2) @binding(0)
var<uniform> chunk: SurfaceChunk;

@group(3) @binding(0)
var raster_texture: texture_2d<f32>;
@group(3) @binding(1)
var raster_sampler: sampler;
struct RasterMap {
    uv_x: vec4<f32>,
    uv_y: vec4<f32>,
};
@group(3) @binding(2)
var<uniform> raster_map: RasterMap;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Flat face normal from the triangle's provoking vertex, unit length and
    // pre-oriented with z >= 0 on the CPU. Using a stored normal instead of
    // dpdx/dpdy of the position avoids derivative cancellation noise when the
    // per-pixel position delta approaches the f32 ULP (static-like speckle,
    // worst close-up in fly mode).
    @location(1) @interpolate(flat) normal: vec3<f32>,
    @location(2) surface_xy: vec2<f32>,
    @location(3) pattern_position: vec3<f32>,
    // Scene-relative height, for the depth ramp. `pattern_position` cannot
    // stand in: its phase is deliberately wrapped to the hatch period.
    @location(4) scene_z: f32,
};

@vertex
fn vs_main(model: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.color = surface_style.color;
    out.normal = model.normal;
    let scene_position = model.position + chunk.offset.xyz;
    out.surface_xy = scene_position.xy;
    // Reduce each chunk origin in f64 on the CPU; interpolate only local
    // coordinates so close-up derivatives do not quantize at mine coordinates.
    out.pattern_position = model.position + chunk.pattern_phase.xyz;
    out.scene_z = scene_position.z;
    out.clip_position = camera.view_proj * vec4<f32>(scene_position, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(in.normal);
    // Fixed world-space orientation lighting. Surfaces are two-sided, so n and
    // -n describe the same face orientation: abs(dot(...)) keeps their shading
    // identical and avoids discontinuities on nearly vertical walls when the
    // CPU's z >= 0 normal orientation flips across z == 0. A second oblique
    // direction separates faces that receive similar light from the key.
    let key_direction = normalize(vec3<f32>(-0.55, -0.35, 0.76));
    let cross_direction = normalize(vec3<f32>(0.75, -0.62, 0.22));
    let key_light = abs(dot(normal, key_direction));
    let cross_light = abs(dot(normal, cross_direction));
    let horizontal_light = abs(normal.z);
    // Curve the orientation response so mid-lit faces sit in the mid greys
    // instead of bunching near white. The ambient floor remains high enough
    // to reveal unlit faces, while strongly aligned faces can still reach
    // white. There is deliberately no camera headlight.
    let orientation_light = clamp(
        0.70 * key_light + 0.22 * cross_light + 0.08 * horizontal_light,
        0.0,
        1.0,
    );
    let intensity = 0.18 + 0.82 * orientation_light * orientation_light;
    var surface_color = in.color;
    // Depth ramp: lowest ground near black, highest at full colour, so a
    // plan view still reads as a pit. Raster and hatch composite over it.
    let depth_span = surface_style.params.w - surface_style.params.z;
    if depth_span > 0.0 {
        let depth = clamp((in.scene_z - surface_style.params.z) / depth_span, 0.0, 1.0);
        surface_color = vec4<f32>(surface_color.rgb * mix(0.25, 1.0, depth), surface_color.a);
    }
    if surface_style.params.x > 0.0 {
        let uv = vec2<f32>(
            dot(raster_map.uv_x.xyz, vec3<f32>(in.surface_xy, 1.0)),
            dot(raster_map.uv_y.xyz, vec3<f32>(in.surface_xy, 1.0)),
        );
        let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
        if inside {
            // The branch is fragment-varying, so implicit derivatives are not
            // available uniformly on WebGPU. Raster textures have one mip.
            let image = textureSampleLevel(raster_texture, raster_sampler, uv, 0.0);
            surface_color = vec4<f32>(
                mix(surface_color.rgb, image.rgb, clamp(surface_style.params.x * image.a, 0.0, 1.0)),
                surface_color.a,
            );
        }
    }
    // Project onto the dominant world plane, including vertical slab walls.
    var hatch_uv = in.pattern_position.xy;
    let axis = abs(normal);
    if axis.x > axis.y && axis.x > axis.z {
        hatch_uv = in.pattern_position.yz;
    } else if axis.y > axis.z {
        hatch_uv = in.pattern_position.xz;
    }
    let diagonal = vec2<f32>(hatch_uv.x + hatch_uv.y, hatch_uv.x - hatch_uv.y) / 5.0;
    let distance = abs(fract(diagonal + 0.5) - 0.5);
    let dx = dpdx(diagonal);
    let dy = dpdy(diagonal);
    let pixels = max(sqrt(dx * dx + dy * dy), vec2<f32>(1e-7));
    let pixel_distance = distance / pixels;
    // Constant on-screen stroke: a half-pixel solid core fading out by 1.25
    // pixels either side, so zoom changes its world width, not its apparent
    // one. Fade the whole pattern out once a period falls below a few pixels,
    // at grazing angles or far zoom, instead of producing moire.
    let fade = vec2<f32>(1.0) - smoothstep(vec2<f32>(0.2), vec2<f32>(0.5), pixels);
    let strokes = (vec2<f32>(1.0) - smoothstep(vec2<f32>(0.25), vec2<f32>(1.25), pixel_distance)) * fade;
    if surface_style.params.y > 0.5 {
        var coverage = strokes.x;
        if surface_style.params.y > 1.5 {
            coverage = max(coverage, strokes.y);
        }
        surface_color = vec4<f32>(mix(surface_color.rgb, surface_style.pattern_color.rgb,
            coverage * surface_style.pattern_color.a), surface_color.a);
    }
    return vec4<f32>(surface_color.rgb * intensity, surface_color.a);
}
