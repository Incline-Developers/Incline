//! Terrain TIN generation from survey point clouds.

use glam::DVec3;
use rayon::prelude::*;

use super::*;
use crate::model::point_cloud::PointCloudId;

#[derive(Clone, Copy)]
struct TerrainVertex {
    position: spade::Point2<f64>,
    z: f64,
}

impl spade::HasPosition for TerrainVertex {
    type Scalar = f64;

    fn position(&self) -> spade::Point2<Self::Scalar> {
        self.position
    }
}

/// How the vertex budget is specified by the caller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TerrainBudget {
    /// Percentage of the source points; accepts fractions such as 0.125.
    Percent(f64),
    /// Absolute maximum vertex count.
    Count(usize),
}

/// Everything the terrain TIN reconstruction needs beyond the point cloud.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TerrainTinParams {
    pub(crate) name: String,
    pub(crate) budget: TerrainBudget,
    pub(crate) max_edge: f64,
    pub(crate) sampler: TerrainSampler,
    /// Candidate fine cells per budgeted vertex for the adaptive sampler; higher
    /// gives the greedy cut more freedom at the cost of build time.
    pub(crate) candidate_multiplier: u32,
    /// Bridge (fill) holes and boundary concavities narrower than this distance;
    /// wider gaps stay open. Zero fills only sub-cell scan gaps.
    pub(crate) hole_fill_distance: f64,
}

/// Resolve a budget specification to an absolute vertex target.
pub(crate) fn terrain_budget_target(point_count: usize, budget: TerrainBudget) -> usize {
    if point_count <= 3 {
        return point_count;
    }
    let target = match budget {
        TerrainBudget::Percent(percent) => (point_count as f64 * percent.clamp(0.0, 100.0) / 100.0).round() as usize,
        TerrainBudget::Count(count) => count,
    };
    target.clamp(3, point_count)
}

impl<'a> App<'a> {
    /// Build an open XY Delaunay terrain surface from a loaded point cloud on a
    /// background job and register it like any generated triangulation.
    pub(crate) fn run_point_cloud_tin(&mut self, cloud_id: PointCloudId, params: TerrainTinParams) -> Result<()> {
        let cloud = self
            .point_clouds
            .iter()
            .find(|cloud| cloud.id == cloud_id)
            .ok_or_else(|| anyhow::anyhow!("The selected point cloud is no longer loaded"))?;
        let points = cloud.points.clone();
        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| -> Result<crate::model::triangulation::GeneratedTriangulation> {
            reconstruct_terrain_tin_from_point_cloud(&points, &params, cancel, progress)
        };
        let apply = move |app: &mut App, result: Result<crate::model::triangulation::GeneratedTriangulation>| match result {
            Ok(generated) => app.insert_generated_triangulation(generated),
            Err(error) => {
                userspace_warn!("Point cloud TIN failed: {error:#}");
            }
        };
        self.spawn_job_reporting_progress("Point cloud TIN...", vec![crate::app::jobs::JobKey::PointCloud(cloud_id)], compute, apply);
        Ok(())
    }
}

/// Selects how the point budget is distributed before triangulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerrainSampler {
    /// Uniform XY grid of cell means (fast, feature-agnostic).
    Grid,
    /// Adaptive quadtree that concentrates vertices on complex terrain.
    Adaptive,
}

fn subsample_terrain(
    points: &[DVec3],
    max_points: usize,
    sampler: TerrainSampler,
    candidate_multiplier: u32,
    hole_fill_distance: f64,
    cancel: &crate::app::jobs::CancelFlag,
) -> Result<(Vec<DVec3>, Option<OccupancyGrid>)> {
    match sampler {
        TerrainSampler::Grid => spatial_grid_subsample_terrain(points, max_points, hole_fill_distance, cancel),
        TerrainSampler::Adaptive => adaptive_quadtree_subsample_terrain(points, max_points, candidate_multiplier, hole_fill_distance, cancel),
    }
}

/// Occupancy dilation radius (in cells) that bridges gaps up to
/// `hole_fill_distance` wide. At least one cell, so the void test always
/// tolerates the isolated empty cells that pepper real coverage.
fn hole_fill_dilation(hole_fill_distance: f64, cell_size: f64) -> i64 {
    let radius = (hole_fill_distance / (2.0 * cell_size)).ceil();
    if radius.is_finite() { (radius as i64).max(1) } else { 1 }
}

