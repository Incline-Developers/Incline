//! Pure maths for the section view's grid: line spacing, and where the world's easting/northing lines cross the cut face.

use glam::{DVec2, DVec3};

use crate::ui::state::SectionGridAxis;

/// The finest a section is ever ruled at, in metres, below which labels would crowd unreadably.
pub(crate) const MIN_SPACING_M: f64 = 1.0;

/// How many grid lines a fully-visible axis aims to show; the actual count only ever lands at or under this.
const TARGET_LINE_COUNT: f64 = 10.0;

/// The closest two ruled lines may sit on screen, in egui points. The line-count rule alone is blind to how large the grid lands on screen.
/// Whichever of the count and pitch rules asks for the coarser spacing wins.
const MIN_LABEL_PITCH_PT: f64 = 48.0;

/// The grid's step table: 1-2-5, deliberately omitting the 2.5 that plotted scales use.
const SPACING_STEPS: [f64; 4] = [1.0, 2.0, 5.0, 10.0];

/// Spacing in metres to rule an axis at, given its span in metres and the same span in egui points on screen, snapped up to the 1-2-5 series.
/// Never finer than [`MIN_SPACING_M`].
/// A non-finite or non-positive `span` returns [`MIN_SPACING_M`] rather than propagating the bad input into a spacing of zero or NaN.
pub(crate) fn grid_spacing(span: f64, span_pt: f64) -> f64 {
    if !span.is_finite() || span <= 0.0 {
        return MIN_SPACING_M;
    }
    let for_count = span / TARGET_LINE_COUNT;
    let for_pitch = if span_pt.is_finite() && span_pt > 0.0 {
        span * MIN_LABEL_PITCH_PT / span_pt
    } else {
        0.0
    };
    crate::model::plot::round_up_to_series(for_count.max(for_pitch), &SPACING_STEPS).max(MIN_SPACING_M)
}

/// Which world axis a cut along `direction` is ruled against: northings for a mostly north-south cut, eastings for mostly east-west, switching
/// exactly at 45 degrees with a tie taking the northings. A cut gridded against the axis it runs parallel to would put every line at the same place, or nowhere.
pub(crate) fn upright_axis(direction: DVec2) -> SectionGridAxis {
    if direction.y.abs() >= direction.x.abs() {
        SectionGridAxis::Northing
    } else {
        SectionGridAxis::Easting
    }
}

/// How much of `axis` the cut covers, in metres, over a strike run of `2 * half_length` - the span the axis is ruled from, not the strike length.
/// A cut square-on to the axis sweeps the whole run; one at 45 degrees sweeps about seven tenths of it.
pub(crate) fn upright_span(direction: DVec2, half_length: f64) -> f64 {
    axis_component(direction, upright_axis(direction)).abs() * 2.0 * half_length
}

/// One place the cut face crosses a line of the world's grid.
pub(crate) struct UprightCrossing {
    /// Distance along the strike from the view centre, in metres, signed the same way the strike direction runs.
    pub(crate) along_strike: f64,
    /// The world easting or northing the crossing sits on, an exact multiple of the spacing.
    pub(crate) value: f64,
}

/// Where the cut face crosses the world's grid lines, over a strike run of `2 * half_length` metres centred on `center_xy`, the view's true world easting and northing.
/// The cut sweeps `axis` from `centre - |component| * half_length` to `centre + |component| * half_length`, where `component` is how much of the axis one metre along the strike covers.
/// `component` cannot be zero: [`upright_axis`] always picks the larger of the two, which for a unit direction is at least `1/sqrt(2)`.
pub(crate) fn upright_crossings(center_xy: DVec2, direction: DVec2, half_length: f64, spacing: f64, max_lines: usize) -> Vec<UprightCrossing> {
    let direction = direction.normalize_or(DVec2::X);
    let axis = upright_axis(direction);
    let component = axis_component(direction, axis);
    let centre = axis_component(center_xy, axis);
    let reach = (component * half_length).abs();
    grid_values((centre - reach, centre + reach), spacing, max_lines)
        .into_iter()
        .map(|value| UprightCrossing {
            along_strike: (value - centre) / component,
            value,
        })
        .collect()
}

/// The true world point on the section plane `along_strike` metres from the view centre, at true elevation `elevation` (only elevation is ever exaggerated for display).
pub(crate) fn plane_point(center_xy: DVec2, direction: DVec2, along_strike: f64, elevation: f64) -> DVec3 {
    let direction = direction.normalize_or(DVec2::X);
    let xy = center_xy + direction * along_strike;
    DVec3::new(xy.x, xy.y, elevation)
}

/// The component of a horizontal vector along one of the world's axes.
fn axis_component(vector: DVec2, axis: SectionGridAxis) -> f64 {
    match axis {
        SectionGridAxis::Easting => vector.x,
        SectionGridAxis::Northing => vector.y,
    }
}

/// Every multiple of `spacing` inside the inclusive `range`, ascending, built as `index * spacing` rather than by repeated addition so values are exact and never drift.
/// A range that would take more than `max_lines` lines is coarsened up through the 1-2-5 series rather than dropped, so an extreme view still gets a grid.
pub(crate) fn grid_values(range: (f64, f64), spacing: f64, max_lines: usize) -> Vec<f64> {
    let (low, high) = range;
    let mut spacing = spacing;
    if !spacing.is_finite() || spacing <= 0.0 {
        return Vec::new();
    }
    loop {
        let first_index = (low / spacing).ceil();
        let last_index = (high / spacing).floor();
        let count = last_index - first_index + 1.0;
        // Rejects an inverted or empty range, a NaN bound, and a range with only one infinite bound.
        if !(count.is_finite() && count >= 1.0) {
            return Vec::new();
        }
        if count <= max_lines as f64 {
            return (0..count as usize).map(|i| (first_index + i as f64) * spacing).collect();
        }
        // Doubling before the round-up always advances to the next step (1->2, 2->5, 5->10), so the count falls every pass.
        spacing = crate::model::plot::round_up_to_series(spacing * 2.0, &SPACING_STEPS);
    }
}
