//! Converting economic block values into the integers the solver works in.

use crate::error::{Error, Result, invalid};

/// The solver accumulates block values, so leave headroom below i64::MAX for
/// the running total rather than only bounding individual blocks.
const TOTAL_LIMIT: f64 = 4.6e18;

/// A fixed point scale for economic block values.
///
/// The pseudoflow solver works in exact integers, but block values are dollars
/// and come as floats. A scale converts between the two: pick one for a value
/// set, use it for every solve on that set, and use it to read pit values back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueScale {
    factor: f64,
}

impl ValueScale {
    /// Values are already integers; no conversion is applied.
    pub const IDENTITY: ValueScale = ValueScale { factor: 1.0 };

    /// A scale that multiplies by `factor` before rounding.
    ///
    /// A factor of 100 keeps cents, 1000 keeps tenths of a cent, and so on.
    pub fn new(factor: f64) -> Result<Self> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(invalid("the scale factor must be finite and positive"));
        }
        Ok(ValueScale { factor })
    }

    /// The largest power of ten that keeps the total of `values` well inside
    /// the integer range, capped at a billion.
    ///
    /// This keeps as much precision as the value set allows without risking an
    /// overflow part way through a solve.
    pub fn auto(values: &[f64]) -> Result<Self> {
        let total: f64 = values.iter().map(|value| value.abs()).sum();
        if !total.is_finite() {
            return Err(Error::Overflow(String::from("block values must all be finite")));
        }
        if total == 0.0 {
            return Ok(ValueScale::IDENTITY);
        }

        let largest = TOTAL_LIMIT / total;
        let mut factor = 1.0f64;
        while factor * 10.0 <= largest && factor < 1.0e9 {
            factor *= 10.0;
        }
        // A value set too large even at a factor of one has to lose precision.
        while factor > largest && factor > f64::MIN_POSITIVE {
            factor /= 10.0;
        }

        ValueScale::new(factor)
    }

    pub fn factor(self) -> f64 {
        self.factor
    }

    /// Converts one float value into solver units.
    pub fn encode_one(self, value: f64) -> Result<i64> {
        if !value.is_finite() {
            return Err(Error::Overflow(format!("block value {value} is not finite")));
        }

        let scaled = (value * self.factor).round();
        if scaled < i64::MIN as f64 || scaled >= -(i64::MIN as f64) {
            return Err(Error::Overflow(format!("block value {value} does not fit a 64 bit integer at a scale of {}", self.factor)));
        }
        Ok(scaled as i64)
    }

    /// Converts float block values into solver units.
    pub fn encode(self, values: &[f64]) -> Result<Vec<i64>> {
        values.iter().map(|value| self.encode_one(*value)).collect()
    }

    /// Converts a solver value, such as a pit value, back into dollars.
    pub fn decode(self, scaled: i64) -> f64 {
        scaled as f64 / self.factor
    }
}

impl Default for ValueScale {
    fn default() -> Self {
        ValueScale::IDENTITY
    }
}
