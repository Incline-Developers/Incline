use crate::{
    app::App,
    logging::CommandReportSpec,
    model::{Command, LayerId, Object, PolyVertex, geometry::circle_polyline_vertices},
    ui::state::{ActiveTool, CircleDraft},
};

impl<'a> App<'a> {
    pub(crate) fn measure_distance_click(&mut self) {
        if matches!(
            self.editor.cursor_mode,
            crate::ui::state::CursorMode::SnapToPoint | crate::ui::state::CursorMode::SnapToLine | crate::ui::state::CursorMode::SnapToSurface
        ) && !self.editor.cursor_snapped
        {
            return;
        }
        let Some(point) = self.editor.cursor_world else {
            return;
        };
        if let Some(_) = self.editor.measurement_start
            && self.editor.measurement_end.is_none()
        {
            self.editor.measurement_end = Some(point);
        } else {
            self.editor.measurement_start = Some(point);
            self.editor.measurement_end = None;
        }
        self.invalidate_overlay();
    }

    pub(crate) fn measure_batter_angle_click(&mut self) {
        if matches!(
            self.editor.cursor_mode,
            crate::ui::state::CursorMode::SnapToPoint | crate::ui::state::CursorMode::SnapToLine | crate::ui::state::CursorMode::SnapToSurface
        ) && !self.editor.cursor_snapped
        {
            return;
        }
        let Some(point) = self.editor.cursor_world else {
            return;
        };
        if self.editor.batter_angle_points.len() >= 3 {
            self.editor.batter_angle_points.clear();
        }
        self.editor.batter_angle_points.push(point);
        self.invalidate_overlay();
    }

    pub(crate) fn place_point_at_cursor(&mut self) {
        if !self.editing_ready() {
            return;
        }
        // Block if snap mode is active but cursor isn't snapped
        if matches!(
            self.editor.cursor_mode,
            crate::ui::state::CursorMode::SnapToPoint | crate::ui::state::CursorMode::SnapToLine | crate::ui::state::CursorMode::SnapToSurface
        ) && !self.editor.cursor_snapped
        {
            return;
        }
        let Some(world) = self.editor.cursor_world else {
            return;
        };
        let Some(layer) = self.active_layer() else {
            return;
        };
        let color = crate::model::ObjectColor::Fixed(self.editor.tool_line_color);
        let created = if let Some(project) = self.workspace.active_project_mut() {
            let doc = &mut project.project.document;
            let id = doc.allocate_object_id();
            self.execute_edit(Command::AddObject(Object::Point { id, layer, pos: world, color }));
            true
        } else {
            false
        };
        if !created {
            return;
        }
        crate::logging::report_completed_action(
            CommandReportSpec::new(
                crate::i18n::tr!(literal = "Create Point"),
                format!("X {:.3} · Y {:.3} · Z {:.3}", world.x, world.y, world.z),
            ),
            crate::i18n::tr_format!(
                literal = "Placed point at %x%, %y%, %z%",
                x = format!("{:.3}", world.x),
                y = format!("{:.3}", world.y),
                z = format!("{:.3}", world.z)
            ),
        );
        self.invalidate_geometry();
    }

    pub(crate) fn place_stroke_point(&mut self) {
        if !self.editing_ready() {
            return;
        }
        // Block if snap mode is active but cursor isn't snapped
        if matches!(
            self.editor.cursor_mode,
            crate::ui::state::CursorMode::SnapToPoint | crate::ui::state::CursorMode::SnapToLine | crate::ui::state::CursorMode::SnapToSurface
        ) && !self.editor.cursor_snapped
        {
            return;
        }
        let Some(world) = self.editor.cursor_world else {
            return;
        };
        let Some(layer) = self.active_layer() else {
            return;
        };
        self.editor.pending_stroke.push(world);
        match self.editor.active_tool {
            ActiveTool::MakeLine if self.editor.pending_stroke.len() >= 2 => {
                let verts: Vec<PolyVertex> = self.editor.pending_stroke.iter().map(|&p| PolyVertex::straight(p)).collect();
                let chain_start = *self.editor.pending_stroke.last().unwrap();
                if !self.commit_polyline(verts, false, layer) {
                    return;
                }
                crate::logging::report_completed_action(
                    CommandReportSpec::new(crate::i18n::tr!(literal = "Create Line"), crate::i18n::tr!(literal = "2 vertices")),
                    crate::i18n::tr!(literal = "Created line segment with 2 vertices"),
                );
                // Chain: end of the last segment becomes start of next
                self.editor.pending_stroke.clear();
                self.editor.pending_stroke.push(chain_start);
            }
            ActiveTool::MakePoly => {
                // If the user clicked the first vertex, auto-close without showing a dialog.
                let is_closing = self.editor.pending_stroke.len() >= 2
                    && self
                        .editor
                        .pending_stroke
                        .first()
                        .zip(self.editor.pending_stroke.last())
                        .is_some_and(|(first, last)| (*first - *last).length_squared() <= 1.0e-16);
                if is_closing {
                    self.finish_poly_closed();
                    return;
                }
            }
            _ => {}
        }
        self.invalidate_geometry();
    }

