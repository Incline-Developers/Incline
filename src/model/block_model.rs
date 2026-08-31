use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::model::{
    formats::{block_model_data::BlockModelData, csv_block_model::CsvColumnMapping},
    project::ProjectItemState,
};

/// How many gradient entries a [`NormalizedRamp`] - and so the GPU uniform and
/// the shader loop - can carry. A stored [`ColorTransferFunction`] is *not*
/// bounded by this: OMF puts no limit on a gradient, so a file's colormap is
/// kept whole and only the render-side projection is capped.
pub(crate) const MAX_GRADIENT_ENTRIES: usize = 32;
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
    /// `1 / cell` (the cell size is not stored - nothing reads it), so the
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

/// One boundary of an OMF `Discrete` colormap: a value, and whether that value
/// itself belongs to the band below it.
///
/// Mirrors OMF2 `Boundary::{Less, LessEqual}`. Values are `f64` because that is
/// what [`BlockModelData`] decodes every numeric column to, so a boundary
/// written by Incline Design always matches the type of the numbers it is written
/// alongside - which the standard requires.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct Boundary {
    /// Stable runtime identity used by egui widgets. Nothing else reads it.
    #[serde(default)]
    pub(crate) id: u64,
    pub(crate) value: f64,
    /// `false` is OMF `Less` (values below this boundary sit in the band under
    /// it); `true` is `LessEqual` (this exact value does too).
    #[serde(default)]
    pub(crate) inclusive: bool,
}

impl PartialEq for Boundary {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.inclusive == other.inclusive
    }
}

/// How a variable is coloured. A direct mirror of the OMF2 colormap model, so
/// reading and writing a colormap is a move rather than a translation, and
/// nothing an OMF file can say has to be approximated on the way in.
///
/// Every variant carries a `gradient` of straight (non-premultiplied) **linear**
/// RGBA - the scene renders into an sRGB surface view, so shaders emit linear
/// and the UI converts on the way to and from egui.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum ColorTransferFunction {
    /// OMF `AttributeData::Category`'s optional `gradient`: one colour per
    /// category, keyed by the category's code. Categories have no boundaries -
    /// the code *is* the selector - so this is a lookup, not a ramp.
    Category { gradient: BTreeMap<u32, [f32; 4]> },
    /// OMF `NumberColormap::Continuous`. `gradient` is sampled evenly across
    /// `range`; values outside it clamp to the end colours.
    Continuous { range: (f64, f64), gradient: Vec<[f32; 4]> },
    /// OMF `NumberColormap::Discrete`. `gradient` is one longer than
    /// `boundaries`: `gradient[0]` covers everything below the first boundary,
    /// and `gradient[i + 1]` the band at or above `boundaries[i]`.
    ///
    /// A transparent `gradient[0]` is how a grade cutoff is expressed - it is an
    /// ordinary editable colour, not a marker, so it means the same thing to
    /// Incline Design and to any other reader that honours alpha.
    Discrete { boundaries: Vec<Boundary>, gradient: Vec<[f32; 4]> },
}

impl Default for ColorTransferFunction {
    fn default() -> Self {
        Self::for_range(0.0, 1.0)
    }
}

impl ColorTransferFunction {
    /// The default cut-off / green / yellow / red ramp spread over `min..max`.
    ///
    /// `gradient[0]` is transparent, so everything below the first boundary is
    /// hidden - the legend keeps it that way, making the first boundary a
    /// grade cutoff. The remaining three split the range into equal thirds,
    /// red owning the last third rather than only the single point at the
    /// maximum.
    pub(crate) fn for_range(min: f64, max: f64) -> Self {
        let span = max - min;
        Self::Discrete {
            boundaries: vec![
                Boundary {
                    id: 1,
                    value: min,
                    inclusive: false,
                },
                Boundary {
                    id: 2,
                    value: min + span / 3.0,
                    inclusive: false,
                },
                Boundary {
                    id: 3,
                    value: min + span * 2.0 / 3.0,
                    inclusive: false,
                },
            ],
            gradient: vec![[0.0; 4], [0.0, 0.86, 0.22, 1.0], [1.0, 0.85, 0.0, 1.0], [0.92, 0.0, 0.0, 1.0]],
        }
    }

