//! Evaluates the block model colour ramp and opacity transfers for CPU caching and test reference.

use crate::model::block_model::{ColorTransferFunction, MAX_COLOR_STOPS};

/// Mirrors `VISIBLE_ALPHA_EPSILON` in `block_model.wgsl`: graded fragments
/// whose ramp alpha falls below this are discarded.
pub(crate) const VISIBLE_ALPHA_EPSILON: f32 = 0.004;

const HIDDEN_GRADE_DISCARD_THRESHOLD: f32 = -1.5;
// Opaque stops must occlude on the first occupied cell instead of having
// their opacity distributed over the whole model chord. This dimensionless
// optical depth is large enough to saturate even a very thin cell segment;
// mirrored by `OPAQUE_OPTICAL_DEPTH` in `block_model_volume.wgsl`.
const OPAQUE_OPTICAL_DEPTH: f32 = 1.0e6;

pub(crate) fn is_hidden_block_grade(grade: f32) -> bool {
    grade < HIDDEN_GRADE_DISCARD_THRESHOLD
}

pub(crate) fn is_hidden_block_appearance(grade: f32, has_grade: bool, color_transfer: &ColorTransferFunction) -> bool {
    is_hidden_block_grade(grade) || (has_grade && grade >= 0.0 && ramp_alpha(color_transfer, grade) < VISIBLE_ALPHA_EPSILON)
}

/// CPU replica of `ramp_color` in the block-model WGSL shaders. The first
/// stop is a lower cutoff: values below it are transparent. Each stop then
/// starts at its own position and remains active until the next.
pub(crate) fn ramp_rgba(color_transfer: &ColorTransferFunction, t: f32) -> [f32; 4] {
    let stops = &color_transfer.stops[..color_transfer.stops.len().min(MAX_COLOR_STOPS)];
    let [first, .., last] = stops else {
        return [0.0; 4];
    };
    if t < first.t {
        return [0.0; 4];
    }
    if t >= last.t {
        return last.color;
    }
    for pair in stops.windows(2) {
        if t < pair[1].t {
            return pair[0].color;
        }
    }
    last.color
}

pub(crate) fn ramp_alpha(color_transfer: &ColorTransferFunction, t: f32) -> f32 {
    ramp_rgba(color_transfer, t)[3]
}

/// CPU replica of `optical_depth_for_alpha` in
/// `block_model_volume.wgsl`. The volume shader distributes this optical
/// depth over the ray's complete chord through the model, so a homogeneous
/// model renders at the opacity selected in the colour picker instead of
/// compounding that opacity once per cell.
pub(crate) fn volume_optical_depth_for_alpha(alpha: f32) -> f32 {
    if alpha >= 0.98 {
        return OPAQUE_OPTICAL_DEPTH;
    }
    -(1.0 - alpha).max(0.001).ln()
}

pub(crate) fn make_translucent(color: &mut [f32; 4]) {
    color[3] *= 0.3;
}
