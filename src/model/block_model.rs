use std::{cell::RefCell, collections::HashSet, path::PathBuf, sync::Arc};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::model::formats::{block_model_data::BlockModelData, csv_block_model::CsvColumnMapping};

pub(crate) const MIN_COLOR_STOPS: usize = 2;
pub(crate) const MAX_COLOR_STOPS: usize = 12;
pub(crate) const FIRST_CUSTOM_COLOR_STOP_ID: u64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BlockModelId(pub(crate) u64);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct BlockModelSource {
    #[serde(alias = "bmf_path")]
    pub(crate) path: PathBuf,
    /// Present when the source is a CSV block model. The mapping is persisted
    /// with the session so custom coordinate/size headers can be reopened.
    #[serde(default)]
    pub(crate) csv_columns: Option<CsvColumnMapping>,
    /// Runtime-generated models have no backing file, are marked unsaved, and
    /// are not restored as session sources. Save As converts one into a normal
    /// CSV-backed model.
    #[serde(default)]
    pub(crate) generated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BlockBounds {
    pub(crate) lower: DVec3,
    pub(crate) upper: DVec3,
}

/// Block geometry without requiring one 48-byte [`BlockBounds`] per regular
/// grid cell. Optional voxel tiling changes file/index order, so the
/// implicit representation retains it and maps an index back to xyz on demand.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct RegularBlockBounds {
    lower: DVec3,
    cell: DVec3,
    dims: [usize; 3],
    voxel_size: Option<[usize; 3]>,
    len: usize,
}

#[allow(dead_code)]
impl RegularBlockBounds {
    pub(crate) fn new(lower: DVec3, upper: DVec3, dims: [usize; 3], voxel_size: Option<[usize; 3]>, expected_len: usize) -> Result<Self, String> {
        let len = dims
            .into_iter()
            .try_fold(1usize, |product, dim| product.checked_mul(dim))
            .ok_or_else(|| "regular block-grid dimensions overflow usize".to_owned())?;
        if dims.contains(&0) || len != expected_len {
            return Err(format!("regular block-grid dimensions imply {len} blocks, metadata reports {expected_len}"));
        }
        if voxel_size.is_some_and(|sizes| sizes.contains(&0)) {
            return Err("regular block-grid voxel dimensions must be non-zero".to_owned());
        }
        let cell = (upper - lower) / DVec3::from_array(dims.map(|dim| dim as f64));
        if !lower.is_finite() || !upper.is_finite() || !cell.is_finite() || cell.min_element() <= 0.0 {
            return Err("regular block-grid bounds are invalid".to_owned());
        }
        Ok(Self {
            lower,
            cell,
            dims,
            voxel_size,
            len,
        })
    }

    fn coords(&self, index: usize) -> Option<[usize; 3]> {
        if index >= self.len {
            return None;
        }
        let [dim_x, dim_y, dim_z] = self.dims;
        let Some([size_x, size_y, size_z]) = self.voxel_size else {
            let x = index % dim_x;
            let yz = index / dim_x;
            return Some([x, yz % dim_y, yz / dim_y]);
        };

        // Whole voxel-z slabs contain dim_x*dim_y*depth blocks; within one,
        // whole voxel-y strips contain dim_x*height*depth. Only the final
        // slab/strip can be clipped, so division by the full size locates the
        // correct group and the remaining index decodes the local xyz order.
        let z_slab_blocks = dim_x.checked_mul(dim_y)?.checked_mul(size_z)?;
        let voxel_z = index / z_slab_blocks;
        let mut remaining = index % z_slab_blocks;
        let z_start = voxel_z * size_z;
        let depth = size_z.min(dim_z - z_start);

        let y_strip_blocks = dim_x.checked_mul(size_y)?.checked_mul(depth)?;
        let voxel_y = remaining / y_strip_blocks;
        remaining %= y_strip_blocks;
        let y_start = voxel_y * size_y;
        let height = size_y.min(dim_y - y_start);

        let x_voxel_blocks = size_x.checked_mul(height)?.checked_mul(depth)?;
        let voxel_x = remaining / x_voxel_blocks;
        remaining %= x_voxel_blocks;
        let x_start = voxel_x * size_x;
        let width = size_x.min(dim_x - x_start);

        let local_x = remaining % width;
        let local_yz = remaining / width;
        let local_y = local_yz % height;
        let local_z = local_yz / height;
        Some([x_start + local_x, y_start + local_y, z_start + local_z])
    }

    fn get(&self, index: usize) -> Option<BlockBounds> {
        let [x, y, z] = self.coords(index)?;
        let lower = self.lower + self.cell * DVec3::new(x as f64, y as f64, z as f64);
        Some(BlockBounds { lower, upper: lower + self.cell })
    }

