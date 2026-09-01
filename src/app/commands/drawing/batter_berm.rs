use crate::{
    app::{App, PICK_THRESHOLD_PX},
    logging::CommandReportSpec,
    model::{Command, Object, PolyVertex, SceneEntityId},
    ui::state::{ActiveTool, BatterBermMode, BatterBermPreviewKey},
};

/// Bounds the maximum-depth search for open strings, which do not collapse
/// under a parallel offset. Closed pit/stockpile boundaries normally stop
/// naturally much earlier.
const BATTER_BERM_SEARCH_LIMIT: u32 = 4_096;

impl<'a> App<'a> {
    pub(crate) fn open_batter_berm_dialog(&mut self) {
        let object_id = self.editor.selected_handles.iter().find_map(|h| match h {
            SceneEntityId::Object(id)
                if self
                    .workspace
                    .active_document()
                    .and_then(|document| document.get_object(*id))
                    .is_some_and(|object| matches!(object, Object::Polyline { .. })) =>
            {
                Some(*id)
            }
            _ => None,
        });
        if let Some(id) = object_id {
            if !self.activate_project_for_object(id) {
                return;
            }
            self.editor.batter_berm_target_id = Some(id);
            self.editor.batter_berm_dialog_open = true;
            self.editor.batter_berm_preview_key = None;
            self.editor.tool_highlight_id = Some(id);
            let closed = matches!(self.active_document().get_object(id), Some(Object::Polyline { closed: true, .. }));
            self.editor.batter_berm_preview_closed = closed;
            self.invalidate_geometry();
        }
    }

