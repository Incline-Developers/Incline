pub(crate) mod camera;
pub(crate) mod color;
pub(crate) mod geometry;
pub(crate) mod graphics;
pub(crate) mod pick;
pub(crate) mod query;
pub(crate) mod scene;
pub(crate) mod snap;
pub(crate) mod text;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug)]
pub(crate) struct Vertex {
    pub(crate) pos: [f32; 3],
    pub(crate) color: [f32; 4],
}

/// Vertex for triangulation surfaces — colour comes from a per-draw uniform.
/// `pos` is chunk-origin-relative (see `CachedSurfaceChunk`) so f32 stays
/// precise far from the scene origin. `normal` is the flat face normal of the
/// triangle this vertex provokes (`@interpolate(flat)` in the shader), already
/// oriented with `z >= 0` for the two-sided surface lighting.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug)]
pub(crate) struct SurfaceVertex {
    pub(crate) pos: [f32; 3],
    pub(crate) normal: [f32; 3],
}

/// One instanced block: local-space axis-aligned bounds plus grade. The
/// vertex shader expands this into a full cube and applies the model's
/// local->scene rotation, so a block costs 32 B instead of eight vertices plus
/// indices. Grade is a normalized 0..1 value, or a negative sentinel when no
/// numeric colour variable is active / the block is hidden.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug)]
pub(crate) struct BlockInstance {
    pub(crate) lower: [f32; 3],
    pub(crate) grade: f32,
    pub(crate) upper: [f32; 3],
    pub(crate) _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug)]
pub(crate) struct StrokeVertex {
    pub(crate) pos: [f32; 3],
    pub(crate) color: [f32; 4],
    pub(crate) other_pos: [f32; 3],
    pub(crate) offset_px: [f32; 2],
    pub(crate) screen_space: f32,
}
