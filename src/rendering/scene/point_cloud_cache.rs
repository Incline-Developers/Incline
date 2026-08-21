//! Persistent, incrementally uploaded GPU representation of point clouds.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use glam::{DMat4, DVec2, DVec3, Mat4, Vec3};
use web_time::Instant;
use wgpu::util::DeviceExt;

pub(crate) use crate::model::point_cloud::{PointInstance, PointPosition};
use crate::{
    model::{
        SceneEntityId,
        point_cloud::{OpenPointCloud, POINT_CLOUD_LOD_LEVELS, PointCloudId, PreparedPointCloud},
    },
    rendering::{
        graphics::frustum::Frustum,
        scene::point_buffer_arena::{PointBufferArena, PointSlot},
    },
};

/// Mirrors `PointCloudStyle` in `point_cloud.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PointCloudStyleUniform {
    color: [f32; 4],
    /// x: screen-facing splat width in world units.
    options: [f32; 4],
    /// Cloud-local origin relative to the current floating scene origin.
    origin: [f32; 4],
}

pub(crate) struct CachedPointChunk {
    /// Suballocated vertex region. Draw and upload address `slot.buffer()` at
    /// `slot.offset()`; the slot returns to the arena on eviction.
    pub(crate) slot: PointSlot,
    pub(crate) level_counts: [u32; POINT_CLOUD_LOD_LEVELS],
    /// Finest prefix for which `slot` has allocated capacity. This can be finer
    /// than `uploaded_level`; unused capacity is filled one LOD suffix at a time
    /// without reallocating the slot.
    pub(crate) capacity_level: usize,
    /// Finest prefix currently present in `slot` (smaller is finer).
    pub(crate) uploaded_level: usize,
    /// Camera-requested level from the latest primary scene pass.
    pub(crate) requested_level: Cell<usize>,
    /// `uploaded_level` last acknowledged by the primary scene pass. A newer
    /// upload snaps the displayed count to the fresh data instead of ramping.
    pub(crate) displayed_uploaded_level: Cell<usize>,
    /// Prefix count actually drawn by the latest primary scene pass.
    pub(crate) displayed_count: Cell<u32>,
    /// Available/requested prefix that `displayed_count` is approaching.
    pub(crate) display_target_count: Cell<u32>,
    pub(crate) last_display_update: Cell<Instant>,
    /// Bounds in cloud-local coordinates.
    pub(crate) bounds_min: Vec3,
    pub(crate) bounds_max: Vec3,
    /// Full-resolution nearest-neighbour spacing (cloud units) copied from the
    /// prepared chunk. The LOD pass scales it by decimation and projection to
    /// pick a gap-free prefix without any per-frame coverage measurement.
    pub(crate) base_spacing: f32,
}

pub(crate) struct CachedPointCloudGpu {
    /// Slots retain prepared-chunk indexing even when the GPU allocation is
    /// evicted, keeping render and CPU-pick ranges aligned.
    pub(crate) chunks: Vec<Option<CachedPointChunk>>,
    pub(crate) style_bind_group: wgpu::BindGroup,
    pub(crate) colored: bool,
    pub(crate) origin_scene: Vec3,
    style_buffer: wgpu::Buffer,
    color: [f32; 4],
    pub(crate) point_size: f32,
    scene_origin: DVec3,
    prepared: Arc<PreparedPointCloud>,
    visible: bool,
    selected: bool,
}

#[derive(Default)]
pub(crate) struct PointCloudGpuCache {
    clouds: HashMap<PointCloudId, CachedPointCloudGpu>,
    arena: PointBufferArena,
    pending_uploads: bool,
    rejected_chunks: HashSet<ChunkKey>,
}

/// Limit newly exposed point records uploaded in any one render call, bounding
/// the CPU-side staging copy a single frame absorbs. Existing prefixes remain
/// resident and are not charged or retransferred when a chunk refines. This is
/// the sole per-frame streaming throttle: because chunks suballocate from the
/// arena, uploading no longer creates GPU buffers, so a chunk-count cap is
/// unnecessary - the byte budget alone bounds both transfer volume and command
/// count (a frame of bootstraps is at most this budget / a bootstrap prefix).
/// The frame-time tail scales with it, so it is kept modest; the load-in
/// latency win comes from refining straight to the requested level.
const UPLOAD_BUDGET_BYTES: usize = 32 * 1024 * 1024;
/// The first visible version of a chunk is small but spatially representative.
/// For a full 256k chunk this is level 8; smaller chunks bootstrap in full.
const BOOTSTRAP_POINTS_PER_CHUNK: u32 = 1_024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ChunkKey {
    cloud: PointCloudId,
    chunk: usize,
}