    /// Restore the invariants every other reader of this type relies on:
    /// boundaries sorted ascending and distinct, and `gradient` exactly one
    /// longer than `boundaries` (or non-empty, for `Continuous`).
    ///
    /// `range` sets the scale the "distinct" epsilon is measured in. Boundaries
    /// outside it are left alone - a colormap is entitled to be authored wider
    /// than the data it covers.
    pub(crate) fn sanitise(&mut self, range: Option<(f64, f64)>) {
        let (min, max) = range.unwrap_or((0.0, 1.0));
        match self {
            Self::Category { .. } => {}
            Self::Continuous { range, gradient } => {
                gradient.truncate(MAX_GRADIENT_ENTRIES);
                if gradient.is_empty() {
                    *gradient = vec![[0.0, 0.86, 0.22, 1.0]];
                }
                for color in gradient.iter_mut() {
                    clamp_rgba(color);
                }
                if !range.0.is_finite() || !range.1.is_finite() || range.1 <= range.0 {
                    *range = (min, max.max(min + f64::MIN_POSITIVE));
                }
            }
            Self::Discrete { boundaries, gradient } => {
                for boundary in boundaries.iter_mut() {
                    if !boundary.value.is_finite() {
                        boundary.value = min;
                    }
                }
                // Sort boundaries and their upper-side colours together; index
                // `i + 1` of the gradient belongs to boundary `i`.
                let mut paired = boundaries
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, boundary)| (boundary, gradient.get(index + 1).copied().unwrap_or([0.0; 4])))
                    .collect::<Vec<_>>();
                paired.sort_by(|a, b| a.0.value.total_cmp(&b.0.value));
                let epsilon = ((max - min).abs() * 1e-4).max(f64::MIN_POSITIVE);
                paired.dedup_by(|a, b| (a.0.value - b.0.value).abs() < epsilon);
                paired.truncate(MAX_GRADIENT_ENTRIES - 1);

                let mut below = gradient.first().copied().unwrap_or([0.0; 4]);
                clamp_rgba(&mut below);
                *boundaries = paired.iter().map(|(boundary, _)| *boundary).collect();
                *gradient = std::iter::once(below)
                    .chain(paired.into_iter().map(|(_, mut color)| {
                        clamp_rgba(&mut color);
                        color
                    }))
                    .collect();
                if boundaries.is_empty() {
                    *self = Self::for_range(min, max);
                }
            }
        }
    }

    /// Resolve against the range the renderer normalises grades with, ready to
    /// sample. See [`NormalizedRamp`].
    pub(crate) fn normalized(&self, range: Option<(f64, f64)>, categories: &BTreeMap<u32, String>) -> NormalizedRamp {
        let Some((min, max)) = range else {
            // No usable range means no block carries a normalised grade
            // either, so the ramp is never sampled. Keep it well-formed.
            return NormalizedRamp::default();
        };
        let degenerate = (max - min).abs() <= f64::EPSILON;
        let to_t = |value: f64| -> f32 { if degenerate { 0.0 } else { (((value - min) / (max - min)).clamp(0.0, 1.0)) as f32 } };
        match self {
            // A category code selects a colour outright. The renderer only
            // knows normalised grades, so the codes become the boundaries of a
            // discrete ramp here - a rendering detail that never reaches the
            // file, where the colours stay a plain per-name list.
            Self::Category { gradient } => {
                let mut codes = categories.keys().copied().collect::<Vec<_>>();
                codes.sort_unstable();
                codes.truncate(MAX_GRADIENT_ENTRIES - 1);
                let mut bands = Vec::with_capacity(codes.len());
                for (index, &code) in codes.iter().enumerate() {
                    // Each band has to contain its own code, so it starts
                    // halfway to the code below.
                    let value = if index == 0 {
                        f64::from(code)
                    } else {
                        (f64::from(codes[index - 1]) + f64::from(code)) * 0.5
                    };
                    bands.push(NormalizedBand {
                        t: to_t(value),
                        inclusive: false,
                        color: gradient.get(&code).copied().unwrap_or(FALLBACK_CATEGORY_COLOR),
                    });
                }
                NormalizedRamp {
                    below: bands.first().map_or([0.0; 4], |band| band.color),
                    bands,
                    interpolate: false,
                }
            }
            Self::Continuous { range: gradient_range, gradient } => {
                let (low, high) = *gradient_range;
                let last = gradient.len().saturating_sub(1).max(1);
                let bands = subsample(gradient, MAX_GRADIENT_ENTRIES)
                    .into_iter()
                    .map(|(index, color)| NormalizedBand {
                        t: to_t(low + (high - low) * index as f64 / last as f64),
                        inclusive: false,
                        color,
                    })
                    .collect::<Vec<_>>();
                NormalizedRamp {
                    // Below the range, OMF clamps to the first gradient entry.
                    below: gradient.first().copied().unwrap_or([0.0; 4]),
                    bands,
                    interpolate: true,
                }
            }
            Self::Discrete { boundaries, gradient } => {
                // The leading colour takes a slot of its own, so the bands get
                // one fewer.
                let bands = subsample(boundaries, MAX_GRADIENT_ENTRIES - 1)
                    .into_iter()
                    .map(|(index, boundary)| NormalizedBand {
                        t: to_t(boundary.value),
                        inclusive: boundary.inclusive,
                        color: gradient.get(index + 1).copied().unwrap_or([0.0; 4]),
                    })
                    .collect();
                NormalizedRamp {
                    below: gradient.first().copied().unwrap_or([0.0; 4]),
                    bands,
                    interpolate: false,
                }
            }
        }
    }
}

