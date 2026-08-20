use crate::{app::App, userspace_log};

impl<'a> App<'a> {
    pub(crate) fn save_preferences(&self) -> anyhow::Result<()> {
        crate::app::io::save_config(&crate::app::io::Config {
            dark_mode: self.editor.dark_mode,
            show_console: self.editor.show_console,
            show_world_axis_gizmo: self.editor.show_world_axis_gizmo,
            show_xy_grid: self.editor.show_xy_grid,
            show_scale_bar: self.editor.show_scale_bar,
            renderer_background_color: self.editor.renderer_background_color,
            snap_poll_rate: self.editor.snap_poll_rate,
            frame_rate_cap: self.editor.frame_rate_cap,
            resize_frame_rate_cap: self.editor.resize_frame_rate_cap,
            block_model_interaction_resolution_divisor: self.editor.block_model_interaction_resolution_divisor,
            show_block_model_boundary_highlights: self.editor.show_block_model_boundary_highlights,
            downscale_raster_previews: self.editor.downscale_raster_previews,
            frame_counter_enabled: self.editor.frame_counter_enabled,
            debug_chunk_coloring: self.editor.debug_chunk_coloring,
            debug_clip_planes: self.editor.debug_clip_planes,
            plan_orbit_sensitivity: self.editor.plan_orbit_sensitivity,
            plan_zoom_sensitivity: self.editor.plan_zoom_sensitivity,
            plan_invert_vertical_look: self.editor.plan_invert_vertical_look,
            plan_invert_horizontal_look: self.editor.plan_invert_horizontal_look,
            plan_zoom_towards_cursor: self.editor.plan_zoom_towards_cursor,
            fly_field_of_view_degrees: self.editor.fly_field_of_view_degrees,
            fly_mouse_look_sensitivity: self.editor.fly_mouse_look_sensitivity,
            fly_invert_vertical_look: self.editor.fly_invert_vertical_look,
            fly_invert_horizontal_look: self.editor.fly_invert_horizontal_look,
            fly_near_clip_limit: self.editor.fly_near_clip_limit,
            fly_max_clip_span: self.editor.fly_max_clip_span,
        })?;
        Ok(())
    }

    pub(crate) fn set_topology_wireframes(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.editor.topology_wireframes_enabled = enabled;
        // Deliberately not persisted: this is a per-session view toggle.
        // The topology GPU cache detects the style change during the next
        // render; document geometry does not need rebuilding.
        self.redraw_requested = true;
        userspace_log!("Set topology wireframes = {}", enabled);
        Ok(())
    }

    pub(crate) fn set_show_points(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.editor.show_points = enabled;
        // Deliberately not persisted: this is a per-session view toggle.
        self.redraw_requested = true;
        userspace_log!("Set view points = {}", enabled);
        Ok(())
    }

    pub(crate) fn set_dark_mode(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.editor.dark_mode = enabled;
        if let Some(draft) = self.editor.preferences_draft.as_mut() {
            draft.dark_mode = enabled;
        }
        self.save_preferences()?;
        self.redraw_requested = true;
        userspace_log!("Set dark mode = {}", enabled);
        Ok(())
    }

    pub(crate) fn set_show_console(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.editor.show_console = enabled;
        if let Some(draft) = self.editor.preferences_draft.as_mut() {
            draft.show_console = enabled;
        }
        self.save_preferences()?;
        self.redraw_requested = true;
        userspace_log!("Set show console = {}", enabled);
        Ok(())
    }

    pub(crate) fn set_show_xy_grid(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.editor.show_xy_grid = enabled;
        if let Some(draft) = self.editor.preferences_draft.as_mut() {
            draft.show_xy_grid = enabled;
        }
        self.save_preferences()?;
        self.redraw_requested = true;
        userspace_log!("Set show XY grid = {}", enabled);
        Ok(())
    }

    pub(crate) fn set_show_scale_bar(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.editor.show_scale_bar = enabled;
        self.save_preferences()?;
        self.redraw_requested = true;
        userspace_log!("Set show scale bar = {}", enabled);
        Ok(())
    }

