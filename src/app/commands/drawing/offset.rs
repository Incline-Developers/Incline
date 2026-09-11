use crate::{
    app::{App, PICK_THRESHOLD_PX},
    i18n::tr,
    logging::CommandReportSpec,
    model::{Command, Object, ObjectId, PolyVertex, SceneEntityId},
    ui::state::ActiveTool,
    userspace_warn,
};

impl<'a> App<'a> {
    /// Open the offset dialog for the given object, or pick from selection.
    pub(crate) fn open_offset_dialog(&mut self) {
        let object_ids = self.selected_offset_polyline_ids();
        if let Some(&first_id) = object_ids.first() {
            if !self.activate_project_for_object(first_id) {
                return;
            }
            self.editor.offset_target_id = Some(first_id);
            self.editor.offset_target_ids = object_ids;
            self.editor.offset_dialog_open = true;
            self.editor.tool_highlight_id = Some(first_id);
            self.invalidate_geometry();
        }
    }

    fn selected_offset_polyline_ids(&self) -> Vec<ObjectId> {
        self.editor
            .selected_handles
            .iter()
            .filter_map(|h| match h {
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
            })
            .collect()
    }

    /// Pick an element to offset when the tool is active but no target yet.
    pub(crate) fn pick_offset_target(&mut self) {
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));
        if let Some((SceneEntityId::Object(id), _)) = picked
            && self.activate_project_for_object(id)
            && matches!(self.active_document().get_object(id), Some(Object::Polyline { .. }))
        {
            self.editor.offset_target_id = Some(id);
            self.editor.offset_target_ids = vec![id];
            self.editor.offset_dialog_open = true;
            self.editor.tool_highlight_id = Some(id);
            self.invalidate_geometry();
        }
    }

    /// Called when the dialog Apply button is pressed. Computes horiz_dist and
    /// z_delta from dialog inputs, then enters the side-pick phase.
    pub(crate) fn begin_offset_pick(&mut self, object_ids: Vec<ObjectId>, horiz_dist: f64, z_delta: f64, project_to_rl: Option<(f64, f64)>, collide_with_triangulation: bool) {
        let Some(&first_id) = object_ids.first() else {
            return;
        };
        if !self.activate_project_for_object(first_id) {
            return;
        }
        if project_to_rl.is_none() && horiz_dist.abs() < 1e-9 && z_delta.abs() < 1e-9 {
            userspace_warn!("{}", tr!(literal = "Offset distance must be greater than zero"));
            return;
        }
        let target_ids: Vec<ObjectId> = object_ids
            .into_iter()
            .filter(|id| matches!(self.active_document().get_object(*id), Some(Object::Polyline { .. })))
            .collect();
        if target_ids.is_empty() {
            return;
        }
        self.editor.offset_target_id = target_ids.first().copied();
        self.editor.offset_target_ids = target_ids;
        self.editor.offset_horiz_dist = horiz_dist;
        self.editor.offset_z_delta = z_delta;
        self.editor.offset_project_to_rl = project_to_rl;
        self.editor.offset_collide_with_triangulation = collide_with_triangulation;
        self.editor.offset_dialog_open = false;
        self.editor.offset_awaiting_side_pick = true;
        let closed = matches!(
            self.active_document().get_object(self.editor.offset_target_ids[0]),
            Some(Object::Polyline { closed: true, .. })
        );
        self.editor.offset_preview_closed = closed;
    }

    /// Compute the offset result geometry for the current side-pick settings,
    /// choosing between a uniform offset and a per-vertex angled projection to a
    /// target absolute RL depending on `offset_project_to_rl`.
    /// `scale` multiplies the configured offset, so the planning tool can lay
    /// down its second, third and further copies through the same path.
    fn compute_offset_result(&self, src_verts: &[glam::DVec3], closed: bool, cursor_world_xy: glam::DVec2, scale: f64) -> Vec<glam::DVec3> {
        let result = if let Some((tan_angle, target_rl)) = self.editor.offset_project_to_rl {
            // Per-vertex horizontal distance implied by each vertex's own elevation,
            // used only to size the cursor-side probe below.
            let probe_dist = if tan_angle.abs() < 1e-9 {
                0.0
            } else {
                src_verts.iter().map(|v| ((target_rl - v.z) / tan_angle).abs()).fold(0.0_f64, f64::max)
            };
            let side = crate::model::geometry::offset_side_from_cursor(src_verts, closed, cursor_world_xy, probe_dist);
            crate::model::geometry::geometric_offset_project_to_rl(src_verts, closed, side, tan_angle, target_rl)
        } else {
            let abs_dist = self.editor.offset_horiz_dist.abs() * scale;
            let z_delta = self.editor.offset_z_delta * scale;
            let side = crate::model::geometry::offset_side_from_cursor(src_verts, closed, cursor_world_xy, abs_dist);
            crate::model::geometry::geometric_offset(src_verts, closed, side * abs_dist, z_delta)
        };

        // Planning cuts lie flat on one bench or flitch, where there is no
        // batter for the clamp to stop against, so the setting is ignored
        // rather than reset - the full tool keeps whatever the user chose.
        if self.editor.offset_collide_with_triangulation && !self.editor.is_planning_cut_step() {
            self.clamp_offset_to_triangulations(src_verts, &result)
        } else {
            result
        }
    }

    /// How many evenly spaced copies one side-pick should lay down.
    ///
    /// A planning cut is drawn flat on a single bench or flitch, so the tool is
    /// reduced to a spacing and repeats out to the cursor: pointing 110 m south
    /// at a 20 m spacing lays strips at 20, 40, 60, 80 and 100 m. Everywhere
    /// else the tool keeps its single-copy behaviour.
    fn offset_repeat_steps(&self, src_verts: &[glam::DVec3], cursor_world_xy: glam::DVec2) -> usize {
        /// Guards against a spacing typo turning one drag into unbounded work.
        const MAX_STEPS: usize = 500;

        if !self.editor.is_planning_cut_step() {
            return 1;
        }
        let spacing = self.editor.offset_horiz_dist.abs();
        if spacing < 1e-9 {
            return 1;
        }
        let reach = src_verts
            .windows(2)
            .map(|edge| segment_distance_xy(cursor_world_xy, edge[0].truncate(), edge[1].truncate()))
            .fold(f64::INFINITY, f64::min);
        spaced_copies(reach, spacing, MAX_STEPS)
    }

    fn clamp_offset_to_triangulations(&self, src_verts: &[glam::DVec3], proposed: &[glam::DVec3]) -> Vec<glam::DVec3> {
        const HIT_EPSILON: f64 = 1.0e-6;

        src_verts
            .iter()
            .zip(proposed.iter())
            .map(|(&source, &target)| {
                let delta = target - source;
                let length = delta.length();
                if length <= HIT_EPSILON {
                    return target;
                }

                let direction = delta / length;
                let ray_origin = source + direction * HIT_EPSILON;
                let max_hit_distance = length - HIT_EPSILON;
                let mut nearest: Option<(f64, glam::DVec3)> = None;

                for triangulation in &self.triangulations {
                    if !triangulation.state.loaded || self.editor.hidden_handles.contains(&SceneEntityId::Triangulation(triangulation.id)) {
                        continue;
                    }

                    let Some(hit) = triangulation.spatial.ray_hit(&triangulation.mesh, ray_origin, direction) else {
                        continue;
                    };
                    let distance = (hit - ray_origin).dot(direction);
                    if distance >= 0.0 && distance <= max_hit_distance + HIT_EPSILON && nearest.is_none_or(|(nearest_distance, _)| distance < nearest_distance) {
                        nearest = Some((distance, hit));
                    }
                }

                nearest.map_or(target, |(_, hit)| hit)
            })
            .collect()
    }

    /// Recompute the offset preview based on current cursor position.
    pub(crate) fn update_offset_preview(&mut self) {
        if !self.editor.offset_awaiting_side_pick {
            return;
        }
        let object_ids = self.editor.offset_target_ids.clone();
        let Some(&first_id) = object_ids.first() else {
            return;
        };
        if !self.activate_project_for_object(first_id) {
            return;
        }
        if self.editor.cursor_screen_px.is_none() {
            return;
        }
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        let cursor_world_xy = graphics.cursor_world(0.0).map(|w| glam::DVec2::new(w.x, w.y)).unwrap_or(glam::DVec2::ZERO);

        let mut source_world = Vec::new();
        let mut preview_world = Vec::new();
        let mut ranges = Vec::new();
        let mut first_closed = false;

        for object_id in object_ids {
            let (src_verts, closed) = match self.active_document().get_object(object_id) {
                Some(Object::Polyline { verts, closed, .. }) => (crate::model::geometry::tessellate_polyline_bulges(verts, *closed), *closed),
                _ => continue,
            };
            for step in 1..=self.offset_repeat_steps(&src_verts, cursor_world_xy) {
                let preview = self.compute_offset_result(&src_verts, closed, cursor_world_xy, step as f64);
                let start = preview_world.len();
                source_world.extend(src_verts.iter().copied());
                preview_world.extend(preview);
                ranges.push((start, preview_world.len(), closed));
                if ranges.len() == 1 {
                    first_closed = closed;
                }
            }
        }

        if self.editor.offset_preview_world != preview_world || self.editor.offset_preview_ranges != ranges {
            self.editor.offset_source_world = source_world;
            self.editor.offset_preview_world = preview_world;
            self.editor.offset_preview_ranges = ranges;
            self.editor.offset_preview_closed = first_closed;
        }
    }

    /// Commit the offset using the current preview side.
    pub(crate) fn commit_offset(&mut self) {
        let object_ids = self.editor.offset_target_ids.clone();
        let Some(&first_id) = object_ids.first() else {
            return;
        };
        if !self.activate_project_for_object(first_id) {
            return;
        }
        if self.editor.cursor_screen_px.is_none() {
            return;
        }
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };

        let cursor_world_xy = graphics.cursor_world(0.0).map(|w| glam::DVec2::new(w.x, w.y)).unwrap_or(glam::DVec2::ZERO);

        let mut offset_specs = Vec::new();
        for object_id in object_ids {
            let Some(Object::Polyline {
                verts,
                closed,
                layer,
                color,
                fill,
                line_weight,
                ..
            }) = self.active_document().get_object(object_id)
            else {
                continue;
            };
            let src_verts = crate::model::geometry::tessellate_polyline_bulges(verts, *closed);
            for step in 1..=self.offset_repeat_steps(&src_verts, cursor_world_xy) {
                let new_positions = self.compute_offset_result(&src_verts, *closed, cursor_world_xy, step as f64);
                let new_verts: Vec<PolyVertex> = new_positions.into_iter().map(PolyVertex::straight).collect();
                offset_specs.push((*layer, new_verts, *closed, *color, *fill, *line_weight));
            }
        }

        let offset_count = offset_specs.len();
        if offset_count == 0 {
            return;
        }

        if let Some(project) = self.workspace.active_project_mut() {
            let doc = &mut project.project.document;
            let commands = offset_specs
                .into_iter()
                .map(|(layer, verts, closed, color, fill, line_weight)| {
                    let id = doc.allocate_object_id();
                    Command::AddObject(Object::Polyline {
                        id,
                        layer,
                        verts,
                        closed,
                        color,
                        fill,
                        line_weight,
                    })
                })
                .collect();
            self.execute_edit(Command::Batch(commands));
        }

        self.cancel_offset();
        crate::logging::report_completed_action(
            CommandReportSpec::new(
                crate::i18n::tr!(literal = "Create Offset"),
                crate::i18n::tr_format!(literal = "%count% object(s)", count = offset_count),
            ),
            crate::i18n::tr_format!(literal = "Created offset of %count% object(s)", count = offset_count),
        );
        self.invalidate_geometry();
    }

    pub(crate) fn cancel_offset(&mut self) {
        self.editor.offset_dialog_open = false;
        self.editor.offset_target_id = None;
        self.editor.offset_target_ids.clear();
        self.editor.offset_awaiting_side_pick = false;
        self.editor.offset_project_to_rl = None;
        self.editor.offset_preview_world.clear();
        self.editor.offset_source_world.clear();
        self.editor.offset_preview_screen_px.clear();
        self.editor.offset_source_screen_px.clear();
        self.editor.offset_preview_ranges.clear();
        self.editor.tool_highlight_id = None;
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_geometry();
    }
}

/// Copies of a cut that fit within `reach` at `spacing`, always at least one so
/// a pick close to the source still draws the cut the user asked for.
fn spaced_copies(reach: f64, spacing: f64, max_steps: usize) -> usize {
    if !reach.is_finite() || spacing < 1e-9 {
        return 1;
    }
    ((reach / spacing).floor() as usize).clamp(1, max_steps)
}

/// Shortest XY distance from `point` to the segment `start`-`end`.
fn segment_distance_xy(point: glam::DVec2, start: glam::DVec2, end: glam::DVec2) -> f64 {
    let edge = end - start;
    let length_squared = edge.length_squared();
    if length_squared < 1e-18 {
        return point.distance(start);
    }
    let t = ((point - start).dot(edge) / length_squared).clamp(0.0, 1.0);
    point.distance(start + edge * t)
}
