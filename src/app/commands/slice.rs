//! Vertical slice view: two-click line placement and mode enter/exit.

use glam::DVec2;

use crate::{app::App, ui::state::ActiveTool, userspace_log};

impl<'a> App<'a> {
    /// Canvas click while the Vertical Slice tool is armed. First click
    /// stores the line start; the second computes the slice frame and enters
    /// slice mode. Z of the picks only seeds the initial view elevation —
    /// the line is flat in XY by construction.
    pub(crate) fn slice_line_click(&mut self) {
        if matches!(
            self.editor.cursor_mode,
            crate::ui::state::CursorMode::SnapToPoint | crate::ui::state::CursorMode::SnapToLine | crate::ui::state::CursorMode::SnapToSurface
        ) && !self.editor.cursor_snapped
        {
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
            "Entered slice view @ {:.3}, {:.3}, {:.3} along {:.3}, {:.3} ({}m line)",
            center.x,
            center.y,
            center.z,
            direction.x,
            direction.y,
            half_length * 2.0
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
        self.editor.slice_preview_detached = false;
        self.editor.slice_preview_navigation.reset();
        self.slice_preview_cursor_px = None;
        self.slice_preview_middle_down = false;
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.close_slice_preview();
            graphics.exit_slice_mode();
        }
        self.redraw_requested = true;
        userspace_log!("Exited slice view");
    }
}
