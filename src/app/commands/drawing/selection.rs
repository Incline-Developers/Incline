use glam::DVec3;

use crate::{
    app::{App, MOVE_VERTEX_PICK_PX, PICK_THRESHOLD_PX},
    logging::CommandReportSpec,
    model::{Command, Object, ObjectId, ObjectPoint, SceneEntityId},
    rendering::pick,
    ui::state::ActiveTool,
};

struct DeleteVertexHit {
    object_id: ObjectId,
    vertex_index: usize,
    screen_px: (f32, f32),
    world: DVec3,
}

struct MoveVertexHit {
    object_id: ObjectId,
    point: ObjectPoint,
    screen_px: (f32, f32),
    world: DVec3,
}

impl<'a> App<'a> {
    pub(crate) fn select_all_active_objects(&mut self) {
        if self.has_pending_move_delta() {
            self.cancel_move_delta();
        }
        self.editor.selected_handles.clear();
        let handles = self.workspace.active_project().map_or_else(Vec::new, |project| {
            project
                .project
                .document
                .objects()
                .iter()
                .filter(|object| project.loaded_layers.contains(&object.layer()))
                .map(|object| SceneEntityId::Object(object.id()))
                .filter(|handle| !self.editor.hidden_handles.contains(handle) && !self.editor.frozen_handles.contains(handle))
                .collect::<Vec<_>>()
        });
        self.editor.selected_handles.extend(handles);
        self.invalidate_overlay();
    }