    pub(crate) fn apply_preferences(&mut self, mut preferences: crate::ui::state::PreferencesDraft) -> anyhow::Result<()> {
        // Clamp once, up front, so the saved config, the applied editor state
        // and the retained draft cannot diverge.
        preferences.snap_poll_rate = preferences.snap_poll_rate.clamp(5, 1000);
        preferences.frame_rate_cap = preferences.frame_rate_cap.clamp(20, 1000);
        preferences.resize_frame_rate_cap = preferences.resize_frame_rate_cap.clamp(20, 1000);
        preferences.block_model_interaction_resolution_divisor = preferences.block_model_interaction_resolution_divisor.clamp(1, 64);
        preferences.plan_orbit_sensitivity = crate::app::io::finite_clamped(preferences.plan_orbit_sensitivity, 0.0001, 0.02, crate::app::io::default_plan_orbit_sensitivity());
        preferences.plan_zoom_sensitivity = crate::app::io::finite_clamped(preferences.plan_zoom_sensitivity, 0.0001, 0.05, crate::app::io::default_plan_zoom_sensitivity());
        preferences.fly_field_of_view_degrees =
            crate::app::io::finite_clamped(preferences.fly_field_of_view_degrees, 20.0, 120.0, crate::app::io::default_fly_field_of_view_degrees());
        preferences.fly_mouse_look_sensitivity =
            crate::app::io::finite_clamped(preferences.fly_mouse_look_sensitivity, 0.0001, 0.02, crate::app::io::default_fly_mouse_look_sensitivity());
        preferences.fly_near_clip_limit = crate::app::io::finite_clamped(preferences.fly_near_clip_limit, 0.01, 100.0, crate::app::io::default_fly_near_clip_limit());
        preferences.fly_max_clip_span = crate::app::io::finite_clamped(preferences.fly_max_clip_span, 100.0, 1_000_000.0, crate::app::io::default_fly_max_clip_span());

        crate::app::io::save_config(&crate::app::io::Config {
            dark_mode: preferences.dark_mode,
            show_console: preferences.show_console,
            show_world_axis_gizmo: preferences.show_world_axis_gizmo,
            show_xy_grid: preferences.show_xy_grid,
            show_scale_bar: self.editor.show_scale_bar,
            renderer_background_color: preferences.renderer_background_color,
            snap_poll_rate: preferences.snap_poll_rate,
            frame_rate_cap: preferences.frame_rate_cap,
            resize_frame_rate_cap: preferences.resize_frame_rate_cap,
            block_model_interaction_resolution_divisor: preferences.block_model_interaction_resolution_divisor,
            show_block_model_boundary_highlights: preferences.show_block_model_boundary_highlights,
            downscale_raster_previews: preferences.downscale_raster_previews,
            frame_counter_enabled: preferences.frame_counter_enabled,
            debug_chunk_coloring: preferences.debug_chunk_coloring,
            debug_clip_planes: preferences.debug_clip_planes,
            plan_orbit_sensitivity: preferences.plan_orbit_sensitivity,
            plan_zoom_sensitivity: preferences.plan_zoom_sensitivity,
            plan_invert_vertical_look: preferences.plan_invert_vertical_look,
            plan_invert_horizontal_look: preferences.plan_invert_horizontal_look,
            plan_zoom_towards_cursor: preferences.plan_zoom_towards_cursor,
            fly_field_of_view_degrees: preferences.fly_field_of_view_degrees,
            fly_mouse_look_sensitivity: preferences.fly_mouse_look_sensitivity,
            fly_invert_vertical_look: preferences.fly_invert_vertical_look,
            fly_invert_horizontal_look: preferences.fly_invert_horizontal_look,
            fly_near_clip_limit: preferences.fly_near_clip_limit,
            fly_max_clip_span: preferences.fly_max_clip_span,
        })?;

        self.editor.dark_mode = preferences.dark_mode;
        self.editor.show_console = preferences.show_console;
        self.editor.show_world_axis_gizmo = preferences.show_world_axis_gizmo;
        self.editor.show_xy_grid = preferences.show_xy_grid;
        self.editor.renderer_background_color = preferences.renderer_background_color;
        self.editor.snap_poll_rate = preferences.snap_poll_rate;
        self.editor.frame_rate_cap = preferences.frame_rate_cap;
        self.editor.resize_frame_rate_cap = preferences.resize_frame_rate_cap;
        self.editor.block_model_interaction_resolution_divisor = preferences.block_model_interaction_resolution_divisor;
        self.editor.show_block_model_boundary_highlights = preferences.show_block_model_boundary_highlights;
        self.editor.downscale_raster_previews = preferences.downscale_raster_previews;
        self.editor.frame_counter_enabled = preferences.frame_counter_enabled;
        if !preferences.frame_counter_enabled {
            self.editor.measured_fps = None;
        }
        self.editor.debug_chunk_coloring = preferences.debug_chunk_coloring;
        if !preferences.debug_chunk_coloring {
            self.editor.debug_chunk_stats = None;
        }
        self.editor.debug_clip_planes = preferences.debug_clip_planes;
        self.editor.plan_orbit_sensitivity = preferences.plan_orbit_sensitivity;
        self.editor.plan_zoom_sensitivity = preferences.plan_zoom_sensitivity;
        self.editor.plan_invert_vertical_look = preferences.plan_invert_vertical_look;
        self.editor.plan_invert_horizontal_look = preferences.plan_invert_horizontal_look;
        self.editor.plan_zoom_towards_cursor = preferences.plan_zoom_towards_cursor;
        self.editor.fly_field_of_view_degrees = preferences.fly_field_of_view_degrees;
        self.editor.fly_mouse_look_sensitivity = preferences.fly_mouse_look_sensitivity;
        self.editor.fly_invert_vertical_look = preferences.fly_invert_vertical_look;
        self.editor.fly_invert_horizontal_look = preferences.fly_invert_horizontal_look;
        self.editor.fly_near_clip_limit = preferences.fly_near_clip_limit;
        self.editor.fly_max_clip_span = preferences.fly_max_clip_span;
        self.configure_graphics_camera_preferences();
        self.editor.preferences_draft = Some(preferences);
        userspace_log!(
            "Applied preferences (dark_mode={}, snap_rate={}, fps_cap={}, frame_counter={}, debug_chunks={})",
            preferences.dark_mode,
            preferences.snap_poll_rate,
            preferences.frame_rate_cap,
            preferences.frame_counter_enabled,
            preferences.debug_chunk_coloring
        );
        self.redraw_requested = true;
        Ok(())
    }

