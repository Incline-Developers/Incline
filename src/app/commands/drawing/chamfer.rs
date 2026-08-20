use std::f64::consts::TAU;

use glam::DVec3;

use crate::{
    app::{App, PICK_THRESHOLD_PX},
    logging::CommandReportSpec,
    model::{Command, Object, ObjectPoint, PolyVertex},
    rendering::pick,
    ui::state::ActiveTool,
};

impl<'a> App<'a> {
    /// Called when the user clicks while the Chamfer tool is active.
    /// Picks the nearest closed-polygon vertex within the threshold and stores it.
    pub(crate) fn pick_chamfer_corner(&mut self) {
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        let Some(cursor_px) = self.editor.cursor_screen_px else {
            return;
        };

        let result = pick::pick_nearest_vertex(
            &self.scene_document,
            &self.editor.hidden_handles,
            &self.editor.frozen_handles,
            &graphics.view_proj(),
            graphics.screen_size_pub(),
            cursor_px,
            PICK_THRESHOLD_PX * 2.5,
        );

        let Some((oid, ObjectPoint::Vertex(vi), _world)) = result else {
            return;
        };
        if !self.activate_project_for_object(oid) {
            return;
        }

        // Only accept real closed polygons. Modifying tools are not restricted
        // to the active layer.
        if !self
            .active_document()
            .get_object(oid)
            .is_some_and(|o| matches!(o, Object::Polyline { closed: true, verts, .. } if verts.len() >= 3))
        {
            return;
        }

        self.editor.chamfer_poly_id = Some(oid);
        self.editor.chamfer_corner_index = Some(vi);
        self.editor.chamfer_gizmo_drag_start_px = None;
        self.invalidate_overlay();
    }

