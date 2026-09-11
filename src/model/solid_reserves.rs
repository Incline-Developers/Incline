//! Volume-prorated reserves for closed planning slabs.
//!
//! The input contract is a *partition*, not a flat list of slabs. Planning
//! divides a solid vertically into bands - benches, then the flitches inside
//! them - and then divides each band laterally into blasts and dig blocks. The
//! vertical bands are ordered and never overlap, but two dig blocks in one
//! flitch share that band's elevations exactly. Flattening both levels into one
//! ordered list is impossible, which is why [`ReservePartition`] keeps them
//! apart: bands carry the ordering, pieces carry the lateral split.
mod intersection;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use super::{
    ReserveAggregation, ReserveField, ReserveFieldId, ReserveFieldIssue,
    block_model::{BlockBoundsSource, ReserveFieldMapping, ReserveMappingSource},
    formats::{block_model_data::BlockModelData, mesh_data::Triangulation},
    spatial::TriangleBvh,
};

/// How much of one block may be claimed across the whole partition before the
/// pieces are treated as genuinely overlapping rather than merely rounded.
///
/// A block is clipped independently against every piece it touches, so the
/// error grows with the number of clips rather than staying fixed. The budget
/// is therefore per touched piece, on top of a floor that covers a single
/// clip's own conditioning. Both are fractions of one block's volume; the
/// clipper itself works in block-local units, so they are scale free.
const OVERLAP_FLOOR: f64 = 1e-7;
const OVERLAP_PER_PIECE: f64 = 1e-9;

#[derive(Clone, Debug, Default)]
pub(crate) struct NumericTotal {
    pub(crate) sum: f64,
    pub(crate) weight: f64,
    /// Blocks that actually contributed a finite value to `sum`.
    ///
    /// The accumulator's own availability, carried at every scope it is kept
    /// at - piece, category and merged selection alike. Without it a field
    /// nothing contributed to is indistinguishable from one that genuinely
    /// summed to nothing, and a dash's reason computed for the whole solid
    /// cannot speak for one selected block.
    pub(crate) contributions: u64,
    /// Blocks in range whose value was not a finite number.
    pub(crate) missing: u64,
    /// Blocks in range whose weight was absent, zero or negative.
    pub(crate) unusable_weights: u64,
}
impl NumericTotal {
    /// The figure, or `None` when nothing measured contributed to it.
    ///
    /// A measured zero keeps its zero; an absent one stays absent. The two
    /// must never render alike, which is what returning `Some(0.0)` for an
    /// all-missing column did.
    pub(crate) fn value(&self, aggregation: ReserveAggregation) -> Option<f64> {
        if self.contributions == 0 {
            return None;
        }
        match aggregation {
            ReserveAggregation::Sum => self.sum.is_finite().then_some(self.sum),
            ReserveAggregation::WeightedAverage { .. } => (self.weight > 0.0 && self.weight.is_finite() && self.sum.is_finite()).then(|| self.sum / self.weight),
            ReserveAggregation::Category => None,
        }
    }

