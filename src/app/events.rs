use std::time::Duration;

use web_time::Instant;
use winit::{
    event::*,
    keyboard::{KeyCode, PhysicalKey},
};

use crate::{app::App, i18n::tr_format, logging::CommandReportSpec, rendering::graphics::RenderSurfaceError, ui::state::ActiveTool, userspace_error};

const RIGHT_CLICK_DRAG_THRESHOLD_PX: f32 = 3.0;

impl<'a> App<'a> {
    pub(crate) fn handle_window_event(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, window_id: winit::window::WindowId, event: WindowEvent) {
        if self.graphics.as_ref().and_then(|graphics| graphics.slice_preview_window_id()) == Some(window_id) {
            self.handle_slice_preview_event(event);
            return;
        }
        // A dropped secondary window can still have a final queued event.
        // Never feed such an event into the root window's egui/camera state.
        if self.window.as_ref().map(|window| window.id()) != Some(window_id) {
            return;
        }
        if let WindowEvent::ModifiersChanged(modifiers) = &event {
            self.modifiers = modifiers.state();
        }
        if let WindowEvent::Focused(focused) = &event {
            self.window_focused = *focused;
            if !focused {
                self.finish_left_button_interactions();
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.release_mouse_capture();
                    graphics.release_slice_keys();
                }
            }
            self.redraw_requested = true;
        }

        if let WindowEvent::MouseWheel { .. } = &event {
            self.last_scroll_instant = Some(Instant::now());
        }

        if self.handle_cursor_mode_cycle(&event) {
            return;
        }

        let gui_response = self.graphics.as_mut().map(|graphics| graphics.gui_input(&event));
        if gui_response.is_some_and(|response| response.repaint) {
            self.redraw_requested = true;
        }

        if let Some(graphics) = self.graphics.as_mut() {
            graphics.set_fly_mode_enabled(self.editor.fly_mode_enabled);
        }

        let gui_consumed = gui_response.is_some_and(|response| response.consumed);
        if !gui_consumed && self.handle_circle_radius_key_input(&event) {
            return;
        }

