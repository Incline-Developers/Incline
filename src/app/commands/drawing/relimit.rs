use glam::DVec3;

use crate::{
    app::{App, PICK_THRESHOLD_PX},
    logging::CommandReportSpec,
    model::{Command, Object, ObjectId, PolyVertex, SceneEntityId},
    ui::state::{ActiveTool, RelimitCandidate, RelimitMode, TrimEnd},
    userspace_warn,
};

impl<'a> App<'a> {
    pub(crate) fn open_relimit_dialog(&mut self) {
        // Relimiting modifies an existing object, so it isn't restricted to the
        // active layer (that restriction only matters for where *new* geometry
        // gets created).
        let object_id = self.editor.selected_handles.iter().find_map(|h| match h {
            SceneEntityId::Object(id) => {
                if matches!(self.active_document().get_object(*id), Some(Object::Polyline { closed: false, .. })) {
                    Some(*id)
                } else {
                    None
                }
            }
            _ => None,
        });
        if let Some(id) = object_id {
            self.editor.relimit_source_id = Some(id);
            self.editor.relimit_awaiting_source_pick = false;
            self.editor.relimit_dialog_open = true;
            self.editor.tool_highlight_id = None;
        } else {
            self.editor.relimit_awaiting_source_pick = true;
        }
    }