/// A category with no colour of its own. Only reachable when a file names more
/// categories than its gradient colours, which the standard forbids.
const FALLBACK_CATEGORY_COLOR: [f32; 4] = [0.72, 0.72, 0.75, 1.0];

/// Take at most `limit` evenly spaced items, keeping both ends, and pair each
/// with its index in the original slice.
///
/// OMF puts no bound on a gradient's length but the shader's stop array does,
/// so a colormap longer than the array is thinned on its way to the renderer.
/// The stored colormap is untouched, and the original index comes back with
/// each item so positions are still computed against the original spacing.
fn subsample<T: Copy>(items: &[T], limit: usize) -> Vec<(usize, T)> {
    if items.len() <= limit || limit < 2 {
        return items.iter().copied().enumerate().collect();
    }
    let last = limit - 1;
    (0..limit)
        .map(|step| {
            let index = (step * (items.len() - 1) + last / 2) / last;
            (index, items[index])
        })
        .collect()
}

fn clamp_rgba(color: &mut [f32; 4]) {
    for channel in color.iter_mut() {
        *channel = channel.clamp(0.0, 1.0);
    }
}

/// A ramp as written into `incline:style` by builds that predate colormaps
/// living in the OMF attribute.
///
/// Those builds stored a stop list with an implicit "everything below the first
/// stop is hidden" rule, and earlier ones stored each stop's position
/// normalised to `0..1` rather than in data units. Both forms deserialise here
/// and [`Self::resolve`] lifts them into a [`ColorTransferFunction::Discrete`],
/// where the implicit cutoff becomes an explicit transparent `gradient[0]`.
/// Nothing writes this any more; it exists so old projects reopen unchanged.
#[derive(Debug, Deserialize)]
pub(crate) struct StoredColorTransferFunction {
    stops: Vec<StoredColorStop>,
    #[serde(default)]
    shape: StoredRampShape,
}

#[derive(Debug, Default, Deserialize)]
enum StoredRampShape {
    #[default]
    Discrete,
    Continuous,
}

#[derive(Debug, Deserialize)]
struct StoredColorStop {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    t: Option<f32>,
    color: [f32; 4],
}

