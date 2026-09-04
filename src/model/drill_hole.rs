use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use glam::{DQuat, DVec2, DVec3};
use serde::{Deserialize, Serialize};

use crate::{
    i18n::tr,
    model::{formats::csv_drill_hole::CsvDrillFileMapping, project::ProjectItemState},
};

/// How much wider than the hole itself the collar marker is drawn.
pub(crate) const COLLAR_MARKER_RADIUS_SCALE: f64 = 5.0;
/// Smallest world-space diameter used to draw a drill hole. Source diameters
/// remain untouched; this only stops narrow geometry disappearing visually.
pub(crate) const MIN_RENDER_DIAMETER: f64 = 0.5;
pub(crate) const COLLAR_MARKER_OUTLINE_COLOR: [f32; 3] = [0.086, 0.376, 0.851];
pub(crate) const COLLAR_MARKER_FILL_COLOR: [f32; 3] = [1.0, 1.0, 1.0];
pub(crate) const MAX_DRILL_COLOR_STOPS: usize = 12;
/// Upper bound for an interactively generated blast pattern. It keeps a bad
/// unit/spacing entry from building millions of preview primitives on the UI
/// thread while remaining comfortably above ordinary production rounds.
pub(crate) const MAX_PATTERN_HOLES: usize = 25_000;
/// How thick a tie-in connector is drawn, as a multiple of the radius of the
/// holes it joins. A tie has no physical thickness of its own, so it takes
/// its weight from the pattern it belongs to rather than from the screen.
pub(crate) const TIE_RADIUS_SCALE: f64 = 1.5;