        // Slice mode claims plain W/S (slab forward/back) and Q/E (rotate the
        // line). Presses are dropped when the GUI consumed the event (e.g.
        // typing in the slice panel), but releases always pass through so
        // keys can't get stuck held.
        if self.editor.slice_mode_enabled
            && !self.modifiers.control_key()
            && !(cfg!(target_os = "macos") && self.modifiers.super_key())
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state,
                        physical_key: PhysicalKey::Code(key @ (KeyCode::KeyW | KeyCode::KeyS | KeyCode::KeyQ | KeyCode::KeyE)),
                        ..
                    },
                ..
            } = &event
            && (!gui_consumed || *state == ElementState::Released)
            && let Some(graphics) = self.graphics.as_mut()
        {
            graphics.slice_process_key(*key, *state == ElementState::Pressed);
            self.redraw_requested = true;
        }

        if !gui_consumed {
            let canvas_pick_mode_active =
                self.editor.triangulation_pick_target.is_some() || self.editor.tri_cut_poly_awaiting_pick || self.editor.drill_pattern_awaiting_shape_pick;
            let measurement_tool_active = matches!(self.editor.active_tool, ActiveTool::MeasureDistance | ActiveTool::MeasureBatterAngle);
            let suppress_view_mode_canvas_click = (self.editor.fly_mode_enabled || (self.editor.slice_mode_enabled && !measurement_tool_active))
                && matches!(event, WindowEvent::MouseInput { button: MouseButton::Left, .. });
            if !suppress_view_mode_canvas_click || canvas_pick_mode_active {
                self.handle_mouse_press(&event);
                self.handle_mouse_release(&event);
            }
            self.handle_right_press_for_orbit(&event);
            self.handle_right_release(&event);
            self.handle_control_shortcuts(&event);
        } else {
            if is_left_mouse_release(&event) {
                self.finish_left_button_interactions();
            }
            if matches!(event, WindowEvent::MouseInput { button: MouseButton::Right, .. }) {
                // Never retain a viewport press across a GUI-owned right-button
                // event. In particular, a consumed release must not leave a
                // pending context click or orbit promotion behind.
                self.right_press_px = None;
                self.right_orbit_active = false;
            }
        }

        let graphics_consumed = self.graphics.as_mut().is_some_and(|graphics| {
            if gui_consumed && !graphics.should_receive_event_when_gui_consumed(&event) {
                return false;
            }

            let consumed = graphics.input(&event);
            if consumed {
                self.redraw_requested = true;
            }
            consumed
        });
        let input_consumed = gui_consumed || graphics_consumed;

        if !input_consumed {
            match event {
                WindowEvent::CloseRequested => {
                    if let Err(error) = self.request_exit() {
                        userspace_error!("{}", tr_format!(literal = "Couldn't exit: %error%", error = error));
                    }
                }
                WindowEvent::KeyboardInput { .. } => self.handle_key_action(&event),
                WindowEvent::Resized(physical_size) if physical_size.width > 0 && physical_size.height > 0 => {
                    self.pending_resize = Some(physical_size);
                    self.redraw_requested = true;
                }
                WindowEvent::RedrawRequested => {
                    self.poll_triangulation_loads();
                    self.poll_block_model_loads();
                    self.poll_drill_hole_loads();
                    self.poll_point_cloud_loads();
                    self.poll_raster_loads();
                    self.poll_saves();
                    self.poll_jobs();
                    self.refresh_status_message();
                    let now = Instant::now();
                    let frame_interval = if self.pending_resize.is_some() {
                        super::rate_interval(self.editor.resize_frame_rate_cap)
                    } else {
                        super::rate_interval(self.editor.frame_rate_cap)
                    };
                    if self.last_render_time.is_some_and(|last_render| now.duration_since(last_render) < frame_interval) {
                        // Winit/compositors can issue redraw events directly. Defer
                        // early events so both normal and resize rendering obey the
                        // configured frame limits.
                        self.redraw_requested = true;
                        return;
                    }
                    // This event satisfies any queued redraw request, including
                    // redraws generated directly by the compositor during resize.
                    self.redraw_requested = false;
                    self.refresh_intersection_availability();
                    self.refresh_tie_preview();
                    self.refresh_blast_round();
                    let project = self.project_view();
                    if let Some(window) = &self.window {
                        let title = project.projects.first().map_or_else(
                            || crate::APP_NAME.to_owned(),
                            |entry| {
                                let name = entry.name.trim_end_matches(".omf");
                                format!("{}: {}{}", crate::APP_NAME, name, if entry.dirty { " *" } else { "" })
                            },
                        );
                        window.set_title(&title);
                    }
                    #[cfg(target_os = "macos")]
                    crate::mac::sync_menu_state(&self.editor, &project);
                    let completing_topology_load = self.topology_uploads_pending();
                    let applied_resize = self.pending_resize.take();
                    if let Some(graphics) = self.graphics.as_mut() {
                        if let Some(size) = applied_resize {
                            graphics.resize(size);
                        }
                        let last_render_time = *self.last_render_time.get_or_insert(now);
                        let dt = now - last_render_time;
                        self.last_render_time = Some(now);
                        if !dt.is_zero() {
                            let instantaneous_fps = 1.0 / dt.as_secs_f32();
                            self.editor.measured_fps = Some(self.editor.measured_fps.map_or(instantaneous_fps, |fps| fps * 0.9 + instantaneous_fps * 0.1));
                        }
                        graphics.update(dt, self.editor.block_model_interaction_resolution_divisor);
                        self.editor.can_undo = self.history.can_undo();
                        self.editor.can_redo = self.history.can_redo();
                        match graphics.render(crate::rendering::graphics::frame::RenderInput {
                            editor: &mut self.editor,
                            document: &mut self.scene_document,
                            triangulations: &self.triangulations,
                            block_models: &self.block_models,
                            drill_holes: &self.drill_holes,
                            point_clouds: &self.point_clouds,
                            rasters: &self.raster_textures,
                            project: &project,
                        }) {
                            Ok(ui_output) => {
                                self.render_validation_recovery_attempts = 0;
                                self.next_ui_repaint_deadline = ui_output.repaint_after.and_then(|delay| Instant::now().checked_add(delay));
                                if ui_output.repaint_after.is_some_and(|delay| delay.is_zero()) {
                                    self.redraw_requested = true;
                                }
                                if let Some(graphics) = self.graphics.as_mut() {
                                    graphics.set_fly_mode_enabled(self.editor.fly_mode_enabled);
                                    self.editor.debug_chunk_stats = Some(graphics.chunk_render_stats);
                                }
                                if completing_topology_load && !self.graphics.as_ref().is_some_and(|graphics| graphics.point_cloud_uploads_pending()) {
                                    self.finish_topology_load();
                                }
                                self.set_ui_pointer_gesture_active(ui_output.pointer_gesture_active);
                                self.handle_ui_commands(ui_output.commands);
                                self.sync_slice_preview_window(event_loop);
                                if let Some(graphics) = self.graphics.as_mut() {
                                    graphics.request_slice_preview_redraw_if_scene_changed(
                                        &self.editor,
                                        &self.scene_document,
                                        &self.triangulations,
                                        &self.block_models,
                                        &self.drill_holes,
                                        &self.point_clouds,
                                        &self.raster_textures,
                                    );
                                }
                                // Keep move panel preview in sync - apply whenever the panel delta
                                // differs from the last applied preview (catches typed values that
                                // don't always trigger changed() on every frame).
                                if self.editor.active_tool.translates()
                                    && self.gizmo_drag.is_none()
                                    && self.editor.move_tool_has_targets()
                                    && self.editor.move_panel_delta != self.editor.move_panel_last_preview
                                {
                                    let d = self.editor.move_panel_delta;
                                    self.ensure_move_session_original();
                                    self.preview_move_delta(glam::DVec3::new(d[0], d[1], d[2]));
                                    self.editor.move_panel_last_preview = d;
                                    self.invalidate_geometry();
                                }
                                // Keep the Rotate Collar panel previewing on the same
                                // belt-and-braces rule: a typed angle does not always
                                // report changed() on the frame it lands.
                                if self.editor.active_tool.rotates() && self.collar_rotate_drag.is_none() && self.editor.rotate_tool_has_targets() {
                                    let angles = [self.editor.rotate_panel_azimuth, self.editor.rotate_panel_dip];
                                    if angles != self.editor.rotate_panel_last_preview {
                                        self.preview_collar_rotation(crate::model::drill_hole::CollarRotation::Absolute(crate::model::drill_hole::HoleOrientation {
                                            azimuth: angles[0],
                                            dip: angles[1],
                                        }));
                                    }
                                }
                                // And keep them reading out where the selected holes
                                // point whenever no turn of the user's own stands over
                                // them - so arming the tool, or picking another hole,
                                // shows the angles that hole is drilled at.
                                if self.editor.active_tool.rotates() {
                                    self.refresh_rotate_panel_readout();
                                }
                                // Clean up per-tool state when the tool is switched via the
                                // toolbar. A standing turn is not stale state: it is the
                                // rotate tool's own preview, and it is cancelled by the
                                // same rule only once that tool is put down.
                                if !self.editor.active_tool.translates() && !self.editor.active_tool.rotates() && self.has_pending_move_delta() {
                                    self.editor.move_panel_last_preview = [f64::NAN; 3];
                                    self.cancel_move_delta();
                                }
                                let hover_highlight_active = matches!(self.editor.active_tool, ActiveTool::ExplodePolyline | ActiveTool::Move | ActiveTool::DeletePoints)
                                    || (self.editor.active_tool == ActiveTool::RelimitLine && (self.editor.relimit_awaiting_source_pick || self.editor.relimit_waiting_for_pick));
                                if !hover_highlight_active
                                    && self.editor.tool_highlight_id.is_some()
                                    && self.editor.offset_target_ids.is_empty()
                                    && self.editor.batter_berm_target_id.is_none()
                                    && !self.editor.tri_cut_poly_open
                                    && !self.editor.drill_pattern_open
                                {
                                    self.editor.tool_highlight_id = None;
                                    self.invalidate_geometry();
                                }
                                // Update batter berm preview every frame while dialog is open.
                                if self.editor.batter_berm_dialog_open {
                                    self.update_batter_berm_preview();
                                    self.invalidate_overlay();
                                }
                                // Clear in-progress stroke when switching away from drawing tools.
                                if !matches!(self.editor.active_tool, ActiveTool::MakeLine | ActiveTool::MakePoly) && !self.editor.pending_stroke.is_empty() {
                                    self.editor.pending_stroke.clear();
                                    self.invalidate_overlay();
                                }
                                if self.editor.active_tool != ActiveTool::MakeCircle && self.editor.circle_draft.take().is_some() {
                                    self.invalidate_overlay();
                                }
                                // Clear the pending slice line when switching
                                // away from the slice tool.
                                if self.editor.active_tool != ActiveTool::VerticalSlice && self.editor.slice_pending_start.is_some() {
                                    self.editor.slice_pending_start = None;
                                    self.invalidate_overlay();
                                }
                                // Cancel fuse state when switching away from the fuse tool.
                                if self.editor.active_tool != ActiveTool::FuseIntoPolyline
                                    && (self.editor.fuse_awaiting_endpoint.is_some() || !self.editor.fuse_segments.is_empty())
                                {
                                    self.cancel_fuse();
                                }
                                // Auto-initiate fuse from an existing selection when the fuse
                                // tool is first activated with exactly one selected line/polyline.
                                if self.editor.active_tool == ActiveTool::FuseIntoPolyline && self.editor.fuse_awaiting_endpoint.is_none() && self.editor.fuse_segments.is_empty() {
                                    self.fuse_init_from_selection();
                                }
                                if self.editor.active_tool == ActiveTool::SplitAtPoints && self.editor.split_poly_id.is_none() {
                                    self.split_init_from_selection();
                                }
                            }
                            Err(RenderSurfaceError::Lost | RenderSurfaceError::Outdated) => {
                                graphics.reconfigure();
                                self.redraw_requested = true;
                            }
                            Err(RenderSurfaceError::Timeout | RenderSurfaceError::Occluded) => {
                                self.redraw_requested = true;
                            }
                            Err(RenderSurfaceError::Validation) => {
                                // Bounded surface/device recovery first; only
                                // repeated failures without one good frame in
                                // between are treated as fatal.
                                const MAX_VALIDATION_RECOVERY_ATTEMPTS: u32 = 3;
                                if self.render_validation_recovery_attempts < MAX_VALIDATION_RECOVERY_ATTEMPTS {
                                    self.render_validation_recovery_attempts += 1;
                                    log::warn!(
                                        "Renderer validation error; attempting surface recovery ({}/{})",
                                        self.render_validation_recovery_attempts,
                                        MAX_VALIDATION_RECOVERY_ATTEMPTS
                                    );
                                    graphics.reconfigure();
                                    self.redraw_requested = true;
                                } else {
                                    self.begin_fatal_shutdown("renderer validation error");
                                }
                            }
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    self.editor.cursor_screen_px = Some((position.x as f32, position.y as f32));
                    let pixels_per_point = self.window.as_ref().map_or(1.0, |window| window.scale_factor() as f32);
                    let circle_input_reset = self
                        .editor
                        .circle_draft
                        .as_mut()
                        .is_some_and(|draft| draft.reset_typed_for_pointer_movement(self.editor.cursor_screen_px, pixels_per_point));
                    if circle_input_reset {
                        self.invalidate_overlay();
                    }
                    if self.editor.selection_box_start_px.is_some() {
                        self.editor.selection_box_current_px = self.editor.cursor_screen_px;
                    }
                    if self.graphics.as_mut().is_some_and(|g| g.set_mouse_location(position.into())) {
                        self.redraw_requested = true;
                    }
                    self.maybe_start_right_orbit_drag();
                    let z = self.editor.z_level;
                    let raw = self.graphics.as_ref().and_then(|g| g.cursor_world(z));
                    let is_drawing_tool = matches!(
                        self.editor.active_tool,
                        ActiveTool::MakePoint
                            | ActiveTool::MakeLine
                            | ActiveTool::MakePoly
                            | ActiveTool::MakeCircle
                            | ActiveTool::MakeText
                            | ActiveTool::MeasureDistance
                            | ActiveTool::MeasureBatterAngle
                            | ActiveTool::VerticalSlice
                    );
                    let is_scrolling = self.last_scroll_instant.is_some_and(|t| t.elapsed() < Duration::from_millis(250));
                    let snap_mode_enabled = matches!(
                        self.editor.cursor_mode,
                        crate::ui::state::CursorMode::SnapToPoint | crate::ui::state::CursorMode::SnapToLine | crate::ui::state::CursorMode::SnapToSurface
                    );
                    let camera_active = self.graphics.as_ref().is_some_and(|g| g.is_camera_active());
                    let snap_eligible = is_drawing_tool && snap_mode_enabled && !camera_active && !is_scrolling;
                    let now = Instant::now();
                    let snap_poll_due = self
                        .last_snap_poll_instant
                        .is_none_or(|last_poll| now.duration_since(last_poll) >= super::rate_interval(self.editor.snap_poll_rate));
                    let snapped = if snap_eligible && snap_poll_due {
                        self.last_snap_poll_instant = Some(now);
                        self.refresh_snap_index();
                        let document = &self.scene_document;
                        self.graphics.as_ref().and_then(|g| {
                            g.snap_cursor(
                                document,
                                &self.snap_index,
                                &self.triangulations,
                                &self.editor.hidden_handles,
                                &self.editor.frozen_handles,
                                &self.editor.cursor_mode,
                                self.editor.xray_enabled,
                            )
                        })
                    } else if snap_eligible && self.editor.cursor_snapped {
                        self.editor.cursor_world
                    } else {
                        None
                    };
                    let was_snapped = self.editor.cursor_snapped;
                    // During a camera drag the mouse steers the view, not the
                    // cursor: keep the last world cursor (and its snapped flag)
                    // so in-progress tool previews don't chase the unprojection
                    // of a rotating camera.
                    let effective = if camera_active { self.editor.cursor_world } else { snapped.or(raw) };
                    self.editor.cursor_snapped = if camera_active { was_snapped } else { snapped.is_some() };
                    let cursor_world_changed = self.editor.cursor_world != effective;
                    if cursor_world_changed {
                        self.editor.cursor_world = effective;
                        self.redraw_requested = true;
                    }
                    self.drag_to_cursor();
                    if self.gizmo_drag.is_some() {
                        self.move_gizmo_to_cursor();
                        self.invalidate_overlay();
                    }
                    if self.collar_rotate_drag.is_some() {
                        self.collar_rotate_drag_to_cursor();
                        self.invalidate_overlay();
                    }
                    if self.editor.active_tool.rotates()
                        && let Some(cursor_px) = self.editor.cursor_screen_px
                    {
                        let hovered = hit_rotate_gizmo_ring(&self.editor, cursor_px);
                        if self.editor.rotate_gizmo_hovered_ring != hovered {
                            self.editor.rotate_gizmo_hovered_ring = hovered;
                            self.invalidate_overlay();
                        }
                    }
                    if self.editor.active_tool.translates()
                        && let Some(cursor_px) = self.editor.cursor_screen_px
                    {
                        let hit = hit_gizmo_handle(&self.editor, cursor_px);
                        let hovered_plane = hit.as_ref().and_then(GizmoHandleHit::plane_index);
                        let hovered_axis = hit.as_ref().and_then(GizmoHandleHit::axis_index);
                        if self.editor.move_gizmo_hovered_axis != hovered_axis || self.editor.move_gizmo_hovered_plane != hovered_plane {
                            self.editor.move_gizmo_hovered_axis = hovered_axis;
                            self.editor.move_gizmo_hovered_plane = hovered_plane;
                            self.invalidate_overlay();
                        }
                    }
                    // Chamfer gizmo: hover detection and radius drag
                    if self.editor.active_tool == ActiveTool::Chamfer {
                        // If no corner has been picked yet, show a hover sphere on the nearest
                        // valid vertex.
                        if self.editor.chamfer_corner_index.is_none()
                            && let Some(cursor_px) = self.editor.cursor_screen_px
                            && let Some(graphics) = self.graphics.as_ref()
                        {
                            let vp = graphics.view_proj();
                            let screen = graphics.screen_size_pub();
                            let nearest = crate::rendering::pick::pick_nearest_vertex(
                                &self.scene_document,
                                &self.editor.hidden_handles,
                                &self.editor.frozen_handles,
                                &vp,
                                screen,
                                graphics.window_to_viewport_px(cursor_px),
                                crate::app::PICK_THRESHOLD_PX * 2.5,
                            );
                            let hover_px = nearest.and_then(|(oid, _vi, world)| {
                                let is_closed_polyline = self.scene_document.get_object(oid).is_some_and(|o| {
                                    matches!(
                                        o,
                                        crate::model::Object::Polyline {
                                            verts,
                                            closed: true,
                                            ..
                                        } if verts.len() >= 3
                                    )
                                });
                                if !is_closed_polyline {
                                    return None;
                                }
                                graphics.world_to_window_px(&vp, world)
                            });
                            if hover_px != self.editor.chamfer_hover_corner_px {
                                self.editor.chamfer_hover_corner_px = hover_px;
                                self.invalidate_overlay();
                            }
                        }
                    }
                    let hover_pick_due = snap_poll_due
                        && (matches!(self.editor.active_tool, ActiveTool::Move | ActiveTool::DeletePoints)
                            || self.editor.active_tool == ActiveTool::ExplodePolyline
                            || self.editor.relimit_awaiting_source_pick
                            || self.editor.relimit_waiting_for_pick
                            || self.editor.relimit_confirming_end
                            || self.editor.triangulation_pick_target.is_some()
                            || self.editor.tri_cut_poly_awaiting_pick
                            || self.editor.drill_pattern_awaiting_shape_pick);
                    if hover_pick_due {
                        self.last_snap_poll_instant = Some(now);
                    }
                    if matches!(self.editor.active_tool, ActiveTool::Move | ActiveTool::DeletePoints) && hover_pick_due {
                        self.update_move_delete_hover();
                    } else if !matches!(self.editor.active_tool, ActiveTool::Move | ActiveTool::DeletePoints) && self.editor.tool_hover_vertex_px.is_some() {
                        self.editor.tool_hover_vertex_px = None;
                        self.editor.tool_hover_vertex_world = None;
                        self.invalidate_overlay();
                    }
                    if let Some(cursor_px) = self.editor.cursor_screen_px {
                        // Drag: update radius from cursor offset along edge screen direction
                        if let Some(start_px) = self.editor.chamfer_gizmo_drag_start_px {
                            if let Some(dir) = self.editor.chamfer_gizmo_edge_screen_dir {
                                let dx = cursor_px.0 - start_px.0;
                                let dy = cursor_px.1 - start_px.1;
                                let dot = dx * dir.0 + dy * dir.1;
                                let new_radius = (self.editor.chamfer_gizmo_drag_start_radius + dot as f64 / self.editor.chamfer_gizmo_px_per_world)
                                    .max(0.001)
                                    .min(self.editor.chamfer_max_radius);
                                self.editor.chamfer_radius = new_radius;
                                self.invalidate_overlay();
                            }
                        } else if let (Some(corner), Some(handle)) = (self.editor.chamfer_gizmo_corner_px, self.editor.chamfer_gizmo_handle_px) {
                            // Hover detection: anywhere along the visible arrow shaft.
                            // When radius≈0 the handle coincides with the corner, so use
                            // the stub direction to compute the visual tip (matches UI drawing).
                            const STUB_LEN: f32 = 40.0;
                            let threshold = 10.0f32;
                            let near = {
                                let raw_ax = handle.0 - corner.0;
                                let raw_ay = handle.1 - corner.1;
                                let raw_len = (raw_ax * raw_ax + raw_ay * raw_ay).sqrt();
                                // Compute the visual tip the same way the UI does.
                                let (tip_x, tip_y) = if raw_len > STUB_LEN {
                                    (handle.0, handle.1)
                                } else {
                                    // Use stub dir (edge direction) when handle is near corner.
                                    let (dx, dy) = if raw_len > 4.0 {
                                        (raw_ax / raw_len, raw_ay / raw_len)
                                    } else if let Some(ed) = self.editor.chamfer_gizmo_bisector_px {
                                        let el = (ed.0 * ed.0 + ed.1 * ed.1).sqrt().max(1e-6);
                                        (ed.0 / el, ed.1 / el)
                                    } else {
                                        (1.0, 0.0)
                                    };
                                    (corner.0 + dx * STUB_LEN, corner.1 + dy * STUB_LEN)
                                };
                                // Point-to-segment distance from cursor to corner→tip.
                                let ax = tip_x - corner.0;
                                let ay = tip_y - corner.1;
                                let bx = cursor_px.0 - corner.0;
                                let by = cursor_px.1 - corner.1;
                                let seg_len_sq = ax * ax + ay * ay;
                                let dist_sq = if seg_len_sq < 1e-6 {
                                    bx * bx + by * by
                                } else {
                                    let t = ((bx * ax + by * ay) / seg_len_sq).clamp(0.0, 1.0);
                                    let px = bx - t * ax;
                                    let py = by - t * ay;
                                    px * px + py * py
                                };
                                dist_sq < threshold * threshold
                            };
                            if self.editor.chamfer_gizmo_hovered != near {
                                self.editor.chamfer_gizmo_hovered = near;
                                self.invalidate_overlay();
                            }
                        }
                    }
                    // Bezier tool: hover detection and CP drag
                    if self.editor.active_tool == ActiveTool::Bezier
                        && let Some(cursor_px) = self.editor.cursor_screen_px
                    {
                        // Update CP drag position using cursor_world
                        if let Some(cp_idx) = self.editor.bezier_dragging_cp
                            && let Some(world) = self.graphics.as_ref().and_then(|g| g.cursor_world(self.editor.z_level))
                        {
                            if cp_idx == 0 {
                                self.editor.bezier_cp1 = [world.x, world.y, world.z];
                            } else {
                                self.editor.bezier_cp2 = [world.x, world.y, world.z];
                            }
                        }
                        // Update hover state
                        let hover = bezier_cp_hit(&self.editor, cursor_px);
                        if hover != self.editor.bezier_hover_cp {
                            self.editor.bezier_hover_cp = hover;
                            self.invalidate_overlay();
                        }
                    }
                    if hover_pick_due && (self.editor.relimit_awaiting_source_pick || self.editor.relimit_waiting_for_pick) {
                        self.update_relimit_hover_line();
                    }
                    self.update_offset_preview();
                    if self.editor.active_tool == ActiveTool::ExplodePolyline && hover_pick_due {
                        self.update_explode_hover();
                    }
                    if (self.editor.triangulation_pick_target.is_some() || self.editor.tri_cut_poly_awaiting_pick || self.editor.drill_pattern_awaiting_shape_pick)
                        && hover_pick_due
                    {
                        self.update_viewport_field_pick_hover();
                    }
                    if !self.editor.pending_stroke.is_empty()
                        || self.editor.circle_draft.is_some()
                        || self.editor.cursor_snapped
                        || was_snapped
                        || self.editor.relimit_confirming_end
                        || self.editor.relimit_waiting_for_pick
                        || self.editor.offset_awaiting_side_pick
                        || self.gizmo_drag.is_some()
                        || self.collar_rotate_drag.is_some()
                        || self.editor.active_tool.translates()
                        || self.editor.active_tool.rotates()
                        || self.editor.active_tool == ActiveTool::MeasureDistance
                        || self.editor.active_tool == ActiveTool::MeasureBatterAngle
                        || self.editor.active_tool == ActiveTool::DeletePoints
                        || self.editor.active_tool == ActiveTool::Chamfer
                        || self.editor.active_tool == ActiveTool::Bezier
                        || self.editor.slice_pending_start.is_some()
                    {
                        self.invalidate_overlay();
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_slice_preview_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::Focused(false) => {
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.release_slice_keys();
                }
                self.slice_preview_middle_down = false;
                self.redraw_requested = true;
            }
            WindowEvent::CloseRequested => {
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.close_slice_preview();
                }
                self.editor.slice_preview_detached = false;
                self.slice_preview_cursor_px = None;
                self.slice_preview_middle_down = false;
                self.redraw_requested = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let next = (position.x, position.y);
                let viewport = self.graphics.as_ref().and_then(|graphics| graphics.slice_preview_viewport());
                let changed = if self.slice_preview_middle_down
                    && let Some(previous) = self.slice_preview_cursor_px
                    && let Some((size, scale_factor)) = viewport
                {
                    let fitted_zoom = crate::ui::state::fitted_slice_preview_zoom(self.editor.slice_half_length, size.height, scale_factor);
                    let current_zoom = fitted_zoom * self.editor.slice_preview_navigation.zoom_multiplier();
                    self.editor
                        .slice_preview_navigation
                        .pan_by_pixels([next.0 - previous.0, next.1 - previous.1], current_zoom, f64::from(size.height))
                } else {
                    false
                };
                self.slice_preview_cursor_px = Some(next);
                if changed && let Some(graphics) = self.graphics.as_ref() {
                    graphics.request_slice_preview_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.slice_preview_cursor_px = None;
                self.slice_preview_middle_down = false;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Middle,
                ..
            } => {
                self.slice_preview_middle_down = state == ElementState::Pressed;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, value) => f64::from(value) * 100.0,
                    MouseScrollDelta::PixelDelta(position) => position.y,
                };
                let changed = self
                    .graphics
                    .as_ref()
                    .and_then(|graphics| graphics.slice_preview_viewport())
                    .is_some_and(|(size, scale_factor)| {
                        let cursor = self.slice_preview_cursor_px.unwrap_or((f64::from(size.width) * 0.5, f64::from(size.height) * 0.5));
                        let fitted_zoom = crate::ui::state::fitted_slice_preview_zoom(self.editor.slice_half_length, size.height, scale_factor);
                        self.editor
                            .slice_preview_navigation
                            .zoom_at_pixel(scroll, [cursor.0, cursor.1], [f64::from(size.width), f64::from(size.height)], fitted_zoom)
                    });
                if changed && let Some(graphics) = self.graphics.as_ref() {
                    graphics.request_slice_preview_redraw();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        physical_key: PhysicalKey::Code(KeyCode::KeyF | KeyCode::Home),
                        ..
                    },
                ..
            } => {
                self.editor.slice_preview_navigation.reset();
                if let Some(graphics) = self.graphics.as_ref() {
                    graphics.request_slice_preview_redraw();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        physical_key: PhysicalKey::Code(KeyCode::Escape),
                        ..
                    },
                ..
            } => {
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.close_slice_preview();
                }
                self.editor.slice_preview_detached = false;
                self.slice_preview_cursor_px = None;
                self.slice_preview_middle_down = false;
                self.redraw_requested = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state,
                        physical_key: PhysicalKey::Code(key @ (KeyCode::KeyW | KeyCode::KeyS | KeyCode::KeyQ | KeyCode::KeyE)),
                        ..
                    },
                ..
            } if !(cfg!(target_os = "macos") || self.modifiers.control_key()) && self.modifiers.super_key() => {
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.slice_process_key(key, state == ElementState::Pressed);
                    graphics.request_slice_preview_redraw();
                }
                self.redraw_requested = true;
            }
            WindowEvent::Resized(size) => {
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.resize_slice_preview(size);
                }
            }
            WindowEvent::RedrawRequested => {
                let result = self.graphics.as_mut().map(|graphics| {
                    graphics.render_slice_preview(
                        &self.scene_document,
                        &self.triangulations,
                        &self.block_models,
                        &self.drill_holes,
                        &self.point_clouds,
                        &self.raster_textures,
                        &self.editor,
                    )
                });
                match result {
                    Some(Err(RenderSurfaceError::Lost | RenderSurfaceError::Outdated)) => {
                        if let Some(graphics) = self.graphics.as_mut() {
                            graphics.reconfigure_slice_preview();
                            graphics.request_slice_preview_redraw();
                        }
                    }
                    Some(Err(RenderSurfaceError::Timeout | RenderSurfaceError::Occluded)) => {
                        if let Some(graphics) = self.graphics.as_ref() {
                            graphics.request_slice_preview_redraw();
                        }
                    }
                    Some(Err(RenderSurfaceError::Validation)) => {
                        if let Some(graphics) = self.graphics.as_mut() {
                            graphics.close_slice_preview();
                        }
                        self.editor.slice_preview_detached = false;
                    }
                    Some(Ok(())) | None => {}
                }
            }
            _ => {}
        }
    }
}

