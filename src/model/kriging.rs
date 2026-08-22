//! Ordinary kriging of drill-hole interval samples onto a regular block grid.

use std::collections::HashMap;

use anyhow::{Context, Result};
use glam::DVec3;

use crate::{app::jobs::CancelFlag, model::progress::Phase};

#[derive(Clone, Copy, Debug)]
pub(crate) struct KrigingSample {
    pub(crate) position: DVec3,
    pub(crate) value: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct KrigingGrid {
    pub(crate) lower: DVec3,
    pub(crate) upper: DVec3,
    pub(crate) cell: DVec3,
}

impl KrigingGrid {
    pub(crate) fn dimensions(self) -> Result<[usize; 3]> {
        if !self.lower.is_finite() || !self.upper.is_finite() || !self.cell.is_finite() || self.lower.cmpge(self.upper).any() || self.cell.min_element() <= 0.0 {
            anyhow::bail!("Block-model bounds and cell sizes must be finite and positive");
        }
        let spans = ((self.upper - self.lower) / self.cell).ceil().to_array();
        let mut dims = [0usize; 3];
        for axis in 0..3 {
            if spans[axis] < 1.0 || spans[axis] > usize::MAX as f64 {
                anyhow::bail!("Block-grid dimension is outside the supported range");
            }
            dims[axis] = spans[axis] as usize;
        }
        dims.into_iter()
            .try_fold(1usize, |count, dim| count.checked_mul(dim))
            .context("Block-grid cell count overflows")?;
        Ok(dims)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OrdinaryKrigingOptions {
    /// Spherical variogram range and neighbour search radius, in world units.
    pub(crate) range: f64,
    /// Partial sill. The covariance at zero is `sill + nugget`.
    pub(crate) sill: f64,
    pub(crate) nugget: f64,
    pub(crate) min_samples: usize,
    pub(crate) max_samples: usize,
}

#[derive(Debug)]
pub(crate) struct KrigedGrid {
    pub(crate) dims: [usize; 3],
    pub(crate) upper: DVec3,
    pub(crate) estimates: Vec<f64>,
}

pub(crate) fn ordinary_kriging(samples: &[KrigingSample], grid: KrigingGrid, options: OrdinaryKrigingOptions, cancel: &CancelFlag, progress: &Phase) -> Result<KrigedGrid> {
    let dims = grid.dimensions()?;
    let count = dims
        .into_iter()
        .try_fold(1usize, |count, dim| count.checked_mul(dim))
        .context("Block-grid cell count overflows")?;
    if samples.is_empty() || samples.iter().any(|sample| !sample.position.is_finite() || !sample.value.is_finite()) {
        anyhow::bail!("Ordinary kriging requires at least one finite sample");
    }
    if !options.range.is_finite() || options.range <= 0.0 || !options.sill.is_finite() || options.sill <= 0.0 || !options.nugget.is_finite() || options.nugget < 0.0 {
        anyhow::bail!("Variogram range and sill must be positive; nugget must be non-negative");
    }
    if options.min_samples == 0 || options.max_samples < options.min_samples || options.max_samples > 64 {
        anyhow::bail!("Neighbour counts must satisfy 1 <= minimum <= maximum <= 64");
    }

    let bins = SampleBins::new(samples, options.range);
    let mut estimates = allocated_values(count, f64::NAN, "kriging estimates")?;
    let total_covariance = options.sill + options.nugget;

    for (index, slot) in estimates.iter_mut().enumerate() {
        if index.is_multiple_of(256) {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            progress.set_items(index as u64, count as u64);
        }
        let x = index % dims[0];
        let yz = index / dims[0];
        let y = yz % dims[1];
        let z = yz / dims[1];
        let center = grid.lower + grid.cell * DVec3::new(x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5);
        let neighbours = bins.nearest(center, samples, options.range, options.max_samples);
        if neighbours.len() < options.min_samples {
            continue;
        }
        if let Some(estimate) = estimate_at(center, samples, &neighbours, options, total_covariance) {
            *slot = estimate;
        }
    }
    progress.finish();
    Ok(KrigedGrid {
        dims,
        upper: grid.lower + grid.cell * DVec3::from_array(dims.map(|dim| dim as f64)),
        estimates,
    })
}

fn allocated_values(count: usize, initial: f64, description: &str) -> Result<Vec<f64>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .with_context(|| format!("Not enough memory to allocate {count} {description}"))?;
    values.resize(count, initial);
    Ok(values)
}

struct SampleBins {
    cell: f64,
    bins: HashMap<[i64; 3], Vec<usize>>,
}

impl SampleBins {
    fn new(samples: &[KrigingSample], cell: f64) -> Self {
        let mut bins: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
        for (index, sample) in samples.iter().enumerate() {
            bins.entry(bin_key(sample.position, cell)).or_default().push(index);
        }
        Self { cell, bins }
    }