    /// Phase 0 pick: user clicked the source line while no dialog was open.
    fn pick_relimit_source(&mut self) {
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));
        if let Some((SceneEntityId::Object(id), _)) = picked
            && matches!(self.active_document().get_object(id), Some(Object::Polyline { closed: false, .. }))
        {
            self.editor.relimit_source_id = Some(id);
            self.editor.relimit_awaiting_source_pick = false;
            self.editor.relimit_dialog_open = true;
            self.editor.tool_highlight_id = None;
            self.invalidate_geometry();
        }
    }

    pub(crate) fn relimit_line_click(&mut self) {
        if !self.editing_ready() {
            return;
        }

        // Phase 0: no source selected yet.
        if self.editor.relimit_awaiting_source_pick {
            self.pick_relimit_source();
            return;
        }

        // Phase 2: user clicks to confirm which end to move.
        if self.editor.relimit_confirming_end {
            self.commit_relimit_intersect();
            return;
        }

        // Phase 1: pick the second line (only when dialog is closed).
        if !self.editor.relimit_waiting_for_pick {
            userspace_warn!("Relimit: click ignored, tool is not currently waiting for a target pick");
            return;
        }
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));
        let Some((SceneEntityId::Object(second_id), _)) = picked else {
            userspace_warn!("Relimit: click did not hit any object (nothing under cursor)");
            return;
        };
        let Some(source_id) = self.editor.relimit_source_id else {
            userspace_warn!("Relimit: no source line is set, aborting pick");
            return;
        };
        if second_id == source_id {
            userspace_warn!("Relimit: clicked the source line itself, pick a different line");
            return;
        }

        // Both must be open polylines with at least 1 segment. Relimiting modifies
        // existing objects, so neither is restricted to the active layer.
        let (src_verts, tgt_verts) = {
            let src = match self.active_document().get_object(source_id) {
                Some(Object::Polyline { verts, closed: false, .. }) if verts.len() >= 2 => verts.clone(),
                _ => {
                    userspace_warn!("Relimit: source object {source_id:?} is no longer a valid open polyline");
                    return;
                }
            };
            let tgt = match self.active_document().get_object(second_id) {
                Some(Object::Polyline { verts, closed, .. }) if verts.len() >= 2 => (verts.clone(), *closed),
                Some(Object::Polyline { verts, .. }) => {
                    userspace_warn!("Relimit: clicked polyline {second_id:?} has only {} vertex/vertices, need at least 2", verts.len());
                    return;
                }
                Some(other) => {
                    userspace_warn!("Relimit: clicked object {second_id:?} is not a polyline (it's a {})", other.kind_name());
                    return;
                }
                None => {
                    userspace_warn!("Relimit: clicked object {second_id:?} no longer exists");
                    return;
                }
            };
            (src, tgt)
        };
        let (tgt_verts, tgt_closed) = tgt_verts;

        // Each end continues its own terminal segment. Using the first-to-last
        // chord here makes every bent polyline relimit in the wrong direction.
        let world_candidates = relimit_world_candidates(&src_verts, &tgt_verts, tgt_closed);
        if world_candidates.is_empty() {
            userspace_warn!("Relimit: neither terminal segment crosses the selected target line in plan view");
            return;
        }

        let candidates: Vec<RelimitCandidate> = world_candidates
            .into_iter()
            .map(|candidate| RelimitCandidate {
                end: candidate.end,
                target: candidate.target,
                is_extension: candidate.is_extension,
            })
            .collect();

        self.editor.relimit_second_id = Some(second_id);
        self.editor.relimit_candidates = candidates;
        self.editor.relimit_waiting_for_pick = false;
        self.editor.tool_highlight_id = None;
        self.editor.relimit_confirming_end = true;
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    /// Highlight the line that the current Relimit pick phase would accept.
    pub(crate) fn update_relimit_hover_line(&mut self) {
        let picked = self.graphics.as_ref().and_then(|graphics| {
            graphics.pick_at_cursor(
                PICK_THRESHOLD_PX,
                &self.triangulations,
                &self.editor.hidden_handles,
                &self.editor.frozen_handles,
                self.editor.xray_enabled,
            )
        });
        let candidate = picked.and_then(|(entity, _)| match entity {
            SceneEntityId::Object(id)
                if self.editor.relimit_awaiting_source_pick && matches!(self.active_document().get_object(id), Some(Object::Polyline { closed: false, .. })) =>
            {
                Some(id)
            }
            SceneEntityId::Object(id)
                if self.editor.relimit_waiting_for_pick
                    && Some(id) != self.editor.relimit_source_id
                    && matches!(
                        self.active_document().get_object(id),
                        Some(Object::Polyline { verts, .. }) if verts.len() >= 2
                    ) =>
            {
                Some(id)
            }
            _ => None,
        });
        if candidate != self.editor.tool_highlight_id {
            self.editor.tool_highlight_id = candidate;
            self.invalidate_geometry();
        }
    }

    fn commit_relimit_intersect(&mut self) {
        let Some(source_id) = self.editor.relimit_source_id else {
            return;
        };
        let before = match self.active_document().get_object(source_id) {
            Some(obj) => obj.clone(),
            None => return,
        };
        let (source_start, source_end) = match &before {
            Object::Polyline { verts, .. } if verts.len() >= 2 => (verts[0].pos, verts[verts.len() - 1].pos),
            _ => return,
        };
        let Some(cursor) = self.editor.cursor_screen_px else {
            return;
        };
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        let Some((candidate_index, _)) = crate::rendering::graphics::projections::projected_relimit_candidate_nearest_cursor(
            graphics.window_to_viewport_px(cursor),
            &self.editor.relimit_candidates,
            source_start,
            source_end,
            &graphics.view_proj(),
            graphics.screen_size_pub(),
        ) else {
            return;
        };
        let Some(candidate) = self.editor.relimit_candidates.get(candidate_index).copied() else {
            return;
        };

        let mut after = before.clone();
        if let Object::Polyline { verts, .. } = &mut after {
            match candidate.end {
                TrimEnd::Start => {
                    if let Some(v) = verts.first_mut() {
                        v.pos = candidate.target;
                    }
                }
                TrimEnd::End => {
                    if let Some(v) = verts.last_mut() {
                        v.pos = candidate.target;
                    }
                }
            }
        }

        let changed = if let Some(project) = self.workspace.active_project_mut() {
            self.history.execute(&mut project.project.document, Command::Replace { before, after });
            true
        } else {
            false
        };
        self.cancel_relimit();
        if changed {
            crate::logging::report_completed_action(
                CommandReportSpec::new("Relimit Line", format!("{source_id:?}")),
                format!("Relimited line {source_id:?} to the selected target"),
            );
        }
        self.invalidate_geometry();
    }

    pub(crate) fn relimit_resize(&mut self, source_id: ObjectId, mode: RelimitMode, value: f64) {
        let before = match self.active_document().get_object(source_id) {
            Some(obj @ Object::Polyline { .. }) => obj.clone(),
            _ => return,
        };
        let mut after = before.clone();
        let resize_end = self.editor.relimit_resize_end;
        if let Object::Polyline { verts, .. } = &mut after {
            let Some(position) = resized_terminal_position(verts, resize_end, mode, value) else {
                return;
            };
            match resize_end {
                TrimEnd::End => {
                    let last = verts.len() - 1;
                    verts[last].pos = position;
                }
                TrimEnd::Start => verts[0].pos = position,
            }
        }

        let changed = if let Some(project) = self.workspace.active_project_mut() {
            self.history.execute(&mut project.project.document, Command::Replace { before, after });
            true
        } else {
            false
        };
        self.cancel_relimit();
        if changed {
            crate::logging::report_completed_action(
                CommandReportSpec::new("Relimit Line", format!("{source_id:?}")),
                format!("Resized line {source_id:?} using {mode:?} value {value}"),
            );
        }
        self.invalidate_geometry();
    }

    pub(crate) fn cancel_relimit(&mut self) {
        self.editor.relimit_confirming_end = false;
        self.editor.relimit_waiting_for_pick = false;
        self.editor.relimit_awaiting_source_pick = false;
        self.editor.relimit_dialog_open = false;
        self.editor.relimit_source_id = None;
        self.editor.relimit_second_id = None;
        self.editor.relimit_intersection_3d = None;
        self.editor.relimit_candidates.clear();
        self.editor.tool_highlight_id = None;
        self.editor.relimit_preview_from_px = None;
        self.editor.relimit_preview_to_px = None;
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_geometry();
        self.invalidate_overlay();
    }
}

