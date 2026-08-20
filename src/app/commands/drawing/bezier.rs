use glam::DVec3;

use crate::{
    app::{App, PICK_THRESHOLD_PX},
    logging::CommandReportSpec,
    model::{Command, Object, PolyVertex, SceneEntityId, geometry::tessellate_bulge_segment},
    rendering::pick,
    ui::state::ActiveTool,
};

impl<'a> App<'a> {
    /// Dispatch a canvas click for the Bezier tool.
    pub(crate) fn bezier_click(&mut self) {
        if self.editor.bezier_poly_id.is_none() {
            self.pick_bezier_polygon();
        } else if self.editor.bezier_selected_verts[0].is_none() || self.editor.bezier_selected_verts[1].is_none() {
            self.pick_bezier_vertex();
        }
        // Both verts selected: user interacts through the panel/gizmo only
    }

    fn pick_bezier_polygon(&mut self) {
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));

        let Some((SceneEntityId::Object(oid), _)) = picked else {
            return;
        };
        if !self.activate_project_for_object(oid) {
            return;
        }

        let Some((closed, vertex_count)) = self.active_document().get_object(oid).and_then(|obj| {
            let Object::Polyline { verts, closed, .. } = obj else {
                return None;
            };
            Some((*closed, verts.len()))
        }) else {
            return;
        };
        if (closed && vertex_count < 3) || (!closed && vertex_count < 2) {
            return;
        }

        self.editor.bezier_poly_id = Some(oid);
        self.editor.bezier_poly_closed = closed;
        self.editor.bezier_selected_verts = [None; 2];
        self.editor.bezier_replace_longer = false;
        self.editor.bezier_dialog_open = true;
        self.invalidate_overlay();
    }

    fn pick_bezier_vertex(&mut self) {
        let Some(oid) = self.editor.bezier_poly_id else {
            return;
        };
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        let Some(cursor_px) = self.editor.cursor_screen_px else {
            return;
        };

        // Find the nearest vertex of the selected polyline only.
        let vp = graphics.view_proj();
        let screen = graphics.screen_size_pub();
        let (verts, closed) = match self.scene_document.get_object(oid) {
            Some(Object::Polyline { verts, closed, .. }) => (verts.clone(), *closed),
            _ => return,
        };

        let cursor_d = glam::DVec2::new(f64::from(cursor_px.0), f64::from(cursor_px.1));
        let mut best_dist = (PICK_THRESHOLD_PX * 2.5) as f64;
        let mut best_idx: Option<usize> = None;

        for (i, vert) in verts.iter().enumerate() {
            if let Some(sp) = pick::world_to_screen(&vp, vert.pos, screen) {
                let d = sp.distance(cursor_d);
                if d < best_dist {
                    best_dist = d;
                    best_idx = Some(i);
                }
            }
        }

        let Some(vi) = best_idx else {
            return;
        };

        if self.editor.bezier_selected_verts[0].is_none() {
            // Picking the first vertex
            self.editor.bezier_selected_verts[0] = Some(vi);
            self.invalidate_overlay();
        } else {
            let first = self.editor.bezier_selected_verts[0].unwrap();
            if vi == first {
                return;
            }

            let (start, end) = if closed {
                choose_closed_span(&verts, first, vi, false)
            } else {
                (first.min(vi), first.max(vi))
            };

            self.editor.bezier_selected_verts[0] = Some(start);
            self.editor.bezier_selected_verts[1] = Some(end);
            self.editor.bezier_replace_longer = false;

            // Initialise control points at 1/3 and 2/3 along the chord.
            let v0 = verts[start].pos;
            let v1 = verts[end].pos;
            let cp1 = v0 + (v1 - v0) * (1.0 / 3.0);
            let cp2 = v0 + (v1 - v0) * (2.0 / 3.0);
            self.editor.bezier_cp1 = [cp1.x, cp1.y, cp1.z];
            self.editor.bezier_cp2 = [cp2.x, cp2.y, cp2.z];
            self.invalidate_overlay();
        }
    }

    pub(crate) fn apply_bezier(&mut self) {
        let (Some(oid), [Some(vi), Some(vj)]) = (self.editor.bezier_poly_id, self.editor.bezier_selected_verts) else {
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
        let Object::Polyline { verts, closed, .. } = obj else {
            return;
        };

        let before = obj.clone();
        let verts = verts.clone();
        let closed = *closed;

        let cp1 = DVec3::from(self.editor.bezier_cp1);
        let cp2 = DVec3::from(self.editor.bezier_cp2);
        let segments = self.editor.bezier_segments.max(2) as usize;

        // The object can change while the panel is open (for example via an
        // external command). Ignore stale indices rather than indexing the
        // ring and panicking.
        let Some(new_verts) = replace_polyline_span_with_bezier(&verts, closed, vi, vj, cp1, cp2, segments) else {
            return;
        };

        let mut after = before.clone();
        if let Object::Polyline { verts: after_verts, .. } = &mut after {
            *after_verts = new_verts;
        }

        if before != after {
            self.history.execute(doc, Command::Replace { before, after });
            crate::logging::report_completed_action(
                CommandReportSpec::new("Create Bezier Curve", format!("Vertices {vi} to {vj}")),
                format!("Replaced polyline span {vi}→{vj} with {} sampled intermediate points", segments - 1),
            );
        }

        self.clear_bezier_state();
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    pub(crate) fn cancel_bezier(&mut self) {
        self.clear_bezier_state();
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_overlay();
    }

    pub(crate) fn clear_bezier_state(&mut self) {
        self.editor.bezier_poly_id = None;
        self.editor.bezier_poly_closed = false;
        self.editor.bezier_selected_verts = [None; 2];
        self.editor.bezier_replace_longer = false;
        self.editor.bezier_cp1 = [0.0; 3];
        self.editor.bezier_cp2 = [0.0; 3];
        self.editor.bezier_poly_verts_screen_px.clear();
        self.editor.bezier_cp1_screen_px = None;
        self.editor.bezier_cp2_screen_px = None;
        self.editor.bezier_span_screen_px.clear();
        self.editor.bezier_preview_screen_px.clear();
        self.editor.bezier_dragging_cp = None;
        self.editor.bezier_hover_cp = None;
        self.editor.bezier_dialog_open = false;
    }
}

/// Evaluate a cubic bezier at parameter `t` in [0, 1].
pub(crate) fn bezier_eval(p0: DVec3, p1: DVec3, p2: DVec3, p3: DVec3, t: f64) -> DVec3 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

/// Pick one of the two directed paths between vertices of a closed polygon.
/// Length is measured along the actual source geometry, including bulged arcs.
fn choose_closed_span(verts: &[PolyVertex], first: usize, second: usize, longer: bool) -> (usize, usize) {
    let forward = directed_span_length(verts, true, first, second);
    let reverse = directed_span_length(verts, true, second, first);
    if (longer && forward >= reverse) || (!longer && forward <= reverse) {
        (first, second)
    } else {
        (second, first)
    }
}

fn directed_span_length(verts: &[PolyVertex], closed: bool, start: usize, end: usize) -> f64 {
    directed_span_points(verts, closed, start, end).windows(2).map(|pair| pair[0].distance(pair[1])).sum()
}

/// Points along a directed source span, including both endpoints and the
/// tessellation of any bulged edges.
pub(crate) fn directed_span_points(verts: &[PolyVertex], closed: bool, start: usize, end: usize) -> Vec<DVec3> {
    let vertex_count = verts.len();
    if start >= vertex_count || end >= vertex_count || start == end || (!closed && start > end) {
        return Vec::new();
    }

    let mut points = vec![verts[start].pos];
    let mut index = start;
    for _ in 0..vertex_count {
        if index == end {
            return points;
        }
        let next = if index + 1 < vertex_count {
            index + 1
        } else if closed {
            0
        } else {
            return Vec::new();
        };
        let edge = tessellate_bulge_segment(verts[index].pos, verts[next].pos, verts[index].bulge);
        points.extend(edge.into_iter().skip(1));
        index = next;
    }
    Vec::new()
}

/// Replace a directed span of either a closed polygon or an open polyline.
/// Open spans are stored in ascending index order because they have no
/// complementary route between their two anchors.
pub(crate) fn replace_polyline_span_with_bezier(
    verts: &[PolyVertex],
    closed: bool,
    start_index: usize,
    end_index: usize,
    cp1: DVec3,
    cp2: DVec3,
    segments: usize,
) -> Option<Vec<PolyVertex>> {
    if closed {
        return replace_polygon_span_with_bezier(verts, start_index, end_index, cp1, cp2, segments);
    }

    if verts.len() < 2 || start_index >= verts.len() || end_index >= verts.len() || start_index >= end_index || segments == 0 {
        return None;
    }

    let start = verts[start_index].pos;
    let end = verts[end_index].pos;
    let removed_edges = end_index - start_index;
    let mut result = Vec::with_capacity(verts.len() - removed_edges + segments);
    result.extend_from_slice(&verts[..=start_index]);
    result.last_mut().expect("the selected start vertex was retained").bulge = 0.0;
    append_bezier_interior(&mut result, start, cp1, cp2, end, segments);
    result.extend_from_slice(&verts[end_index..]);
    Some(result)
}

/// Replace the vertices strictly inside the directed `start_index`→`end_index`
/// span of a closed ring with straight-vertex samples of a cubic Bezier.
///
/// The start and end vertices are preserved. The start vertex's outgoing
/// bulge is cleared because that is the edge being replaced; the end vertex's
/// bulge and every bulge on the complementary span remain unchanged. A span
/// which crosses index zero may rotate the returned ring so the retained end
/// vertex remains the first element.
pub(crate) fn replace_polygon_span_with_bezier(verts: &[PolyVertex], start_index: usize, end_index: usize, cp1: DVec3, cp2: DVec3, segments: usize) -> Option<Vec<PolyVertex>> {
    let vertex_count = verts.len();
    if vertex_count < 2 || start_index >= vertex_count || end_index >= vertex_count || start_index == end_index || segments == 0 {
        return None;
    }

    let start = verts[start_index].pos;
    let end = verts[end_index].pos;
    let forward_edges = if end_index > start_index {
        end_index - start_index
    } else {
        vertex_count - start_index + end_index
    };
    let mut result = Vec::with_capacity(vertex_count - forward_edges + segments);

    if start_index < end_index {
        result.extend_from_slice(&verts[..=start_index]);
        result.last_mut().expect("the selected start vertex was retained").bulge = 0.0;
        append_bezier_interior(&mut result, start, cp1, cp2, end, segments);
        result.extend_from_slice(&verts[end_index..]);
    } else {
        // The replaced span crosses index zero. Start the rotated result at
        // the retained end anchor, keep the complementary span through the
        // start anchor, then close back to the end through the Bezier samples.
        result.extend_from_slice(&verts[end_index..=start_index]);
        result.last_mut().expect("the selected start vertex was retained").bulge = 0.0;
        append_bezier_interior(&mut result, start, cp1, cp2, end, segments);
    }

    Some(result)
}

fn append_bezier_interior(result: &mut Vec<PolyVertex>, start: DVec3, cp1: DVec3, cp2: DVec3, end: DVec3, segments: usize) {
    for segment in 1..segments {
        let t = segment as f64 / segments as f64;
        result.push(PolyVertex::straight(bezier_eval(start, cp1, cp2, end, t)));
    }
}