    pub(crate) fn apply_chamfer(&mut self) {
        let (Some(oid), Some(ci)) = (self.editor.chamfer_poly_id, self.editor.chamfer_corner_index) else {
            return;
        };
        if !self.activate_project_for_object(oid) {
            return;
        }
        let Some(project) = self.workspace.active_project_mut() else {
            return;
        };
        let doc = &mut project.pidb.document;
        let Some(obj) = doc.get_object(oid) else {
            return;
        };
        let Object::Polyline { verts, closed: true, .. } = obj else {
            return;
        };
        let before = obj.clone();
        let new_verts = chamfer_corner(verts, ci, self.editor.chamfer_radius, self.editor.chamfer_segments);
        let mut after = before.clone();
        if let Object::Polyline { verts, .. } = &mut after {
            *verts = new_verts;
        }
        if before != after {
            self.history.execute(doc, Command::Replace { before, after });
            crate::logging::report_completed_action(
                CommandReportSpec::new("Chamfer", format!("Radius {:.3}", self.editor.chamfer_radius)),
                format!(
                    "Chamfered corner {} with radius {:.3} and {} segments",
                    ci, self.editor.chamfer_radius, self.editor.chamfer_segments
                ),
            );
        }
        self.editor.chamfer_poly_id = None;
        self.editor.chamfer_corner_index = None;
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    pub(crate) fn cancel_chamfer(&mut self) {
        self.editor.chamfer_poly_id = None;
        self.editor.chamfer_corner_index = None;
        self.editor.chamfer_gizmo_drag_start_px = None;
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_overlay();
    }
}

/// Compute the maximum chamfer radius for a given corner without distorting
/// adjacent edges. Returns `f64::MAX` if there is no geometric limit.
pub(crate) fn chamfer_max_radius(verts: &[PolyVertex], corner_index: usize) -> f64 {
    let n = verts.len();
    if n < 3 || corner_index >= n {
        return f64::MAX;
    }
    let v = verts[corner_index].pos;
    let prev_i = (corner_index + n - 1) % n;
    let next_i = (corner_index + 1) % n;
    let a = verts[prev_i].pos;
    let b = verts[next_i].pos;
    let e1_2d = glam::DVec2::new(a.x - v.x, a.y - v.y);
    let e2_2d = glam::DVec2::new(b.x - v.x, b.y - v.y);
    let e1_len = e1_2d.length();
    let e2_len = e2_2d.length();
    if e1_len < 1e-10 || e2_len < 1e-10 {
        return f64::MAX;
    }
    let e1 = e1_2d / e1_len;
    let e2 = e2_2d / e2_len;
    let cross = e1.x * e2.y - e1.y * e2.x;
    if cross.abs() < 1e-6 {
        return f64::MAX;
    }
    let bis = e1 + e2;
    let bis_len = bis.length();
    if bis_len < 1e-10 {
        return f64::MAX;
    }
    let bisector = bis / bis_len;
    let sin_half = (e1.x * bisector.y - e1.y * bisector.x).abs();
    let cos_half = e1.dot(bisector);
    if sin_half < 1e-6 || cos_half.abs() < 1e-10 {
        return f64::MAX;
    }
    let max_tan = (e1_len * 0.499).min(e2_len * 0.499);
    max_tan * sin_half / cos_half
}

/// Replace a single corner of a closed polygon with a circular arc.
///
/// All other vertices are returned unchanged. Z is linearly interpolated along edges.
pub(crate) fn chamfer_corner(verts: &[PolyVertex], corner_index: usize, radius: f64, segments: u32) -> Vec<PolyVertex> {
    let n = verts.len();
    if n < 3 || radius <= 1e-12 || corner_index >= n {
        return verts.to_vec();
    }

    let segments = segments.max(1) as usize;
    let positions: Vec<DVec3> = verts.iter().map(|v| v.pos).collect();
    let mut result: Vec<PolyVertex> = Vec::with_capacity(n + segments);

    for i in 0..n {
        if i != corner_index {
            result.push(verts[i]);
            continue;
        }

        let prev_i = (i + n - 1) % n;
        let next_i = (i + 1) % n;

        let v = positions[i];
        let a = positions[prev_i];
        let b = positions[next_i];

        let e1_2d = glam::DVec2::new(a.x - v.x, a.y - v.y);
        let e2_2d = glam::DVec2::new(b.x - v.x, b.y - v.y);
        let e1_len = e1_2d.length();
        let e2_len = e2_2d.length();

        if e1_len < 1e-10 || e2_len < 1e-10 {
            result.push(verts[i]);
            continue;
        }

        let e1 = e1_2d / e1_len;
        let e2 = e2_2d / e2_len;

        // Near-straight edge: nothing to round
        if (e1.x * e2.y - e1.y * e2.x).abs() < 1e-6 {
            result.push(verts[i]);
            continue;
        }

        let bis = e1 + e2;
        let bis_len = bis.length();
        if bis_len < 1e-10 {
            result.push(verts[i]);
            continue;
        }
        let bisector = bis / bis_len;

        let sin_half = (e1.x * bisector.y - e1.y * bisector.x).abs();
        let cos_half = e1.dot(bisector);
        if sin_half < 1e-6 {
            result.push(verts[i]);
            continue;
        }

        // Clamp radius so tangent points stay within each edge (leave half the edge free)
        let max_tan = (e1_len * 0.499).min(e2_len * 0.499);
        let r = if cos_half.abs() < 1e-10 { radius } else { (max_tan * sin_half / cos_half).min(radius) }.max(1e-10);

        let tan_len = r * cos_half / sin_half;
        let t1_frac = tan_len / e1_len;
        let t2_frac = tan_len / e2_len;
        let t1 = v + (a - v) * t1_frac;
        let t2 = v + (b - v) * t2_frac;

        let center_dist = r / sin_half;
        let cx = v.x + bisector.x * center_dist;
        let cy = v.y + bisector.y * center_dist;

        let a1 = (t1.y - cy).atan2(t1.x - cx);
        let a2 = (t2.y - cy).atan2(t2.x - cx);
        let av = (v.y - cy).atan2(v.x - cx);

        let ccw_range = (a2 - a1).rem_euclid(TAU);
        let av_ccw = (av - a1).rem_euclid(TAU);
        let sweep = if av_ccw < ccw_range { ccw_range } else { -(TAU - ccw_range) };

        for j in 0..=segments {
            let t = j as f64 / segments as f64;
            let angle = a1 + t * sweep;
            let x = cx + angle.cos() * r;
            let y = cy + angle.sin() * r;
            let z = t1.z + t * (t2.z - t1.z);
            result.push(PolyVertex::straight(DVec3::new(x, y, z)));
        }
    }

    result
}
