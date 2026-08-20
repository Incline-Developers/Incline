//! Persistent GPU representation of immutable and infrequently-changing scene assets.

use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    sync::mpsc,
};

use glam::{DVec3, Vec3};
use wgpu::util::DeviceExt;

use crate::{
    model::{
        block_model::{BlockModelId, BlockModelSlice, ColorTransferFunction, MAX_COLOR_STOPS, OpenBlockModel, color_variable_default},
        formats::mesh_data,
        raster::RasterTextureId,
        triangulation::{OpenTriangulation, TriangulationId},
    },
    rendering::{BlockInstance, SurfaceVertex},
    ui::state::EditorState,
};

const YELLOW_HIGHLIGHT_COLOR: [f32; 4] = [1.0, 0.85, 0.0, 1.0];
const MAX_SURFACE_CHUNK_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const FALLBACK_BLOCK_GRADE: f32 = -1.0;
const HIDDEN_BLOCK_GRADE: f32 = -2.0;
/// Grades below this are discarded by `block_model.wgsl` (`grade < -1.5`).
/// Geometry building must treat such blocks as absent — a discarded block
/// leaves a hole, so it can't be allowed to cull its neighbours' faces.
use super::block_model_ramp::{VISIBLE_ALPHA_EPSILON, is_hidden_block_appearance, is_hidden_block_grade, make_translucent, ramp_alpha};

#[derive(Clone, Copy)]
enum BlockSurfaceSelection {
    All,
    OpaqueOnly,
    TransparentOnly,
}

pub(crate) struct CachedTriangulationGpu {
    pub(crate) surface_chunks: Vec<CachedSurfaceChunk>,
    pub(crate) surface_style_buffer: wgpu::Buffer,
    pub(crate) surface_style_bind_group: wgpu::BindGroup,
    /// Cached computed colour; used for transparency sorting and dirty-checking.
    pub(crate) color: [f32; 4],
    pub(crate) raster_texture: Option<RasterTextureId>,
    pub(crate) raster_opacity: f32,
    pub(crate) edge_chunks: Vec<CachedEdgeChunk>,
    pub(crate) edge_style_buffer: wgpu::Buffer,
    pub(crate) edge_style_bind_group: wgpu::BindGroup,
    line_color: [f32; 4],
    pub(crate) edge_width: f32,
}

pub(crate) struct CachedSurfaceChunk {
    pub(crate) vertex_buffer: wgpu::Buffer,
    pub(crate) index_buffer: wgpu::Buffer,
    pub(crate) index_count: u32,
    /// Scene-origin-relative AABB, for per-chunk frustum culling. Vertex
    /// positions are stored relative to the chunk's own AABB centre instead
    /// (small magnitudes keep f32 interpolation precise far from the scene
    /// origin); the shader re-adds the scene-relative centre from the chunk
    /// uniform in `chunk_bind_group`.
    pub(crate) bounds_min: Vec3,
    pub(crate) bounds_max: Vec3,
    /// Uniform bind group (group 2) holding this chunk's scene-origin-relative
    /// rebase offset; the bind group keeps the buffer alive.
    pub(crate) chunk_bind_group: wgpu::BindGroup,
    /// A `SurfaceStyle` bind group holding a distinct per-chunk debug colour,
    /// bound instead of the mesh colour when the chunk-debug view is on.
    pub(crate) debug_style_bind_group: wgpu::BindGroup,
}

pub(crate) struct CachedEdgeChunk {
    pub(crate) instance_buffer: wgpu::Buffer,
    pub(crate) instance_count: u32,
}

/// Per-instance edge geometry — position only. Color and width live in EdgeStyleUniform.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct EdgeInstance {
    pub(crate) start: [f32; 3],
    pub(crate) end: [f32; 3],
}

/// Per-triangulation surface colour uniform. Updated cheaply when colour changes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SurfaceStyleUniform {
    color: [f32; 4],
    params: [f32; 4],
}

/// Mirrors `ColorStop` for upload; `pos.x` holds the stop's `t`, the rest is
/// padding to keep the array's stride 16-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorStopUniform {
    color: [f32; 4],
    pos: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlockModelStyleUniform {
    fallback_color: [f32; 4],
    options: [f32; 4],
    /// Local->scene rotation as three column vectors (`.xyz` used), and the
    /// scene-relative translation. The block-model shader expands each
    /// instance's local bounds with `rotation * local + translation`. Field
    /// order and vec4 padding must match `BlockModelStyle` in the WGSL.
    rotation_0: [f32; 4],
    rotation_1: [f32; 4],
    rotation_2: [f32; 4],
    translation: [f32; 4],
    /// Slice clip box in the instance coordinate frame (local bounds offset
    /// by `local_ref`, matching `build_block_model_surface_chunks`). Boundary
    /// blocks are clamped and outside blocks collapsed by the vertex shader.
    /// `±f32::MAX` when no slice is active.
    clip_min: [f32; 4],
    clip_max: [f32; 4],
    stops: [ColorStopUniform; MAX_COLOR_STOPS],
}

/// Per-triangulation edge style uniform. Updated cheaply when colour/width changes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeStyleUniform {
    color: [f32; 4],
    width: f32,
    _pad: [f32; 3],
}

#[derive(Default)]
pub(crate) struct TriangulationGpuCache {
    meshes: HashMap<TriangulationId, CachedTriangulationGpu>,
}

#[derive(Default)]
pub(crate) struct BlockModelGpuCache {
    models: HashMap<BlockModelId, CachedBlockModelGpu>,
    pending_volume_builds: HashMap<BlockModelId, PendingVolumeBuild>,
    prepared_volumes: HashMap<BlockModelId, PreparedVolume>,
    pending_surface_builds: HashMap<BlockModelId, PendingSurfaceBuild>,
    prepared_surface_chunks: HashMap<BlockModelId, PreparedSurfaceChunks>,
}

struct PendingVolumeBuild {
    key: u64,
    receiver: mpsc::Receiver<anyhow::Result<Option<BlockVolumeAsset>>>,
}

struct PreparedVolume {
    key: u64,
    asset: Option<BlockVolumeAsset>,
}

struct PendingSurfaceBuild {
    key: u64,
    receiver: mpsc::Receiver<anyhow::Result<SurfaceChunkSets>>,
}

struct PreparedSurfaceChunks {
    key: u64,
    chunks: SurfaceChunkSets,
}

/// CPU-side output of one background surface build: the depth-writing and
/// transparent chunk sets, ready to upload on the render thread.
#[derive(Default)]
struct SurfaceChunkSets {
    opaque: Vec<SurfaceChunkCpu>,
    transparent: Vec<SurfaceChunkCpu>,
}

/// One chunk's instances built off-thread; `upload_surface_chunks` turns it
/// into a [`CachedBlockModelSurfaceChunk`].
struct SurfaceChunkCpu {
    instances: Vec<BlockInstance>,
    block_indices: Vec<usize>,
    bounds_min: Vec3,
    bounds_max: Vec3,
}

pub(crate) struct CachedBlockModelGpu {
    /// Depth-writing chunks: all chunks for an opaque model, or only the
    /// alpha-opaque grade subset for a mixed-alpha ramp.
    pub(crate) surface_chunks: Vec<CachedBlockModelSurfaceChunk>,
    /// Order-independent transparent chunks. Empty unless the whole model is
    /// translucent or the active ramp has genuinely partial-alpha grades.
    pub(crate) transparent_surface_chunks: Vec<CachedBlockModelSurfaceChunk>,
    /// Solid-volume raycast representation used for transparent/mixed-alpha
    /// block models. When present it replaces both the opaque and transparent
    /// cube draws for this model, preserving front-to-back volumetric opacity.
    pub(crate) volume: Option<CachedBlockVolumeGpu>,
    volume_asset_key: Option<u64>,
    pub(crate) surface_style_buffer: wgpu::Buffer,
    pub(crate) surface_style_bind_group: wgpu::BindGroup,
    pub(crate) translucent: bool,
    force_translucent: bool,
    pub(crate) edge_chunks: Vec<CachedEdgeChunk>,
    pub(crate) edge_style_buffer: wgpu::Buffer,
    pub(crate) edge_style_bind_group: wgpu::BindGroup,
    line_color: [f32; 4],
    edge_width: f32,
    variable: Option<String>,
    color_transfer: ColorTransferFunction,
    hide_empty_color_values: bool,
    boundary_highlights: bool,
    /// See [`hidden_blocks_fingerprint`]: geometry was built for this set of
    /// shader-hidden blocks; when it changes, recolor-in-place is not enough.
    hidden_fingerprint: u64,
    /// [`surface_chunks_key`] of the inputs the current surface chunks were
    /// built from, or `None` while none are built (new model, or a volume
    /// renders the model). A mismatch with the current inputs schedules a
    /// background rebuild; the stale chunks keep drawing until it lands.
    surface_key: Option<u64>,
    scene_origin: DVec3,
    rotation: glam::DMat3,
    visible: bool,
    fallback_color: [f32; 4],
    /// Active crop in the surface-instance coordinate frame; also used by
    /// depth queries so clipped-away cubes do not occlude document picks.
    slice: Option<BlockModelSlice>,
}

pub(crate) use super::block_model_volume_cache::{
    BlockVolumeAsset, CachedBlockVolumeGpu, apply_scene_to_local, build_block_volume_asset, clear_volume_feedback, poll_volume_feedback as poll_block_volume_feedback,
    schedule_volume_feedback, stream_volume_bricks, update_block_volume_slice, update_block_volume_style, upload_block_volume_gpu,
};

/// A block model surface chunk plus enough CPU-side state to re-colour it
/// (on an active-variable/legend switch) without re-walking blocks, redoing
/// face culling, or reallocating GPU buffers — only geometry changes
/// (translucency toggling, or the block model itself changing) need a full
/// rebuild.
pub(crate) struct CachedBlockModelSurfaceChunk {
    pub(crate) gpu: CachedBlockModelChunkGpu,
    /// Mirrors the instance buffer's current contents so `grade` can be
    /// patched in place and re-uploaded with a single `write_buffer`.
    cpu_instances: Vec<BlockInstance>,
    /// Source block index for each instance, parallel to `cpu_instances`, so
    /// recolour can recompute each block's grade without re-deriving geometry.
    block_indices: Vec<usize>,
}

/// GPU handles for one instanced block-model chunk. Unlike a triangulation's
/// [`CachedSurfaceChunk`] there is no index buffer: each block is a single
/// 32-byte instance the shader expands into a cube via a non-indexed draw.
pub(crate) struct CachedBlockModelChunkGpu {
    pub(crate) instance_buffer: wgpu::Buffer,
    pub(crate) instance_count: u32,
    /// Scene-relative bounds of this chunk's blocks, for frustum culling and
    /// transparency depth sorting.
    pub(crate) bounds_min: Vec3,
    pub(crate) bounds_max: Vec3,
}

fn volume_bounds_visible_in_primary_view(model_visible: bool, editor_hidden: bool, bounds: Option<(Vec3, Vec3)>, frustum: &crate::rendering::graphics::frustum::Frustum) -> bool {
    model_visible && !editor_hidden && bounds.is_some_and(|(min, max)| frustum.intersects_aabb(min, max))
}

