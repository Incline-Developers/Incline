//! Circular interpolation of slopes measured clockwise from north.
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use crate::{
    Angle,
    error::{Result, invalid},
    radians,
};

#[derive(Clone, Debug)]
pub struct Slope {
    pairs: Vec<(f64, f64)>,
}
impl Slope {
    pub fn constant(slope: Angle) -> Result<Self> {
        Self::from_pairs(&[(radians(0.0), slope)])
    }
    pub fn from_pairs(pairs: &[(Angle, Angle)]) -> Result<Self> {
        if pairs.is_empty() {
            return Err(invalid("at least one slope is required"));
        }
        let mut result = Vec::with_capacity(pairs.len());
        for &(azimuth, slope) in pairs {
            Self::validate_slope(slope)?;
            if !azimuth.as_radians().is_finite() {
                return Err(invalid("azimuth must be finite"));
            }
            result.push((azimuth.as_radians().rem_euclid(TAU), slope.as_radians()));
        }
        result.sort_by(|a, b| a.0.total_cmp(&b.0));
        if result.windows(2).any(|p| p[0].0 == p[1].0) {
            return Err(invalid("azimuths must be distinct modulo 360 degrees"));
        }
        Ok(Self { pairs: result })
    }
    pub fn from_degree_pairs(pairs: &[(f64, f64)]) -> Result<Self> {
        Self::from_pairs(&pairs.iter().map(|&(a, s)| (crate::degrees(a), crate::degrees(s))).collect::<Vec<_>>())
    }
    pub(crate) fn validate_slope(slope: Angle) -> Result<()> {
        let s = slope.as_radians();
        if !s.is_finite() || s <= 0.0 || s > FRAC_PI_2 {
            return Err(invalid("slope must be in (0, 90] degrees"));
        }
        Ok(())
    }
    fn bracket(&self, a: f64) -> (usize, usize, f64) {
        let a = a.rem_euclid(TAU);
        let r = self.pairs.partition_point(|p| p.0 < a) % self.pairs.len();
        let l = (r + self.pairs.len() - 1) % self.pairs.len();
        let left = (a - self.pairs[l].0).rem_euclid(TAU);
        let right = (self.pairs[r].0 - a).rem_euclid(TAU);
        (l, r, if left + right == 0.0 { 0.0 } else { left / (left + right) })
    }
    pub fn at(&self, azimuth: Angle) -> Result<Angle> {
        if !azimuth.as_radians().is_finite() {
            return Err(invalid("azimuth must be finite"));
        }
        if self.pairs.len() == 1 {
            return Ok(radians(self.pairs[0].1));
        }
        let (l, r, t) = self.bracket(azimuth.as_radians());
        Ok(radians(self.pairs[l].1 + (self.pairs[r].1 - self.pairs[l].1) * t))
    }
    pub fn min(&self) -> Result<Angle> {
        Ok(radians(self.pairs.iter().map(|p| p.1).fold(FRAC_PI_2, f64::min)))
    }
    pub fn contains_offset(&self, dx: f64, dy: f64, dz: f64) -> Result<bool> {
        if [dx, dy, dz].iter().any(|v| !v.is_finite()) {
            return Err(invalid("offset must be finite"));
        }
        if dx == 0.0 && dy == 0.0 {
            return Ok(true);
        }
        let theta = (dz.abs() / (dx * dx + dy * dy).sqrt()).atan();
        Ok(theta >= self.at(radians(FRAC_PI_2 - dy.atan2(dx)))?.as_radians())
    }
    pub fn pairs(&self) -> Result<Vec<(Angle, Angle)>> {
        Ok(self.pairs.iter().map(|&(a, s)| (radians(a), radians(s))).collect())
    }
    pub fn cubic_interpolated(&self, count: usize) -> Result<Self> {
        self.interpolated(count, true)
    }
    pub fn cosine_interpolated(&self, count: usize) -> Result<Self> {
        self.interpolated(count, false)
    }
    fn interpolated(&self, count: usize, cubic: bool) -> Result<Self> {
        let n = self.pairs.len();
        if count == 0 || n < if cubic { 4 } else { 2 } {
            return Err(invalid("insufficient slope pairs or zero interpolation count"));
        }
        let mut pairs = Vec::new();
        pairs.try_reserve_exact(count).map_err(|_| invalid("interpolation count exceeds memory"))?;
        for i in 0..count {
            let a = TAU * i as f64 / count as f64;
            let (l, r, t) = self.bracket(a);
            let y1 = self.pairs[l].1;
            let y2 = self.pairs[r].1;
            let s = if cubic {
                let y0 = self.pairs[(l + n - 1) % n].1;
                let y3 = self.pairs[(r + 1) % n].1;
                let a0 = y3 - y2 - y0 + y1;
                let a1 = y0 - y1 - a0;
                a0 * t * t * t + a1 * t * t + (y2 - y0) * t + y1
            } else {
                let mu = (1.0 - (t * PI).cos()) / 2.0;
                y1 * (1.0 - mu) + y2 * mu
            };
            pairs.push((radians(a), radians(s)));
        }
        Self::from_pairs(&pairs)
    }
}