    /// Whether some blocks in range contributed and others did not, so a
    /// figure can be shown as a partial one rather than a complete total.
    pub(crate) fn is_partial(&self) -> bool {
        self.contributions > 0 && (self.missing > 0 || self.unusable_weights > 0)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ReserveGroup {
    pub(crate) equivalent_blocks: f64,
    /// Cubic metres of block-model ground measured into this group. Kept
    /// apart from the piece's own geometric volume so a solid that the model
    /// only partly covers cannot read as fully measured.
    pub(crate) covered_volume: f64,
    pub(crate) numeric: HashMap<ReserveFieldId, NumericTotal>,
}
impl ReserveGroup {
    fn merge(&mut self, other: &Self) {
        self.equivalent_blocks += other.equivalent_blocks;
        self.covered_volume += other.covered_volume;
        // A missing mapping must not masquerade as a complete selected total.
        self.numeric.retain(|id, total| {
            if let Some(value) = other.numeric.get(id) {
                total.sum += value.sum;
                total.weight += value.weight;
                total.contributions += value.contributions;
                total.missing += value.missing;
                total.unusable_weights += value.unusable_weights;
                true
            } else {
                false
            }
        });
    }
}
#[derive(Clone, Debug, Default)]
pub(crate) struct ReserveTotals {
    pub(crate) all: ReserveGroup,
    pub(crate) categories: HashMap<ReserveFieldId, BTreeMap<String, ReserveGroup>>,
}
impl ReserveTotals {
    pub(crate) fn merge(&mut self, other: &Self) {
        self.all.merge(&other.all);
        self.categories.retain(|id, groups| {
            let Some(incoming) = other.categories.get(id) else {
                return false;
            };
            for (name, group) in incoming {
                if let Some(existing) = groups.get_mut(name) {
                    existing.merge(group);
                } else {
                    groups.insert(name.clone(), group.clone());
                }
            }
            true
        });
    }
}

/// One closed piece of ground within a band: a whole flitch, a blast's share
/// of one, or a single dig block.
pub(crate) struct ReservePiece {
    pub(crate) mesh: Arc<Triangulation>,
    pub(crate) spatial: Arc<TriangleBvh>,
    /// Where this piece's totals belong in the caller's own result list.
    /// Several pieces may share a slot - a dig block split in two by a hole
    /// is still one dig block - and their totals then add together.
    pub(crate) slot: usize,
}

/// One vertical band of a solid, and the pieces it is laterally divided into.
pub(crate) struct ReserveBand {
    pub(crate) base: f64,
    pub(crate) top: f64,
    pub(crate) pieces: Vec<ReservePiece>,
}

/// A whole solid's partition: ordered bands, bottom up, each cut laterally.
pub(crate) struct ReservePartition {
    pub(crate) bands: Vec<ReserveBand>,
    /// How many result slots to return. Slots with no piece come back empty
    /// rather than missing, so the caller's own indexing still lines up.
    pub(crate) slots: usize,
}

impl ReservePartition {
    /// Bands must be finite, non-degenerate and disjoint, bottom up; every
    /// piece must name a slot that exists. Checked once before the scan so a
    /// malformed partition is a reported error rather than a wrong number.
    fn validate(&self) -> anyhow::Result<()> {
        for band in &self.bands {
            anyhow::ensure!(
                band.base.is_finite() && band.top.is_finite() && band.base < band.top,
                "Reserve band {}–{} is not a finite, non-empty elevation range",
                band.base,
                band.top
            );
            anyhow::ensure!(
                band.pieces.iter().all(|piece| piece.slot < self.slots),
                "Reserve piece names a result slot outside the partition"
            );
        }
        anyhow::ensure!(
            self.bands.windows(2).all(|pair| pair[0].top <= pair[1].base + 1e-9),
            "Reserve bands must be ordered bottom up and must not overlap"
        );
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.bands.iter().all(|band| band.pieces.is_empty())
    }
}

enum Values {
    Column(Arc<Vec<f64>>),
    Constant(f64),
}
impl Values {
    fn at(&self, index: usize) -> f64 {
        match self {
            Self::Column(values) => values[index],
            Self::Constant(value) => *value,
        }
    }
}
struct NumericField {
    id: ReserveFieldId,
    values: Values,
    weights: Option<Values>,
}
struct CategoryField {
    id: ReserveFieldId,
    values: Values,
    labels: BTreeMap<u32, String>,
}

/// Why each field of the project's list did or did not reach the totals.
///
/// Carried beside the numbers so the panels can say what a dash means instead
/// of drawing one for every absent value alike.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReserveResolution {
    /// Fields that produced no accumulator at all, and why.
    pub(crate) issues: Vec<(ReserveFieldId, ReserveFieldIssue)>,
    /// Blocks whose mapped value was not a finite number, per field.
    pub(crate) missing_values: HashMap<ReserveFieldId, u64>,
    /// Blocks whose weight was absent or not positive, per weighted field.
    pub(crate) unusable_weights: HashMap<ReserveFieldId, u64>,
    /// Category values with no label in the model's own dictionary.
    pub(crate) unmapped_labels: HashMap<ReserveFieldId, u64>,
}

impl ReserveResolution {
    pub(crate) fn issue(&self, field: ReserveFieldId) -> Option<&ReserveFieldIssue> {
        self.issues.iter().find(|(id, _)| *id == field).map(|(_, issue)| issue)
    }
}

#[derive(Debug)]
pub(crate) struct ReserveOutcome {
    pub(crate) totals: Vec<ReserveTotals>,
    pub(crate) resolution: ReserveResolution,
}

/// Resolve one field's mapping onto a model's own columns, reporting why it
/// could not be resolved rather than dropping it silently.
///
/// Shared with the Block Models step's whole-model statistics so a field that
/// is dashed there is dashed for the same stated reason here.
pub(crate) fn resolve_mapping(
    model: &BlockModelData,
    fields: &[ReserveField],
    mapping: &[ReserveFieldMapping],
    field: ReserveFieldId,
    blocks: usize,
) -> Result<(), ReserveFieldIssue> {
    let Some(entry) = mapping.iter().find(|entry| entry.field == field) else {
        return Err(ReserveFieldIssue::Unmapped);
    };
    let name = match &entry.source {
        ReserveMappingSource::Constant(value) => {
            return if value.is_finite() { Ok(()) } else { Err(ReserveFieldIssue::ConstantNotFinite) };
        }
        ReserveMappingSource::Column(name) => name,
    };
    let categorical = fields.iter().any(|entry| entry.id == field && entry.aggregation == ReserveAggregation::Category);
    let declared = if categorical { model.categorical_variables() } else { model.numeric_variables() };
    if !declared.iter().any(|variable| variable.name == *name) {
        return Err(if model.variable(name).is_some() {
            ReserveFieldIssue::WrongColumnKind {
                column: name.clone(),
                wanted_categorical: categorical,
            }
        } else {
            ReserveFieldIssue::MissingColumn(name.clone())
        });
    }
    let Some(values) = model.shared_numeric_values(name) else {
        return Err(ReserveFieldIssue::ColumnNotResident(name.clone()));
    };
    if values.len() != blocks {
        return Err(ReserveFieldIssue::LengthMismatch {
            column: name.clone(),
            values: values.len(),
            blocks,
        });
    }
    Ok(())
}

/// The issue that stops a weighted average, if anything does.
fn weight_issue(fields: &[ReserveField], weight_field: ReserveFieldId) -> Option<ReserveFieldIssue> {
    match fields.iter().find(|field| field.id == weight_field) {
        None => Some(ReserveFieldIssue::WeightFieldMissing),
        Some(field) if field.aggregation != ReserveAggregation::Sum => Some(ReserveFieldIssue::WeightNotSummed(field.name.clone())),
        Some(_) => None,
    }
}

pub(crate) fn compute(
    model: &BlockModelData,
    blocks: &BlockBoundsSource,
    mapping: &[ReserveFieldMapping],
    fields: &[ReserveField],
    partition: &ReservePartition,
    cancel: &crate::app::jobs::CancelFlag,
    progress: &super::progress::Progress,
) -> anyhow::Result<ReserveOutcome> {
    anyhow::ensure!(blocks.len() == model.metadata.n_blocks, "Block geometry and attribute counts disagree");
    partition.validate()?;

    let mut resolution = ReserveResolution::default();
    let note = |resolution: &mut ReserveResolution, id: ReserveFieldId, issue: ReserveFieldIssue| {
        if resolution.issue(id).is_none() {
            resolution.issues.push((id, issue));
        }
    };
    let resolve = |id: ReserveFieldId, resolution: &mut ReserveResolution| -> Option<Values> {
        if let Err(issue) = resolve_mapping(model, fields, mapping, id, blocks.len()) {
            note(resolution, id, issue);
            return None;
        }
        match &mapping.iter().find(|entry| entry.field == id)?.source {
            ReserveMappingSource::Constant(value) => Some(Values::Constant(*value)),
            ReserveMappingSource::Column(name) => model.shared_numeric_values(name).map(Values::Column),
        }
    };
    let mut numeric = Vec::new();
    let mut categories = Vec::new();
    for field in fields {
        let Some(values) = resolve(field.id, &mut resolution) else {
            continue;
        };
        match field.aggregation {
            ReserveAggregation::Sum => numeric.push(NumericField {
                id: field.id,
                values,
                weights: None,
            }),
            ReserveAggregation::WeightedAverage { weight_field } => {
                if let Some(issue) = weight_issue(fields, weight_field) {
                    note(&mut resolution, field.id, issue);
                    continue;
                }
                match resolve(weight_field, &mut resolution) {
                    Some(weights) => numeric.push(NumericField {
                        id: field.id,
                        values,
                        weights: Some(weights),
                    }),
                    // The weight's own reason is recorded against the weight
                    // field; this one fails because that one did.
                    None => note(&mut resolution, field.id, ReserveFieldIssue::WeightUnresolved),
                }
            }
            ReserveAggregation::Category => {
                let labels = mapping
                    .iter()
                    .find(|entry| entry.field == field.id)
                    .and_then(|entry| match &entry.source {
                        ReserveMappingSource::Column(name) => model.variable(name).map(|variable| variable.strings.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                categories.push(CategoryField { id: field.id, values, labels });
            }
        }
    }

    let empty = ReserveGroup {
        equivalent_blocks: 0.0,
        covered_volume: 0.0,
        numeric: numeric.iter().map(|field| (field.id, NumericTotal::default())).collect(),
    };
    let mut totals = vec![
        ReserveTotals {
            all: empty.clone(),
            categories: categories.iter().map(|field| (field.id, BTreeMap::new())).collect()
        };
        partition.slots
    ];
    if partition.is_empty() {
        return Ok(ReserveOutcome { totals, resolution });
    }

    let mut counters = Counters::default();
    // Which fields have already had a missing value counted for the block
    // being scanned, so a block split across several pieces reports one
    // missing value rather than one per piece.
    let mut counted: Vec<bool> = vec![false; numeric.len()];
    let mut contributing_blocks = 0u64;

    let mut stack = Vec::new();
    for index in 0..blocks.len() {
        if index.is_multiple_of(1024) {
            anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
            progress.set_items(index as u64, blocks.len() as u64);
        }
        let bounds = blocks.get(index).ok_or_else(|| anyhow::anyhow!("Missing block geometry at {index}"))?;
        let block = intersection::Block::new(model, bounds)?;
        let block_volume = block.world_volume();
        // Bands are ordered, so the first that can reach this block is found
        // by bisection and the scan stops as soon as one starts above it.
        let first = partition.bands.partition_point(|band| band.top <= block.min.z);
        let mut assigned = 0.0;
        let mut touched = 0usize;
        counted.iter_mut().for_each(|flag| *flag = false);
        for band in &partition.bands[first..] {
            if band.base >= block.max.z {
                break;
            }
            for piece in &band.pieces {
                let fraction = block.fraction(piece, &mut stack, cancel)?;
                if fraction <= 0.0 {
                    continue;
                }
                touched += 1;
                assigned += fraction;
                anyhow::ensure!(
                    assigned <= 1.0 + OVERLAP_FLOOR + OVERLAP_PER_PIECE * touched as f64,
                    "Block overlap exceeds its volume across planning pieces; the partition overlaps itself"
                );
                let total = &mut totals[piece.slot];
                add(&mut total.all, &numeric, index, fraction, block_volume, Some((&mut counted, &mut counters)));
                for field in &categories {
                    let value = field.values.at(index);
                    let first_touch = touched == 1;
                    let label = if !value.is_finite() {
                        if first_touch {
                            *counters.missing_values.entry(field.id).or_default() += 1;
                        }
                        crate::i18n::tr!("planning-empty-category")
                    } else if value.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(&value) {
                        match field.labels.get(&(value as u32)) {
                            Some(label) => label.clone(),
                            None => {
                                if first_touch {
                                    *counters.unmapped_labels.entry(field.id).or_default() += 1;
                                }
                                value.to_string()
                            }
                        }
                    } else {
                        if first_touch {
                            *counters.unmapped_labels.entry(field.id).or_default() += 1;
                        }
                        value.to_string()
                    };
                    let group = total.categories.get_mut(&field.id).unwrap().entry(label).or_insert_with(|| empty.clone());
                    add(group, &numeric, index, fraction, block_volume, None);
                }
            }
        }
        contributing_blocks += u64::from(touched > 0);
    }
    progress.set_items(blocks.len() as u64, blocks.len() as u64);
    // A field every contributing block was missing is not a measured zero.
    if contributing_blocks > 0 {
        for field in &numeric {
            if counters.missing_values.get(&field.id).copied().unwrap_or(0) >= contributing_blocks {
                note(&mut resolution, field.id, ReserveFieldIssue::AllValuesMissing);
            }
        }
        for field in &categories {
            if counters.missing_values.get(&field.id).copied().unwrap_or(0) >= contributing_blocks {
                note(&mut resolution, field.id, ReserveFieldIssue::AllValuesMissing);
            }
        }
    }
    resolution.missing_values = counters.missing_values;
    resolution.unusable_weights = counters.unusable_weights;
    resolution.unmapped_labels = counters.unmapped_labels;
    Ok(ReserveOutcome { totals, resolution })
}

/// Per-block data-quality tallies gathered during one scan.
#[derive(Default)]
struct Counters {
    missing_values: HashMap<ReserveFieldId, u64>,
    unusable_weights: HashMap<ReserveFieldId, u64>,
    unmapped_labels: HashMap<ReserveFieldId, u64>,
}

/// Accumulate one block's share of every numeric field into a group.
///
/// `tally` is present only for the group that owns the block's counts - the
/// `all` group. Category sub-groups measure the same block again and would
/// otherwise double-count its missing values.
fn add(group: &mut ReserveGroup, numeric: &[NumericField], index: usize, fraction: f64, block_volume: f64, mut tally: Option<(&mut Vec<bool>, &mut Counters)>) {
    group.equivalent_blocks += fraction;
    group.covered_volume += fraction * block_volume;
    for (slot, field) in numeric.iter().enumerate() {
        let total = group.numeric.get_mut(&field.id).unwrap();
        let value = field.values.at(index);
        if !value.is_finite() {
            // Counted on the accumulator itself as well as in the run-wide
            // tally, so a selection of one piece can say what is missing from
            // that piece rather than from the solid it belongs to.
            total.missing += 1;
            if let Some((counted, counters)) = tally.as_mut()
                && !counted[slot]
            {
                counted[slot] = true;
                *counters.missing_values.entry(field.id).or_default() += 1;
            }
            continue;
        }
        match &field.weights {
            None => {
                total.sum += value * fraction;
                total.contributions += 1;
            }
            Some(weights) => {
                let weight = weights.at(index) * fraction;
                if weight.is_finite() && weight > 0.0 {
                    total.sum += value * weight;
                    total.weight += weight;
                    total.contributions += 1;
                } else {
                    total.unusable_weights += 1;
                    if let Some((counted, counters)) = tally.as_mut()
                        && !counted[slot]
                    {
                        counted[slot] = true;
                        *counters.unusable_weights.entry(field.id).or_default() += 1;
                    }
                }
            }
        }
    }
}