impl BlockModelGpuCache {
    pub(crate) fn clear(&mut self) {
        self.models.clear();
        self.pending_volume_builds.clear();
        self.prepared_volumes.clear();
        self.pending_surface_builds.clear();
        self.prepared_surface_chunks.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    pub(crate) fn has_pending_builds(&self) -> bool {
        !self.pending_volume_builds.is_empty() || !self.pending_surface_builds.is_empty()
    }

    pub(crate) fn has_visible_pending_streaming(&self, frustum: &crate::rendering::graphics::frustum::Frustum, hidden: &HashSet<crate::model::SceneEntityId>) -> bool {
        self.models
            .iter()
            .any(|(&id, cached)| Self::volume_visible_in(id, cached, frustum, hidden) && cached.volume.as_ref().is_some_and(CachedBlockVolumeGpu::has_pending_streaming))
    }

    pub(crate) fn has_pending_feedback(&self) -> bool {
        self.models
            .values()
            .any(|cached| cached.volume.as_ref().is_some_and(CachedBlockVolumeGpu::has_pending_feedback))
    }

    /// Drain completed feedback receivers for every volume. This is CPU-only
    /// and must include volumes that moved off-screen after scheduling a GPU
    /// readback; residency uploads remain visibility-gated separately.
    pub(crate) fn poll_volume_feedback(&mut self) {
        for cached in self.models.values_mut() {
            if let Some(volume) = cached.volume.as_mut() {
                poll_block_volume_feedback(volume);
            }
        }
    }

    fn poll_surface_builds(&mut self) {
        let pending = std::mem::take(&mut self.pending_surface_builds);
        for (id, build) in pending {
            match build.receiver.try_recv() {
                Ok(Ok(chunks)) => {
                    self.prepared_surface_chunks.insert(id, PreparedSurfaceChunks { key: build.key, chunks });
                }
                Ok(Err(error)) => {
                    log::warn!("Block-model surface build failed: {error:#}");
                    // Store an empty result under the key so the model draws
                    // nothing rather than rescheduling the failing build
                    // every frame.
                    self.prepared_surface_chunks.insert(
                        id,
                        PreparedSurfaceChunks {
                            key: build.key,
                            chunks: SurfaceChunkSets::default(),
                        },
                    );
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending_surface_builds.insert(id, build);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::warn!("Block-model surface build worker disconnected");
                }
            }
        }
    }

    fn poll_volume_builds(&mut self) {
        let pending = std::mem::take(&mut self.pending_volume_builds);
        for (id, build) in pending {
            match build.receiver.try_recv() {
                Ok(Ok(asset)) => {
                    self.prepared_volumes.insert(id, PreparedVolume { key: build.key, asset });
                }
                Ok(Err(error)) => {
                    crate::userspace_warn!("Translucent volume could not be built ({error:#}); showing this block model as cubes instead.");
                    self.prepared_volumes.insert(id, PreparedVolume { key: build.key, asset: None });
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending_volume_builds.insert(id, build);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::warn!("Block-volume preparation worker disconnected");
                }
            }
        }
    }

    fn schedule_volume_builds(&mut self, block_models: &[OpenBlockModel], editor: &EditorState) {
        for block_model in block_models {
            let entity = block_model.entity_id();
            let translucent = editor.translucent_handles.contains(&entity) || block_model_has_partial_alpha_stops(block_model);
            if !block_model_volume_preferred(block_model, translucent, editor.slice_mode_enabled) || !block_model.active_values_available_for_render() {
                continue;
            }
            let key = volume_asset_key(block_model);
            let already_current = self.models.get(&block_model.id).is_some_and(|cached| cached.volume_asset_key == Some(key));
            // Keep at most one CPU volume build per model. If a style drag
            // changes the key while work is running, let that worker settle,
            // discard its stale result, then build only the latest snapshot.
            // Dropping a receiver alone would leave every superseded task
            // queued on the bounded pool.
            let already_pending = self.pending_volume_builds.contains_key(&block_model.id);
            let already_prepared = self.prepared_volumes.get(&block_model.id).is_some_and(|prepared| prepared.key == key);
            if already_current || already_pending || already_prepared {
                continue;
            }
            self.pending_volume_builds.remove(&block_model.id);
            self.prepared_volumes.remove(&block_model.id);
            let snapshot = block_model.clone();
            let (sender, receiver) = mpsc::channel();
            crate::app::jobs::spawn_pool_task(move || {
                let result = crate::app::jobs::run_compute_catching_panic(|| build_block_volume_asset(&snapshot).map_err(anyhow::Error::msg));
                let _ = sender.send(result);
            });
            self.pending_volume_builds.insert(block_model.id, PendingVolumeBuild { key, receiver });
        }
    }

    /// Advance cell-pool streaming for one volume already proven visible in
    /// the primary viewport. Off-screen volumes retain their current resident
    /// set and aggregates without queue writes or residency replans.
    pub(crate) fn stream_volume(&mut self, queue: &wgpu::Queue, camera_scene: Vec3, id: BlockModelId) {
        let Some(volume) = self.models.get_mut(&id).and_then(|cached| cached.volume.as_mut()) else {
            return;
        };
        let camera_local = apply_scene_to_local(&volume.scene_to_local, camera_scene);
        stream_volume_bricks(queue, volume, camera_local);
    }

    fn volume_visible_in(
        id: BlockModelId,
        cached: &CachedBlockModelGpu,
        frustum: &crate::rendering::graphics::frustum::Frustum,
        hidden: &HashSet<crate::model::SceneEntityId>,
    ) -> bool {
        volume_bounds_visible_in_primary_view(
            cached.visible,
            hidden.contains(&crate::model::SceneEntityId::BlockModel(id)),
            cached.volume.as_ref().map(|volume| (volume.bounds_min, volume.bounds_max)),
            frustum,
        )
    }

    /// Clear usage feedback only for volumes that can contribute pixels to
    /// the primary viewport this frame. Clearing an off-screen volume used to
    /// force a pointless GPU clear and later readback every feedback cycle.
    pub(crate) fn clear_visible_volume_feedback(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        phase: u32,
        frustum: &crate::rendering::graphics::frustum::Frustum,
        hidden: &HashSet<crate::model::SceneEntityId>,
    ) {
        for (&id, cached) in &self.models {
            if Self::volume_visible_in(id, cached, frustum, hidden)
                && let Some(volume) = cached.volume.as_ref()
            {
                clear_volume_feedback(queue, encoder, volume, phase);
            }
        }
    }

    pub(crate) fn schedule_visible_volume_feedback(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frustum: &crate::rendering::graphics::frustum::Frustum,
        hidden: &HashSet<crate::model::SceneEntityId>,
    ) -> bool {
        let mut scheduled = false;
        for (&id, cached) in &mut self.models {
            if Self::volume_visible_in(id, cached, frustum, hidden)
                && let Some(volume) = cached.volume.as_mut()
            {
                scheduled |= schedule_volume_feedback(device, queue, volume);
            }
        }
        scheduled
    }

    pub(crate) fn get(&self, id: BlockModelId) -> Option<&CachedBlockModelGpu> {
        self.models.get(&id)
    }

    /// Nearest depth-writing block under a ray. The cached opaque instance
    /// stream is the exact subset rendered with depth writes, so this avoids
    /// scanning hidden/transparent blocks from the source model.
    pub(crate) fn nearest_opaque_hit(&self, ray_origin: DVec3, ray_direction: DVec3, hidden: &HashSet<crate::model::SceneEntityId>) -> Option<DVec3> {
        self.nearest_hit(ray_origin, ray_direction, hidden, None, false)
    }

    /// Nearest visible block under a ray, including translucent surface or
    /// volume blocks. Frozen models are excluded because this is an
    /// interaction query rather than an occlusion query.
    pub(crate) fn nearest_visible_hit(
        &self,
        ray_origin: DVec3,
        ray_direction: DVec3,
        hidden: &HashSet<crate::model::SceneEntityId>,
        frozen: &HashSet<crate::model::SceneEntityId>,
    ) -> Option<DVec3> {
        self.nearest_hit(ray_origin, ray_direction, hidden, Some(frozen), true)
    }

    fn nearest_hit(
        &self,
        ray_origin: DVec3,
        ray_direction: DVec3,
        hidden: &HashSet<crate::model::SceneEntityId>,
        frozen: Option<&HashSet<crate::model::SceneEntityId>>,
        include_transparent: bool,
    ) -> Option<DVec3> {
        let mut nearest = f64::INFINITY;
        for (&id, cached) in &self.models {
            let entity = crate::model::SceneEntityId::BlockModel(id);
            if !cached.visible || hidden.contains(&entity) || frozen.is_some_and(|set| set.contains(&entity)) {
                continue;
            }
            let inverse_rotation = cached.rotation.transpose();
            let local_origin = inverse_rotation * (ray_origin - cached.scene_origin);
            let local_direction = inverse_rotation * ray_direction;
            if let Some(volume) = cached.volume.as_ref() {
                let scene_origin = (ray_origin - cached.scene_origin).as_vec3();
                let scene_direction = ray_direction.as_vec3();
                let volume_origin = apply_scene_to_local(&volume.scene_to_local, scene_origin);
                let volume_direction = apply_scene_to_local(&volume.scene_to_local, scene_origin + scene_direction) - volume_origin;
                let volume_hit = if include_transparent {
                    volume.nearest_visible_cell_hit(volume_origin, volume_direction, &cached.color_transfer, cached.fallback_color[3])
                } else {
                    volume.nearest_opaque_cell_hit(volume_origin, volume_direction, &cached.color_transfer, cached.fallback_color[3])
                };
                if let Some(distance) = volume_hit {
                    nearest = nearest.min(f64::from(distance));
                }
            }
            let transparent_chunks = if include_transparent { cached.transparent_surface_chunks.as_slice() } else { &[] };
            for chunk in cached.surface_chunks.iter().chain(transparent_chunks) {
                let padding = DVec3::splat(1.0e-3);
                let chunk_min = cached.scene_origin + chunk.gpu.bounds_min.as_dvec3() - padding;
                let chunk_max = cached.scene_origin + chunk.gpu.bounds_max.as_dvec3() + padding;
                if ray_aabb_distance(ray_origin, ray_direction, chunk_min, chunk_max).is_none_or(|distance| distance >= nearest) {
                    continue;
                }
                for instance in &chunk.cpu_instances {
                    let mut lower = DVec3::from_array(instance.lower.map(f64::from));
                    let mut upper = DVec3::from_array(instance.upper.map(f64::from));
                    if let Some(slice) = cached.slice {
                        lower = lower.max(slice.min);
                        upper = upper.min(slice.max);
                        if lower.cmpge(upper).any() {
                            continue;
                        }
                    }
                    if let Some(distance) = ray_aabb_distance(local_origin, local_direction, lower, upper)
                        && distance < nearest
                    {
                        nearest = distance;
                    }
                }
            }
        }
        nearest.is_finite().then(|| ray_origin + ray_direction * nearest)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_origin: DVec3,
        scale_factor: f32,
        block_models: &[OpenBlockModel],
        editor: &EditorState,
        surface_style_layout: &wgpu::BindGroupLayout,
        volume_layout: &wgpu::BindGroupLayout,
        edge_style_layout: &wgpu::BindGroupLayout,
    ) {
        let loaded: HashSet<_> = block_models.iter().map(|model| model.id).collect();
        self.models.retain(|id, _| loaded.contains(id));
        self.pending_volume_builds.retain(|id, _| loaded.contains(id));
        self.prepared_volumes.retain(|id, _| loaded.contains(id));
        self.pending_surface_builds.retain(|id, _| loaded.contains(id));
        self.prepared_surface_chunks.retain(|id, _| loaded.contains(id));
        self.poll_volume_builds();
        self.poll_surface_builds();
        self.schedule_volume_builds(block_models, editor);
        for block_model in block_models {
            let entity = block_model.entity_id();
            let selected = editor.selected_handles.contains(&entity);
            let force_translucent = editor.translucent_handles.contains(&entity);
            let has_partial_alpha_stops = block_model_has_partial_alpha_stops(block_model);
            let translucent = force_translucent || has_partial_alpha_stops;
            let wants_volume = block_model_volume_preferred(block_model, translucent, editor.slice_mode_enabled);
            let desired_volume_key = wants_volume.then(|| volume_asset_key(block_model));
            let prepared_volume = desired_volume_key.and_then(|key| {
                self.prepared_volumes
                    .get(&block_model.id)
                    .is_some_and(|prepared| prepared.key == key)
                    .then(|| self.prepared_volumes.remove(&block_model.id))
                    .flatten()
            });
            let volume_ready = prepared_volume.is_some();
            let mut line_color = if selected { crate::ui::SELECTION_COLOR_F32 } else { [0.03, 0.05, 0.06, 1.0] };
            if translucent {
                make_translucent(&mut line_color);
            }
            let show_edges = selected || editor.topology_wireframes_enabled;
            let edge_width = if show_edges { scale_factor.max(1.0) } else { 0.0 };
            let boundary_highlights = editor.show_block_model_boundary_highlights;

            if let Some(cached) = self.models.get_mut(&block_model.id) {
                cached.visible = block_model.visible;
                let variable_dirty = cached.variable != block_model.active_color_variable;
                let fallback_color_dirty = cached.fallback_color != block_model.color;
                let geometry_dirty = cached.translucent != translucent || cached.force_translucent != force_translucent;
                let style_dirty = cached.color_transfer != block_model.color_transfer;
                let empty_visibility_dirty = cached.hide_empty_color_values != block_model.hide_empty_color_values;
                let boundary_highlights_dirty = cached.boundary_highlights != boundary_highlights;
                let scene_origin_dirty = cached.scene_origin != scene_origin;
                let slice = block_model_instance_slice(scene_origin, block_model);
                let slice_dirty = cached.slice != slice;
                // A completed volume build wakes the cubes->volume switch;
                // volume->cubes must also keep syncing while its replacement
                // surface chunks build in the background.
                let renderer_dirty = !wants_volume && cached.volume.is_some();
                let edge_geom_dirty = (cached.edge_width == 0.0) != (edge_width == 0.0);
                let edge_style_dirty = cached.line_color != line_color || cached.edge_width != edge_width;
                // The shader-hidden set (which also determines volume cell
                // occupancy) is needed by both the volume fast-path decision
                // and the chunk branches below, and walking it is O(blocks) —
                // compute it once, but only when something that can change it
                // is dirty (skip it for edge/scene-origin-only updates).
                let ramp_visibility_dirty = style_dirty && !same_ramp_visibility(&cached.color_transfer, &block_model.color_transfer);
                let hidden_fingerprint = if variable_dirty || empty_visibility_dirty || ramp_visibility_dirty {
                    hidden_blocks_fingerprint(block_model)
                } else {
                    cached.hidden_fingerprint
                };
                let desired_surface_key = surface_chunks_key(scene_origin, block_model, force_translucent, has_partial_alpha_stops, hidden_fingerprint);
                // Stale also covers an in-flight rebuild: it keeps this model
                // past the early-continue so a finished build gets applied
                // (and a superseded one rescheduled) even on quiet frames.
                let surface_stale = cached.volume.is_none() && cached.surface_key != Some(desired_surface_key);
                if !variable_dirty
                    && !geometry_dirty
                    && !style_dirty
                    && !fallback_color_dirty
                    && !empty_visibility_dirty
                    && !boundary_highlights_dirty
                    && !scene_origin_dirty
                    && !slice_dirty
                    && !renderer_dirty
                    && !edge_geom_dirty
                    && !edge_style_dirty
                    && !volume_ready
                    && !surface_stale
                {
                    continue;
                }
                // (Re)build the volume first. When it renders the model the
                // surface chunks below are dead weight, so `volume_present`
                // lets the scheduler skip building them. A colour-ramp-only
                // change that leaves occupancy unchanged takes the cheap
                // in-place restyle (recompute aggregates + stops uniform)
                // instead of a full rebuild + re-upload of the large buffers —
                // this is what keeps dragging a gradient stop smooth.
                let style_only = style_dirty && !variable_dirty && !geometry_dirty && !empty_visibility_dirty && !scene_origin_dirty;
                if let Some(prepared) = prepared_volume {
                    cached.volume = prepared
                        .asset
                        .and_then(|asset| upload_block_volume_gpu(device, queue, scene_origin, block_model, volume_layout, asset, boundary_highlights));
                    cached.volume_asset_key = Some(prepared.key);
                    cached.scene_origin = scene_origin;
                    if cached.volume.is_some() {
                        cached.surface_chunks.clear();
                        cached.transparent_surface_chunks.clear();
                    }
                } else if !wants_volume {
                    // Keep the old volume visible until cube chunks are ready
                    // below, avoiding a blank model during a manual switch.
                    if cached.volume.is_none() {
                        cached.volume_asset_key = None;
                    }
                } else if cached.volume_asset_key != desired_volume_key {
                    // A worker is preparing the new variable/visibility
                    // snapshot. Keep drawing the stale volume, restyled with
                    // the live ramp colours, until it lands: dropping it here
                    // would blank the model, since the fallback surface
                    // chunks also build asynchronously now. (Stale-until-
                    // replaced, same as the surface chunks.)
                    if let Some(volume) = cached.volume.as_mut() {
                        update_block_volume_style(queue, volume, scene_origin, block_model, boundary_highlights);
                    }
                } else if style_only && hidden_fingerprint == cached.hidden_fingerprint {
                    if let Some(volume) = cached.volume.as_mut() {
                        update_block_volume_style(queue, volume, scene_origin, block_model, boundary_highlights);
                    }
                } else if scene_origin_dirty {
                    if let Some(volume) = cached.volume.as_mut() {
                        update_block_volume_style(queue, volume, scene_origin, block_model, boundary_highlights);
                    }
                    cached.scene_origin = scene_origin;
                }
                let volume_present = cached.volume.is_some();
                if scene_origin_dirty && !geometry_dirty {
                    // Surface and edge chunks bake in scene-origin-relative
                    // positions, so an origin change invalidates them along
                    // with the volume; the surface-key mismatch below
                    // reschedules the chunks, the edges rebuild here. (Camera
                    // code currently clears the whole cache on origin
                    // changes, so this path is a backstop.)
                    cached.scene_origin = scene_origin;
                    if cached.edge_width > 0.0 {
                        cached.edge_chunks = build_block_model_edge_chunks(device, scene_origin, block_model);
                    }
                }
                if geometry_dirty {
                    // Translucency changes which pipeline/chunking the model
                    // draws with, so the chunks fully rebuild (via the
                    // surface-key mismatch below).
                    let style = block_model_style(scene_origin, block_model, force_translucent);
                    queue.write_buffer(&cached.surface_style_buffer, 0, bytemuck::bytes_of(&style));
                    cached.variable = block_model.active_color_variable.clone();
                    cached.color_transfer = block_model.color_transfer.clone();
                    cached.hide_empty_color_values = block_model.hide_empty_color_values;
                    cached.hidden_fingerprint = hidden_fingerprint;
                    cached.translucent = translucent;
                    cached.force_translucent = force_translucent;
                } else if variable_dirty || empty_visibility_dirty {
                    if hidden_fingerprint == cached.hidden_fingerprint && !volume_present {
                        // The attribute/legend switch hides the same set of
                        // blocks, so which blocks and faces render is
                        // unchanged — only re-colour, without re-walking
                        // blocks or reallocating GPU buffers. Only reached
                        // when no volume represents the model.
                        recolor_block_model_surface_chunks(queue, block_model, &mut cached.surface_chunks);
                        recolor_block_model_surface_chunks(queue, block_model, &mut cached.transparent_surface_chunks);
                    } else {
                        // The set of shader-hidden blocks or the alpha class
                        // changed: skipped blocks, exposed-face culling, and
                        // opaque/transparent routing are stale. The chunks
                        // rebuild via the surface-key mismatch below; the
                        // stale ones keep drawing until the build lands.
                        if cached.edge_width > 0.0 {
                            cached.edge_chunks = build_block_model_edge_chunks(device, scene_origin, block_model);
                        }
                        cached.hidden_fingerprint = hidden_fingerprint;
                    }
                    if variable_dirty || style_dirty {
                        let style = block_model_style(scene_origin, block_model, force_translucent);
                        queue.write_buffer(&cached.surface_style_buffer, 0, bytemuck::bytes_of(&style));
                    }
                    cached.variable = block_model.active_color_variable.clone();
                    cached.color_transfer = block_model.color_transfer.clone();
                    cached.hide_empty_color_values = block_model.hide_empty_color_values;
                } else if style_dirty {
                    // The colour-transfer function changed. This can change
                    // which blocks are shader-hidden and which blocks are
                    // routed to the depth-writing vs transparent pass; the
                    // surface key captures both, so any geometry impact
                    // reschedules the chunks in the background. Here only the
                    // style uniform, edges, and fingerprint bookkeeping
                    // update — that is what keeps dragging a gradient stop
                    // smooth on large models.
                    if hidden_fingerprint != cached.hidden_fingerprint {
                        if cached.edge_width > 0.0 {
                            cached.edge_chunks = build_block_model_edge_chunks(device, scene_origin, block_model);
                        }
                        cached.hidden_fingerprint = hidden_fingerprint;
                    }
                    let style = block_model_style(scene_origin, block_model, force_translucent);
                    queue.write_buffer(&cached.surface_style_buffer, 0, bytemuck::bytes_of(&style));
                    cached.color_transfer = block_model.color_transfer.clone();
                }
                if fallback_color_dirty {
                    let style = block_model_style(scene_origin, block_model, force_translucent);
                    queue.write_buffer(&cached.surface_style_buffer, 0, bytemuck::bytes_of(&style));
                    if let Some(volume) = cached.volume.as_mut() {
                        update_block_volume_style(queue, volume, scene_origin, block_model, boundary_highlights);
                    }
                    cached.fallback_color = block_model.color;
                }
                if edge_geom_dirty {
                    cached.edge_chunks = if edge_width > 0.0 {
                        build_block_model_edge_chunks(device, scene_origin, block_model)
                    } else {
                        Vec::new()
                    };
                }
                if slice_dirty {
                    // Apply the new clip immediately. The surface-key path
                    // below also refreshes cropped cube occupancy in the
                    // background so opaque cut faces appear and outside
                    // instances disappear once the drag settles.
                    let style = block_model_style(scene_origin, block_model, force_translucent);
                    queue.write_buffer(&cached.surface_style_buffer, 0, bytemuck::bytes_of(&style));
                    if let Some(volume) = cached.volume.as_mut() {
                        update_block_volume_slice(queue, volume, scene_origin, block_model, boundary_highlights);
                    }
                    if edge_width > 0.0 && !edge_geom_dirty {
                        cached.edge_chunks = build_block_model_edge_chunks(device, scene_origin, block_model);
                    }
                    cached.slice = slice;
                }
                if boundary_highlights_dirty {
                    if !slice_dirty && let Some(volume) = cached.volume.as_mut() {
                        update_block_volume_slice(queue, volume, scene_origin, block_model, boundary_highlights);
                    }
                    cached.boundary_highlights = boundary_highlights;
                }
                if edge_style_dirty || edge_geom_dirty {
                    let style = EdgeStyleUniform {
                        color: line_color,
                        width: edge_width,
                        _pad: [0.0; 3],
                    };
                    queue.write_buffer(&cached.edge_style_buffer, 0, bytemuck::bytes_of(&style));
                    cached.line_color = line_color;
                    cached.edge_width = edge_width;
                }
                // Surface chunks build asynchronously on the job pool: apply
                // a finished build, then (re)schedule whenever the cached
                // chunks don't match the current inputs. Stale chunks keep
                // drawing until their replacement lands.
                if volume_present && !wants_volume {
                    let replacement_ready = self
                        .prepared_surface_chunks
                        .get(&block_model.id)
                        .is_some_and(|prepared| prepared.key == desired_surface_key);
                    if replacement_ready && let Some(prepared) = self.prepared_surface_chunks.remove(&block_model.id) {
                        cached.surface_chunks = upload_surface_chunks(device, prepared.chunks.opaque);
                        cached.transparent_surface_chunks = upload_surface_chunks(device, prepared.chunks.transparent);
                        cached.surface_key = Some(prepared.key);
                        cached.volume = None;
                        cached.volume_asset_key = None;
                    } else {
                        schedule_surface_build(
                            &mut self.pending_surface_builds,
                            &mut self.prepared_surface_chunks,
                            block_model,
                            scene_origin,
                            force_translucent,
                            has_partial_alpha_stops,
                            desired_surface_key,
                        );
                    }
                } else if volume_present {
                    // The volume renders the model; the chunks were cleared
                    // above and must rebuild if the volume later goes away.
                    cached.surface_key = None;
                } else {
                    if self
                        .prepared_surface_chunks
                        .get(&block_model.id)
                        .is_some_and(|prepared| prepared.key == desired_surface_key)
                        && let Some(prepared) = self.prepared_surface_chunks.remove(&block_model.id)
                    {
                        cached.surface_chunks = upload_surface_chunks(device, prepared.chunks.opaque);
                        cached.transparent_surface_chunks = upload_surface_chunks(device, prepared.chunks.transparent);
                        cached.surface_key = Some(prepared.key);
                    }
                    if cached.surface_key != Some(desired_surface_key) {
                        schedule_surface_build(
                            &mut self.pending_surface_builds,
                            &mut self.prepared_surface_chunks,
                            block_model,
                            scene_origin,
                            force_translucent,
                            has_partial_alpha_stops,
                            desired_surface_key,
                        );
                    }
                }
            } else {
                // Build the volume first so the scheduler can skip the
                // surface chunks (never drawn when a volume renders the
                // model).
                let (volume, volume_asset_key) = prepared_volume.map_or((None, None), |prepared| {
                    let key = prepared.key;
                    let volume = prepared
                        .asset
                        .and_then(|asset| upload_block_volume_gpu(device, queue, scene_origin, block_model, volume_layout, asset, boundary_highlights));
                    (volume, Some(key))
                });
                let hidden_fingerprint = hidden_blocks_fingerprint(block_model);
                if volume.is_none() {
                    schedule_surface_build(
                        &mut self.pending_surface_builds,
                        &mut self.prepared_surface_chunks,
                        block_model,
                        scene_origin,
                        force_translucent,
                        has_partial_alpha_stops,
                        surface_chunks_key(scene_origin, block_model, force_translucent, has_partial_alpha_stops, hidden_fingerprint),
                    );
                }
                let surface_style = block_model_style(scene_origin, block_model, force_translucent);
                let surface_style_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Block Model Surface Style Uniform"),
                    contents: bytemuck::bytes_of(&surface_style),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                let surface_style_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    layout: surface_style_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: surface_style_buffer.as_entire_binding(),
                    }],
                    label: Some("Block Model Surface Style Bind Group"),
                });
                let edge_chunks = if edge_width > 0.0 {
                    build_block_model_edge_chunks(device, scene_origin, block_model)
                } else {
                    Vec::new()
                };
                let edge_style = EdgeStyleUniform {
                    color: line_color,
                    width: edge_width,
                    _pad: [0.0; 3],
                };
                let edge_style_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Block Model Edge Style Uniform"),
                    contents: bytemuck::bytes_of(&edge_style),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                let edge_style_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    layout: edge_style_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: edge_style_buffer.as_entire_binding(),
                    }],
                    label: Some("Block Model Edge Style Bind Group"),
                });
                self.models.insert(
                    block_model.id,
                    CachedBlockModelGpu {
                        surface_chunks: Vec::new(),
                        transparent_surface_chunks: Vec::new(),
                        volume,
                        volume_asset_key,
                        surface_key: None,
                        surface_style_buffer,
                        surface_style_bind_group,
                        translucent,
                        force_translucent,
                        edge_chunks,
                        edge_style_buffer,
                        edge_style_bind_group,
                        line_color,
                        edge_width,
                        variable: block_model.active_color_variable.clone(),
                        color_transfer: block_model.color_transfer.clone(),
                        hide_empty_color_values: block_model.hide_empty_color_values,
                        boundary_highlights,
                        hidden_fingerprint,
                        scene_origin,
                        rotation: block_model.model.rotation(),
                        visible: block_model.visible,
                        fallback_color: block_model.color,
                        slice: block_model_instance_slice(scene_origin, block_model),
                    },
                );
            }
        }
    }
}

