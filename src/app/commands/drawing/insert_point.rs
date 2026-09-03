use std::{
    collections::hash_map::DefaultHasher,
    f64::consts::TAU,
    hash::{Hash, Hasher},
};

use glam::{DVec2, DVec3};

use crate::{
    app::App,
    i18n::{tr, tr_format},
    logging::CommandReportSpec,
    model::{
        Command, Object, ObjectId, PolyVertex, SceneEntityId,
        kernel::{self, SegSeg, XY_TOL},
    },
    userspace_warn,
};

#[derive(Clone, Copy, Debug)]
enum Curve {
    Line { start: DVec2, end: DVec2 },
    Arc { center: DVec2, radius: f64, start_angle: f64, sweep: f64 },
}

impl Curve {
    fn from_vertices(start: PolyVertex, end: PolyVertex) -> Self {
        let a = start.pos.truncate();
        let b = end.pos.truncate();
        let chord = b - a;
        let chord_len = chord.length();
        if start.bulge.abs() <= f64::EPSILON || chord_len <= f64::EPSILON {
            return Self::Line { start: a, end: b };
        }

        let center = (a + b) * 0.5 + DVec2::new(-chord.y, chord.x) * (1.0 - start.bulge * start.bulge) / (4.0 * start.bulge);
        Self::Arc {
            center,
            radius: center.distance(a),
            start_angle: (a - center).y.atan2((a - center).x),
            sweep: 4.0 * start.bulge.atan(),
        }
    }

    fn point(self, t: f64) -> DVec2 {
        match self {
            Self::Line { start, end } => start.lerp(end, t),
            Self::Arc {
                center,
                radius,
                start_angle,
                sweep,
            } => {
                let angle = start_angle + sweep * t;
                center + radius * DVec2::new(angle.cos(), angle.sin())
            }
        }
    }

    fn length(self) -> f64 {
        match self {
            Self::Line { start, end } => start.distance(end),
            Self::Arc { radius, sweep, .. } => radius * sweep.abs(),
        }
    }

    fn parameter_for_point(self, point: DVec2) -> Option<f64> {
        match self {
            Self::Line { start, end } => {
                let (_, t) = kernel::project_onto_segment(point, start, end);
                Some(t)
            }
            Self::Arc {
                center,
                radius,
                start_angle,
                sweep,
            } => {
                let angle = (point - center).y.atan2((point - center).x);
                let travelled = if sweep >= 0.0 {
                    (angle - start_angle).rem_euclid(TAU)
                } else {
                    (start_angle - angle).rem_euclid(TAU)
                };
                let extent = sweep.abs();
                let angular_tol = XY_TOL / radius.max(XY_TOL);
                (travelled <= extent + angular_tol).then_some((travelled / extent).clamp(0.0, 1.0))
            }
        }
    }
}

impl<'a> App<'a> {
    pub(crate) fn insert_points_at_selected_intersections(&mut self) {
        let selected = self.selected_polylines();
        if selected.len() < 2 {
            userspace_warn!("{}", tr!(literal = "Select at least two polylines before inserting intersection points"));
            return;
        }

        let source: Vec<(ObjectId, Vec<PolyVertex>, bool)> = selected
            .into_iter()
            .filter_map(|id| match self.scene_document.get_object(id) {
                Some(Object::Polyline { verts, closed, .. }) => Some((id, verts.clone(), *closed)),
                _ => None,
            })
            .collect();
        let shapes: Vec<(Vec<PolyVertex>, bool)> = source.iter().map(|(_, verts, closed)| (verts.clone(), *closed)).collect();
        let updated = insert_at_intersections(&shapes);
        self.commit_inserted_vertices(&source, updated, &tr!(literal = "Intersection"));
    }