impl<'a> App<'a> {
    fn handle_cursor_mode_cycle(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button,
            ..
        } = event
        else {
            return false;
        };

        self.editor.cursor_mode = match button {
            MouseButton::Forward => self.editor.cursor_mode.next(),
            MouseButton::Back => self.editor.cursor_mode.previous(),
            _ => return false,
        };
        self.editor.cursor_snapped = false;
        self.redraw_requested = true;
        true
    }

    fn handle_mouse_press(&mut self, event: &WindowEvent) {
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if self.editor.triangulation_pick_target.is_some() || self.editor.tri_cut_poly_awaiting_pick || self.editor.drill_pattern_awaiting_shape_pick {
                self.editor.canvas_context_menu_open = false;
                self.begin_select_or_drag();
                return;
            }
            self.editor.canvas_context_menu_open = false;
            match self.editor.active_tool {
                ActiveTool::MakePoint => self.place_point_at_cursor(),
                ActiveTool::MakeText if !self.editor.text_editing_enabled => self.text_tool_click(),
                ActiveTool::MakeLine | ActiveTool::MakePoly if !self.editor.poly_finish_dialog => self.place_stroke_point(),
                ActiveTool::MakeCircle => self.make_circle_click(),
                ActiveTool::MeasureDistance => self.measure_distance_click(),
                ActiveTool::MeasureBatterAngle => self.measure_batter_angle_click(),
                ActiveTool::VerticalSlice => self.slice_line_click(),
                ActiveTool::DeletePoints => {
                    self.editor.selection_box_start_px = self.editor.cursor_screen_px;
                    self.editor.selection_box_current_px = self.editor.cursor_screen_px;
                }
                ActiveTool::None => {
                    if !self.select_tie_at_cursor() {
                        self.begin_select_or_drag();
                    }
                }
                ActiveTool::Move => {
                    if let Some(cursor_px) = self.editor.cursor_screen_px {
                        match hit_gizmo_handle(&self.editor, cursor_px) {
                            Some(GizmoHandleHit::Plane(plane_idx, axes)) => self.begin_gizmo_plane_drag(plane_idx, axes, cursor_px),
                            Some(GizmoHandleHit::Axis(axis_idx, axis)) => self.begin_gizmo_drag(axis_idx, axis, cursor_px),
                            None => {
                                if !self.pick_move_vertex_target() {
                                    self.begin_select_or_drag();
                                }
                            }
                        }
                    } else {
                        self.begin_select_or_drag();
                    }
                }
                // Move Collar has no equivalent of the vertex target: a hole
                // is picked whole, so a press off the gizmo is an ordinary
                // Drill & Blast selection.
                ActiveTool::MoveCollar => {
                    match self
                        .editor
                        .cursor_screen_px
                        .and_then(|cursor_px| hit_gizmo_handle(&self.editor, cursor_px).map(|hit| (cursor_px, hit)))
                    {
                        Some((cursor_px, GizmoHandleHit::Plane(plane_idx, axes))) => self.begin_gizmo_plane_drag(plane_idx, axes, cursor_px),
                        Some((cursor_px, GizmoHandleHit::Axis(axis_idx, axis))) => self.begin_gizmo_drag(axis_idx, axis, cursor_px),
                        None => self.begin_select_or_drag(),
                    }
                }
                // Rotate Collar has nothing to grab but its rings, so a press
                // off them is an ordinary Drill & Blast selection - the same
                // rule Move Collar follows for a press off its gizmo.
                ActiveTool::RotateCollar => {
                    match self
                        .editor
                        .cursor_screen_px
                        .and_then(|cursor_px| hit_rotate_gizmo_ring(&self.editor, cursor_px).map(|ring| (cursor_px, ring)))
                    {
                        Some((cursor_px, ring)) => self.begin_collar_rotate_drag(ring, cursor_px),
                        None => self.begin_select_or_drag(),
                    }
                }
                ActiveTool::TieHoles => self.tie_holes_click(),
                ActiveTool::Chamfer => {
                    if let Some(cursor_px) = self.editor.cursor_screen_px {
                        if self.editor.chamfer_gizmo_hovered {
                            self.editor.chamfer_gizmo_drag_start_px = Some(cursor_px);
                            self.editor.chamfer_gizmo_drag_start_radius = self.editor.chamfer_radius;
                        } else {
                            self.pick_chamfer_corner();
                        }
                    }
                }
                ActiveTool::Bezier => {
                    if let Some(cursor_px) = self.editor.cursor_screen_px {
                        // Check if clicking on a CP handle first
                        let hit_cp = bezier_cp_hit(&self.editor, cursor_px);
                        if let Some(cp_idx) = hit_cp {
                            self.editor.bezier_dragging_cp = Some(cp_idx);
                        } else {
                            self.bezier_click();
                        }
                    }
                }
                ActiveTool::SetInitiationPoint => self.set_initiation_at_cursor(),
                ActiveTool::ExplodePolyline => self.explode_at_cursor(),
                ActiveTool::FuseIntoPolyline => self.fuse_click(),
                ActiveTool::SplitAtPoints => self.split_at_points_click(),
                ActiveTool::RelimitLine => self.relimit_line_click(),
                ActiveTool::OffsetElement => {
                    if self.editor.offset_awaiting_side_pick {
                        self.commit_offset();
                    } else if !self.editor.offset_dialog_open {
                        self.pick_offset_target();
                    }
                }
                ActiveTool::DrapeToTopology => self.begin_select_or_drag(),
                ActiveTool::BatterBermOffset if !self.editor.batter_berm_dialog_open => {
                    self.pick_batter_berm_target();
                }
                _ => {}
            }
        }
    }

    fn handle_mouse_release(&mut self, event: &WindowEvent) {
        if is_left_mouse_release(event) {
            self.finish_left_button_interactions();
        }
    }

    fn finish_left_button_interactions(&mut self) {
        self.finish_box_selection();
        self.finish_drag();
        if self.gizmo_drag.is_some() {
            self.finish_gizmo_drag();
        }
        if self.collar_rotate_drag.is_some() {
            self.finish_collar_rotate_drag();
        }
        if self.editor.chamfer_gizmo_drag_start_px.is_some() {
            self.editor.chamfer_gizmo_drag_start_px = None;
            self.invalidate_overlay();
        }
        if self.editor.bezier_dragging_cp.is_some() {
            self.editor.bezier_dragging_cp = None;
            self.invalidate_overlay();
        }
    }

    fn handle_right_press_for_orbit(&mut self, event: &WindowEvent) {
        if let WindowEvent::MouseInput {
            button: MouseButton::Right,
            state: ElementState::Pressed,
            ..
        } = event
        {
            self.right_press_px = self.editor.cursor_screen_px;
            self.right_orbit_active = false;
            // Drop the volume raycaster to interaction quality now rather
            // than when the press promotes to an orbit drag, so the drag's
            // first frames aren't queued behind full-quality renders.
            if let Some(graphics) = self.graphics.as_mut() {
                graphics.note_interaction_start();
            }
        }
    }

    fn handle_right_release(&mut self, event: &WindowEvent) {
        let measurement_tool_active = matches!(self.editor.active_tool, ActiveTool::MeasureDistance | ActiveTool::MeasureBatterAngle);
        if self.editor.fly_mode_enabled || (self.editor.slice_mode_enabled && !measurement_tool_active) {
            // View modes do not open the canvas context menu. Slice mode still
            // lets a quick right click reach the cancellation path while one
            // of its supported measurement tools is active.
            self.right_press_px = None;
            self.right_orbit_active = false;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Right,
            ..
        } = event
        {
            let orbit_was_active = self.right_orbit_active;
            self.right_orbit_active = false;
            let is_quick_press = match (self.right_press_px.take(), self.editor.cursor_screen_px) {
                (Some(press), Some(cur)) => {
                    let dx = cur.0 - press.0;
                    let dy = cur.1 - press.1;
                    (dx * dx + dy * dy).sqrt() < RIGHT_CLICK_DRAG_THRESHOLD_PX
                }
                _ => false,
            } && !orbit_was_active;
            if is_quick_press && self.editor.tie_anchor.is_some() {
                // The same thing a right click does to a polyline being drawn:
                // put the run down, without the canvas menu over the pattern.
                self.end_tie_chain();
                self.redraw_requested = true;
            } else if is_quick_press && matches!(self.editor.active_tool, ActiveTool::MakeLine | ActiveTool::MakePoly | ActiveTool::MakeCircle) {
                self.try_finish_tool();
                self.redraw_requested = true;
            } else if is_quick_press && self.editor.active_tool != ActiveTool::None {
                self.cancel_active_tool();
                self.redraw_requested = true;
            } else if is_quick_press && self.editor.active_tool == ActiveTool::None && !self.editor.tri_create_open {
                let frozen = &self.editor.frozen_handles;
                let picked = self.graphics.as_ref().and_then(|g| {
                    g.pick_scene_entity_at_cursor(
                        crate::app::PICK_THRESHOLD_PX,
                        &self.triangulations,
                        self.selectable_drill_holes(),
                        &self.editor.hidden_handles,
                        frozen,
                        self.editor.xray_enabled,
                    )
                });
                if let Some(pick) = picked {
                    let handle = pick.entity;
                    // Same rule the left-click path follows: Drill & Blast
                    // acts on the hole under the cursor, not its dataset.
                    let hole = pick.hole.filter(|_| self.editor.active_workspace == crate::ui::state::Workspace::DrillAndBlast);
                    if let crate::model::SceneEntityId::Object(id) = handle {
                        self.activate_project_for_object(id);
                    }
                    match hole {
                        Some(hole) if !self.editor.selected_drill_holes.contains(&hole) => {
                            self.editor.on_drill_hole_pick(hole, pick.world, crate::ui::state::SelectionMode::Replace);
                            self.invalidate_geometry();
                        }
                        None if !self.editor.selected_handles.contains(&handle) => {
                            self.editor.on_canvas_pick(handle, pick.world, crate::ui::state::SelectionMode::Replace);
                            self.invalidate_geometry();
                        }
                        _ => {}
                    }
                    self.active_triangulation = match handle {
                        crate::model::SceneEntityId::Triangulation(id) => Some(id),
                        _ => None,
                    };
                    self.editor.canvas_context_menu_open = true;
                    self.editor.canvas_context_menu_px = self.editor.cursor_screen_px;
                    self.redraw_requested = true;
                }
            }
        }
    }

    fn handle_control_shortcuts(&mut self, event: &WindowEvent) {
        if !self.editor.text_editing_enabled
            && (self.modifiers.control_key() || (cfg!(target_os = "macos") && self.modifiers.super_key()))
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        physical_key: PhysicalKey::Code(key),
                        ..
                    },
                ..
            } = event
        {
            match key {
                KeyCode::KeyZ if self.modifiers.shift_key() => self.apply_history_step(false),
                KeyCode::KeyZ => self.apply_history_step(true),
                KeyCode::KeyY => self.apply_history_step(false),
                KeyCode::KeyS => {
                    if let Err(error) = self.save_dirty_project() {
                        userspace_error!("{}", tr_format!(literal = "Couldn't save: %error%", error = error));
                    }
                }
                KeyCode::KeyA => {
                    self.select_all_active_objects();
                }
                KeyCode::KeyD => {
                    self.duplicate_selection();
                }
                _ => {}
            }
        }
    }

    fn maybe_start_right_orbit_drag(&mut self) {
        if self.editor.fly_mode_enabled || self.right_orbit_active {
            return;
        }
        let (Some(press), Some(cur)) = (self.right_press_px, self.editor.cursor_screen_px) else {
            return;
        };
        // The cursor is moving with the right button held: keep the volume
        // raycaster at interaction quality even before the drag threshold is
        // crossed, and across a still hold that outlives the press-time
        // cooldown before the drag starts.
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.note_interaction_start();
        }
        let dx = cur.0 - press.0;
        let dy = cur.1 - press.1;
        if (dx * dx + dy * dy).sqrt() < RIGHT_CLICK_DRAG_THRESHOLD_PX {
            return;
        }

        if self.editor.slice_mode_enabled {
            // No orbiting in slice mode; the camera is fully derived from the
            // slice state (Q/E rotates the slice line instead).
            return;
        }

        self.refresh_snap_index();
        let Some(graphics) = self.graphics.as_mut() else {
            return;
        };
        graphics.begin_orbit_at_surface(
            &self.triangulations,
            &self.drill_holes,
            &self.editor.hidden_handles,
            &self.editor.frozen_handles,
            &self.scene_document,
            &self.snap_index,
        );
        graphics.begin_right_orbit_drag();
        self.right_orbit_active = true;
        self.redraw_requested = true;
    }

    fn handle_key_action(&mut self, event: &WindowEvent) {
        let WindowEvent::KeyboardInput {
            event: KeyEvent {
                state,
                physical_key: PhysicalKey::Code(key),
                ..
            },
            ..
        } = event
        else {
            return;
        };
        match state {
            ElementState::Pressed => self.handle_key_code(*key),
            ElementState::Released => {
                // Once the Enter that opened the polyline finish dialog is
                // released, let its Enter shortcut work. Until then a held key
                // must not skip straight past the Open/Closed choice.
                if matches!(key, KeyCode::Enter | KeyCode::NumpadEnter) && self.editor.poly_finish_dialog && !self.editor.poly_finish_dialog_confirm_armed {
                    self.editor.poly_finish_dialog_confirm_armed = true;
                    self.redraw_requested = true;
                }
            }
        }
    }

    /// Bridge the one-frame gap between creating the cursor-following radius
    /// field and egui granting it keyboard focus. Once focused, egui consumes
    /// these events normally; this fallback only sees otherwise-unclaimed keys.
    fn handle_circle_radius_key_input(&mut self, event: &WindowEvent) -> bool {
        if self.editor.active_tool != ActiveTool::MakeCircle
            || self.editor.circle_draft.is_none()
            || self.modifiers.control_key()
            || self.modifiers.alt_key()
            || (cfg!(target_os = "macos") && self.modifiers.super_key())
        {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    state: ElementState::Pressed,
                    physical_key: PhysicalKey::Code(key),
                    text,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };

        match key {
            KeyCode::Backspace => {
                if let Some(draft) = self.editor.circle_draft.as_mut() {
                    draft.radius_text.pop();
                    if draft.radius_text.is_empty() {
                        draft.typing_origin_screen_px = None;
                    }
                    draft.focus_requested = true;
                }
                self.invalidate_overlay();
                return true;
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                self.commit_circle_typed_radius();
                self.redraw_requested = true;
                return true;
            }
            _ => {}
        }

        let Some(text) = text.as_deref() else {
            return false;
        };
        if text.is_empty() || !text.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
            return false;
        }

        let cursor_screen_px = self.editor.cursor_screen_px;
        if let Some(draft) = self.editor.circle_draft.as_mut() {
            let was_empty = draft.radius_text.is_empty();
            for ch in text.chars() {
                if ch.is_ascii_digit() || (ch == '.' && !draft.radius_text.contains('.')) {
                    draft.radius_text.push(ch);
                }
            }
            draft.note_radius_text_changed(was_empty, cursor_screen_px);
            draft.focus_requested = true;
        }
        self.invalidate_overlay();
        true
    }

    fn handle_key_code(&mut self, key: KeyCode) {
        // An open dialog owns Enter/Escape: the GUI only reports the press as
        // consumed when a text field has focus, so without this the viewport
        // tools would act on the same key before the dialog is drawn. The
        // dialog itself consumes the key from egui's queue next frame.
        let dialog_owns_key = match key {
            KeyCode::Enter | KeyCode::NumpadEnter => self.editor.dialog_owns_confirm_key(),
            KeyCode::Escape => self.editor.dialog_owns_cancel_key(),
            _ => false,
        };
        if dialog_owns_key {
            self.redraw_requested = true;
            return;
        }
        match &key {
            KeyCode::Escape => {
                if self.editor.tie_anchor.is_some() {
                    self.end_tie_chain();
                    self.redraw_requested = true;
                } else if self.editor.canvas_context_menu_open {
                    self.editor.canvas_context_menu_open = false;
                    self.redraw_requested = true;
                } else if self.editor.text_editing_enabled {
                    self.cancel_text_edit();
                } else if self.editor.triangulation_pick_target.is_some() {
                    self.editor.triangulation_pick_target = None;
                    self.editor.viewport_pick_hover_label = None;
                    self.editor.tri_hover_handles.clear();
                    self.invalidate_geometry();
                } else if self.editor.tri_cut_poly_awaiting_pick {
                    self.editor.tri_cut_poly_awaiting_pick = false;
                    self.editor.viewport_pick_hover_label = None;
                    self.editor.tool_highlight_id = self.editor.tri_cut_poly_object_id;
                    self.invalidate_geometry();
                } else if self.editor.drill_pattern_awaiting_shape_pick {
                    self.editor.drill_pattern_awaiting_shape_pick = false;
                    self.editor.viewport_pick_hover_label = None;
                    self.editor.tool_highlight_id = self.editor.drill_pattern_boundary_id;
                    self.invalidate_geometry();
                } else if self.editor.slice_mode_enabled {
                    self.set_slice_mode_enabled(false);
                } else if self.editor.active_tool == ActiveTool::VerticalSlice {
                    self.editor.slice_pending_start = None;
                    self.editor.active_tool = ActiveTool::None;
                    self.invalidate_overlay();
                } else if self.editor.active_tool == ActiveTool::DrapeToTopology {
                    self.cancel_drape();
                } else if self.editor.offset_awaiting_side_pick || self.editor.offset_dialog_open {
                    self.cancel_offset();
                } else if self.editor.relimit_confirming_end || self.editor.relimit_waiting_for_pick || self.editor.relimit_awaiting_source_pick || self.editor.relimit_dialog_open
                {
                    self.cancel_relimit();
                } else if self.editor.active_tool == ActiveTool::FuseIntoPolyline {
                    self.cancel_fuse();
                    self.editor.active_tool = ActiveTool::None;
                } else if self.editor.active_tool == ActiveTool::SplitAtPoints {
                    self.cancel_split_at_points();
                } else if self.editor.active_tool.translates() || self.editor.active_tool.rotates() {
                    self.gizmo_drag = None;
                    self.editor.gizmo_drag_axis_index = None;
                    self.editor.gizmo_drag_plane_index = None;
                    self.cancel_move_delta();
                    self.editor.active_tool = ActiveTool::None;
                } else if self.editor.active_tool == ActiveTool::Chamfer {
                    self.cancel_chamfer();
                } else if self.editor.active_tool == ActiveTool::Bezier {
                    self.cancel_bezier();
                } else if self.editor.active_tool == ActiveTool::ExplodePolyline {
                    self.editor.tool_highlight_id = None;
                    self.editor.active_tool = ActiveTool::None;
                    self.invalidate_geometry();
                } else if self.editor.active_tool == ActiveTool::BatterBermOffset || self.editor.batter_berm_dialog_open || self.editor.batter_berm_target_id.is_some() {
                    self.cancel_batter_berm();
                } else if self.editor.active_tool == ActiveTool::MakeCircle {
                    if self.editor.circle_draft.as_ref().is_some_and(|draft| !draft.radius_text.is_empty()) {
                        if let Some(draft) = self.editor.circle_draft.as_mut() {
                            draft.radius_text.clear();
                            draft.typing_origin_screen_px = None;
                            draft.focus_requested = true;
                        }
                        self.invalidate_overlay();
                    } else if self.editor.circle_draft.take().is_some() {
                        self.invalidate_overlay();
                    } else {
                        self.editor.active_tool = ActiveTool::None;
                        self.invalidate_overlay();
                    }
                } else if !self.editor.pending_stroke.is_empty() || self.editor.active_tool != ActiveTool::None {
                    self.discard_stroke();
                }
            }
            KeyCode::Backquote => {
                let picked = self.graphics.as_ref().and_then(|graphics| {
                    graphics.pick_at_cursor(
                        crate::app::PICK_THRESHOLD_PX,
                        &self.triangulations,
                        &self.editor.hidden_handles,
                        &self.editor.frozen_handles,
                        self.editor.xray_enabled,
                    )
                });
                if let Some((_handle, world)) = picked
                    && world.z.is_finite()
                {
                    let z = (world.z * 10_000.0).round() / 10_000.0;
                    self.editor.z_level = z;
                    self.editor.z_input = z;
                    self.redraw_requested = true;
                    crate::logging::report_completed_action(
                        CommandReportSpec::new(crate::i18n::tr!(literal = "Set Elevation"), format!("Z {z:.4}")),
                        crate::i18n::tr_format!(literal = "Set elevation from cursor hit to Z %z%", z = format!("{z:.4}")),
                    );
                }
            }
            KeyCode::Enter | KeyCode::NumpadEnter if !self.editor.text_editing_enabled => {
                if self.editor.active_tool.translates() {
                    let d = self.editor.move_panel_delta;
                    self.apply_move_delta(glam::DVec3::new(d[0], d[1], d[2]));
                    self.editor.active_tool = ActiveTool::None;
                } else if self.editor.active_tool.rotates() {
                    self.apply_pending_collar_rotation();
                    self.editor.active_tool = ActiveTool::None;
                } else if self.editor.active_tool == ActiveTool::OffsetElement && self.editor.offset_awaiting_side_pick {
                    self.commit_offset();
                } else if self.editor.active_tool == ActiveTool::DrapeToTopology {
                    self.confirm_drape_selection();
                } else if self.editor.active_tool == ActiveTool::Bezier {
                    self.apply_bezier();
                } else {
                    self.try_finish_tool();
                }
            }
            KeyCode::Delete | KeyCode::Backspace if !self.editor.text_editing_enabled => {
                if !self.editor.selected_tie_ins.is_empty() {
                    self.delete_selected_tie_ins();
                    return;
                }
                let object_count = self.editor.selected_handles.iter().filter(|h| matches!(h, crate::model::SceneEntityId::Object(_))).count();
                if object_count > 0 {
                    self.editor.delete_confirm_open = true;
                    self.redraw_requested = true;
                }
            }
            _ => {}
        }
    }

    /// Cancel the current active tool, mirroring the Escape key behaviour.
    pub(crate) fn cancel_active_tool(&mut self) {
        if self.editor.text_editing_enabled {
            return; // let the text editor handle it
        }
        if self.editor.triangulation_pick_target.is_some() {
            self.editor.triangulation_pick_target = None;
            self.editor.viewport_pick_hover_label = None;
            self.editor.tri_hover_handles.clear();
            self.invalidate_geometry();
        } else if self.editor.tri_cut_poly_awaiting_pick {
            self.editor.tri_cut_poly_awaiting_pick = false;
            self.editor.viewport_pick_hover_label = None;
            self.editor.tool_highlight_id = self.editor.tri_cut_poly_object_id;
            self.invalidate_geometry();
        } else if self.editor.drill_pattern_awaiting_shape_pick {
            self.editor.drill_pattern_awaiting_shape_pick = false;
            self.editor.viewport_pick_hover_label = None;
            self.editor.tool_highlight_id = self.editor.drill_pattern_boundary_id;
            self.invalidate_geometry();
        } else if self.editor.active_tool == ActiveTool::DrapeToTopology {
            self.cancel_drape();
        } else if self.editor.offset_awaiting_side_pick || self.editor.offset_dialog_open {
            self.cancel_offset();
        } else if self.editor.relimit_confirming_end || self.editor.relimit_waiting_for_pick || self.editor.relimit_awaiting_source_pick || self.editor.relimit_dialog_open {
            self.cancel_relimit();
        } else if self.editor.active_tool == ActiveTool::FuseIntoPolyline {
            self.cancel_fuse();
            self.editor.active_tool = ActiveTool::None;
        } else if self.editor.active_tool == ActiveTool::SplitAtPoints {
            self.cancel_split_at_points();
        } else if self.editor.active_tool.translates() || self.editor.active_tool.rotates() {
            self.gizmo_drag = None;
            self.editor.gizmo_drag_axis_index = None;
            self.editor.gizmo_drag_plane_index = None;
            self.cancel_move_delta();
            self.editor.active_tool = ActiveTool::None;
        } else if self.editor.active_tool == ActiveTool::ExplodePolyline {
            self.editor.tool_highlight_id = None;
            self.editor.active_tool = ActiveTool::None;
            self.invalidate_geometry();
        } else if self.editor.active_tool == ActiveTool::Chamfer {
            self.cancel_chamfer();
        } else if self.editor.active_tool == ActiveTool::Bezier {
            self.cancel_bezier();
        } else if self.editor.active_tool == ActiveTool::BatterBermOffset || self.editor.batter_berm_dialog_open || self.editor.batter_berm_target_id.is_some() {
            self.cancel_batter_berm();
        } else if self.editor.active_tool == ActiveTool::MeasureBatterAngle {
            self.editor.batter_angle_points.clear();
            self.editor.active_tool = ActiveTool::None;
            self.invalidate_overlay();
        } else if self.editor.active_tool == ActiveTool::VerticalSlice {
            self.editor.slice_pending_start = None;
            self.editor.active_tool = ActiveTool::None;
            self.invalidate_overlay();
        } else if self.editor.active_tool == ActiveTool::MakeCircle {
            self.editor.circle_draft = None;
            self.editor.active_tool = ActiveTool::None;
            self.invalidate_overlay();
        } else if !self.editor.pending_stroke.is_empty() {
            self.discard_stroke();
        } else {
            self.editor.active_tool = ActiveTool::None;
        }
    }

    pub(crate) fn set_active_tool_from_toolbar(&mut self, tool: ActiveTool) {
        if self.editor.text_editing_enabled {
            return;
        }
        if self.editor.drill_pattern_open {
            self.editor.close_drill_pattern();
            self.invalidate_geometry();
        }
        let allowed_in_slice = matches!(tool, ActiveTool::None | ActiveTool::MeasureDistance | ActiveTool::MeasureBatterAngle);
        if (self.editor.fly_mode_enabled && tool != ActiveTool::None) || (self.editor.slice_mode_enabled && !allowed_in_slice) {
            return;
        }

        let previous_tool = self.editor.active_tool;
        let next_tool = if previous_tool == tool { ActiveTool::None } else { tool };

        match previous_tool {
            ActiveTool::TieHoles if next_tool != ActiveTool::TieHoles => {
                self.end_tie_chain();
            }
            ActiveTool::OffsetElement if next_tool != ActiveTool::OffsetElement => {
                self.cancel_offset();
            }
            ActiveTool::DrapeToTopology if next_tool != ActiveTool::DrapeToTopology => {
                self.cancel_drape();
            }
            ActiveTool::RelimitLine if next_tool != ActiveTool::RelimitLine => {
                self.cancel_relimit();
            }
            ActiveTool::BatterBermOffset if next_tool != ActiveTool::BatterBermOffset => {
                self.cancel_batter_berm();
            }
            ActiveTool::Bezier if next_tool != ActiveTool::Bezier => {
                self.cancel_bezier();
            }
            ActiveTool::Chamfer if next_tool != ActiveTool::Chamfer => {
                self.cancel_chamfer();
            }
            ActiveTool::SplitAtPoints if next_tool != ActiveTool::SplitAtPoints => {
                self.cancel_split_at_points();
            }
            _ => {}
        }

        self.editor.active_tool = next_tool;
        if next_tool == ActiveTool::DrapeToTopology && previous_tool != ActiveTool::DrapeToTopology {
            self.editor.drape_phase = crate::ui::state::DrapePhase::Designs;
            self.editor.drape_object_ids.clear();
            self.editor.selected_handles.clear();
            self.invalidate_geometry();
        }
        // Arming a translate tool puts down whatever it cannot translate: the
        // selection it inherits is held to the same rule as the picks it will
        // make from here - see `App::tool_accepts_pick` - so the gizmo never
        // comes up over something the tool would leave where it is.
        let held = self.editor.selected_handles.len() + self.editor.selected_drill_holes.len();
        match next_tool {
            ActiveTool::Move => {
                self.editor.selected_drill_holes.clear();
                self.editor.selected_handles.retain(|handle| matches!(handle, crate::model::SceneEntityId::Object(_)));
            }
            ActiveTool::MoveCollar | ActiveTool::RotateCollar => self.editor.selected_handles.clear(),
            _ => {}
        }
        if self.editor.selected_handles.len() + self.editor.selected_drill_holes.len() != held {
            self.invalidate_geometry();
        }
        if next_tool != ActiveTool::MakeCircle && self.editor.circle_draft.take().is_some() {
            self.invalidate_overlay();
        }
        if next_tool != ActiveTool::None {
            self.editor.new_layer_dialog_open = false;
        }

        if matches!(tool, ActiveTool::MeasureDistance | ActiveTool::MeasureBatterAngle) {
            self.editor.measurement_start = None;
            self.editor.measurement_end = None;
            self.editor.batter_angle_points.clear();
        }
    }

    pub(crate) fn set_fly_mode_enabled(&mut self, enabled: bool) {
        if self.editor.fly_mode_enabled == enabled {
            return;
        }

        if enabled {
            if self.editor.text_editing_enabled {
                self.cancel_text_edit();
            } else {
                self.cancel_active_tool();
            }
            // Fly and slice modes are mutually exclusive: both claim W/S and
            // right-drag.
            self.set_slice_mode_enabled(false);
            self.finish_left_button_interactions();
            self.editor.canvas_context_menu_open = false;
            self.editor.cursor_snapped = false;
            self.right_press_px = None;
            self.right_orbit_active = false;
        }

        self.editor.fly_mode_enabled = enabled;
        self.redraw_requested = true;
    }
}

