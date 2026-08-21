use crate::{
    app::{App, PICK_THRESHOLD_PX},
    logging::CommandReportSpec,
    model::{Command, Object, ObjectId, PolyVertex, SceneEntityId},
    rendering::pick,
    ui::state::ActiveTool,
    userspace_warn,
};

impl<'a> App<'a> {
    pub(crate) fn split_at_points_click(&mut self) {
        if self.editor.split_poly_id.is_none() {
            self.pick_split_polyline();
        } else if self.editor.split_selected_verts[0].is_none() || self.editor.split_selected_verts[1].is_none() {
            self.pick_split_vertex();
        }
    }

    pub(crate) fn split_init_from_selection(&mut self) {
        let selected = self
            .editor
            .selected_handles
            .iter()
            .filter_map(|handle| match handle {
                SceneEntityId::Object(id) => Some(*id),
                _ => None,
            })
            .find(|id| self.is_valid_split_polyline(*id));

        if let Some(id) = selected {
            self.editor.split_poly_id = Some(id);
            self.editor.split_selected_verts = [None; 2];
            self.invalidate_overlay();
        }
    }

    fn pick_split_polyline(&mut self) {
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));

        let Some((SceneEntityId::Object(object_id), _)) = picked else {
            return;
        };
        if !self.activate_project_for_object(object_id) || !self.is_valid_split_polyline(object_id) {
            return;
        }

        self.editor.split_poly_id = Some(object_id);
        self.editor.split_selected_verts = [None; 2];
        self.editor.selected_handles.insert(SceneEntityId::Object(object_id));
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    fn pick_split_vertex(&mut self) {
        let Some(object_id) = self.editor.split_poly_id else {
            return;
        };
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        let Some(cursor_px) = self.editor.cursor_screen_px else {
            return;
        };

        let (verts, closed) = match self.scene_document.get_object(object_id) {
            Some(Object::Polyline { verts, closed, .. }) => (verts.clone(), *closed),
            _ => return,
        };

        let vp = graphics.view_proj();
        let screen = graphics.screen_size_pub();
        let cursor = glam::DVec2::new(f64::from(cursor_px.0), f64::from(cursor_px.1));
        let mut best_dist = (PICK_THRESHOLD_PX * 2.5) as f64;
        let mut best_idx = None;

        for (index, vertex) in verts.iter().enumerate() {
            if let Some(screen_pos) = pick::world_to_screen(&vp, vertex.pos, screen) {
                let dist = screen_pos.distance(cursor);
                if dist < best_dist {
                    best_dist = dist;
                    best_idx = Some(index);
                }
            }
        }

        let Some(vertex_index) = best_idx else {
            return;
        };

        // An open line needs only one interior split point. Its endpoints
        // would leave an empty piece and are therefore not actionable.
        if !closed {
            if vertex_index == 0 || vertex_index + 1 == verts.len() {
                userspace_warn!("Split At Points: choose an interior vertex of the open line");
                return;
            }
            self.editor.split_selected_verts = [Some(vertex_index), None];
            self.commit_split_at_points();
            return;
        }

        if self.editor.split_selected_verts[0].is_none() {
            self.editor.split_selected_verts[0] = Some(vertex_index);
            self.invalidate_overlay();
            return;
        }

        if self.editor.split_selected_verts[0] == Some(vertex_index) {
            return;
        }

        self.editor.split_selected_verts[1] = Some(vertex_index);
        self.commit_split_at_points();
    }

    fn commit_split_at_points(&mut self) {
        let Some(object_id) = self.editor.split_poly_id else {
            return;
        };
        let [Some(first), second] = self.editor.split_selected_verts else {
            return;
        };
        if !self.activate_project_for_object(object_id) {
            return;
        }

        let Some(project) = self.workspace.active_project_mut() else {
            return;
        };
        let doc = &mut project.project.document;
        let Some(source) = doc.get_object(object_id).cloned() else {
            return;
        };
        let Object::Polyline {
            layer,
            verts,
            color,
            fill,
            line_weight,
            closed,
            ..
        } = &source
        else {
            return;
        };

        let pieces = if *closed {
            let Some(second) = second else {
                return;
            };
            split_closed_ring(verts, first, second).ok_or("Split At Points: choose two non-adjacent polyline vertices")
        } else {
            split_open_line(verts, first).ok_or("Split At Points: choose an interior vertex of the open line")
        };
        let (first_ring, second_ring) = match pieces {
            Ok(pieces) => pieces,
            Err(message) => {
                userspace_warn!("{message}");
                self.editor.split_selected_verts = [None; 2];
                self.invalidate_overlay();
                return;
            }
        };

        let first_id = doc.allocate_object_id();
        let second_id = doc.allocate_object_id();
        let first_object = Object::Polyline {
            id: first_id,
            layer: *layer,
            verts: first_ring,
            closed: false,
            color: *color,
            fill: *fill,
            line_weight: *line_weight,
        };
        let second_object = Object::Polyline {
            id: second_id,
            layer: *layer,
            verts: second_ring,
            closed: false,
            color: *color,
            fill: *fill,
            line_weight: *line_weight,
        };

        self.history.execute(
            doc,
            Command::Batch(vec![Command::delete_object(source), Command::AddObject(first_object), Command::AddObject(second_object)]),
        );

        self.editor.selected_handles.clear();
        self.clear_split_at_points_state();
        self.editor.active_tool = ActiveTool::None;
        crate::logging::report_completed_action(
            CommandReportSpec::new("Split Line", "Created 2 open polylines"),
            "Split source polyline into two open polylines",
        );
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    pub(crate) fn cancel_split_at_points(&mut self) {
        self.clear_split_at_points_state();
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_overlay();
    }

    fn clear_split_at_points_state(&mut self) {
        self.editor.split_poly_id = None;
        self.editor.split_selected_verts = [None; 2];
        self.editor.split_poly_verts_screen_px.clear();
    }

    fn is_valid_split_polyline(&self, object_id: ObjectId) -> bool {
        self.scene_document.get_object(object_id).is_some_and(|object| {
            matches!(
                object,
                Object::Polyline { verts, closed: true, .. } if verts.len() >= 4
            ) || matches!(
                object,
                Object::Polyline { verts, closed: false, .. } if verts.len() >= 3
            )
        })
    }
}

