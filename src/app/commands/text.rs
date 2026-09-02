use crate::{
    app::{App, PICK_THRESHOLD_PX},
    i18n::{tr, tr_format},
    model::{Command, Object, ObjectColor, ObjectId, SceneEntityId},
    userspace_log,
};

const DEFAULT_TEXT_HEIGHT: f64 = 15.0;
/// Fraction of the viewport half-height used as the default text height when placing new text.
const ZOOM_TEXT_SCALE: f64 = 0.04;

impl<'a> App<'a> {
    /// Handle a canvas click while the text tool is active. Existing text opens
    /// in the editor; any other location creates a new label there.
    pub(crate) fn text_tool_click(&mut self) {
        if !self.editing_ready() {
            return;
        }

        let picked_text = self
            .graphics
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
            .and_then(|(handle, _)| match handle {
                SceneEntityId::Object(id) if matches!(self.scene_document.get_object(id), Some(Object::Text { .. })) => Some(id),
                _ => None,
            });

        if let Some(id) = picked_text {
            self.begin_text_edit(id, false);
        } else {
            self.place_text_at_cursor();
        }
    }

    fn place_text_at_cursor(&mut self) {
        if matches!(
            self.editor.snap_cursor_mode(),
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
        let color = ObjectColor::Fixed(self.editor.tool_line_color);
        let height = self.graphics.as_ref().map(|g| g.zoom() * ZOOM_TEXT_SCALE).unwrap_or(DEFAULT_TEXT_HEIGHT);
        let Some(project) = self.workspace.active_project_mut() else {
            return;
        };
        let doc = &mut project.project.document;
        let id = doc.allocate_object_id();
        doc.insert_object(Object::Text {
            id,
            layer,
            pos: world,
            content: tr!(literal = "Text"),
            height,
            rotation: 0.0,
            color,
        });
        self.invalidate_geometry();
        self.begin_text_edit(id, true);
        // Placement is one-shot, but the text editor remains open for the
        // newly created object until the user applies or discards it.
        self.editor.active_tool = crate::ui::state::ActiveTool::None;
    }

    fn begin_text_edit(&mut self, object_id: ObjectId, created: bool) {
        let Some(document) = self.workspace.active_document() else {
            return;
        };
        let Some(
            object @ Object::Text {
                layer, content, height, rotation, ..
            },
        ) = document.get_object(object_id)
        else {
            return;
        };
        let (layer, content, height, rotation_degrees) = (*layer, content.clone(), *height, rotation.to_degrees());
        let pending_color = document.object_rgba(object);

        self.editor.active_layer = Some(layer);
        self.editor.selected_handles.clear();
        self.editor.selected_handles.insert(SceneEntityId::Object(object_id));
        self.editor.editing_labels_id = Some(object_id);
        self.editor.pending_text = content;
        self.editor.pending_text_height = height;
        self.editor.pending_text_rotation_degrees = rotation_degrees;
        self.editor.pending_text_color = pending_color;
        self.editor.text_edit_dialog_px = self.editor.cursor_screen_px;
        self.editor.text_edit_position_frames = 2;
        self.editor.text_edit_focus_requested = true;
        self.editor.text_edit_created = created;
        self.editor.text_editing_enabled = true;
        self.invalidate_geometry();
    }

    pub(crate) fn commit_text_edit(&mut self, object_id: ObjectId, content: String, height: f64, rotation_degrees: f64, color: [f32; 4]) {
        let Some(document) = self.workspace.active_document_mut() else {
            self.finish_text_edit_state();
            return;
        };
        let Some(before) = document.get_object(object_id).cloned() else {
            self.finish_text_edit_state();
            return;
        };
        let original_resolved_color = document.object_rgba(&before);
        let preserve_by_layer = matches!(before, Object::Text { color: ObjectColor::ByLayer, .. }) && color == original_resolved_color;
        let mut after = before.clone();
        if let Object::Text {
            content: current_content,
            height: current_height,
            rotation: current_rotation,
            color: current_color,
            ..
        } = &mut after
        {
            *current_content = content;
            *current_height = height.max(0.001);
            *current_rotation = rotation_degrees.to_radians();
            *current_color = if preserve_by_layer { ObjectColor::ByLayer } else { ObjectColor::Fixed(color) };
        }
        let created = self.editor.text_edit_created;
        let changed = before != after;
        if created {
            document.remove_object(object_id);
            document.insert_object(after.clone());
            self.record_applied_edit(Command::AddObject(after));
        } else if changed {
            self.execute_edit(Command::Replace { before, after });
        }
        self.finish_text_edit_state();
        if created || changed {
            userspace_log!("{}", tr_format!(literal = "Updated text on object %object_id%", object_id = format!("{object_id:?}")));
        }
        userspace_log!(
            "{}",
            tr_format!(literal = "Finished text edit for object %object_id%", object_id = format!("{object_id:?}"))
        );
        self.invalidate_geometry();
    }

    pub(crate) fn cancel_text_edit(&mut self) {
        let created = self.editor.text_edit_created;
        let object_id = self.editor.editing_labels_id;
        if created
            && let Some(object_id) = object_id
            && let Some(project_index) = self.workspace.project_index_for_object(object_id)
            && let Some(project) = self.workspace.projects.get_mut(project_index)
            && project.project.document.get_object(object_id).is_some()
        {
            project.project.document.remove_object(object_id);
            self.editor.selected_handles.remove(&SceneEntityId::Object(object_id));
        }
        self.finish_text_edit_state();
        self.invalidate_geometry();
    }

    fn finish_text_edit_state(&mut self) {
        self.editor.text_editing_enabled = false;
        self.editor.editing_labels_id = None;
        self.editor.text_edit_dialog_px = None;
        self.editor.text_edit_position_frames = 0;
        self.editor.text_edit_focus_requested = false;
        self.editor.text_edit_created = false;
    }
}