fn reconstruct_terrain_tin_from_point_cloud(
    points: &[DVec3],
    params: &TerrainTinParams,
    cancel: &crate::app::jobs::CancelFlag,
    progress: &crate::model::progress::Progress,
) -> Result<crate::model::triangulation::GeneratedTriangulation> {
    if points.len() < 3 {
        anyhow::bail!("The point cloud has too few points to triangulate a terrain surface");
    }

    let max_points = terrain_budget_target(points.len(), params.budget);
    // Subsampling, then triangulation: each is a single pass, so the bar steps
    // between them on an estimate of their relative cost.
    let (sampled, occupancy) = subsample_terrain(points, max_points, params.sampler, params.candidate_multiplier, params.hole_fill_distance, cancel)?;
    progress.set_fraction(0.4);
    if sampled.len() < points.len() {
        userspace_log!("Terrain TIN: spatially subsampled {} of {} points", sampled.len(), points.len());
    }
    let generated = reconstruct_terrain_tin(sampled, occupancy.as_ref(), params.name.clone(), params.max_edge, cancel)?;
    progress.set_fraction(1.0);
    Ok(generated)
}

fn reconstruct_terrain_tin(
    sampled: Vec<DVec3>,
    occupancy: Option<&OccupancyGrid>,
    name: String,
    max_edge: f64,
    cancel: &crate::app::jobs::CancelFlag,
) -> Result<crate::model::triangulation::GeneratedTriangulation> {
    use spade::{DelaunayTriangulation, Triangulation as _};

    if sampled.len() < 3 {
        anyhow::bail!("The point cloud has too few points to triangulate a terrain surface");
    }

    let unique = deduplicate_terrain_xy(sampled, cancel)?;
    if cancel.is_cancelled() {
        anyhow::bail!("Terrain TIN reconstruction cancelled");
    }
    let tin: DelaunayTriangulation<TerrainVertex> = DelaunayTriangulation::bulk_load(unique).map_err(|error| anyhow::anyhow!("Terrain TIN bulk load failed: {error:?}"))?;

    if tin.num_vertices() < 3 {
        anyhow::bail!("The point cloud has fewer than 3 unique XY points");
    }
    if cancel.is_cancelled() {
        anyhow::bail!("Terrain TIN reconstruction cancelled");
    }

    let vertices: Vec<mesh_data::Vertex> = tin
        .vertices()
        .map(|vertex| {
            let position = vertex.position();
            mesh_data::Vertex::new(position.x, position.y, vertex.data().z)
        })
        .collect();
    let face_handles: Vec<_> = tin.inner_faces().map(|face| face.fix()).collect();
    let max_edge_sq = (max_edge > 0.0).then_some(max_edge * max_edge);
    let faces: Vec<[u32; 3]> = face_handles.par_iter().filter_map(|handle| terrain_face(&tin, *handle, max_edge_sq, occupancy)).collect();

    if cancel.is_cancelled() {
        anyhow::bail!("Terrain TIN reconstruction cancelled");
    }

    if faces.is_empty() {
        anyhow::bail!("Terrain TIN produced no faces - increase Max edge or use more input points");
    }

    userspace_log!(
        "Terrain TIN: triangulated {} unique XY points into {} faces{}",
        vertices.len(),
        faces.len(),
        if max_edge > 0.0 {
            format!(" (max edge {max_edge:.3})")
        } else {
            " (max edge disabled)".to_owned()
        }
    );
    session::build_generated_triangulation(name, vertices, faces, TriSurfaceType::Surface, crate::model::triangulation::unique_edges)
}

fn deduplicate_terrain_xy(sampled: Vec<DVec3>, cancel: &crate::app::jobs::CancelFlag) -> Result<Vec<TerrainVertex>> {
    let mut finite: Vec<DVec3> = sampled.into_par_iter().filter(|point| point.is_finite()).collect();
    finite.par_sort_unstable_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)).then_with(|| a.z.total_cmp(&b.z)));
    if cancel.is_cancelled() {
        anyhow::bail!("Terrain TIN reconstruction cancelled");
    }

    let mut unique = Vec::with_capacity(finite.len());
    let mut start = 0usize;
    while start < finite.len() {
        if start.is_multiple_of(262_144) && cancel.is_cancelled() {
            anyhow::bail!("Terrain TIN reconstruction cancelled");
        }
        let mut end = start + 1;
        let mut z_sum = finite[start].z;
        while end < finite.len() && finite[end].x == finite[start].x && finite[end].y == finite[start].y {
            z_sum += finite[end].z;
            end += 1;
        }
        unique.push(TerrainVertex {
            position: spade::Point2::new(finite[start].x, finite[start].y),
            z: z_sum / (end - start) as f64,
        });
        start = end;
    }
    Ok(unique)
}