impl StoredColorTransferFunction {
    pub(crate) fn resolve(self, range: Option<(f64, f64)>) -> ColorTransferFunction {
        let (min, max) = range.unwrap_or((0.0, 1.0));
        let stops = self
            .stops
            .into_iter()
            .enumerate()
            .map(|(index, stop)| {
                let value = stop.value.unwrap_or_else(|| min + (max - min) * f64::from(stop.t.unwrap_or(0.0)));
                (
                    Boundary {
                        id: if stop.id == 0 { index as u64 + 1 } else { stop.id },
                        value,
                        inclusive: false,
                    },
                    stop.color,
                )
            })
            .collect::<Vec<_>>();
        if stops.is_empty() {
            return ColorTransferFunction::for_range(min, max);
        }
        match self.shape {
            StoredRampShape::Discrete => ColorTransferFunction::Discrete {
                boundaries: stops.iter().map(|(boundary, _)| *boundary).collect(),
                // The old implicit cutoff, made explicit.
                gradient: std::iter::once([0.0; 4]).chain(stops.iter().map(|(_, color)| *color)).collect(),
            },
            // The old form interpolated between unevenly spaced stops; OMF
            // samples evenly across a range, so resample onto an even grid.
            StoredRampShape::Continuous => {
                let (low, high) = (stops[0].0.value, stops[stops.len() - 1].0.value);
                let samples = stops.len().clamp(2, MAX_GRADIENT_ENTRIES);
                let sample = |value: f64| -> [f32; 4] {
                    match stops.iter().position(|(boundary, _)| boundary.value > value) {
                        None => stops[stops.len() - 1].1,
                        Some(0) => stops[0].1,
                        Some(upper) => {
                            let (low_boundary, low_color) = stops[upper - 1];
                            let (high_boundary, high_color) = stops[upper];
                            let span = high_boundary.value - low_boundary.value;
                            let f = if span.abs() <= f64::EPSILON { 0.0 } else { (value - low_boundary.value) / span };
                            lerp_rgba(low_color, high_color, f as f32)
                        }
                    }
                };
                ColorTransferFunction::Continuous {
                    range: (low, high),
                    gradient: (0..samples).map(|index| sample(low + (high - low) * index as f64 / (samples - 1) as f64)).collect(),
                }
            }
        }
    }
}

/// One band of a [`NormalizedRamp`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NormalizedBand {
    /// Where the band begins, normalised to `0..1` against the render range.
    pub(crate) t: f32,
    /// See [`Boundary::inclusive`]. Meaningless when interpolating.
    pub(crate) inclusive: bool,
    pub(crate) color: [f32; 4],
}

impl NormalizedBand {
    /// Whether `t` sits at or above this band's start - i.e. inside it or past
    /// it. Mirrors [`Boundary::is_above`] in normalised space.
    pub(crate) fn is_above(&self, t: f32) -> bool {
        if self.inclusive { t > self.t } else { t >= self.t }
    }
}

/// A [`ColorTransferFunction`] resolved against a concrete value range, with
/// band positions normalised to `0..1`.
///
/// Blocks reach the shaders carrying a normalised grade (see `normalized_grade`
/// in `rendering/scene/gpu_cache.rs`), so the ramp has to be expressed in the
/// same space to be sampled against them. Building it once per rebuild - rather
/// than normalising per block - is also what keeps the CPU replica in
/// `rendering/scene/block_model_ramp.rs` and the GPU uniform from drifting
/// apart. It is a render-side projection only: it flattens all three colormap
/// kinds into one shape and is capped at [`MAX_GRADIENT_ENTRIES`], neither of
/// which the stored colormap is.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NormalizedRamp {
    /// The colour below the first band - OMF's `gradient[0]`.
    pub(crate) below: [f32; 4],
    pub(crate) bands: Vec<NormalizedBand>,
    pub(crate) interpolate: bool,
}