fn ray_aabb_distance(origin: DVec3, direction: DVec3, min: DVec3, max: DVec3) -> Option<f64> {
    let mut near = 0.0_f64;
    let mut far = f64::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() <= f64::EPSILON {
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
            continue;
        }
        let inverse = direction[axis].recip();
        let first = (min[axis] - origin[axis]) * inverse;
        let second = (max[axis] - origin[axis]) * inverse;
        near = near.max(first.min(second));
        far = far.min(first.max(second));
        if near > far {
            return None;
        }
    }
    Some(near)
}

/// Hash of every input that changes the *contents* of the surface chunks
/// (which blocks are drawn, face culling, opaque/transparent routing, baked
/// scene-relative positions). Deliberately excluded: the active variable and
/// ramp colours for opaque models — those only change per-instance grades and
/// the style uniform, which the recolor-in-place path and a uniform write
/// handle without rebuilding geometry. Grade routing does depend on them for
/// translucent/partial-alpha models, so they are keyed in that case.
fn surface_chunks_key(scene_origin: DVec3, block_model: &OpenBlockModel, force_translucent: bool, has_partial_alpha_stops: bool, hidden_fingerprint: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    scene_origin.x.to_bits().hash(&mut hasher);
    scene_origin.y.to_bits().hash(&mut hasher);
    scene_origin.z.to_bits().hash(&mut hasher);
    force_translucent.hash(&mut hasher);
    has_partial_alpha_stops.hash(&mut hasher);
    hidden_fingerprint.hash(&mut hasher);
    // Opaque cubes cull fully enclosed blocks. Cropping can expose a formerly
    // enclosed block at a new cut face, so the cropped occupancy belongs in
    // the geometry key (the old chunks remain clipped while this rebuilds).
    if let Some(slice) = block_model.active_slice() {
        for value in slice.min.to_array().into_iter().chain(slice.max.to_array()) {
            value.to_bits().hash(&mut hasher);
        }
    }
    // Grades feed the chunk-set routing (and are baked per instance with no
    // recolor path) whenever the model draws translucently. Alpha
    // classification thresholds route blocks between the depth-writing and
    // transparent sets; exact alpha values and RGB never affect geometry.
    // For opaque models the ramp's only geometric influence is the hidden
    // set, which the fingerprint above already captures — keying the stops
    // there would rebuild on every stop-position drag for nothing.
    if force_translucent || has_partial_alpha_stops {
        block_model.active_color_variable.hash(&mut hasher);
        block_model.hide_empty_color_values.hash(&mut hasher);
        (block_model.color[3] >= 0.98).hash(&mut hasher);
        for stop in &block_model.color_transfer.stops {
            stop.t.to_bits().hash(&mut hasher);
            (stop.color[3] >= VISIBLE_ALPHA_EPSILON).hash(&mut hasher);
            (stop.color[3] >= 0.98).hash(&mut hasher);
        }
    }
    // A variable whose values are still decoding builds fallback-grade
    // chunks; the decode landing must produce a rebuild even though no other
    // input changed.
    block_model.active_values_available_for_render().hash(&mut hasher);
    hasher.finish()
}