fn bezier_cp_hit(editor: &crate::ui::state::EditorState, cursor_px: (f32, f32)) -> Option<u8> {
    if editor.bezier_selected_verts[0].is_none() || editor.bezier_selected_verts[1].is_none() {
        return None;
    }
    const HANDLE_THRESHOLD_PX: f32 = 12.0;
    let dist = |(ax, ay): (f32, f32), (bx, by): (f32, f32)| -> f32 { ((ax - bx) * (ax - bx) + (ay - by) * (ay - by)).sqrt() };
    let mut best: Option<(u8, f32)> = None;
    if let Some(cp1_px) = editor.bezier_cp1_screen_px {
        let d = dist(cursor_px, cp1_px);
        if d < HANDLE_THRESHOLD_PX {
            best = Some((0, d));
        }
    }
    if let Some(cp2_px) = editor.bezier_cp2_screen_px {
        let d = dist(cursor_px, cp2_px);
        if d < HANDLE_THRESHOLD_PX && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((1, d));
        }
    }
    best.map(|(idx, _)| idx)
}

fn is_left_mouse_release(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        }
    )
}

/// Cursor slack around a gizmo handle, in logical points.
const GIZMO_HIT_POINTS: f32 = 9.0;

/// Which Move gizmo handle the cursor is over.
enum GizmoHandleHit {
    Axis(u8, glam::DVec3),
    /// A plane handle, or the view-aligned ring as `MOVE_GIZMO_VIEW_PLANE`.
    Plane(u8, [glam::DVec3; 2]),
}