// -------------------------------------------------------------------------
// World-space (XY plane) line intersection helper (outside the impl)
// -------------------------------------------------------------------------

/// Intersect the *infinite* line through `a`,`b` with the *segment* `c`–`d`.
/// Returns the parameter `t` along `a`→`b` (any value), but only when the hit
/// lies on (or within `XY_TOL` metres of) the actual `c`–`d` segment - so a
/// polyline edge is never extended to an arbitrary off-edge point, while a
/// touch point that drifted off the end by floating-point noise (e.g. from a
/// prior relimit) still resolves.
fn line_line_intersect_t(a: glam::DVec2, b: glam::DVec2, c: glam::DVec2, d: glam::DVec2) -> Option<f64> {
    use crate::model::kernel;
    let (point, _) = kernel::line_segment(a, b - a, c, d, kernel::XY_TOL)?;
    let r = b - a;
    Some((point - a).dot(r) / r.length_squared())
}

#[derive(Clone, Copy, Debug)]
struct RelimitWorldCandidate {
    end: TrimEnd,
    target: DVec3,
    is_extension: bool,
}

/// Find trim/extend candidates along the source's first and last segments.
/// The target is tessellated first so a bulged target is not reduced to its
/// stored endpoint chords.
fn relimit_world_candidates(source: &[PolyVertex], target: &[PolyVertex], target_closed: bool) -> Vec<RelimitWorldCandidate> {
    if source.len() < 2 || target.len() < 2 {
        return Vec::new();
    }
    let target_points = crate::model::geometry::tessellate_polyline_bulges(target, target_closed);
    if target_points.len() < 2 {
        return Vec::new();
    }

    let target_edge_count = if target_closed { target_points.len() } else { target_points.len() - 1 };
    let crossings = |a: DVec3, b: DVec3| -> Vec<(f64, DVec3)> {
        if (b - a).truncate().length_squared() <= f64::EPSILON {
            return Vec::new();
        }
        (0..target_edge_count)
            .filter_map(|i| {
                let c = target_points[i];
                let d = target_points[(i + 1) % target_points.len()];
                line_line_intersect_t(a.truncate(), b.truncate(), c.truncate(), d.truncate()).map(|t| (t, a + t * (b - a)))
            })
            .collect()
    };

    let mut candidates = Vec::with_capacity(4);
    let start_a = source[0].pos;
    let start_b = source[1].pos;
    let start_touch = crate::model::kernel::XY_TOL / (start_b - start_a).truncate().length().max(crate::model::kernel::XY_TOL);
    let start_crossings = crossings(start_a, start_b);
    if let Some(&(_, target)) = start_crossings.iter().filter(|(t, _)| *t < -start_touch).max_by(|(a, _), (b, _)| a.total_cmp(b)) {
        candidates.push(RelimitWorldCandidate {
            end: TrimEnd::Start,
            target,
            is_extension: true,
        });
    }
    if let Some(&(_, target)) = start_crossings
        .iter()
        .filter(|(t, _)| *t >= -start_touch && *t < 1.0)
        .min_by(|(a, _), (b, _)| a.total_cmp(b))
    {
        candidates.push(RelimitWorldCandidate {
            end: TrimEnd::Start,
            target,
            is_extension: false,
        });
    }

    let last = source.len() - 1;
    let end_a = source[last - 1].pos;
    let end_b = source[last].pos;
    let end_touch = crate::model::kernel::XY_TOL / (end_b - end_a).truncate().length().max(crate::model::kernel::XY_TOL);
    let end_crossings = crossings(end_a, end_b);
    if let Some(&(_, target)) = end_crossings
        .iter()
        .filter(|(t, _)| *t > 0.0 && *t <= 1.0 + end_touch)
        .max_by(|(a, _), (b, _)| a.total_cmp(b))
    {
        candidates.push(RelimitWorldCandidate {
            end: TrimEnd::End,
            target,
            is_extension: false,
        });
    }
    if let Some(&(_, target)) = end_crossings.iter().filter(|(t, _)| *t > 1.0 + end_touch).min_by(|(a, _), (b, _)| a.total_cmp(b)) {
        candidates.push(RelimitWorldCandidate {
            end: TrimEnd::End,
            target,
            is_extension: true,
        });
    }

    candidates
}