/// Queue a background CPU build of the model's surface chunk sets, unless a
/// build is already running (let it settle and discard its stale result
/// rather than queueing superseded work on the bounded pool) or the wanted
/// result is already prepared.
fn schedule_surface_build(
    pending: &mut HashMap<BlockModelId, PendingSurfaceBuild>,
    prepared: &mut HashMap<BlockModelId, PreparedSurfaceChunks>,
    block_model: &OpenBlockModel,
    scene_origin: DVec3,
    force_translucent: bool,
    has_partial_alpha_stops: bool,
    key: u64,
) {
    if pending.contains_key(&block_model.id) || prepared.get(&block_model.id).is_some_and(|ready| ready.key == key) {
        return;
    }
    prepared.remove(&block_model.id);
    let snapshot = block_model.clone();
    let (sender, receiver) = mpsc::channel();
    crate::app::jobs::spawn_pool_task(move || {
        let result = crate::app::jobs::run_compute_catching_panic(|| Ok(build_block_model_surface_chunk_sets(scene_origin, &snapshot, force_translucent, has_partial_alpha_stops)));
        let _ = sender.send(result);
    });
    pending.insert(block_model.id, PendingSurfaceBuild { key, receiver });
}

fn upload_surface_chunks(device: &wgpu::Device, chunks: Vec<SurfaceChunkCpu>) -> Vec<CachedBlockModelSurfaceChunk> {
    chunks
        .into_iter()
        .filter_map(|chunk| {
            upload_block_model_surface_chunk(device, &chunk.instances, chunk.bounds_min, chunk.bounds_max).map(|gpu| CachedBlockModelSurfaceChunk {
                gpu,
                cpu_instances: chunk.instances,
                block_indices: chunk.block_indices,
            })
        })
        .collect()
}

fn volume_asset_key(block_model: &OpenBlockModel) -> u64 {
    let mut hasher = DefaultHasher::new();
    block_model.active_color_variable.hash(&mut hasher);
    block_model.hide_empty_color_values.hash(&mut hasher);
    for stop in &block_model.color_transfer.stops {
        stop.t.to_bits().hash(&mut hasher);
        (stop.color[3] >= VISIBLE_ALPHA_EPSILON).hash(&mut hasher);
    }
    hasher.finish()
}

/// Decide whether a model should use the volume representation. Slice view
/// always needs it: fixed-function clipping can remove cube geometry at the
/// slab planes but cannot synthesize the filled cut faces through enclosed
/// blocks. Outside slice view the two cube paths use separate predictors:
/// translucent cubes retain every block, while opaque cubes remove blocks
/// enclosed by six neighbours and write depth.
fn block_model_volume_preferred(block_model: &OpenBlockModel, translucent: bool, slice_view: bool) -> bool {
    slice_view || block_model_auto_prefers_volume(block_model, translucent)
}