impl GizmoHandleHit {
    fn axis_index(&self) -> Option<u8> {
        match *self {
            GizmoHandleHit::Axis(index, _) => Some(index),
            GizmoHandleHit::Plane(..) => None,
        }
    }

    fn plane_index(&self) -> Option<u8> {
        match *self {
            GizmoHandleHit::Plane(index, _) => Some(index),
            GizmoHandleHit::Axis(..) => None,
        }
    }
}

fn point_to_segment_distance(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let px = point.0 - start.0;
    let py = point.1 - start.1;
    let bx = end.0 - start.0;
    let by = end.1 - start.1;
    let length_sq = bx * bx + by * by;
    if length_sq < 1e-8 {
        return px.hypot(py);
    }
    let t = ((px * bx + py * by) / length_sq).clamp(0.0, 1.0);
    (px - t * bx).hypot(py - t * by)
}

/// Handle under the cursor, preferring the smaller targets: plane handles,
/// then axis arrows, then the ring the arrows pass through.
fn hit_gizmo_handle(editor: &crate::ui::state::EditorState, cursor_px: (f32, f32)) -> Option<GizmoHandleHit> {
    let gizmo = &editor.move_gizmo;
    let center = gizmo.center_px?;
    let threshold = GIZMO_HIT_POINTS * gizmo.scale_factor.max(1.0);

    let plane_axes = [[glam::DVec3::X, glam::DVec3::Y], [glam::DVec3::X, glam::DVec3::Z], [glam::DVec3::Y, glam::DVec3::Z]];
    let plane = gizmo.plane_quad_px.iter().enumerate().find_map(|(index, quad)| {
        let quad = quad.as_ref()?;
        (gizmo.plane_fade[index] > 0.0 && point_in_convex_quad(cursor_px, quad)).then_some(GizmoHandleHit::Plane(index as u8, plane_axes[index]))
    });
    if plane.is_some() {
        return plane;
    }

    let mut best: Option<(u8, glam::DVec3, f32)> = None;
    for (index, axis) in [glam::DVec3::X, glam::DVec3::Y, glam::DVec3::Z].into_iter().enumerate() {
        // A faded-out axis is not clickable: its screen direction is unreliable
        // and the user cannot see what they would be grabbing.
        if gizmo.axis_fade[index] <= 0.0 {
            continue;
        }
        let Some(tip) = gizmo.axis_tip_px[index] else {
            continue;
        };
        // Arrows are drawn from the ring outwards, so start the hit segment
        // there and leave the ring itself grabbable.
        let start = gizmo_axis_start_px(center, tip, gizmo.ring_radius_px);
        let distance = point_to_segment_distance(cursor_px, start, tip);
        if distance < threshold && best.is_none_or(|(_, _, best_distance)| distance < best_distance) {
            best = Some((index as u8, axis, distance));
        }
    }
    if let Some((index, axis, _)) = best {
        return Some(GizmoHandleHit::Axis(index, axis));
    }

    let view_axes = gizmo.view_axes?;
    let distance = (cursor_px.0 - center.0).hypot(cursor_px.1 - center.1);
    ((distance - gizmo.ring_radius_px).abs() < threshold * 0.7).then_some(GizmoHandleHit::Plane(crate::ui::state::MOVE_GIZMO_VIEW_PLANE, view_axes))
}