fn terrain_face(
    tin: &spade::DelaunayTriangulation<TerrainVertex>,
    handle: spade::handles::FixedFaceHandle<spade::handles::InnerTag>,
    max_edge_sq: Option<f64>,
    occupancy: Option<&OccupancyGrid>,
) -> Option<[u32; 3]> {
    use spade::Triangulation as _;

    let face_vertices = tin.face(handle).vertices();
    let positions = face_vertices.map(|vertex| vertex.position());
    let edge_sq = [
        point2_distance_sq(positions[1], positions[0]),
        point2_distance_sq(positions[2], positions[1]),
        point2_distance_sq(positions[0], positions[2]),
    ];
    if max_edge_sq.is_some_and(|limit| edge_sq.iter().any(|distance| *distance > limit)) {
        return None;
    }
    // Drop triangles the convex hull draws across concave boundaries or holes,
    // where the survey has no data to support a surface.
    if occupancy.is_some_and(|grid| grid.triangle_spans_void(positions)) {
        return None;
    }

    let twice_area = (positions[1].x - positions[0].x) * (positions[2].y - positions[0].y) - (positions[1].y - positions[0].y) * (positions[2].x - positions[0].x);
    if twice_area.abs() <= 1e-12 {
        return None;
    }

    let mut triangle = face_vertices.map(|vertex| vertex.fix().index() as u32);
    if twice_area < 0.0 {
        triangle.swap(1, 2);
    }
    Some(triangle)
}

fn point2_distance_sq(a: spade::Point2<f64>, b: spade::Point2<f64>) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

/// Finite XYZ bounds and finite-point count of the cloud. `min`/`max` reductions
/// are exact, so the result is deterministic.
fn terrain_bounds(points: &[DVec3], cancel: &crate::app::jobs::CancelFlag) -> Result<(DVec3, DVec3, usize)> {
    let bounds = points
        .par_chunks(262_144)
        .map(|chunk| -> Result<(DVec3, DVec3, usize)> {
            if cancel.is_cancelled() {
                anyhow::bail!("Terrain TIN reconstruction cancelled");
            }
            Ok(chunk
                .iter()
                .filter(|point| point.is_finite())
                .fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY), 0usize), |(min, max, count), point| {
                    (min.min(*point), max.max(*point), count + 1)
                }))
        })
        .collect::<Result<Vec<_>>>()?;
    let (min, max, finite_count) = bounds.into_iter().fold(
        (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY), 0usize),
        |(min, max, count), (chunk_min, chunk_max, chunk_count)| (min.min(chunk_min), max.max(chunk_max), count.saturating_add(chunk_count)),
    );
    if finite_count < 3 {
        anyhow::bail!("The point cloud has fewer than 3 finite points");
    }
    Ok((min, max, finite_count))
}

fn spatial_grid_subsample_terrain(
    points: &[DVec3],
    max_points: usize,
    hole_fill_distance: f64,
    cancel: &crate::app::jobs::CancelFlag,
) -> Result<(Vec<DVec3>, Option<OccupancyGrid>)> {
    if points.len() <= max_points {
        let finite = points.par_iter().copied().filter(|point| point.is_finite()).collect();
        if cancel.is_cancelled() {
            anyhow::bail!("Terrain TIN reconstruction cancelled");
        }
        return Ok((finite, None));
    }

    let (min, max, finite_count) = terrain_bounds(points, cancel)?;
    let extent = max - min;
    let area = (extent.x * extent.y).abs();
    if area <= f64::EPSILON {
        return Ok((jittered_subsample(points, max_points), None));
    }

    let cell_size = choose_terrain_cell_size(points, min, area, max_points, finite_count, cancel)?;
    let cells = bin_terrain_cells(points, min, cell_size, cancel)?;
    let occupancy = OccupancyGrid {
        cells: cells.keys().copied().collect(),
        min,
        cell_size,
        dilation: hole_fill_dilation(hole_fill_distance, cell_size),
    };

    let mut sampled: Vec<DVec3> = cells.into_par_iter().filter_map(|(_, (sum, count))| (count > 0).then(|| sum.mean(count))).collect();
    if sampled.len() > max_points {
        sampled.par_sort_unstable_by(|a, b| spatial_hash(a).cmp(&spatial_hash(b)).then_with(|| a.x.total_cmp(&b.x)).then_with(|| a.y.total_cmp(&b.y)));
        sampled.truncate(max_points);
    }
    Ok((sampled, Some(occupancy)))
}