/// Calibrated automatic renderer selection for a block model.
fn block_model_auto_prefers_volume(block_model: &OpenBlockModel, translucent: bool) -> bool {
    if block_model.model.metadata.is_irregular {
        return opaque_irregular_volume_preferred(block_model.opaque_surface_blocks, translucent);
    }
    let renderable_blocks = block_model.renderable_block_indices.len();
    let total_blocks = block_model.model.metadata.n_blocks;
    let dims = block_model.model.metadata.dims;
    if translucent {
        translucent_regular_grid_volume_preferred(renderable_blocks, total_blocks, dims)
    } else {
        block_model
            .opaque_surface_blocks
            .is_none_or(|surface_blocks| opaque_regular_grid_volume_preferred(surface_blocks, renderable_blocks, total_blocks, dims))
    }
}

fn opaque_irregular_volume_preferred(surface_blocks: Option<usize>, translucent: bool) -> bool {
    // Irregular opaque models generally favour the cube representation. Keep
    // Volume for translucent overlap semantics and when the loader's bounded
    // hash analysis declined a model that was too large to count safely;
    // otherwise choose Cubes.
    translucent || surface_blocks.is_none()
}

fn translucent_regular_grid_volume_preferred(renderable_blocks: usize, total_blocks: usize, dims: [usize; 3]) -> bool {
    const BLOCKS_PER_BRICK_AXIS: usize = 8;
    const CUBE_INSTANCES_PER_BRICK_DIAGONAL: f64 = 2_500.0;
    let Some(grid_cells) = dims.into_iter().try_fold(1usize, |product, dim| product.checked_mul(dim)) else {
        return true;
    };
    if grid_cells == 0 || grid_cells != total_blocks || renderable_blocks > grid_cells {
        return true;
    }
    if renderable_blocks == 0 {
        return false;
    }
    let brick_dims = dims.map(|dim| dim.div_ceil(BLOCKS_PER_BRICK_AXIS));
    let brick_diagonal = brick_dims.into_iter().map(|dim| (dim as f64).powi(2)).sum::<f64>().sqrt();
    let occupancy = renderable_blocks as f64 / grid_cells as f64;
    let opacity_discount = 1.0 - 0.5 * occupancy;
    let sparse_safety_margin = 1.0 + 0.25 * (1.0 - occupancy);
    let crossover = CUBE_INSTANCES_PER_BRICK_DIAGONAL * brick_diagonal * opacity_discount * sparse_safety_margin;
    renderable_blocks as f64 >= crossover
}

/// Opaque cube cost follows the instances left after whole-interior-block
/// culling, not the original block count. Volume cost followed the fitted
/// screen footprint better than the full 3D diagonal, so use the two smaller
/// brick-grid axes. Occupancy raises the crossover for dense connected grids,
/// where depth-writing cubes benefit most from shared-face culling.
fn opaque_regular_grid_volume_preferred(surface_blocks: usize, renderable_blocks: usize, total_blocks: usize, dims: [usize; 3]) -> bool {
    const BLOCKS_PER_BRICK_AXIS: usize = 8;
    const BASE_SURFACE_INSTANCES: f64 = 90_000.0;
    const INSTANCES_PER_MINOR_BRICK_DIAGONAL: f64 = 1_500.0;
    const DENSE_CONNECTED_MARGIN: f64 = 70_000.0;
    let Some(grid_cells) = dims.into_iter().try_fold(1usize, |product, dim| product.checked_mul(dim)) else {
        return true;
    };
    if grid_cells == 0 || grid_cells != total_blocks || renderable_blocks > grid_cells || surface_blocks > renderable_blocks {
        return true;
    }
    if renderable_blocks == 0 {
        return false;
    }
    let mut brick_dims = dims.map(|dim| dim.div_ceil(BLOCKS_PER_BRICK_AXIS));
    brick_dims.sort_unstable();
    let minor_diagonal = ((brick_dims[0] as f64).powi(2) + (brick_dims[1] as f64).powi(2)).sqrt();
    let occupancy = renderable_blocks as f64 / grid_cells as f64;
    let crossover = BASE_SURFACE_INSTANCES + INSTANCES_PER_MINOR_BRICK_DIAGONAL * minor_diagonal + DENSE_CONNECTED_MARGIN * occupancy;
    surface_blocks as f64 >= crossover
}

impl TriangulationGpuCache {
    pub(crate) fn clear(&mut self) {
        self.meshes.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }

    pub(crate) fn get(&self, id: TriangulationId) -> Option<&CachedTriangulationGpu> {
        self.meshes.get(&id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_origin: DVec3,
        scale_factor: f32,
        triangulations: &[OpenTriangulation],
        editor: &EditorState,
        surface_style_layout: &wgpu::BindGroupLayout,
        surface_chunk_layout: &wgpu::BindGroupLayout,
        edge_style_layout: &wgpu::BindGroupLayout,
    ) {
        let loaded: HashSet<_> = triangulations.iter().map(|tri| tri.id).collect();
        self.meshes.retain(|id, _| loaded.contains(id));
        for triangulation in triangulations {
            let entity = triangulation.entity_id();
            let selected = editor.selected_handles.contains(&entity);
            // Selection is represented by the highlighted wireframe below. A live
            // picker hover temporarily overrides the face colour as well so the
            // candidate remains obvious even when it was already selected.
            let mut color = if editor.tri_hover_handles.contains(&entity) {
                YELLOW_HIGHLIGHT_COLOR
            } else {
                triangulation.color
            };
            if editor.translucent_handles.contains(&entity) {
                make_translucent(&mut color);
            }
            let mut line_color = if selected { crate::ui::SELECTION_COLOR_F32 } else { triangulation.line_color };
            if editor.translucent_handles.contains(&entity) {
                make_translucent(&mut line_color);
            }
            // Selection always shows edges, even when global wireframes are off.
            let show_edges = selected || editor.topology_wireframes_enabled;
            let edge_width = if show_edges {
                (triangulation.line_weight.unwrap_or(1.0) * scale_factor).max(1.0)
            } else {
                0.0
            };

            if let Some(cached) = self.meshes.get_mut(&triangulation.id) {
                let surface_dirty = cached.color != color || cached.raster_texture != triangulation.raster_texture || cached.raster_opacity != triangulation.raster_opacity;
                // Rebuild edge geometry only when edges flip between present and absent.
                let edge_geom_dirty = (cached.edge_width == 0.0) != (edge_width == 0.0);
                let edge_style_dirty = cached.line_color != line_color || cached.edge_width != edge_width;

                if !surface_dirty && !edge_geom_dirty && !edge_style_dirty {
                    continue;
                }

                if surface_dirty {
                    let style = SurfaceStyleUniform {
                        color,
                        params: [
                            if triangulation.raster_texture.is_some() {
                                triangulation.raster_opacity.clamp(0.0, 1.0)
                            } else {
                                0.0
                            },
                            0.0,
                            0.0,
                            0.0,
                        ],
                    };
                    queue.write_buffer(&cached.surface_style_buffer, 0, bytemuck::bytes_of(&style));
                    cached.color = color;
                    cached.raster_texture = triangulation.raster_texture;
                    cached.raster_opacity = triangulation.raster_opacity;
                }

                if edge_geom_dirty && edge_width > 0.0 && cached.edge_chunks.is_empty() {
                    cached.edge_chunks = build_edge_chunks(device, scene_origin, triangulation);
                }

                if edge_style_dirty || edge_geom_dirty {
                    let style = EdgeStyleUniform {
                        color: line_color,
                        width: edge_width,
                        _pad: [0.0; 3],
                    };
                    queue.write_buffer(&cached.edge_style_buffer, 0, bytemuck::bytes_of(&style));
                    cached.line_color = line_color;
                    cached.edge_width = edge_width;
                }
            } else {
                let surface_chunks = build_surface_chunks(device, scene_origin, triangulation, surface_style_layout, surface_chunk_layout);
                if surface_chunks.is_empty() {
                    continue;
                }

                let surface_style = SurfaceStyleUniform {
                    color,
                    params: [
                        if triangulation.raster_texture.is_some() {
                            triangulation.raster_opacity.clamp(0.0, 1.0)
                        } else {
                            0.0
                        },
                        0.0,
                        0.0,
                        0.0,
                    ],
                };
                let surface_style_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Surface Style Uniform"),
                    contents: bytemuck::bytes_of(&surface_style),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                let surface_style_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    layout: surface_style_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: surface_style_buffer.as_entire_binding(),
                    }],
                    label: Some("Surface Style Bind Group"),
                });

                let edge_chunks = if edge_width > 0.0 {
                    build_edge_chunks(device, scene_origin, triangulation)
                } else {
                    Vec::new()
                };
                let edge_style = EdgeStyleUniform {
                    color: line_color,
                    width: edge_width,
                    _pad: [0.0; 3],
                };
                let edge_style_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Edge Style Uniform"),
                    contents: bytemuck::bytes_of(&edge_style),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                let edge_style_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    layout: edge_style_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: edge_style_buffer.as_entire_binding(),
                    }],
                    label: Some("Edge Style Bind Group"),
                });

                self.meshes.insert(
                    triangulation.id,
                    CachedTriangulationGpu {
                        surface_chunks,
                        surface_style_buffer,
                        surface_style_bind_group,
                        color,
                        raster_texture: triangulation.raster_texture,
                        raster_opacity: triangulation.raster_opacity,
                        edge_chunks,
                        edge_style_buffer,
                        edge_style_bind_group,
                        line_color,
                        edge_width,
                    },
                );
            }
        }
    }
}

/// Target triangles per spatial chunk. Chunks are the granularity of frustum
/// culling (and per-chunk debug colouring), so this trades culling precision
/// (smaller = tighter) against per-chunk draw-call/AABB-test overhead
/// (smaller = more). ~100k keeps a multi-million-face mesh in tens of chunks.
const TARGET_FACES_PER_CHUNK: usize = 100_000;

