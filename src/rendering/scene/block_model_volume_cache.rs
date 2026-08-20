//! CPU scene representation and dynamic GPU streaming cache for block model volumes.

use std::collections::VecDeque;

use glam::{DVec3, Vec3};
use wgpu::util::DeviceExt;

use super::{
    block_model_ramp::{VISIBLE_ALPHA_EPSILON, is_hidden_block_appearance, ramp_rgba, volume_optical_depth_for_alpha},
    gpu_cache::{block_model_color_values, grade_for_block},
};
use crate::model::block_model::{ColorTransferFunction, OpenBlockModel};

/// Hard CPU/backing-store ceiling for the optional volume path. Models above
/// it fall back to the already bounded cube renderer instead of doing several
/// GiB of synchronous cell construction during a frame.
pub(crate) const MAX_VOLUME_CELL_BYTES: usize = crate::app::memory::VOLUME_CELL_LIMIT;
const MAX_VOLUME_METADATA_BYTES: usize = crate::app::memory::VOLUME_METADATA_LIMIT;
pub(crate) const BRICK_SIZE: usize = 8;
pub(crate) const CELLS_PER_BRICK: usize = BRICK_SIZE * BRICK_SIZE * BRICK_SIZE;
/// Bricks per axis of a level-1 "super-brick" (so 32^3 cells). Super-brick
/// aggregates live in the tail of the brick-aggregate buffer — no extra
/// binding, because the volume bind group already sits at WebGPU's default
/// 8-storage-buffers-per-stage limit.
pub(crate) const SUPER_FACTOR: usize = 4;
pub(crate) const EMPTY_BRICK: u32 = u32::MAX;
pub(crate) const UNIFORM_BRICK_FLAG: u32 = 0x8000_0000;
/// At least one cell in the brick is visible and at least one is invisible
/// under the active filter. Averaging such a brick across its full extent
/// changes its silhouette, so the shader must retain cell-level marching.
pub(crate) const MIXED_VISIBILITY_BRICK_FLAG: u32 = 0x4000_0000;
pub(crate) const NOT_RESIDENT_SLOT: u32 = 0x3fff_ffff;
// Cell payloads are 16 bits so two pack into one pool `u32`, halving pool
// memory, upload bandwidth, and the CPU/mmap backing. Encoding (must match
// the WGSL constants of the same name): `0xffff` empty, bit 15 the fallback
// flag, bits 0..14 a 15-bit quantized grade. Decode order matters — test
// empty before fallback, since empty also has bit 15 set.
const EMPTY_CELL_PAYLOAD: u16 = 0xffff;
const FALLBACK_CELL_FLAG: u16 = 1 << 15;
const CELL_GRADE_MASK: u16 = 0x7fff;
const CELL_GRADE_MAX: f32 = CELL_GRADE_MASK as f32;
/// Absolute floor for [`plane_tolerance`], covering coordinates near zero.
const PLANE_DEDUP_EPSILON: f32 = 1.0e-4;

const VOLUME_POOL_BUDGET_FRACTION: f64 = 0.6;
/// Raising the storage-buffer binding limit is necessary for large dense
/// address/aggregate tables, but it must not also reserve most of that limit
/// as a cell cache. In particular, a 2 GiB binding limit would otherwise make
/// one volume allocate and seed a ~1.2 GiB GPU buffer. Keep the production
/// cache anchored to the former 128 MiB binding envelope.
const VOLUME_POOL_BINDING_CAP_BYTES: u64 = 128 * 1024 * 1024;
const VOLUME_MAX_UPLOADS_PER_UPDATE: usize = 1024;
const VOLUME_OPACITY_CUTOFF: f32 = 0.95;
const VOLUME_MAX_STEPS: u32 = 4096;
const VOLUME_LOD_FOOTPRINT_FACTOR: f32 = 3.0;

fn volume_super_bricks_enabled() -> bool {
    false
}

fn default_volume_pool_budget_bytes(storage_limit: u64, max_buffer: u64) -> u64 {
    ((storage_limit.min(max_buffer).min(VOLUME_POOL_BINDING_CAP_BYTES) as f64) * VOLUME_POOL_BUDGET_FRACTION) as u64
}

fn detailed_volume_fits_pool(mixed_count: u64, pool_budget_bytes: u64) -> bool {
    let slot_bytes = (CELLS_PER_BRICK * size_of::<u16>()) as u64;
    mixed_count <= (pool_budget_bytes / slot_bytes).max(1)
}

/// Whether the beam pre-pass is likely to pay for itself on this model.
/// Mostly-uniform models have more skippable structure, so gate on that.
#[cfg(any(not(target_arch = "wasm32"), test))]
fn beam_heuristic(mixed_count: usize, occupied_count: usize) -> bool {
    mixed_count * 2 <= occupied_count
}