    pub(crate) fn configure_graphics_camera_preferences(&mut self) {
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.configure_camera_preferences(
                self.editor.plan_orbit_sensitivity,
                self.editor.plan_zoom_sensitivity,
                self.editor.plan_invert_vertical_look,
                self.editor.plan_invert_horizontal_look,
                self.editor.plan_zoom_towards_cursor,
                self.editor.fly_field_of_view_degrees,
                self.editor.fly_mouse_look_sensitivity,
                self.editor.fly_invert_vertical_look,
                self.editor.fly_invert_horizontal_look,
                self.editor.fly_near_clip_limit,
                self.editor.fly_max_clip_span,
            );
        }
    }

    /// Reset the camera to a plan view that fits all visible content.
    pub(crate) fn reset_view(&mut self) {
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.fit_to_extents(
                &self.scene_document,
                &self.triangulations,
                &self.block_models,
                &self.drill_holes,
                &self.point_clouds,
                &self.editor.hidden_handles,
            );
            self.redraw_requested = true;
        }
        userspace_log!("Reset view (fit to extents)");
    }

    /// Fit all visible content while preserving the current orbit angle.
    pub(crate) fn zoom_to_extents(&mut self) {
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.zoom_to_extents(
                &self.scene_document,
                &self.triangulations,
                &self.block_models,
                &self.drill_holes,
                &self.point_clouds,
                &self.editor.hidden_handles,
            );
            self.redraw_requested = true;
        }
        userspace_log!("Zoom to extents (preserving angle)");
    }
}
