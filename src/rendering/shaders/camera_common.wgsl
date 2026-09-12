// Shared camera uniform and section-slab helpers, prefixed to the source of
// every shader that binds a camera (see `make_shader` in rendering/graphics/init.rs).

struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
    cam_position: vec4<f32>,
    viewport: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
    // xy: the viewport rect's offset in physical pixels within its render
    // target, to be subtracted from `@builtin(position)`; zero except in the
    // main window's toolbar-bounded canvas. zw: padding.
    viewport_origin: vec4<f32>,
    // xyz: a point on the vertical section plane, relative to the scene
    // origin the vertex positions are relative to; w: half the slab width
    // in metres.
    section_plane: vec4<f32>,
    // xyz: the section plane's unit normal, horizontal by construction;
    // w: 1 while the section slab clips the scene, 0 in every other view.
    section_normal: vec4<f32>,
};
@group(0) @binding(0)
var<uniform> camera: CameraUniform;

// How far a point lies from the section plane, signed by which wall it is on.
// The normal is horizontal, so vertical exaggeration cannot skew this.
fn section_plane_offset(world: vec3<f32>) -> f32 {
    return dot(world - camera.section_plane.xyz, camera.section_normal.xyz);
}

// A section shows a fixed slab of ground between two walls parallel to its
// plane. Outside a section the slab is off and this discards nothing.
fn outside_section_slab(offset: f32) -> bool {
    return camera.section_normal.w > 0.5 && abs(offset) > camera.section_plane.w;
}
