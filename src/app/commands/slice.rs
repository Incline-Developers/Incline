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
        self.editor.cursor_snapped = false;
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
        if !self.editor.slice_mode_enabled {
            return;
        }
        self.editor.slice_mode_enabled = false;
        let discarded_vertices = self.editor.pending_stroke.len();
        let discarded_measurement = self.editor.measurement_start.is_some() || !self.editor.batter_angle_points.is_empty();
        self.discard_stroke();
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
        self.redraw_requested = true;
        userspace_log!("{}", tr!(literal = "Exited slice view"));
        if discarded_vertices > 0 {
            userspace_log!("{}", tr!("slice-discarded-vertices", count = discarded_vertices));
        }
        if discarded_measurement {
            userspace_log!("{}", tr!(literal = "Discarded the measurement picked on the section"));
        }
    }
}