#[derive(Clone, Copy, Debug)]
struct ResidencyCandidate {
    key: ChunkKey,
    upload_level: usize,
    allocation_level: usize,
    upload_offset: usize,
    upload_bytes: usize,
    missing: bool,
    /// Currently uploaded level, `usize::MAX` when nothing is resident yet.
    /// Coarser resident chunks refine before already-detailed ones.
    resident_level: usize,
    distance_squared: f32,
}

impl PointCloudGpuCache {
    pub(crate) fn is_empty(&self) -> bool {
        !self.clouds.values().any(|cloud| cloud.chunks.iter().any(Option::is_some))
    }

    pub(crate) fn has_pending_uploads(&self) -> bool {
        self.pending_uploads
            || self.clouds.values().any(|cloud| {
                cloud.visible
                    && cloud.chunks.iter().flatten().any(|chunk| {
                        let target_level = chunk.uploaded_level.max(chunk.requested_level.get());
                        chunk.uploaded_level > chunk.requested_level.get() || chunk.displayed_count.get() != chunk.level_counts[target_level]
                    })
            })
    }

    pub(crate) fn get(&self, id: PointCloudId) -> Option<&CachedPointCloudGpu> {
        self.clouds.get(&id)
    }

    /// Nearest depth-writing point splat covering `screen_point` at the LOD
    /// used by the most recent render pass.
    pub(crate) fn nearest_depth_at_screen(&self, view_proj: &DMat4, screen: (f32, f32), screen_point: DVec2, hidden: &HashSet<SceneEntityId>) -> Option<f64> {
        self.nearest_hit_at_screen(view_proj, screen, screen_point, 0.0, hidden, &HashSet::new())
            .map(|(_, _, depth)| depth)
    }

    /// Nearest visible point-cloud splat under the cursor, including a small
    /// interaction tolerance beyond the rendered billboard.
    pub(crate) fn nearest_visible_entity_at_screen(
        &self,
        view_proj: &DMat4,
        screen: (f32, f32),
        screen_point: DVec2,
        threshold_px: f32,
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
    ) -> Option<(SceneEntityId, DVec3)> {
        self.nearest_hit_at_screen(view_proj, screen, screen_point, f64::from(threshold_px), hidden, frozen)
            .map(|(entity, world, _)| (entity, world))
    }

