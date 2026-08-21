//! Evaluates the block model colour ramp and opacity transfers for CPU caching and test reference.

use crate::model::block_model::{NormalizedRamp, lerp_rgba};

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

pub(crate) fn is_hidden_block_appearance(grade: f32, has_grade: bool, ramp: &NormalizedRamp) -> bool {
    is_hidden_block_grade(grade) || (has_grade && grade >= 0.0 && ramp_alpha(ramp, grade) < VISIBLE_ALPHA_EPSILON)
}

/// CPU replica of `ramp_color` in the block-model WGSL shaders.
///
/// Below the first band the colour is `ramp.below` - OMF's `gradient[0]`, which
/// covers everything under the first boundary. Each band then starts at its own
/// position and either holds until the next (a discrete colormap) or blends
/// into it (a continuous one). A grade cutoff is simply a transparent
/// `gradient[0]`, so it needs no special case here.
pub(crate) fn ramp_rgba(ramp: &NormalizedRamp, t: f32) -> [f32; 4] {
    let bands = ramp.bands.as_slice();
    let (Some(first), Some(last)) = (bands.first(), bands.last()) else {
        return ramp.below;
    };
    if !first.is_above(t) {
        return ramp.below;
    }
    if t >= last.t {
        return last.color;
    }
    for pair in bands.windows(2) {
        let (low, high) = (pair[0], pair[1]);
        if !high.is_above(t) {
            if !ramp.interpolate {
                return low.color;
            }
            let span = high.t - low.t;
            let f = if span <= f32::EPSILON { 0.0 } else { (t - low.t) / span };
            return lerp_rgba(low.color, high.color, f);
        }
    }
    last.color
}

pub(crate) fn ramp_alpha(ramp: &NormalizedRamp, t: f32) -> f32 {
    ramp_rgba(ramp, t)[3]
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