pub(crate) fn lerp_rgba(a: [f32; 4], b: [f32; 4], f: f32) -> [f32; 4] {
    let f = f.clamp(0.0, 1.0);
    std::array::from_fn(|channel| a[channel] + (b[channel] - a[channel]) * f)
}

/// An axis-aligned crop of a block model, expressed in its local grid
/// coordinates (the frame of `metadata.lower`/`upper` and every block's
/// bounds). Blocks outside the box are removed, boundary blocks are clipped
/// to it, and the volume raycaster shortens its march to the cropped extent.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
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
    pub(crate) state: ProjectItemState,
    pub(crate) name: String,
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
    /// One ramp per colour variable, keyed by variable name. OMF hangs a
    /// colormap off each attribute rather than off the element, so keeping
    /// them per variable is both what the standard assumes and what stops a
    /// user's colours being discarded every time they switch variable and
    /// back. Populated on demand by [`Self::ensure_color_transfer_for_active_variable`].
    pub(crate) color_transfers: BTreeMap<String, ColorTransferFunction>,
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
    /// Per-code occurrence counts across the renderable blocks of the decoded
    /// column, plus the renderable total. Kept beside the values so the legend
    /// never rescans a large model on every UI frame.
    category_code_counts: Option<Arc<CategoryCounts>>,
}

/// Occurrence counts for a categorical colour variable, over the renderable
/// blocks only.
#[derive(Clone, Debug, Default)]
pub(crate) struct CategoryCounts {
    per_code: HashMap<u32, usize>,
    total: usize,
}

impl CategoryCounts {
    /// Whether any renderable block carries this category code.
    pub(crate) fn contains(&self, code: u32) -> bool {
        self.per_code.get(&code).is_some_and(|count| *count > 0)
    }

