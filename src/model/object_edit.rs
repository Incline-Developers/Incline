//! Pure helpers behind the "Edit Object" dialog: bulge/arc conversions, the
//! compact-circle round trip, vertex row operations, derived length and area,
//! and working-copy validation.

use glam::{DVec2, DVec3};

use crate::model::{
    Object, PolyVertex,
    geometry::{bulge_arc_parameters, compact_circle_center, tessellate_polyline_bulges},
};

/// Sweep angle of a bulged segment, in degrees. `bulge = tan(sweep / 4)`.
pub(crate) fn bulge_to_sweep_degrees(bulge: f64) -> f64 {
    if !bulge.is_finite() {
        return 0.0;
    }
    (4.0 * bulge.atan()).to_degrees()
}

/// Inverse of [`bulge_to_sweep_degrees`]. The tangent asymptotes at a full
/// turn, so a sweep outside the open `(-360, 360)` range (or non-finite) has
/// no bulge encoding and falls back to a straight segment.
pub(crate) fn sweep_degrees_to_bulge(sweep_degrees: f64) -> f64 {
    if !sweep_degrees.is_finite() || sweep_degrees <= -360.0 || sweep_degrees >= 360.0 {
        return 0.0;
    }
    (sweep_degrees.to_radians() / 4.0).tan()
}

/// Horizontal chord length of a segment. Bulge arcs are horizontal by data
/// model, so the arc's chord is the XY distance.
pub(crate) fn chord_length_xy(start: DVec3, end: DVec3) -> f64 {
    (end.truncate() - start.truncate()).length()
}

/// Radius of the arc a chord and bulge describe.
pub(crate) fn bulge_radius(chord: f64, bulge: f64) -> Option<f64> {
    if !chord.is_finite() || !bulge.is_finite() || bulge.abs() <= f64::EPSILON || chord <= f64::EPSILON {
        return None;
    }
    Some(chord * (1.0 + bulge * bulge) / (4.0 * bulge.abs()))
}

/// Bulge that gives `radius` over `chord`, keeping the direction and the
/// minor/major choice of `previous_bulge`.
///
/// `c = 2r sin(sweep / 2)`, so the minor-arc sweep recovers as
/// `2 * asin(c / (2r))`; `previous_bulge` over unit magnitude means the major
/// arc, so the complementary sweep is used instead.
pub(crate) fn bulge_for_radius(chord: f64, radius: f64, previous_bulge: f64) -> Option<f64> {
    if !chord.is_finite() || !radius.is_finite() || !previous_bulge.is_finite() || chord <= 0.0 || radius <= 0.0 || 2.0 * radius < chord {
        return None;
    }
    let sweep_minor = 2.0 * (chord / (2.0 * radius)).asin();
    let sweep = if previous_bulge.abs() > 1.0 {
        2.0 * std::f64::consts::PI - sweep_minor
    } else {
        sweep_minor
    };
    let magnitude = (sweep / 4.0).tan().abs();
    Some(if previous_bulge < 0.0 { -magnitude } else { magnitude })
}

/// Centre and radius of a compact two-vertex circle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CircleSpec {
    pub(crate) center: DVec3,
    pub(crate) radius: f64,
}

/// Read a compact circle's centre and radius, if the vertices are one.
pub(crate) fn compact_circle(verts: &[PolyVertex], closed: bool) -> Option<CircleSpec> {
    let center = compact_circle_center(verts, closed)?;
    let [first, second] = verts else {
        return None;
    };
    let radius = chord_length_xy(first.pos, second.pos) * 0.5;
    if !radius.is_finite() || radius <= f64::EPSILON {
        return None;
    }
    Some(CircleSpec { center, radius })
}

/// Write a new centre and radius back onto a compact circle's two vertices,
/// keeping their bearing and winding direction. Returns whether it applied.
pub(crate) fn set_compact_circle(verts: &mut [PolyVertex], center: DVec3, radius: f64) -> bool {
    if !center.is_finite() || !radius.is_finite() || radius <= f64::EPSILON {
        return false;
    }
    let [first, second] = verts else {
        return false;
    };
    let old_center = (first.pos + second.pos) * 0.5;
    let offset_dir = first.pos.truncate() - old_center.truncate();
    let bearing = if offset_dir.is_finite() && offset_dir.length_squared() > f64::EPSILON {
        offset_dir.normalize()
    } else {
        DVec2::X
    };
    let sign = if first.bulge.is_sign_negative() { -1.0 } else { 1.0 };
    let offset = DVec3::new(bearing.x * radius, bearing.y * radius, 0.0);
    first.pos = center + offset;
    second.pos = center - offset;
    first.bulge = sign;
    second.bulge = sign;
    true
}