/// Upload indexed surface geometry as spatially-coherent chunks, walking faces
/// in the precomputed XY-Morton order (`triangulation.surface_face_order`,
/// built off-thread) so each chunk covers a compact region with a tight AABB
/// the renderer can frustum-cull — rather than the old size-only split whose
/// chunks each spanned the whole mesh. No sorting happens here, so a huge
/// mesh's first upload doesn't hitch the render thread. Colour is NOT baked in;
/// it lives in a per-draw SurfaceStyle uniform (plus a per-chunk debug-colour
/// uniform for the chunk-debug view).
fn build_surface_chunks(
    device: &wgpu::Device,
    scene_origin: DVec3,
    triangulation: &OpenTriangulation,
    surface_style_layout: &wgpu::BindGroupLayout,
    surface_chunk_layout: &wgpu::BindGroupLayout,
) -> Vec<CachedSurfaceChunk> {
    let mesh = &triangulation.mesh;
    let source = mesh.vertices();
    let face_count = mesh.face_count();
    if source.is_empty() || face_count == 0 || source.len() > u32::MAX as usize {
        if source.len() > u32::MAX as usize {
            log::error!("Triangulation '{}' has {} vertices (> u32::MAX); cannot chunk for GPU", triangulation.name, source.len());
        }
        return Vec::new();
    }
    let order = &triangulation.surface_face_order;

    // Chunk size: the target, capped so a chunk's vertex/index buffers fit the
    // device limit and local indices stay within u32.
    let limit = (device.limits().max_buffer_size as usize).min(MAX_SURFACE_CHUNK_BYTES);
    let max_indices = limit / size_of::<u32>();
    let max_vertices = limit / size_of::<SurfaceVertex>();
    let faces_cap = (max_indices / 3).min(max_vertices / 3).min(u32::MAX as usize / 3).max(1);
    let faces_per_chunk = TARGET_FACES_PER_CHUNK.min(faces_cap).max(1);

    let mut chunks = Vec::new();
    // Dense sentinel remap reused across chunks: remap[global] == u32::MAX means
    // "not yet in this chunk". Reset only touched entries between chunks.
    let mut remap = vec![u32::MAX; source.len()];
    let mut dirty: Vec<usize> = Vec::new();
    let mut vertices: Vec<SurfaceVertex> = Vec::new();
    // Flat shading takes the normal from the triangle's provoking (first)
    // vertex, so each face needs a first vertex no other face uses. Tracks,
    // per local vertex, whether a face has already claimed it.
    let mut provoking_claimed: Vec<bool> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for (chunk_index, run) in order.chunks(faces_per_chunk).enumerate() {
        vertices.clear();
        provoking_claimed.clear();
        indices.clear();

        // First pass: world-space AABB in f64. Its centre becomes the chunk's
        // local origin so vertex magnitudes stay small no matter how far the
        // chunk sits from the scene origin.
        let mut world_min = DVec3::splat(f64::INFINITY);
        let mut world_max = DVec3::splat(f64::NEG_INFINITY);
        for &face_index in run {
            let Some(face) = mesh.face_vertex_indices(face_index as usize) else {
                continue;
            };
            for global_index in face {
                if let Some(point) = source.get(global_index) {
                    let pos = DVec3::new(point.x, point.y, point.z);
                    world_min = world_min.min(pos);
                    world_max = world_max.max(pos);
                }
            }
        }
        if !world_min.x.is_finite() {
            continue;
        }
        let chunk_origin = (world_min + world_max) * 0.5;

        for &face_index in run {
            let Some(face) = mesh.face_vertex_indices(face_index as usize) else {
                continue;
            };
            let [Some(pa), Some(pb), Some(pc)] = face.map(|index| source.get(index).map(|point| DVec3::new(point.x, point.y, point.z))) else {
                continue;
            };
            // Flat face normal in f64; oriented z >= 0 to match the two-sided
            // surface lighting (previously done per-fragment in the shader).
            let mut face_normal = (pb - pa).cross(pc - pa).normalize_or_zero();
            if face_normal.z < 0.0 {
                face_normal = -face_normal;
            }
            let normal = face_normal.as_vec3().to_array();

            let mut local = [0u32; 3];
            for (slot, global_index) in face.into_iter().enumerate() {
                local[slot] = if remap[global_index] != u32::MAX {
                    remap[global_index]
                } else {
                    let point = source[global_index];
                    let index = vertices.len() as u32;
                    vertices.push(surface_vertex(point, chunk_origin, normal));
                    provoking_claimed.push(false);
                    remap[global_index] = index;
                    dirty.push(global_index);
                    index
                };
            }
            // Rotate the face (winding-preserving) so an unclaimed vertex
            // provokes it; duplicate one vertex only when all three are taken.
            match (0..3).find(|&i| !provoking_claimed[local[i] as usize]) {
                Some(i) => local.rotate_left(i),
                None => {
                    let duplicate = SurfaceVertex {
                        pos: vertices[local[0] as usize].pos,
                        normal,
                    };
                    local[0] = vertices.len() as u32;
                    vertices.push(duplicate);
                    provoking_claimed.push(false);
                }
            }
            let provoking = local[0] as usize;
            vertices[provoking].normal = normal;
            provoking_claimed[provoking] = true;
            indices.extend_from_slice(&local);
        }
        if let Some(chunk) = upload_surface_chunk(
            device,
            &vertices,
            &indices,
            (world_min - scene_origin).as_vec3(),
            (world_max - scene_origin).as_vec3(),
            (chunk_origin - scene_origin).as_vec3(),
            chunk_index,
            surface_style_layout,
            surface_chunk_layout,
        ) {
            chunks.push(chunk);
        }
        for &global_index in &dirty {
            remap[global_index] = u32::MAX;
        }
        dirty.clear();
    }
    if chunks.len() > 1 {
        log::info!("Triangulation '{}' uploaded in {} spatial chunks ({} faces)", triangulation.name, chunks.len(), face_count);
    }
    chunks
}

/// A distinct, well-spread debug colour per chunk index (golden-ratio hue).
fn chunk_debug_color(chunk_index: usize) -> [f32; 4] {
    let hue = (chunk_index as f32 * 0.618_034).fract();
    let [r, g, b] = hsv_to_rgb(hue, 0.65, 0.95);
    [r, g, b, 1.0]
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match (i as i32).rem_euclid(6) {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn surface_vertex(point: mesh_data::Vertex, chunk_origin: DVec3, normal: [f32; 3]) -> SurfaceVertex {
    let local = DVec3::new(point.x, point.y, point.z) - chunk_origin;
    SurfaceVertex {
        pos: local.as_vec3().to_array(),
        normal,
    }
}

#[allow(clippy::too_many_arguments)]
fn upload_surface_chunk(
    device: &wgpu::Device,
    vertices: &[SurfaceVertex],
    indices: &[u32],
    bounds_min: Vec3,
    bounds_max: Vec3,
    chunk_offset: Vec3,
    chunk_index: usize,
    surface_style_layout: &wgpu::BindGroupLayout,
    surface_chunk_layout: &wgpu::BindGroupLayout,
) -> Option<CachedSurfaceChunk> {
    if vertices.is_empty() || indices.is_empty() {
        return None;
    }
    let limit = device.limits().max_buffer_size;
    let vertex_bytes = size_of_val(vertices) as u64;
    let index_bytes = size_of_val(indices) as u64;
    if vertex_bytes > limit || index_bytes > limit {
        log::error!("Triangulation GPU chunk rejected before allocation: vertices={vertex_bytes} bytes, indices={index_bytes} bytes, limit={limit} bytes");
        return None;
    }
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Cached Triangulation Surface Vertices"),
        contents: bytemuck::cast_slice(vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Cached Triangulation Surface Indices"),
        contents: bytemuck::cast_slice(indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    // Per-chunk debug colour uniform; the bind group keeps the buffer alive.
    let debug_style = SurfaceStyleUniform {
        color: chunk_debug_color(chunk_index),
        params: [0.0; 4],
    };
    let debug_style_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Chunk Debug Style Uniform"),
        contents: bytemuck::bytes_of(&debug_style),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let debug_style_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        layout: surface_style_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: debug_style_buffer.as_entire_binding(),
        }],
        label: Some("Chunk Debug Style Bind Group"),
    });
    // Scene-origin-relative rebase offset re-added in the vertex shader.
    let chunk_uniform: [f32; 4] = [chunk_offset.x, chunk_offset.y, chunk_offset.z, 0.0];
    let chunk_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Surface Chunk Offset Uniform"),
        contents: bytemuck::bytes_of(&chunk_uniform),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let chunk_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        layout: surface_chunk_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: chunk_buffer.as_entire_binding(),
        }],
        label: Some("Surface Chunk Offset Bind Group"),
    });
    Some(CachedSurfaceChunk {
        vertex_buffer,
        index_buffer,
        index_count: indices.len() as u32,
        bounds_min,
        bounds_max,
        chunk_bind_group,
        debug_style_bind_group,
    })
}

fn build_edge_chunks(device: &wgpu::Device, scene_origin: DVec3, triangulation: &OpenTriangulation) -> Vec<CachedEdgeChunk> {
    const TARGET_CHUNK_BYTES: usize = 64 * 1024 * 1024;
    let buffer_limit = (device.limits().max_buffer_size as usize).min(TARGET_CHUNK_BYTES);
    let edges_per_chunk = (buffer_limit / size_of::<EdgeInstance>()).max(1);
    let mut chunks = Vec::new();
    let vertices = triangulation.mesh.vertices();
    for source_edges in triangulation.edges.chunks(edges_per_chunk) {
        let instances = source_edges
            .iter()
            .map(|[a, b]| {
                let a = vertices[*a as usize];
                let b = vertices[*b as usize];
                let start = DVec3::new(a.x, a.y, a.z) - scene_origin;
                let end = DVec3::new(b.x, b.y, b.z) - scene_origin;
                EdgeInstance {
                    start: start.as_vec3().to_array(),
                    end: end.as_vec3().to_array(),
                }
            })
            .collect::<Vec<_>>();
        if let Some(chunk) = upload_edge_chunk(device, &instances) {
            chunks.push(chunk);
        }
    }
    chunks
}

fn upload_edge_chunk(device: &wgpu::Device, instances: &[EdgeInstance]) -> Option<CachedEdgeChunk> {
    if instances.is_empty() {
        return None;
    }
    let limit = device.limits().max_buffer_size;
    let instance_bytes = size_of_val(instances) as u64;
    if instance_bytes > limit {
        log::error!("Triangulation edge chunk rejected before GPU allocation: instances={instance_bytes} bytes, limit={limit} bytes");
        return None;
    }
    let instance_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Cached Triangulation Edge Instances"),
        contents: bytemuck::cast_slice(instances),
        usage: wgpu::BufferUsages::VERTEX,
    });
    Some(CachedEdgeChunk {
        instance_buffer,
        instance_count: instances.len() as u32,
    })
}

pub(crate) struct BlockModelColorValues {
    values: std::sync::Arc<Vec<f64>>,
    default: Option<f64>,
    range: Option<(f64, f64)>,
    categorical: bool,
}

pub(crate) fn block_model_color_values(block_model: &OpenBlockModel) -> Option<BlockModelColorValues> {
    let name = block_model.active_color_variable.as_deref()?;
    let var = block_model.model.variable(name)?;
    let values = block_model.active_color_values()?;
    let default = color_variable_default(var);
    // Cached alongside the decoded values, so a rebuild doesn't re-scan the
    // whole column just to recover the render range.
    let range = block_model.active_value_range();
    Some(BlockModelColorValues {
        values,
        default,
        range,
        categorical: matches!(var.physical_type.as_str(), "namedbyte" | "namedshort"),
    })
}

/// Fingerprint of the set of blocks the fragment shader will hide outright.
/// Chunk geometry (skipped blocks, exposed-face culling) depends on this set,
/// so the cheap recolor-in-place path is only valid while it is unchanged.
///
/// XOR of per-index mixes rather than a sequential hasher: it is
/// order-independent, so the O(blocks) scan — which runs on every ramp-drag
/// tick to validate the restyle fast path — parallelizes cleanly. Block
/// indices are unique, so equal hidden sets always fingerprint equal;
/// distinct sets collide with ~2^-64 probability.
fn hidden_blocks_fingerprint(block_model: &OpenBlockModel) -> u64 {
    // Sebastiano Vigna's SplitMix64 finalizer: a bijective 64-bit mix, so
    // single-index "sets" can't collide and XOR combinations scatter well.
    fn splitmix64(mut x: u64) -> u64 {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }

    let color_values = block_model_color_values(block_model);
    // Captured by field: `OpenBlockModel` holds a `RefCell` cache and is not
    // `Sync`, but the pieces this scan needs all are.
    let color_transfer = &block_model.color_transfer;
    let hide_empty = block_model.hide_empty_color_values;
    block_model.renderable_block_indices.par_filter_map_reduce(
        || 0,
        |index| block_is_hidden(&color_values, color_transfer, index, hide_empty).then(|| splitmix64(index as u64)),
        |a, b| a ^ b,
    )
}

