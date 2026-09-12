//! Pure Rust port of [MineFlow](https://github.com/MineFlowCSM/MineFlow).
//!
//! Maximizes the sum of economic block values subject to mining precedence.
//! An edge `(a, b)` means that mining block `a` requires mining block `b`.
//! Uses MineFlow's highest-label pseudoflow algorithm, with lazy precedence
//! generation for regular grids and exact i128 intermediate arithmetic.
//!
//! ```
//! let pit = mineflow::solve(&[7, 2, -2, -2, -2], &[(0, 2), (0, 3), (1, 3), (1, 4)])?;
//! assert_eq!(pit, [true, false, true, true, false]);
//! # Ok::<(), mineflow::Error>(())
//! ```
//!
//! [`UltimatePit`] combines a [`BlockModel`], [`Slope`], [`Pattern`] and
//! [`Solver`]. Economic values are inputs: costs, prices and cutoffs belong
//! to the caller. [`ValueScale`] explicitly quantizes floating point values.
#![forbid(unsafe_code)]

mod error;
mod geometry;
mod pattern;
mod pit;
mod precedence;
mod pseudoflow;
mod slope;
mod solver;
mod source;
mod values;

pub use error::{Error, Result};
pub use geometry::{Angle, BlockModel, degrees, radians};
pub use pattern::{Pattern, PatternAccuracy};
pub use pit::{Solution, UltimatePit};
pub use precedence::{Precedence, SearchBuffer};
pub use slope::Slope;
pub use solver::{Pit, SolveInfo, Solver};
pub use source::{BlockValueSource, PrecedenceSource};
pub use values::ValueScale;

/// Solve an explicit graph; regular models should use implicit precedence.
pub fn solve(values: &[i64], edges: &[(i64, i64)]) -> Result<Vec<bool>> {
    let n = i64::try_from(values.len()).map_err(|_| error::invalid("too many blocks"))?;
    let precedence = Precedence::explicit(n, edges)?;
    let mut solver = Solver::new(&precedence, values)?;
    solver.solve()?;
    Ok(solver.pit()?.into_vec())
}