    /// Share of renderable blocks carrying this category code, `0.0..=1.0`.
    pub(crate) fn fraction(&self, code: u32) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        *self.per_code.get(&code).unwrap_or(&0) as f32 / self.total as f32
    }
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
        let category_code_counts = model
            .variable(name)
            .filter(|variable| categorical_variable(variable))
            .and_then(|_| values.as_deref().map(|values| collect_category_counts(values.as_slice(), renderable_block_indices)));
        RefCell::new(Some(ActiveValuesCacheEntry {
            variable: name.to_owned(),
            values,
            range,
            category_code_counts,
        }))
    }

    pub(crate) fn begin_active_values_decode(&self, variable: &str) {
        *self.active_values_cache.borrow_mut() = Some(ActiveValuesCacheEntry {
            variable: variable.to_owned(),
            values: None,
            range: None,
            category_code_counts: None,
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
        let category_code_counts = self
            .model
            .variable(name)
            .filter(|variable| categorical_variable(variable))
            .and_then(|_| values.as_deref().map(|values| collect_category_counts(values.as_slice(), &self.renderable_block_indices)));
        *cache = Some(ActiveValuesCacheEntry {
            variable: name.to_owned(),
            values,
            range,
            category_code_counts,
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
            .category_code_counts
            .as_ref()
            .map(|counts| counts.contains(code))
    }

    /// Share of the renderable blocks whose active categorical value is `code`,
    /// `0.0..=1.0`. `None` while the column is still decoding or the active
    /// variable is numeric.
    pub(crate) fn active_category_code_fraction(&self, code: u32) -> Option<f32> {
        let name = self.active_color_variable.as_deref()?;
        self.ensure_active_values_cached(name);
        self.active_values_cache
            .borrow()
            .as_ref()?
            .category_code_counts
            .as_ref()
            .map(|counts| counts.fraction(code))
    }

    /// The ramp for the active colour variable, or a neutral one when no
    /// variable is active. Never inserts, so this stays callable behind `&self`
    /// from the render path; [`Self::ensure_color_transfer_for_active_variable`]
    /// is what populates the map.
    pub(crate) fn color_transfer(&self) -> &ColorTransferFunction {
        static FALLBACK: std::sync::LazyLock<ColorTransferFunction> = std::sync::LazyLock::new(ColorTransferFunction::default);
        self.active_color_variable.as_deref().and_then(|name| self.color_transfers.get(name)).unwrap_or(&FALLBACK)
    }

    /// The active ramp resolved against the render range the shaders normalise
    /// grades with, ready to sample. See [`NormalizedRamp`].
    pub(crate) fn normalized_ramp(&self) -> NormalizedRamp {
        static NO_CATEGORIES: std::sync::LazyLock<BTreeMap<u32, String>> = std::sync::LazyLock::new(BTreeMap::new);
        let categories = self
            .active_color_variable
            .as_deref()
            .and_then(|name| self.model.variable(name))
            .map_or(&*NO_CATEGORIES, |variable| &variable.strings);
        self.color_transfer().normalized(self.active_value_range(), categories)
    }

    /// Replace the active variable's ramp, restoring its invariants first.
    pub(crate) fn set_color_transfer_for_active_variable(&mut self, mut transfer: ColorTransferFunction) {
        let Some(name) = self.active_color_variable.clone() else {
            return;
        };
        transfer.sanitise(self.active_value_range());
        self.color_transfers.insert(name, transfer);
    }

    /// Give the active variable a ramp if it has none yet, derived from its
    /// kind: a colour per category for a categorical column, otherwise the
    /// default cut-off/green/yellow/red spread over its render range.
    /// Existing ramps are left alone - that is what makes switching variable
    /// and back non-destructive.
    pub(crate) fn ensure_color_transfer_for_active_variable(&mut self) {
        let Some(name) = self.active_color_variable.clone() else {
            return;
        };
        if self.color_transfers.contains_key(&name) {
            return;
        }
        let transfer = self.default_color_transfer_for(&name);
        self.color_transfers.insert(name, transfer);
    }

    /// Discard the active variable's ramp and rebuild it from the data.
    pub(crate) fn reset_color_transfer_for_active_variable(&mut self) {
        let Some(name) = self.active_color_variable.clone() else {
            return;
        };
        let transfer = self.default_color_transfer_for(&name);
        self.color_transfers.insert(name, transfer);
    }

    fn default_color_transfer_for(&self, name: &str) -> ColorTransferFunction {
        let range = self.active_value_range();
        match self.model.variable(name) {
            Some(variable) if categorical_variable(variable) => categorical_color_transfer(variable),
            _ => {
                let (min, max) = range.unwrap_or((0.0, 1.0));
                ColorTransferFunction::for_range(min, max)
            }
        }
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

    /// The model's extent in its local grid frame - the frame block bounds,
    /// schemas, and [`Self::slice`] are expressed in.
    pub(crate) fn local_bounds(&self) -> (DVec3, DVec3) {
        (self.model.metadata.lower, self.model.metadata.upper)
    }

    /// The slice clamped to the model's local bounds, or `None` when there is
    /// no slice or it crops nothing - so renderers can compare/act on "does
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

fn collect_category_counts(values: &[f64], renderable: &RenderableBlockIndices) -> Arc<CategoryCounts> {
    let mut per_code: HashMap<u32, usize> = HashMap::new();
    let mut total = 0usize;
    for value in renderable.iter().filter_map(|index| values.get(index).copied()) {
        if value.is_finite() && value.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(&value) {
            *per_code.entry(value as u32).or_insert(0) += 1;
            total += 1;
        }
    }
    Arc::new(CategoryCounts { per_code, total })
}

/// The colour list a categorical variable starts with: whatever palette the
/// source file shipped, and an invented one for any category it left out.
pub(crate) fn categorical_color_transfer(variable: &crate::model::formats::block_model_data::BlockVariable) -> ColorTransferFunction {
    const PALETTE: [[f32; 4]; 12] = [
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
    ColorTransferFunction::Category {
        gradient: variable
            .strings
            .keys()
            .enumerate()
            .map(|(index, &code)| {
                let color = variable.category_colors.get(&code).copied().unwrap_or(PALETTE[index % PALETTE.len()]);
                (code, color)
            })
            .collect(),
    }
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