/// Insert a vertex after `index`, splitting that segment. Returns the new
/// vertex's row.
///
/// A straight segment splits at its XYZ midpoint, a bulged one at the arc's
/// true midpoint (elevation interpolated linearly), each half taking half
/// the original sweep. An open polyline's last segment instead extends past
/// the end by repeating the preceding step, or a unit step east if none
/// exists (single vertex, coincident last two, non-finite data) - never an
/// exact duplicate row.
pub(crate) fn insert_vertex_after(verts: &mut Vec<PolyVertex>, index: usize, closed: bool) -> Option<usize> {
    /// Fallback extension step for when no existing segment gives a direction.
    const NO_DIRECTION_STEP: DVec3 = DVec3::new(1.0, 0.0, 0.0);

    if verts.is_empty() || index >= verts.len() {
        return None;
    }
    let n = verts.len();
    let is_last = index == n - 1;

    if (is_last && !closed) || n == 1 {
        let start = verts[index];
        let previous_step = if n >= 2 { start.pos - verts[n - 2].pos } else { DVec3::ZERO };
        let delta = if previous_step.is_finite() && previous_step.length_squared() > f64::EPSILON {
            previous_step
        } else {
            NO_DIRECTION_STEP
        };
        let new_index = index + 1;
        verts.insert(
            new_index,
            PolyVertex {
                pos: start.pos + delta,
                bulge: 0.0,
            },
        );
        return Some(new_index);
    }

    let start = verts[index];
    let end_index = (index + 1) % n;
    let end = verts[end_index];

    let (new_vertex, updated_start_bulge) = if start.bulge.abs() <= f64::EPSILON {
        (PolyVertex::straight((start.pos + end.pos) * 0.5), None)
    } else if let Some((center, start_vec, sweep, _radius)) = bulge_arc_parameters(start.pos, end.pos, start.bulge) {
        let half_bulge = (start.bulge.atan() * 0.5).tan();
        let half_angle = sweep * 0.5;
        let (sin, cos) = half_angle.sin_cos();
        let rotated = DVec2::new(start_vec.x * cos - start_vec.y * sin, start_vec.x * sin + start_vec.y * cos);
        let mid_xy = center + rotated;
        let mid_z = (start.pos.z + end.pos.z) * 0.5;
        (
            PolyVertex {
                pos: DVec3::new(mid_xy.x, mid_xy.y, mid_z),
                bulge: half_bulge,
            },
            Some(half_bulge),
        )
    } else {
        (PolyVertex::straight((start.pos + end.pos) * 0.5), None)
    };

    if let Some(bulge) = updated_start_bulge {
        verts[index].bulge = bulge;
    }
    let new_index = index + 1;
    verts.insert(new_index, new_vertex);
    Some(new_index)
}

/// Remove one vertex. Returns whether it applied.
pub(crate) fn delete_vertex(verts: &mut Vec<PolyVertex>, index: usize) -> bool {
    if index >= verts.len() {
        return false;
    }
    verts.remove(index);
    true
}

/// Swap a vertex with its neighbour. Returns the moved vertex's new row.
pub(crate) fn move_vertex(verts: &mut [PolyVertex], index: usize, up: bool) -> Option<usize> {
    if index >= verts.len() {
        return None;
    }
    let target = if up {
        index.checked_sub(1)?
    } else {
        let next = index + 1;
        if next >= verts.len() {
            return None;
        }
        next
    };
    verts.swap(index, target);
    Some(target)
}