    pub(crate) fn pick_batter_berm_target(&mut self) {
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));
        if let Some((SceneEntityId::Object(id), _)) = picked
            && self.activate_project_for_object(id)
            && matches!(self.active_document().get_object(id), Some(Object::Polyline { .. }))
        {
            self.editor.batter_berm_target_id = Some(id);
            self.editor.batter_berm_dialog_open = true;
            self.editor.batter_berm_preview_key = None;
            self.editor.tool_highlight_id = Some(id);
            let closed = matches!(self.active_document().get_object(id), Some(Object::Polyline { closed: true, .. }));
            self.editor.batter_berm_preview_closed = closed;
            self.invalidate_geometry();
        }
    }

    /// Recompute preview rings from current dialog inputs. Called every render frame while open.
    pub(crate) fn update_batter_berm_preview(&mut self) {
        if !self.editor.batter_berm_dialog_open {
            return;
        }
        let Some(object_id) = self.editor.batter_berm_target_id else {
            return;
        };
        if !self.activate_project_for_object(object_id) {
            return;
        }

        let document_revision = self.active_document().revision();
        let berm_width = self.editor.batter_berm_width;
        let angle_deg = self.editor.batter_berm_angle;
        let bench_height = self.editor.batter_berm_bench_height;
        let benches = self.editor.batter_berm_benches;
        let mode = self.editor.batter_berm_mode;
        let direction_up = self.editor.batter_berm_direction_up;
        let preview_key = BatterBermPreviewKey {
            target_id: object_id,
            document_revision,
            width: berm_width,
            angle: angle_deg,
            bench_height,
            benches,
            mode,
            direction_up,
        };
        if self.editor.batter_berm_preview_key == Some(preview_key) {
            return;
        }

        let (src_verts, closed) = match self.active_document().get_object(object_id) {
            Some(Object::Polyline { verts, closed, .. }) => (crate::model::geometry::tessellate_polyline_bulges(verts, *closed), *closed),
            _ => return,
        };

        self.editor.batter_berm_source_world = src_verts.clone();
        if !berm_width.is_finite() || berm_width <= 0.0 || !angle_deg.is_finite() || angle_deg <= 0.0 || angle_deg >= 90.0 || !bench_height.is_finite() || bench_height <= 0.0 {
            self.editor.batter_berm_max_benches = 0;
            self.editor.batter_berm_rings_world.clear();
            self.editor.batter_berm_preview_key = Some(preview_key);
            return;
        }

        let (side, delta_height, outward) = mode_to_side_and_dz(&src_verts, closed, mode, direction_up, bench_height);
        let batter_horiz = batter_horizontal_dist(angle_deg, bench_height);
        let generation = compute_batter_berm_rings(
            &src_verts,
            BatterBermGenerationParams {
                closed,
                side,
                outward,
                batter_horiz,
                berm_width,
                delta_height,
                requested_benches: None,
                hard_limit: BATTER_BERM_SEARCH_LIMIT,
            },
            || false,
        );
        self.editor.batter_berm_max_benches = generation.completed_benches;
        self.editor.batter_berm_benches = if generation.completed_benches == 0 {
            0
        } else {
            benches.clamp(1, generation.completed_benches)
        };
        let preview_ring_count = usize::try_from(self.editor.batter_berm_benches).unwrap_or(usize::MAX / 2).saturating_mul(2);
        self.editor.batter_berm_rings_world = generation.rings.into_iter().take(preview_ring_count).collect();
        self.editor.batter_berm_preview_closed = closed;
        self.editor.batter_berm_preview_key = Some(BatterBermPreviewKey {
            benches: self.editor.batter_berm_benches,
            ..preview_key
        });
    }

    /// Commit all rings from the current preview as new polylines.
    pub(crate) fn commit_batter_berm(&mut self) {
        let Some(object_id) = self.editor.batter_berm_target_id else {
            return;
        };
        if !self.activate_project_for_object(object_id) {
            return;
        }

        if self.editor.batter_berm_rings_world.is_empty() {
            return;
        }

        let (layer, color, fill, line_weight, closed) = match self.active_document().get_object(object_id) {
            Some(Object::Polyline {
                layer,
                color,
                fill,
                line_weight,
                closed,
                ..
            }) => (*layer, *color, *fill, *line_weight, *closed),
            _ => return,
        };

        let rings = self.editor.batter_berm_rings_world.clone();

        if let Some(project) = self.workspace.active_project_mut() {
            let doc = &mut project.project.document;
            let commands = rings
                .into_iter()
                .map(|ring_verts| {
                    let new_verts: Vec<PolyVertex> = ring_verts.into_iter().map(PolyVertex::straight).collect();
                    let id = doc.allocate_object_id();
                    Command::AddObject(Object::Polyline {
                        id,
                        layer,
                        verts: new_verts,
                        closed,
                        color,
                        fill,
                        line_weight,
                    })
                })
                .collect::<Vec<_>>();
            if !commands.is_empty() {
                self.history.execute(doc, Command::Batch(commands));
            }
        }

        self.cancel_batter_berm();
        crate::logging::report_completed_action(
            CommandReportSpec::new("Create Batter Berm", format!("{object_id:?}")),
            format!("Created batter berm from object {object_id:?}"),
        );
        self.invalidate_geometry();
    }

    pub(crate) fn cancel_batter_berm(&mut self) {
        self.editor.batter_berm_dialog_open = false;
        self.editor.batter_berm_target_id = None;
        self.editor.batter_berm_rings_world.clear();
        self.editor.batter_berm_source_world.clear();
        self.editor.batter_berm_rings_screen_px.clear();
        self.editor.batter_berm_source_screen_px.clear();
        self.editor.tool_highlight_id = None;
        self.editor.batter_berm_preview_key = None;
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_geometry();
    }
}

/// Resolve the Type + Direction selectors into a horizontal offset side, a
/// per-bench vertical step, and whether the rings expand outward.
///
/// Direction alone sets the vertical sign: `direction_up` rises by
/// `bench_height`, otherwise it falls. The horizontal side is the combination
/// of the two selectors:
///
/// | Type      | Direction | Horizontal |
/// |-----------|-----------|------------|
/// | Pit       | Up        | outward    |
/// | Pit       | Down      | inward     |
/// | Stockpile | Up        | inward     |
/// | Stockpile | Down      | outward    |
fn mode_to_side_and_dz(verts: &[glam::DVec3], closed: bool, mode: BatterBermMode, direction_up: bool, bench_height: f64) -> (f64, f64, bool) {
    let inward = inward_side(verts, closed);
    // Pit + Up and Stockpile + Down step outward; the other two step inward.
    let steps_outward = matches!(mode, BatterBermMode::Pit) == direction_up;
    let side = if steps_outward { -inward } else { inward };
    let delta_z = if direction_up { bench_height } else { -bench_height };
    (side, delta_z, steps_outward)
}