    pub(crate) fn make_circle_click(&mut self) {
        if !self.editing_ready() {
            return;
        }
        if matches!(
            self.editor.cursor_mode,
            crate::ui::state::CursorMode::SnapToPoint | crate::ui::state::CursorMode::SnapToLine | crate::ui::state::CursorMode::SnapToSurface
        ) && !self.editor.cursor_snapped
        {
            return;
        }
        let Some(world) = self.editor.cursor_world else {
            return;
        };

        let Some(draft) = self.editor.circle_draft.as_mut() else {
            self.editor.circle_draft = Some(CircleDraft::new(world));
            self.invalidate_overlay();
            return;
        };

        // A pointer click always uses the pointer distance, even when a typed
        // preview is active.
        let radius = draft.pointer_commit_radius(Some(world));
        draft.radius_text.clear();
        draft.typing_origin_screen_px = None;
        let Some(radius) = radius else {
            self.invalidate_overlay();
            return;
        };
        self.commit_circle(radius);
    }

    pub(crate) fn commit_circle_typed_radius(&mut self) {
        let Some(radius) = self.editor.circle_draft.as_ref().and_then(CircleDraft::typed_radius) else {
            return;
        };
        self.commit_circle(radius);
    }

    fn commit_circle(&mut self, radius: f64) {
        if self.editor.active_tool != ActiveTool::MakeCircle || !self.editing_ready() {
            return;
        }
        let Some(center) = self.editor.circle_draft.as_ref().map(|draft| draft.center) else {
            return;
        };
        let bearing = self.editor.cursor_world.map_or(glam::DVec2::X, |cursor| cursor.truncate() - center.truncate());
        let Some(verts) = circle_polyline_vertices(center, radius, bearing) else {
            return;
        };
        let Some(layer) = self.active_layer() else {
            return;
        };
        if !self.commit_polyline(verts.to_vec(), true, layer) {
            return;
        }
        self.editor.circle_draft = None;
        self.invalidate_geometry();
        crate::logging::report_completed_action(
            CommandReportSpec::new(
                crate::i18n::tr!(literal = "Create Circle"),
                crate::i18n::tr_format!(literal = "Radius %radius% m", radius = format!("{radius:.3}")),
            ),
            crate::i18n::tr_format!(literal = "Created circle with radius %radius% m", radius = format!("{radius:.3}")),
        );
    }

    /// Finish an in-progress MakePoly stroke as a closed polyline.
    pub(crate) fn finish_poly_closed(&mut self) {
        if self.editor.active_tool != ActiveTool::MakePoly {
            return;
        }
        // Clicking the first vertex to close the preview records that point a
        // second time. Closed polylines already add the last-to-first edge, so
        // discard the duplicate instead of creating a zero-length segment.
        if self.editor.pending_stroke.len() >= 2
            && self
                .editor
                .pending_stroke
                .first()
                .zip(self.editor.pending_stroke.last())
                .is_some_and(|(first, last)| (*first - *last).length_squared() <= 1.0e-16)
        {
            self.editor.pending_stroke.pop();
        }
        if self.editor.pending_stroke.len() < 3 {
            // A two-vertex stroke cannot form a polyline. This can happen when
            // the user clicks the first vertex to close it, so finish it as
            // the only valid shape instead of leaving the stroke pending.
            self.commit_stroke_open();
            return;
        }
        let Some(layer) = self.active_layer() else {
            return;
        };
        let verts: Vec<PolyVertex> = self.editor.pending_stroke.iter().map(|&p| PolyVertex::straight(p)).collect();
        let vertex_count = verts.len();
        if !self.commit_polyline(verts, true, layer) {
            return;
        }
        self.editor.pending_stroke.clear();
        self.editor.poly_finish_dialog = false;
        self.editor.poly_finish_dialog_px = None;
        self.invalidate_geometry();
        crate::logging::report_completed_action(
            CommandReportSpec::new(
                crate::i18n::tr!(literal = "Create Polyline"),
                crate::i18n::tr_format!(literal = "%count% vertices", count = vertex_count),
            ),
            crate::i18n::tr_format!(literal = "Created closed polyline with %count% vertices", count = vertex_count),
        );
    }