    fn nearest_hit_at_screen(
        &self,
        view_proj: &DMat4,
        screen: (f32, f32),
        screen_point: DVec2,
        padding_px: f64,
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
    ) -> Option<(SceneEntityId, DVec3, f64)> {
        let mut nearest = None;
        let inverse = view_proj.inverse();
        let billboard_axes = (
            inverse.x_axis.truncate().try_normalize().unwrap_or(DVec3::X),
            inverse.y_axis.truncate().try_normalize().unwrap_or(DVec3::Y),
        );
        for (&id, cached) in self.clouds.iter().filter(|(_, cached)| cached.visible) {
            let entity = SceneEntityId::PointCloud(id);
            if hidden.contains(&entity) || frozen.contains(&entity) {
                continue;
            }
            for (chunk_index, chunk) in cached.chunks.iter().enumerate().filter_map(|(index, chunk)| chunk.as_ref().map(|chunk| (index, chunk))) {
                let Some(prepared) = cached.prepared.chunks.get(chunk_index) else {
                    continue;
                };
                let count = chunk.displayed_count.get() as usize;
                for group in prepared.pick_groups.iter().filter(|group| (group.start as usize) < count) {
                    let bounds_min = cached.prepared.origin + group.bounds_min.as_dvec3();
                    let bounds_max = cached.prepared.origin + group.bounds_max.as_dvec3();
                    let group_half_extent = projected_world_splat_half_extent(view_proj, screen, (bounds_min + bounds_max) * 0.5, f64::from(cached.point_size), billboard_axes);
                    if !projected_bounds_overlap(view_proj, screen, screen_point, group_half_extent.max_element() + padding_px, bounds_min, bounds_max) {
                        continue;
                    }
                    let end = (group.end as usize).min(count);
                    for index in group.start as usize..end {
                        let Some(local) = prepared.data.position(index) else {
                            continue;
                        };
                        let world = cached.prepared.origin + DVec3::from_array(local.map(f64::from));
                        let Some(projected) = crate::rendering::pick::world_to_screen(view_proj, world, screen) else {
                            continue;
                        };
                        let half_extent = projected_world_splat_half_extent(view_proj, screen, world, f64::from(cached.point_size), billboard_axes);
                        if (projected.x - screen_point.x).abs() > half_extent.x + padding_px || (projected.y - screen_point.y).abs() > half_extent.y + padding_px {
                            continue;
                        }
                        let clip = *view_proj * world.extend(1.0);
                        if clip.w.abs() > f64::EPSILON {
                            let depth = clip.z / clip.w;
                            if nearest.as_ref().is_none_or(|(_, _, nearest_depth)| depth > *nearest_depth) {
                                nearest = Some((entity, world, depth));
                            }
                        }
                    }
                }
            }
        }
        nearest
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene_origin: DVec3,
        _scale_factor: f32,
        view_proj: Mat4,
        camera_scene: Vec3,
        point_clouds: &[OpenPointCloud],
        editor: &crate::ui::state::EditorState,
        style_layout: &wgpu::BindGroupLayout,
    ) {
        let loaded: HashSet<_> = point_clouds.iter().filter(|cloud| cloud.state.loaded).map(|cloud| cloud.id).collect();
        // Return the slots of any unloaded cloud to the arena before dropping
        // its cache entry, so their regions can back future clouds.
        {
            let Self { clouds, arena, .. } = &mut *self;
            clouds.retain(|id, cloud| {
                let keep = loaded.contains(id);
                if !keep {
                    for chunk in cloud.chunks.iter_mut().filter_map(Option::take) {
                        arena.free(chunk.slot);
                    }
                }
                keep
            });
        }
        // With every cloud unloaded no slots are outstanding, so release the
        // arena's GPU blocks rather than holding the high-water mark forever.
        if self.clouds.is_empty() {
            self.arena.clear();
        }
        self.rejected_chunks.retain(|key| loaded.contains(&key.cloud));

        for cloud in point_clouds {
            if !cloud.state.loaded {
                continue;
            }
            let point_size = cloud.point_size.max(1.0e-6);
            let selected = editor.selected_handles.contains(&cloud.entity_id());
            let replace = self.clouds.get(&cloud.id).is_some_and(|cached| !Arc::ptr_eq(&cached.prepared, &cloud.prepared));
            if replace {
                // Return the stale entry's slots before dropping it, otherwise
                // the reallocation leaks them for the arena's lifetime.
                if let Some(stale) = self.clouds.remove(&cloud.id) {
                    for chunk in stale.chunks.into_iter().flatten() {
                        self.arena.free(chunk.slot);
                    }
                }
            }

            if let Some(cached) = self.clouds.get_mut(&cloud.id) {
                cached.visible = cloud.visible;
                if cached.color != cloud.color || cached.point_size != point_size || cached.scene_origin != scene_origin || cached.selected != selected {
                    let style = style_uniform(cloud, point_size, scene_origin, selected);
                    queue.write_buffer(&cached.style_buffer, 0, bytemuck::bytes_of(&style));
                    cached.color = cloud.color;
                    cached.point_size = point_size;
                    cached.scene_origin = scene_origin;
                    cached.origin_scene = (cloud.prepared.origin - scene_origin).as_vec3();
                    cached.selected = selected;
                }
                continue;
            }

            let style = style_uniform(cloud, point_size, scene_origin, selected);
            let style_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Point Cloud Style Uniform"),
                contents: bytemuck::bytes_of(&style),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let style_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                layout: style_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: style_buffer.as_entire_binding(),
                }],
                label: Some("Point Cloud Style Bind Group"),
            });
            self.clouds.insert(
                cloud.id,
                CachedPointCloudGpu {
                    style_buffer,
                    style_bind_group,
                    colored: cloud.prepared.colored,
                    origin_scene: (cloud.prepared.origin - scene_origin).as_vec3(),
                    color: cloud.color,
                    point_size,
                    scene_origin,
                    prepared: Arc::clone(&cloud.prepared),
                    chunks: std::iter::repeat_with(|| None).take(cloud.prepared.chunks.len()).collect(),
                    visible: cloud.visible,
                    selected,
                },
            );
        }

        self.update_residency(device, queue, encoder, view_proj, camera_scene);
    }

    fn update_residency(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, view_proj: Mat4, camera_scene: Vec3) {
        let max_buffer_bytes = usize::try_from(device.limits().max_buffer_size).unwrap_or(usize::MAX);
        let frustum = Frustum::from_view_proj(view_proj);
        let mut visible_keys = HashSet::new();
        let mut candidates = Vec::new();
        for (&cloud_id, cloud) in &self.clouds {
            if !cloud.visible {
                continue;
            }
            for (chunk_index, prepared) in cloud.prepared.chunks.iter().enumerate() {
                let min = prepared.bounds_min + cloud.origin_scene;
                let max = prepared.bounds_max + cloud.origin_scene;
                if !frustum.intersects_aabb(min, max) {
                    continue;
                }
                let key = ChunkKey {
                    cloud: cloud_id,
                    chunk: chunk_index,
                };
                visible_keys.insert(key);
                if self.rejected_chunks.contains(&key) {
                    continue;
                }
                let resident = cloud.chunks[chunk_index].as_ref();
                let requested_level = resident.map(|chunk| chunk.requested_level.get()).unwrap_or_else(|| bootstrap_level(prepared.level_counts));
                let Some(mut upload_level) = next_upload_level(resident.map(|chunk| chunk.uploaded_level), requested_level, prepared.level_counts) else {
                    continue;
                };
                let uploaded_bytes = resident.map_or(0, |chunk| prefix_bytes(prepared.data.bytes().len(), prepared.level_counts, chunk.uploaded_level));
                let mut upload_end = prefix_bytes(prepared.data.bytes().len(), prepared.level_counts, upload_level);
                if upload_end > max_buffer_bytes {
                    // The requested prefix cannot fit in one buffer; refine
                    // to the finest prefix the adapter can hold instead.
                    let fallback = (upload_level + 1..prepared.level_counts.len())
                        .map(|level| {
                            let end = prefix_bytes(prepared.data.bytes().len(), prepared.level_counts, level);
                            (level, end)
                        })
                        .take_while(|&(_, end)| end > uploaded_bytes)
                        .find(|&(_, end)| end <= max_buffer_bytes);
                    match fallback {
                        Some((level, end)) => {
                            upload_level = level;
                            upload_end = end;
                        }
                        None => {
                            if self.rejected_chunks.insert(key) {
                                crate::userspace_error!(
                                    "Point-cloud LOD prefix needs {} MiB, exceeding the GPU's per-buffer limit; the chunk cannot be refined further",
                                    upload_end / (1024 * 1024),
                                );
                            }
                            continue;
                        }
                    }
                }
                // Reserve the complete jump target so budget-clamped
                // refinements can append in place on later frames.
                let allocation_level = resident.map(|chunk| chunk.capacity_level.min(upload_level)).unwrap_or(upload_level);
                candidates.push(ResidencyCandidate {
                    key,
                    upload_level,
                    allocation_level,
                    upload_offset: uploaded_bytes,
                    upload_bytes: upload_end - uploaded_bytes,
                    missing: resident.is_none(),
                    resident_level: resident.map_or(usize::MAX, |chunk| chunk.uploaded_level),
                    distance_squared: distance_squared_to_aabb(camera_scene, min, max),
                });
            }
        }

        sort_candidates_for_upload(&mut candidates);

        // Residency follows visibility only. Visible chunks are all admitted;
        // the upload scheduler controls their current prefix detail. Evicted
        // slots return to the arena for reuse rather than freeing GPU memory.
        let Self { clouds, arena, .. } = &mut *self;
        for (&cloud_id, cloud) in clouds.iter_mut() {
            for (chunk_index, slot) in cloud.chunks.iter_mut().enumerate() {
                let key = ChunkKey {
                    cloud: cloud_id,
                    chunk: chunk_index,
                };
                if !visible_keys.contains(&key)
                    && let Some(chunk) = slot.take()
                {
                    arena.free(chunk.slot);
                }
            }
        }

        let mut upload_budget = UPLOAD_BUDGET_BYTES;
        for candidate in &candidates {
            if candidate.upload_bytes > upload_budget {
                continue;
            }
            let (new_slot, level_counts, bounds_min, bounds_max, base_spacing) = {
                let cloud = &clouds[&candidate.key.cloud];
                let prepared = &cloud.prepared.chunks[candidate.key.chunk];
                let resident = cloud.chunks[candidate.key.chunk].as_ref();
                let needs_allocation = resident.is_none_or(|chunk| chunk.capacity_level != candidate.allocation_level);
                // Growing a chunk moves it to a larger slot and copies the
                // already-resident prefix GPU-to-GPU; appending detail in place
                // keeps the current slot. Neither path creates a buffer once the
                // arena's blocks are warm.
                let new_slot = needs_allocation.then(|| {
                    let size = prefix_bytes(prepared.data.bytes().len(), prepared.level_counts, candidate.allocation_level) as u64;
                    let slot = arena.alloc(device, size);
                    if let Some(resident) = resident {
                        encoder.copy_buffer_to_buffer(resident.slot.buffer(), resident.slot.offset(), slot.buffer(), slot.offset(), candidate.upload_offset as u64);
                    }
                    slot
                });
                let (dest_buffer, dest_offset) = match (&new_slot, resident) {
                    (Some(slot), _) => (slot.buffer(), slot.offset()),
                    (None, Some(resident)) => (resident.slot.buffer(), resident.slot.offset()),
                    (None, None) => {
                        unreachable!("a missing point-cloud chunk always allocates a slot")
                    }
                };
                let upload_end = candidate.upload_offset + candidate.upload_bytes;
                queue.write_buffer(
                    dest_buffer,
                    dest_offset + candidate.upload_offset as u64,
                    &prepared.data.bytes()[candidate.upload_offset..upload_end],
                );
                (new_slot, prepared.level_counts, prepared.bounds_min, prepared.bounds_max, prepared.base_spacing)
            };
            let cloud = clouds.get_mut(&candidate.key.cloud).expect("residency candidate cloud disappeared");
            if let Some(chunk) = cloud.chunks[candidate.key.chunk].as_mut() {
                if let Some(new_slot) = new_slot {
                    arena.free(std::mem::replace(&mut chunk.slot, new_slot));
                    chunk.capacity_level = candidate.allocation_level;
                }
                chunk.uploaded_level = candidate.upload_level;
            } else {
                cloud.chunks[candidate.key.chunk] = Some(CachedPointChunk {
                    slot: new_slot.expect("a missing point-cloud chunk needs a new slot"),
                    level_counts,
                    capacity_level: candidate.allocation_level,
                    uploaded_level: candidate.upload_level,
                    requested_level: Cell::new(candidate.upload_level),
                    displayed_uploaded_level: Cell::new(candidate.upload_level),
                    displayed_count: Cell::new(level_counts[candidate.upload_level]),
                    display_target_count: Cell::new(level_counts[candidate.upload_level]),
                    last_display_update: Cell::new(Instant::now()),
                    bounds_min,
                    bounds_max,
                    base_spacing,
                });
            }
            upload_budget -= candidate.upload_bytes;
        }

        self.pending_uploads = visible_keys.iter().any(|key| {
            if self.rejected_chunks.contains(key) {
                return false;
            }
            self.clouds
                .get(&key.cloud)
                .and_then(|cloud| cloud.chunks.get(key.chunk))
                .and_then(Option::as_ref)
                .is_none_or(|chunk| chunk.uploaded_level > chunk.requested_level.get())
        });
    }
}