/// Returns the sign that offsets geometry inward for the given polyline.
fn inward_side(verts: &[glam::DVec3], closed: bool) -> f64 {
    if closed && verts.len() >= 3 {
        // Positive offset = left of directed edges = inward for CCW polylines.
        let area = crate::model::geometry::signed_area_xy(verts);
        if area > 0.0 { 1.0 } else { -1.0 }
    } else {
        -1.0
    }
}

/// Horizontal run of the batter face for a given angle and bench height.
fn batter_horizontal_dist(angle_deg: f64, bench_height: f64) -> f64 {
    let tan = angle_deg.to_radians().tan();
    if tan.abs() < 1e-9 { 0.0 } else { bench_height / tan }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatterBermStopReason {
    RequestedCount,
    NoValidBatter,
    NoValidBerm,
    SafetyLimit,
    Cancelled,
}

#[derive(Debug)]
struct BatterBermGeneration {
    /// Complete pairs: toe_0, berm_0, toe_1, berm_1, …
    rings: Vec<Vec<glam::DVec3>>,
    completed_benches: u32,
    _stop_reason: BatterBermStopReason,
}

#[derive(Clone, Copy, Debug)]
struct BatterBermGenerationParams {
    closed: bool,
    side: f64,
    /// `true` when successive rings grow away from the source boundary
    /// (Pit + Up, Stockpile + Down); `false` when they shrink inward.
    outward: bool,
    batter_horiz: f64,
    berm_width: f64,
    delta_height: f64,
    requested_benches: Option<u32>,
    hard_limit: u32,
}

/// Produce complete batter-and-berm pairs. A requested finite count stops
/// after exactly that many pairs; `None` continues until geometry fails.
/// `hard_limit` and `is_cancelled` bound the maximum-depth search, especially
/// for open strings whose offsets do not naturally collapse.
fn compute_batter_berm_rings(src_verts: &[glam::DVec3], params: BatterBermGenerationParams, mut is_cancelled: impl FnMut() -> bool) -> BatterBermGeneration {
    const MAX_OUTPUT_VERTICES: usize = 1_000_000;
    let target = params.requested_benches.unwrap_or(params.hard_limit).min(params.hard_limit);
    let ring_capacity = usize::try_from(target).unwrap_or(usize::MAX / 2).saturating_mul(2);
    let mut rings = Vec::with_capacity(ring_capacity);
    let mut previous = src_verts.to_vec();
    let mut completed_benches = 0;
    let mut output_vertices = 0usize;

    while completed_benches < target {
        if is_cancelled() {
            return BatterBermGeneration {
                rings,
                completed_benches,
                _stop_reason: BatterBermStopReason::Cancelled,
            };
        }
        let level = f64::from(completed_benches + 1);
        let toe_distance = level * params.batter_horiz + f64::from(completed_benches) * params.berm_width;
        let berm_distance = level * (params.batter_horiz + params.berm_width);
        let level_delta_z = level * params.delta_height;

        // Always offset from the original boundary using cumulative design
        // distances. Recursively offsetting a cleaned intermediate ring loses
        // the original edge constraints when a short side collapses. The
        // cumulative construction lets a new apex move farther than the
        // nominal berm width while the surviving sides remain exactly one
        // berm width apart.
        let toe = clean_offset(batter_ring_offset(src_verts, params.closed, params.side * toe_distance, level_delta_z), params.closed);
        if !valid_offset(&previous, &toe, params.closed, params.outward) {
            return BatterBermGeneration {
                rings,
                completed_benches,
                _stop_reason: BatterBermStopReason::NoValidBatter,
            };
        }
        if is_cancelled() {
            return BatterBermGeneration {
                rings,
                completed_benches,
                _stop_reason: BatterBermStopReason::Cancelled,
            };
        }
        let berm = clean_offset(batter_ring_offset(src_verts, params.closed, params.side * berm_distance, level_delta_z), params.closed);
        if !valid_offset(&toe, &berm, params.closed, params.outward) {
            return BatterBermGeneration {
                rings,
                completed_benches,
                _stop_reason: BatterBermStopReason::NoValidBerm,
            };
        }
        let pair_vertices = toe.len().saturating_add(berm.len());
        if output_vertices.saturating_add(pair_vertices) > MAX_OUTPUT_VERTICES {
            return BatterBermGeneration {
                rings,
                completed_benches,
                _stop_reason: BatterBermStopReason::SafetyLimit,
            };
        }
        output_vertices += pair_vertices;
        rings.push(toe);
        rings.push(berm.clone());
        previous = berm;
        completed_benches += 1;
    }

    let stop_reason = if params.requested_benches.is_some_and(|count| completed_benches >= count) {
        BatterBermStopReason::RequestedCount
    } else {
        BatterBermStopReason::SafetyLimit
    };
    BatterBermGeneration {
        rings,
        completed_benches,
        _stop_reason: stop_reason,
    }
}

/// Offset a design ring while retaining both independently offset side
/// endpoints at a concave corner. Joining their infinite lines into one miter
/// makes a re-entrant wedge overlap at later levels; the short bevel edge is
/// the geometrically valid join and keeps both adjacent sides at the requested
/// perpendicular distance.
fn batter_ring_offset(verts: &[glam::DVec3], closed: bool, signed_horiz_dist: f64, z_delta: f64) -> Vec<glam::DVec3> {
    use glam::DVec2;

    if !closed || verts.len() < 3 {
        return crate::model::geometry::geometric_offset(verts, closed, signed_horiz_dist, z_delta);
    }

    const MITER_LIMIT: f64 = 4.0;
    let count = verts.len();
    let mut directions = Vec::with_capacity(count);
    let mut normals = Vec::with_capacity(count);
    for index in 0..count {
        let edge = verts[(index + 1) % count].truncate() - verts[index].truncate();
        let direction = if edge.length_squared() > 1.0e-20 { edge.normalize() } else { DVec2::X };
        directions.push(direction);
        normals.push(DVec2::new(-direction.y, direction.x));
    }

    let mut result = Vec::with_capacity(count + 2);
    for index in 0..count {
        let previous = (index + count - 1) % count;
        let vertex = verts[index];
        let incoming_endpoint = vertex.truncate() + signed_horiz_dist * normals[previous];
        let outgoing_start = vertex.truncate() + signed_horiz_dist * normals[index];
        let turn = crate::model::kernel::orient2d(DVec2::ZERO, directions[previous], directions[index]);
        let is_concave_for_offset = turn * signed_horiz_dist < 0.0;
        let intersection = if is_concave_for_offset {
            None
        } else {
            crate::model::kernel::line_line(incoming_endpoint, directions[previous], outgoing_start, directions[index])
        }
        .filter(|point| signed_horiz_dist.abs() < 1.0e-10 || (*point - vertex.truncate()).length() <= MITER_LIMIT * signed_horiz_dist.abs());
        let z = vertex.z + z_delta;
        if is_concave_for_offset {
            let Some((incoming_cap, outgoing_cap)) = concave_bisector_cap(
                vertex.truncate(),
                signed_horiz_dist,
                [normals[previous], normals[index]],
                [incoming_endpoint, outgoing_start],
                [directions[previous], directions[index]],
            ) else {
                return Vec::new();
            };
            push_distinct_offset_point(&mut result, glam::DVec3::new(incoming_cap.x, incoming_cap.y, z));
            push_distinct_offset_point(&mut result, glam::DVec3::new(outgoing_cap.x, outgoing_cap.y, z));
        } else if let Some(point) = intersection {
            push_distinct_offset_point(&mut result, glam::DVec3::new(point.x, point.y, z));
        } else {
            push_distinct_offset_point(&mut result, glam::DVec3::new(incoming_endpoint.x, incoming_endpoint.y, z));
            push_distinct_offset_point(&mut result, glam::DVec3::new(outgoing_start.x, outgoing_start.y, z));
        }
    }
    if result.len() >= 2 && result[0].truncate().distance_squared(result[result.len() - 1].truncate()) <= crate::model::kernel::XY_TOL.powi(2) {
        result.pop();
    }
    result
}

/// Build the short edge that replaces a concave wedge point. Its line is a
/// full `signed_horiz_dist` from the original point along the inward angle
/// bisector, so consecutive toe/berm caps retain the requested perpendicular
/// spacing. Endpoints are intersections with the independently offset side
/// lines, preserving those sides' design offsets too.
fn concave_bisector_cap(
    vertex: glam::DVec2,
    signed_horiz_dist: f64,
    normals: [glam::DVec2; 2],
    line_points: [glam::DVec2; 2],
    directions: [glam::DVec2; 2],
) -> Option<(glam::DVec2, glam::DVec2)> {
    let side = signed_horiz_dist.signum();
    let inward_bisector = (side * (normals[0] + normals[1])).try_normalize()?;
    let cap_center = vertex + inward_bisector * signed_horiz_dist.abs();
    let cap_direction = glam::DVec2::new(-inward_bisector.y, inward_bisector.x);
    let incoming_cap = crate::model::kernel::line_line(line_points[0], directions[0], cap_center, cap_direction)?;
    let outgoing_cap = crate::model::kernel::line_line(line_points[1], directions[1], cap_center, cap_direction)?;

    // Once the two endpoints meet or exchange order, the wedge has collapsed.
    // Continuing would create the overlapping future benches this join avoids.
    let natural_order = line_points[1] - line_points[0];
    let cap_order = outgoing_cap - incoming_cap;
    if cap_order.dot(natural_order) <= crate::model::kernel::XY_TOL.powi(2) {
        return None;
    }
    Some((incoming_cap, outgoing_cap))
}

fn push_distinct_offset_point(points: &mut Vec<glam::DVec3>, point: glam::DVec3) {
    let duplicate = points
        .last()
        .is_some_and(|previous| previous.truncate().distance_squared(point.truncate()) <= crate::model::kernel::XY_TOL.powi(2));
    if !duplicate {
        points.push(point);
    }
}

/// Validate one generated ring against the one before it. An inward step must
/// shrink and stay inside its predecessor; an outward step (`outward == true`)
/// must grow and fully contain it. Either way the winding must not flip and the
/// smaller ring's vertices plus edge midpoints must lie inside the larger one,
/// which catches a cleaned ring that folded across or escaped the boundary.
fn valid_offset(previous: &[glam::DVec3], next: &[glam::DVec3], closed: bool, outward: bool) -> bool {
    use crate::model::geometry::{point_in_polyline_xy, signed_area_xy};

    if !closed {
        return next.len() >= 2;
    }
    if previous.len() < 3 || next.len() < 3 {
        return false;
    }

    let previous_area = signed_area_xy(previous);
    let next_area = signed_area_xy(next);
    if previous_area.signum() != next_area.signum() {
        return false;
    }
    let area_tolerance = previous_area.abs().max(1.0) * 1e-9;
    let area_grew = next_area.abs() >= previous_area.abs() - area_tolerance;
    let area_shrank = next_area.abs() <= previous_area.abs() + area_tolerance;
    if (outward && area_shrank) || (!outward && area_grew) {
        return false;
    }

    // The inner ring is the predecessor when stepping outward, otherwise the
    // new ring. Every one of its vertices (and edge midpoints) must sit inside
    // the outer ring.
    let (inner, outer) = if outward { (previous, next) } else { (next, previous) };
    for i in 0..inner.len() {
        let a = inner[i];
        let b = inner[(i + 1) % inner.len()];
        if !point_in_polyline_xy(a.truncate(), outer) || !point_in_polyline_xy(a.lerp(b, 0.5).truncate(), outer) {
            return false;
        }
    }
    true
}

fn clean_offset(verts: Vec<glam::DVec3>, closed: bool) -> Vec<glam::DVec3> {
    if closed { crate::model::geometry::remove_self_intersections(verts) } else { verts }
}