    fn local_bounds(&self) -> BlockBounds {
        BlockBounds {
            lower: self.lower,
            upper: self.lower + self.cell * DVec3::from_array(self.dims.map(|dim| dim as f64)),
        }
    }

    fn uniform_grid(&self) -> Option<UniformBlockGrid> {
        let cells = self.dims[0].checked_mul(self.dims[1])?.checked_mul(self.dims[2])?;
        (cells <= MAX_UNIFORM_GRID_CELLS).then_some(UniformBlockGrid {
            origin: self.lower,
            inv_cell: DVec3::ONE / self.cell,
            dims: self.dims,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) enum BlockBoundsSource {
    Explicit(Vec<BlockBounds>),
    #[allow(dead_code)]
    Regular(RegularBlockBounds),
}

impl BlockBoundsSource {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Explicit(blocks) => blocks.len(),
            Self::Regular(grid) => grid.len,
        }
    }

    pub(crate) fn get(&self, index: usize) -> Option<BlockBounds> {
        match self {
            Self::Explicit(blocks) => blocks.get(index).copied(),
            Self::Regular(grid) => grid.get(index),
        }
    }

    pub(crate) fn implicit_local_bounds(&self) -> Option<BlockBounds> {
        match self {
            Self::Regular(grid) => Some(grid.local_bounds()),
            Self::Explicit(_) => None,
        }
    }

    pub(crate) fn uniform_grid(&self) -> Option<UniformBlockGrid> {
        match self {
            Self::Regular(grid) => grid.uniform_grid(),
            Self::Explicit(blocks) => detect_uniform_grid(blocks),
        }
    }
}

/// Most BMFs render every block. Keep that common case as a length rather than
/// retaining an additional 8-byte `usize` for every block.
#[derive(Clone, Debug)]
pub(crate) enum RenderableBlockIndices {
    All(usize),
    #[allow(dead_code)]
    Explicit(Vec<usize>),
}

impl RenderableBlockIndices {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::All(len) => *len,
            Self::Explicit(indices) => indices.len(),
        }
    }

    pub(crate) fn get(&self, position: usize) -> Option<usize> {
        match self {
            Self::All(len) => (position < *len).then_some(position),
            Self::Explicit(indices) => indices.get(position).copied(),
        }
    }

    pub(crate) fn iter(&self) -> RenderableBlockIndicesIter<'_> {
        match self {
            Self::All(len) => RenderableBlockIndicesIter::All(0..*len),
            Self::Explicit(indices) => RenderableBlockIndicesIter::Explicit(indices.iter()),
        }
    }

    pub(crate) fn is_all(&self) -> bool {
        matches!(self, Self::All(_))
    }

    pub(crate) fn par_map<T: Send>(&self, map: impl Fn(usize) -> T + Sync + Send) -> Vec<T> {
        use rayon::prelude::*;
        match self {
            Self::All(len) => (0..*len).into_par_iter().map(&map).collect(),
            Self::Explicit(indices) => indices.par_iter().copied().map(map).collect(),
        }
    }

    pub(crate) fn par_for_each(&self, visit: impl Fn(usize) + Sync + Send) {
        use rayon::prelude::*;
        match self {
            Self::All(len) => (0..*len).into_par_iter().for_each(&visit),
            Self::Explicit(indices) => indices.par_iter().copied().for_each(visit),
        }
    }

    pub(crate) fn par_filter_map_reduce<T: Send>(
        &self,
        identity: impl Fn() -> T + Sync + Send,
        map: impl Fn(usize) -> Option<T> + Sync + Send,
        reduce: impl Fn(T, T) -> T + Sync + Send,
    ) -> T {
        use rayon::prelude::*;
        match self {
            Self::All(len) => (0..*len).into_par_iter().filter_map(&map).reduce(&identity, &reduce),
            Self::Explicit(indices) => indices.par_iter().copied().filter_map(map).reduce(identity, reduce),
        }
    }
}

pub(crate) enum RenderableBlockIndicesIter<'a> {
    All(std::ops::Range<usize>),
    Explicit(std::slice::Iter<'a, usize>),
}

impl Iterator for RenderableBlockIndicesIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::All(range) => range.next(),
            Self::Explicit(indices) => indices.next().copied(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::All(range) => range.size_hint(),
            Self::Explicit(indices) => indices.size_hint(),
        }
    }
}

impl ExactSizeIterator for RenderableBlockIndicesIter<'_> {}