/// Exact fixed-point accumulator for a cell's summed coordinates. Summing raw
/// f64 in parallel is non-associative, so the resulting mean — and thus the
/// truncation order and Delaunay topology built from it — varied run to run.
/// Quantising each coordinate to fixed-point integers before summing makes the
/// total order-independent and the whole TIN reproducible. `SCALE` is finer than
/// the survey's own quantisation, so no meaningful precision is lost, and i128
/// cannot overflow for any realistic point count.
#[derive(Clone, Copy, Default)]
struct CellSum {
    x: i128,
    y: i128,
    z: i128,
}

/// Fixed-point units per metre (micrometres): well below survey precision.
const CELL_SUM_SCALE: f64 = 1.0e6;

impl CellSum {
    fn add(&mut self, point: &DVec3) {
        self.x += (point.x * CELL_SUM_SCALE).round() as i128;
        self.y += (point.y * CELL_SUM_SCALE).round() as i128;
        self.z += (point.z * CELL_SUM_SCALE).round() as i128;
    }

    fn merge(&mut self, other: &Self) {
        self.x += other.x;
        self.y += other.y;
        self.z += other.z;
    }

    fn mean(&self, count: u64) -> DVec3 {
        let divisor = count as f64 * CELL_SUM_SCALE;
        DVec3::new(self.x as f64 / divisor, self.y as f64 / divisor, self.z as f64 / divisor)
    }
}

type CellMap = HashMap<(i64, i64), (CellSum, u64)>;

fn terrain_cell_key(point: &DVec3, min: DVec3, cell_size: f64) -> (i64, i64) {
    (((point.x - min.x) / cell_size).floor() as i64, ((point.y - min.y) / cell_size).floor() as i64)
}

/// Which grid cells actually contain survey points, used to reject triangles
/// that a convex-hull Delaunay would otherwise bridge across concave
/// boundaries and interior voids. Cells are the sampler's own bins, so at that
/// resolution genuine terrain is densely occupied while gaps read as empty.
struct OccupancyGrid {
    cells: HashSet<(i64, i64)>,
    min: DVec3,
    cell_size: f64,
    /// Neighbourhood radius (cells) treated as covered. One rejects only genuine
    /// multi-cell gaps; larger values bridge (fill) holes up to that width.
    dilation: i64,
}

impl OccupancyGrid {
    /// True if any cell within `dilation` of the sample holds points. Dilating
    /// makes the void test robust to the isolated empty cells that pepper real
    /// coverage (occlusion, absorption, scan gaps) and, at larger radii, fills
    /// holes and boundary concavities narrower than the dilation.
    fn near_points(&self, x: f64, y: f64) -> bool {
        let kx = ((x - self.min.x) / self.cell_size).floor() as i64;
        let ky = ((y - self.min.y) / self.cell_size).floor() as i64;
        (-self.dilation..=self.dilation).any(|dx| (-self.dilation..=self.dilation).any(|dy| self.cells.contains(&(kx + dx, ky + dy))))
    }

    /// True when a triangle spans terrain the survey never covered. The three
    /// edges are walked cell by cell (not just sampled at their midpoints), so a
    /// gap crossed anywhere along an edge is caught — a narrow, curved or
    /// off-centre hole no longer slips between a handful of sample points. The
    /// centroid is checked too, and the dilation keeps legitimately coarse
    /// triangles over real, if sparse, ground.
    fn triangle_spans_void(&self, positions: [spade::Point2<f64>; 3]) -> bool {
        // Cap samples per edge so an enormous (but valid) flat triangle stays
        // cheap; the coarser spacing still lands inside any substantial gap.
        const MAX_EDGE_SAMPLES: usize = 64;

        let centroid = (
            (positions[0].x + positions[1].x + positions[2].x) / 3.0,
            (positions[0].y + positions[1].y + positions[2].y) / 3.0,
        );
        if !self.near_points(centroid.0, centroid.1) {
            return true;
        }
        for &(a, b) in &[(0, 1), (1, 2), (2, 0)] {
            let (start, end) = (positions[a], positions[b]);
            let dx = end.x - start.x;
            let dy = end.y - start.y;
            let length = (dx * dx + dy * dy).sqrt();
            let steps = ((length / self.cell_size).ceil() as usize).clamp(1, MAX_EDGE_SAMPLES);
            // Skip the endpoints — they are grid vertices and always occupied.
            for step in 1..steps {
                let t = step as f64 / steps as f64;
                if !self.near_points(start.x + dx * t, start.y + dy * t) {
                    return true;
                }
            }
        }
        false
    }
}

