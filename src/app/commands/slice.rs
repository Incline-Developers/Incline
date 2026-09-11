//! Vertical slice view: two-click line placement and mode enter/exit.

use glam::DVec2;

use crate::{
    app::App,
    i18n::{tr, tr_format},
    ui::state::ActiveTool,
    userspace_log,
};

impl<'a> App<'a> {
    /// Canvas click while the Vertical Slice tool is armed. First click
    /// stores the line start; the second computes the slice frame and enters
    /// slice mode. Z of the picks only seeds the initial view elevation -
    /// the line is flat in XY by construction.
    pub(crate) fn slice_line_click(&mut self) {
        if self.editor.snapping_active() && !self.editor.cursor_snapped {
            return;
        }
        let Some(point) = self.editor.cursor_world else {
            return;
        };

        let Some(start) = self.editor.slice_pending_start else {
            self.editor.slice_pending_start = Some(point);
            self.invalidate_overlay();
            return;
        };

        let delta = (point - start).truncate();
        if delta.length_squared() <= 1.0e-12 {
            return;
        }
        let direction: DVec2 = delta.normalize();
        let center = (start + point) * 0.5;
        let half_length = delta.length() * 0.5;

        self.editor.slice_pending_start = None;
        self.editor.active_tool = ActiveTool::None;
        self.editor.slice_preview_navigation.reset();
        self.slice_preview_cursor_px = None;
        self.slice_preview_middle_down = false;
        self.set_fly_mode_enabled(false);
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.set_fly_mode_enabled(false);
            graphics.enter_slice_mode(
                center,
                direction,
                half_length,
                self.editor.slice_width_input,
                self.editor.slice_speed_input,
                self.editor.slice_rotate_input.to_radians(),
            );
            self.editor.slice_mode_enabled = true;
        }
        // No mouse event behind this camera swap; ending any orbit lets the cursor land on the section.
        self.end_right_orbit();
        self.invalidate_overlay();
        self.redraw_requested = true;
        userspace_log!(
            "{}",
            tr_format!(
                literal = "Entered slice view @ %cx%, %cy%, %cz% along %dx%, %dy% (%length%m line)",
                cx = format!("{:.3}", center.x),
                cy = format!("{:.3}", center.y),
                cz = format!("{:.3}", center.z),
                dx = format!("{:.3}", direction.x),
                dy = format!("{:.3}", direction.y),
                length = half_length * 2.0
            )
        );
    }

    /// Toggle the vertical slice viewing mode. Enabling without a drawn line
    /// is meaningless, so `true` only arms the placement tool.
    pub(crate) fn set_slice_mode_enabled(&mut self, enabled: bool) {
        if enabled {
            if !self.editor.slice_mode_enabled {
                self.set_active_tool_from_toolbar(ActiveTool::VerticalSlice);
            }
            return;
        }
        self.leave_slice_mode();
    }

    /// Single exit point for slice mode; a half-drawn stroke, the active tool and any picks carry over into plan view.
    pub(crate) fn leave_slice_mode(&mut self) {
        if !self.editor.slice_mode_enabled {
            return;
        }
        self.editor.slice_mode_enabled = false;
        self.editor.slice_preview_detached = false;
        self.editor.slice_preview_navigation.reset();
        self.slice_preview_cursor_px = None;
        self.slice_preview_middle_down = false;
        self.editor.selection_box_start_px = None;
        self.editor.selection_box_current_px = None;
        self.pending_selection_click = None;
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.close_slice_preview();
            graphics.exit_slice_mode();
        }
        self.end_right_orbit();
        self.clear_rotation_centre();
        self.redraw_requested = true;
        userspace_log!("{}", tr!(literal = "Exited slice view"));
    }

    /// Reset View while sliced: square the camera to the section plane, then
    /// frame what is visible on it. The section itself - its direction and
    /// where the slab sits along its normal - is left exactly where it is,
    /// which is what lets Reset View mean something here without dropping the
    /// mode to get back to a plan view.
    pub(crate) fn reset_slice_view(&mut self) {
        if let Some(graphics) = self.graphics.as_mut() {
            if !graphics.reset_slice_view(self.editor.rotation_centre) {
                return;
            }
            // Squared up first, so the fit measures the section as it will be seen.
            graphics.zoom_to_extents(
                &self.scene_document,
                &self.triangulations,
                &self.block_models,
                &self.drill_holes,
                &self.point_clouds,
                &self.editor.hidden_handles,
            );
        }
        self.end_right_orbit();
        self.redraw_requested = true;
        userspace_log!("{}", tr!(literal = "Reset the section view (fit to extents)"));
    }

    /// Whether the cursor may be re-projected onto the section: yes when the section moves with no mouse event behind it, not while a right drag is orbiting it.
    pub(crate) fn slice_cursor_tracks_section(&self) -> bool {
        self.editor.slice_mode_enabled && !self.right_orbit_active
    }

    /// The RL grid in a section, the XY grid in plan. One button drives both,
    /// so which one it means is decided here.
    pub(crate) fn set_grid_shown(&mut self, shown: bool) -> anyhow::Result<()> {
        if self.editor.slice_mode_enabled {
            self.set_slice_grid_enabled(shown);
        } else if self.editor.show_xy_grid != shown {
            self.set_xy_grid_shown(shown);
        }
        Ok(())
    }

    /// Toggles the section grid: elevation levels plus easting/northing lines where the cut face crosses them. Not persisted with the project.
    pub(crate) fn set_slice_grid_enabled(&mut self, enabled: bool) {
        self.editor.slice_grid_enabled = enabled;
        self.redraw_requested = true;
        userspace_log!("{}", tr_format!(literal = "Set section grid = %enabled%", enabled = enabled));
    }

    /// Re-places the cursor after a camera move with no mouse event behind it.
    /// Walking the section carries its targets past a still mouse, so the snap
    /// is asked again here rather than dropped; off any target the cursor
    /// falls back to the section plane under it.
    pub(crate) fn refresh_slice_cursor(&mut self) {
        if !self.slice_cursor_tracks_section() {
            return;
        }
        let snapped = if self.editor.active_tool.snaps_cursor() && self.editor.snapping_active() {
            self.cursor_snap_point(web_time::Instant::now())
        } else {
            None
        };
        let Some(world) = snapped.or_else(|| self.graphics.as_ref().and_then(|graphics| graphics.cursor_world(self.editor.z_level))) else {
            return;
        };
        if self.editor.cursor_world == Some(world) && self.editor.cursor_snapped == snapped.is_some() {
            return;
        }
        self.editor.cursor_world = Some(world);
        self.editor.cursor_snapped = snapped.is_some();
        if self.editor.overlay_follows_cursor() {
            self.invalidate_overlay();
        }
    }
}
