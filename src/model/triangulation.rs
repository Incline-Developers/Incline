use std::{path::PathBuf, sync::Arc};

use crate::model::formats::mesh_data;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TriangulationId(pub(crate) u64);

/// The computed outputs of loading a triangulation file, produced on a background
/// thread and sent back to the main thread via channel.
pub(crate) struct LoadedTriangulation {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) mesh: Arc<mesh_data::Triangulation>,
    pub(crate) spatial: Arc<crate::model::spatial::TriangleBvh>,
    pub(crate) edges: Vec<[u32; 2]>,
    pub(crate) surface_face_order: Arc<Vec<u32>>,
}

/// A freshly generated triangulation with its mesh, BVH and edge list already
/// built — the owned, `Send` output of the worker half of a background compute
/// job (include/cut/create). The UI thread turns this into an
/// `OpenTriangulation` via `insert_generated_triangulation`.
pub(crate) struct GeneratedTriangulation {
    pub(crate) name: String,
    pub(crate) mesh: Arc<mesh_data::Triangulation>,
    pub(crate) spatial: Arc<crate::model::spatial::TriangleBvh>,
    pub(crate) edges: Vec<[u32; 2]>,
    pub(crate) surface_face_order: Arc<Vec<u32>>,
    pub(crate) surface_type: crate::ui::state::TriSurfaceType,
}

/// A `GeneratedTriangulation` paired with the single completion log line to
/// emit when it is applied. The common shape for backgrounded cut/create jobs.
pub(crate) struct GeneratedTriangulationLog {
    pub(crate) generated: GeneratedTriangulation,
    pub(crate) message: String,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenTriangulation {
    pub(crate) id: TriangulationId,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    /// Whether `path` points to a mesh that has been written to disk.
    pub(crate) is_saved: bool,
    pub(crate) mesh: Arc<mesh_data::Triangulation>,
    pub(crate) spatial: Arc<crate::model::spatial::TriangleBvh>,
    pub(crate) edges: Vec<[u32; 2]>,
    /// Face indices ordered by XY-Morton code (see `morton_surface_face_order`),
    /// precomputed off-thread so GPU surface chunking never sorts on the render
    /// thread. `Arc` so cloning an `OpenTriangulation` stays cheap.
    pub(crate) surface_face_order: Arc<Vec<u32>>,
    pub(crate) visible: bool,
    pub(crate) color: [f32; 4],
    pub(crate) line_color: [f32; 4],
    pub(crate) line_weight: Option<f32>,
    /// Optional georeferenced raster draped over this surface in world XY.
    pub(crate) raster_texture: Option<crate::model::raster::RasterTextureId>,
    pub(crate) raster_opacity: f32,
}

impl OpenTriangulation {
    pub(crate) fn entity_id(&self) -> crate::model::SceneEntityId {
        crate::model::SceneEntityId::Triangulation(self.id)
    }
}

/// Face indices ordered by the 2D (XY) Morton (Z-order) code of each face
/// centroid, so GPU surface chunks are spatially compact (tight AABBs for
/// frustum culling). XY-only because topologies/solids are viewed from above:
/// partitioning the map and letting each chunk own its vertical column culls
/// far better than 3D height bands you rarely look at edge-on.
///
/// Computed at mesh build/load time — off the render thread — and stored on
/// `OpenTriangulation`, so the first GPU upload of a huge mesh doesn't sort
/// millions of faces during a frame.
pub(crate) fn morton_surface_face_order(mesh: &mesh_data::Triangulation) -> Vec<u32> {
    use rayon::prelude::*;

    let face_count = mesh.face_count();
    if face_count == 0 {
        return Vec::new();
    }
    let vertices = mesh.vertices();
    let bounds = mesh.bounds();
    let scale = u32::MAX as f64;
    let (min_x, min_y) = (bounds.min.x, bounds.min.y);
    let extent_x = (bounds.max.x - bounds.min.x).max(1e-9);
    let extent_y = (bounds.max.y - bounds.min.y).max(1e-9);

    let mut keyed: Vec<(u64, u32)> = (0..face_count)
        .into_par_iter()
        .map(|face_index| {
            let face = mesh.face_vertex_indices(face_index).unwrap_or([0, 0, 0]);
            let mut cx = 0.0;
            let mut cy = 0.0;
            for vertex_index in face {
                let point = vertices[vertex_index];
                cx += point.x;
                cy += point.y;
            }
            cx /= 3.0;
            cy /= 3.0;
            let qx = (((cx - min_x) / extent_x) * scale).clamp(0.0, scale) as u32;
            let qy = (((cy - min_y) / extent_y) * scale).clamp(0.0, scale) as u32;
            (morton2(qx, qy), face_index as u32)
        })
        .collect();
    keyed.par_sort_unstable_by_key(|&(key, _)| key);
    keyed.into_iter().map(|(_, face_index)| face_index).collect()
}

/// Interleave the 32 bits of two coordinates into a 64-bit Morton code.
fn morton2(x: u32, y: u32) -> u64 {
    spread_bits_32(x) | (spread_bits_32(y) << 1)
}

/// Spread the 32 bits of `n` so each occupies every other bit position.
fn spread_bits_32(n: u32) -> u64 {
    let mut n = n as u64;
    n = (n | n << 16) & 0x0000_ffff_0000_ffff;
    n = (n | n << 8) & 0x00ff_00ff_00ff_00ff;
    n = (n | n << 4) & 0x0f0f_0f0f_0f0f_0f0f;
    n = (n | n << 2) & 0x3333_3333_3333_3333;
    n = (n | n << 1) & 0x5555_5555_5555_5555;
    n
}

pub(crate) fn unique_edges(mesh: &mesh_data::Triangulation) -> Vec<[u32; 2]> {
    use rayon::prelude::*;

    let mut packed: Vec<u64> = mesh
        .face_vertex_indices_iter()
        .flat_map(|face| {
            [(face[0], face[1]), (face[1], face[2]), (face[2], face[0])].map(|(a, b)| {
                let (a, b) = if a <= b { (a as u32, b as u32) } else { (b as u32, a as u32) };
                (u64::from(a) << 32) | u64::from(b)
            })
        })
        .collect();

    packed.par_sort_unstable();
    packed.dedup();

    packed.into_iter().map(|e| [(e >> 32) as u32, e as u32]).collect()
}
