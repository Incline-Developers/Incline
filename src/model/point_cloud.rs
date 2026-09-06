use std::{cmp::Ordering, collections::BinaryHeap, path::PathBuf, sync::Arc};

use glam::DVec3;
use rayon::prelude::*;

use crate::model::project::ProjectItemState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PointCloudId(pub(crate) u64);

/// The decoded contents of a point cloud file, produced on a background
/// thread and sent back to the main thread via channel.
pub(crate) struct LoadedPointCloud {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) points: Arc<Vec<DVec3>>,
    /// Source-order packed RGBA8 values, when the input provides them.
    pub(crate) colors: Option<Arc<Vec<u32>>>,
    /// Spatially ordered, cloud-local render data prepared by the loader.
    pub(crate) prepared: Arc<PreparedPointCloud>,
    pub(crate) bounds: (DVec3, DVec3),
}

#[derive(Clone)]
pub(crate) struct OpenPointCloud {
    pub(crate) id: PointCloudId,
    pub(crate) state: ProjectItemState,
    pub(crate) name: String,
    pub(crate) points: Arc<Vec<DVec3>>,
    /// Source-order packed RGBA8 values, retained for lossless interchange.
    pub(crate) colors: Option<Arc<Vec<u32>>>,
    pub(crate) prepared: Arc<PreparedPointCloud>,
    pub(crate) bounds: (DVec3, DVec3),
    /// Uniform colour used when the file carries no per-point colours.
    pub(crate) color: [f32; 4],
    /// Screen-facing splat width in source/world metres.
    pub(crate) point_size: f32,
}

impl OpenPointCloud {
    pub(crate) fn entity_id(&self) -> crate::model::SceneEntityId {
        crate::model::SceneEntityId::PointCloud(self.id)
    }
}

/// Position-only instance used by clouds without per-point colours.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct PointPosition {
    pub(crate) pos: [f32; 3],
}

/// Position and packed RGBA8 used by coloured clouds.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct PointInstance {
    pub(crate) pos: [f32; 3],
    pub(crate) color: u32,
}

pub(crate) enum PreparedPointData {
    Uncolored(Vec<PointPosition>),
    Colored(Vec<PointInstance>),
}