    /// Delete Points acts on points only: a polyline vertex under the cursor, or
    /// a standalone point object. Polylines and other entities are never picked
    /// here, so the tool can't delete a whole element.
    pub(crate) fn delete_at_cursor(&mut self) {
        if !self.editing_ready() {
            return;
        }
        if self.delete_polyline_vertex_at_cursor() {
            return;
        }
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|graphics| graphics.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));
        let Some((SceneEntityId::Object(object_id), world)) = picked else {
            return;
        };
        if !self.scene_document.get_object(object_id).is_some_and(is_point_object) {
            return;
        }
        if !self.activate_project_for_object(object_id) {
            return;
        }
        self.editor
            .on_canvas_pick(SceneEntityId::Object(object_id), world, crate::ui::state::SelectionMode::Replace);
        self.invalidate_geometry();
        self.editor.delete_confirm_open = true;
    }

    /// Keep canvas hover feedback aligned with the action that a click would
    /// perform. A nearby editable vertex takes priority; otherwise the object
    /// under the cursor is highlighted as a whole.
    pub(crate) fn update_move_delete_hover(&mut self) {
        let vertex_hit = match self.editor.active_tool {
            ActiveTool::Move if self.editor.move_gizmo_hovered_axis.is_none() && self.editor.move_gizmo_hovered_plane.is_none() && self.gizmo_drag.is_none() => {
                self.move_vertex_hit().map(|hit| (hit.object_id, hit.screen_px, hit.world))
            }
            ActiveTool::DeletePoints => self.delete_polyline_vertex_hit().map(|hit| (hit.object_id, hit.screen_px, hit.world)),
            _ => None,
        };
        let hover_px = vertex_hit.map(|(_, screen_px, _)| screen_px);
        let hover_world = vertex_hit.map(|(_, _, world)| world);
        // Delete Points only ever acts on point objects, so don't highlight
        // whole elements it can't delete.
        let point_objects_only = self.editor.active_tool == ActiveTool::DeletePoints;
        let hovered_object = vertex_hit.map(|(object_id, _, _)| object_id).or_else(|| {
            self.pick_hovered_object()
                .filter(|&id| !point_objects_only || self.scene_document.get_object(id).is_some_and(is_point_object))
        });

        if self.editor.tool_hover_vertex_px != hover_px {
            self.editor.tool_hover_vertex_px = hover_px;
            self.editor.tool_hover_vertex_world = hover_world;
            self.invalidate_overlay();
        }
        if self.editor.tool_highlight_id != hovered_object {
            self.editor.tool_highlight_id = hovered_object;
            self.invalidate_geometry();
        }
    }

    fn pick_hovered_object(&self) -> Option<ObjectId> {
        self.graphics
            .as_ref()
            .and_then(|graphics| {
                graphics.pick_at_cursor(
                    PICK_THRESHOLD_PX,
                    &self.triangulations,
                    &self.editor.hidden_handles,
                    &self.editor.frozen_handles,
                    self.editor.xray_enabled,
                )
            })
            .and_then(|(entity, _)| match entity {
                SceneEntityId::Object(object_id) => Some(object_id),
                _ => None,
            })
    }

    fn delete_polyline_vertex_at_cursor(&mut self) -> bool {
        let Some(hit) = self.delete_polyline_vertex_hit() else {
            return false;
        };
        if !self.activate_project_for_object(hit.object_id) {
            return false;
        }
        let Some(before) = self.active_document().get_object(hit.object_id).cloned() else {
            return false;
        };
        let Some(after) = without_polyline_vertex(&before, hit.vertex_index) else {
            return false;
        };

        let Some(project) = self.workspace.active_project_mut() else {
            return false;
        };
        self.history.execute(&mut project.project.document, Command::Replace { before, after });
        self.editor.tool_hover_vertex_px = None;
        self.editor.tool_hover_vertex_world = None;
        self.editor.selected_handles.clear();
        self.editor.selected_handles.insert(SceneEntityId::Object(hit.object_id));
        crate::logging::report_completed_action(
            CommandReportSpec::new("Delete Vertex", format!("{:?}", hit.object_id)),
            format!("Deleted vertex {} from polyline {:?}", hit.vertex_index, hit.object_id),
        );
        self.invalidate_geometry();
        self.invalidate_overlay();
        true
    }

    fn delete_polyline_vertex_hit(&mut self) -> Option<DeleteVertexHit> {
        self.refresh_snap_index();
        let graphics = self.graphics.as_ref()?;
        let cursor_px = self.editor.cursor_screen_px?;
        let (object_id, point, world) = pick::pick_nearest_vertex_indexed(
            &self.scene_document,
            &self.snap_index,
            &self.editor.hidden_handles,
            &self.editor.frozen_handles,
            &graphics.view_proj(),
            graphics.screen_size_pub(),
            graphics.window_to_viewport_px(cursor_px),
            PICK_THRESHOLD_PX * 2.0,
            pick::VertexPickFilter::DeletablePolyline,
        )?;
        let ObjectPoint::Vertex(vertex_index) = point else {
            return None;
        };
        let screen_px = graphics.world_to_window_px(&graphics.view_proj(), world)?;
        Some(DeleteVertexHit {
            object_id,
            vertex_index,
            screen_px,
            world,
        })
    }

    pub(crate) fn delete_selection(&mut self) {
        if self.editor.selected_handles.is_empty() {
            return;
        }
        if !self.editing_ready() {
            return;
        }
        let handles: Vec<SceneEntityId> = self.editor.selected_handles.iter().copied().collect();
        let batch: Vec<Command> = handles
            .iter()
            .filter_map(|&handle| {
                if let SceneEntityId::Object(id) = handle {
                    self.active_document().get_object(id).cloned().map(Command::delete_object)
                } else {
                    None
                }
            })
            .collect();
        let deleted = batch.len();
        if deleted > 0
            && let Some(project) = self.workspace.active_project_mut()
        {
            self.history.execute(&mut project.project.document, Command::Batch(batch));
        }
        if deleted > 0 {
            self.editor.selected_handles.clear();
            // Deleting a tool target must also discard its transient markers.
            self.editor.fuse_segments.clear();
            self.editor.fuse_awaiting_endpoint = None;
            self.editor.fuse_endpoint_markers.clear();
            self.editor.fuse_chain_tail = None;
            self.editor.fuse_close_marker = None;
            self.editor.active_tool = ActiveTool::None;
            crate::logging::report_completed_action(
                CommandReportSpec::new("Delete Selection", format!("{deleted} object(s)")),
                format!("Deleted {deleted} selected object(s)"),
            );
            self.invalidate_geometry();
            self.invalidate_overlay();
        }
    }

    pub(crate) fn duplicate_selection(&mut self) {
        if self.editor.selected_handles.is_empty() {
            return;
        }
        if !self.editing_ready() {
            return;
        }
        let originals: Vec<Object> = self
            .editor
            .selected_handles
            .iter()
            .filter_map(|&handle| {
                if let SceneEntityId::Object(id) = handle {
                    self.active_document().get_object(id).cloned()
                } else {
                    None
                }
            })
            .collect();
        if originals.is_empty() {
            return;
        }
        let Some(project) = self.workspace.active_project_mut() else {
            return;
        };
        let mut copies: Vec<Object> = Vec::with_capacity(originals.len());
        for obj in &originals {
            let new_id = project.project.document.allocate_object_id();
            copies.push(obj.with_id_and_layer(new_id, obj.layer()));
        }
        let new_ids: Vec<SceneEntityId> = copies.iter().map(|o| SceneEntityId::Object(o.id())).collect();
        let count = copies.len();
        let batch = Command::Batch(copies.into_iter().map(Command::AddObject).collect());
        self.history.execute(&mut project.project.document, batch);
        self.editor.selected_handles.clear();
        for id in new_ids {
            self.editor.selected_handles.insert(id);
        }
        crate::logging::report_completed_action(
            CommandReportSpec::new("Duplicate Selection", format!("{count} object(s)")),
            format!("Duplicated {count} object(s)"),
        );
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    pub(crate) fn pick_move_vertex_target(&mut self) -> bool {
        if !self.editing_ready() {
            return false;
        }
        let Some(hit) = self.move_vertex_hit() else {
            self.editor.move_vertex_target = None;
            return false;
        };

        self.cancel_move_delta();
        if !self.activate_project_for_object(hit.object_id) {
            return false;
        }
        self.editor.move_vertex_target = Some((hit.object_id, hit.point));
        self.editor.selected_handles.clear();
        self.editor.selected_handles.insert(SceneEntityId::Object(hit.object_id));
        self.editor.move_panel_delta = [0.0; 3];
        self.invalidate_geometry();
        self.invalidate_overlay();
        true
    }

    fn move_vertex_hit(&mut self) -> Option<MoveVertexHit> {
        self.refresh_snap_index();
        let graphics = self.graphics.as_ref()?;
        let cursor_px = self.editor.cursor_screen_px?;
        // Cursor positions are physical pixels; the marker the user aims at is
        // sized in logical ones, so scale the radius to match what is drawn.
        let scale_factor = self.window.as_ref().map_or(1.0, |window| window.scale_factor() as f32);
        let (object_id, point, world) = pick::pick_nearest_vertex_indexed(
            &self.scene_document,
            &self.snap_index,
            &self.editor.hidden_handles,
            &self.editor.frozen_handles,
            &graphics.view_proj(),
            graphics.screen_size_pub(),
            graphics.window_to_viewport_px(cursor_px),
            MOVE_VERTEX_PICK_PX * scale_factor,
            pick::VertexPickFilter::AnyEditable,
        )?;
        let screen_px = graphics.world_to_window_px(&graphics.view_proj(), world)?;
        Some(MoveVertexHit {
            object_id,
            point,
            screen_px,
            world,
        })
    }
}

fn is_point_object(object: &Object) -> bool {
    matches!(object, Object::Point { .. })
}

fn without_polyline_vertex(object: &Object, vertex_index: usize) -> Option<Object> {
    let mut result = object.clone();
    let Object::Polyline { verts, closed, .. } = &mut result else {
        return None;
    };
    let minimum = if *closed { 3 } else { 2 };
    if verts.len() <= minimum || vertex_index >= verts.len() {
        return None;
    }
    verts.remove(vertex_index);
    // Removing a vertex joins its two neighbours. There is no generally exact
    // single bulge for that new segment, so make only the joined segment
    // straight. Endpoint removal from an open line keeps the untouched
    // outgoing bulges exactly as authored.
    if *closed {
        let previous = if vertex_index == 0 { verts.len() - 1 } else { vertex_index - 1 };
        verts[previous].bulge = 0.0;
    } else if vertex_index > 0 {
        verts[vertex_index - 1].bulge = 0.0;
    }
    Some(result)
}