fn projected_world_splat_half_extent(view_proj: &DMat4, screen: (f32, f32), world: DVec3, world_size: f64, billboard_axes: (DVec3, DVec3)) -> DVec2 {
    let Some(center) = crate::rendering::pick::world_to_screen(view_proj, world, screen) else {
        return DVec2::splat(2.0);
    };
    let half_size = world_size * 0.5;
    let Some(right) = crate::rendering::pick::world_to_screen(view_proj, world + billboard_axes.0 * half_size, screen) else {
        return DVec2::splat(2.0);
    };
    let Some(up) = crate::rendering::pick::world_to_screen(view_proj, world + billboard_axes.1 * half_size, screen) else {
        return DVec2::splat(2.0);
    };
    // The two billboard axes should project horizontally and vertically, but
    // summing their absolute deltas remains conservative under numerical
    // skew and vertical exaggeration.
    ((right - center).abs() + (up - center).abs()).max(DVec2::splat(1.0))
}

fn bootstrap_level(level_counts: [u32; POINT_CLOUD_LOD_LEVELS]) -> usize {
    level_counts.iter().position(|count| *count <= BOOTSTRAP_POINTS_PER_CHUNK).unwrap_or(level_counts.len() - 1)
}

fn next_upload_level(uploaded_level: Option<usize>, requested_level: usize, level_counts: [u32; POINT_CLOUD_LOD_LEVELS]) -> Option<usize> {
    let Some(uploaded_level) = uploaded_level else {
        return Some(bootstrap_level(level_counts));
    };
    // Jump straight to the camera's requested prefix. The LOD data is a
    // contiguous prefix, so refining several levels at once is a single
    // upload of the newly exposed suffix rather than one transfer per level.
    let requested_level = requested_level.min(level_counts.len() - 1);
    (uploaded_level > requested_level).then_some(requested_level)
}