/// Reverse vertex order, re-seating the bulges onto their new segments.
///
/// After reversal `v'[j] = v[n-1-j]`, so `bulge'[j] = -bulge[n-2-j]` - the
/// old segment `n-2-j` traversed backwards, wrapped for a closed ring. An
/// open polyline's final row has no outgoing segment left, so it goes straight.
pub(crate) fn reverse_vertices(verts: &mut [PolyVertex], closed: bool) {
    let n = verts.len();
    if n < 2 {
        return;
    }
    let original: Vec<PolyVertex> = verts.to_vec();
    for j in 0..n {
        let pos = original[n - 1 - j].pos;
        let bulge = if closed {
            let src = (n as i64 - 2 - j as i64).rem_euclid(n as i64) as usize;
            -original[src].bulge
        } else if j == n - 1 {
            0.0
        } else {
            let src = n - 2 - j;
            -original[src].bulge
        };
        verts[j] = PolyVertex { pos, bulge };
    }
}

/// Total length, following arcs rather than chords.
pub(crate) fn polyline_length(verts: &[PolyVertex], closed: bool) -> f64 {
    let n = verts.len();
    if n < 2 {
        return 0.0;
    }
    let edge_count = if closed { n } else { n - 1 };
    let mut total = 0.0;
    for i in 0..edge_count {
        let start = verts[i];
        let end = verts[(i + 1) % n];
        let contribution = if start.bulge.abs() <= f64::EPSILON {
            (end.pos - start.pos).length()
        } else if let Some((_center, _start_vec, sweep, radius)) = bulge_arc_parameters(start.pos, end.pos, start.bulge) {
            let dz = end.pos.z - start.pos.z;
            (radius * sweep.abs()).hypot(dz)
        } else {
            (end.pos - start.pos).length()
        };
        if contribution.is_finite() {
            total += contribution;
        }
    }
    total
}

/// Plan area of a closed polyline, `None` when it is open.
///
/// Arcs are tessellated first for true area, not the chord's; the shoelace
/// sum then runs about the first point as a local origin, since UTM-scale
/// coordinates would otherwise cancel catastrophically under `f64`.
pub(crate) fn polyline_area_xy(verts: &[PolyVertex], closed: bool) -> Option<f64> {
    if !closed {
        return None;
    }
    let points = tessellate_polyline_bulges(verts, true);
    if points.len() < 3 {
        return None;
    }
    let origin = points[0];
    let mut twice_area = 0.0;
    for index in 0..points.len() {
        let a = points[index] - origin;
        let b = points[(index + 1) % points.len()] - origin;
        twice_area += a.x * b.y - b.x * a.y;
    }
    Some((twice_area * 0.5).abs())
}

/// Has the document moved on since the editor took its working copy?
///
/// `baseline` is the object as the document held it when editing started, or
/// as of the last Apply; writing back over a later change would silently revert it.
pub(crate) fn drift_detected(baseline: &Object, current: &Object) -> bool {
    baseline != current
}

/// Why a working copy cannot be written back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObjectEditIssue {
    /// The numbered row's position or bulge is not a finite number.
    NonFiniteVertex(usize),
    /// A scalar property (text height, rotation, line weight) is not finite.
    NonFiniteValue,
    /// Fewer vertices than the kind allows.
    TooFewVertices { required: usize },
}

/// Check a working copy before it is handed back to the document.
pub(crate) fn validate_object(object: &Object) -> Option<ObjectEditIssue> {
    match object {
        Object::Point { pos, .. } => {
            if !pos.is_finite() {
                return Some(ObjectEditIssue::NonFiniteVertex(0));
            }
            None
        }
        Object::Polyline { verts, line_weight, .. } => {
            if verts.len() < 2 {
                return Some(ObjectEditIssue::TooFewVertices { required: 2 });
            }
            for (row, vertex) in verts.iter().enumerate() {
                if !vertex.pos.is_finite() || !vertex.bulge.is_finite() {
                    return Some(ObjectEditIssue::NonFiniteVertex(row));
                }
            }
            if !line_weight.is_finite() {
                return Some(ObjectEditIssue::NonFiniteValue);
            }
            None
        }
        Object::Text { pos, height, rotation, .. } => {
            if !pos.is_finite() {
                return Some(ObjectEditIssue::NonFiniteVertex(0));
            }
            if !height.is_finite() || *height <= 0.0 || !rotation.is_finite() {
                return Some(ObjectEditIssue::NonFiniteValue);
            }
            None
        }
    }
}