#[derive(Clone, Debug)]
pub(crate) struct LoadedBlockModel {
    pub(crate) name: String,
    pub(crate) source: BlockModelSource,
    pub(crate) model: BlockModelData,
    pub(crate) blocks: Arc<BlockBoundsSource>,
    pub(crate) renderable_block_indices: Arc<RenderableBlockIndices>,
    pub(crate) uniform_grid: Option<UniformBlockGrid>,
    /// Number of instances the opaque cube path emits after removing blocks
    /// enclosed by six renderable neighbours. Computed once on the loader
    /// worker so renderer selection is O(1) per frame.
    pub(crate) opaque_surface_blocks: Option<usize>,
    pub(crate) world_bounds: Option<(DVec3, DVec3)>,
    pub(crate) active_color_variable: Option<String>,
    pub(crate) active_values_cache: ActiveValuesCache,
}

/// A uniform axis-aligned grid (in local model coordinates) that every block
/// of a model was verified to lie on: all blocks are `cell`-sized and sit at
/// integer multiples of `cell` from `origin`. Detected once at load by
/// [`detect_uniform_grid`]; sub-blocked or irregular models get `None`.
/// Shared-face culling uses this for O(1) bitset neighbour tests instead of
/// hashing quantized block bounds.
#[derive(Clone, Debug)]
pub(crate) struct UniformBlockGrid {
    pub(crate) origin: DVec3,
    /// `1 / cell` (the cell size is not stored — nothing reads it), so the
    /// per-block coordinate lookup in the culling hot loop multiplies
    /// instead of dividing.
    inv_cell: DVec3,
    pub(crate) dims: [usize; 3],
}

impl UniformBlockGrid {
    pub(crate) fn cell_count(&self) -> usize {
        // Checked against overflow in `detect_uniform_grid`.
        self.dims[0] * self.dims[1] * self.dims[2]
    }

    /// Grid coordinates of the cell whose lower corner is `lower`, or `None`
    /// if it lies outside the grid. Callers pass lower corners of blocks that
    /// passed detection (or their axis neighbours), so no size re-check.
    pub(crate) fn cell_coords(&self, lower: DVec3) -> Option<[usize; 3]> {
        let t = ((lower - self.origin) * self.inv_cell).to_array();
        let mut coords = [0usize; 3];
        for axis in 0..3 {
            // `+ 0.5` then truncation rounds to nearest for the in-range
            // values that reach here, without `f64::round`'s libm call (the
            // x86-64 baseline has no round instruction, and this is the
            // culling hot loop). The explicit negative check keeps t <= -0.5
            // (e.g. the out-of-grid neighbour at exactly -1) from truncating
            // up to cell 0.
            let shifted = t[axis] + 0.5;
            if shifted < 0.0 {
                return None;
            }
            let index = shifted as usize;
            if index >= self.dims[axis] {
                return None;
            }
            coords[axis] = index;
        }
        Some(coords)
    }

    pub(crate) fn linear_index(&self, coords: [usize; 3]) -> usize {
        (coords[2] * self.dims[1] + coords[1]) * self.dims[0] + coords[0]
    }
}

/// Count the cube instances left after the opaque path's whole-interior-block
/// cull. Dense implicit grids are O(1); sparse grids build a temporary bitset
/// and walk only their renderable blocks. `None` means the supplied geometry
/// does not agree with the verified grid.
pub(crate) fn opaque_surface_block_count(blocks: &BlockBoundsSource, renderable: &RenderableBlockIndices, grid: &UniformBlockGrid) -> Option<usize> {
    let grid_cells = grid.cell_count();
    if renderable.is_all() && renderable.len() == grid_cells {
        let interior = grid
            .dims
            .map(|dim| dim.saturating_sub(2))
            .into_iter()
            .try_fold(1usize, |product, dim| product.checked_mul(dim))?;
        return Some(grid_cells - interior);
    }

    let mut occupied = vec![0u64; grid_cells.div_ceil(64)];
    for block_index in renderable.iter() {
        let block = blocks.get(block_index)?;
        let index = grid.linear_index(grid.cell_coords(block.lower)?);
        occupied[index / 64] |= 1 << (index % 64);
    }
    let strides = [1, grid.dims[0], grid.dims[0].checked_mul(grid.dims[1])?];
    let mut surface_blocks = 0usize;
    for block_index in renderable.iter() {
        let block = blocks.get(block_index)?;
        let coords = grid.cell_coords(block.lower)?;
        let index = grid.linear_index(coords);
        let fully_occluded = (0..3).all(|axis| {
            if coords[axis] == 0 || coords[axis] + 1 >= grid.dims[axis] {
                return false;
            }
            [index - strides[axis], index + strides[axis]]
                .into_iter()
                .all(|neighbour| occupied[neighbour / 64] & (1 << (neighbour % 64)) != 0)
        });
        surface_blocks += usize::from(!fully_occluded);
    }
    Some(surface_blocks)
}

