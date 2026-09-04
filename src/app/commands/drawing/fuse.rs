use glam::DVec3;

use crate::{
    app::{App, PICK_THRESHOLD_PX},
    i18n::{tr, tr_format},
    logging::CommandReportSpec,
    model::{Command, Object, ObjectId, PolyVertex, SceneEntityId, geometry::points_coincident},
    rendering::pick,
    ui::state::FuseSegment,
    userspace_warn,
};

impl<'a> App<'a> {
    pub(crate) fn fuse_click(&mut self) {
        if !self.editing_ready() {
            return;
        }

        if let Some(awaiting_id) = self.editor.fuse_awaiting_endpoint {
            // Phase 2: user is clicking to select an endpoint of the awaiting line.
            let cursor_px = match self.editor.cursor_screen_px {
                Some(px) => px,
                None => return,
            };
            match self.pick_fuse_endpoint(awaiting_id, cursor_px) {
                Some(marker_index) => self.select_fuse_endpoint(awaiting_id, marker_index),
                None => userspace_warn!("{}", tr!(literal = "Fuse: click was not close enough to either endpoint of the selected line")),
            }
            return;
        }

        // After choosing the start of a single open polyline, clicking its
        // highlighted opposite endpoint closes that same object.
        if self.editor.fuse_segments.len() == 1 && self.editor.fuse_close_marker.is_some_and(|endpoint| self.fuse_cursor_near(endpoint)) {
            self.close_single_fuse_source();
            return;
        }

        // Phase 1: pick a line to add to the chain.
        let frozen = &self.editor.frozen_handles;
        let picked = self
            .graphics
            .as_ref()
            .and_then(|g| g.pick_at_cursor(PICK_THRESHOLD_PX, &self.triangulations, &self.editor.hidden_handles, frozen, self.editor.xray_enabled));
        let Some((SceneEntityId::Object(object_id), _)) = picked else {
            userspace_warn!("{}", tr!(literal = "Fuse: click did not hit any object (nothing under cursor)"));
            return;
        };
        // Near a shared endpoint the pick can land back on a line already in
        // the chain; fusing a line onto itself would double its vertices.
        if self.editor.fuse_segments.iter().any(|segment| segment.object_id == object_id) {
            userspace_warn!(
                "{}",
                tr_format!(
                    literal = "Fuse: object %object_id% is already part of the fuse chain, click a different line",
                    object_id = format!("{object_id:?}")
                )
            );
            return;
        }
        // Must be an open polyline. Fusing modifies/deletes existing objects, so
        // it isn't restricted to the active layer (that restriction only matters
        // for where *new* geometry gets created).
        let markers = match self.active_document().get_object(object_id) {
            Some(Object::Polyline { verts, closed: false, .. }) if verts.len() >= 2 => {
                vec![(0, verts[0].pos), (verts.len() - 1, verts[verts.len() - 1].pos)]
            }
            Some(Object::Polyline { closed: true, .. }) => {
                userspace_warn!(
                    "{}",
                    tr_format!(
                        literal = "Fuse: clicked object %object_id% is a closed polyline, fuse only works on open polylines",
                        object_id = format!("{object_id:?}")
                    )
                );
                return;
            }
            Some(Object::Polyline { verts, .. }) => {
                userspace_warn!(
                    "{}",
                    tr_format!(
                        literal = "Fuse: clicked polyline %object_id% has only %count% vertex/vertices, need at least 2",
                        object_id = format!("{object_id:?}"),
                        count = verts.len()
                    )
                );
                return;
            }
            Some(other) => {
                userspace_warn!(
                    "{}",
                    tr_format!(
                        literal = "Fuse: clicked object %object_id% is not an open polyline (it's a %kind%)",
                        object_id = format!("{object_id:?}"),
                        kind = other.kind_name()
                    )
                );
                return;
            }
            None => {
                userspace_warn!(
                    "{}",
                    tr_format!(literal = "Fuse: clicked object %object_id% no longer exists", object_id = format!("{object_id:?}"))
                );
                return;
            }
        };

        // If this line already has an endpoint sitting exactly where the chain
        // currently ends (e.g. it was drawn/snapped to share a vertex with the
        // previous line), skip asking the user to click that endpoint too -
        // just join there. This compares full 3D position (not just plan-view
        // XY), since a genuine shared vertex matches in elevation too.
        if let Some(tail) = self.editor.fuse_chain_tail {
            let auto_marker = markers.iter().position(|(_, point)| points_coincident(*point, tail));
            if let Some(marker_index) = auto_marker {
                self.editor.fuse_endpoint_markers = markers;
                self.select_fuse_endpoint(object_id, marker_index);
                return;
            }
        }

        self.editor.fuse_awaiting_endpoint = Some(object_id);
        self.editor.fuse_endpoint_markers = markers;
        self.editor.selected_handles.insert(SceneEntityId::Object(object_id));
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    /// Add `awaiting_id`'s endpoint at `marker_index` (from
    /// `fuse_endpoint_markers`) as the next segment in the chain, auto-finishing
    /// once two segments are collected.
    fn select_fuse_endpoint(&mut self, awaiting_id: ObjectId, marker_index: usize) {
        let Some(&(vertex_index, join_point)) = self.editor.fuse_endpoint_markers.get(marker_index) else {
            userspace_warn!(
                "{}",
                tr_format!(literal = "Fuse: endpoint marker %marker_index% no longer exists", marker_index = marker_index)
            );
            return;
        };
        let (vertex_count, closed) = match self.active_document().get_object(awaiting_id) {
            Some(Object::Polyline { verts, closed, .. }) => (verts.len(), *closed),
            _ => {
                userspace_warn!(
                    "{}",
                    tr_format!(
                        literal = "Fuse: object %awaiting_id% is no longer a valid polyline",
                        awaiting_id = format!("{awaiting_id:?}")
                    )
                );
                return;
            }
        };
        let first_segment = self.editor.fuse_segments.is_empty();
        // On the first source the chosen point is the outgoing join end;
        // on the second source it is the incoming start. Thus choosing
        // A then C connects A-C rather than B-C.
        let reversed = !closed
            && if first_segment {
                vertex_index == 0
            } else {
                vertex_index == vertex_count.saturating_sub(1)
            };
        let start_index = vertex_index;
        let tail = if first_segment {
            join_point
        } else if closed {
            self.editor
                .fuse_endpoint_markers
                .iter()
                .find(|(index, _)| *index == (vertex_index + vertex_count - 1) % vertex_count)
                .map_or(join_point, |(_, point)| *point)
        } else {
            self.editor
                .fuse_endpoint_markers
                .iter()
                .find(|(index, _)| *index != vertex_index)
                .map_or(join_point, |(_, point)| *point)
        };
        // The chosen endpoint is the designated join with the chain tail. If
        // it sits on the tail - or merely looks like it does at the current
        // zoom (unsnapped digitising, import rounding) - weld: commit drops
        // the duplicate instead of keeping a doubled point plus a micro edge.
        // Endpoints clearly apart stay, becoming a deliberate bridging edge.
        let weld_start = !first_segment
            && self
                .editor
                .fuse_chain_tail
                .is_some_and(|chain_tail| points_coincident(join_point, chain_tail) || self.fuse_points_visually_coincident(join_point, chain_tail));
        self.editor.fuse_segments.push(FuseSegment {
            object_id: awaiting_id,
            reversed,
            start_index,
            closed,
            weld_start,
        });
        self.editor.fuse_chain_tail = Some(tail);
        self.editor.fuse_close_marker = if first_segment && !closed && vertex_count >= 3 {
            self.editor.fuse_endpoint_markers.iter().find(|(index, _)| *index != vertex_index).map(|(_, point)| *point)
        } else {
            None
        };
        self.editor.fuse_awaiting_endpoint = None;
        // Remove highlight for the awaiting line.
        self.editor.selected_handles.remove(&SceneEntityId::Object(awaiting_id));
        let should_finish = self.editor.fuse_segments.len() == 2;
        self.invalidate_geometry();
        self.invalidate_overlay();
        if should_finish {
            self.commit_fuse(false);
        }
    }

    /// Returns the closest displayed fuse-marker index within the pick threshold.
    fn pick_fuse_endpoint(&self, _object_id: ObjectId, cursor_px: (f32, f32)) -> Option<usize> {
        let graphics = self.graphics.as_ref()?;
        let vp = graphics.view_proj();
        let screen = graphics.screen_size_pub();
        let cursor_px = graphics.window_to_viewport_px(cursor_px);
        let threshold = PICK_THRESHOLD_PX * 2.0;

        let dist_sq = |world: DVec3| -> Option<f64> {
            let sp = pick::world_to_screen(&vp, world, screen)?;
            let dx = sp.x - cursor_px.0 as f64;
            let dy = sp.y - cursor_px.1 as f64;
            Some(dx * dx + dy * dy)
        };

        let threshold_sq = (threshold * threshold) as f64;
        self.editor
            .fuse_endpoint_markers
            .iter()
            .enumerate()
            .filter_map(|(index, (_, point))| dist_sq(*point).map(|distance| (index, distance)))
            .filter(|(_, distance)| *distance <= threshold_sq)
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
    }

    /// True when two world points render within the endpoint-pick radius of
    /// each other, i.e. the user cannot tell them apart at the current zoom.
    fn fuse_points_visually_coincident(&self, a: DVec3, b: DVec3) -> bool {
        let Some(graphics) = self.graphics.as_ref() else {
            return false;
        };
        let vp = graphics.view_proj();
        let screen = graphics.screen_size_pub();
        let (Some(sa), Some(sb)) = (pick::world_to_screen(&vp, a, screen), pick::world_to_screen(&vp, b, screen)) else {
            return false;
        };
        let dx = sa.x - sb.x;
        let dy = sa.y - sb.y;
        dx * dx + dy * dy <= f64::from(PICK_THRESHOLD_PX * PICK_THRESHOLD_PX * 4.0)
    }

    fn fuse_cursor_near(&self, world: DVec3) -> bool {
        let (Some(graphics), Some(cursor)) = (self.graphics.as_ref(), self.editor.cursor_screen_px) else {
            return false;
        };
        let cursor = graphics.window_to_viewport_px(cursor);
        let Some(screen_point) = pick::world_to_screen(&graphics.view_proj(), world, graphics.screen_size_pub()) else {
            return false;
        };
        let dx = screen_point.x - f64::from(cursor.0);
        let dy = screen_point.y - f64::from(cursor.1);
        dx * dx + dy * dy <= f64::from(PICK_THRESHOLD_PX * PICK_THRESHOLD_PX * 4.0)
    }

    fn close_single_fuse_source(&mut self) {
        let Some(segment) = self.editor.fuse_segments.first() else {
            userspace_warn!("{}", tr!(literal = "Fuse: no source line to close into a polyline"));
            return;
        };
        let Some(before) = self.active_document().get_object(segment.object_id).cloned() else {
            userspace_warn!(
                "{}",
                tr_format!(literal = "Fuse: source object %object_id% no longer exists", object_id = format!("{:?}", segment.object_id))
            );
            return;
        };
        let Object::Polyline { closed: false, .. } = &before else {
            userspace_warn!(
                "{}",
                tr_format!(
                    literal = "Fuse: source object %object_id% is no longer a valid open polyline",
                    object_id = format!("{:?}", segment.object_id)
                )
            );
            return;
        };
        let mut after = before.clone();
        if let Object::Polyline { verts, closed, .. } = &mut after {
            // A line that already ends on (or visually on) its own start
            // would close into a polyline with a doubled vertex - weld it.
            if verts.len() > 2
                && verts
                    .first()
                    .zip(verts.last())
                    .is_some_and(|(first, last)| points_coincident(first.pos, last.pos) || self.fuse_points_visually_coincident(first.pos, last.pos))
            {
                verts.pop();
            }
            if verts.len() < 3 {
                userspace_warn!(
                    "{}",
                    tr_format!(
                        literal = "Fuse: line needs at least 3 distinct vertices to close into a polyline (has %count%)",
                        count = verts.len()
                    )
                );
                return;
            }
            *closed = true;
        }
        self.execute_edit(Command::Replace { before, after });
        self.finish_fuse_state();
    }

    fn commit_fuse(&mut self, closed: bool) {
        if self.editor.fuse_segments.len() < 2 {
            userspace_warn!(
                "{}",
                tr_format!(literal = "Fuse: need at least 2 segments to commit (have %count%)", count = self.editor.fuse_segments.len())
            );
            return;
        }
        if !self.editing_ready() {
            userspace_warn!("{}", tr!(literal = "Fuse: no active project, cannot commit"));
            return;
        }

        // Collect ordered vertices.
        let mut all_verts: Vec<PolyVertex> = Vec::new();
        let source_ids: Vec<ObjectId> = self.editor.fuse_segments.iter().map(|s| s.object_id).collect();

        for seg in &self.editor.fuse_segments {
            let doc = self.active_document();
            let verts = match doc.get_object(seg.object_id) {
                Some(Object::Polyline { verts, .. }) => verts.clone(),
                _ => {
                    userspace_warn!(
                        "{}",
                        tr_format!(
                            literal = "Fuse: segment object %object_id% is no longer a valid polyline, aborting",
                            object_id = format!("{:?}", seg.object_id)
                        )
                    );
                    return;
                }
            };
            let ordered: Vec<PolyVertex> = if seg.closed {
                (0..verts.len()).map(|offset| verts[(seg.start_index + offset) % verts.len()]).collect()
            } else if seg.reversed {
                reverse_open_polyline(&verts)
            } else {
                verts
            };
            append_fuse_vertices(&mut all_verts, ordered, seg.weld_start);
        }

        let closed = if all_verts.first().zip(all_verts.last()).is_some_and(|(first, last)| points_coincident(first.pos, last.pos)) {
            all_verts.pop();
            true
        } else {
            closed
        };

        if all_verts.len() < 2 || (closed && all_verts.len() < 3) {
            userspace_warn!("{}", tr_format!(literal = "Fuse: result has too few vertices (%count%), aborting", count = all_verts.len()));
            return;
        }

        let Some(layer) = self.active_layer() else {
            userspace_warn!("{}", tr!(literal = "Fuse: no active layer to place the fused line on"));
            return;
        };
        let color = crate::model::ObjectColor::Fixed(self.editor.tool_line_color);
        let line_weight = self.editor.tool_line_weight;
        let fill = self.editor.tool_hatch.to_fill_style();

        let vertex_count = all_verts.len();
        // The two ends of the freshly fused line - `far_end` is the free end of
        // the segment that was just joined on (where the chain should continue
        // from), `near_end` is the opposite free end.
        let far_end = all_verts.last().map(|v| v.pos);
        let near_end = all_verts.first().map(|v| v.pos);
        let mut new_id = None;
        if let Some(project) = self.workspace.active_project_mut() {
            let doc = &mut project.project.document;
            let id = doc.allocate_object_id();
            new_id = Some(id);
            let mut commands = vec![Command::AddObject(Object::Polyline {
                id,
                layer,
                verts: all_verts,
                closed,
                color,
                fill,
                line_weight,
            })];
            for src_id in source_ids {
                if let Some(obj) = doc.get_object(src_id).cloned() {
                    commands.push(Command::delete_object(obj));
                }
            }
            self.execute_edit(Command::Batch(commands));
            let shape = if closed {
                crate::i18n::tr!(literal = "Closed polyline")
            } else {
                crate::i18n::tr!(literal = "Open polyline")
            };
            let source_count = self.editor.fuse_segments.len();
            crate::logging::report_completed_action(
                CommandReportSpec::new(
                    crate::i18n::tr!(literal = "Fuse Lines"),
                    crate::i18n::tr_format!(literal = "%count% source line(s)", count = source_count),
                ),
                crate::i18n::tr_format!(
                    literal = "Created %shape% %object_id% with %vertices% vertices from %sources% source line(s)",
                    shape = shape,
                    object_id = format!("{id:?}"),
                    vertices = vertex_count,
                    sources = source_count
                ),
            );
        }

        match (new_id, closed) {
            // The result is still an open line: stay in the tool, keep the new
            // line selected, and treat its free end as the chain's join point
            // so the user can immediately pick another line to fuse onto it
            // without re-clicking this line or its endpoint.
            (Some(id), false) => {
                self.editor.fuse_segments = vec![FuseSegment {
                    object_id: id,
                    reversed: false,
                    start_index: vertex_count - 1,
                    closed: false,
                    weld_start: false,
                }];
                self.editor.fuse_awaiting_endpoint = None;
                self.editor.fuse_endpoint_markers.clear();
                self.editor.fuse_chain_tail = far_end;
                self.editor.fuse_close_marker = if vertex_count >= 3 { near_end } else { None };
                self.editor.selected_handles.insert(SceneEntityId::Object(id));
                self.invalidate_geometry();
                self.invalidate_overlay();
            }
            _ => self.finish_fuse_state(),
        }
    }

    fn finish_fuse_state(&mut self) {
        if let Some(id) = self.editor.fuse_awaiting_endpoint {
            self.editor.selected_handles.remove(&SceneEntityId::Object(id));
        }
        self.editor.fuse_segments.clear();
        self.editor.fuse_awaiting_endpoint = None;
        self.editor.fuse_endpoint_markers.clear();
        self.editor.fuse_chain_tail = None;
        self.editor.fuse_close_marker = None;
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    /// If exactly one polyline is currently selected, pre-fill the fuse
    /// awaiting-endpoint state so the user doesn't have to click the line again.
    pub(crate) fn fuse_init_from_selection(&mut self) {
        let mut selected_object_ids = self.editor.selected_handles.iter().filter_map(|h| match h {
            SceneEntityId::Object(id) => Some(*id),
            _ => None,
        });
        // Exactly one selected object, otherwise iterating the HashSet would
        // arm an arbitrary line as the fuse source.
        let (Some(object_id), None) = (selected_object_ids.next(), selected_object_ids.next()) else {
            return;
        };
        let markers = match self.active_document().get_object(object_id) {
            Some(Object::Polyline { verts, closed: false, .. }) if verts.len() >= 2 => {
                vec![(0, verts[0].pos), (verts.len() - 1, verts[verts.len() - 1].pos)]
            }
            _ => return,
        };
        self.editor.fuse_awaiting_endpoint = Some(object_id);
        self.editor.fuse_endpoint_markers = markers;
        self.invalidate_overlay();
    }

    pub(crate) fn cancel_fuse(&mut self) {
        if let Some(id) = self.editor.fuse_awaiting_endpoint {
            self.editor.selected_handles.remove(&SceneEntityId::Object(id));
        }
        self.editor.fuse_segments.clear();
        self.editor.fuse_awaiting_endpoint = None;
        self.editor.fuse_endpoint_markers.clear();
        self.editor.fuse_chain_tail = None;
        self.editor.fuse_close_marker = None;
        self.invalidate_geometry();
        self.invalidate_overlay();
    }
}

/// Reverse an open bulged polyline without changing its geometry. A bulge
/// belongs to the segment starting at a vertex, so reversal both shifts it to
/// the new segment start and negates its sweep direction.
fn reverse_open_polyline(verts: &[PolyVertex]) -> Vec<PolyVertex> {
    let n = verts.len();
    (0..n)
        .map(|i| {
            let mut vertex = verts[n - 1 - i];
            vertex.bulge = if i + 1 < n { -verts[n - 2 - i].bulge } else { 0.0 };
            vertex
        })
        .collect()
}

fn append_fuse_vertices(all_verts: &mut Vec<PolyVertex>, mut ordered: Vec<PolyVertex>, weld_start: bool) {
    let coincident_start = all_verts.last().zip(ordered.first()).is_some_and(|(a, b)| points_coincident(a.pos, b.pos));
    if (coincident_start || weld_start) && !all_verts.is_empty() && !ordered.is_empty() {
        // The removed join vertex owns the first outgoing arc of this source.
        // Transfer it to the retained shared vertex before dropping it.
        let outgoing_bulge = ordered[0].bulge;
        all_verts.last_mut().expect("non-empty checked above").bulge = outgoing_bulge;
        ordered.remove(0);
    }
    all_verts.extend(ordered);
}
