//! MineFlow's slope search templates and their geometric accuracy.
use std::collections::HashSet;

use crate::{
    Angle, BlockModel, Slope,
    error::{Result, invalid},
};

pub(crate) type Offset = (i64, i64, i64);

/// Counts and measures use MineFlow's original naming: false positives are
/// geometric cone blocks missed by the template; false negatives are extras.
/// Rates with an empty denominator are reported as zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatternAccuracy {
    pub true_positive: i64,
    pub true_negative: i64,
    pub false_positive: i64,
    pub false_negative: i64,
    pub accuracy: f64,
    pub true_positive_rate: f64,
    pub false_negative_rate: f64,
    pub matthews_correlation: f64,
}
impl PatternAccuracy {
    fn measures(&mut self) {
        let (tp, tn, fp, fn_) = (self.true_positive as f64, self.true_negative as f64, self.false_positive as f64, self.false_negative as f64);
        let ratio = |a, b| if b == 0.0 { 0.0 } else { a / b };
        self.accuracy = ratio(tp + tn, tp + tn + fp + fn_);
        self.true_positive_rate = ratio(tp, tp + fp);
        self.false_negative_rate = ratio(fn_, tp + fp);
        self.matthews_correlation = ratio(tp * tn - fp * fn_, ((tp + fp) * (tp + fn_) * (tn + fp) * (tn + fn_)).sqrt());
    }
    fn count(&mut self, flag: u8) {
        match flag {
            0 => self.true_negative += 1,
            1 => self.false_positive += 1,
            2 => self.true_positive += 1,
            _ => self.false_negative += 1,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Pattern {
    pub(crate) offsets: Vec<Offset>,
}
impl Pattern {
    pub fn from_offsets(offsets: &[Offset]) -> Result<Self> {
        if offsets.iter().any(|&(x, y, z)| z <= 0 || x == i64::MIN || y == i64::MIN) {
            return Err(invalid("offsets must point to a higher bench and be reversible"));
        }
        let mut offsets = offsets.to_vec();
        offsets.sort_by_key(|&(x, y, z)| (z, x, y));
        offsets.dedup();
        Ok(Self { offsets })
    }
    pub fn one_five() -> Result<Self> {
        Ok(Self {
            offsets: vec![(0, -1, 1), (-1, 0, 1), (0, 0, 1), (1, 0, 1), (0, 1, 1)],
        })
    }
    pub fn one_nine() -> Result<Self> {
        Ok(Self {
            offsets: (-1..=1).flat_map(|y| (-1..=1).map(move |x| (x, y, 1))).collect(),
        })
    }
    pub fn knights_move() -> Result<Self> {
        let mut p = Self::one_five()?;
        p.offsets
            .extend([(-1, -2, 2), (1, -2, 2), (-2, -1, 2), (2, -1, 2), (-2, 1, 2), (2, 1, 2), (-1, 2, 2), (1, 2, 2)]);
        Ok(p)
    }
    pub fn len(&self) -> Result<usize> {
        Ok(self.offsets.len())
    }
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.offsets.is_empty())
    }
    pub fn offsets(&self) -> Result<Vec<Offset>> {
        Ok(self.offsets.clone())
    }
    fn extent(model: &BlockModel, slope: &Slope, z: i64) -> Result<(i64, i64)> {
        let throw = model.block_size[2] * z as f64 / slope.min()?.as_radians().tan();
        let x = (throw / model.block_size[0]).ceil();
        let y = (throw / model.block_size[1]).ceil();
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 || x >= (i64::MAX / 2) as f64 || y >= (i64::MAX / 2) as f64 {
            return Err(invalid("slope search extent exceeds index range"));
        }
        Ok((x as i64, y as i64))
    }
    fn validate(model: &BlockModel, benches: i64) -> Result<()> {
        model.validate()?;
        if benches <= 0 || benches == i64::MAX {
            return Err(invalid("bench count must be positive and addressable"));
        }
        Ok(())
    }
    pub fn naive(model: &BlockModel, slope: &Slope, benches: i64) -> Result<Self> {
        Self::validate(model, benches)?;
        let mut offsets = Vec::new();
        for z in 1..=benches {
            let (mx, my) = Self::extent(model, slope, z)?;
            for x in -mx..=mx {
                for y in -my..=my {
                    if slope.contains_offset(x as f64 * model.block_size[0], y as f64 * model.block_size[1], z as f64 * model.block_size[2])? {
                        offsets.push((x, y, z));
                    }
                }
            }
        }
        Ok(Self { offsets })
    }
    pub fn less_naive(model: &BlockModel, slope: &Slope, benches: i64) -> Result<Self> {
        let mut p = Self::naive(model, slope, benches)?;
        let mut seen = HashSet::new();
        p.offsets.retain(|&(x, y, _)| seen.insert((x, y)));
        Ok(p)
    }
    pub fn min_search_constant(slope: Angle, benches: i64) -> Result<Self> {
        Self::min_search(&BlockModel::new(1, 1, 1), &Slope::constant(slope)?, benches)
    }
    pub fn min_search(model: &BlockModel, slope: &Slope, benches: i64) -> Result<Self> {
        Self::validate(model, benches)?;
        let (cx, cy) = Self::extent(model, slope, benches)?;
        let grid = BlockModel::new(cx * 2 + 1, cy * 2 + 1, benches + 1);
        let total = grid.checked_num_blocks()?;
        let mut flags = Vec::new();
        flags.try_reserve_exact(total).map_err(|_| invalid("slope search exceeds memory"))?;
        flags.resize(total, 0u8);
        let origin = grid.grid_index(cx, cy, 0);
        flags[origin as usize] = 1;
        let mut flagged = vec![origin];
        let mut offsets: Vec<Offset> = Vec::new();
        for z in 1..=benches {
            let (mx, my) = Self::extent(model, slope, z)?;
            let first_new = offsets.len();
            for x in -mx..=mx {
                for y in -my..=my {
                    let i = grid.grid_index(cx + x, cy + y, z) as usize;
                    if flags[i] == 0 && slope.contains_offset(x as f64 * model.block_size[0], y as f64 * model.block_size[1], z as f64 * model.block_size[2])? {
                        flags[i] = 1;
                        flagged.push(i as i64);
                        offsets.push((x, y, z));
                    }
                }
            }
            let mut extra = Vec::new();
            for &i in &flagged {
                let start = if flags[i as usize] == 1 {
                    flags[i as usize] = 2;
                    0
                } else {
                    first_new
                };
                for &offset in &offsets[start..] {
                    if grid.xyz(i).2 + offset.2 >= grid.num_z {
                        break;
                    }
                    if let Some(j) = grid.offset(i, offset)
                        && flags[j as usize] == 0
                    {
                        flags[j as usize] = 1;
                        extra.push(j);
                    }
                }
            }
            flagged.extend(extra);
        }
        Ok(Self { offsets })
    }
    fn accuracy_flags(&self, model: &BlockModel, slope: &Slope) -> Result<Vec<u8>> {
        let total = model.checked_num_blocks()?;
        let mut flags = Vec::new();
        flags.try_reserve_exact(total).map_err(|_| invalid("accuracy grid exceeds memory"))?;
        flags.resize(total, 0);
        let origin = model.grid_index(model.num_x / 2, model.num_y / 2, 0);
        for offset in Self::naive(model, slope, model.num_z)?.offsets {
            if let Some(i) = model.offset(origin, offset) {
                flags[i as usize] = 1;
            }
        }
        let mut stack = vec![origin];
        while let Some(i) = stack.pop() {
            for &offset in &self.offsets {
                if let Some(j) = model.offset(i, offset) {
                    let f = &mut flags[j as usize];
                    if *f > 1 {
                        continue;
                    }
                    *f = if *f == 1 { 2 } else { 3 };
                    stack.push(j);
                }
            }
        }
        Ok(flags)
    }
    pub fn accuracy(&self, model: &BlockModel, slope: &Slope) -> Result<PatternAccuracy> {
        let mut a = PatternAccuracy::default();
        for f in self.accuracy_flags(model, slope)? {
            a.count(f);
        }
        a.measures();
        Ok(a)
    }
    /// Cumulative accuracy through each bench; the origin counts as a true positive.
    pub fn accuracy_by_bench(&self, model: &BlockModel, slope: &Slope) -> Result<Vec<PatternAccuracy>> {
        let flags = self.accuracy_flags(model, slope)?;
        let layer = (model.num_x * model.num_y) as usize;
        let mut a = PatternAccuracy {
            true_positive: 1,
            true_negative: layer as i64 - 1,
            ..Default::default()
        };
        a.measures();
        let mut result = vec![a.clone()];
        for slice in flags[layer..].chunks(layer) {
            for &f in slice {
                a.count(f);
            }
            a.measures();
            result.push(a.clone());
        }
        Ok(result)
    }
}