fn beam_enabled_for(asset: &BlockVolumeAsset) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mixed = asset.brick_uniform.iter().filter(|&&u| !u).count();
        beam_heuristic(mixed, asset.occupied_count)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = asset;
        false
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorStopUniform {
    color: [f32; 4],
    pos: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct BlockVolumeUniform {
    fallback_color: [f32; 4],
    options: [f32; 4],
    lod: [f32; 4],
    /// xyz: cell counts; w: bitmask of axes whose planes are uniformly
    /// spaced (1 = x, 2 = y, 4 = z), letting the raycaster replace the
    /// per-axis plane binary searches and plane fetches with arithmetic.
    dims: [u32; 4],
    brick_dims: [u32; 4],
    bounds_min: [f32; 4],
    bounds_max: [f32; 4],
    scene_to_local_0: [f32; 4],
    scene_to_local_1: [f32; 4],
    scene_to_local_2: [f32; 4],
    /// xyz: first plane per axis (valid for flagged axes).
    plane_origin: [f32; 4],
    /// xyz: plane spacing per axis (valid for flagged axes).
    plane_step: [f32; 4],
    /// xyz: 1 / spacing per axis (valid for flagged axes).
    plane_inv_step: [f32; 4],
    /// xyz: super-brick counts per axis; w: index of the first super-brick
    /// aggregate in the brick-aggregate buffer (= occupied brick count), or
    /// 0 to disable the level-1 LOD entirely.
    super_dims: [u32; 4],
    stops: [ColorStopUniform; crate::model::block_model::MAX_COLOR_STOPS],
}

pub(crate) struct CachedBlockVolumeGpu {
    pub(crate) bind_group: wgpu::BindGroup,
    pub(crate) uniform_buffer: wgpu::Buffer,
    pub(crate) _x_planes_buffer: wgpu::Buffer,
    pub(crate) _y_planes_buffer: wgpu::Buffer,
    pub(crate) _z_planes_buffer: wgpu::Buffer,
    pub(crate) cell_pool_buffer: wgpu::Buffer,
    pub(crate) _brick_table_buffer: wgpu::Buffer,
    pub(crate) brick_aggregate_buffer: wgpu::Buffer,
    pub(crate) brick_info_buffer: wgpu::Buffer,
    pub(crate) brick_usage_buffer: wgpu::Buffer,
    feedback_receiver: Option<std::sync::mpsc::Receiver<Result<Vec<u32>, String>>>,
    pub(crate) streamer: BrickStreamer,
    pub(crate) scene_to_local: [[f32; 4]; 3],
    pub(crate) asset: BlockVolumeAsset,
    /// Effective local ray-march bounds after applying the user slice.
    pub(crate) local_bounds_min: Vec3,
    pub(crate) local_bounds_max: Vec3,
    /// Scene-relative AABB used for culling and streaming visibility.
    pub(crate) bounds_min: Vec3,
    pub(crate) bounds_max: Vec3,
}

impl CachedBlockVolumeGpu {
    /// Whether the beam pre-pass should run for this volume; must agree with
    /// the `lod.w` flag written by `block_volume_uniform`, which is refreshed
    /// on every restyle from the same asset state.
    pub(crate) fn beam_enabled(&self) -> bool {
        beam_enabled_for(&self.asset)
    }

    pub(crate) fn has_pending_feedback(&self) -> bool {
        self.feedback_receiver.is_some()
    }

    pub(crate) fn has_pending_streaming(&self) -> bool {
        self.feedback_receiver.is_some() || self.streamer.has_pending()
    }

    /// Nearest visible volume cell along a model-local ray. This mirrors the
    /// volume asset's sparse occupancy rather than treating its outer AABB as
    /// solid, so document picks remain possible through genuine holes.
    pub(crate) fn nearest_opaque_cell_hit(&self, ray_origin: Vec3, ray_direction: Vec3, color_transfer: &ColorTransferFunction, fallback_alpha: f32) -> Option<f32> {
        self.nearest_cell_hit(ray_origin, ray_direction, color_transfer, fallback_alpha, 0.98)
    }

    /// Nearest cell that contributes visible colour to the volume render.
    /// Unlike the depth-occlusion query above, this includes translucent cells
    /// so interaction rays can anchor to what the user can actually see.
    pub(crate) fn nearest_visible_cell_hit(&self, ray_origin: Vec3, ray_direction: Vec3, color_transfer: &ColorTransferFunction, fallback_alpha: f32) -> Option<f32> {
        self.nearest_cell_hit(ray_origin, ray_direction, color_transfer, fallback_alpha, VISIBLE_ALPHA_EPSILON)
    }

    fn nearest_cell_hit(&self, ray_origin: Vec3, ray_direction: Vec3, color_transfer: &ColorTransferFunction, fallback_alpha: f32, minimum_alpha: f32) -> Option<f32> {
        let asset = &self.asset;
        let brick_dims = asset.brick_dims.map(|value| value as usize);
        let dims = asset.dims.map(|value| value as usize);
        let mut nearest = f32::INFINITY;
        for (ordinal, &brick_index) in asset.occupied_brick_indices.iter().enumerate() {
            if asset.brick_aggregates[ordinal][3] <= 0.0 {
                continue;
            }
            let brick_index = brick_index as usize;
            let bi = brick_index % brick_dims[0];
            let bj = (brick_index / brick_dims[0]) % brick_dims[1];
            let bk = brick_index / (brick_dims[0] * brick_dims[1]);
            let starts = [bi * BRICK_SIZE, bj * BRICK_SIZE, bk * BRICK_SIZE];
            let ends = [
                (starts[0] + BRICK_SIZE).min(dims[0]),
                (starts[1] + BRICK_SIZE).min(dims[1]),
                (starts[2] + BRICK_SIZE).min(dims[2]),
            ];
            let brick_min = Vec3::new(asset.x_planes[starts[0]], asset.y_planes[starts[1]], asset.z_planes[starts[2]]).max(self.local_bounds_min);
            let brick_max = Vec3::new(asset.x_planes[ends[0]], asset.y_planes[ends[1]], asset.z_planes[ends[2]]).min(self.local_bounds_max);
            if brick_min.cmpge(brick_max).any() {
                continue;
            }
            if ray_box_distance_f32(ray_origin, ray_direction, brick_min, brick_max).is_none_or(|distance| distance >= nearest) {
                continue;
            }
            let cells = asset.cells.brick_cells(ordinal as u32);
            for k in starts[2]..ends[2] {
                for j in starts[1]..ends[1] {
                    for i in starts[0]..ends[0] {
                        let local_index = brick_local_index(i - starts[0], j - starts[1], k - starts[2]);
                        let payload = cells[local_index];
                        let alpha = if payload == EMPTY_CELL_PAYLOAD {
                            0.0
                        } else if payload & FALLBACK_CELL_FLAG != 0 {
                            fallback_alpha
                        } else {
                            ramp_rgba(color_transfer, (payload & CELL_GRADE_MASK) as f32 / CELL_GRADE_MAX)[3]
                        };
                        if alpha < minimum_alpha {
                            continue;
                        }
                        let cell_min = Vec3::new(asset.x_planes[i], asset.y_planes[j], asset.z_planes[k]).max(self.local_bounds_min);
                        let cell_max = Vec3::new(asset.x_planes[i + 1], asset.y_planes[j + 1], asset.z_planes[k + 1]).min(self.local_bounds_max);
                        if cell_min.cmpge(cell_max).any() {
                            continue;
                        }
                        if let Some(distance) = ray_box_distance_f32(ray_origin, ray_direction, cell_min, cell_max)
                            && distance < nearest
                        {
                            nearest = distance;
                        }
                    }
                }
            }
        }
        nearest.is_finite().then_some(nearest)
    }
}

fn ray_box_distance_f32(origin: Vec3, direction: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let mut near = 0.0_f32;
    let mut far = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() <= f32::EPSILON {
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

pub(crate) enum CellBacking {
    Ram {
        cells: Vec<u16>,
        _reservation: crate::app::memory::MemoryReservation,
    },
    #[cfg(not(target_arch = "wasm32"))]
    Mapped(MappedCells),
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct MappedCells {
    mmap: memmap2::Mmap,
    // Keep the anonymous temporary file alive for exactly as long as its
    // mapping. `tempfile()` unlinks immediately on Unix and uses
    // delete-on-close on Windows, so the OS also cleans it after a crash.
    // Declaring the mapping first makes it drop before the file handle.
    _file: std::fs::File,
}

impl CellBacking {
    pub(crate) fn brick_cells(&self, ordinal: u32) -> &[u16] {
        let start = ordinal as usize * CELLS_PER_BRICK;
        let end = start + CELLS_PER_BRICK;
        match self {
            CellBacking::Ram { cells, .. } => &cells[start..end],
            #[cfg(not(target_arch = "wasm32"))]
            CellBacking::Mapped(m) => bytemuck::cast_slice(&m.mmap[start * 2..end * 2]),
        }
    }
}

enum CellBackingBuilder {
    Ram {
        cells: Vec<u16>,
        reservation: crate::app::memory::MemoryReservation,
    },
    #[cfg(not(target_arch = "wasm32"))]
    Mapped(MappedCellBackingBuilder),
}

#[cfg(not(target_arch = "wasm32"))]
struct MappedCellBackingBuilder {
    // Drop the mapping before its delete-on-close file handle on every early
    // return from sizing, initialization, flush, or read-only conversion.
    mmap: memmap2::MmapMut,
    file: std::fs::File,
}

#[cfg(not(target_arch = "wasm32"))]
const MAX_VOLUME_RAM_CELL_BYTES: usize = 256 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
const MAX_VOLUME_RAM_CELL_BYTES: usize = 1024 * 1024 * 1024;

impl CellBackingBuilder {
    fn new(cell_count: usize) -> Result<Self, String> {
        let bytes = cell_count.checked_mul(size_of::<u16>()).ok_or_else(|| "cell byte size overflows usize".to_owned())?;
        if bytes <= MAX_VOLUME_RAM_CELL_BYTES {
            let reservation = crate::app::memory::reserve(bytes, "block-volume cells")?;
            let mut cells = Vec::new();
            cells
                .try_reserve_exact(cell_count)
                .map_err(|error| format!("allocating {bytes} bytes for volume cells: {error}"))?;
            cells.resize(cell_count, EMPTY_CELL_PAYLOAD);
            return Ok(CellBackingBuilder::Ram { cells, reservation });
        }
        #[cfg(target_arch = "wasm32")]
        return Err(format!("volume cells require {bytes} bytes, exceeding the 1 GiB browser cache limit"));
        #[cfg(not(target_arch = "wasm32"))]
        Self::new_mapped(cell_count)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn new_mapped(cell_count: usize) -> Result<Self, String> {
        Self::new_mapped_in(cell_count, &std::env::temp_dir())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn new_mapped_in(cell_count: usize, directory: &std::path::Path) -> Result<Self, String> {
        let bytes = cell_count.checked_mul(size_of::<u16>()).ok_or_else(|| "cell byte size overflows usize".to_owned())?;
        // Unlike a named PID/counter path, `tempfile_in` creates a private,
        // unpredictable file and asks the OS to make it anonymous. Its File
        // handle owns cleanup immediately, including every failure below.
        let file = create_anonymous_cell_file(directory)?;
        file.set_len(bytes as u64).map_err(|e| format!("sizing temp cell file to {bytes} bytes: {e}"))?;
        let mut mmap = unsafe { memmap2::MmapMut::map_mut(&file) }.map_err(|e| format!("mapping temp cell file: {e}"))?;
        let words: &mut [u16] = bytemuck::cast_slice_mut(&mut mmap[..]);
        words.fill(EMPTY_CELL_PAYLOAD);
        Ok(CellBackingBuilder::Mapped(MappedCellBackingBuilder { mmap, file }))
    }

    fn as_mut_slice(&mut self) -> &mut [u16] {
        match self {
            CellBackingBuilder::Ram { cells, .. } => cells.as_mut_slice(),
            #[cfg(not(target_arch = "wasm32"))]
            CellBackingBuilder::Mapped(mapped) => bytemuck::cast_slice_mut(&mut mapped.mmap[..]),
        }
    }

    fn finish(self) -> Result<CellBacking, String> {
        match self {
            CellBackingBuilder::Ram { cells, reservation } => Ok(CellBacking::Ram { cells, _reservation: reservation }),
            #[cfg(not(target_arch = "wasm32"))]
            CellBackingBuilder::Mapped(mapped) => {
                mapped.mmap.flush().map_err(|e| format!("flushing temp cell file: {e}"))?;
                let mmap = mapped.mmap.make_read_only().map_err(|e| format!("sealing temp cell file: {e}"))?;
                Ok(CellBacking::Mapped(MappedCells { mmap, _file: mapped.file }))
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn create_anonymous_cell_file(directory: &std::path::Path) -> Result<std::fs::File, String> {
    let file = tempfile::tempfile_in(directory).map_err(|e| format!("creating anonymous temp cell file: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Linux's O_TMPFILE path can inherit OpenOptions' default mode. The
        // file is already anonymous here, so tightening it has no exposure
        // window, and the owned handle still cleans up if chmod fails.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("restricting anonymous temp cell file permissions: {e}"))?;
    }
    Ok(file)
}

const NO_OCCUPANT: u32 = u32::MAX;

pub(crate) struct BrickStreamer {
    pool_slots: u32,
    ordinal_slot: Vec<u32>,
    slot_occupant: Vec<u32>,
    free_slots: Vec<u32>,
    /// Style-independent residency requirement. A brick with more than one
    /// cell payload can become visually mixed under a later colour ramp, so
    /// its cells remain resident even while the current ramp makes it look
    /// uniform.
    detail_required: Vec<bool>,
    uniform: Vec<bool>,
    mixed_visibility: Vec<bool>,
    centers: Vec<[f32; 3]>,
    pending: VecDeque<u32>,
    fits: bool,
    dirty: bool,
    last_camera: Option<Vec3>,
    replan_distance: f32,
    desired_scratch: Vec<bool>,
    requested: Vec<bool>,
    request_age: Vec<u8>,
}

#[derive(Default)]
struct StreamPlan {
    uploads: Vec<(u32, u32)>,
    info_updates: Vec<(u32, u32)>,
}

impl BrickStreamer {
    fn new(pool_slots: u32, detail_required: Vec<bool>, uniform: Vec<bool>, mixed_visibility: Vec<bool>, centers: Vec<[f32; 3]>, replan_distance: f32) -> Self {
        debug_assert_eq!(detail_required.len(), uniform.len());
        debug_assert_eq!(uniform.len(), mixed_visibility.len());
        let occupied = uniform.len();
        let streamable = detail_required.iter().filter(|&&required| required).count();
        BrickStreamer {
            pool_slots,
            ordinal_slot: vec![NOT_RESIDENT_SLOT; occupied],
            slot_occupant: vec![NO_OCCUPANT; pool_slots as usize],
            free_slots: (0..pool_slots).rev().collect(),
            detail_required,
            uniform,
            mixed_visibility,
            centers,
            pending: VecDeque::new(),
            fits: streamable <= pool_slots as usize,
            dirty: true,
            last_camera: None,
            replan_distance,
            desired_scratch: vec![false; occupied],
            requested: vec![false; occupied],
            request_age: vec![u8::MAX; occupied],
        }
    }

    fn info(&self, ordinal: u32) -> u32 {
        let ordinal = ordinal as usize;
        let uniform_flag = if self.uniform[ordinal] { UNIFORM_BRICK_FLAG } else { 0 };
        let visibility_flag = if self.mixed_visibility[ordinal] { MIXED_VISIBILITY_BRICK_FLAG } else { 0 };
        uniform_flag | visibility_flag | self.ordinal_slot[ordinal]
    }

    fn all_info(&self) -> Vec<u32> {
        (0..self.ordinal_slot.len() as u32).map(|o| self.info(o)).collect()
    }

    fn set_style_flags(&mut self, uniform: Vec<bool>, mixed_visibility: Vec<bool>) {
        debug_assert_eq!(uniform.len(), self.uniform.len());
        debug_assert_eq!(mixed_visibility.len(), self.mixed_visibility.len());
        self.uniform = uniform;
        self.mixed_visibility = mixed_visibility;
        for (ordinal, &is_uniform) in self.uniform.iter().enumerate() {
            if is_uniform {
                self.requested[ordinal] = false;
                self.request_age[ordinal] = u8::MAX;
            }
        }
        // Accepted volumes reserve every structurally detailed brick, so a
        // style-only edit changes flags but never residency. Keep the
        // oversubscribed path coherent for defensive/future callers.
        self.dirty |= !self.fits;
    }

    fn feedback_enabled(&self) -> bool {
        !self.fits
    }

    fn apply_feedback(&mut self, words: &[u32]) {
        if self.fits {
            return;
        }
        let mut needs_residency_change = false;
        for ordinal in 0..self.ordinal_slot.len() {
            let requested = self.detail_required[ordinal] && !self.uniform[ordinal] && words.get(ordinal / 32).is_some_and(|word| word & (1 << (ordinal % 32)) != 0);
            self.requested[ordinal] = requested;
            self.request_age[ordinal] = if requested { 0 } else { self.request_age[ordinal].saturating_add(1) };
            needs_residency_change |= requested && self.ordinal_slot[ordinal] == NOT_RESIDENT_SLOT;
        }
        self.dirty |= needs_residency_change;
    }

    fn needs_replan(&self, camera_local: Vec3) -> bool {
        if self.dirty {
            return true;
        }
        if self.fits {
            return false;
        }
        self.last_camera.is_none_or(|c| c.distance(camera_local) > self.replan_distance)
    }

    fn replan(&mut self, camera_local: Vec3) -> Vec<(u32, u32)> {
        self.dirty = false;
        self.last_camera = Some(camera_local);
        self.pending.clear();
        let occupied = self.ordinal_slot.len();

        for d in self.desired_scratch.iter_mut() {
            *d = false;
        }
        let mut order: Vec<u32> = (0..occupied as u32).filter(|&o| self.detail_required[o as usize]).collect();
        if !self.fits {
            let cam = camera_local;
            let priority = |o: u32| {
                let ordinal = o as usize;
                let c = self.centers[o as usize];
                let class = if self.requested[ordinal] {
                    0
                } else if self.ordinal_slot[ordinal] != NOT_RESIDENT_SLOT && self.request_age[ordinal] <= 2 {
                    1
                } else {
                    2
                };
                (class, self.request_age[ordinal], (Vec3::from(c) - cam).length_squared())
            };
            let compare = |&a: &u32, &b: &u32| {
                let a = priority(a);
                let b = priority(b);
                a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.total_cmp(&b.2))
            };
            let k = self.pool_slots as usize;
            order.select_nth_unstable_by(k - 1, compare);
            order.truncate(k);
            order.sort_by(compare);
        }
        for &o in &order {
            self.desired_scratch[o as usize] = true;
        }

        let mut evictions = Vec::new();
        for slot in 0..self.slot_occupant.len() {
            let occupant = self.slot_occupant[slot];
            if occupant != NO_OCCUPANT && !self.desired_scratch[occupant as usize] {
                self.slot_occupant[slot] = NO_OCCUPANT;
                self.free_slots.push(slot as u32);
                self.ordinal_slot[occupant as usize] = NOT_RESIDENT_SLOT;
                evictions.push((occupant, self.info(occupant)));
            }
        }

        for &o in &order {
            if self.ordinal_slot[o as usize] == NOT_RESIDENT_SLOT {
                self.pending.push_back(o);
            }
        }
        evictions
    }

    fn drain(&mut self, max_uploads: usize) -> StreamPlan {
        let mut plan = StreamPlan::default();
        while plan.uploads.len() < max_uploads {
            let Some(ordinal) = self.pending.pop_front() else {
                break;
            };
            if self.ordinal_slot[ordinal as usize] != NOT_RESIDENT_SLOT || !self.detail_required[ordinal as usize] {
                continue;
            }
            let Some(slot) = self.free_slots.pop() else {
                self.pending.push_front(ordinal);
                break;
            };
            self.slot_occupant[slot as usize] = ordinal;
            self.ordinal_slot[ordinal as usize] = slot;
            plan.uploads.push((slot, ordinal));
            plan.info_updates.push((ordinal, self.info(ordinal)));
        }
        plan
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

pub(crate) struct BlockVolumeAsset {
    x_planes: Vec<f32>,
    y_planes: Vec<f32>,
    z_planes: Vec<f32>,
    pub(crate) cells: CellBacking,
    pub(crate) brick_table: Vec<u32>,
    /// Brick-table index for each occupied ordinal. Cached because style
    /// changes are frequent and rediscovering/sorting this mapping walked the
    /// entire sparse address space on every colour-picker tick.
    occupied_brick_indices: Vec<u32>,
    pub(crate) brick_aggregates: Vec<[f32; 4]>,
    /// Dense level-1 aggregates, one per `SUPER_FACTOR`^3-brick region
    /// (raster order over `super_dims`). Same (rgb, optical depth) encoding as
    /// `brick_aggregates`; `w == 0` means the region has no visible cell
    /// under the current ramp and the march skips it in one step.
    pub(crate) super_aggregates: Vec<[f32; 4]>,
    /// Bricks containing more than one underlying cell payload. Unlike
    /// `brick_uniform`, this is independent of the active colour ramp and is
    /// therefore the stable upper bound for detailed-cell residency.
    brick_detail_required: Vec<bool>,
    pub(crate) brick_uniform: Vec<bool>,
    /// Style-dependent bricks whose active filter leaves a mixture of visible
    /// and invisible cells. Their aggregate is valid for colour/opacity but
    /// not for silhouette, so the GPU does not use distance LOD for them.
    brick_visibility_mixed: Vec<bool>,
    brick_centers: Vec<[f32; 3]>,
    occupied_count: usize,
    dims: [u32; 3],
    brick_dims: [u32; 3],
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    reference_len: f32,
    _metadata_reservation: crate::app::memory::MemoryReservation,
}

impl BlockVolumeAsset {
    fn super_dims(&self) -> [u32; 3] {
        self.brick_dims.map(|d| d.div_ceil(SUPER_FACTOR as u32))
    }
}

pub(crate) fn upload_block_volume_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene_origin: DVec3,
    block_model: &OpenBlockModel,
    layout: &wgpu::BindGroupLayout,
    asset: BlockVolumeAsset,
    boundary_highlights: bool,
) -> Option<CachedBlockVolumeGpu> {
    let storage_limit = device.limits().max_storage_buffer_binding_size;
    let max_buffer = device.limits().max_buffer_size;

    let pool_budget_bytes = default_volume_pool_budget_bytes(storage_limit, max_buffer).min(storage_limit).min(max_buffer);
    // Two 16-bit cells pack into one pool `u32`, so a slot is
    // `CELLS_PER_BRICK` * 2 bytes (= `CELLS_PER_BRICK / 2` u32s). The 16-bit
    // CPU backing is byte-identical to this packed layout, so uploads are a
    // plain byte copy (see `write_cell_uploads`).
    let slot_bytes = (CELLS_PER_BRICK * size_of::<u16>()) as u64;
    let mixed_count = asset.brick_uniform.iter().filter(|&&u| !u).count() as u64;
    let detail_count = asset.brick_detail_required.iter().filter(|&&required| required).count() as u64;
    // A non-resident mixed brick is rendered from one aggregate spanning its
    // full 8×8×8 cell region. Those proxies are useful for navigation, but
    // they look like enormous source blocks and can never all resolve when
    // the visible working set exceeds the bounded pool. Preserve geometric
    // fidelity by declining the volume path in that case; the caller keeps
    // (or builds) exact cube geometry instead.
    if !detailed_volume_fits_pool(detail_count, pool_budget_bytes) {
        crate::userspace_warn!(
            "Block model '{}' can require {} MiB for detailed volume bricks under colour edits, exceeding the {} MiB GPU pool; showing exact cubes instead of coarse volume proxies.",
            block_model.name,
            detail_count * slot_bytes / (1024 * 1024),
            pool_budget_bytes / (1024 * 1024),
        );
        return None;
    }
    let pool_slots = detail_count.max(1) as u32;

    let plane_bytes = |n: usize| (n * size_of::<f32>()) as u64;
    let brick_table_bytes = (asset.brick_table.len() * size_of::<u32>()) as u64;
    let brick_aggregate_bytes = ((asset.brick_aggregates.len() + asset.super_aggregates.len()) * size_of::<[f32; 4]>()) as u64;
    let brick_info_bytes = (asset.occupied_count * size_of::<u32>()) as u64;
    let pool_bytes = pool_slots as u64 * slot_bytes;
    if brick_table_bytes > storage_limit
        || brick_aggregate_bytes > storage_limit
        || brick_info_bytes > storage_limit
        || pool_bytes > storage_limit
        || plane_bytes(asset.x_planes.len()) > storage_limit
        || plane_bytes(asset.y_planes.len()) > storage_limit
        || plane_bytes(asset.z_planes.len()) > storage_limit
    {
        crate::userspace_warn!(
            "Block model '{}' is too large for translucent volume rendering (needs a {} MiB brick table, GPU limit {} MiB); showing it as cubes instead.",
            block_model.name,
            brick_table_bytes / (1024 * 1024),
            storage_limit / (1024 * 1024),
        );
        return None;
    }
    // This upload can recur after a colour-ramp visibility change. Keep the
    // detailed GPU diagnostics out of the normal info console; the model's
    // user-facing summary is logged once when it is loaded.
    log::debug!(
        "Block model '{}' volume: {}x{}x{} bricks, {} occupied ({} mixed now, {} potentially detailed), uniform axes x/y/z: {}/{}/{}, beam pre-pass: {}",
        block_model.name,
        asset.brick_dims[0],
        asset.brick_dims[1],
        asset.brick_dims[2],
        asset.occupied_count,
        mixed_count,
        detail_count,
        uniform_plane_spacing(&asset.x_planes).is_some(),
        uniform_plane_spacing(&asset.y_planes).is_some(),
        uniform_plane_spacing(&asset.z_planes).is_some(),
        beam_enabled_for(&asset),
    );
    let mut streamer = BrickStreamer::new(
        pool_slots,
        asset.brick_detail_required.clone(),
        asset.brick_uniform.clone(),
        asset.brick_visibility_mixed.clone(),
        asset.brick_centers.clone(),
        streamer_replan_distance(&asset),
    );
    let seed_camera = 0.5 * (Vec3::from_array(asset.bounds_min) + Vec3::from_array(asset.bounds_max));
    streamer.replan(seed_camera);
    let seed = streamer.drain(pool_slots as usize);

    let uniform = block_volume_uniform(block_model, scene_origin, &asset, streamer.feedback_enabled(), boundary_highlights);
    let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Block Model Volume Uniform"),
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let x_planes_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Block Model Volume X Planes"),
        contents: bytemuck::cast_slice(&asset.x_planes),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let y_planes_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Block Model Volume Y Planes"),
        contents: bytemuck::cast_slice(&asset.y_planes),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let z_planes_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Block Model Volume Z Planes"),
        contents: bytemuck::cast_slice(&asset.z_planes),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let cell_pool_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Block Model Volume Cell Pool"),
        size: pool_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let brick_table_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Block Model Volume Brick Table"),
        contents: bytemuck::cast_slice(&asset.brick_table),
        usage: wgpu::BufferUsages::STORAGE,
    });
    // Per-brick aggregates (by ordinal) followed by the dense level-1
    // super-brick aggregates; the uniform's `super_dims.w` carries the tail
    // offset so no extra binding is needed.
    let aggregate_contents: Vec<[f32; 4]> = asset.brick_aggregates.iter().chain(asset.super_aggregates.iter()).copied().collect();
    let brick_aggregate_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Block Model Volume Brick Aggregates"),
        contents: bytemuck::cast_slice(&aggregate_contents),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let brick_info_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Block Model Volume Brick Info"),
        contents: bytemuck::cast_slice(&streamer.all_info()),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let usage_words = asset.occupied_count.div_ceil(32).max(1);
    let brick_usage_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Block Model Volume Brick Usage"),
        contents: bytemuck::cast_slice(&vec![0u32; usage_words]),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
    });
    write_cell_uploads(queue, &cell_pool_buffer, &asset.cells, &seed.uploads, slot_bytes);
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: x_planes_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: y_planes_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: z_planes_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: cell_pool_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: brick_table_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: brick_aggregate_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: brick_info_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: brick_usage_buffer.as_entire_binding(),
            },
        ],
        label: Some("Block Model Volume Bind Group"),
    });

    let (local_bounds_min, local_bounds_max) = effective_volume_bounds(block_model, &asset);
    let (bounds_min, bounds_max) = volume_scene_bounds(block_model, scene_origin, (local_bounds_min, local_bounds_max));

    Some(CachedBlockVolumeGpu {
        bind_group,
        uniform_buffer,
        _x_planes_buffer: x_planes_buffer,
        _y_planes_buffer: y_planes_buffer,
        _z_planes_buffer: z_planes_buffer,
        cell_pool_buffer,
        _brick_table_buffer: brick_table_buffer,
        brick_aggregate_buffer,
        brick_info_buffer,
        brick_usage_buffer,
        feedback_receiver: None,
        streamer,
        scene_to_local: scene_to_local_rows(block_model, scene_origin),
        asset,
        local_bounds_min,
        local_bounds_max,
        bounds_min,
        bounds_max,
    })
}

fn streamer_replan_distance(asset: &BlockVolumeAsset) -> f32 {
    let extent = Vec3::from_array(asset.bounds_max) - Vec3::from_array(asset.bounds_min);
    (extent.length() * 0.05).max(1.0e-3)
}

pub(crate) fn stream_volume_bricks(queue: &wgpu::Queue, volume: &mut CachedBlockVolumeGpu, camera_local: Vec3) {
    poll_volume_feedback(volume);
    if volume.streamer.needs_replan(camera_local) {
        let evictions = volume.streamer.replan(camera_local);
        write_info_updates(queue, &volume.brick_info_buffer, evictions);
    }
    if !volume.streamer.has_pending() {
        return;
    }
    let plan = volume.streamer.drain(VOLUME_MAX_UPLOADS_PER_UPDATE);
    // Packed 16-bit cells: `CELLS_PER_BRICK` * 2 bytes per slot (see the
    // matching computation in `upload_block_volume_gpu`).
    let slot_bytes = (CELLS_PER_BRICK * size_of::<u16>()) as u64;
    write_cell_uploads(queue, &volume.cell_pool_buffer, &volume.asset.cells, &plan.uploads, slot_bytes);
    write_info_updates(queue, &volume.brick_info_buffer, plan.info_updates);
}

pub(crate) fn poll_volume_feedback(volume: &mut CachedBlockVolumeGpu) {
    let Some(receiver) = volume.feedback_receiver.as_ref() else {
        return;
    };
    match receiver.try_recv() {
        Ok(Ok(words)) => {
            volume.feedback_receiver = None;
            volume.streamer.apply_feedback(&words);
        }
        Ok(Err(error)) => {
            volume.feedback_receiver = None;
            log::warn!("Block-volume usage feedback readback failed: {error}");
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {}
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            volume.feedback_receiver = None;
            log::warn!("Block-volume usage feedback readback disconnected");
        }
    }
}

pub(crate) fn clear_volume_feedback(queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, volume: &CachedBlockVolumeGpu, phase: u32) {
    if volume.streamer.feedback_enabled() && volume.feedback_receiver.is_none() {
        // Rotate which pixel of each 8x8 tile the raycast samples this cycle;
        // queued buffer writes execute before the encoder's clear + raycast.
        let offset = std::mem::offset_of!(BlockVolumeUniform, lod) + 2 * size_of::<f32>();
        queue.write_buffer(&volume.uniform_buffer, offset as u64, bytemuck::bytes_of(&((phase % 64) as f32)));
        encoder.clear_buffer(&volume.brick_usage_buffer, 0, None);
    }
}

pub(crate) fn schedule_volume_feedback(device: &wgpu::Device, queue: &wgpu::Queue, volume: &mut CachedBlockVolumeGpu) -> bool {
    if !volume.streamer.feedback_enabled() || volume.feedback_receiver.is_some() {
        return false;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    wgpu::util::DownloadBuffer::read_buffer(device, queue, &volume.brick_usage_buffer.slice(..), move |result| {
        let result = result
            .map(|download| bytemuck::cast_slice::<u8, u32>(&download).to_vec())
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });
    volume.feedback_receiver = Some(receiver);
    true
}

/// Upload consecutive pool slots together. The streamer commonly assigns a
/// long run of neighbouring free slots; issuing one write per 2 KiB brick made
/// camera movement spend much more CPU time in queue submission than copying.
/// Cap each temporary batch so seeding a very large pool stays memory-bounded.
fn write_cell_uploads(queue: &wgpu::Queue, buffer: &wgpu::Buffer, cells: &CellBacking, uploads: &[(u32, u32)], slot_bytes: u64) {
    const MAX_BATCH_BRICKS: usize = 1024;
    if uploads.is_empty() {
        return;
    }
    let mut ordered = uploads.to_vec();
    ordered.sort_unstable_by_key(|&(slot, _)| slot);
    let mut start = 0;
    while start < ordered.len() {
        let first_slot = ordered[start].0;
        let mut end = start + 1;
        while end < ordered.len() && end - start < MAX_BATCH_BRICKS && ordered[end].0 == ordered[end - 1].0 + 1 {
            end += 1;
        }
        // Contiguous slots' 16-bit cells, laid out exactly as the pool's
        // packed `u32`s (two little-endian cells per word) via a byte cast.
        let mut packed = Vec::<u16>::with_capacity((end - start) * CELLS_PER_BRICK);
        for &(_, ordinal) in &ordered[start..end] {
            packed.extend_from_slice(cells.brick_cells(ordinal));
        }
        queue.write_buffer(buffer, u64::from(first_slot) * slot_bytes, bytemuck::cast_slice(&packed));
        start = end;
    }
}

/// Coalesce neighbouring ordinal updates into a small number of queue writes.
fn write_info_updates(queue: &wgpu::Queue, buffer: &wgpu::Buffer, mut updates: Vec<(u32, u32)>) {
    const MAX_BATCH_INFOS: usize = 16 * 1024;
    updates.sort_unstable_by_key(|&(ordinal, _)| ordinal);
    let mut start = 0;
    while start < updates.len() {
        let first_ordinal = updates[start].0;
        let mut end = start + 1;
        while end < updates.len() && end - start < MAX_BATCH_INFOS && updates[end].0 == updates[end - 1].0 + 1 {
            end += 1;
        }
        let infos: Vec<u32> = updates[start..end].iter().map(|&(_, info)| info).collect();
        queue.write_buffer(buffer, u64::from(first_ordinal) * size_of::<u32>() as u64, bytemuck::cast_slice(&infos));
        start = end;
    }
}

pub(crate) fn update_block_volume_style(queue: &wgpu::Queue, volume: &mut CachedBlockVolumeGpu, scene_origin: DVec3, block_model: &OpenBlockModel, boundary_highlights: bool) {
    let style = compute_brick_style_data(&volume.asset, &block_model.color_transfer, block_model.color);
    queue.write_buffer(&volume.brick_aggregate_buffer, 0, bytemuck::cast_slice(&style.aggregates));
    if !style.super_aggregates.is_empty() {
        queue.write_buffer(
            &volume.brick_aggregate_buffer,
            (style.aggregates.len() * size_of::<[f32; 4]>()) as u64,
            bytemuck::cast_slice(&style.super_aggregates),
        );
    }
    volume.asset.brick_aggregates = style.aggregates;
    volume.asset.super_aggregates = style.super_aggregates;
    volume.asset.brick_uniform = style.uniform_flags.clone();
    volume.asset.brick_visibility_mixed = style.visibility_mixed_flags.clone();
    volume.streamer.set_style_flags(style.uniform_flags, style.visibility_mixed_flags);
    queue.write_buffer(&volume.brick_info_buffer, 0, bytemuck::cast_slice(&volume.streamer.all_info()));
    update_block_volume_slice(queue, volume, scene_origin, block_model, boundary_highlights);
}

/// Update only clip-dependent state. Unlike a colour restyle this does not
/// recompute or upload per-brick aggregates, so dragging a slice bound stays
/// responsive on large models.
pub(crate) fn update_block_volume_slice(queue: &wgpu::Queue, volume: &mut CachedBlockVolumeGpu, scene_origin: DVec3, block_model: &OpenBlockModel, boundary_highlights: bool) {
    let uniform = block_volume_uniform(block_model, scene_origin, &volume.asset, volume.streamer.feedback_enabled(), boundary_highlights);
    queue.write_buffer(&volume.uniform_buffer, 0, bytemuck::bytes_of(&uniform));
    let local_bounds = effective_volume_bounds(block_model, &volume.asset);
    (volume.local_bounds_min, volume.local_bounds_max) = local_bounds;
    (volume.bounds_min, volume.bounds_max) = volume_scene_bounds(block_model, scene_origin, local_bounds);
}

fn effective_volume_bounds(block_model: &OpenBlockModel, asset: &BlockVolumeAsset) -> (Vec3, Vec3) {
    let mut min = Vec3::from_array(asset.bounds_min);
    let mut max = Vec3::from_array(asset.bounds_max);
    if let Some(slice) = block_model.active_slice() {
        min = min.max(slice.min.as_vec3());
        max = max.min(slice.max.as_vec3());
        // Keep an empty slice well-formed for the slab intersection code.
        max = max.max(min);
    }
    (min, max)
}

fn volume_scene_bounds(block_model: &OpenBlockModel, scene_origin: DVec3, (local_min, local_max): (Vec3, Vec3)) -> (Vec3, Vec3) {
    if block_model.active_slice().is_none()
        && let Some((min, max)) = block_model.visible_world_bounds()
    {
        return ((min - scene_origin).as_vec3(), (max - scene_origin).as_vec3());
    }

    let rotation = block_model.model.rotation();
    let translation = block_model.model.origin() - scene_origin;
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for x in [local_min.x, local_max.x] {
        for y in [local_min.y, local_max.y] {
            for z in [local_min.z, local_max.z] {
                let point = rotation * DVec3::new(x as f64, y as f64, z as f64) + translation;
                min = min.min(point);
                max = max.max(point);
            }
        }
    }
    (min.as_vec3(), max.as_vec3())
}

fn block_volume_uniform(block_model: &OpenBlockModel, scene_origin: DVec3, asset: &BlockVolumeAsset, feedback_enabled: bool, boundary_highlights: bool) -> BlockVolumeUniform {
    let mut fallback_color = block_model.color;
    fallback_color[3] = fallback_color[3].clamp(0.0, 1.0);

    let mut stops = [ColorStopUniform { color: [0.0; 4], pos: [0.0; 4] }; crate::model::block_model::MAX_COLOR_STOPS];
    let stop_count = block_model.color_transfer.stops.len().clamp(2, crate::model::block_model::MAX_COLOR_STOPS);
    for (slot, stop) in stops
        .iter_mut()
        .zip(block_model.color_transfer.stops.iter().take(crate::model::block_model::MAX_COLOR_STOPS))
    {
        slot.color = stop.color;
        slot.pos = [stop.t, 0.0, 0.0, 0.0];
    }

    let [row_x, row_y, row_z] = scene_to_local_rows(block_model, scene_origin);

    let mut uniform_axis_flags = 0u32;
    let mut plane_origin = [0.0f32; 4];
    let mut plane_step = [1.0f32; 4];
    let mut plane_inv_step = [1.0f32; 4];
    for (axis, planes) in [&asset.x_planes, &asset.y_planes, &asset.z_planes].into_iter().enumerate() {
        if let Some((origin, step)) = uniform_plane_spacing(planes) {
            uniform_axis_flags |= 1 << axis;
            plane_origin[axis] = origin;
            plane_step[axis] = step;
            plane_inv_step[axis] = step.recip();
        }
    }

    let (bounds_min, bounds_max) = effective_volume_bounds(block_model, asset);

    BlockVolumeUniform {
        fallback_color,
        options: [stop_count as f32, asset.reference_len.max(1.0e-6), VOLUME_OPACITY_CUTOFF, VOLUME_MAX_STEPS as f32],
        lod: [
            VOLUME_LOD_FOOTPRINT_FACTOR,
            if feedback_enabled { 1.0 } else { 0.0 },
            // z: feedback sample phase, rewritten each feedback cycle by
            // `clear_volume_feedback`.
            0.0,
            if beam_enabled_for(asset) { 1.0 } else { 0.0 },
        ],
        dims: [asset.dims[0], asset.dims[1], asset.dims[2], uniform_axis_flags],
        brick_dims: [asset.brick_dims[0], asset.brick_dims[1], asset.brick_dims[2], BRICK_SIZE as u32],
        bounds_min: [bounds_min.x, bounds_min.y, bounds_min.z, if boundary_highlights { 1.0 } else { 0.0 }],
        bounds_max: [bounds_max.x, bounds_max.y, bounds_max.z, 0.0],
        scene_to_local_0: row_x,
        scene_to_local_1: row_y,
        scene_to_local_2: row_z,
        plane_origin,
        plane_step,
        plane_inv_step,
        super_dims: {
            let [sx, sy, sz] = asset.super_dims();
            let enabled = volume_super_bricks_enabled() && asset.super_aggregates.len() == (sx * sy * sz) as usize;
            [sx, sy, sz, if enabled { asset.occupied_count as u32 } else { 0 }]
        },
        stops,
    }
}

fn scene_to_local_rows(block_model: &OpenBlockModel, scene_origin: DVec3) -> [[f32; 4]; 3] {
    let rotation = block_model.model.rotation();
    let model_origin_scene = block_model.model.origin() - scene_origin;
    let row = |axis: DVec3| [axis.x as f32, axis.y as f32, axis.z as f32, -axis.dot(model_origin_scene) as f32];
    [row(rotation.x_axis), row(rotation.y_axis), row(rotation.z_axis)]
}

pub(crate) fn apply_scene_to_local(rows: &[[f32; 4]; 3], scene: Vec3) -> Vec3 {
    let axis = |r: &[f32; 4]| r[0] * scene.x + r[1] * scene.y + r[2] * scene.z + r[3];
    Vec3::new(axis(&rows[0]), axis(&rows[1]), axis(&rows[2]))
}

fn validate_volume_metadata_budget(brick_count: usize) -> Result<(), String> {
    // Dense address/occupancy tables plus the worst-case per-occupied-brick
    // aggregate, centre, uniform/residency and streaming metadata. Keep this
    // estimate deliberately conservative; cell payloads have their own cap.
    const BYTES_PER_BRICK_UPPER_BOUND: usize = 64;
    let bytes = brick_count
        .checked_mul(BYTES_PER_BRICK_UPPER_BOUND)
        .ok_or_else(|| "brick metadata byte size overflows usize".to_owned())?;
    if bytes > MAX_VOLUME_METADATA_BYTES || brick_count > u32::MAX as usize {
        return Err(format!("brick grid metadata would require at least {} MiB", bytes / (1024 * 1024)));
    }
    Ok(())
}

pub(crate) fn build_block_volume_asset(block_model: &OpenBlockModel) -> Result<Option<BlockVolumeAsset>, String> {
    let Some((x_planes, y_planes, z_planes)) = block_volume_planes(block_model) else {
        return Ok(None);
    };
    if x_planes.len() < 2 || y_planes.len() < 2 || z_planes.len() < 2 {
        return Ok(None);
    }

    let dims_usize = [x_planes.len() - 1, y_planes.len() - 1, z_planes.len() - 1];
    let brick_dims_usize = brick_grid_dims(dims_usize);
    let brick_count = brick_dims_usize
        .into_iter()
        .try_fold(1usize, |acc, dim| acc.checked_mul(dim))
        .ok_or_else(|| "brick grid dimensions overflow usize".to_owned())?;
    validate_volume_metadata_budget(brick_count)?;
    let metadata_bytes = brick_count.checked_mul(64).ok_or_else(|| "brick metadata byte size overflows usize".to_owned())?;
    let metadata_reservation = crate::app::memory::reserve(metadata_bytes, "block-volume metadata")?;
    let color_values = block_model_color_values(block_model);

    let x_lookup = PlaneLookup::new(&x_planes);
    let y_lookup = PlaneLookup::new(&y_planes);
    let z_lookup = PlaneLookup::new(&z_planes);
    // Capture only the thread-safe fields needed by the parallel occupancy
    // pass; `OpenBlockModel` also contains a `RefCell` cache.
    let blocks = &block_model.blocks;
    let color_transfer = &block_model.color_transfer;
    let hide_empty = block_model.hide_empty_color_values;
    type BlockPlacement = (u16, [[usize; 2]; 3]);
    // Resolve placement from compact block geometry on demand. This is walked
    // twice so a regular model does not need another large per-block vector:
    // first in parallel to discover occupied bricks, then in model order to
    // populate their cells and preserve last-block-wins overlap behaviour.
    let placement = |block_index| -> Result<Option<BlockPlacement>, ()> {
        let Some(block) = blocks.get(block_index) else {
            return Ok(None);
        };
        let grade = grade_for_block(&color_values, block_index, hide_empty);
        if is_hidden_block_appearance(grade, color_values.is_some(), color_transfer) {
            return Ok(None);
        }
        let Some(ix0) = x_lookup.index(block.lower.x as f32) else {
            return Err(());
        };
        let Some(ix1) = x_lookup.index(block.upper.x as f32) else {
            return Err(());
        };
        let Some(iy0) = y_lookup.index(block.lower.y as f32) else {
            return Err(());
        };
        let Some(iy1) = y_lookup.index(block.upper.y as f32) else {
            return Err(());
        };
        let Some(iz0) = z_lookup.index(block.lower.z as f32) else {
            return Err(());
        };
        let Some(iz1) = z_lookup.index(block.upper.z as f32) else {
            return Err(());
        };
        if ix1 <= ix0 || iy1 <= iy0 || iz1 <= iz0 {
            return Err(());
        }
        Ok(Some((pack_cell_payload(grade, color_values.is_some()), [[ix0, ix1], [iy0, iy1], [iz0, iz1]])))
    };
    // The occupancy bitset only takes idempotent one-way (false→true)
    // writes, so relaxed atomics are sufficient.
    let occupancy_words: Vec<std::sync::atomic::AtomicU64> = (0..brick_count.div_ceil(64)).map(|_| std::sync::atomic::AtomicU64::new(0)).collect();
    // Blocks whose bounds fail to resolve to cell planes are dropped from the
    // volume; count them so the failure is visible instead of silently
    // rendering holes.
    let unplaced_blocks = std::sync::atomic::AtomicUsize::new(0);
    block_model.renderable_block_indices.par_for_each(|block_index| match placement(block_index) {
        Ok(Some((_, [[ix0, ix1], [iy0, iy1], [iz0, iz1]]))) => {
            for bk in (iz0 / BRICK_SIZE)..=((iz1 - 1) / BRICK_SIZE) {
                for bj in (iy0 / BRICK_SIZE)..=((iy1 - 1) / BRICK_SIZE) {
                    for bi in (ix0 / BRICK_SIZE)..=((ix1 - 1) / BRICK_SIZE) {
                        let bit = brick_index(brick_dims_usize, bi, bj, bk);
                        occupancy_words[bit / 64].fetch_or(1 << (bit % 64), std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
        Err(()) => {
            unplaced_blocks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(None) => {}
    });
    let unplaced_blocks = unplaced_blocks.into_inner();
    if unplaced_blocks > 0 {
        crate::userspace_warn!(
            "Block model volume build could not align {unplaced_blocks} block(s) with the \
             model's cell planes; they will not be rendered. Please report this if blocks \
             appear to be missing."
        );
    }
    let occupancy_words: Vec<u64> = occupancy_words.into_iter().map(std::sync::atomic::AtomicU64::into_inner).collect();
    let mut brick_occupied = Vec::new();
    brick_occupied
        .try_reserve_exact(brick_count)
        .map_err(|error| format!("could not allocate brick occupancy table: {error}"))?;
    brick_occupied.resize(brick_count, false);
    for (bindex, occupied) in brick_occupied.iter_mut().enumerate() {
        *occupied = occupancy_words[bindex / 64] & (1 << (bindex % 64)) != 0;
    }

    let occupied_count = brick_occupied.iter().filter(|&&occ| occ).count();
    if occupied_count == 0 {
        return Ok(None);
    }

    let sparse_cell_count = occupied_count.checked_mul(CELLS_PER_BRICK).ok_or_else(|| "sparse cell count overflows usize".to_owned())?;
    let sparse_cell_bytes = sparse_cell_count
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| "sparse cell byte size overflows usize".to_owned())?;
    if sparse_cell_bytes > MAX_VOLUME_CELL_BYTES {
        return Err(format!("sparse brick volume would require {} MiB of cell payloads", sparse_cell_bytes / (1024 * 1024)));
    }

    let mut brick_table = Vec::new();
    brick_table
        .try_reserve_exact(brick_count)
        .map_err(|error| format!("could not allocate brick address table: {error}"))?;
    brick_table.resize(brick_count, EMPTY_BRICK);
    let mut brick_centers = vec![[0.0f32; 3]; occupied_count];
    let mut occupied_brick_indices = Vec::with_capacity(occupied_count);
    let mut next_ordinal = 0u32;
    for (bindex, &occ) in brick_occupied.iter().enumerate() {
        if !occ {
            continue;
        }
        brick_table[bindex] = next_ordinal;
        occupied_brick_indices.push(bindex as u32);
        let bi = bindex % brick_dims_usize[0];
        let bj = (bindex / brick_dims_usize[0]) % brick_dims_usize[1];
        let bk = bindex / (brick_dims_usize[0] * brick_dims_usize[1]);
        let axis_center = |planes: &[f32], b: usize, dim: usize| {
            let lo = b * BRICK_SIZE;
            let hi = ((b + 1) * BRICK_SIZE).min(dim);
            (planes[lo] + planes[hi]) * 0.5
        };
        brick_centers[next_ordinal as usize] = [
            axis_center(&x_planes, bi, dims_usize[0]),
            axis_center(&y_planes, bj, dims_usize[1]),
            axis_center(&z_planes, bk, dims_usize[2]),
        ];
        next_ordinal += 1;
    }

    let mut builder = CellBackingBuilder::new(sparse_cell_count)?;
    {
        let cells = builder.as_mut_slice();
        for block_index in block_model.renderable_block_indices.iter() {
            let Ok(Some((payload, [[ix0, ix1], [iy0, iy1], [iz0, iz1]]))) = placement(block_index) else {
                continue;
            };
            for k in iz0..iz1 {
                for j in iy0..iy1 {
                    for i in ix0..ix1 {
                        let bindex = brick_index(brick_dims_usize, i / BRICK_SIZE, j / BRICK_SIZE, k / BRICK_SIZE);
                        let ordinal = brick_table[bindex] as usize;
                        let local = brick_local_index(i % BRICK_SIZE, j % BRICK_SIZE, k % BRICK_SIZE);
                        cells[ordinal * CELLS_PER_BRICK + local] = payload;
                    }
                }
            }
        }
    }
    let cells = builder.finish()?;

    let reference_len = reference_cell_length(&x_planes, &y_planes, &z_planes);

    let dims = [
        dims_usize[0].try_into().map_err(|_| "x cell count exceeds u32".to_owned())?,
        dims_usize[1].try_into().map_err(|_| "y cell count exceeds u32".to_owned())?,
        dims_usize[2].try_into().map_err(|_| "z cell count exceeds u32".to_owned())?,
    ];
    let brick_dims = [brick_dims_usize[0] as u32, brick_dims_usize[1] as u32, brick_dims_usize[2] as u32];
    let mut asset = BlockVolumeAsset {
        bounds_min: [x_planes[0], y_planes[0], z_planes[0]],
        bounds_max: [*x_planes.last().unwrap(), *y_planes.last().unwrap(), *z_planes.last().unwrap()],
        x_planes,
        y_planes,
        z_planes,
        cells,
        brick_table,
        occupied_brick_indices,
        brick_aggregates: Vec::new(),
        super_aggregates: Vec::new(),
        brick_detail_required: Vec::new(),
        brick_uniform: Vec::new(),
        brick_visibility_mixed: Vec::new(),
        brick_centers,
        occupied_count,
        dims,
        brick_dims,
        reference_len,
        _metadata_reservation: metadata_reservation,
    };
    asset.brick_detail_required = compute_brick_detail_requirements(&asset);
    let style = compute_brick_style_data(&asset, &block_model.color_transfer, block_model.color);
    asset.brick_aggregates = style.aggregates;
    asset.super_aggregates = style.super_aggregates;
    asset.brick_uniform = style.uniform_flags;
    asset.brick_visibility_mixed = style.visibility_mixed_flags;
    Ok(Some(asset))
}

struct VolumeRampLut {
    entries: Vec<([f32; 4], f32)>,
    fallback: ([f32; 4], f32),
}

impl VolumeRampLut {
    fn build(color_transfer: &ColorTransferFunction, fallback_color: [f32; 4]) -> Self {
        let entry = |color: [f32; 4]| (color, volume_optical_depth_for_alpha(color[3].clamp(0.0, 1.0)));
        // The ramp is piecewise-constant over sorted stops, so the table is
        // runs of identical entries. Sweep the stop list alongside the grade
        // index — one optical-depth (`ln`) evaluation per stop — instead of a
        // full `ramp_rgba` evaluation per entry: this runs on the render thread
        // on every colour-stop drag tick. Must mirror `ramp_rgba` exactly:
        // below the first stop is transparent, each stop's colour runs until
        // the next stop's `t`, the last runs to 1.
        let stops = &color_transfer.stops[..color_transfer.stops.len().min(crate::model::block_model::MAX_COLOR_STOPS)];
        let transparent = entry([0.0; 4]);
        // One entry per 15-bit grade value.
        let mut entries = vec![transparent; CELL_GRADE_MASK as usize + 1];
        if stops.len() >= 2 {
            let per_stop: Vec<([f32; 4], f32)> = stops.iter().map(|stop| entry(stop.color)).collect();
            let mut next_stop = 0;
            for (grade, slot) in entries.iter_mut().enumerate() {
                let t = grade as f32 / CELL_GRADE_MAX;
                while next_stop < stops.len() && t >= stops[next_stop].t {
                    next_stop += 1;
                }
                if next_stop > 0 {
                    *slot = per_stop[next_stop - 1];
                }
            }
        }
        Self {
            entries,
            fallback: entry(fallback_color),
        }
    }

    fn resolve(&self, payload: u16) -> Option<&([f32; 4], f32)> {
        if payload == EMPTY_CELL_PAYLOAD {
            return None;
        }
        if payload & FALLBACK_CELL_FLAG != 0 {
            return Some(&self.fallback);
        }
        Some(&self.entries[(payload & CELL_GRADE_MASK) as usize])
    }
}

struct BrickStyleData {
    aggregates: Vec<[f32; 4]>,
    super_aggregates: Vec<[f32; 4]>,
    uniform_flags: Vec<bool>,
    visibility_mixed_flags: Vec<bool>,
}

/// Level-1 aggregates over `SUPER_FACTOR`^3-brick regions, derived from the
/// per-brick aggregates: rgb = optical-depth·volume-weighted mean of child
/// colours, w = child optical depth averaged over the region's *full* clipped
/// volume (empty bricks dilute it, mirroring how empty cells dilute a brick's
/// aggregate). Combining child optical depths linearly is exact for homogeneous
/// children and a biased approximation otherwise — acceptable at this LOD
/// tier. `w == 0` iff the region has no visible cell.
fn compute_super_aggregates(
    dims: [u32; 3],
    brick_dims: [u32; 3],
    occupied_brick_indices: &[u32],
    brick_aggregates: &[[f32; 4]],
    x_planes: &[f32],
    y_planes: &[f32],
    z_planes: &[f32],
) -> Vec<[f32; 4]> {
    let super_dims = brick_dims.map(|d| d.div_ceil(SUPER_FACTOR as u32));
    let super_count = super_dims.iter().map(|&d| d as usize).product();
    // Cell-plane span of a super region along one axis, clipped to the grid.
    let axis_span = |planes: &[f32], s: usize, dim: usize| {
        let lo = (s * SUPER_FACTOR * BRICK_SIZE).min(dim);
        let hi = ((s + 1) * SUPER_FACTOR * BRICK_SIZE).min(dim);
        f64::from(planes[hi] - planes[lo])
    };
    let brick_span = |planes: &[f32], b: usize, dim: usize| {
        let lo = (b * BRICK_SIZE).min(dim);
        let hi = ((b + 1) * BRICK_SIZE).min(dim);
        f64::from(planes[hi] - planes[lo])
    };

    let mut rgb_sums = vec![[0.0f64; 3]; super_count];
    let mut optical_depth_volume_sums = vec![0.0f64; super_count];
    let bd = brick_dims.map(|d| d as usize);
    let sd = super_dims.map(|d| d as usize);
    for (ordinal, &bindex) in occupied_brick_indices.iter().enumerate() {
        let aggregate = brick_aggregates[ordinal];
        if aggregate[3] <= 0.0 {
            continue;
        }
        let bindex = bindex as usize;
        let bi = bindex % bd[0];
        let bj = (bindex / bd[0]) % bd[1];
        let bk = bindex / (bd[0] * bd[1]);
        let volume = brick_span(x_planes, bi, dims[0] as usize) * brick_span(y_planes, bj, dims[1] as usize) * brick_span(z_planes, bk, dims[2] as usize);
        let weight = f64::from(aggregate[3]) * volume;
        let sindex = ((bk / SUPER_FACTOR) * sd[1] + bj / SUPER_FACTOR) * sd[0] + bi / SUPER_FACTOR;
        optical_depth_volume_sums[sindex] += weight;
        rgb_sums[sindex][0] += f64::from(aggregate[0]) * weight;
        rgb_sums[sindex][1] += f64::from(aggregate[1]) * weight;
        rgb_sums[sindex][2] += f64::from(aggregate[2]) * weight;
    }

    (0..super_count)
        .map(|sindex| {
            let optical_depth_volume = optical_depth_volume_sums[sindex];
            if optical_depth_volume <= 0.0 {
                return [0.0; 4];
            }
            let si = sindex % sd[0];
            let sj = (sindex / sd[0]) % sd[1];
            let sk = sindex / (sd[0] * sd[1]);
            let region_volume = axis_span(x_planes, si, dims[0] as usize) * axis_span(y_planes, sj, dims[1] as usize) * axis_span(z_planes, sk, dims[2] as usize);
            if region_volume <= 0.0 {
                return [0.0; 4];
            }
            [
                (rgb_sums[sindex][0] / optical_depth_volume) as f32,
                (rgb_sums[sindex][1] / optical_depth_volume) as f32,
                (rgb_sums[sindex][2] / optical_depth_volume) as f32,
                (optical_depth_volume / region_volume) as f32,
            ]
        })
        .collect()
}

/// Style-independent upper bound on which bricks can ever need cell-level
/// rendering. Comparing packed payloads (rather than their current colours)
/// ensures a slider edit cannot turn a previously unallocated uniform brick
/// into a non-resident mixed brick.
fn compute_brick_detail_requirements(asset: &BlockVolumeAsset) -> Vec<bool> {
    use rayon::prelude::*;

    let dims = asset.dims.map(|dim| dim as usize);
    let brick_dims = asset.brick_dims.map(|dim| dim as usize);
    asset
        .occupied_brick_indices
        .par_iter()
        .enumerate()
        .map(|(ordinal, &bindex)| {
            let bindex = bindex as usize;
            let bi = bindex % brick_dims[0];
            let bj = (bindex / brick_dims[0]) % brick_dims[1];
            let bk = bindex / (brick_dims[0] * brick_dims[1]);
            let cells = asset.cells.brick_cells(ordinal as u32);
            let mut first = None;
            for lk in 0..BRICK_SIZE.min(dims[2] - bk * BRICK_SIZE) {
                for lj in 0..BRICK_SIZE.min(dims[1] - bj * BRICK_SIZE) {
                    for li in 0..BRICK_SIZE.min(dims[0] - bi * BRICK_SIZE) {
                        let payload = cells[brick_local_index(li, lj, lk)];
                        match first {
                            None => first = Some(payload),
                            Some(first) if first != payload => return true,
                            Some(_) => {}
                        }
                    }
                }
            }
            false
        })
        .collect()
}

fn compute_brick_style_data(asset: &BlockVolumeAsset, color_transfer: &ColorTransferFunction, fallback_color: [f32; 4]) -> BrickStyleData {
    use rayon::prelude::*;

    let mut fallback_color = fallback_color;
    fallback_color[3] = fallback_color[3].clamp(0.0, 1.0);
    let lut = VolumeRampLut::build(color_transfer, fallback_color);
    let dims = [asset.dims[0] as usize, asset.dims[1] as usize, asset.dims[2] as usize];
    let brick_dims = [asset.brick_dims[0] as usize, asset.brick_dims[1] as usize, asset.brick_dims[2] as usize];
    let cell_lengths = |planes: &[f32]| -> Vec<f32> { planes.windows(2).map(|pair| pair[1] - pair[0]).collect() };
    let x_lengths = cell_lengths(&asset.x_planes);
    let y_lengths = cell_lengths(&asset.y_planes);
    let z_lengths = cell_lengths(&asset.z_planes);

    let per_brick: Vec<([f32; 4], bool, bool)> = asset
        .occupied_brick_indices
        .par_iter()
        .enumerate()
        .map(|(ordinal, &bindex)| {
            let bindex = bindex as usize;
            let ordinal = ordinal as u32;
            let bi = bindex % brick_dims[0];
            let bj = (bindex / brick_dims[0]) % brick_dims[1];
            let bk = bindex / (brick_dims[0] * brick_dims[1]);
            let brick_cells = asset.cells.brick_cells(ordinal);
            let mut volume_sum = 0.0f64;
            let mut optical_depth_volume_sum = 0.0f64;
            let mut rgb_sum = [0.0f64; 3];
            let mut first_appearance: Option<Option<[f32; 4]>> = None;
            let mut first_visibility = None;
            let mut uniform = true;
            let mut mixed_visibility = false;
            for lk in 0..BRICK_SIZE.min(dims[2] - bk * BRICK_SIZE) {
                let k = bk * BRICK_SIZE + lk;
                for lj in 0..BRICK_SIZE.min(dims[1] - bj * BRICK_SIZE) {
                    let j = bj * BRICK_SIZE + lj;
                    for li in 0..BRICK_SIZE.min(dims[0] - bi * BRICK_SIZE) {
                        let i = bi * BRICK_SIZE + li;
                        let cell_volume = (x_lengths[i] * y_lengths[j] * z_lengths[k]) as f64;
                        volume_sum += cell_volume;
                        let payload = brick_cells[brick_local_index(li, lj, lk)];
                        let visible = lut.resolve(payload).filter(|(rgba, _)| rgba[3] >= VISIBLE_ALPHA_EPSILON);
                        let cell_visible = visible.is_some();
                        match first_visibility {
                            None => first_visibility = Some(cell_visible),
                            Some(first) => mixed_visibility |= first != cell_visible,
                        }
                        let appearance = visible.map(|(rgba, _)| *rgba);
                        match first_appearance {
                            None => first_appearance = Some(appearance),
                            Some(first) => uniform &= first == appearance,
                        }
                        let Some((rgba, optical_depth)) = visible else {
                            continue;
                        };
                        let weight = f64::from(*optical_depth) * cell_volume;
                        optical_depth_volume_sum += weight;
                        rgb_sum[0] += f64::from(rgba[0]) * weight;
                        rgb_sum[1] += f64::from(rgba[1]) * weight;
                        rgb_sum[2] += f64::from(rgba[2]) * weight;
                    }
                }
            }
            let aggregate = if optical_depth_volume_sum > 0.0 && volume_sum > 0.0 {
                [
                    (rgb_sum[0] / optical_depth_volume_sum) as f32,
                    (rgb_sum[1] / optical_depth_volume_sum) as f32,
                    (rgb_sum[2] / optical_depth_volume_sum) as f32,
                    (optical_depth_volume_sum / volume_sum) as f32,
                ]
            } else {
                [0.0; 4]
            };
            (aggregate, uniform, mixed_visibility)
        })
        .collect();

    let mut aggregates = Vec::with_capacity(per_brick.len());
    let mut uniform_flags = Vec::with_capacity(per_brick.len());
    let mut visibility_mixed_flags = Vec::with_capacity(per_brick.len());
    for (aggregate, uniform, mixed_visibility) in per_brick {
        aggregates.push(aggregate);
        uniform_flags.push(uniform);
        visibility_mixed_flags.push(mixed_visibility);
    }
    let super_aggregates = if volume_super_bricks_enabled() {
        compute_super_aggregates(
            asset.dims,
            asset.brick_dims,
            &asset.occupied_brick_indices,
            &aggregates,
            &asset.x_planes,
            &asset.y_planes,
            &asset.z_planes,
        )
    } else {
        Vec::new()
    };
    BrickStyleData {
        aggregates,
        super_aggregates,
        uniform_flags,
        visibility_mixed_flags,
    }
}

fn block_volume_planes(block_model: &OpenBlockModel) -> Option<(Vec<f32>, Vec<f32>, Vec<f32>)> {
    let dims = block_model.model.metadata.dims;
    let regular_count = dims.into_iter().try_fold(1usize, |acc, dim| acc.checked_mul(dim))?;
    if !block_model.model.metadata.is_irregular && regular_count == block_model.model.metadata.n_blocks && dims.into_iter().all(|dim| dim > 0) {
        let lower = block_model.model.metadata.lower;
        let upper = block_model.model.metadata.upper;
        return Some((
            regular_planes(lower.x, upper.x, dims[0]),
            regular_planes(lower.y, upper.y, dims[1]),
            regular_planes(lower.z, upper.z, dims[2]),
        ));
    }

    let mut x_planes = Vec::new();
    let mut y_planes = Vec::new();
    let mut z_planes = Vec::new();
    for index in block_model.renderable_block_indices.iter() {
        let Some(block) = block_model.blocks.get(index) else {
            continue;
        };
        x_planes.push(block.lower.x as f32);
        x_planes.push(block.upper.x as f32);
        y_planes.push(block.lower.y as f32);
        y_planes.push(block.upper.y as f32);
        z_planes.push(block.lower.z as f32);
        z_planes.push(block.upper.z as f32);
    }
    dedup_planes(&mut x_planes);
    dedup_planes(&mut y_planes);
    dedup_planes(&mut z_planes);
    Some((x_planes, y_planes, z_planes))
}

/// Cell-boundary planes for a regular grid, computed in f64 with the same
/// arithmetic `RegularBlockBounds::get` uses for implicit block bounds
/// (`lower + index * step`), so a regular block's f64 bound cast to f32 lands
/// exactly on its plane. Computing these in f32 drifted from the f64 bounds
/// by more than the match tolerance at some indices in large models, and
/// every block on such a plane was silently dropped from the volume — the
/// "missing strips" failure mode.
fn regular_planes(lower: f64, upper: f64, cells: usize) -> Vec<f32> {
    let step = (upper - lower) / cells as f64;
    (0..=cells).map(|index| (lower + step * index as f64) as f32).collect()
}

/// Tolerance for treating two plane coordinates as the same plane. f32
/// spacing grows with magnitude (one ULP at 100,000 is ~0.0078), so a purely
/// absolute epsilon rejects lookups that miss by nothing more than cast/
/// arithmetic rounding once local coordinates reach the thousands. Four
/// representable steps absorb that noise without the up-to-eight-ULP window
/// produced by `f32::EPSILON * value` near the top of each binade.
fn plane_tolerance(value: f32) -> f32 {
    let magnitude = value.abs();
    let next = magnitude.next_up();
    let ulp = if next.is_finite() { next - magnitude } else { magnitude - magnitude.next_down() };
    (4.0 * ulp).max(PLANE_DEDUP_EPSILON)
}

fn planes_match(a: f32, b: f32) -> bool {
    (a - b).abs() <= plane_tolerance(a).min(plane_tolerance(b))
}

fn dedup_planes(planes: &mut Vec<f32>) {
    planes.sort_by(f32::total_cmp);
    planes.dedup_by(|a, b| planes_match(*a, *b));
}

fn plane_index(planes: &[f32], value: f32) -> Option<usize> {
    let insertion = planes.partition_point(|plane| *plane < value);
    let before = insertion.checked_sub(1).filter(|&index| planes_match(planes[index], value));
    let after = (insertion < planes.len() && planes_match(planes[insertion], value)).then_some(insertion);
    match (before, after) {
        (Some(a), Some(b)) if (planes[a] - value).abs() <= (planes[b] - value).abs() => Some(a),
        (Some(_), Some(b)) => Some(b),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}

/// Plane lookup strategy, chosen once per axis. Uniformly spaced plane
/// arrays — `regular_planes` output and most explicit-bounds grids — resolve
/// in O(1) arithmetic; anything else (sub-blocked models) falls back to
/// binary search. Six lookups per block made the searches dominate the
/// volume-build profile.
enum PlaneLookup<'a> {
    Uniform { planes: &'a [f32], origin: f32, inv_step: f32 },
    Binary(&'a [f32]),
}

/// `(origin, step)` when every plane sits within [`plane_tolerance`] of a
/// uniformly spaced lattice, i.e. the axis can be resolved arithmetically
/// instead of by search. Shared by the CPU [`PlaneLookup`] and the GPU
/// raycaster's uniform-axis fast path.
fn uniform_plane_spacing(planes: &[f32]) -> Option<(f32, f32)> {
    let (&first, &last) = (planes.first()?, planes.last()?);
    if planes.len() < 2 {
        return None;
    }
    let step = (last - first) / (planes.len() - 1) as f32;
    if step <= 0.0 {
        return None;
    }
    planes
        .iter()
        .enumerate()
        .all(|(index, &plane)| planes_match(plane, first + step * index as f32))
        .then_some((first, step))
}

impl<'a> PlaneLookup<'a> {
    fn new(planes: &'a [f32]) -> Self {
        match uniform_plane_spacing(planes) {
            Some((origin, step)) => Self::Uniform {
                planes,
                origin,
                inv_step: step.recip(),
            },
            None => Self::Binary(planes),
        }
    }

    /// Same contract as [`plane_index`]: the index of the plane within
    /// [`plane_tolerance`] of `value`, or `None`.
    fn index(&self, value: f32) -> Option<usize> {
        match *self {
            Self::Uniform { planes, origin, inv_step } => {
                // `+ 0.5` then truncation rounds to nearest without a libm
                // `round` call; the negative guard keeps values below the
                // first plane from truncating up to index 0. The final
                // comparison re-checks against the actual plane, so a false
                // candidate never resolves.
                let shifted = (value - origin) * inv_step + 0.5;
                if shifted < 0.0 {
                    return None;
                }
                let index = shifted as usize;
                (index < planes.len() && planes_match(planes[index], value)).then_some(index)
            }
            Self::Binary(planes) => plane_index(planes, value),
        }
    }
}

fn brick_grid_dims(dims: [usize; 3]) -> [usize; 3] {
    [dims[0].div_ceil(BRICK_SIZE), dims[1].div_ceil(BRICK_SIZE), dims[2].div_ceil(BRICK_SIZE)]
}

fn brick_index(brick_dims: [usize; 3], bi: usize, bj: usize, bk: usize) -> usize {
    (bk * brick_dims[1] + bj) * brick_dims[0] + bi
}

fn brick_local_index(li: usize, lj: usize, lk: usize) -> usize {
    (lk * BRICK_SIZE + lj) * BRICK_SIZE + li
}

fn pack_cell_payload(grade: f32, has_grade: bool) -> u16 {
    if has_grade && grade >= 0.0 {
        let quantized = (grade.clamp(0.0, 1.0) * CELL_GRADE_MAX).round() as u16;
        quantized & CELL_GRADE_MASK
    } else {
        FALLBACK_CELL_FLAG
    }
}

fn reference_cell_length(x_planes: &[f32], y_planes: &[f32], z_planes: &[f32]) -> f32 {
    let avg_delta = |planes: &[f32]| -> f32 {
        if planes.len() < 2 {
            return 1.0;
        }
        let span = (planes[planes.len() - 1] - planes[0]).abs();
        (span / (planes.len() - 1) as f32).max(1.0e-6)
    };
    (avg_delta(x_planes) * avg_delta(y_planes) * avg_delta(z_planes)).cbrt()
}
