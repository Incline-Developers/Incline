use crate::{
    app::{App, PICK_THRESHOLD_PX},
    logging::CommandReportSpec,
    model::{Command, Object, ObjectId, SceneEntityId},
    ui::state::ActiveTool,
};

impl<'a> App<'a> {
    pub(crate) fn explode_at_cursor(&mut self) {
        if !self.editing_ready() {
            return;
        }
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));
        let Some((SceneEntityId::Object(object_id), _)) = picked else {
            return;
        };
        if !self.activate_project_for_object(object_id) {
            return;
        }
        self.editor.tool_highlight_id = None;
        self.explode_polyline(object_id);
    }

    /// Update the hover highlight for the Explode tool on cursor move.
    pub(crate) fn update_explode_hover(&mut self) {
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));
        let hovered = picked.and_then(|(h, _)| match h {
            SceneEntityId::Object(id)
                if self
                    .workspace
                    .active_document()
                    .and_then(|document| document.get_object(id))
                    .is_some_and(|object| matches!(object, Object::Polyline { .. })) =>
            {
                Some(id)
            }
            _ => None,
        });
        if hovered != self.editor.tool_highlight_id {
            self.editor.tool_highlight_id = hovered;
            self.invalidate_geometry();
        }
    }

    pub(crate) fn explode_polyline(&mut self, object_id: ObjectId) {
        if !self.activate_project_for_object(object_id) {
            return;
        }
        let source = match self.active_document().get_object(object_id) {
            Some(obj @ Object::Polyline { .. }) => obj.clone(),
            _ => {
                return;
            }
        };
        let (verts, layer, color, line_weight, closed) = match &source {
            Object::Polyline {
                verts,
                layer,
                color,
                line_weight,
                closed,
                ..
            } => (verts.clone(), *layer, *color, *line_weight, *closed),
            _ => return,
        };

        if verts.len() < 2 {
            return;
        }

        // Build edge segments.
        let edge_count = if closed { verts.len() } else { verts.len() - 1 };
        let mut batch_cmds = vec![Command::delete_object(source)];

        if let Some(project) = self.workspace.active_project_mut() {
            let doc = &mut project.project.document;
            for i in 0..edge_count {
                let a = verts[i];
                let b = verts[(i + 1) % verts.len()];
                let id = doc.allocate_object_id();
                batch_cmds.push(Command::AddObject(Object::Polyline {
                    id,
                    layer,
                    verts: vec![a, b],
                    closed: false,
                    color,
                    fill: crate::model::FillStyle::Clear,
                    line_weight,
                }));
            }
            self.history.execute(doc, Command::Batch(batch_cmds));
        }

        self.editor.selected_handles.remove(&SceneEntityId::Object(object_id));
        self.editor.active_tool = ActiveTool::None;
        self.editor.tool_highlight_id = None;
        crate::logging::report_completed_action(
            CommandReportSpec::new(
                crate::i18n::tr!(literal = "Explode Polyline"),
                crate::i18n::tr_format!(literal = "%count% line(s)", count = edge_count),
            ),
            crate::i18n::tr_format!(literal = "Exploded polyline into %count% line segments", count = edge_count),
        );
        self.invalidate_geometry();
    }
}