/// Size the grid so occupied cells approximately fill the vertex budget. The
/// naive `sqrt(area / budget)` assumes the whole bounding rectangle is covered,
/// but survey footprints are usually concave and fill far less, wasting most of
/// the budget. Estimate the occupied footprint area, then size cells so about
/// `target` of them fall inside it.
///
/// Occupancy is probed at a coarse resolution chosen so each cell holds
/// thousands of points — dense enough that the strided sample reliably hits
/// every occupied cell. Probing at the (possibly fine) budget resolution would
/// miss sparse cells, underestimate the footprint, and drive the candidate grid
/// toward the full point count and a memory blow-up at high budgets.
fn choose_terrain_cell_size(points: &[DVec3], min: DVec3, area: f64, target: usize, finite_count: usize, cancel: &crate::app::jobs::CancelFlag) -> Result<f64> {
    const FOOTPRINT_SAMPLE_TARGET: usize = 4_000_000;
    /// Average points per probe cell relative to the sampling stride. At 32× the
    /// stride even a tenth-density cell is hit with near certainty.
    const PROBE_POINTS_PER_STRIDE: f64 = 32.0;

    let stride = (points.len() / FOOTPRINT_SAMPLE_TARGET).max(1);
    // Probe at the budget resolution, but never finer than reliable sampling
    // allows: when the sample is strided, a too-fine probe misses sparse cells
    // and underestimates the footprint. When `stride == 1` every point is seen,
    // so the budget resolution is used directly (identical to sizing straight
    // from the occupied fraction).
    let base = (area / target as f64).sqrt().max(1.0e-9);
    let probe_size = if stride > 1 {
        let dense = (PROBE_POINTS_PER_STRIDE * stride as f64 * area / finite_count.max(1) as f64).sqrt();
        base.max(dense)
    } else {
        base
    };
    let occupied = points
        .par_iter()
        .enumerate()
        .try_fold(HashSet::<(i64, i64)>::new, |mut occupied, (index, point)| -> Result<_> {
            if index % 262_144 == 0 && cancel.is_cancelled() {
                anyhow::bail!("Terrain TIN reconstruction cancelled");
            }
            if index.is_multiple_of(stride) && point.is_finite() {
                occupied.insert(terrain_cell_key(point, min, probe_size));
            }
            Ok(occupied)
        })
        .try_reduce(HashSet::new, |mut left, mut right| -> Result<_> {
            // Union both partials, extending the larger for fewer inserts.
            // Returning either alone would drop the other's cells and make
            // the occupied count depend on the reduce tree shape.
            if left.len() < right.len() {
                right.extend(left);
                Ok(right)
            } else {
                left.extend(right);
                Ok(left)
            }
        })?;
    if occupied.is_empty() {
        return Ok((area / target as f64).sqrt().max(1.0e-9));
    }
    // Coarse cells straddling the boundary slightly overestimate the footprint,
    // which errs toward a larger cell size — fewer candidate cells, never a
    // blow-up.
    let footprint_area = occupied.len() as f64 * probe_size * probe_size;
    Ok((footprint_area / target as f64).sqrt().max(1.0e-9))
}

fn merge_terrain_cells(mut destination: CellMap, source: CellMap) -> Result<CellMap> {
    for (key, (sum, count)) in source {
        let entry = destination.entry(key).or_insert((CellSum::default(), 0));
        entry.0.merge(&sum);
        entry.1 = entry.1.checked_add(count).ok_or_else(|| anyhow::anyhow!("Terrain TIN cell point count overflow"))?;
    }
    Ok(destination)
}

/// Bin every finite point into fixed-point cell accumulators. Shared by the grid
/// and adaptive samplers; the fixed-point sums keep the resulting means
/// reproducible regardless of parallel accumulation order.
fn bin_terrain_cells(points: &[DVec3], min: DVec3, cell_size: f64, cancel: &crate::app::jobs::CancelFlag) -> Result<CellMap> {
    points
        .par_iter()
        .enumerate()
        .try_fold(CellMap::new, |mut cells, (index, point)| -> Result<CellMap> {
            if index % 262_144 == 0 && cancel.is_cancelled() {
                anyhow::bail!("Terrain TIN reconstruction cancelled");
            }
            if point.is_finite() {
                let key = terrain_cell_key(point, min, cell_size);
                let entry = cells.entry(key).or_insert((CellSum::default(), 0));
                entry.0.add(point);
                entry.1 += 1;
            }
            Ok(cells)
        })
        .try_reduce(CellMap::new, |left, right| -> Result<CellMap> {
            if cancel.is_cancelled() {
                anyhow::bail!("Terrain TIN reconstruction cancelled");
            }
            if left.len() < right.len() {
                return merge_terrain_cells(right, left);
            }
            merge_terrain_cells(left, right)
        })
}

