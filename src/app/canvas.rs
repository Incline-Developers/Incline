use crate::{
    app::{App, PICK_THRESHOLD_PX},
    model::{Command, Object, SceneEntityId},
    ui::state::{ActiveTool, DrapePhase, SelectionMode, TriangulationPickTarget},
};

impl<'a> App<'a> {
    pub(crate) fn begin_select_or_drag(&mut self) {
        self.pending_topology_click = None;

        if let Some(target) = self.editor.triangulation_pick_target {
            let picked = self
                .graphics
                .as_ref()
                .and_then(|graphics| graphics.pick_triangulation_at_cursor(&self.triangulations, &self.editor.hidden_handles, &self.editor.frozen_handles));
            if let Some((SceneEntityId::Triangulation(id), _)) = picked {
                let name = self
                    .triangulations
                    .iter()
                    .find(|triangulation| triangulation.id == id)
                    .map(|triangulation| triangulation.name.clone())
                    .unwrap_or_else(|| "surface".to_owned());
                self.apply_triangulation_field_pick(target, id, &name);
            }
            return;
        }

        // Drape owns a two-stage selection session. Defer both click and box
        // picks until release so each stage can strictly filter entity types.
        if self.editor.active_tool == ActiveTool::DrapeToTopology {
            self.editor.selection_box_start_px = self.editor.cursor_screen_px;
            self.editor.selection_box_current_px = self.editor.cursor_screen_px;
            return;
        }

        // Polyline-pick mode for cut-by-polyline: intercept the click and look for a closed polyline.
        if self.editor.tri_cut_poly_awaiting_pick {
            let frozen = &self.editor.frozen_handles;
            if let Some((SceneEntityId::Object(oid), _)) = self
                .graphics
                .as_ref()
                .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &[], &self.editor.hidden_handles, frozen, self.editor.xray_enabled))
            {
                let is_closed_poly = self.scene_document.get_object(oid).is_some_and(|o| {
                    matches!(
                        o,
                        Object::Polyline {
                            closed: true,
                            verts,
                            ..
                        } if verts.len() >= 3
                    )
                });
                if is_closed_poly {
                    let name = self
                        .scene_document
                        .get_object(oid)
                        .and_then(|o| {
                            let layer_id = o.layer();
                            self.scene_document.layer(layer_id).map(|l| format!("Polyline on '{}'", l.name))
                        })
                        .unwrap_or_else(|| "Polyline".to_owned());
                    self.editor.tri_cut_poly_object_id = Some(oid);
                    self.editor.tri_cut_poly_object_name = name;
                    self.editor.tri_cut_poly_awaiting_pick = false;
                    self.editor.viewport_pick_hover_label = None;
                    self.editor.tool_highlight_id = Some(oid);
                    self.invalidate_geometry();
                }
            }
            return;
        }

        // In triangulation creation mode, every canvas press starts a potential
        // box selection. On release, a short press becomes a normal click-pick.
        // This allows box drags to begin over a polyline instead of requiring
        // empty space. Explicit field/polyline pickers above take priority over
        // this broader selection mode when dialogs overlap.
        if self.editor.tri_create_open {
            self.editor.selection_box_start_px = self.editor.cursor_screen_px;
            self.editor.selection_box_current_px = self.editor.cursor_screen_px;
            return;
        }

        if !self.workspace.has_active_project() && self.triangulations.is_empty() {
            return;
        }
        if self.editor.active_tool == crate::ui::state::ActiveTool::Move {
            self.cancel_move_delta();
            self.editor.move_vertex_target = None;
        }
        let frozen = &self.editor.frozen_handles;
        let picked = self.graphics.as_ref().and_then(|graphics| {
            graphics
                .pick_scene_entity_at_cursor(
                    PICK_THRESHOLD_PX,
                    &self.triangulations,
                    &self.drill_holes,
                    &self.editor.hidden_handles,
                    frozen,
                    self.editor.xray_enabled,
                )
                .map(|(handle, world)| (Some(handle), world))
                .or_else(|| graphics.cursor_world(self.editor.z_level).map(|world| (None, world)))
        });
        match picked {
            Some((Some(handle), world)) => {
                if matches!(handle, SceneEntityId::Triangulation(_)) {
                    self.pending_topology_click = Some((handle, world));
                    self.editor.selection_box_start_px = self.editor.cursor_screen_px;
                    self.editor.selection_box_current_px = self.editor.cursor_screen_px;
                    return;
                }

                // Selecting an object may retarget the active project, but never the
                // active layer: that is owned solely by the toolbar layer selector.
                if let SceneEntityId::Object(object_id) = handle {
                    self.activate_project_for_object(object_id);
                }
                let selection_mode = if self.modifiers.shift_key() {
                    SelectionMode::Toggle
                } else if self.modifiers.control_key() {
                    SelectionMode::Add
                } else if matches!(
                    handle,
                    SceneEntityId::Triangulation(_) | SceneEntityId::BlockModel(_) | SceneEntityId::DrillHole(_) | SceneEntityId::PointCloud(_)
                ) && self.editor.selected_handles.contains(&handle)
                {
                    SelectionMode::Toggle
                } else {
                    SelectionMode::Replace
                };
                self.editor.on_canvas_pick(handle, world, selection_mode);
                self.active_triangulation = match handle {
                    SceneEntityId::Triangulation(id) if self.editor.selected_handles.contains(&handle) => Some(id),
                    _ => None,
                };
            }
            Some((None, world)) => {
                self.active_triangulation = None;
                if matches!(self.editor.active_tool, crate::ui::state::ActiveTool::None | crate::ui::state::ActiveTool::Move) {
                    self.editor.selection_box_start_px = self.editor.cursor_screen_px;
                    self.editor.selection_box_current_px = self.editor.cursor_screen_px;
                } else if self.workspace.has_active_project() {
                    self.editor.on_canvas_click(world);
                } else {
                    self.editor.selected_handles.clear();
                }
            }
            None => {
                // A background click may not intersect the current Z plane
                // (for example, when clicking above the horizon). It should
                // still begin a selection gesture so a short click can clear
                // the current selection.
                self.active_triangulation = None;
                if matches!(self.editor.active_tool, crate::ui::state::ActiveTool::None | crate::ui::state::ActiveTool::Move) {
                    self.editor.selection_box_start_px = self.editor.cursor_screen_px;
                    self.editor.selection_box_current_px = self.editor.cursor_screen_px;
                } else {
                    return;
                }
            }
        }
        self.invalidate_geometry();
    }

    /// Update the candidate highlight and description shown by dialog-driven
    /// viewport pickers. Work is invoked at the existing bounded hover-poll
    /// rate, and geometry is invalidated only when the candidate changes.
    pub(crate) fn update_viewport_field_pick_hover(&mut self) {
        if self.editor.triangulation_pick_target.is_some() {
            let hovered = self
                .graphics
                .as_ref()
                .and_then(|graphics| graphics.pick_triangulation_at_cursor(&self.triangulations, &self.editor.hidden_handles, &self.editor.frozen_handles))
                .and_then(|(entity, _)| match entity {
                    SceneEntityId::Triangulation(id) => Some(id),
                    _ => None,
                });
            let mut next_handles = std::collections::HashSet::new();
            if let Some(id) = hovered {
                next_handles.insert(SceneEntityId::Triangulation(id));
            }
            let next_label = hovered.and_then(|id| {
                self.triangulations
                    .iter()
                    .find(|triangulation| triangulation.id == id)
                    .map(|triangulation| format!("Surface | {}", triangulation.name))
            });
            if self.editor.tri_hover_handles != next_handles || self.editor.viewport_pick_hover_label != next_label {
                self.editor.tri_hover_handles = next_handles;
                self.editor.viewport_pick_hover_label = next_label;
                self.invalidate_geometry();
            }
            return;
        }

        if !self.editor.tri_cut_poly_awaiting_pick {
            return;
        }

        let raw_hover = self.graphics.as_ref().and_then(|graphics| {
            graphics
                .pick_at_cursor(PICK_THRESHOLD_PX, &[], &self.editor.hidden_handles, &self.editor.frozen_handles, self.editor.xray_enabled)
                .map(|(entity, _)| entity)
        });
        let (next_highlight, next_label) = match raw_hover {
            Some(SceneEntityId::Object(id)) => match self.scene_document.get_object(id) {
                Some(object @ Object::Polyline { closed: true, verts, .. }) if verts.len() >= 3 => {
                    let layer = self.scene_document.layer(object.layer()).map(|layer| layer.name.as_str()).unwrap_or("?");
                    (Some(id), Some(format!("Polyline | Layer: {layer} | {} vertices", verts.len())))
                }
                _ => (None, Some("Not selectable | Choose a closed polyline".to_owned())),
            },
            Some(_) => (None, Some("Not selectable | Choose a closed polyline".to_owned())),
            None => (None, None),
        };
        if self.editor.tool_highlight_id != next_highlight || self.editor.viewport_pick_hover_label != next_label {
            self.editor.tool_highlight_id = next_highlight;
            self.editor.viewport_pick_hover_label = next_label;
            self.invalidate_geometry();
        }
    }

    fn apply_triangulation_field_pick(&mut self, target: TriangulationPickTarget, id: crate::model::triangulation::TriangulationId, name: &str) {
        match target {
            TriangulationPickTarget::ClipSurface => {
                self.editor.tri_cut_poly_tri_id = Some(id);
                update_auto_derived_name(&mut self.editor.tri_cut_poly_name_input, self.editor.tri_cut_poly_name_auto, name, "Clipped");
            }
            TriangulationPickTarget::SliceSurface => {
                self.editor.tri_cut_z_tri_id = Some(id);
                update_auto_derived_name(&mut self.editor.tri_cut_z_name_input, self.editor.tri_cut_z_name_auto, name, "Sliced");
            }
            TriangulationPickTarget::TrimTopology => {
                self.editor.tri_cut_surface_reference_id = Some(id);
                if self.editor.tri_cut_surface_target_id == Some(id) {
                    self.editor.tri_cut_surface_target_id = None;
                }
            }
            TriangulationPickTarget::TrimSurface => {
                self.editor.tri_cut_surface_target_id = Some(id);
                if self.editor.tri_cut_surface_reference_id == Some(id) {
                    self.editor.tri_cut_surface_reference_id = None;
                }
                update_auto_derived_name(&mut self.editor.tri_cut_surface_name_input, self.editor.tri_cut_surface_name_auto, name, "Trimmed");
            }
            TriangulationPickTarget::CutPitTopology => {
                self.editor.tri_cut_pitshell_topology_id = Some(id);
                if self.editor.tri_cut_pitshell_pitshell_id == Some(id) {
                    self.editor.tri_cut_pitshell_pitshell_id = None;
                }
                update_auto_derived_name(&mut self.editor.tri_cut_pitshell_name_input, self.editor.tri_cut_pitshell_name_auto, name, "Cut");
            }
            TriangulationPickTarget::CutPitShell => {
                self.editor.tri_cut_pitshell_pitshell_id = Some(id);
                if self.editor.tri_cut_pitshell_topology_id == Some(id) {
                    self.editor.tri_cut_pitshell_topology_id = None;
                }
            }
            TriangulationPickTarget::IncludeTopology => {
                self.editor.tri_include_solid_topology_id = Some(id);
                if self.editor.tri_include_solid_shape_id == Some(id) {
                    self.editor.tri_include_solid_shape_id = None;
                }
                update_auto_derived_name(&mut self.editor.tri_include_solid_name_input, self.editor.tri_include_solid_name_auto, name, "With Shell");
            }
            TriangulationPickTarget::IncludeShape => {
                self.editor.tri_include_solid_shape_id = Some(id);
                if self.editor.tri_include_solid_topology_id == Some(id) {
                    self.editor.tri_include_solid_topology_id = None;
                }
            }
            TriangulationPickTarget::ContourSurface => {
                self.editor.tri_contour_tri_id = Some(id);
                self.editor.update_contour_layer_name_from_surface(name);
            }
        }
        self.editor.triangulation_pick_target = None;
        self.editor.viewport_pick_hover_label = None;
        self.editor.tri_hover_handles.clear();
        self.invalidate_geometry();
    }

    pub(crate) fn finish_box_selection(&mut self) {
        let (Some(start), Some(end)) = (self.editor.selection_box_start_px.take(), self.editor.selection_box_current_px.take()) else {
            self.pending_topology_click = None;
            return;
        };
        let dragged = (end.0 - start.0).abs().max((end.1 - start.1).abs()) >= 3.0;
        let pending_topology_click = self.pending_topology_click.take();

        if self.editor.active_tool == ActiveTool::DrapeToTopology {
            self.finish_drape_selection(start, end, dragged);
            return;
        }

        // Delete Points tool: box drag selects point objects and confirms deletion; single click
        // deletes the point (polyline vertex or point object) at the cursor. Polylines and other
        // elements are deliberately never picked, so the tool can't delete a whole element.
        if self.editor.active_tool == crate::ui::state::ActiveTool::DeletePoints {
            if dragged {
                let cross_select = end.0 > start.0;
                let enclosed = self
                    .graphics
                    .as_ref()
                    .map(|g| {
                        if cross_select {
                            g.entities_touching_screen_rect(start, end, &self.editor.frozen_handles)
                        } else {
                            g.entities_in_screen_rect(start, end, &self.editor.frozen_handles)
                        }
                    })
                    .unwrap_or_default();
                let active_object_ids = self.active_project_object_ids();
                self.editor.selected_handles.clear();
                for handle in enclosed {
                    if let SceneEntityId::Object(id) = handle
                        && active_object_ids.contains(&id)
                        && matches!(self.scene_document.get_object(id), Some(Object::Point { .. }))
                    {
                        self.editor.selected_handles.insert(handle);
                    }
                }
                // Always invalidate geometry so deselected objects lose their highlight color.
                self.invalidate_geometry();
                if !self.editor.selected_handles.is_empty() {
                    self.editor.delete_confirm_open = true;
                }
            } else {
                self.delete_at_cursor();
            }
            return;
        }

        // In triangulation creation mode, drag-select adds eligible source objects to the tri pick list.
        if self.editor.tri_create_open {
            if dragged {
                // Same left/right direction convention as regular selection.
                let cross_select = end.0 > start.0;
                let enclosed = self
                    .graphics
                    .as_ref()
                    .map(|g| {
                        if cross_select {
                            g.entities_touching_screen_rect(start, end, &self.editor.frozen_handles)
                        } else {
                            g.entities_in_screen_rect(start, end, &self.editor.frozen_handles)
                        }
                    })
                    .unwrap_or_default();
                let mut added = 0usize;
                for handle in enclosed {
                    if let SceneEntityId::Object(oid) = handle
                        && !self.editor.tri_selected_object_ids.contains(&oid)
                        && self.scene_document.get_object(oid).is_some_and(is_triangulation_polyline)
                    {
                        self.editor.tri_selected_object_ids.push(oid);
                        self.editor.selected_handles.insert(SceneEntityId::Object(oid));
                        added += 1;
                    }
                }
                if added > 0 {
                    self.invalidate_geometry();
                }
            } else {
                self.tri_pick_at_cursor();
            }
            return;
        }

        if !dragged {
            if let Some((handle, world)) = pending_topology_click {
                let selection_mode = if self.modifiers.shift_key() {
                    SelectionMode::Toggle
                } else if self.modifiers.control_key() {
                    SelectionMode::Add
                } else if self.editor.selected_handles.contains(&handle) {
                    SelectionMode::Toggle
                } else {
                    SelectionMode::Replace
                };
                self.editor.on_canvas_pick(handle, world, selection_mode);
                self.active_triangulation = match handle {
                    SceneEntityId::Triangulation(id) if self.editor.selected_handles.contains(&handle) => Some(id),
                    _ => None,
                };
                self.invalidate_geometry();
                return;
            }

            let preserve = self.modifiers.shift_key() || self.modifiers.control_key();
            if !preserve {
                self.editor.selected_handles.clear();
                self.active_triangulation = None;
                if self.editor.active_tool == crate::ui::state::ActiveTool::Move {
                    self.editor.move_vertex_target = None;
                }
            }
            self.invalidate_geometry();
            return;
        }

        // Box selection: left-to-right (end.x > start.x) = cross select (any vertex
        // inside box); right-to-left (end.x < start.x) = window select (all vertices inside).
        let cross_select = end.0 > start.0;
        let mut enclosed = self
            .graphics
            .as_ref()
            .map(|graphics| {
                if cross_select {
                    graphics.entities_touching_screen_rect(start, end, &self.editor.frozen_handles)
                } else {
                    graphics.entities_in_screen_rect(start, end, &self.editor.frozen_handles)
                }
            })
            .unwrap_or_default();
        let active_object_ids = self.active_project_object_ids();
        enclosed.retain(|handle| match handle {
            SceneEntityId::Object(object_id) => active_object_ids.contains(object_id),
            SceneEntityId::Triangulation(_) => true,
            SceneEntityId::BlockModel(_) => true,
            SceneEntityId::DrillHole(_) => true,
            SceneEntityId::PointCloud(_) => true,
        });
        if self.modifiers.shift_key() {
            for handle in enclosed {
                if !self.editor.selected_handles.remove(&handle) {
                    self.editor.selected_handles.insert(handle);
                }
            }
        } else {
            if !self.modifiers.control_key() {
                self.editor.selected_handles.clear();
            }
            self.editor.selected_handles.extend(enclosed);
        }
        if self.editor.active_tool == crate::ui::state::ActiveTool::Move {
            self.editor.move_vertex_target = None;
        }
        self.invalidate_geometry();
    }

    fn finish_drape_selection(&mut self, start: (f32, f32), end: (f32, f32), dragged: bool) {
        let mut candidates = if dragged {
            let cross_select = end.0 > start.0;
            self.graphics
                .as_ref()
                .map(|graphics| {
                    if cross_select {
                        graphics.entities_touching_screen_rect(start, end, &self.editor.frozen_handles)
                    } else {
                        graphics.entities_in_screen_rect(start, end, &self.editor.frozen_handles)
                    }
                })
                .unwrap_or_default()
        } else {
            self.graphics
                .as_ref()
                .and_then(|graphics| match self.editor.drape_phase {
                    DrapePhase::Designs => graphics
                        .pick_at_cursor(PICK_THRESHOLD_PX, &[], &self.editor.hidden_handles, &self.editor.frozen_handles, self.editor.xray_enabled)
                        .map(|(handle, _)| vec![handle]),
                    DrapePhase::Topologies => graphics
                        .pick_triangulation_at_cursor(&self.triangulations, &self.editor.hidden_handles, &self.editor.frozen_handles)
                        .map(|(handle, _)| vec![handle]),
                })
                .unwrap_or_default()
        };

        let active_object_ids = self.active_project_object_ids();
        candidates.retain(|handle| match (self.editor.drape_phase, handle) {
            (DrapePhase::Designs, SceneEntityId::Object(id)) => active_object_ids.contains(id),
            (DrapePhase::Topologies, SceneEntityId::Triangulation(_)) => true,
            _ => false,
        });

        if candidates.is_empty() {
            if !self.modifiers.shift_key() && !self.modifiers.control_key() {
                self.editor.selected_handles.clear();
            }
            self.invalidate_geometry();
            return;
        }

        if self.modifiers.shift_key() {
            for handle in candidates {
                if !self.editor.selected_handles.remove(&handle) {
                    self.editor.selected_handles.insert(handle);
                }
            }
        } else if self.modifiers.control_key() {
            self.editor.selected_handles.extend(candidates);
        } else if self.editor.drape_phase == DrapePhase::Topologies && !dragged && candidates.len() == 1 && self.editor.selected_handles.contains(&candidates[0]) {
            self.editor.selected_handles.remove(&candidates[0]);
        } else {
            self.editor.selected_handles.clear();
            self.editor.selected_handles.extend(candidates);
        }
        self.invalidate_geometry();
    }

    /// Click-pick in triangulation creation mode: toggle the picked object in the tri selection.
    fn tri_pick_at_cursor(&mut self) {
        let Some(picked) = self.graphics.as_ref().and_then(|g| {
            g.pick_at_cursor(
                PICK_THRESHOLD_PX,
                &self.triangulations,
                &self.editor.hidden_handles,
                &self.editor.frozen_handles,
                self.editor.xray_enabled,
            )
        }) else {
            return;
        };
        let (handle, _world) = picked;
        let SceneEntityId::Object(oid) = handle else {
            return;
        };
        if self.editor.tri_selected_object_ids.contains(&oid) {
            self.editor.tri_selected_object_ids.retain(|&o| o != oid);
            self.editor.selected_handles.remove(&SceneEntityId::Object(oid));
        } else if self.scene_document.get_object(oid).is_some_and(is_triangulation_polyline) {
            self.editor.tri_selected_object_ids.push(oid);
            self.editor.selected_handles.insert(SceneEntityId::Object(oid));
        }
        self.invalidate_geometry();
    }

    pub(crate) fn drag_to_cursor(&mut self) {
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        let (object_id, plane_z, last) = (drag.object_id, drag.plane_z, drag.last_world);
        let Some(world) = self.graphics.as_ref().and_then(|g| g.cursor_world(plane_z)) else {
            return;
        };
        let delta = world - last;
        if delta.length_squared() <= 0.0 {
            return;
        }
        if let Some(doc) = self.workspace.active_document_mut() {
            doc.translate_object(object_id, delta);
        }
        if let Some(drag) = self.drag.as_mut() {
            drag.last_world = world;
            drag.moved = true;
        }
        self.invalidate_geometry();
    }

    pub(crate) fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if drag.moved
            && let Some(after) = self.active_document().get_object(drag.object_id).cloned()
        {
            self.history.push_applied(Command::Replace { before: drag.before, after });
        }
    }

    pub(crate) fn active_project_object_ids(&self) -> std::collections::HashSet<crate::model::ObjectId> {
        self.workspace
            .active_project()
            .map(|project| project.project.document.objects().iter().map(Object::id).collect())
            .unwrap_or_default()
    }
}

/// Standardised name for a triangulation derived from another: the source's
/// stem verbatim (its own casing is left alone), then a Title-Case operation
/// word - e.g. `"minedesign.dxf"` with `"Clipped"` becomes `"minedesign Clipped"`.
pub(crate) fn derived_triangulation_name(name: &str, suffix: &str) -> String {
    let stem = std::path::Path::new(name).file_stem().and_then(|value| value.to_str()).unwrap_or(name).trim();
    if stem.is_empty() { name.to_owned() } else { format!("{stem} {suffix}") }
}

fn update_auto_derived_name(output: &mut String, is_auto: bool, source: &str, suffix: &str) {
    if is_auto {
        *output = derived_triangulation_name(source, suffix);
    }
}

pub(crate) fn is_triangulation_polyline(obj: &Object) -> bool {
    matches!(
        obj,
        Object::Polyline {
            verts,
            closed,
            ..
        } if verts.len() >= if *closed { 3 } else { 2 }
    )
}
