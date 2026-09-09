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
        self.redraw_requested = true;
        userspace_log!("{}", tr!(literal = "Exited slice view"));
    }

    /// Squares the section camera to its plane; undoes only the orbit, leaving direction, slab position, pan and zoom untouched.
    pub(crate) fn reset_slice_view(&mut self) {
        if !self.graphics.as_mut().is_some_and(|graphics| graphics.reset_slice_view()) {
            return;
        }
        self.end_right_orbit();
        self.redraw_requested = true;
        userspace_log!("{}", tr!(literal = "Reset the section view"));
    }

    /// Whether the cursor may be re-projected onto the section: yes when the section moves with no mouse event behind it, not while a right drag is orbiting it.
    pub(crate) fn slice_cursor_tracks_section(&self) -> bool {
        self.editor.slice_mode_enabled && !self.right_orbit_active
    }

    /// Toggles the section grid: elevation levels plus easting/northing lines where the cut face crosses them. Not persisted with the project.
    pub(crate) fn set_slice_grid_enabled(&mut self, enabled: bool) {
        self.editor.slice_grid_enabled = enabled;
        self.redraw_requested = true;
        userspace_log!("{}", tr_format!(literal = "Set section grid = %enabled%", enabled = enabled));
    }

    /// Re-projects the cursor onto the section after a camera move with no mouse event behind it; always unsnapped, since a section never honours a snap.
    pub(crate) fn refresh_slice_cursor(&mut self) {
        if !self.slice_cursor_tracks_section() {
            return;
        }
        let Some(world) = self.graphics.as_ref().and_then(|graphics| graphics.cursor_world(self.editor.z_level)) else {
            return;
        };
        if self.editor.cursor_world == Some(world) {
            return;
        }
        self.editor.cursor_world = Some(world);
        self.editor.cursor_snapped = false;
        if self.editor.overlay_follows_cursor() {
            self.invalidate_overlay();
        }
    }
}
