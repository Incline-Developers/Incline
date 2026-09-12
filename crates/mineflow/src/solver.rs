//! Pseudoflow optimization with exact integer arithmetic.
use std::time::Duration;

use web_time::Instant;

use crate::{
    BlockValueSource, Precedence, ValueScale,
    error::{Error, Result, invalid},
    pseudoflow::Forest,
};

#[derive(Clone, Debug, PartialEq)]
pub struct SolveInfo {
    pub elapsed: Duration,
    pub num_blocks: i64,
    pub num_blocks_in_pit: i64,
    /// Cumulative number of tree arcs created during the solve.
    pub num_precedence_constraints_used: usize,
    /// Exact total, including when the sum exceeds the i64 input range.
    pub pit_value: i128,
    pub pit_value_exact: String,
}
impl SolveInfo {
    pub fn pit_value_with(&self, scale: ValueScale) -> f64 {
        self.pit_value as f64 / scale.factor()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pit {
    selected: Vec<bool>,
    num_selected: usize,
}
impl Pit {
    fn new(selected: Vec<bool>) -> Self {
        let num_selected = selected.iter().filter(|&&v| v).count();
        Self { selected, num_selected }
    }
    pub fn contains(&self, block: i64) -> bool {
        usize::try_from(block).ok().and_then(|i| self.selected.get(i)).copied().unwrap_or(false)
    }
    pub fn blocks(&self) -> impl Iterator<Item = i64> + '_ {
        self.selected.iter().enumerate().filter(|&(_, &v)| v).map(|(i, _)| i as i64)
    }
    pub fn num_blocks(&self) -> usize {
        self.selected.len()
    }
    pub fn num_selected(&self) -> usize {
        self.num_selected
    }
    pub fn as_slice(&self) -> &[bool] {
        &self.selected
    }
    pub fn into_vec(self) -> Vec<bool> {
        self.selected
    }
}

/// Owns a shared, immutable precedence model and one economic value per block.
/// Updating values retains the model but resets the flow forest, as upstream
/// MineFlow does. Failed or cancelled solves expose no partial result.
///
/// Intermediate excess and flow use i128. With at most i64::MAX blocks and
/// i64 values, the sum of absolute values fits i128 without saturation.
pub struct Solver {
    precedence: Precedence,
    values: Vec<i64>,
    forest: Option<Forest>,
    pit: Option<Pit>,
    largest: Option<Pit>,
}
impl std::fmt::Debug for Solver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Solver")
            .field("num_blocks", &self.values.len())
            .field("solved", &self.pit.is_some())
            .finish()
    }
}
impl Solver {
    pub fn new(precedence: &Precedence, values: &[i64]) -> Result<Self> {
        if usize::try_from(precedence.num_blocks()?).ok() != Some(values.len()) {
            return Err(invalid("one value is required per block"));
        }
        Ok(Self {
            precedence: precedence.clone(),
            values: values.to_vec(),
            forest: None,
            pit: None,
            largest: None,
        })
    }
    pub fn from_source<S: BlockValueSource>(precedence: &Precedence, source: S) -> Result<Self> {
        if source.num_blocks() != precedence.num_blocks()? {
            return Err(invalid("value source has a different block count"));
        }
        let values: Vec<_> = (0..source.num_blocks()).map(|i| source.value(i)).collect();
        Self::new(precedence, &values)
    }
    pub fn num_blocks(&self) -> i64 {
        self.values.len() as i64
    }
    pub fn update_values(&mut self, values: &[i64]) -> Result<()> {
        if values.len() != self.values.len() {
            return Err(invalid("one value is required per block"));
        }
        self.values.copy_from_slice(values);
        self.invalidate();
        Ok(())
    }
    pub fn update_values_from_source<S: BlockValueSource>(&mut self, source: S) -> Result<()> {
        if source.num_blocks() != self.num_blocks() {
            return Err(invalid("value source has a different block count"));
        }
        let values: Vec<_> = (0..source.num_blocks()).map(|i| source.value(i)).collect();
        self.update_values(&values)
    }
    fn invalidate(&mut self) {
        self.forest = None;
        self.pit = None;
        self.largest = None;
    }
    fn info(&self, pit: &Pit, elapsed: Duration) -> SolveInfo {
        let value: i128 = pit.blocks().map(|i| i128::from(self.values[i as usize])).sum();
        SolveInfo {
            elapsed,
            num_blocks: self.num_blocks(),
            num_blocks_in_pit: pit.num_selected() as i64,
            num_precedence_constraints_used: self.forest.as_ref().map_or(0, |f| f.used_arcs),
            pit_value: value,
            pit_value_exact: value.to_string(),
        }
    }
    pub fn solve(&mut self) -> Result<SolveInfo> {
        self.solve_with_cancel(|| false)
    }
    /// Return true from `cancelled` to abort. The callback must be inexpensive.
    /// Providers run synchronously; cancellation cannot interrupt a provider call.
    pub fn solve_with_cancel(&mut self, mut cancelled: impl FnMut() -> bool) -> Result<SolveInfo> {
        let start = Instant::now();
        self.largest = None;
        if cancelled() {
            self.invalidate();
            return Err(Error::Cancelled);
        }
        if self.pit.is_none() {
            let mut forest = match Forest::new(self.values.iter().map(|&v| i128::from(v))) {
                Ok(forest) => forest,
                Err(error) => {
                    self.invalidate();
                    return Err(error);
                }
            };
            match forest.solve(&self.precedence, &mut cancelled) {
                Ok(selected) => {
                    self.pit = Some(Pit::new(selected));
                    self.forest = Some(forest);
                }
                Err(error) => {
                    self.invalidate();
                    return Err(error);
                }
            }
        } else if cancelled() {
            self.invalidate();
            return Err(Error::Cancelled);
        }
        Ok(self.info(self.pit.as_ref().expect("successful solve"), start.elapsed()))
    }
    pub fn solve_largest(&mut self) -> Result<SolveInfo> {
        self.solve_largest_with_cancel(|| false)
    }
    pub fn solve_largest_with_cancel(&mut self, mut cancelled: impl FnMut() -> bool) -> Result<SolveInfo> {
        let start = Instant::now();
        self.solve_with_cancel(&mut cancelled)?;
        let result = self.forest.as_ref().expect("successful solve").largest(&self.precedence, &mut cancelled);
        match result {
            Ok(selected) => {
                self.largest = Some(Pit::new(selected));
            }
            Err(error) => {
                self.invalidate();
                return Err(error);
            }
        }
        Ok(self.info(self.largest.as_ref().expect("successful solve"), start.elapsed()))
    }
    /// Smallest optimal pit, also available after solving for the largest pit.
    pub fn pit(&self) -> Result<Pit> {
        self.pit.clone().ok_or(Error::NotSolved("solve"))
    }
    pub fn largest_pit(&self) -> Result<Pit> {
        self.largest.clone().ok_or(Error::NotSolved("solve_largest"))
    }
}