/// Whether two ramps hide exactly the same grade intervals. RGB edits and
/// alpha edits that stay above the discard epsilon cannot change volume
/// occupancy, so they do not justify walking every block merely to reproduce
/// the cached hidden-set fingerprint.
fn same_ramp_visibility(a: &ColorTransferFunction, b: &ColorTransferFunction) -> bool {
    fn visibility_runs(ramp: &ColorTransferFunction) -> Vec<(f32, bool)> {
        let stops = &ramp.stops[..ramp.stops.len().min(MAX_COLOR_STOPS)];
        let mut runs = Vec::with_capacity(stops.len() + 1);
        let mut visible = false;
        runs.push((0.0, visible));
        for stop in stops {
            let next_visible = stop.color[3] >= VISIBLE_ALPHA_EPSILON;
            let at = stop.t.clamp(0.0, 1.0);
            if next_visible != visible {
                if runs.last().is_some_and(|(last_at, _)| *last_at == at) {
                    runs.pop();
                }
                runs.push((at, next_visible));
                visible = next_visible;
            }
        }
        runs
    }

    visibility_runs(a) == visibility_runs(b)
}

pub(crate) fn grade_for_block(color_values: &Option<BlockModelColorValues>, block_index: usize, hide_empty: bool) -> f32 {
    let Some(color_values) = color_values.as_ref() else {
        return FALLBACK_BLOCK_GRADE;
    };
    let Some(value) = color_values.values.get(block_index).copied() else {
        return empty_grade(hide_empty);
    };
    let is_default = color_values.default.is_some_and(|default| (value - default).abs() < 1e-8);
    if color_values.categorical {
        if !value.is_finite() || (hide_empty && is_default) {
            return empty_grade(hide_empty);
        }
    } else if is_empty_grade_value(value, color_values.default) {
        return empty_grade(hide_empty);
    }
    color_values.range.map(|range| normalized_grade(value, range)).unwrap_or(FALLBACK_BLOCK_GRADE)
}

/// Whether the fragment shader will discard every fragment of the block at
/// `block_index` — combining the hidden-grade sentinel with a colour-ramp
/// alpha below the visibility epsilon. Geometry building treats such blocks as
/// absent, so this must stay in lockstep with `fs_main` in `block_model.wgsl`.
fn block_is_hidden(color_values: &Option<BlockModelColorValues>, color_transfer: &ColorTransferFunction, block_index: usize, hide_empty: bool) -> bool {
    let grade = grade_for_block(color_values, block_index, hide_empty);
    is_hidden_block_appearance(grade, color_values.is_some(), color_transfer)
}

/// Recomputes and re-uploads `grade` for every already-built chunk of a
/// block model, without re-walking blocks, redoing face culling, or
/// reallocating GPU buffers. Valid whenever chunk geometry (which blocks are
/// rendered, and which faces they have) hasn't changed — i.e. everything
/// except a translucency toggle or the block model's own data changing.
fn recolor_block_model_surface_chunks(queue: &wgpu::Queue, block_model: &OpenBlockModel, chunks: &mut [CachedBlockModelSurfaceChunk]) {
    let color_values = block_model_color_values(block_model);
    for chunk in chunks {
        for (instance, &block_index) in chunk.cpu_instances.iter_mut().zip(&chunk.block_indices) {
            instance.grade = grade_for_block(&color_values, block_index, block_model.hide_empty_color_values);
        }
        queue.write_buffer(&chunk.gpu.instance_buffer, 0, bytemuck::cast_slice(&chunk.cpu_instances));
    }
}

/// Build the opaque/transparent surface-chunk sets on the CPU. Runs on the
/// job pool (via [`schedule_surface_build`]); the render thread only uploads
/// the finished sets. When a volume renders the model these chunks are never
/// drawn, so the scheduler simply doesn't request them in that case.
fn build_block_model_surface_chunk_sets(scene_origin: DVec3, block_model: &OpenBlockModel, force_translucent: bool, has_partial_alpha_stops: bool) -> SurfaceChunkSets {
    if force_translucent {
        return SurfaceChunkSets {
            opaque: Vec::new(),
            transparent: build_block_model_surface_chunks(scene_origin, block_model, false, BlockSurfaceSelection::All),
        };
    }
    if has_partial_alpha_stops {
        return SurfaceChunkSets {
            opaque: build_block_model_surface_chunks(scene_origin, block_model, true, BlockSurfaceSelection::OpaqueOnly),
            transparent: build_block_model_surface_chunks(scene_origin, block_model, false, BlockSurfaceSelection::TransparentOnly),
        };
    }
    SurfaceChunkSets {
        opaque: build_block_model_surface_chunks(scene_origin, block_model, true, BlockSurfaceSelection::All),
        transparent: Vec::new(),
    }
}

fn build_block_model_surface_chunks(scene_origin: DVec3, block_model: &OpenBlockModel, cull_shared_faces: bool, selection: BlockSurfaceSelection) -> Vec<SurfaceChunkCpu> {
    use rayon::prelude::*;

    const BLOCKS_PER_CHUNK: usize = 8192;

    // Captured by field below: `OpenBlockModel` holds a `RefCell` cache and
    // is not `Sync`, but every piece the parallel loops need is.
    let color_values = block_model_color_values(block_model);
    let renderable = &block_model.renderable_block_indices;
    let blocks = &block_model.blocks;
    let color_transfer = &block_model.color_transfer;
    let hide_empty = block_model.hide_empty_color_values;
    let fallback_alpha = block_model.color[3];
    let model = &block_model.model;
    let slice = block_model.active_slice();

    // One pass over the renderable blocks computes each block's grade and
    // whether the shader will draw it (not hidden, matches the pass's alpha
    // selection). Both the occupancy build and the instance loop below read
    // these instead of re-evaluating the predicates per block.
    let (grades, drawn): (Vec<f32>, Vec<bool>) = renderable
        .par_map(|block_index| {
            let grade = grade_for_block(&color_values, block_index, hide_empty);
            let intersects_slice = slice.is_none_or(|slice| {
                blocks
                    .get(block_index)
                    .is_some_and(|block| block.lower.cmplt(slice.max).all() && block.upper.cmpgt(slice.min).all())
            });
            let block_drawn = intersects_slice && block_is_drawn(grade, color_values.is_some(), color_transfer, selection, fallback_alpha);
            (grade, block_drawn)
        })
        .into_iter()
        .unzip();
    let occupancy = cull_shared_faces.then(|| build_block_occupancy(block_model, &drawn));

    // The shader places blocks with `rotation * local + translation`. Offset
    // local bounds by `local_ref` — the local point that maps to the scene
    // origin — so the f32 instance coordinates stay small (block-extent scale)
    // and keep the precision the old CPU `local_to_world - scene_origin` had.
    // With this reference the translation is exactly zero (see
    // `block_model_style`), so only the rotation is uploaded.
    let rotation = model.rotation();
    let local_ref = rotation.inverse() * (scene_origin - model.origin());

    // Each 8192-block chunk is independent, so build them in parallel.
    let chunk_count = renderable.len().div_ceil(BLOCKS_PER_CHUNK);
    (0..chunk_count)
        .into_par_iter()
        .filter_map(|chunk_index| {
            let chunk_base = chunk_index * BLOCKS_PER_CHUNK;
            let chunk_end = (chunk_base + BLOCKS_PER_CHUNK).min(renderable.len());
            let mut instances = Vec::with_capacity(chunk_end - chunk_base);
            let mut block_indices = Vec::with_capacity(chunk_end - chunk_base);
            let mut bounds_min = Vec3::splat(f32::INFINITY);
            let mut bounds_max = Vec3::splat(f32::NEG_INFINITY);
            for position in chunk_base..chunk_end {
                let Some(block_index) = renderable.get(position) else {
                    continue;
                };
                // Hidden blocks would have every fragment discarded by the
                // shader; emitting them would only stale the culling when
                // values change. Blocks not matching the pass's alpha
                // selection are the other pass's problem.
                if !drawn[position] {
                    continue;
                }
                let Some(block) = blocks.get(block_index) else {
                    continue;
                };
                let grade = grades[position];
                // Whole-interior-block cull: when culling shared faces
                // (opaque), a block whose six neighbours are all
                // present-and-drawn shows no face, so it needs no instance.
                // Hidden blocks are already absent from the occupancy, so
                // this preserves the Phase-1 hole fix.
                if let Some(occupancy) = occupancy.as_ref()
                    && occupancy.fully_occluded(block)
                {
                    continue;
                }
                instances.push(BlockInstance {
                    lower: (block.lower - local_ref).as_vec3().to_array(),
                    grade,
                    upper: (block.upper - local_ref).as_vec3().to_array(),
                    _pad: 0.0,
                });
                block_indices.push(block_index);
                // Chunk bounds are the scene-relative world AABB, so keep
                // walking the block's rotated corners here (frustum culling /
                // depth sort).
                for corner in block_corners(model, block) {
                    let scene_rel = (corner - scene_origin).as_vec3();
                    bounds_min = bounds_min.min(scene_rel);
                    bounds_max = bounds_max.max(scene_rel);
                }
            }
            (!instances.is_empty()).then_some(SurfaceChunkCpu {
                instances,
                block_indices,
                bounds_min,
                bounds_max,
            })
        })
        .collect()
}

fn block_model_style(scene_origin: DVec3, block_model: &OpenBlockModel, translucent: bool) -> BlockModelStyleUniform {
    let has_grade = block_model_color_values(block_model).is_some();
    let mut fallback_color = block_model.color;
    if translucent {
        make_translucent(&mut fallback_color);
    }
    let mut stops = [ColorStopUniform { color: [0.0; 4], pos: [0.0; 4] }; MAX_COLOR_STOPS];
    let stop_count = block_model.color_transfer.stops.len().min(MAX_COLOR_STOPS);
    for (slot, stop) in stops.iter_mut().zip(block_model.color_transfer.stops.iter().take(MAX_COLOR_STOPS)) {
        slot.color = stop.color;
        slot.pos = [stop.t, 0.0, 0.0, 0.0];
    }
    // Local->scene rotation columns. Translation is zero because
    // `build_block_model_surface_chunks` pre-offsets instance bounds by the
    // local point that maps to the scene origin.
    let rotation = block_model.model.rotation();
    let col = |axis: DVec3| {
        let v = axis.as_vec3();
        [v.x, v.y, v.z, 0.0]
    };
    // Instances are uploaded offset by `local_ref` (see
    // `build_block_model_surface_chunks`), so the clip box is expressed in
    // that same frame for a direct per-instance comparison in the shader.
    let (clip_min, clip_max) = match block_model_instance_slice(scene_origin, block_model) {
        Some(slice) => {
            let min = slice.min.as_vec3();
            let max = slice.max.as_vec3();
            ([min.x, min.y, min.z, 0.0], [max.x, max.y, max.z, 0.0])
        }
        None => ([f32::MIN; 4], [f32::MAX; 4]),
    };
    BlockModelStyleUniform {
        fallback_color,
        options: [if has_grade { 1.0 } else { 0.0 }, stop_count as f32, 0.0, 0.0],
        rotation_0: col(rotation.x_axis),
        rotation_1: col(rotation.y_axis),
        rotation_2: col(rotation.z_axis),
        translation: [0.0; 4],
        clip_min,
        clip_max,
        stops,
    }
}

fn block_model_instance_slice(scene_origin: DVec3, block_model: &OpenBlockModel) -> Option<BlockModelSlice> {
    let slice = block_model.active_slice()?;
    let local_ref = block_model.model.rotation().inverse() * (scene_origin - block_model.model.origin());
    Some(BlockModelSlice {
        min: slice.min - local_ref,
        max: slice.max - local_ref,
    })
}