    fn nearest(&self, target: DVec3, samples: &[KrigingSample], radius: f64, maximum: usize) -> Vec<usize> {
        let base = bin_key(target, self.cell);
        let radius_squared = radius * radius;
        let mut found = Vec::new();
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let key = [base[0].saturating_add(dx), base[1].saturating_add(dy), base[2].saturating_add(dz)];
                    if let Some(indices) = self.bins.get(&key) {
                        found.extend(indices.iter().copied().filter_map(|index| {
                            let distance_squared = samples[index].position.distance_squared(target);
                            (distance_squared <= radius_squared).then_some((index, distance_squared))
                        }));
                    }
                }
            }
        }
        found.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        found.truncate(maximum);
        found.into_iter().map(|(index, _)| index).collect()
    }
}

fn bin_key(position: DVec3, cell: f64) -> [i64; 3] {
    (position / cell).floor().to_array().map(|value| value.clamp(i64::MIN as f64, i64::MAX as f64) as i64)
}

fn spherical_covariance(distance: f64, options: OrdinaryKrigingOptions) -> f64 {
    if distance <= f64::EPSILON {
        return options.sill + options.nugget;
    }
    if distance >= options.range {
        return 0.0;
    }
    let ratio = distance / options.range;
    options.sill * (1.0 - 1.5 * ratio + 0.5 * ratio * ratio * ratio)
}

fn estimate_at(target: DVec3, samples: &[KrigingSample], neighbours: &[usize], options: OrdinaryKrigingOptions, total_covariance: f64) -> Option<f64> {
    let n = neighbours.len();
    let width = n + 2;
    let mut augmented = vec![0.0; (n + 1) * width];
    let jitter = total_covariance.max(1.0) * 1.0e-10;
    for row in 0..n {
        for column in 0..n {
            let distance = samples[neighbours[row]].position.distance(samples[neighbours[column]].position);
            augmented[row * width + column] = spherical_covariance(distance, options) + if row == column { jitter } else { 0.0 };
        }
        augmented[row * width + n] = 1.0;
        augmented[row * width + n + 1] = spherical_covariance(samples[neighbours[row]].position.distance(target), options);
    }
    for column in 0..n {
        augmented[n * width + column] = 1.0;
    }
    augmented[n * width + n] = 0.0;
    augmented[n * width + n + 1] = 1.0;
    gaussian_solve(&mut augmented, n + 1, width)?;

    let mut estimate = 0.0;
    for row in 0..n {
        let weight = augmented[row * width + n + 1];
        estimate += weight * samples[neighbours[row]].value;
    }
    estimate.is_finite().then_some(estimate)
}

fn gaussian_solve(matrix: &mut [f64], size: usize, width: usize) -> Option<()> {
    for pivot_column in 0..size {
        let pivot_row = (pivot_column..size).max_by(|&a, &b| matrix[a * width + pivot_column].abs().total_cmp(&matrix[b * width + pivot_column].abs()))?;
        let pivot = matrix[pivot_row * width + pivot_column];
        if !pivot.is_finite() || pivot.abs() < 1.0e-14 {
            return None;
        }
        if pivot_row != pivot_column {
            for column in pivot_column..width {
                matrix.swap(pivot_row * width + column, pivot_column * width + column);
            }
        }
        for column in pivot_column..width {
            matrix[pivot_column * width + column] /= pivot;
        }
        for row in 0..size {
            if row == pivot_column {
                continue;
            }
            let factor = matrix[row * width + pivot_column];
            for column in pivot_column..width {
                matrix[row * width + column] -= factor * matrix[pivot_column * width + column];
            }
        }
    }
    Some(())
}