/// Convert a source diameter into the world-space radius used by visual
/// geometry. Kept separate so transient previews use exactly the same rule as
/// persisted holes.
pub(crate) fn render_radius_for_diameter(diameter: Option<f64>) -> f64 {
    diameter.unwrap_or(MIN_RENDER_DIAMETER).max(MIN_RENDER_DIAMETER) * 0.5
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DrillHoleId(pub(crate) u64);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum DrillHoleSource {
    /// Deserialization-only compatibility for sessions created before DHD
    /// support was removed. These sources are never restored or opened.
    #[serde(rename = "Dhd")]
    LegacyDhd { path: PathBuf },
    Csv {
        name: String,
        files: Vec<CsvDrillFileMapping>,
        /// Filename-only browser identity. Native imports use their first
        /// mapped file path while the project retains the decoded dataset.
        #[serde(default)]
        browser_path: Option<PathBuf>,
    },
    /// A dataset decoded from an Open Mining Format container. It stays
    /// in-memory until exported or explicitly saved in another format.
    Omf { name: String, path: PathBuf },
}

impl DrillHoleSource {
    pub(crate) fn display_name(&self) -> String {
        match self {
            Self::LegacyDhd { path } => path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| tr!(literal = "Unsupported drillhole source")),
            Self::Csv { name, .. } => name.clone(),
            Self::Omf { name, .. } => name.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceStation {
    pub(crate) depth: f64,
    pub(crate) position: DVec3,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum DrillValue {
    Numeric(f64),
    Category(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillInterval {
    pub(crate) from: f64,
    pub(crate) to: f64,
    pub(crate) values: BTreeMap<String, DrillValue>,
}

/// One surface connector: the delay laid between two holes, and which way the
/// round travels over it.
///
/// The product is carried by value rather than by [`crate::ui::state::DelayProductId`].
/// The palette is application configuration - its ids are handed out afresh
/// each run and its order is by delay - so an id stored here would repoint at
/// whatever product took its place. A tie is a record of what was actually
/// laid, which is why editing the palette leaves a tied round alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TieIn {
    /// Index of the hole the signal arrives at first.
    pub(crate) from: usize,
    /// ...and of the one it fires onward into.
    pub(crate) to: usize,
    pub(crate) delay_ms: u32,
    pub(crate) product: String,
    pub(crate) color: [f32; 3],
}

impl TieIn {
    /// Whether this connector joins the same two holes as `other`, whichever
    /// way round either runs. Two holes are joined by one connector or none -
    /// there is nowhere to put a second - so this is the identity a new tie
    /// overwrites on.
    pub(crate) fn joins(&self, from: usize, to: usize) -> bool {
        (self.from == from && self.to == to) || (self.from == to && self.to == from)
    }
}

/// Where a round starts, and how long after the shot is fired it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Initiation {
    pub(crate) hole: usize,
    pub(crate) delay_ms: u32,
}

/// Ties as a file holds them: keyed by hole name, because a hole's index is
/// only stable for as long as the dataset stays loaded - see [`DrillHoleRef`] -
/// and ties outlive that.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct StoredTieIns {
    #[serde(default)]
    pub(crate) ties: Vec<StoredTieIn>,
    /// Every hole that can start the round. New files use this collection;
    /// `initiation` below is retained only so projects written by the first
    /// tie-in implementation still open without losing their start point.
    #[serde(default)]
    pub(crate) initiations: Vec<StoredInitiation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) initiation: Option<StoredInitiation>,
}

impl StoredTieIns {
    pub(crate) fn is_empty(&self) -> bool {
        self.ties.is_empty() && self.initiations.is_empty() && self.initiation.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredTieIn {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) delay_ms: u32,
    pub(crate) product: String,
    pub(crate) color: [f32; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredInitiation {
    pub(crate) hole: String,
    pub(crate) delay_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillHole {
    pub(crate) dhid: String,
    pub(crate) collar: DVec3,
    /// Source diameter, kept distinct from the visual floor supplied by
    /// [`DrillHole::render_radius`].
    pub(crate) diameter: Option<f64>,
    pub(crate) trace: Vec<TraceStation>,
    /// Explicit geometry coverage. Empty means the complete measured-depth
    /// trace is continuous.
    pub(crate) render_ranges: Vec<(f64, f64)>,
    pub(crate) intervals: Vec<DrillInterval>,
}

/// Row arrangement used when filling a blast boundary with collars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DrillPatternLayout {
    #[default]
    Square,
    Staggered,
}

impl DrillPatternLayout {
    pub(crate) const ALL: [Self; 2] = [Self::Square, Self::Staggered];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Square => "Square",
            Self::Staggered => "Staggered",
        }
    }
}

/// Fill an XY polygon with a drill grid rotated counter-clockwise from global
/// X. `spacing` runs within a row and `burden` separates rows; staggered rows
/// move half a spacing. Collars sit half a cell inside the rotated polygon
/// bounds and take their Z from the boundary's polygon plane.
pub(crate) fn generate_pattern_collars(
    boundary: &[DVec3],
    burden: f64,
    spacing: f64,
    rotation_degrees: f64,
    offset: DVec2,
    layout: DrillPatternLayout,
) -> Result<Vec<DVec3>, String> {
    if boundary.len() < 3 || boundary.iter().any(|point| !point.is_finite()) {
        return Err("Choose a valid closed polyline".to_owned());
    }
    if !burden.is_finite() || !spacing.is_finite() || burden <= 0.0 || spacing <= 0.0 {
        return Err("Burden and spacing must be greater than zero".to_owned());
    }
    if !rotation_degrees.is_finite() || !offset.is_finite() {
        return Err("Rotation and offsets must contain valid numbers".to_owned());
    }

    let centroid = boundary.iter().copied().sum::<DVec3>() / boundary.len() as f64;
    let (sin_rotation, cos_rotation) = rotation_degrees.to_radians().sin_cos();
    let grid_offset = DVec2::new(offset.x * cos_rotation + offset.y * sin_rotation, -offset.x * sin_rotation + offset.y * cos_rotation);
    let grid_boundary = boundary
        .iter()
        .map(|point| {
            let offset = point.truncate() - centroid.truncate();
            DVec3::new(
                offset.x * cos_rotation + offset.y * sin_rotation,
                -offset.x * sin_rotation + offset.y * cos_rotation,
                point.z,
            )
        })
        .collect::<Vec<_>>();
    let min_x = grid_boundary.iter().map(|point| point.x).fold(f64::INFINITY, f64::min);
    let max_x = grid_boundary.iter().map(|point| point.x).fold(f64::NEG_INFINITY, f64::max);
    let min_y = grid_boundary.iter().map(|point| point.y).fold(f64::INFINITY, f64::min);
    let max_y = grid_boundary.iter().map(|point| point.y).fold(f64::NEG_INFINITY, f64::max);
    let width = max_x - min_x;
    let height = max_y - min_y;
    let columns = ((width / spacing).ceil() as usize).max(1);
    let rows = ((height / burden).ceil() as usize).max(1);
    let cells = columns.saturating_mul(rows);
    if cells > MAX_PATTERN_HOLES.saturating_mul(20) {
        return Err(format!(
            "This spacing would scan too many grid cells; increase burden or spacing (maximum {MAX_PATTERN_HOLES} holes)"
        ));
    }

    // Newell's method gives a stable normal for either winding and polygons
    // with more than three vertices. A near-vertical/degenerate ring falls
    // back to the mean boundary elevation below.
    let mut normal = DVec3::ZERO;
    for index in 0..boundary.len() {
        let current = boundary[index];
        let next = boundary[(index + 1) % boundary.len()];
        normal.x += (current.y - next.y) * (current.z + next.z);
        normal.y += (current.z - next.z) * (current.x + next.x);
        normal.z += (current.x - next.x) * (current.y + next.y);
    }
    if normal.z.abs() <= (width * height).abs().max(1.0) * 1.0e-12 {
        return Err("The selected polyline has no usable XY area".to_owned());
    }
    let elevation = |x: f64, y: f64| {
        if normal.z.abs() > 1.0e-12 {
            centroid.z - (normal.x * (x - centroid.x) + normal.y * (y - centroid.y)) / normal.z
        } else {
            centroid.z
        }
    };

    let mut collars = Vec::with_capacity(cells.min(MAX_PATTERN_HOLES));
    let base_x = if width < spacing { (min_x + max_x) * 0.5 } else { min_x + spacing * 0.5 };
    let base_y = if height < burden { (min_y + max_y) * 0.5 } else { min_y + burden * 0.5 };
    // Reduce arbitrary offsets to one lattice period and start at the first
    // candidate inside the rotated bounds. This keeps large coordinate entry
    // values cheap while preserving their exact pattern phase.
    let first_x = base_x + grid_offset.x + ((min_x - base_x - grid_offset.x) / spacing).ceil() * spacing;
    let first_y = base_y + grid_offset.y + ((min_y - base_y - grid_offset.y) / burden).ceil() * burden;
    for row in 0..rows {
        let y = first_y + row as f64 * burden;
        if y >= max_y {
            break;
        }
        let stagger = if layout == DrillPatternLayout::Staggered && row % 2 == 1 { spacing * 0.5 } else { 0.0 };
        for column in 0..columns {
            let x = first_x + column as f64 * spacing + stagger;
            if x >= max_x {
                break;
            }
            if point_in_polygon_xy(x, y, &grid_boundary) {
                let world_x = centroid.x + x * cos_rotation - y * sin_rotation;
                let world_y = centroid.y + x * sin_rotation + y * cos_rotation;
                collars.push(DVec3::new(world_x, world_y, elevation(world_x, world_y)));
                if collars.len() > MAX_PATTERN_HOLES {
                    return Err(format!("Pattern exceeds the maximum of {MAX_PATTERN_HOLES} holes; increase burden or spacing"));
                }
            }
        }
    }
    if collars.is_empty() {
        return Err("No holes fit inside this boundary at the current burden and spacing".to_owned());
    }
    Ok(collars)
}

fn point_in_polygon_xy(x: f64, y: f64, boundary: &[DVec3]) -> bool {
    let point = glam::DVec2::new(x, y);
    let mut inside = false;
    for index in 0..boundary.len() {
        let a = boundary[index].truncate();
        let b = boundary[(index + 1) % boundary.len()].truncate();
        let edge = b - a;
        let length_squared = edge.length_squared();
        if length_squared > 0.0 {
            let t = ((point - a).dot(edge) / length_squared).clamp(0.0, 1.0);
            if point.distance_squared(a + edge * t) <= 1.0e-16 {
                return true;
            }
        }
        if (a.y > y) != (b.y > y) {
            let crossing_x = (b.x - a.x) * (y - a.y) / (b.y - a.y) + a.x;
            if x < crossing_x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Everything a move rewrites in a hole: its collar and its trace. Nothing
/// else is touched - the intervals above all, whose per-interval value maps
/// are what makes a whole [`DrillHole`] expensive to copy - so a live preview
/// captures and rewrites only this.
#[derive(Clone, Debug)]
pub(crate) struct HolePlacement {
    pub(crate) collar: DVec3,
    pub(crate) trace: Vec<TraceStation>,
}

/// Where a hole points, in the terms a drill plan is written in: `azimuth`
/// degrees clockwise from grid north, `dip` degrees from horizontal with down
/// negative. This is the convention [`project_tangent`] resolves a survey in,
/// so a hole read out of a file and a hole turned here describe themselves the
/// same way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HoleOrientation {
    pub(crate) azimuth: f64,
    pub(crate) dip: f64,
}

/// How far a hole may be tipped either side of horizontal. Passing vertical
/// would carry the hole over to the opposite bearing, silently rewriting the
/// azimuth the driller was given, so a turn stops there instead.
pub(crate) const MAX_HOLE_DIP: f64 = 90.0;

/// How a Rotate Collar edit turns the holes it was handed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CollarRotation {
    /// Point every hole the same way, whatever each was pointing before - what
    /// the panel applies, a round being drilled at one angle.
    Absolute(HoleOrientation),
    /// Turn each hole from where it stood, so a pattern that was not uniform
    /// to begin with keeps its spread - what a ring drag produces.
    Delta { azimuth: f64, dip: f64 },
}

impl CollarRotation {
    /// The turn that leaves every hole exactly where it was, which is what
    /// reverting a Rotate Collar command writes.
    pub(crate) const IDENTITY: Self = Self::Delta { azimuth: 0.0, dip: 0.0 };

    pub(crate) fn is_identity(self) -> bool {
        matches!(self, Self::Delta { azimuth, dip } if azimuth == 0.0 && dip == 0.0)
    }
}

/// Which way a collar was set up, read off the first surveyed length below
/// it - the piece of the hole the rig actually aimed, rather than the
/// collar-to-toe chord, which on a hole that bends is neither.
///
/// A trace with no length below the collar has no direction to report.
fn trace_orientation(collar: DVec3, trace: &[TraceStation]) -> Option<HoleOrientation> {
    let direction = trace.iter().find_map(|station| {
        let offset = station.position - collar;
        (offset.length_squared() > 1.0e-12).then_some(offset)
    })?;
    Some(direction_orientation(direction))
}

/// Azimuth and dip of a direction, in the drill-plan convention.
///
/// A dead-vertical direction has no bearing to report and comes back as north,
/// the same stand-in [`resolve_trace`] falls back on for a survey that never
/// gave one.
pub(crate) fn direction_orientation(direction: DVec3) -> HoleOrientation {
    HoleOrientation {
        azimuth: direction.x.atan2(direction.y).to_degrees().rem_euclid(360.0),
        dip: direction.z.atan2(direction.x.hypot(direction.y)).to_degrees(),
    }
}

/// The world rotation taking a hole pointing `from` to one pointing `to`: a
/// swing about the vertical, then a tilt within the vertical plane the swing
/// left it in.
///
/// Decomposed that way rather than as a shortest-arc rotation because the two
/// halves are the two numbers on the drill plan: each gizmo ring drives one of
/// them, and neither disturbs the other.
fn orientation_rotation(from: HoleOrientation, to: HoleOrientation) -> DQuat {
    // Azimuth runs clockwise seen from above, which is the negative direction
    // about +Z.
    let swing = DQuat::from_rotation_z(-(to.azimuth - from.azimuth).to_radians());
    // Horizontal axis square to the destination bearing: turning about it is
    // what raises or drops the toe.
    let bearing = to.azimuth.to_radians();
    let tilt_axis = DVec3::new(bearing.cos(), -bearing.sin(), 0.0);
    DQuat::from_axis_angle(tilt_axis, (to.dip - from.dip).to_radians()) * swing
}

impl HolePlacement {
    /// Where the collar stands. Mirrors [`DrillHole::collar_position`]: the
    /// trace's first station is it, and `collar` only stands in for a hole
    /// that arrived without a trace.
    pub(crate) fn collar_position(&self) -> DVec3 {
        self.trace.first().map_or(self.collar, |station| station.position)
    }

    /// Which way the collar was set up. See [`trace_orientation`].
    pub(crate) fn orientation(&self) -> Option<HoleOrientation> {
        trace_orientation(self.collar_position(), &self.trace)
    }

    /// What `rotation` would leave this hole pointing at. `None` where the
    /// hole has no direction of its own to turn from.
    pub(crate) fn rotated_orientation(&self, rotation: CollarRotation) -> Option<HoleOrientation> {
        let from = self.orientation()?;
        Some(match rotation {
            CollarRotation::Absolute(target) => target,
            CollarRotation::Delta { azimuth, dip } => HoleOrientation {
                azimuth: (from.azimuth + azimuth).rem_euclid(360.0),
                dip: (from.dip + dip).clamp(-MAX_HOLE_DIP, MAX_HOLE_DIP),
            },
        })
    }
}

impl DrillHole {
    /// Capture where the hole stands, for a preview to rewrite it from.
    pub(crate) fn placement(&self) -> HolePlacement {
        HolePlacement {
            collar: self.collar,
            trace: self.trace.clone(),
        }
    }

    /// Put the hole back at `placement` translated by `delta` - a zero delta
    /// therefore restores it exactly. The trace is copied into the existing
    /// allocation rather than replacing it. Interval geometry is measured down
    /// the trace rather than in world space, so it needs no adjustment.
    pub(crate) fn set_placement(&mut self, placement: &HolePlacement, delta: DVec3) {
        self.collar = placement.collar + delta;
        self.trace.clone_from(&placement.trace);
        if delta != DVec3::ZERO {
            for station in &mut self.trace {
                station.position += delta;
            }
        }
    }

    /// Put the hole back at `placement` turned by `rotation` about its own
    /// collar - [`CollarRotation::IDENTITY`] therefore restores it exactly.
    ///
    /// The collar never moves: the whole trace swings rigidly about it, survey
    /// curvature and all, so a turned hole is the same hole aimed elsewhere
    /// rather than a straightened one. Interval geometry is measured down the
    /// trace rather than in world space, so it needs no adjustment - the same
    /// reason [`Self::set_placement`] leaves it alone.
    pub(crate) fn set_rotated_placement(&mut self, placement: &HolePlacement, rotation: CollarRotation) {
        self.collar = placement.collar;
        self.trace.clone_from(&placement.trace);
        if rotation.is_identity() {
            // The restore path, taken on every rollback: the trace copy above
            // is already the whole of it.
            return;
        }
        let Some(from) = placement.orientation() else {
            // Nothing below the collar to aim, so there is nothing to turn.
            return;
        };
        let Some(to) = placement.rotated_orientation(rotation) else {
            return;
        };
        if to == from {
            return;
        }
        let quat = orientation_rotation(from, to);
        let pivot = placement.collar_position();
        for station in &mut self.trace {
            station.position = pivot + quat * (station.position - pivot);
        }
    }

    /// The world radius the hole is drawn at. Missing or physically narrow
    /// diameters use the visual floor, so traces and the geometry scaled from
    /// them stay legible without changing the source value.
    pub(crate) fn render_radius(&self) -> f64 {
        render_radius_for_diameter(self.diameter)
    }

    /// Where the collar stands. The trace's first station is it; `collar`
    /// only stands in for a dataset that arrived without a trace.
    pub(crate) fn collar_position(&self) -> DVec3 {
        self.trace.first().map_or(self.collar, |station| station.position)
    }

    /// Which way the hole was set up. See [`trace_orientation`]. Asked of the
    /// hole rather than of a captured placement so a per-frame readout costs
    /// no trace copy.
    pub(crate) fn orientation(&self) -> Option<HoleOrientation> {
        trace_orientation(self.collar_position(), &self.trace)
    }

    pub(crate) fn position_at_depth(&self, depth: f64) -> Option<DVec3> {
        let first = *self.trace.first()?;
        if depth <= first.depth {
            return Some(first.position);
        }
        for pair in self.trace.windows(2) {
            let [a, b] = [pair[0], pair[1]];
            if depth <= b.depth {
                let span = b.depth - a.depth;
                let t = if span > 0.0 { ((depth - a.depth) / span).clamp(0.0, 1.0) } else { 0.0 };
                return Some(a.position.lerp(b.position, t));
            }
        }
        self.trace.last().map(|station| station.position)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DrillFieldKind {
    Numeric { min: f64, max: f64 },
    Categorical { categories: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrillField {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) kind: DrillFieldKind,
}

#[derive(Clone, Debug)]
pub(crate) struct DrillHoleDataset {
    pub(crate) holes: Vec<DrillHole>,
    pub(crate) fields: Vec<DrillField>,
    pub(crate) bounds: Option<(DVec3, DVec3)>,
    /// The surface connectors tying the pattern in. Content rather than
    /// styling: they dirty the project and are undone with everything else.
    pub(crate) ties: Vec<TieIn>,
    /// Holes the round can start at. One initiation per collar, with any
    /// number of collars participating in the same firing graph.
    pub(crate) initiations: Vec<Initiation>,
}

impl DrillHoleDataset {
    pub(crate) fn new(mut holes: Vec<DrillHole>) -> Self {
        holes.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.dhid, &b.dhid));
        let fields = collect_fields(&holes);
        let bounds = drill_bounds(&holes);
        Self {
            holes,
            fields,
            bounds,
            ties: Vec::new(),
            initiations: Vec::new(),
        }
    }

    /// The connector between two holes, whichever way round it runs.
    pub(crate) fn tie_between(&self, from: usize, to: usize) -> Option<&TieIn> {
        self.ties.iter().find(|tie| tie.joins(from, to))
    }

    /// When each hole fires, in milliseconds from the shot going off, or
    /// `None` for a hole no signal reaches.
    ///
    /// A hole fires on the *first* signal to arrive, so this is a multi-source
    /// shortest path from the initiation points rather than a walk of the graph: a round
    /// tied in a loop is well defined, and a connector that arrives after its
    /// hole has already gone simply does nothing.
    pub(crate) fn firing_times(&self) -> Vec<Option<u32>> {
        let mut times = vec![None; self.holes.len()];
        let mut queue = std::collections::BinaryHeap::new();
        for initiation in self.initiations.iter().filter(|initiation| initiation.hole < self.holes.len()) {
            if times[initiation.hole].is_none_or(|existing| initiation.delay_ms < existing) {
                times[initiation.hole] = Some(initiation.delay_ms);
                queue.push(std::cmp::Reverse((initiation.delay_ms, initiation.hole)));
            }
        }
        while let Some(std::cmp::Reverse((time, hole))) = queue.pop() {
            if times[hole].is_some_and(|settled| settled < time) {
                continue;
            }
            for tie in self.ties.iter().filter(|tie| tie.from == hole) {
                let Some(arrival) = times.get_mut(tie.to) else {
                    continue;
                };
                let candidate = time.saturating_add(tie.delay_ms);
                if arrival.is_none_or(|existing| candidate < existing) {
                    *arrival = Some(candidate);
                    queue.push(std::cmp::Reverse((candidate, tie.to)));
                }
            }
        }
        times
    }

    /// The ties as a file holds them, keyed by hole name.
    pub(crate) fn stored_ties(&self) -> StoredTieIns {
        let name = |index: usize| self.holes.get(index).map(|hole| hole.dhid.clone());
        StoredTieIns {
            ties: self
                .ties
                .iter()
                .filter_map(|tie| {
                    Some(StoredTieIn {
                        from: name(tie.from)?,
                        to: name(tie.to)?,
                        delay_ms: tie.delay_ms,
                        product: tie.product.clone(),
                        color: tie.color,
                    })
                })
                .collect(),
            initiations: self
                .initiations
                .iter()
                .filter_map(|initiation| {
                    Some(StoredInitiation {
                        hole: name(initiation.hole)?,
                        delay_ms: initiation.delay_ms,
                    })
                })
                .collect(),
            initiation: None,
        }
    }

    /// Resolve stored ties back onto this dataset's holes, reporting how many
    /// were dropped because a hole they named is no longer here. Called after
    /// construction, since it is [`Self::new`] that fixes the hole order the
    /// indices are against.
    pub(crate) fn apply_stored_ties(&mut self, stored: StoredTieIns) -> usize {
        let index_of = |name: &str| self.holes.iter().position(|hole| hole.dhid == name);
        let mut dropped = 0;
        self.ties = stored
            .ties
            .into_iter()
            .filter_map(|tie| {
                let (Some(from), Some(to)) = (index_of(&tie.from), index_of(&tie.to)) else {
                    dropped += 1;
                    return None;
                };
                Some(TieIn {
                    from,
                    to,
                    delay_ms: tie.delay_ms,
                    product: tie.product,
                    color: tie.color,
                })
            })
            .collect();
        let stored_initiations = stored.initiations.into_iter().chain(stored.initiation);
        self.initiations = stored_initiations
            .filter_map(|initiation| {
                let Some(hole) = index_of(&initiation.hole) else {
                    dropped += 1;
                    return None;
                };
                Some(Initiation {
                    hole,
                    delay_ms: initiation.delay_ms,
                })
            })
            .collect();
        // A collar has one editable initiation card. If a malformed file
        // names it twice, keep the last value rather than drawing stacked
        // cards or feeding duplicate sources into the firing graph.
        self.initiations.reverse();
        let mut seen = std::collections::HashSet::new();
        self.initiations.retain(|initiation| seen.insert(initiation.hole));
        self.initiations.reverse();
        dropped
    }

    /// Approximate retained size, used by the undo history's memory budget.
    pub(crate) fn estimated_bytes(&self) -> usize {
        size_of::<Self>()
            + self
                .holes
                .iter()
                .map(|hole| {
                    size_of::<DrillHole>()
                        + hole.dhid.len()
                        + hole.trace.len() * size_of::<TraceStation>()
                        + hole.render_ranges.len() * size_of::<(f64, f64)>()
                        + hole
                            .intervals
                            .iter()
                            .map(|interval| {
                                size_of::<DrillInterval>()
                                    + interval
                                        .values
                                        .iter()
                                        .map(|(key, value)| {
                                            key.len()
                                                + size_of::<DrillValue>()
                                                + match value {
                                                    DrillValue::Category(text) => text.len(),
                                                    DrillValue::Numeric(_) => 0,
                                                }
                                        })
                                        .fold(0usize, usize::saturating_add)
                            })
                            .fold(0usize, usize::saturating_add)
                })
                .fold(0usize, usize::saturating_add)
            + self.ties.iter().map(|tie| size_of::<TieIn>() + tie.product.len()).fold(0usize, usize::saturating_add)
            + self.initiations.len() * size_of::<Initiation>()
    }

    pub(crate) fn field(&self, key: &str) -> Option<&DrillField> {
        self.fields.iter().find(|field| field.key == key)
    }

    /// Recompute the dataset's extent after its holes have moved. Fields are
    /// interval values rather than geometry, so only the bounds go stale.
    pub(crate) fn refresh_bounds(&mut self) {
        self.bounds = drill_bounds(&self.holes);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DrillColorPreset {
    Rainbow,
    Grayscale,
    Heat,
    GreenYellowRed,
}

impl DrillColorPreset {
    pub(crate) const ALL: [Self; 4] = [Self::Rainbow, Self::Grayscale, Self::Heat, Self::GreenYellowRed];

    pub(crate) fn label(self) -> String {
        match self {
            Self::Rainbow => crate::i18n::tr!(literal = "Rainbow"),
            Self::Grayscale => crate::i18n::tr!(literal = "Grayscale"),
            Self::Heat => crate::i18n::tr!(literal = "Heat"),
            Self::GreenYellowRed => crate::i18n::tr!(literal = "Green–Yellow–Red"),
        }
    }

    pub(crate) fn smooth(self) -> bool {
        matches!(self, Self::Rainbow | Self::Grayscale | Self::Heat)
    }

    pub(crate) fn stops(self) -> Vec<DrillColorStop> {
        let colors: &[[f32; 3]] = match self {
            Self::Rainbow => &[[0.05, 0.20, 0.95], [0.00, 0.85, 0.95], [0.05, 0.82, 0.20], [1.00, 0.85, 0.00], [0.92, 0.02, 0.02]],
            Self::Grayscale => &[[0.05, 0.05, 0.05], [0.95, 0.95, 0.95]],
            Self::Heat => &[[0.02, 0.02, 0.02], [0.85, 0.00, 0.00], [1.00, 0.78, 0.00], [1.00, 1.00, 0.92]],
            Self::GreenYellowRed => &[[0.00, 0.82, 0.20], [1.00, 0.85, 0.00], [0.92, 0.00, 0.00]],
        };
        let denom = if self.smooth() { colors.len().saturating_sub(1).max(1) } else { colors.len().max(1) } as f32;
        colors
            .iter()
            .enumerate()
            .map(|(index, color)| DrillColorStop {
                t: index as f32 / denom,
                color: *color,
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillColorStop {
    pub(crate) t: f32,
    pub(crate) color: [f32; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillCategoryColor {
    pub(crate) value: String,
    pub(crate) color: [f32; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillColorState {
    pub(crate) active_field: Option<String>,
    pub(crate) preset: DrillColorPreset,
    pub(crate) smooth: bool,
    pub(crate) stops: Vec<DrillColorStop>,
    pub(crate) categories: Vec<DrillCategoryColor>,
}

impl Default for DrillColorState {
    fn default() -> Self {
        let preset = DrillColorPreset::Rainbow;
        Self {
            active_field: None,
            preset,
            smooth: preset.smooth(),
            stops: preset.stops(),
            categories: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedDrillHoleDataset {
    pub(crate) name: String,
    pub(crate) source: DrillHoleSource,
    pub(crate) dataset: Arc<DrillHoleDataset>,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenDrillHoleDataset {
    pub(crate) id: DrillHoleId,
    pub(crate) state: ProjectItemState,
    pub(crate) name: String,
    pub(crate) dataset: Arc<DrillHoleDataset>,
    pub(crate) visible: bool,
    pub(crate) color: DrillColorState,
}

impl OpenDrillHoleDataset {
    pub(crate) fn entity_id(&self) -> crate::model::SceneEntityId {
        crate::model::SceneEntityId::DrillHole(self.id)
    }
}

/// One hole inside a dataset.
///
/// A dataset is a scene entity - it is what the explorer lists, what is
/// hidden, frozen and coloured - so [`crate::model::SceneEntityId`] names the
/// dataset and stops there. A single hole is a part of one, the way a vertex
/// is part of a polyline, and this is how the Drill & Blast workspace points
/// at it: the dataset's id and the hole's index in
/// [`DrillHoleDataset::holes`], which is fixed for as long as the dataset is
/// loaded ([`DrillHoleDataset::new`] sorts the holes once, on import).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DrillHoleRef {
    pub(crate) dataset: DrillHoleId,
    pub(crate) hole: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SurveyObservation {
    pub(crate) depth: f64,
    pub(crate) azimuth: Option<f64>,
    pub(crate) dip: Option<f64>,
    pub(crate) position: Option<DVec3>,
}

pub(crate) fn resolve_trace(collar: DVec3, observations: &mut [SurveyObservation], target_depth: f64) -> Vec<TraceStation> {
    observations.sort_by(|a, b| a.depth.total_cmp(&b.depth));
    let mut trace = vec![TraceStation { depth: 0.0, position: collar }];
    let mut last_orientation = observations.iter().find_map(SurveyObservation::orientation);
    for observation in observations.iter().copied() {
        if !observation.depth.is_finite() || observation.depth < 0.0 {
            continue;
        }
        let previous = *trace.last().expect("collar station exists");
        if observation.depth + 1.0e-9 < previous.depth {
            continue;
        }
        let stored_position = observation.position.filter(|position| position.is_finite());
        let position = stored_position.unwrap_or_else(|| {
            let (azimuth, dip) = last_orientation.unwrap_or((0.0, -90.0));
            project_tangent(previous.position, observation.depth - previous.depth, azimuth, dip)
        });
        if observation.depth > previous.depth + 1.0e-9 {
            trace.push(TraceStation {
                depth: observation.depth,
                position,
            });
        } else if let Some(first) = trace.first_mut() {
            first.position = collar;
        }
        last_orientation = observation
            .orientation()
            .or_else(|| stored_position.and_then(|_| orientation_from_delta(position - previous.position)))
            .or(last_orientation);
    }
    let previous = *trace.last().expect("collar station exists");
    if target_depth.is_finite() && target_depth > previous.depth + 1.0e-9 {
        let (azimuth, dip) = last_orientation.unwrap_or((0.0, -90.0));
        trace.push(TraceStation {
            depth: target_depth,
            position: project_tangent(previous.position, target_depth - previous.depth, azimuth, dip),
        });
    }
    trace
}

impl SurveyObservation {
    fn orientation(&self) -> Option<(f64, f64)> {
        match (self.azimuth, self.dip) {
            (Some(azimuth), Some(dip)) if azimuth.is_finite() && dip.is_finite() => Some((azimuth, dip)),
            _ => None,
        }
    }
}

fn orientation_from_delta(delta: DVec3) -> Option<(f64, f64)> {
    let horizontal = delta.x.hypot(delta.y);
    let length = horizontal.hypot(delta.z);
    (length > 1.0e-12).then(|| (delta.x.atan2(delta.y).to_degrees().rem_euclid(360.0), delta.z.atan2(horizontal).to_degrees()))
}

fn project_tangent(origin: DVec3, distance: f64, azimuth_degrees: f64, dip_degrees: f64) -> DVec3 {
    let azimuth = azimuth_degrees.to_radians();
    let dip = dip_degrees.to_radians();
    let horizontal = distance * dip.cos();
    origin + DVec3::new(horizontal * azimuth.sin(), horizontal * azimuth.cos(), distance * dip.sin())
}

fn collect_fields(holes: &[DrillHole]) -> Vec<DrillField> {
    let mut numeric: BTreeMap<String, (String, f64, f64)> = BTreeMap::new();
    let mut categorical: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for hole in holes {
        for interval in &hole.intervals {
            for (key, value) in &interval.values {
                let label = key.clone();
                match value {
                    DrillValue::Numeric(value) if value.is_finite() => {
                        numeric
                            .entry(key.clone())
                            .and_modify(|(_, min, max)| {
                                *min = min.min(*value);
                                *max = max.max(*value);
                            })
                            .or_insert((label, *value, *value));
                    }
                    DrillValue::Category(value) if !value.trim().is_empty() => {
                        let values = &mut categorical.entry(key.clone()).or_insert_with(|| (label, Vec::new())).1;
                        if !values.contains(value) {
                            values.push(value.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut fields = Vec::new();
    fields.extend(numeric.into_iter().map(|(key, (label, min, max))| DrillField {
        key,
        label,
        kind: DrillFieldKind::Numeric { min, max },
    }));
    fields.extend(categorical.into_iter().map(|(key, (label, mut categories))| {
        categories.sort_by(|a, b| crate::natural_sort::natural_cmp(a, b));
        categories.truncate(MAX_DRILL_COLOR_STOPS);
        DrillField {
            key,
            label,
            kind: DrillFieldKind::Categorical { categories },
        }
    }));
    fields.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.label, &b.label));
    fields
}

fn drill_bounds(holes: &[DrillHole]) -> Option<(DVec3, DVec3)> {
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    let mut any = false;
    for hole in holes {
        // A collar without a resolvable measured-depth segment remains part
        // of the dataset/explorer counts, but it is not rendered and must not
        // pull camera fitting toward an otherwise empty coordinate.
        if hole.trace.len() < 2 {
            continue;
        }
        // Conservatively cover both the trace and the world-sized collar,
        // including the same visual diameter floor as the instance builders.
        let radius = hole.render_radius() * COLLAR_MARKER_RADIUS_SCALE;
        for station in &hole.trace {
            min = min.min(station.position - DVec3::splat(radius));
            max = max.max(station.position + DVec3::splat(radius));
            any = true;
        }
    }
    any.then_some((min, max))
}

pub(crate) fn default_category_colors(categories: &[String]) -> Vec<DrillCategoryColor> {
    const COLORS: [[f32; 3]; 12] = [
        [0.12, 0.47, 0.71],
        [1.00, 0.50, 0.05],
        [0.17, 0.63, 0.17],
        [0.84, 0.15, 0.16],
        [0.58, 0.40, 0.74],
        [0.55, 0.34, 0.29],
        [0.89, 0.47, 0.76],
        [0.50, 0.50, 0.50],
        [0.74, 0.74, 0.13],
        [0.09, 0.75, 0.81],
        [0.30, 0.60, 0.90],
        [0.90, 0.60, 0.20],
    ];
    categories
        .iter()
        .take(MAX_DRILL_COLOR_STOPS)
        .enumerate()
        .map(|(index, value)| DrillCategoryColor {
            value: value.clone(),
            color: COLORS[index],
        })
        .collect()
}