/// Whether the shader draws the block in the given pass: not hidden
/// (`is_hidden_block_appearance`) and matching the pass's alpha selection.
/// Fused so the colour ramp is evaluated once per block — this runs for every
/// renderable block on each surface-chunk rebuild. Must stay in lockstep with
/// `is_hidden_block_appearance` and `fs_main` in `block_model.wgsl`.
fn block_is_drawn(grade: f32, has_grade: bool, color_transfer: &ColorTransferFunction, selection: BlockSurfaceSelection, fallback_alpha: f32) -> bool {
    if is_hidden_block_grade(grade) {
        return false;
    }
    let graded = has_grade && grade >= 0.0;
    let alpha = if graded { ramp_alpha(color_transfer, grade) } else { fallback_alpha };
    if graded && alpha < VISIBLE_ALPHA_EPSILON {
        return false;
    }
    match selection {
        BlockSurfaceSelection::All => true,
        BlockSurfaceSelection::OpaqueOnly => alpha >= 0.98,
        BlockSurfaceSelection::TransparentOnly => alpha < 0.98,
    }
}

/// Whether any of the block model's colour-transfer stops has partial
/// (neither fully transparent nor fully opaque) alpha. Such a model must be
/// drawn through the translucent pipeline for grade colouring to blend
/// correctly, even if the user hasn't toggled translucency manually.
pub(crate) fn block_model_has_partial_alpha_stops(block_model: &OpenBlockModel) -> bool {
    let fallback_can_render = block_model.active_color_variable.is_none() || !block_model.hide_empty_color_values;
    let ramp_has_partial_alpha = block_model.active_color_variable.is_some() && block_model.color_transfer.stops.iter().any(|stop| partial_block_alpha(stop.color[3]));
    color_configuration_has_partial_alpha(block_model.color[3], fallback_can_render, ramp_has_partial_alpha)
}

fn color_configuration_has_partial_alpha(fallback_alpha: f32, fallback_can_render: bool, ramp_has_partial_alpha: bool) -> bool {
    (fallback_can_render && partial_block_alpha(fallback_alpha)) || ramp_has_partial_alpha
}

fn partial_block_alpha(alpha: f32) -> bool {
    const SHADER_VISIBLE_ALPHA: f32 = 0.004;
    (SHADER_VISIBLE_ALPHA..0.98).contains(&alpha)
}

fn upload_block_model_surface_chunk(device: &wgpu::Device, instances: &[BlockInstance], bounds_min: Vec3, bounds_max: Vec3) -> Option<CachedBlockModelChunkGpu> {
    if instances.is_empty() {
        return None;
    }
    let limit = device.limits().max_buffer_size;
    let instance_bytes = size_of_val(instances) as u64;
    if instance_bytes > limit {
        log::error!("Block model surface chunk rejected before GPU allocation: instances={instance_bytes} bytes, limit={limit} bytes");
        return None;
    }
    let instance_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Cached Block Model Instances"),
        contents: bytemuck::cast_slice(instances),
        // COPY_DST is required here: `recolor_block_model_surface_chunks`
        // patches `grade` in place with `queue.write_buffer` on a legend/
        // attribute switch, instead of reallocating this buffer.
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
    });
    Some(CachedBlockModelChunkGpu {
        instance_buffer,
        instance_count: instances.len() as u32,
        bounds_min,
        bounds_max,
    })
}

/// Block keys are quantized geometry the process generates itself, so use a
/// fast non-DoS-resistant hasher: culling probes this set six times per block
/// and SipHash dominated the rebuild profile.
type BlockKeySet = HashSet<[u64; 6], foldhash::fast::RandomState>;

/// Occupancy of the blocks that will actually be drawn: renderable geometry
/// minus blocks the fragment shader hides outright. Only drawn blocks may
/// cull a neighbour's shared face — a hidden block leaves a see-through hole,
/// so the face behind it must exist. This mirrors the shader's discard
/// exactly, including blocks made invisible by a fully-transparent colour-ramp
/// stop, so changing the ramp forces a geometry rebuild (correctness over
/// speed).
enum BlockOccupancy {
    /// The model's blocks lie on a verified uniform grid: one bit per cell,
    /// neighbour tests are O(1) index arithmetic with no hashing.
    Grid {
        grid: crate::model::block_model::UniformBlockGrid,
        bits: Vec<u64>,
    },
    /// Fallback for sub-blocked/irregular models (or grids too sparse for a
    /// bitset): quantized block bounds in a hash set.
    Keys(BlockKeySet),
}

/// Build the drawn-block occupancy for shared-face culling. `drawn` is
/// aligned with `renderable_block_indices` (see
/// `build_block_model_surface_chunks`).
fn build_block_occupancy(block_model: &OpenBlockModel, drawn: &[bool]) -> BlockOccupancy {
    let drawn_blocks = block_model
        .renderable_block_indices
        .iter()
        .zip(drawn)
        .filter(|&(_, &block_drawn)| block_drawn)
        .filter_map(|(index, _)| block_model.blocks.get(index));
    if let Some(grid) = block_model.uniform_grid.as_ref() {
        let mut bits = vec![0u64; grid.cell_count().div_ceil(64)];
        for block in drawn_blocks {
            if let Some(coords) = grid.cell_coords(block.lower) {
                let index = grid.linear_index(coords);
                bits[index / 64] |= 1 << (index % 64);
            }
        }
        BlockOccupancy::Grid { grid: grid.clone(), bits }
    } else {
        BlockOccupancy::Keys(drawn_blocks.map(block_key).collect())
    }
}

impl BlockOccupancy {
    /// Whether every one of the block's six same-size neighbours is drawn,
    /// i.e. the block shows no face. Short-circuits on the first exposed
    /// face, which spares surface blocks most of the six probes.
    fn fully_occluded(&self, block: crate::model::block_model::BlockBounds) -> bool {
        match self {
            Self::Grid { grid, bits } => {
                let Some(coords) = grid.cell_coords(block.lower) else {
                    return false;
                };
                const NEIGHBOUR_OFFSETS: [[isize; 3]; 6] = [[-1, 0, 0], [1, 0, 0], [0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1]];
                NEIGHBOUR_OFFSETS.iter().all(|offset| {
                    let mut neighbour = [0usize; 3];
                    for axis in 0..3 {
                        let position = coords[axis] as isize + offset[axis];
                        if position < 0 || position as usize >= grid.dims[axis] {
                            // Grid-edge blocks always show their outer face.
                            return false;
                        }
                        neighbour[axis] = position as usize;
                    }
                    let index = grid.linear_index(neighbour);
                    bits[index / 64] & (1 << (index % 64)) != 0
                })
            }
            Self::Keys(keys) => {
                let size = block.upper - block.lower;
                let neighbours = [
                    DVec3::new(-size.x, 0.0, 0.0),
                    DVec3::new(size.x, 0.0, 0.0),
                    DVec3::new(0.0, -size.y, 0.0),
                    DVec3::new(0.0, size.y, 0.0),
                    DVec3::new(0.0, 0.0, -size.z),
                    DVec3::new(0.0, 0.0, size.z),
                ];
                neighbours.iter().all(|&delta| {
                    let neighbour = block_key(crate::model::block_model::BlockBounds {
                        lower: block.lower + delta,
                        upper: block.upper + delta,
                    });
                    keys.contains(&neighbour)
                })
            }
        }
    }
}

fn block_key(block: crate::model::block_model::BlockBounds) -> [u64; 6] {
    [
        quantize(block.lower.x),
        quantize(block.lower.y),
        quantize(block.lower.z),
        quantize(block.upper.x),
        quantize(block.upper.y),
        quantize(block.upper.z),
    ]
}

fn quantize(value: f64) -> u64 {
    (value * 1_000_000.0).round().to_bits()
}

fn is_empty_grade_value(value: f64, default: Option<f64>) -> bool {
    !value.is_finite() || default.is_some_and(|default| (value - default).abs() < 1e-8) || crate::model::block_model::is_no_data_sentinel(value)
}

fn empty_grade(hide_empty: bool) -> f32 {
    if hide_empty { HIDDEN_BLOCK_GRADE } else { FALLBACK_BLOCK_GRADE }
}

fn normalized_grade(value: f64, range: (f64, f64)) -> f32 {
    let span = range.1 - range.0;
    if span.abs() <= f64::EPSILON {
        // A valid constant variable has a degenerate range. Keep its colour
        // deterministic (the first ramp stop), rather than producing NaN and
        // falling through shader comparisons unpredictably.
        0.0
    } else {
        // Branchy clamp: `f64::clamp` lowers to libm fmin/fmax calls on
        // x86-64 (NaN-correct semantics), which showed up in the per-block
        // grade profile. `value` is finite and `span` non-zero here, so `t`
        // can't be NaN.
        let t = (value - range.0) / span;
        if t < 0.0 {
            0.0
        } else if t > 1.0 {
            1.0
        } else {
            t as f32
        }
    }
}

fn build_block_model_edge_chunks(device: &wgpu::Device, scene_origin: DVec3, block_model: &OpenBlockModel) -> Vec<CachedEdgeChunk> {
    const EDGES_PER_CHUNK: usize = 64 * 1024;
    let color_values = block_model_color_values(block_model);
    let slice = block_model.active_slice();
    let mut chunks = Vec::new();
    let mut instances = Vec::new();
    for block_index in block_model.renderable_block_indices.iter() {
        let Some(mut block) = block_model.blocks.get(block_index) else {
            continue;
        };
        // Match the surface shader: remove outside blocks and clamp boundary
        // block outlines to the exact slice planes.
        if let Some(slice) = slice {
            block.lower = block.lower.max(slice.min);
            block.upper = block.upper.min(slice.max);
            if block.lower.cmpge(block.upper).any() {
                continue;
            }
        }
        // Shader-hidden blocks draw no faces; skip their outlines too.
        if block_is_hidden(&color_values, &block_model.color_transfer, block_index, block_model.hide_empty_color_values) {
            continue;
        }
        let corners = block_corners(&block_model.model, block);
        for [a, b] in [[0, 1], [1, 2], [2, 3], [3, 0], [4, 5], [5, 6], [6, 7], [7, 4], [0, 4], [1, 5], [2, 6], [3, 7]] {
            instances.push(EdgeInstance {
                start: (corners[a] - scene_origin).as_vec3().to_array(),
                end: (corners[b] - scene_origin).as_vec3().to_array(),
            });
            if instances.len() >= EDGES_PER_CHUNK {
                if let Some(chunk) = upload_edge_chunk(device, &instances) {
                    chunks.push(chunk);
                }
                instances.clear();
            }
        }
    }
    if let Some(chunk) = upload_edge_chunk(device, &instances) {
        chunks.push(chunk);
    }
    chunks
}

fn block_corners(model: &crate::model::formats::block_model_data::BlockModelData, block: crate::model::block_model::BlockBounds) -> [DVec3; 8] {
    let lo = block.lower;
    let hi = block.upper;
    [
        model.local_to_world(DVec3::new(lo.x, lo.y, lo.z)),
        model.local_to_world(DVec3::new(hi.x, lo.y, lo.z)),
        model.local_to_world(DVec3::new(hi.x, hi.y, lo.z)),
        model.local_to_world(DVec3::new(lo.x, hi.y, lo.z)),
        model.local_to_world(DVec3::new(lo.x, lo.y, hi.z)),
        model.local_to_world(DVec3::new(hi.x, lo.y, hi.z)),
        model.local_to_world(DVec3::new(hi.x, hi.y, hi.z)),
        model.local_to_world(DVec3::new(lo.x, hi.y, hi.z)),
    ]
}