/// Additive least-squares plane-fit moments plus the total squared vertical
/// residual to that plane. Moments of disjoint point sets add, so a quadtree
/// node accumulates its children's moments and the residual measures how poorly
/// one plane approximates the node — the driver of adaptive refinement.
#[derive(Clone, Copy, Default)]
struct PlaneMoments {
    n: f64,
    sx: f64,
    sy: f64,
    sz: f64,
    sxx: f64,
    sxy: f64,
    syy: f64,
    sxz: f64,
    syz: f64,
    szz: f64,
}

impl PlaneMoments {
    fn from_point(point: DVec3) -> Self {
        Self {
            n: 1.0,
            sx: point.x,
            sy: point.y,
            sz: point.z,
            sxx: point.x * point.x,
            sxy: point.x * point.y,
            syy: point.y * point.y,
            sxz: point.x * point.z,
            syz: point.y * point.z,
            szz: point.z * point.z,
        }
    }

    fn add(&mut self, other: &Self) {
        self.n += other.n;
        self.sx += other.sx;
        self.sy += other.sy;
        self.sz += other.sz;
        self.sxx += other.sxx;
        self.sxy += other.sxy;
        self.syy += other.syy;
        self.sxz += other.sxz;
        self.syz += other.syz;
        self.szz += other.szz;
    }

    /// Total squared vertical distance of the points to their best-fit plane.
    /// Uses centred (covariance) moments for conditioning, and falls back to the
    /// height variance when the XY spread is degenerate, which still flags
    /// vertical relief without inverting a singular normal matrix.
    fn residual_sq(&self) -> f64 {
        if self.n < 1.5 {
            return 0.0;
        }
        let inv_n = 1.0 / self.n;
        let czz = (self.szz - self.sz * self.sz * inv_n).max(0.0);
        if self.n < 3.0 {
            return czz;
        }
        let cxx = self.sxx - self.sx * self.sx * inv_n;
        let cyy = self.syy - self.sy * self.sy * inv_n;
        let cxy = self.sxy - self.sx * self.sy * inv_n;
        let cxz = self.sxz - self.sx * self.sz * inv_n;
        let cyz = self.syz - self.sy * self.sz * inv_n;
        let det = cxx * cyy - cxy * cxy;
        if det <= 1.0e-6 * cxx * cyy {
            return czz;
        }
        let a = (cyy * cxz - cxy * cyz) / det;
        let b = (cxx * cyz - cxy * cxz) / det;
        (czz - a * cxz - b * cyz).max(0.0)
    }
}

struct QuadNode {
    sum: DVec3,
    fine_count: u32,
    children: [u32; 4],
    residual_sq: f64,
    /// Any descendant fine cell lies on the footprint boundary (adjacent to an
    /// empty cell). Such nodes are refined to full detail so the edge keeps its
    /// shape instead of collapsing into a few large triangles.
    contains_boundary: bool,
}

impl QuadNode {
    /// Representative point of the node: the equal-area mean of its fine cells.
    fn mean(&self) -> DVec3 {
        self.sum / f64::from(self.fine_count)
    }

    fn has_children(&self) -> bool {
        self.children[0] != u32::MAX
    }

    fn child_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.children.iter().copied().filter(|&child| child != u32::MAX).map(|child| child as usize)
    }
}

struct QuadTree {
    nodes: Vec<QuadNode>,
    roots: Vec<u32>,
}

/// A candidate refinement: replacing an active node with its children. `key` is
/// the residual reduction per added vertex; ties break on node index so the
/// heap order — and thus the whole selection — is deterministic.
struct Split {
    key: f64,
    node: usize,
}