fn prefix_bytes(full_bytes: usize, level_counts: [u32; POINT_CLOUD_LOD_LEVELS], level: usize) -> usize {
    let full_count = level_counts[0] as usize;
    debug_assert!(full_count > 0);
    debug_assert_eq!(full_bytes % full_count, 0);
    (full_bytes / full_count) * level_counts[level] as usize
}

fn distance_squared_to_aabb(point: Vec3, min: Vec3, max: Vec3) -> f32 {
    let nearest = point.clamp(min, max);
    point.distance_squared(nearest)
}

fn sort_candidates_for_upload(candidates: &mut [ResidencyCandidate]) {
    candidates.sort_by(|a, b| {
        b.missing
            .cmp(&a.missing)
            // Coarsest resident chunks refine first, so the scheduler completes
            // one breadth of the visible hierarchy before spending the budget
            // driving any single chunk to full detail.
            .then_with(|| b.resident_level.cmp(&a.resident_level))
            .then_with(|| a.distance_squared.total_cmp(&b.distance_squared))
            .then_with(|| a.key.cloud.0.cmp(&b.key.cloud.0))
            .then_with(|| a.key.chunk.cmp(&b.key.chunk))
    });
}

fn projected_bounds_overlap(view_proj: &DMat4, screen: (f32, f32), point: DVec2, padding: f64, min: DVec3, max: DVec3) -> bool {
    let mut projected_min = DVec2::splat(f64::INFINITY);
    let mut projected_max = DVec2::splat(f64::NEG_INFINITY);
    let mut projected_any = false;
    for x in [min.x, max.x] {
        for y in [min.y, max.y] {
            for z in [min.z, max.z] {
                let clip = *view_proj * DVec3::new(x, y, z).extend(1.0);
                if clip.w <= f64::EPSILON {
                    return true;
                }
                let ndc = clip.truncate() / clip.w;
                let screen_point = DVec2::new((ndc.x * 0.5 + 0.5) * f64::from(screen.0), (0.5 - ndc.y * 0.5) * f64::from(screen.1));
                projected_min = projected_min.min(screen_point);
                projected_max = projected_max.max(screen_point);
                projected_any = true;
            }
        }
    }
    projected_any && point.x >= projected_min.x - padding && point.x <= projected_max.x + padding && point.y >= projected_min.y - padding && point.y <= projected_max.y + padding
}

fn style_uniform(cloud: &OpenPointCloud, point_size: f32, scene_origin: DVec3, selected: bool) -> PointCloudStyleUniform {
    let origin = (cloud.prepared.origin - scene_origin).as_vec3();
    PointCloudStyleUniform {
        color: if selected { crate::ui::SELECTION_COLOR_F32 } else { cloud.color },
        options: [point_size, if selected { 1.0 } else { 0.0 }, 0.0, 0.0],
        origin: [origin.x, origin.y, origin.z, 0.0],
    }
}