    /// Refresh the menu hint for Insert Point > At intersection.
    ///
    /// Menus need the answer every frame, so the scan result is cached against
    /// the selection and the workspace composite key. Selections whose pairwise
    /// edge count exceeds the scan budget report as available rather than
    /// stalling the frame; the insert itself then reports finding nothing.
    pub(crate) fn refresh_intersection_availability(&mut self) {
        let key = {
            // Order-insensitive fold over the selected object ids, matching the
            // allocation-free style of `ProjectStore::composite_key`.
            let (mut fold, mut count) = (0_u64, 0_u64);
            for handle in &self.editor.selected_handles {
                if let SceneEntityId::Object(id) = handle {
                    fold ^= id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    count += 1;
                }
            }
            let mut hasher = DefaultHasher::new();
            self.workspace.composite_key().hash(&mut hasher);
            fold.hash(&mut hasher);
            count.hash(&mut hasher);
            hasher.finish()
        };
        if self.intersection_availability_key == Some(key) {
            return;
        }
        self.intersection_availability_key = Some(key);

        let selected = self.selected_polylines();
        let shapes: Vec<(&[PolyVertex], bool)> = selected
            .iter()
            .filter_map(|id| match self.scene_document.get_object(*id) {
                Some(Object::Polyline { verts, closed, .. }) => Some((verts.as_slice(), *closed)),
                _ => None,
            })
            .collect();
        let available = any_insertable_intersection(&shapes);
        self.editor.selection_has_intersections = available;
    }

    pub(crate) fn open_insert_point_at_elevation_dialog(&mut self) {
        let object_ids = self.selected_polylines();
        if object_ids.is_empty() {
            userspace_warn!("{}", tr!(literal = "Select one or more polylines before inserting a point at elevation"));
            return;
        }
        // Only elevations the selection actually spans can produce a point, so
        // the dialog's entry box is bounded to that band.
        let (mut min_elevation, mut max_elevation) = (f64::INFINITY, f64::NEG_INFINITY);
        for id in &object_ids {
            let Some(Object::Polyline { verts, .. }) = self.scene_document.get_object(*id) else {
                continue;
            };
            for vertex in verts {
                min_elevation = min_elevation.min(vertex.pos.z);
                max_elevation = max_elevation.max(vertex.pos.z);
            }
        }
        if !(min_elevation.is_finite() && max_elevation.is_finite()) {
            (min_elevation, max_elevation) = (f64::MIN, f64::MAX);
        }
        self.editor.insert_point_at_elevation_dialog = Some(crate::ui::dialogs::InsertPointAtElevationDialog {
            object_ids,
            elevation: self.editor.z_input.clamp(min_elevation, max_elevation),
            min_elevation,
            max_elevation,
        });
    }

    pub(crate) fn insert_points_at_elevation(&mut self, object_ids: Vec<ObjectId>, elevation: f64) {
        if !elevation.is_finite() {
            userspace_warn!("{}", tr!(literal = "Insert Point at Elevation requires a finite elevation"));
            return;
        }
        let source: Vec<(ObjectId, Vec<PolyVertex>, bool)> = object_ids
            .into_iter()
            .filter_map(|id| match self.scene_document.get_object(id) {
                Some(Object::Polyline { verts, closed, .. }) => Some((id, verts.clone(), *closed)),
                _ => None,
            })
            .collect();
        let updated = source.iter().map(|(_, verts, closed)| insert_at_elevation(verts, *closed, elevation)).collect();
        self.commit_inserted_vertices(&source, updated, &tr!(literal = "Elevation"));
    }