impl PartialEq for Split {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for Split {}
impl PartialOrd for Split {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Split {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher gain first; on ties the lower node index is treated as greater
        // so it pops first.
        self.key.total_cmp(&other.key).then_with(|| other.node.cmp(&self.node))
    }
}

impl QuadTree {
    fn split_of(&self, node: usize) -> Option<Split> {
        let entry = &self.nodes[node];
        let occupied_children = entry.child_indices().count();
        if occupied_children < 2 {
            return None;
        }
        let child_residual: f64 = entry.child_indices().map(|child| self.nodes[child].residual_sq).sum();
        let gain = (entry.residual_sq - child_residual).max(0.0);
        let cost = (occupied_children - 1) as f64;
        Some(Split { key: gain / cost, node })
    }
}

/// Interleave the low 32 bits of `value` with zero bits (Morton part).
fn morton_part(value: u32) -> u64 {
    let mut n = u64::from(value);
    n = (n | (n << 16)) & 0x0000_ffff_0000_ffff;
    n = (n | (n << 8)) & 0x00ff_00ff_00ff_00ff;
    n = (n | (n << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
    n = (n | (n << 2)) & 0x3333_3333_3333_3333;
    n = (n | (n << 1)) & 0x5555_5555_5555_5555;
    n
}

/// Z-order code of a non-negative cell key. Sorting by this places the four
/// children of any quadtree node contiguously, so aggregating a level is a
/// linear grouping by `code >> 2`.
fn morton_code(kx: i64, ky: i64) -> u64 {
    morton_part(kx.max(0) as u32) | (morton_part(ky.max(0) as u32) << 1)
}

/// Aggregate Morton-sorted leaf cells bottom-up into a quadtree, summing each
/// node's moments from its children in code order for a reproducible result.
/// Each leaf is `(morton, moments, rebased mean, is_boundary)`.
fn build_quadtree(leaves: &[(u64, PlaneMoments, DVec3, bool)]) -> QuadTree {
    let mut nodes: Vec<QuadNode> = Vec::with_capacity(leaves.len() * 2);
    let mut moments: Vec<PlaneMoments> = Vec::with_capacity(leaves.len() * 2);
    let mut current: Vec<(u64, u32)> = Vec::with_capacity(leaves.len());
    for &(code, cell_moments, sum, boundary) in leaves {
        let index = nodes.len() as u32;
        nodes.push(QuadNode {
            sum,
            fine_count: 1,
            children: [u32::MAX; 4],
            residual_sq: cell_moments.residual_sq(),
            contains_boundary: boundary,
        });
        moments.push(cell_moments);
        current.push((code, index));
    }

    while current.len() > 1 {
        let mut next: Vec<(u64, u32)> = Vec::new();
        let mut start = 0;
        while start < current.len() {
            let parent_code = current[start].0 >> 2;
            let mut child_indices = [u32::MAX; 4];
            let mut occupied = 0;
            let mut node_moments = PlaneMoments::default();
            let mut sum = DVec3::ZERO;
            let mut fine_count = 0u32;
            let mut contains_boundary = false;
            let mut end = start;
            while end < current.len() && current[end].0 >> 2 == parent_code {
                let child = current[end].1;
                child_indices[occupied] = child;
                occupied += 1;
                node_moments.add(&moments[child as usize]);
                sum += nodes[child as usize].sum;
                fine_count += nodes[child as usize].fine_count;
                contains_boundary |= nodes[child as usize].contains_boundary;
                end += 1;
            }
            let index = nodes.len() as u32;
            nodes.push(QuadNode {
                sum,
                fine_count,
                children: child_indices,
                residual_sq: node_moments.residual_sq(),
                contains_boundary,
            });
            moments.push(node_moments);
            next.push((parent_code, index));
            start = end;
        }
        current = next;
    }

    let roots = current.iter().map(|&(_, index)| index).collect();
    QuadTree { nodes, roots }
}

/// Select at most `budget` quadtree nodes as output vertices, each becoming one
/// vertex.
///
/// Phase 1 refines every branch touching the footprint edge down to fine cells
/// so the boundary keeps its shape instead of collapsing into a few large
/// triangles — but only while the budget has room for the extra vertices a
/// split introduces. `count` tracks the eventual active-node total (the roots,
/// plus one extra per split beyond the node it replaces), so descent stops
/// before it can exceed `budget`; a fragmented footprint that makes nearly
/// every cell a boundary cell then refines only as far as the budget allows
/// rather than blowing past it. Phase 2 spends any remaining budget refining the
/// highest residual-per-vertex interior nodes.
fn greedy_cut(tree: &QuadTree, budget: usize) -> Vec<usize> {
    let mut active = vec![false; tree.nodes.len()];
    // The eventual active-node count: descent replaces one prospective vertex
    // with its occupied children, a net gain of `occupied - 1`.
    let mut count = tree.roots.len();
    let mut stack: Vec<usize> = tree.roots.iter().map(|&root| root as usize).collect();
    while let Some(node) = stack.pop() {
        let entry = &tree.nodes[node];
        let occupied = entry.child_indices().count();
        if entry.contains_boundary && entry.has_children() && count + (occupied - 1) <= budget {
            count += occupied - 1;
            stack.extend(entry.child_indices());
        } else {
            active[node] = true;
        }
    }

    // Phase 2 — residual: refine the highest error-per-vertex interior nodes
    // until the budget is spent.
    let mut heap = std::collections::BinaryHeap::new();
    for (node, &is_active) in active.iter().enumerate() {
        if is_active && let Some(split) = tree.split_of(node) {
            heap.push(split);
        }
    }
    while count < budget {
        let Some(split) = heap.pop() else {
            break;
        };
        // Remaining splits only add coplanar detail; stop refining.
        if split.key <= 0.0 {
            break;
        }
        let added = tree.nodes[split.node].child_indices().count() - 1;
        if count + added > budget {
            // Would overshoot; a smaller split deeper in the heap may still fit.
            continue;
        }
        active[split.node] = false;
        for child in tree.nodes[split.node].child_indices().collect::<Vec<_>>() {
            active[child] = true;
            if let Some(child_split) = tree.split_of(child) {
                heap.push(child_split);
            }
        }
        count += added;
    }
    (0..tree.nodes.len()).filter(|&index| active[index]).collect()
}

/// Adaptive terrain sampler: a fine candidate grid aggregated into a quadtree,
/// then greedily refined where a single plane fits the surface worst per added
/// vertex. Complex ground (crests, benches, drains) keeps fine detail while
/// planar areas — even steep batters — collapse to a few vertices. Fully
/// deterministic.
fn adaptive_quadtree_subsample_terrain(
    points: &[DVec3],
    max_points: usize,
    candidate_multiplier: u32,
    hole_fill_distance: f64,
    cancel: &crate::app::jobs::CancelFlag,
) -> Result<(Vec<DVec3>, Option<OccupancyGrid>)> {
    if points.len() <= max_points {
        let finite = points.par_iter().copied().filter(|point| point.is_finite()).collect();
        if cancel.is_cancelled() {
            anyhow::bail!("Terrain TIN reconstruction cancelled");
        }
        return Ok((finite, None));
    }

    let (min, max, finite_count) = terrain_bounds(points, cancel)?;
    let extent = max - min;
    let area = (extent.x * extent.y).abs();
    if area <= f64::EPSILON {
        return Ok((jittered_subsample(points, max_points), None));
    }

    // Candidate grid at `candidate_multiplier` cells per budgeted vertex: more
    // gives the greedy cut freer adaptive allocation, at the cost of binning the
    // cloud into more cells.
    let candidate_target = max_points.saturating_mul(candidate_multiplier.max(1) as usize).max(4);
    let fine_size = choose_terrain_cell_size(points, min, area, candidate_target, finite_count, cancel)?;
    let cells = bin_terrain_cells(points, min, fine_size, cancel)?;
    if cancel.is_cancelled() {
        anyhow::bail!("Terrain TIN reconstruction cancelled");
    }
    let occupancy = OccupancyGrid {
        cells: cells.keys().copied().collect(),
        min,
        cell_size: fine_size,
        dilation: hole_fill_dilation(hole_fill_distance, fine_size),
    };

    // Leaves carry the fine cell's deterministic mean point, rebased to `min`
    // for well-conditioned plane fits, and a boundary flag (adjacent to an empty
    // cell). Sort by Morton code so quadtree children are contiguous.
    let occupied = &occupancy.cells;
    let mut leaves: Vec<(u64, PlaneMoments, DVec3, bool)> = cells
        .par_iter()
        .map(|(&key, &(sum, count))| {
            let rebased = sum.mean(count) - min;
            let boundary = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|&(dx, dy)| !occupied.contains(&(key.0 + dx, key.1 + dy)));
            (morton_code(key.0, key.1), PlaneMoments::from_point(rebased), rebased, boundary)
        })
        .collect();
    drop(cells);
    leaves.par_sort_unstable_by_key(|&(code, ..)| code);

    if leaves.len() <= max_points {
        // The candidate grid already fits the budget; keep every fine cell.
        let vertices = leaves.into_iter().map(|(_, _, point, _)| point + min).collect();
        return Ok((vertices, Some(occupancy)));
    }

    let tree = build_quadtree(&leaves);
    drop(leaves);
    let selected = greedy_cut(&tree, max_points);
    if cancel.is_cancelled() {
        anyhow::bail!("Terrain TIN reconstruction cancelled");
    }
    let vertices = selected.into_iter().map(|node| tree.nodes[node].mean() + min).collect();
    Ok((vertices, Some(occupancy)))
}

fn spatial_hash(point: &DVec3) -> u64 {
    let x = point.x.to_bits();
    let y = point.y.to_bits();
    splitmix64(x ^ y.rotate_left(32))
}

fn jittered_subsample(points: &[DVec3], max_points: usize) -> Vec<DVec3> {
    (0..max_points)
        .into_par_iter()
        .map(|bucket| {
            let start = bucket * points.len() / max_points;
            let end = ((bucket + 1) * points.len() / max_points).min(points.len());
            let width = end.saturating_sub(start).max(1);
            points[start + (splitmix64(bucket as u64) as usize % width)]
        })
        .collect()
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E3779B97F4A7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D049BB133111EB);
    value ^ (value >> 31)
}
