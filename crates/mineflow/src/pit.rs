//! The whole pipeline in one place: model, slopes, values, pit.

use crate::{
    error::{Result, invalid},
    geometry::{Angle, BlockModel},
    pattern::Pattern,
    precedence::Precedence,
    slope::Slope,
    solver::{Pit, SolveInfo, Solver},
    values::ValueScale,
};

/// The default bench count for a generated pattern, matching the MATLAB
/// wrapper. More benches model the slope further from each block at the cost
/// of a larger pattern. The default is twelve, capped by the model height.
const DEFAULT_MAX_BENCHES: i64 = 12;

/// The default grouping applied to per block slope angles.
const DEFAULT_SLOPE_TOLERANCE_DEGREES: f64 = 0.1;

enum SlopeSpec {
    Unset,
    Constant(Angle),
    Uniform(Slope),
    PerBlock(Vec<Angle>),
    Prebuilt(Pattern),
}

/// A solved ultimate pit.
#[derive(Clone, Debug)]
pub struct Solution {
    pub pit: Pit,
    pub info: SolveInfo,
    /// The scale the block values were converted with.
    pub scale: ValueScale,
}

impl Solution {
    /// The pit value in the units the block values were given in.
    pub fn pit_value(&self) -> f64 {
        self.info.pit_value_with(self.scale)
    }
}

/// Builds an ultimate pit from a block model, a slope and block values.
///
/// ```no_run
/// use mineflow::{BlockModel, UltimatePit, degrees};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let model = BlockModel::new(170, 215, 50).with_block_size(20.0, 20.0, 15.0);
/// let values: Vec<f64> = vec![0.0; model.num_blocks() as usize];
///
/// let solution = UltimatePit::new(model)
///     .constant_slope(degrees(45.0))
///     .solve_f64(&values)?;
///
/// println!(
///     "{} blocks worth {}",
///     solution.pit.num_selected(),
///     solution.pit_value()
/// );
/// # Ok(())
/// # }
/// ```
pub struct UltimatePit {
    model: BlockModel,
    slope: SlopeSpec,
    benches: Option<i64>,
    tolerance: Angle,
    largest: bool,
}

impl UltimatePit {
    pub fn new(model: BlockModel) -> Self {
        UltimatePit {
            model,
            slope: SlopeSpec::Unset,
            benches: None,
            tolerance: Angle::from_degrees(DEFAULT_SLOPE_TOLERANCE_DEGREES),
            largest: false,
        }
    }

    /// One slope angle for the whole model, the same in every direction.
    pub fn constant_slope(mut self, slope: Angle) -> Self {
        self.slope = SlopeSpec::Constant(slope);
        self
    }

    /// A slope that varies by azimuth but not by location.
    pub fn slope(mut self, slope: Slope) -> Self {
        self.slope = SlopeSpec::Uniform(slope);
        self
    }

    /// One slope angle per block, in the model's 1D block order.
    pub fn slopes_per_block(mut self, slopes: Vec<Angle>) -> Self {
        self.slope = SlopeSpec::PerBlock(slopes);
        self
    }

    /// A pattern you built yourself, applied to every block.
    pub fn pattern(mut self, pattern: Pattern) -> Self {
        self.slope = SlopeSpec::Prebuilt(pattern);
        self
    }

    /// How many benches up the generated pattern reaches.
    pub fn benches(mut self, benches: i64) -> Self {
        self.benches = Some(benches);
        self
    }

    /// How closely per block slope angles must match to share a pattern.
    pub fn slope_tolerance(mut self, tolerance: Angle) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Return the largest pit among those with the optimal value.
    pub fn largest(mut self, largest: bool) -> Self {
        self.largest = largest;
        self
    }

    fn bench_count(&self) -> i64 {
        self.benches.unwrap_or_else(|| self.model.num_z.clamp(1, DEFAULT_MAX_BENCHES))
    }

    /// Builds the precedence graph without solving, so it can be reused.
    pub fn precedence(&self) -> Result<Precedence> {
        self.model.validate()?;
        let benches = self.bench_count();

        match &self.slope {
            SlopeSpec::Unset => Err(invalid("a slope is required: call constant_slope, slope, slopes_per_block or pattern")),
            SlopeSpec::Constant(angle) => {
                let slope = Slope::constant(*angle)?;
                let pattern = Pattern::min_search(&self.model, &slope, benches)?;
                Precedence::regular_3d(&self.model, &pattern)
            }
            SlopeSpec::Uniform(slope) => {
                let pattern = Pattern::min_search(&self.model, slope, benches)?;
                Precedence::regular_3d(&self.model, &pattern)
            }
            SlopeSpec::PerBlock(slopes) => Precedence::regular_3d_varying_slopes(&self.model, slopes, benches, self.tolerance),
            SlopeSpec::Prebuilt(pattern) => Precedence::regular_3d(&self.model, pattern),
        }
    }

    /// Builds a solver, so several value sets can share one graph.
    pub fn solver(&self, values: &[i64]) -> Result<Solver> {
        Solver::new(&self.precedence()?, values)
    }

    /// Solves with values already in integer units.
    pub fn solve(&self, values: &[i64]) -> Result<Solution> {
        self.solve_with_scale(values, ValueScale::IDENTITY)
    }

    /// Solves with economic block values in dollars, choosing a scale that
    /// keeps as much precision as the values allow.
    pub fn solve_f64(&self, values: &[f64]) -> Result<Solution> {
        let scale = ValueScale::auto(values)?;
        self.solve_with_scale(&scale.encode(values)?, scale)
    }

    fn solve_with_scale(&self, values: &[i64], scale: ValueScale) -> Result<Solution> {
        let mut solver = self.solver(values)?;

        let (info, pit) = if self.largest {
            let info = solver.solve_largest()?;
            (info, solver.largest_pit()?)
        } else {
            let info = solver.solve()?;
            (info, solver.pit()?)
        };

        Ok(Solution { pit, info, scale })
    }
}