    fn selected_polylines(&self) -> Vec<ObjectId> {
        self.editor
            .selected_handles
            .iter()
            .filter_map(|handle| match handle {
                SceneEntityId::Object(id) if matches!(self.scene_document.get_object(*id), Some(Object::Polyline { .. })) => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn commit_inserted_vertices(&mut self, source: &[(ObjectId, Vec<PolyVertex>, bool)], updated: Vec<Vec<PolyVertex>>, operation: &str) {
        let Some(project) = self.workspace.active_project_mut() else {
            return;
        };
        let doc = &mut project.project.document;
        let mut inserted = 0usize;
        let commands: Vec<Command> = source
            .iter()
            .zip(updated)
            .filter_map(|((id, old_verts, _), new_verts)| {
                if new_verts.len() == old_verts.len() {
                    return None;
                }
                let before = doc.get_object(*id)?.clone();
                let mut after = before.clone();
                let Object::Polyline { verts, .. } = &mut after else {
                    return None;
                };
                inserted += new_verts.len() - verts.len();
                *verts = new_verts;
                Some(Command::Replace { before, after })
            })
            .collect();

        if commands.is_empty() {
            userspace_warn!("{}", tr_format!(literal = "No new %operation% points were found", operation = operation));
            return;
        }
        self.history.execute(doc, Command::Batch(commands));
        crate::logging::report_completed_action(
            CommandReportSpec::new(
                crate::i18n::tr!(literal = "Insert Points"),
                crate::i18n::tr_format!(literal = "%count% %operation% point(s)", count = inserted, operation = operation),
            ),
            crate::i18n::tr_format!(literal = "Inserted %count% %operation% point(s)", count = inserted, operation = operation),
        );
        self.invalidate_geometry();
    }
}

fn insert_at_intersections(polylines: &[(Vec<PolyVertex>, bool)]) -> Vec<Vec<PolyVertex>> {
    let mut parameters: Vec<Vec<Vec<f64>>> = polylines.iter().map(|(verts, closed)| vec![Vec::new(); edge_count(verts, *closed)]).collect();

    for first in 0..polylines.len() {
        for second in first + 1..polylines.len() {
            let (a_verts, a_closed) = &polylines[first];
            let (b_verts, b_closed) = &polylines[second];
            for a_edge in 0..edge_count(a_verts, *a_closed) {
                let a_next = (a_edge + 1) % a_verts.len();
                let a_curve = Curve::from_vertices(a_verts[a_edge], a_verts[a_next]);
                for b_edge in 0..edge_count(b_verts, *b_closed) {
                    let b_next = (b_edge + 1) % b_verts.len();
                    let b_curve = Curve::from_vertices(b_verts[b_edge], b_verts[b_next]);
                    for (_, a_t, b_t) in curve_intersections(a_curve, b_curve) {
                        push_interior_parameter(&mut parameters[first][a_edge], a_curve, a_t);
                        push_interior_parameter(&mut parameters[second][b_edge], b_curve, b_t);
                    }
                }
            }
        }
    }

    polylines
        .iter()
        .zip(parameters)
        .map(|((verts, closed), params)| insert_parameters(verts, *closed, params))
        .collect()
}

fn insert_at_elevation(verts: &[PolyVertex], closed: bool, elevation: f64) -> Vec<PolyVertex> {
    let mut parameters = vec![Vec::new(); edge_count(verts, closed)];
    for edge in 0..parameters.len() {
        let next = (edge + 1) % verts.len();
        let start_z = verts[edge].pos.z;
        let end_z = verts[next].pos.z;
        let dz = end_z - start_z;
        // A horizontal segment has either no hit or infinitely many hits. In both
        // cases there is no unique point to insert, so it is deliberately ignored.
        if dz.abs() <= f64::EPSILON {
            continue;
        }
        let t = (elevation - start_z) / dz;
        if t > f64::EPSILON && t < 1.0 - f64::EPSILON {
            parameters[edge].push(t);
        }
    }
    insert_parameters(verts, closed, parameters)
}

fn edge_count(verts: &[PolyVertex], closed: bool) -> usize {
    if verts.len() < 2 {
        0
    } else if closed {
        verts.len()
    } else {
        verts.len() - 1
    }
}

/// True when `t` sits far enough inside `curve` to become a new vertex rather
/// than duplicating one of the edge's endpoints.
fn is_interior_parameter(curve: Curve, t: f64) -> bool {
    let length = curve.length();
    length > 2.0 * XY_TOL && t * length > XY_TOL && (1.0 - t) * length > XY_TOL
}

fn push_interior_parameter(parameters: &mut Vec<f64>, curve: Curve, t: f64) {
    if !is_interior_parameter(curve, t) {
        return;
    }
    let length = curve.length();
    if parameters.iter().all(|existing| (existing - t).abs() * length > XY_TOL) {
        parameters.push(t);
    }
}

/// Pairwise edge tests the availability scan will run before giving up and
/// reporting the action as available.
const INTERSECTION_SCAN_BUDGET: u64 = 4_000_000;

/// True when at least one crossing among `polylines` would insert a vertex.
///
/// Mirrors the filtering in [`insert_at_intersections`] so the menu never
/// offers an action that would report "no new intersection points", but exits
/// on the first hit instead of collecting them all.
fn any_insertable_intersection(polylines: &[(&[PolyVertex], bool)]) -> bool {
    if polylines.len() < 2 {
        return false;
    }
    let edges: Vec<u64> = polylines.iter().map(|(verts, closed)| edge_count(verts, *closed) as u64).collect();
    // Σ(i<j) Ei·Ej, without walking the pairs.
    let total: u64 = edges.iter().sum();
    let pairs = total
        .saturating_mul(total)
        .saturating_sub(edges.iter().map(|count| count.saturating_mul(*count)).sum::<u64>())
        / 2;
    if pairs > INTERSECTION_SCAN_BUDGET {
        return true;
    }

    let bounds: Vec<Option<(DVec2, DVec2)>> = polylines.iter().map(|(verts, closed)| plan_bounds(verts, *closed)).collect();
    for first in 0..polylines.len() {
        for second in first + 1..polylines.len() {
            let (Some(a_bounds), Some(b_bounds)) = (bounds[first], bounds[second]) else {
                continue;
            };
            if !bounds_overlap(a_bounds, b_bounds) {
                continue;
            }
            let (a_verts, a_closed) = polylines[first];
            let (b_verts, b_closed) = polylines[second];
            for a_edge in 0..edge_count(a_verts, a_closed) {
                let a_curve = Curve::from_vertices(a_verts[a_edge], a_verts[(a_edge + 1) % a_verts.len()]);
                for b_edge in 0..edge_count(b_verts, b_closed) {
                    let b_curve = Curve::from_vertices(b_verts[b_edge], b_verts[(b_edge + 1) % b_verts.len()]);
                    for (_, a_t, b_t) in curve_intersections(a_curve, b_curve) {
                        if is_interior_parameter(a_curve, a_t) || is_interior_parameter(b_curve, b_t) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Plan-view bounds of a polyline, grown by the deepest arc sagitta so a
/// bulged edge can never bow outside the box.
fn plan_bounds(verts: &[PolyVertex], closed: bool) -> Option<(DVec2, DVec2)> {
    let edges = edge_count(verts, closed);
    if edges == 0 {
        return None;
    }
    let mut min = DVec2::splat(f64::INFINITY);
    let mut max = DVec2::splat(f64::NEG_INFINITY);
    let mut sagitta = 0.0_f64;
    for vertex in verts {
        min = min.min(vertex.pos.truncate());
        max = max.max(vertex.pos.truncate());
    }
    for edge in 0..edges {
        let chord = verts[(edge + 1) % verts.len()].pos.truncate().distance(verts[edge].pos.truncate());
        sagitta = sagitta.max(verts[edge].bulge.abs() * chord * 0.5);
    }
    let margin = DVec2::splat(sagitta + XY_TOL);
    Some((min - margin, max + margin))
}

fn bounds_overlap(first: (DVec2, DVec2), second: (DVec2, DVec2)) -> bool {
    let ((a_min, a_max), (b_min, b_max)) = (first, second);
    a_min.x <= b_max.x && b_min.x <= a_max.x && a_min.y <= b_max.y && b_min.y <= a_max.y
}

fn insert_parameters(verts: &[PolyVertex], closed: bool, mut parameters: Vec<Vec<f64>>) -> Vec<PolyVertex> {
    if edge_count(verts, closed) == 0 {
        return verts.to_vec();
    }
    let extra: usize = parameters.iter().map(Vec::len).sum();
    let mut result = Vec::with_capacity(verts.len() + extra);
    for edge in 0..parameters.len() {
        let next = (edge + 1) % verts.len();
        let start = verts[edge];
        let end = verts[next];
        let curve = Curve::from_vertices(start, end);
        let edge_parameters = &mut parameters[edge];
        edge_parameters.sort_by(f64::total_cmp);

        let first_fraction = edge_parameters.first().copied().unwrap_or(1.0);
        let mut original = start;
        original.bulge = split_bulge(start.bulge, first_fraction);
        result.push(original);

        for (index, &t) in edge_parameters.iter().enumerate() {
            let next_t = edge_parameters.get(index + 1).copied().unwrap_or(1.0);
            let xy = curve.point(t);
            result.push(PolyVertex {
                pos: DVec3::new(xy.x, xy.y, start.pos.z + (end.pos.z - start.pos.z) * t),
                bulge: split_bulge(start.bulge, next_t - t),
            });
        }
    }
    if !closed && let Some(last) = verts.last().copied() {
        result.push(last);
    }
    result
}

fn split_bulge(bulge: f64, fraction: f64) -> f64 {
    (bulge.atan() * fraction).tan()
}

fn curve_intersections(first: Curve, second: Curve) -> Vec<(DVec2, f64, f64)> {
    match (first, second) {
        (Curve::Line { start: a, end: b }, Curve::Line { start: c, end: d }) => match kernel::segment_segment(a, b, c, d) {
            SegSeg::Crossing { point, t, u } | SegSeg::Touching { point, t, u } => {
                vec![(point, t, u)]
            }
            SegSeg::Disjoint | SegSeg::CollinearOverlap { .. } => Vec::new(),
        },
        (line @ Curve::Line { .. }, arc @ Curve::Arc { .. }) => line_arc_intersections(line, arc),
        (arc @ Curve::Arc { .. }, line @ Curve::Line { .. }) => line_arc_intersections(line, arc).into_iter().map(|(point, line_t, arc_t)| (point, arc_t, line_t)).collect(),
        (first @ Curve::Arc { .. }, second @ Curve::Arc { .. }) => arc_arc_intersections(first, second),
    }
}

fn line_arc_intersections(line: Curve, arc: Curve) -> Vec<(DVec2, f64, f64)> {
    let (Curve::Line { start, end }, Curve::Arc { center, radius, .. }) = (line, arc) else {
        return Vec::new();
    };
    let direction = end - start;
    let a = direction.length_squared();
    if a <= f64::EPSILON {
        return Vec::new();
    }
    let offset = start - center;
    let b = 2.0 * offset.dot(direction);
    let c = offset.length_squared() - radius * radius;
    let discriminant = b * b - 4.0 * a * c;
    let disc_tol = 4.0 * a * radius * XY_TOL;
    if discriminant < -disc_tol {
        return Vec::new();
    }
    let root = discriminant.max(0.0).sqrt();
    let mut result = Vec::new();
    for line_t in [(-b - root) / (2.0 * a), (-b + root) / (2.0 * a)] {
        if !(-XY_TOL / a.sqrt()..=1.0 + XY_TOL / a.sqrt()).contains(&line_t) {
            continue;
        }
        let line_t = line_t.clamp(0.0, 1.0);
        let point = start + direction * line_t;
        if let Some(arc_t) = arc.parameter_for_point(point)
            && result.iter().all(|(existing, ..): &(DVec2, f64, f64)| existing.distance(point) > XY_TOL)
        {
            result.push((point, line_t, arc_t));
        }
    }
    result
}

fn arc_arc_intersections(first: Curve, second: Curve) -> Vec<(DVec2, f64, f64)> {
    let (Curve::Arc { center: c0, radius: r0, .. }, Curve::Arc { center: c1, radius: r1, .. }) = (first, second) else {
        return Vec::new();
    };
    let delta = c1 - c0;
    let distance = delta.length();
    // Coincident arcs have no unique intersection points.
    if distance <= XY_TOL && (r0 - r1).abs() <= XY_TOL {
        return Vec::new();
    }
    if distance <= f64::EPSILON || distance > r0 + r1 + XY_TOL || distance < (r0 - r1).abs() - XY_TOL {
        return Vec::new();
    }
    let along = (r0 * r0 - r1 * r1 + distance * distance) / (2.0 * distance);
    let height_sq = (r0 * r0 - along * along).max(0.0);
    let base = c0 + delta * (along / distance);
    let perpendicular = DVec2::new(-delta.y, delta.x) / distance;
    let height = height_sq.sqrt();
    let mut result = Vec::new();
    for point in [base + perpendicular * height, base - perpendicular * height] {
        let (Some(first_t), Some(second_t)) = (first.parameter_for_point(point), second.parameter_for_point(point)) else {
            continue;
        };
        if result.iter().all(|(existing, ..): &(DVec2, f64, f64)| existing.distance(point) > XY_TOL) {
            result.push((point, first_t, second_t));
        }
    }
    result
}
