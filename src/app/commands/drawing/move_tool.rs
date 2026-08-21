use glam::DVec3;

use crate::{
    app::{App, GizmoDragConstraint, GizmoDragState, MoveSession},
    logging::CommandReportSpec,
    model::{Command, Object, ObjectId, ObjectPoint, SceneEntityId, geometry::compact_circle_center},
};

impl<'a> App<'a> {
    pub(crate) fn apply_move_delta(&mut self, delta: DVec3) {
        self.ensure_move_session_original();
        self.preview_move_delta(delta);
        let Some(session) = self.move_session_original.take() else {
            return;
        };
        let active_runtime_id = self.workspace.active_project().map(|project| project.runtime_id);
        if active_runtime_id != Some(session.project_runtime_id) {
            self.restore_move_session(session);
            self.reset_move_editor_state();
            return;
        }
        let commands: Vec<Command> = session
            .originals
            .into_iter()
            .filter_map(|before| {
                let after = self.active_document().get_object(before.id()).cloned()?;
                objects_differ(&before, &after).then_some(Command::Replace { before, after })
            })
            .collect();

        let moved = commands.len();
        if moved > 0 {
            self.history.push_applied(Command::Batch(commands));
            crate::logging::report_completed_action(
                CommandReportSpec::new("Move Selection", format!("{moved} object(s)")),
                format!("Applied move delta ({delta}) to {moved} object(s)"),
            );
        }
        self.reset_move_editor_state();
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    pub(crate) fn cancel_move_delta(&mut self) {
        self.gizmo_drag = None;
        self.editor.gizmo_drag_axis_index = None;
        self.editor.gizmo_drag_plane_index = None;
        self.restore_move_session_original();
        self.reset_move_editor_state();
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    /// Commit any pending move to history without applying an additional delta.
    ///
    /// Unlike `cancel_move_delta` (which reverts objects), this keeps the
    /// current document state - whatever `preview_move_delta` last produced -
    /// and pushes a Replace command for each changed object.  Used by save
    /// paths so that an in-progress move is preserved rather than silently lost.
    pub(crate) fn commit_pending_move(&mut self) {
        self.gizmo_drag = None;
        self.editor.gizmo_drag_axis_index = None;
        self.editor.gizmo_drag_plane_index = None;

        let Some(session) = self.move_session_original.take() else {
            self.reset_move_editor_state();
            return;
        };
        let active_runtime_id = self.workspace.active_project().map(|project| project.runtime_id);
        if active_runtime_id != Some(session.project_runtime_id) {
            self.restore_move_session(session);
            self.reset_move_editor_state();
            return;
        }
        let commands: Vec<Command> = session
            .originals
            .into_iter()
            .filter_map(|before| {
                let after = self.active_document().get_object(before.id()).cloned()?;
                objects_differ(&before, &after).then_some(Command::Replace { before, after })
            })
            .collect();

        let moved = commands.len();
        if moved > 0 {
            self.history.push_applied(Command::Batch(commands));
        }

        self.reset_move_editor_state();
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    pub(crate) fn has_pending_move_delta(&self) -> bool {
        self.move_session_original.is_some() || self.editor.move_vertex_target.is_some() || self.gizmo_drag.is_some()
    }

    pub(crate) fn begin_gizmo_drag(&mut self, axis_idx: u8, axis: DVec3, cursor_px: (f32, f32)) {
        if !self.editing_ready() {
            return;
        }
        let selected_ids = self.selected_object_ids();
        if selected_ids.is_empty() {
            return;
        }
        self.ensure_move_session_original();
        let (axis_screen_dir, px_per_world_unit) = self.gizmo_axis_screen_basis(axis_idx, cursor_px);
        let start_delta = DVec3::new(self.editor.move_panel_delta[0], self.editor.move_panel_delta[1], self.editor.move_panel_delta[2]);

        self.editor.gizmo_drag_axis_index = Some(axis_idx);
        self.editor.gizmo_drag_plane_index = None;
        self.gizmo_drag = Some(GizmoDragState {
            constraint: GizmoDragConstraint::Axis {
                axis,
                screen_dir: axis_screen_dir,
                px_per_world_unit,
            },
            start_cursor_screen_px: cursor_px,
            start_delta,
        });
    }

    pub(crate) fn begin_gizmo_plane_drag(&mut self, plane_idx: u8, axes: [DVec3; 2], cursor_px: (f32, f32)) {
        if !self.editing_ready() || self.selected_object_ids().is_empty() {
            return;
        }
        let Some(screen_basis) = self.gizmo_plane_screen_basis(plane_idx) else {
            return;
        };
        // Edge-on planes cannot be solved reliably from pointer movement.
        if plane_world_delta((1.0, 1.0), screen_basis).is_none() {
            return;
        }
        self.ensure_move_session_original();
        let start_delta = DVec3::new(self.editor.move_panel_delta[0], self.editor.move_panel_delta[1], self.editor.move_panel_delta[2]);
        self.editor.gizmo_drag_axis_index = None;
        self.editor.gizmo_drag_plane_index = Some(plane_idx);
        self.gizmo_drag = Some(GizmoDragState {
            constraint: GizmoDragConstraint::Plane { axes, screen_basis },
            start_cursor_screen_px: cursor_px,
            start_delta,
        });
    }

    pub(crate) fn move_gizmo_to_cursor(&mut self) {
        let Some(gizmo) = self.gizmo_drag.as_ref() else {
            return;
        };
        let Some(cursor_px) = self.editor.cursor_screen_px else {
            return;
        };
        let screen_delta = (cursor_px.0 - gizmo.start_cursor_screen_px.0, cursor_px.1 - gizmo.start_cursor_screen_px.1);
        let world_delta = match gizmo.constraint {
            GizmoDragConstraint::Axis {
                axis,
                screen_dir,
                px_per_world_unit,
            } => {
                let pixels = screen_delta.0 * screen_dir.0 + screen_delta.1 * screen_dir.1;
                gizmo.start_delta + axis * (f64::from(pixels) / px_per_world_unit)
            }
            GizmoDragConstraint::Plane { axes, screen_basis } => {
                let Some((first, second)) = plane_world_delta(screen_delta, screen_basis) else {
                    return;
                };
                gizmo.start_delta + axes[0] * first + axes[1] * second
            }
        };
        self.editor.move_panel_delta = [world_delta.x, world_delta.y, world_delta.z];
        self.preview_move_delta(world_delta);
        self.invalidate_geometry();
    }

    pub(crate) fn finish_gizmo_drag(&mut self) {
        let Some(_gizmo) = self.gizmo_drag.take() else {
            return;
        };
        self.editor.gizmo_drag_axis_index = None;
        self.editor.gizmo_drag_plane_index = None;
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    pub(crate) fn ensure_move_session_original(&mut self) {
        let selected_ids = self.move_target_object_ids();
        if selected_ids.is_empty() {
            self.move_session_original = None;
            return;
        }
        let should_refresh = self
            .move_session_original
            .as_ref()
            .map(|session| {
                self.workspace.active_project().map(|project| project.runtime_id) != Some(session.project_runtime_id)
                    || session.originals.len() != selected_ids.len()
                    || session.originals.iter().any(|object| !selected_ids.contains(&object.id()))
            })
            .unwrap_or(true);
        if should_refresh {
            // A changed target set must never use already-previewed geometry as
            // its new baseline. Restore first, then capture the new selection.
            if self.move_session_original.is_some() {
                self.restore_move_session_original();
            }
            let Some(project_runtime_id) = self.workspace.active_project().map(|project| project.runtime_id) else {
                return;
            };
            self.move_session_original = Some(MoveSession {
                project_runtime_id,
                originals: selected_ids.iter().filter_map(|&object_id| self.active_document().get_object(object_id).cloned()).collect(),
            });
        }
    }

    pub(crate) fn preview_move_delta(&mut self, delta: DVec3) {
        let Some(session) = self.move_session_original.as_ref() else {
            return;
        };
        let project_runtime_id = session.project_runtime_id;
        let originals = session.originals.clone();
        let vertex_target = self.editor.move_vertex_target;
        if let Some(index) = self.workspace.project_index_for_runtime_id(project_runtime_id)
            && let Some(project) = self.workspace.projects.get_mut(index)
        {
            for object in originals {
                let mut moved = object.clone();
                translate_move_target(&mut moved, vertex_target, delta);
                project.project.document.replace_object(moved);
            }
        }
    }

    pub(crate) fn restore_move_session_original(&mut self) {
        let Some(session) = self.move_session_original.take() else {
            return;
        };
        self.restore_move_session(session);
    }

    fn restore_move_session(&mut self, session: MoveSession) {
        let Some(index) = self.workspace.project_index_for_runtime_id(session.project_runtime_id) else {
            return;
        };
        let project = &mut self.workspace.projects[index];
        for object in session.originals {
            project.project.document.replace_object(object);
        }
    }

    fn reset_move_editor_state(&mut self) {
        self.editor.move_vertex_target = None;
        self.editor.move_panel_delta = [0.0; 3];
        self.editor.move_panel_last_preview = [f64::NAN; 3];
    }

    fn selected_object_ids(&self) -> Vec<ObjectId> {
        self.editor
            .selected_handles
            .iter()
            .filter_map(|handle| match handle {
                SceneEntityId::Object(object_id) => Some(*object_id),
                _ => None,
            })
            .collect()
    }

    fn move_target_object_ids(&self) -> Vec<ObjectId> {
        if let Some((object_id, _)) = self.editor.move_vertex_target {
            vec![object_id]
        } else {
            self.selected_object_ids()
        }
    }

    fn gizmo_axis_screen_basis(&self, axis_idx: u8, cursor_px: (f32, f32)) -> ((f32, f32), f64) {
        let gizmo = &self.editor.move_gizmo;
        let index = usize::from(axis_idx).min(2);
        let center = gizmo.center_px.unwrap_or(cursor_px);
        let tip = gizmo.axis_tip_px[index].unwrap_or(cursor_px);
        let dx = tip.0 - center.0;
        let dy = tip.1 - center.1;
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        ((dx / len, dy / len), gizmo.axis_px_per_world[index].max(0.001))
    }

    /// Screen-pixel vectors produced by one world unit along each of the two
    /// axes a plane handle - or the view ring - constrains movement to.
    fn gizmo_plane_screen_basis(&self, plane_idx: u8) -> Option<[(f64, f64); 2]> {
        let gizmo = &self.editor.move_gizmo;
        if plane_idx == crate::ui::state::MOVE_GIZMO_VIEW_PLANE {
            return gizmo.view_axes.is_some().then_some(gizmo.view_basis_px);
        }
        let center = gizmo.center_px?;
        let vector = |index: usize| {
            let tip = gizmo.axis_tip_px[index]?;
            let dx = f64::from(tip.0 - center.0);
            let dy = f64::from(tip.1 - center.1);
            let length = (dx * dx + dy * dy).sqrt();
            let px_per_world = gizmo.axis_px_per_world[index];
            (length > 1.0e-6).then_some((dx / length * px_per_world, dy / length * px_per_world))
        };
        match plane_idx {
            0 => Some([vector(0)?, vector(1)?]),
            1 => Some([vector(0)?, vector(2)?]),
            2 => Some([vector(1)?, vector(2)?]),
            _ => None,
        }
    }
}

fn plane_world_delta(screen_delta: (f32, f32), screen_basis: [(f64, f64); 2]) -> Option<(f64, f64)> {
    let [(ax, ay), (bx, by)] = screen_basis;
    let determinant = ax * by - ay * bx;
    let scale = (ax * ax + ay * ay).sqrt().max((bx * bx + by * by).sqrt()).max(1.0);
    if determinant.abs() <= scale * scale * 1.0e-4 {
        return None;
    }
    let dx = f64::from(screen_delta.0);
    let dy = f64::from(screen_delta.1);
    Some(((dx * by - dy * bx) / determinant, (ax * dy - ay * dx) / determinant))
}

fn translate_move_target(object: &mut Object, vertex_target: Option<(ObjectId, ObjectPoint)>, delta: DVec3) {
    let Some((target_id, point)) = vertex_target else {
        object.translate(delta);
        return;
    };
    if object.id() != target_id {
        return;
    }
    match (object, point) {
        (Object::Polyline { verts, .. }, ObjectPoint::Vertex(vertex_index)) => {
            if let Some(vertex) = verts.get_mut(vertex_index) {
                vertex.pos += delta;
            }
        }
        (object @ Object::Polyline { .. }, ObjectPoint::Center) => {
            let is_circle = matches!(object, Object::Polyline { verts, closed, .. } if compact_circle_center(verts, *closed).is_some());
            if is_circle {
                object.translate(delta);
            }
        }
        (Object::Point { pos, .. }, ObjectPoint::Vertex(0)) => {
            *pos += delta;
        }
        _ => {}
    }
}

fn objects_differ(before: &Object, after: &Object) -> bool {
    before != after
}