/// Hash-based counterpart to [`opaque_surface_block_count`] for mixed-size
/// and irregular models. This deliberately mirrors the cube builder's
/// same-size six-neighbour test, so the cached number predicts the instances
/// the opaque cube path will actually upload.
pub(crate) fn opaque_irregular_surface_block_count(blocks: &BlockBoundsSource, renderable: &RenderableBlockIndices) -> Option<usize> {
    const MAX_COUNTED_BLOCKS: usize = 1_000_000;
    if renderable.len() > MAX_COUNTED_BLOCKS {
        return None;
    }
    let mut occupied = HashSet::new();
    occupied.try_reserve(renderable.len()).ok()?;
    for index in renderable.iter() {
        occupied.insert(block_bounds_key(blocks.get(index)?));
    }
    let mut surface_blocks = 0usize;
    for index in renderable.iter() {
        let block = blocks.get(index)?;
        let size = block.upper - block.lower;
        let neighbours = [
            DVec3::new(-size.x, 0.0, 0.0),
            DVec3::new(size.x, 0.0, 0.0),
            DVec3::new(0.0, -size.y, 0.0),
            DVec3::new(0.0, size.y, 0.0),
            DVec3::new(0.0, 0.0, -size.z),
            DVec3::new(0.0, 0.0, size.z),
        ];
        let fully_occluded = neighbours.iter().all(|&delta| {
            occupied.contains(&block_bounds_key(BlockBounds {
                lower: block.lower + delta,
                upper: block.upper + delta,
            }))
        });
        surface_blocks += usize::from(!fully_occluded);
    }
    Some(surface_blocks)
}

fn block_bounds_key(block: BlockBounds) -> [u64; 6] {
    fn quantize(value: f64) -> u64 {
        (value * 1_000_000.0).round().to_bits()
    }
    [
        quantize(block.lower.x),
        quantize(block.lower.y),
        quantize(block.lower.z),
        quantize(block.upper.x),
        quantize(block.upper.y),
        quantize(block.upper.z),
    ]
}

/// Positional tolerance for grid detection, as a fraction of the cell size.
/// Tighter than any real jitter in regular models, loose enough for the f64
/// division/rounding noise in bounds decoded from file columns.
const UNIFORM_GRID_TOL: f64 = 1e-4;
/// Upper bound on grid cells so the culling occupancy bitset stays modest
/// (1 << 29 bits = 64 MiB). Larger (very sparse) grids fall back to hashing.
const MAX_UNIFORM_GRID_CELLS: usize = 1 << 29;

/// Verify that `blocks` all lie on one uniform grid and describe it, or
/// `None` when they don't (sub-blocked models, mixed cell sizes) or when the
/// grid would be too large to be useful. One O(n) pass at load time.
pub(crate) fn detect_uniform_grid(blocks: &[BlockBounds]) -> Option<UniformBlockGrid> {
    let first = blocks.first()?;
    let cell = first.upper - first.lower;
    if !cell.is_finite() || cell.min_element() <= 0.0 {
        return None;
    }
    let mut origin = first.lower;
    for block in blocks {
        origin = origin.min(block.lower);
    }
    if !origin.is_finite() {
        return None;
    }
    let cell_array = cell.to_array();
    let mut dims = [0usize; 3];
    for block in blocks {
        let size = (block.upper - block.lower).to_array();
        let t = ((block.lower - origin) / cell).to_array();
        for axis in 0..3 {
            if ((size[axis] - cell_array[axis]) / cell_array[axis]).abs() > UNIFORM_GRID_TOL {
                return None;
            }
            let index = t[axis].round();
            if (t[axis] - index).abs() > UNIFORM_GRID_TOL || index < 0.0 || index >= MAX_UNIFORM_GRID_CELLS as f64 {
                return None;
            }
            dims[axis] = dims[axis].max(index as usize + 1);
        }
    }
    let cells = dims[0].checked_mul(dims[1])?.checked_mul(dims[2])?;
    (cells <= MAX_UNIFORM_GRID_CELLS).then_some(UniformBlockGrid {
        origin,
        inv_cell: DVec3::ONE / cell,
        dims,
    })
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ColorStop {
    /// Stable runtime identity used by egui widgets. Rendering only reads
    /// `t` and `color`.
    pub(crate) id: u64,
    /// Position within the active variable's render range, `0..1`.
    pub(crate) t: f32,
    /// Straight (non-premultiplied) RGBA, `0..1` per channel.
    pub(crate) color: [f32; 4],
}

impl PartialEq for ColorStop {
    fn eq(&self, other: &Self) -> bool {
        self.t == other.t && self.color == other.color
    }
}

/// A colour ramp driving grade colouring. Stops are always kept sorted
/// ascending by `t`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColorTransferFunction {
    pub(crate) stops: Vec<ColorStop>,
}