    /// Discard the current in-progress stroke without committing anything.
    pub(crate) fn discard_stroke(&mut self) {
        self.editor.pending_stroke.clear();
        self.editor.circle_draft = None;
        self.editor.measurement_start = None;
        self.editor.measurement_end = None;
        self.editor.batter_angle_points.clear();
        self.editor.poly_finish_dialog = false;
        self.editor.poly_finish_dialog_px = None;
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_geometry();
    }

    /// Commit the current stroke as an open polyline if it has enough vertices.
    pub(crate) fn commit_stroke_open(&mut self) {
        if self.editor.active_tool == ActiveTool::MakePoly
            && self.editor.pending_stroke.len() >= 2
            && let Some(layer) = self.active_layer()
        {
            let vertex_count = self.editor.pending_stroke.len();
            let verts: Vec<PolyVertex> = self.editor.pending_stroke.iter().map(|&p| PolyVertex::straight(p)).collect();
            if self.commit_polyline(verts, false, layer) {
                crate::logging::report_completed_action(
                    CommandReportSpec::new(
                        crate::i18n::tr!(literal = "Create Line"),
                        crate::i18n::tr_format!(literal = "%count% vertices", count = vertex_count),
                    ),
                    crate::i18n::tr_format!(literal = "Created open polyline with %count% vertices", count = vertex_count),
                );
            }
        }
        self.editor.pending_stroke.clear();
        self.editor.poly_finish_dialog = false;
        self.editor.poly_finish_dialog_px = None;
        self.invalidate_geometry();
    }

    /// Attempt to finish the active drawing tool via Enter.
    /// If no stroke is in progress, exits the active drawing tool.
    /// For MakePoly with three or more verts, opens the finish dialog at the cursor.
    /// A two-vertex stroke can only be an open polyline, so it commits directly.
    /// For MakeLine, clears the chain anchor so the next click starts a new string.
    pub(crate) fn try_finish_tool(&mut self) {
        match self.editor.active_tool {
            ActiveTool::MakeCircle if self.editor.circle_draft.take().is_some() => {
                self.invalidate_overlay();
            }
            ActiveTool::MakeCircle => {
                self.editor.active_tool = ActiveTool::None;
                self.invalidate_overlay();
            }
            ActiveTool::MakePoly if self.editor.pending_stroke.len() == 2 => {
                self.commit_stroke_open();
            }
            ActiveTool::MakePoly if self.editor.pending_stroke.len() >= 2 => {
                self.editor.poly_finish_dialog = true;
                // The Enter that opened this dialog must be released before its
                // own Enter shortcut can fire, or a held key skips it entirely.
                self.editor.poly_finish_dialog_confirm_armed = false;
                self.editor.poly_finish_dialog_px = self.editor.cursor_screen_px;
                self.redraw_requested = true;
            }
            ActiveTool::MakePoly if !self.editor.pending_stroke.is_empty() => {
                self.editor.pending_stroke.clear();
                self.editor.poly_finish_dialog = false;
                self.editor.poly_finish_dialog_px = None;
                self.invalidate_geometry();
            }
            ActiveTool::MakePoly => {
                self.editor.active_tool = ActiveTool::None;
                self.invalidate_geometry();
            }
            ActiveTool::MakeLine => {
                if self.editor.pending_stroke.is_empty() {
                    self.editor.active_tool = ActiveTool::None;
                    self.invalidate_geometry();
                } else {
                    self.editor.pending_stroke.clear();
                    self.invalidate_geometry();
                }
            }
            _ => {}
        }
    }

    fn commit_polyline(&mut self, verts: Vec<PolyVertex>, closed: bool, layer: LayerId) -> bool {
        if verts.len() < 2 {
            return false;
        }
        let color = crate::model::ObjectColor::Fixed(self.editor.tool_line_color);
        let line_weight = self.editor.tool_line_weight;
        let fill = self.editor.tool_hatch.to_fill_style();
        if let Some(project) = self.workspace.active_project_mut() {
            let doc = &mut project.project.document;
            let id = doc.allocate_object_id();
            self.execute_edit(Command::AddObject(Object::Polyline {
                id,
                layer,
                verts,
                closed,
                color,
                fill,
                line_weight,
            }));
            true
        } else {
            false
        }
    }
}