fn resized_terminal_position(verts: &[PolyVertex], end: TrimEnd, mode: RelimitMode, value: f64) -> Option<DVec3> {
    if verts.len() < 2 || !value.is_finite() {
        return None;
    }
    let lengths: Vec<f64> = verts.windows(2).map(|pair| pair[0].pos.distance(pair[1].pos)).collect();
    if lengths.iter().any(|length| !length.is_finite()) {
        return None;
    }
    let current_length: f64 = lengths.iter().sum();
    let requested_length = match mode {
        RelimitMode::AbsoluteLength => value,
        RelimitMode::RelativeLength => current_length + value,
        RelimitMode::Intersect => return None,
    };
    let terminal_index = match end {
        TrimEnd::Start => 0,
        TrimEnd::End => lengths.len() - 1,
    };
    let fixed_length = current_length - lengths[terminal_index];
    let new_terminal_length = requested_length - fixed_length;
    if !new_terminal_length.is_finite() || new_terminal_length <= 1e-9 {
        return None;
    }

    match end {
        TrimEnd::Start => {
            let next = verts[1].pos;
            let outward = verts[0].pos - next;
            (outward.length() > 1e-9).then(|| next + outward.normalize() * new_terminal_length)
        }
        TrimEnd::End => {
            let previous = verts[verts.len() - 2].pos;
            let outward = verts[verts.len() - 1].pos - previous;
            (outward.length() > 1e-9).then(|| previous + outward.normalize() * new_terminal_length)
        }
    }
}
