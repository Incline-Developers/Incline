// Flat plan-view quad for an undraped raster: the image drawn at its
// georeferenced footprint, pinned to the far plane so all scene geometry
// renders over it. Only drawn in exact top-down orthographic views.

@group(1) @binding(0)
var raster_texture: texture_2d<f32>;
@group(1) @binding(1)
var raster_sampler: sampler;

struct VertexInput {
    // Scene-origin-relative world XY of the raster corner (z is irrelevant:
    // the view is exactly top-down and clip z is overridden below).
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Distance from the section plane; affine in position, so interpolation is exact.
    @location(1) section_offset: f32,
};

@vertex
fn vs_main(model: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.uv = model.uv;
    // The plane's z is overridden below and the section normal is horizontal,
    // so elevation cannot affect this offset.
    out.section_offset = section_plane_offset(vec3<f32>(model.position, 0.0));
    var clip = camera.view_proj * vec4<f32>(model.position, 0.0, 1.0);
    // Pin to just inside the far plane: depth writes are off and every scene
    // draw passes GreaterEqual against it, so the image stays behind everything
    // regardless of the current clip range.
    clip.z = clip.w * 0.000001;
    out.clip_position = clip;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if outside_section_slab(in.section_offset) {
        discard;
    }
    return textureSample(raster_texture, raster_sampler, in.uv);
}