impl Default for ColorTransferFunction {
    fn default() -> Self {
        Self {
            // Stops use hard cutoffs: each stop's colour is active from its `t`
            // up to the next stop. Placing them at 0, 1/3, 2/3 gives green,
            // yellow and red each an equal third of the range (red owns the
            // last third rather than only the single point at t=1.0).
            stops: vec![
                ColorStop {
                    id: 1,
                    t: 0.0,
                    color: [0.0, 0.86, 0.22, 1.0],
                },
                ColorStop {
                    id: 2,
                    t: 1.0 / 3.0,
                    color: [1.0, 0.85, 0.0, 1.0],
                },
                ColorStop {
                    id: 3,
                    t: 2.0 / 3.0,
                    color: [0.92, 0.0, 0.0, 1.0],
                },
            ],
        }
    }
}

/// An axis-aligned crop of a block model, expressed in its local grid
/// coordinates (the frame of `metadata.lower`/`upper` and every block's
/// bounds). Blocks outside the box are removed, boundary blocks are clipped
/// to it, and the volume raycaster shortens its march to the cropped extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BlockModelSlice {
    pub(crate) min: DVec3,
    pub(crate) max: DVec3,
}

impl BlockModelSlice {
    pub(crate) fn clamped_to(&self, lower: DVec3, upper: DVec3) -> Self {
        Self {
            min: self.min.clamp(lower, upper),
            max: self.max.clamp(lower, upper),
        }
    }