impl PreparedPointData {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Uncolored(points) => bytemuck::cast_slice(points),
            Self::Colored(points) => bytemuck::cast_slice(points),
        }
    }

    pub(crate) fn position(&self, index: usize) -> Option<[f32; 3]> {
        match self {
            Self::Uncolored(points) => points.get(index).map(|point| point.pos),
            Self::Colored(points) => points.get(index).map(|point| point.pos),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PointPickGroup {
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) bounds_min: glam::Vec3,
    pub(crate) bounds_max: glam::Vec3,
}

pub(crate) struct PreparedPointChunk {
    /// Points are ordered so every power-of-two representative set down to a
    /// single point is a prefix. All LODs therefore share one CPU and GPU
    /// buffer.
    pub(crate) data: PreparedPointData,
    /// Full resolution through one representative point instance counts.
    pub(crate) level_counts: [u32; POINT_CLOUD_LOD_LEVELS],
    /// Robust (median) nearest-neighbour distance at full resolution, in cloud
    /// units. The renderer scales this by the LOD decimation to estimate the
    /// on-screen point spacing and pick a prefix dense enough to leave no gaps,
    /// replacing a per-frame screen-space coverage measurement. Zero when the
    /// chunk has too few points to define a spacing.
    pub(crate) base_spacing: f32,
    pub(crate) bounds_min: glam::Vec3,
    pub(crate) bounds_max: glam::Vec3,
    /// Small Morton-coherent ranges used for CPU visibility queries without
    /// scanning an entire 256k-point render chunk on every snap poll.
    pub(crate) pick_groups: Vec<PointPickGroup>,
}

pub(crate) struct PreparedPointCloud {
    pub(crate) origin: DVec3,
    pub(crate) chunks: Vec<PreparedPointChunk>,
    pub(crate) colored: bool,
}

const POINTS_PER_SPATIAL_CHUNK: usize = 256 * 1024;
const POINTS_PER_PICK_GROUP: usize = 512;
// A 256k-point chunk is 2^18 points. Retaining all 19 power-of-two
// resolutions lets a distant chunk fall all the way to one representative
// instead of bottoming out at 1,024 splats (the previous nine-level limit).
// This only grows the per-chunk count table; point storage remains unchanged.
pub(crate) const POINT_CLOUD_LOD_LEVELS: usize = 19;

#[derive(Clone, Copy)]
struct MortonPointIndex {
    key: u64,
    source_index: usize,
}

/// Convert source doubles into a spatially coherent, render-ready hierarchy.
/// This is called by the point-cloud loader, never by the render thread.
pub(crate) fn prepare_for_render(points: &[DVec3], colors: Option<&[u32]>, bounds: (DVec3, DVec3)) -> PreparedPointCloud {
    let origin = (bounds.0 + bounds.1) * 0.5;
    let extent = (bounds.1 - bounds.0).max(DVec3::splat(f64::EPSILON));

    // Keep only a compact key/index record while sorting. This calculates the
    // expensive Morton key exactly once, lets the parallel unstable sort move
    // 16-byte records with cheap u64 comparisons, and avoids both Rayon's
    // sequential cached-key permutation and a full intermediate render-point
    // allocation. Final f32 positions are written directly into their chunks.
    let mut sorted = points
        .par_iter()
        .enumerate()
        .filter(|(_, point)| point.is_finite())
        .map(|(source_index, point)| {
            let local = (*point - origin).as_vec3().to_array();
            MortonPointIndex {
                key: morton_key(local, origin, bounds.0, extent),
                source_index,
            }
        })
        .collect::<Vec<_>>();
    // Include the source index so coincident quantized points retain a stable
    // order even though the faster unstable parallel sort is used.
    sorted.par_sort_unstable_by_key(|point| (point.key, point.source_index));
    if let Some(colors) = colors {
        PreparedPointCloud {
            origin,
            chunks: sorted
                .par_chunks(POINTS_PER_SPATIAL_CHUNK)
                .map(|chunk| build_colored_chunk(chunk, points, colors, origin))
                .collect(),
            colored: true,
        }
    } else {
        PreparedPointCloud {
            origin,
            chunks: sorted
                .par_chunks(POINTS_PER_SPATIAL_CHUNK)
                .map(|chunk| build_uncolored_chunk(chunk, points, origin))
                .collect(),
            colored: false,
        }
    }
}

pub(crate) fn finite_bounds(points: &[DVec3]) -> Option<(DVec3, DVec3)> {
    let (min, max) = points
        .par_iter()
        .copied()
        .filter(|point| point.is_finite())
        .fold(
            || (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
            |(min, max), point| (min.min(point), max.max(point)),
        )
        .reduce(
            || (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
            |(min_a, max_a), (min_b, max_b)| (min_a.min(min_b), max_a.max(max_b)),
        );
    (min.is_finite() && max.is_finite()).then_some((min, max))
}

fn build_colored_chunk(sorted: &[MortonPointIndex], source_points: &[DVec3], colors: &[u32], origin: DVec3) -> PreparedPointChunk {
    let (ordered, level_counts) = lod_prefix_order(sorted, |point| {
        let source_index = point.source_index;
        PointInstance {
            pos: (source_points[source_index] - origin).as_vec3().to_array(),
            color: colors.get(source_index).copied().unwrap_or(0xffff_ffff),
        }
    });
    let (bounds_min, bounds_max) = local_bounds(ordered.iter().map(|point| point.pos));
    let pick_groups = build_pick_groups(&ordered, level_counts, |point| point.pos);
    PreparedPointChunk {
        data: PreparedPointData::Colored(ordered),
        level_counts,
        base_spacing: chunk_base_spacing(sorted, source_points, origin),
        bounds_min,
        bounds_max,
        pick_groups,
    }
}

fn build_uncolored_chunk(sorted: &[MortonPointIndex], source_points: &[DVec3], origin: DVec3) -> PreparedPointChunk {
    let (ordered, level_counts) = lod_prefix_order(sorted, |point| PointPosition {
        pos: (source_points[point.source_index] - origin).as_vec3().to_array(),
    });
    let (bounds_min, bounds_max) = local_bounds(ordered.iter().map(|point| point.pos));
    let pick_groups = build_pick_groups(&ordered, level_counts, |point| point.pos);
    PreparedPointChunk {
        data: PreparedPointData::Uncolored(ordered),
        level_counts,
        base_spacing: chunk_base_spacing(sorted, source_points, origin),
        bounds_min,
        bounds_max,
        pick_groups,
    }
}

/// Estimate the full-resolution point spacing of a Morton-sorted chunk. Morton
/// order places spatial neighbours adjacently, so the nearest of a small window
/// of successors approximates each point's true nearest neighbour without a
/// spatial index. A high percentile - not the median - is used deliberately:
/// the renderer sizes a prefix so splats at this spacing touch, and targeting
/// the 90th percentile means ~90% of the surface is covered (matching the
/// coverage goal of the screen-space probing this replaces). The window-min
/// also rejects the occasional large jump where the curve crosses a cell
/// boundary.
const BASE_SPACING_COVERAGE_PERCENTILE: f32 = 0.9;

fn chunk_base_spacing(sorted: &[MortonPointIndex], source_points: &[DVec3], origin: DVec3) -> f32 {
    const NEIGHBOUR_WINDOW: usize = 8;
    if sorted.len() < 2 {
        return 0.0;
    }
    let position = |point: &MortonPointIndex| (source_points[point.source_index] - origin).as_vec3();
    let mut spacings = sorted
        .iter()
        .enumerate()
        .filter_map(|(index, point)| {
            let here = position(point);
            let window_end = (index + 1 + NEIGHBOUR_WINDOW).min(sorted.len());
            sorted[index + 1..window_end]
                .iter()
                .map(|other| here.distance_squared(position(other)))
                .min_by(f32::total_cmp)
                .filter(|distance| distance.is_finite())
                .map(f32::sqrt)
        })
        .collect::<Vec<_>>();
    if spacings.is_empty() {
        return 0.0;
    }
    let percentile = (((spacings.len() - 1) as f32) * BASE_SPACING_COVERAGE_PERCENTILE).round() as usize;
    spacings.select_nth_unstable_by(percentile, f32::total_cmp);
    spacings[percentile]
}

fn build_pick_groups<T>(points: &[T], level_counts: [u32; POINT_CLOUD_LOD_LEVELS], position: impl Fn(&T) -> [f32; 3] + Copy) -> Vec<PointPickGroup> {
    let mut groups = Vec::new();
    let mut tier_start = 0;
    for tier_end in level_counts.iter().rev().map(|count| *count as usize) {
        for start in (tier_start..tier_end).step_by(POINTS_PER_PICK_GROUP) {
            let end = (start + POINTS_PER_PICK_GROUP).min(tier_end);
            let (bounds_min, bounds_max) = local_bounds(points[start..end].iter().map(position));
            groups.push(PointPickGroup {
                start: start as u32,
                end: end as u32,
                bounds_min,
                bounds_max,
            });
        }
        tier_start = tier_end;
    }
    debug_assert_eq!(tier_start, points.len());
    groups
}

/// Coarse prefixes are selected by subdividing occupied Morton space instead
/// of point-array indices. This gives an isolated point its own representative
/// as soon as its spatial cell separates from a dense cluster. The first 1,024
/// points cover every LOD used for an initial GPU upload; finer prefixes fall
/// back to the inexpensive shuffled-Morton order because density differences
/// matter progressively less as a chunk approaches full resolution.
fn lod_prefix_order<U: Copy>(points: &[MortonPointIndex], map: impl Fn(&MortonPointIndex) -> U + Copy) -> (Vec<U>, [u32; POINT_CLOUD_LOD_LEVELS]) {
    let (indices, level_counts) = density_aware_prefix_indices(points);
    let ordered = indices.into_iter().map(|index| map(&points[index])).collect();
    (ordered, level_counts)
}

const DENSITY_AWARE_PREFIX_POINTS: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MortonCell {
    start: usize,
    end: usize,
    representative: usize,
    split_bit: u32,
}

impl Ord for MortonCell {
    fn cmp(&self, other: &Self) -> Ordering {
        self.split_bit
            .cmp(&other.split_bit)
            // At equal spatial depth, process cells in Morton order for
            // deterministic, spatially coherent refinement.
            .then_with(|| other.start.cmp(&self.start))
            .then_with(|| other.end.cmp(&self.end))
    }
}

impl PartialOrd for MortonCell {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn density_aware_prefix_indices(points: &[MortonPointIndex]) -> (Vec<usize>, [u32; POINT_CLOUD_LOD_LEVELS]) {
    let mut level_counts = [0; POINT_CLOUD_LOD_LEVELS];
    if points.is_empty() {
        return (Vec::new(), level_counts);
    }

    let mut ordered = Vec::with_capacity(points.len());
    let mut selected = vec![false; points.len()];
    let mut cells = BinaryHeap::new();
    let root_representative = representative_near_morton_center(points, 0, points.len());
    ordered.push(root_representative);
    selected[root_representative] = true;
    if let Some(root) = morton_cell(points, 0, points.len(), root_representative) {
        cells.push(root);
    }

    // This is the previous fixed-stride order. It remains a useful cheap and
    // deterministic fill order once spatial representatives have established
    // the coarse hierarchy, and guarantees every source point appears once.
    let fallback = shuffled_morton_indices(points.len());
    let mut fallback_cursor = 0;

    for level in (0..POINT_CLOUD_LOD_LEVELS).rev() {
        let stride = 1usize << level;
        let target = points.len().div_ceil(stride);
        let density_target = target.min(DENSITY_AWARE_PREFIX_POINTS);

        while ordered.len() < density_target {
            let Some(cell) = cells.pop() else {
                break;
            };
            let (left, right, introduced) = split_morton_cell(points, cell);
            debug_assert!(!selected[introduced]);
            selected[introduced] = true;
            ordered.push(introduced);
            if let Some(left) = left {
                cells.push(left);
            }
            if let Some(right) = right {
                cells.push(right);
            }
        }

        while ordered.len() < target {
            let index = fallback[fallback_cursor];
            fallback_cursor += 1;
            if !selected[index] {
                selected[index] = true;
                ordered.push(index);
            }
        }
        level_counts[level] = ordered.len() as u32;
    }
    debug_assert!(selected.into_iter().all(|is_selected| is_selected));
    (ordered, level_counts)
}

fn shuffled_morton_indices(point_count: usize) -> Vec<usize> {
    let mut ordered = Vec::with_capacity(point_count);
    for level in (0..POINT_CLOUD_LOD_LEVELS).rev() {
        let stride = 1usize << level;
        if level + 1 == POINT_CLOUD_LOD_LEVELS {
            ordered.extend((0..point_count).step_by(stride));
        } else {
            ordered.extend((stride..point_count).step_by(stride << 1));
        }
    }
    debug_assert_eq!(ordered.len(), point_count);
    ordered
}

fn morton_cell(points: &[MortonPointIndex], start: usize, end: usize, representative: usize) -> Option<MortonCell> {
    let differing_bits = points[start].key ^ points[end - 1].key;
    (differing_bits != 0).then(|| MortonCell {
        start,
        end,
        representative,
        split_bit: u64::BITS - 1 - differing_bits.leading_zeros(),
    })
}

fn split_morton_cell(points: &[MortonPointIndex], cell: MortonCell) -> (Option<MortonCell>, Option<MortonCell>, usize) {
    let mask = 1u64 << cell.split_bit;
    let split = cell.start + points[cell.start..cell.end].partition_point(|point| point.key & mask == 0);
    debug_assert!(split > cell.start && split < cell.end);

    let (left_representative, right_representative, introduced) = if cell.representative < split {
        let right = representative_near_morton_center(points, split, cell.end);
        (cell.representative, right, right)
    } else {
        let left = representative_near_morton_center(points, cell.start, split);
        (left, cell.representative, left)
    };
    (
        morton_cell(points, cell.start, split, left_representative),
        morton_cell(points, split, cell.end, right_representative),
        introduced,
    )
}

fn representative_near_morton_center(points: &[MortonPointIndex], start: usize, end: usize) -> usize {
    debug_assert!(start < end);
    let first = points[start].key;
    let last = points[end - 1].key;
    let target = first + (last - first) / 2;
    let insertion = start + points[start..end].partition_point(|point| point.key < target);
    let after = insertion.min(end - 1);
    if after == start {
        return after;
    }
    let before = after - 1;
    let before_distance = points[before].key.abs_diff(target);
    let after_distance = points[after].key.abs_diff(target);
    if before_distance <= after_distance { before } else { after }
}

fn local_bounds(points: impl Iterator<Item = [f32; 3]>) -> (glam::Vec3, glam::Vec3) {
    points
        .map(glam::Vec3::from_array)
        .fold((glam::Vec3::splat(f32::INFINITY), glam::Vec3::splat(f32::NEG_INFINITY)), |(min, max), point| {
            (min.min(point), max.max(point))
        })
}

fn morton_key(local: [f32; 3], origin: DVec3, min: DVec3, extent: DVec3) -> u64 {
    let world = DVec3::from_array(local.map(f64::from)) + origin;
    let normalized = ((world - min) / extent).clamp(DVec3::ZERO, DVec3::ONE);
    let scale = ((1u32 << 21) - 1) as f64;
    let x = (normalized.x * scale) as u32;
    let y = (normalized.y * scale) as u32;
    let z = (normalized.z * scale) as u32;
    interleave_21(x) | (interleave_21(y) << 1) | (interleave_21(z) << 2)
}

fn interleave_21(value: u32) -> u64 {
    let mut value = u64::from(value & 0x1f_ffff);
    value = (value | value << 32) & 0x001f_0000_0000_ffff;
    value = (value | value << 16) & 0x001f_0000_ff00_00ff;
    value = (value | value << 8) & 0x100f_00f0_0f00_f00f;
    value = (value | value << 4) & 0x10c3_0c30_c30c_30c3;
    (value | value << 2) & 0x1249_2492_4924_9249
}
