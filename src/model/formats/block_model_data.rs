use std::{collections::BTreeMap, error::Error, fmt, mem::size_of, sync::Arc};

use glam::{DMat3, DVec3};

use crate::model::block_model::RenderableBlockIndices;

#[cfg(all(not(target_arch = "wasm32"), target_pointer_width = "64"))]
const MAX_ALLOCATION_BYTES: usize = 8 * 1024 * 1024 * 1024;
#[cfg(any(target_arch = "wasm32", target_pointer_width = "32"))]
const MAX_ALLOCATION_BYTES: usize = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct BlockModelData {
    pub(crate) metadata: BlockModelMetadata,
    rotation: DMat3,
    numeric_values: BTreeMap<String, Arc<Vec<f64>>>,
    _reservation: crate::app::memory::MemoryReservation,
}

#[derive(Clone, Debug)]
pub(crate) struct BlockModelColumn {
    pub(crate) name: String,
    pub(crate) values: Arc<Vec<f64>>,
    /// Category code to display label. `None` identifies an ordinary numeric
    /// column; categorical columns store their codes in `values` as exact
    /// non-negative integers and use `NaN` for blank CSV cells.
    pub(crate) categories: Option<BTreeMap<u32, String>>,
    /// Category code to the colour the source file assigned it, for formats
    /// that ship one. Empty when the source has no palette of its own and the
    /// built-in categorical palette should be used instead.
    pub(crate) category_colors: BTreeMap<u32, [f32; 4]>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BlockModelMetadata {
    pub(crate) n_blocks: usize,
    pub(crate) origin: DVec3,
    pub(crate) lower: DVec3,
    pub(crate) upper: DVec3,
    pub(crate) dims: [usize; 3],
    pub(crate) is_irregular: bool,
    pub(crate) variables: Vec<BlockVariable>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BlockVariable {
    pub(crate) name: String,
    pub(crate) physical_type: String,
    pub(crate) default: String,
    pub(crate) global: String,
    pub(crate) strings: BTreeMap<u32, String>,
    /// Source-supplied colour per category code; see
    /// [`BlockModelColumn::category_colors`].
    pub(crate) category_colors: BTreeMap<u32, [f32; 4]>,
    pub(crate) special: bool,
}

#[derive(Debug)]
pub(crate) struct BlockModelDataError(String);

impl fmt::Display for BlockModelDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for BlockModelDataError {}

fn invalid(message: impl Into<String>) -> BlockModelDataError {
    BlockModelDataError(message.into())
}

fn validate_allocation<T>(len: usize, description: &str) -> Result<(), BlockModelDataError> {
    let bytes = len
        .checked_mul(size_of::<T>())
        .ok_or_else(|| invalid(format!("{description} allocation size overflows ({len} items)")))?;
    if bytes > MAX_ALLOCATION_BYTES {
        return Err(invalid(format!(
            "{description} allocation would require {bytes} bytes, exceeding the {MAX_ALLOCATION_BYTES}-byte import limit"
        )));
    }
    Ok(())
}

impl BlockModelData {
    pub(crate) fn unloaded(metadata: BlockModelMetadata) -> Self {
        Self {
            metadata,
            rotation: DMat3::IDENTITY,
            numeric_values: BTreeMap::new(),
            _reservation: crate::app::memory::MemoryReservation::untracked(),
        }
    }

    pub(crate) fn release_values(&mut self) {
        self.numeric_values.clear();
        self._reservation = crate::app::memory::MemoryReservation::untracked();
    }

    pub(crate) fn from_regular_grid(lower: DVec3, cell: DVec3, dims: [usize; 3], columns: Vec<BlockModelColumn>) -> Result<Self, BlockModelDataError> {
        let n_blocks = dims
            .into_iter()
            .try_fold(1usize, |count, dim| count.checked_mul(dim))
            .ok_or_else(|| invalid("regular block-grid dimensions overflow"))?;
        if dims.contains(&0) || !cell.is_finite() || cell.min_element() <= 0.0 {
            return Err(invalid("regular block-grid dimensions and cell sizes must be positive"));
        }
        let upper = lower + cell * DVec3::from_array(dims.map(|dim| dim as f64));
        let mut model = Self::from_columns(n_blocks, lower, upper, columns, 0)?;
        model.metadata.dims = dims;
        model.metadata.is_irregular = false;
        Ok(model)
    }

    pub(crate) fn from_columns(n_blocks: usize, lower: DVec3, upper: DVec3, columns: Vec<BlockModelColumn>, retained_geometry_bytes: usize) -> Result<Self, BlockModelDataError> {
        if n_blocks == 0 {
            return Err(invalid("in-memory block model has no blocks"));
        }
        if !lower.is_finite() || !upper.is_finite() || lower.cmpge(upper).any() {
            return Err(invalid("in-memory block-model bounds are invalid"));
        }

        let mut numeric_values = BTreeMap::new();
        let mut variables = Vec::with_capacity(columns.len());
        let mut retained_bytes = retained_geometry_bytes;
        for column in columns {
            let BlockModelColumn {
                name,
                values,
                categories,
                category_colors,
            } = column;
            if name.trim().is_empty() {
                return Err(invalid("in-memory block-model resource name is blank"));
            }
            if values.len() != n_blocks {
                return Err(invalid(format!("in-memory block-model resource '{name}' has {} values; expected {n_blocks}", values.len())));
            }
            validate_allocation::<f64>(values.len(), "in-memory numeric values")?;
            retained_bytes = retained_bytes
                .checked_add(values.len().checked_mul(size_of::<f64>()).ok_or_else(|| invalid("in-memory resource size overflows"))?)
                .ok_or_else(|| invalid("in-memory model retained size overflows"))?;
            if let Some(categories) = categories.as_ref() {
                for (&code, label) in categories {
                    if label.trim().is_empty() {
                        return Err(invalid(format!("in-memory categorical resource '{name}' has a blank label for code {code}")));
                    }
                }
                if let Some(value) = values
                    .iter()
                    .copied()
                    .find(|value| value.is_finite() && (value.fract() != 0.0 || !(0.0..=u32::MAX as f64).contains(value) || !categories.contains_key(&(*value as u32))))
                {
                    return Err(invalid(format!("in-memory categorical resource '{name}' contains unmapped category code {value}")));
                }
                let category_bytes = categories.values().try_fold(0usize, |bytes, label| {
                    bytes
                        .checked_add(size_of::<(u32, String)>())
                        .and_then(|bytes| bytes.checked_add(label.len()))
                        .ok_or_else(|| invalid("in-memory category metadata size overflows"))
                })?;
                retained_bytes = retained_bytes
                    .checked_add(category_bytes)
                    .ok_or_else(|| invalid("in-memory model retained size overflows"))?;
            }
            if numeric_values.insert(name.clone(), values).is_some() {
                return Err(invalid(format!("duplicate in-memory block-model resource '{name}'")));
            }
            let physical_type = match categories.as_ref().map(BTreeMap::len) {
                None => "double",
                Some(count) if count <= (u8::MAX as usize + 1) => "namedbyte",
                Some(_) => "namedshort",
            };
            variables.push(BlockVariable {
                name,
                physical_type: physical_type.to_owned(),
                default: if categories.is_some() { String::new() } else { "NaN".to_owned() },
                global: if categories.is_some() { String::new() } else { "NaN".to_owned() },
                strings: categories.unwrap_or_default(),
                category_colors,
                special: false,
            });
        }

        let metadata = BlockModelMetadata {
            n_blocks,
            origin: DVec3::ZERO,
            lower,
            upper,
            dims: [n_blocks, 1, 1],
            is_irregular: true,
            variables,
        };
        let reservation = crate::app::memory::reserve(retained_bytes, "in-memory block model").map_err(invalid)?;
        Ok(Self {
            metadata,
            rotation: DMat3::IDENTITY,
            numeric_values,
            _reservation: reservation,
        })
    }

    /// Approximate retained size, used by the undo history's memory budget.
    /// Columns shared through an `Arc` are counted in full: over-counting only
    /// evicts a deleted model from the stack sooner than strictly necessary.
    pub(crate) fn estimated_bytes(&self) -> usize {
        size_of::<Self>()
            + self
                .numeric_values
                .iter()
                .map(|(name, values)| name.len() + values.len() * size_of::<f64>())
                .fold(0usize, usize::saturating_add)
    }

    pub(crate) fn shared_numeric_values(&self, name: &str) -> Option<Arc<Vec<f64>>> {
        self.numeric_values.get(name).map(Arc::clone)
    }

    pub(crate) fn variable(&self, name: &str) -> Option<&BlockVariable> {
        self.metadata.variables.iter().find(|variable| variable.name == name)
    }

    pub(crate) fn numeric_variables(&self) -> Vec<&BlockVariable> {
        self.metadata.variables.iter().filter(|variable| is_numeric_type(&variable.physical_type)).collect()
    }

    pub(crate) fn color_variables(&self) -> Vec<&BlockVariable> {
        self.metadata.variables.iter().filter(|variable| is_color_type(&variable.physical_type)).collect()
    }

    pub(crate) fn color_values(&self, name: &str) -> Result<Vec<f64>, BlockModelDataError> {
        self.values_range(name, 0, self.metadata.n_blocks, is_color_type, "colour")
    }

    pub(crate) fn color_values_range(&self, name: &str, start: usize, end: usize) -> Result<Vec<f64>, BlockModelDataError> {
        self.values_range(name, start, end, is_color_type, "colour")
    }

    pub(crate) fn numeric_values(&self, name: &str) -> Result<Vec<f64>, BlockModelDataError> {
        self.numeric_values_range(name, 0, self.metadata.n_blocks)
    }

    pub(crate) fn numeric_values_range(&self, name: &str, start: usize, end: usize) -> Result<Vec<f64>, BlockModelDataError> {
        self.values_range(name, start, end, is_numeric_type, "numeric")
    }

    fn values_range(&self, name: &str, start: usize, end: usize, supported: impl Fn(&str) -> bool, kind: &str) -> Result<Vec<f64>, BlockModelDataError> {
        let variable = self.variable(name).ok_or_else(|| invalid(format!("unknown block variable '{name}'")))?;
        if !supported(&variable.physical_type) {
            return Err(invalid(format!("variable '{}' is not {kind} ({})", variable.name, variable.physical_type)));
        }
        if start > end || end > self.metadata.n_blocks {
            return Err(invalid(format!(
                "block variable '{}' requested invalid range {start}..{end}; model has {} blocks",
                variable.name, self.metadata.n_blocks
            )));
        }
        let values = self
            .numeric_values
            .get(name)
            .ok_or_else(|| invalid(format!("block variable '{name}' has no value column")))?;
        Ok(values[start..end].to_vec())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn unsupported_variables(&self) -> Vec<&BlockVariable> {
        self.metadata.variables.iter().filter(|variable| !is_color_type(&variable.physical_type)).collect()
    }

    pub(crate) fn renderable_block_indices(&self) -> Result<RenderableBlockIndices, BlockModelDataError> {
        Ok(RenderableBlockIndices::All(self.metadata.n_blocks))
    }

    pub(crate) fn local_to_world(&self, local: DVec3) -> DVec3 {
        self.metadata.origin + self.rotation * local
    }

    pub(crate) fn rotation(&self) -> DMat3 {
        self.rotation
    }

    pub(crate) fn origin(&self) -> DVec3 {
        self.metadata.origin
    }

    /// Install the OMF local-to-world frame after constructing imported block
    /// geometry and columns in local coordinates.
    pub(crate) fn with_transform(mut self, origin: DVec3, rotation: DMat3) -> Result<Self, BlockModelDataError> {
        if !origin.is_finite() || !rotation.is_finite() {
            return Err(invalid("block-model transform contains non-finite values"));
        }
        self.metadata.origin = origin;
        self.rotation = rotation;
        Ok(self)
    }
}

fn is_numeric_type(physical_type: &str) -> bool {
    matches!(physical_type, "float" | "short" | "int" | "longlong" | "double")
}

fn is_color_type(physical_type: &str) -> bool {
    is_numeric_type(physical_type) || matches!(physical_type, "namedbyte" | "namedshort")
}