    /// Whether the slice covers `lower..upper` entirely (i.e. crops nothing).
    pub(crate) fn covers(&self, lower: DVec3, upper: DVec3) -> bool {
        self.min.cmple(lower).all() && self.max.cmpge(upper).all()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OpenBlockModel {
    pub(crate) id: BlockModelId,
    pub(crate) name: String,
    pub(crate) source: BlockModelSource,
    pub(crate) model: BlockModelData,
    pub(crate) blocks: Arc<BlockBoundsSource>,
    pub(crate) renderable_block_indices: Arc<RenderableBlockIndices>,
    /// See [`UniformBlockGrid`]; detected once on the loader worker.
    pub(crate) uniform_grid: Option<UniformBlockGrid>,
    pub(crate) opaque_surface_blocks: Option<usize>,
    pub(crate) visible: bool,
    pub(crate) color: [f32; 4],
    /// Active crop in local grid coordinates, or `None` for the full model.
    pub(crate) slice: Option<BlockModelSlice>,
    pub(crate) active_color_variable: Option<String>,
    pub(crate) color_transfer: ColorTransferFunction,
    pub(crate) hide_empty_color_values: bool,
    /// Lazily decoded values for [`Self::active_color_variable`], shared
    /// by the renderer's grade colouring and the UI's colour-scale legend so
    /// switching colour state doesn't re-decode the whole variable in each.
    /// A failed decode is cached too so a broken variable isn't re-decoded
    /// every frame.
    pub(crate) active_values_cache: ActiveValuesCache,
    /// World-space AABB of all renderable blocks, computed once at load.
    /// Blocks never move afterwards, so the per-frame transparency sort and
    /// scene-bounds queries read this instead of re-walking every block's
    /// eight rotated corners.
    pub(crate) world_bounds: Option<(DVec3, DVec3)>,
}

pub(crate) type ActiveValuesCache = RefCell<Option<ActiveValuesCacheEntry>>;

/// Cached decode of one numeric or named colour variable, keyed on
/// [`ActiveValuesCacheEntry::variable`].
/// Holds the decoded values (a failed decode is cached as `None`) plus the
/// render range derived from them, so both are computed at most once per
/// variable switch rather than re-scanned every frame.
#[derive(Clone, Debug)]
pub(crate) struct ActiveValuesCacheEntry {
    variable: String,
    values: Option<Arc<Vec<f64>>>,
    range: Option<(f64, f64)>,
    /// Integer category codes that actually occur anywhere in the decoded
    /// column. Kept beside the values so the legend never rescans a large
    /// model on every UI frame.
    present_category_codes: Option<Arc<HashSet<u32>>>,
}

pub(crate) fn active_values_cache_has_range(cache: &ActiveValuesCache) -> bool {
    cache.borrow().as_ref().is_some_and(|entry| entry.range.is_some())
}

impl OpenBlockModel {
    /// Decode the initially selected variable while the model is still on the
    /// loader worker. This prevents the first render from materialising and
    /// scanning a whole value column on the UI/render thread.
    pub(crate) fn prepare_active_values_cache(model: &BlockModelData, renderable_block_indices: &RenderableBlockIndices, variable: Option<&str>) -> ActiveValuesCache {
        let Some(name) = variable else {
            return RefCell::new(None);
        };
        let values = model.shared_numeric_values(name).or_else(|| model.color_values(name).ok().map(Arc::new));
        let range = model.variable(name).and_then(|variable| {
            categorical_variable_range(variable).or_else(|| {
                values
                    .as_ref()
                    .and_then(|values| render_value_range(values, renderable_block_indices, numeric_variable_default(variable)))
            })
        });
        let present_category_codes = model
            .variable(name)
            .filter(|variable| categorical_variable(variable))
            .and_then(|_| values.as_deref().map(|values| collect_present_category_codes(values.as_slice(), renderable_block_indices)));
        RefCell::new(Some(ActiveValuesCacheEntry {
            variable: name.to_owned(),
            values,
            range,
            present_category_codes,
        }))
    }

    pub(crate) fn begin_active_values_decode(&self, variable: &str) {
        *self.active_values_cache.borrow_mut() = Some(ActiveValuesCacheEntry {
            variable: variable.to_owned(),
            values: None,
            range: None,
            present_category_codes: None,
        });
    }

    pub(crate) fn install_active_values_cache(&self, prepared: ActiveValuesCache) {
        *self.active_values_cache.borrow_mut() = prepared.into_inner();
    }

    pub(crate) fn active_values_available_for_render(&self) -> bool {
        let Some(variable) = self.active_color_variable.as_deref() else {
            return true;
        };
        self.active_values_cache
            .borrow()
            .as_ref()
            .is_some_and(|entry| entry.variable == variable && entry.values.is_some())
    }

    pub(crate) fn entity_id(&self) -> crate::model::SceneEntityId {
        crate::model::SceneEntityId::BlockModel(self.id)
    }

    /// Populates [`Self::active_values_cache`] for the active variable if it
    /// isn't already current: decodes the values and derives the render range
    /// once, so repeat calls in the same frame (and across frames until the
    /// variable changes) are cache reads.
    fn ensure_active_values_cached(&self, name: &str) {
        let mut cache = self.active_values_cache.borrow_mut();
        if cache.as_ref().is_some_and(|entry| entry.variable == name) {
            return;
        }
        let values = self.model.shared_numeric_values(name).or_else(|| self.model.color_values(name).ok().map(Arc::new));
        let range = self.model.variable(name).and_then(|variable| {
            categorical_variable_range(variable).or_else(|| {
                values
                    .as_ref()
                    .and_then(|values| render_value_range(values, &self.renderable_block_indices, numeric_variable_default(variable)))
            })
        });
        let present_category_codes = self.model.variable(name).filter(|variable| categorical_variable(variable)).and_then(|_| {
            values
                .as_deref()
                .map(|values| collect_present_category_codes(values.as_slice(), &self.renderable_block_indices))
        });
        *cache = Some(ActiveValuesCacheEntry {
            variable: name.to_owned(),
            values,
            range,
            present_category_codes,
        });
    }

    /// Decoded values for the active colour variable, decoding at most once
    /// per variable switch. `None` when there is no active variable or it
    /// can't be decoded.
    pub(crate) fn active_color_values(&self) -> Option<Arc<Vec<f64>>> {
        let name = self.active_color_variable.as_deref()?;
        self.ensure_active_values_cached(name);
        self.active_values_cache.borrow().as_ref()?.values.clone()
    }

    /// Render range `(min, max)` for the active colour variable, computed once
    /// per variable switch and shared by the renderer's grade colouring and
    /// the UI's colour-scale legend. `None` when there is no active variable
    /// or no usable range.
    pub(crate) fn active_value_range(&self) -> Option<(f64, f64)> {
        let name = self.active_color_variable.as_deref()?;
        self.ensure_active_values_cached(name);
        self.active_values_cache.borrow().as_ref()?.range
    }

    pub(crate) fn active_variable_is_categorical(&self) -> bool {
        self.active_color_variable
            .as_deref()
            .and_then(|name| self.model.variable(name))
            .is_some_and(|variable| matches!(variable.physical_type.as_str(), "namedbyte" | "namedshort"))
    }

    /// `Some(true/false)` when the active categorical column has been decoded;
    /// `None` while unavailable or for a numeric variable. The UI treats
    /// unknown as present so an entry cannot disappear merely while loading.
    pub(crate) fn active_category_code_present(&self, code: u32) -> Option<bool> {
        let name = self.active_color_variable.as_deref()?;
        self.ensure_active_values_cached(name);
        self.active_values_cache
            .borrow()
            .as_ref()?
            .present_category_codes
            .as_ref()
            .map(|codes| codes.contains(&code))
    }

    pub(crate) fn reset_color_transfer_for_active_variable(&mut self) {
        let categorical = self
            .active_color_variable
            .as_deref()
            .and_then(|name| self.model.variable(name))
            .filter(|variable| matches!(variable.physical_type.as_str(), "namedbyte" | "namedshort"));
        self.color_transfer = categorical
            .map(|variable| categorical_color_transfer(variable, self.active_value_range()))
            .unwrap_or_default();
    }

    /// World-space AABB of all renderable blocks, cached at load. Reads the
    /// `world_bounds` field; kept as a method so callers are unchanged.
    pub(crate) fn world_bounds(&self) -> Option<(DVec3, DVec3)> {
        self.world_bounds
    }

    /// Conservative world-space bounds of the portion that can currently be
    /// drawn. Using these for frustum/depth fitting lets a tight slice shed
    /// work outside the cropped region as well as inside the shaders.
    pub(crate) fn visible_world_bounds(&self) -> Option<(DVec3, DVec3)> {
        let full = self.world_bounds()?;
        let Some(slice) = self.active_slice() else {
            return Some(full);
        };
        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = DVec3::splat(f64::NEG_INFINITY);
        for x in [slice.min.x, slice.max.x] {
            for y in [slice.min.y, slice.max.y] {
                for z in [slice.min.z, slice.max.z] {
                    let point = self.model.local_to_world(DVec3::new(x, y, z));
                    min = min.min(point);
                    max = max.max(point);
                }
            }
        }
        min = min.max(full.0);
        max = max.min(full.1);
        min.cmple(max).all().then_some((min, max))
    }

    /// The model's extent in its local grid frame — the frame block bounds,
    /// schemas, and [`Self::slice`] are expressed in.
    pub(crate) fn local_bounds(&self) -> (DVec3, DVec3) {
        (self.model.metadata.lower, self.model.metadata.upper)
    }

    /// The slice clamped to the model's local bounds, or `None` when there is
    /// no slice or it crops nothing — so renderers can compare/act on "does
    /// this actually cut anything" directly.
    pub(crate) fn active_slice(&self) -> Option<BlockModelSlice> {
        let (lower, upper) = self.local_bounds();
        let slice = self.slice?.clamped_to(lower, upper);
        (!slice.covers(lower, upper)).then_some(slice)
    }
}

/// World-space AABB of every renderable block, walking each block's eight
/// rotated corners. O(N); call once at load and cache the result.
pub(crate) fn compute_world_bounds(model: &BlockModelData, blocks: &BlockBoundsSource, renderable_block_indices: &RenderableBlockIndices) -> Option<(DVec3, DVec3)> {
    if renderable_block_indices.is_all()
        && let Some(local) = blocks.implicit_local_bounds()
    {
        let bounds = block_world_bounds(model, local);
        return Some((bounds.lower, bounds.upper));
    }
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    let mut any = false;
    for index in renderable_block_indices.iter() {
        let Some(block) = blocks.get(index) else {
            continue;
        };
        let bounds = block_world_bounds(model, block);
        min = min.min(bounds.lower);
        max = max.max(bounds.upper);
        any = true;
    }
    any.then_some((min, max))
}

fn block_world_bounds(model: &BlockModelData, block: BlockBounds) -> BlockBounds {
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for corner in block_corners(block) {
        let world = model.local_to_world(corner);
        min = min.min(world);
        max = max.max(world);
    }
    BlockBounds { lower: min, upper: max }
}

/// A numeric variable's fixed "no value" sentinel, parsed from its source
/// `global`/`default` field. Shared by the renderer's grade colouring and
/// the UI's colour-scale legend so both treat the same blocks as unset.
pub(crate) fn numeric_variable_default(variable: &crate::model::formats::block_model_data::BlockVariable) -> Option<f64> {
    variable.global.trim().parse::<f64>().or_else(|_| variable.default.trim().parse::<f64>()).ok()
}

/// A colour variable's unset value. Numeric columns use their numeric
/// global/default sentinel; named columns map their default label back to its
/// stored integer code so "none" can be hidden just like `-99`.
pub(crate) fn color_variable_default(variable: &crate::model::formats::block_model_data::BlockVariable) -> Option<f64> {
    if matches!(variable.physical_type.as_str(), "namedbyte" | "namedshort") {
        let default = if variable.global.trim().is_empty() {
            variable.default.trim()
        } else {
            variable.global.trim()
        };
        if let Ok(code) = default.parse::<u32>() {
            return Some(code as f64);
        }
        return variable
            .strings
            .iter()
            .find_map(|(&code, label)| label.eq_ignore_ascii_case(default).then_some(code as f64));
    }
    numeric_variable_default(variable)
}

fn categorical_variable_range(variable: &crate::model::formats::block_model_data::BlockVariable) -> Option<(f64, f64)> {
    if !categorical_variable(variable) {
        return None;
    }
    let min = *variable.strings.first_key_value()?.0 as f64;
    let max = *variable.strings.last_key_value()?.0 as f64;
    Some((min, max))
}

fn categorical_variable(variable: &crate::model::formats::block_model_data::BlockVariable) -> bool {
    matches!(variable.physical_type.as_str(), "namedbyte" | "namedshort")
}

fn collect_present_category_codes(values: &[f64], renderable: &RenderableBlockIndices) -> Arc<HashSet<u32>> {
    Arc::new(
        renderable
            .iter()
            .filter_map(|index| values.get(index).copied())
            .filter(|value| value.is_finite() && value.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(value))
            .map(|value| value as u32)
            .collect(),
    )
}

fn categorical_color_transfer(variable: &crate::model::formats::block_model_data::BlockVariable, range: Option<(f64, f64)>) -> ColorTransferFunction {
    const PALETTE: [[f32; 4]; MAX_COLOR_STOPS] = [
        [0.12, 0.56, 1.00, 1.0],
        [1.00, 0.55, 0.10, 1.0],
        [0.18, 0.80, 0.36, 1.0],
        [0.93, 0.20, 0.25, 1.0],
        [0.62, 0.36, 0.90, 1.0],
        [0.55, 0.34, 0.18, 1.0],
        [0.95, 0.42, 0.72, 1.0],
        [0.10, 0.76, 0.78, 1.0],
        [0.60, 0.70, 0.12, 1.0],
        [0.98, 0.78, 0.12, 1.0],
        [0.10, 0.56, 0.52, 1.0],
        [0.58, 0.62, 0.68, 1.0],
    ];
    let mut codes = variable.strings.keys().map(|&code| code as f64).take(MAX_COLOR_STOPS).collect::<Vec<_>>();
    if codes.is_empty() {
        return ColorTransferFunction::default();
    }
    codes.sort_by(f64::total_cmp);
    let (min, max) = range.unwrap_or_else(|| (*codes.first().unwrap(), *codes.last().unwrap()));
    let span = max - min;
    let mut stops = Vec::with_capacity(codes.len().max(MIN_COLOR_STOPS));
    for (index, code) in codes.iter().enumerate() {
        let t = if index == 0 || span.abs() <= f64::EPSILON {
            0.0
        } else {
            let boundary = (codes[index - 1] + code) * 0.5;
            ((boundary - min) / span).clamp(0.0, 1.0) as f32
        };
        stops.push(ColorStop {
            id: index as u64 + 1,
            t,
            color: PALETTE[index],
        });
    }
    if stops.len() == 1 {
        stops.push(ColorStop {
            id: 2,
            t: 1.0,
            color: stops[0].color,
        });
    }
    ColorTransferFunction { stops }
}

/// Conventional "no grade" sentinels. Matched exactly (to float
/// noise) rather than by a `<= -90` threshold so legitimately negative data
/// (sub-sea RLs, elevations) isn't silently treated as missing.
pub(crate) fn is_no_data_sentinel(value: f64) -> bool {
    (value - -99.0).abs() < 1e-8 || (value - -999.0).abs() < 1e-8
}

/// The (min, max) of `values` at `indices`, skipping non-finite values, the
/// variable's default/"unset" value, and common -99/-999 sentinel
/// "no grade" values, so real ore values don't collapse into one colour.
/// `None` when no value in range is usable. A constant column deliberately
/// returns a degenerate `(value, value)` range; callers map that case to the
/// first ramp stop instead of treating valid data as if no variable existed.
/// Shared by the renderer's grade colouring and the UI's colour-scale legend
/// so both agree on the range.
pub(crate) fn render_value_range(values: &[f64], indices: &RenderableBlockIndices, default: Option<f64>) -> Option<(f64, f64)> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for index in indices.iter() {
        let Some(&value) = values.get(index) else {
            continue;
        };
        if !value.is_finite() || default.is_some_and(|default| (value - default).abs() < 1e-8) {
            continue;
        }
        if is_no_data_sentinel(value) {
            continue;
        }
        min = min.min(value);
        max = max.max(value);
    }
    (min.is_finite() && max.is_finite()).then_some((min, max))
}

fn block_corners(block: BlockBounds) -> [DVec3; 8] {
    let lo = block.lower;
    let hi = block.upper;
    [
        DVec3::new(lo.x, lo.y, lo.z),
        DVec3::new(hi.x, lo.y, lo.z),
        DVec3::new(hi.x, hi.y, lo.z),
        DVec3::new(lo.x, hi.y, lo.z),
        DVec3::new(lo.x, lo.y, hi.z),
        DVec3::new(hi.x, lo.y, hi.z),
        DVec3::new(hi.x, hi.y, hi.z),
        DVec3::new(lo.x, hi.y, hi.z),
    ]
}