fn split_open_line(verts: &[PolyVertex], split_index: usize) -> Option<(Vec<PolyVertex>, Vec<PolyVertex>)> {
    if verts.len() < 3 || split_index == 0 || split_index + 1 >= verts.len() {
        return None;
    }
    let mut first = verts[..=split_index].to_vec();
    first.last_mut().expect("the first piece contains the split vertex").bulge = 0.0;
    let second = verts[split_index..].to_vec();
    Some((first, second))
}

fn split_closed_ring(verts: &[PolyVertex], first: usize, second: usize) -> Option<(Vec<PolyVertex>, Vec<PolyVertex>)> {
    if first == second || first >= verts.len() || second >= verts.len() || verts.len() < 4 {
        return None;
    }

    let first_ring = ring_slice(verts, first, second);
    let second_ring = ring_slice(verts, second, first);
    if first_ring.len() < 3 || second_ring.len() < 3 {
        return None;
    }

    Some((with_straight_closing_edge(first_ring), with_straight_closing_edge(second_ring)))
}

fn ring_slice(verts: &[PolyVertex], start: usize, end: usize) -> Vec<PolyVertex> {
    let mut out = Vec::new();
    let mut index = start;
    loop {
        out.push(verts[index]);
        if index == end {
            break;
        }
        index = (index + 1) % verts.len();
    }
    out
}

fn with_straight_closing_edge(mut verts: Vec<PolyVertex>) -> Vec<PolyVertex> {
    if let Some(last) = verts.last_mut() {
        last.bulge = 0.0;
    }
    verts
}