/// Which Rotate Collar ring the cursor is over.
///
/// Whichever ring passes closest wins, so the two can cross - which they do
/// from most viewpoints - without either becoming ungrabbable near the join.
fn hit_rotate_gizmo_ring(editor: &crate::ui::state::EditorState, cursor_px: (f32, f32)) -> Option<u8> {
    use crate::ui::state::{ROTATE_GIZMO_AZIMUTH_RING, ROTATE_GIZMO_DIP_RING};
    let gizmo = &editor.rotate_gizmo;
    gizmo.center_px?;
    let threshold = GIZMO_HIT_POINTS * gizmo.scale_factor.max(1.0);
    let mut best: Option<(u8, f32)> = None;
    for ring in [ROTATE_GIZMO_AZIMUTH_RING, ROTATE_GIZMO_DIP_RING] {
        let index = usize::from(ring);
        // A faded-out ring is not clickable: the user cannot see what they
        // would be grabbing, and a sweep round it would not read reliably.
        if gizmo.ring_fade[index] <= 0.0 {
            continue;
        }
        let points = &gizmo.ring_px[index];
        if points.len() < 2 {
            continue;
        }
        // The ring closes, so the last sample joins back to the first.
        let distance = points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .take(points.len())
            .map(|(&from, &to)| point_to_segment_distance(cursor_px, from, to))
            .fold(f32::INFINITY, f32::min);
        if distance < threshold && best.is_none_or(|(_, closest)| distance < closest) {
            best = Some((ring, distance));
        }
    }
    best.map(|(ring, _)| ring)
}

/// Point on the ring where an axis arrow starts, given its projected tip.
fn gizmo_axis_start_px(center: (f32, f32), tip: (f32, f32), ring_radius_px: f32) -> (f32, f32) {
    let dx = tip.0 - center.0;
    let dy = tip.1 - center.1;
    let length = dx.hypot(dy);
    if length <= ring_radius_px {
        return center;
    }
    (center.0 + dx / length * ring_radius_px, center.1 + dy / length * ring_radius_px)
}

fn point_in_convex_quad(point: (f32, f32), quad: &[(f32, f32); 4]) -> bool {
    let mut orientation = 0.0f32;
    for index in 0..quad.len() {
        let start = quad[index];
        let end = quad[(index + 1) % quad.len()];
        let cross = (end.0 - start.0) * (point.1 - start.1) - (end.1 - start.1) * (point.0 - start.0);
        if cross.abs() <= f32::EPSILON {
            continue;
        }
        if orientation == 0.0 {
            orientation = cross.signum();
        } else if cross.signum() != orientation {
            return false;
        }
    }
    orientation != 0.0
}
