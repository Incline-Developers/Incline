use super::*;
use crate::rendering::scene::{
    build::{DocumentSceneBuildInput, DynamicSceneBuildInput, rebuild_document_scene, rebuild_dynamic_scene},
    overlays::{OverlaySceneBuildInput, rebuild_editor_overlay},
};

const VOLUME_FEEDBACK_INTERVAL_FRAMES: u64 = 8;

#[allow(clippy::too_many_arguments)]
fn main_scene_cache_key(
    camera_uniform: &CameraUniform,
    editor: &EditorState,
    document: &Document,
    triangulations: &[OpenTriangulation],
    block_models: &[OpenBlockModel],
    drill_holes: &[OpenDrillHoleDataset],
    point_clouds: &[OpenPointCloud],
    rasters: &[OpenRasterTexture],
    drill_hole_content_key: u64,
) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    slice_preview::slice_preview_scene_key(editor, document, triangulations, block_models, drill_holes, point_clouds, rasters).hash(&mut hasher);
    // Every other item kind reaches the key above by the identity of the data
    // it is holding, but a drill hole dataset is edited in place - laying a
    // tie, turning a collar - so nothing about it here would change. Its
    // instance cache has already been resynced by the time this runs, and it
    // reports what it is holding: see `DrillHoleGpuCache::content_key`.
    drill_hole_content_key.hash(&mut hasher);
    EditorSceneState::of(editor).hash(&mut hasher);
    hasher.write(bytemuck::bytes_of(camera_uniform));
    hasher.finish()
}

/// The editor state a cached scene image is only valid for.
///
/// Most editor state reaches the renderer by being pushed: hiding an object,
/// changing a selection and the other ~90 callers of `App::invalidate_geometry`
/// all mark the scene dirty, and the cache is rebuilt on the next frame. What
/// cannot work that way is state [`Graphics::render_scene_pass`] reads for
/// itself at draw time - the renderer decides from `editor` how to draw, so
/// nothing upstream knows the scene changed. **Every such read belongs in this
/// struct**, which is the whole of the scene pass's editor dependency.
///
/// Arming Tie Holes is the case that proved it: it lifts drill traces above the
/// topology and draws the surface connectors purely by being armed, and while
/// it was missing here the switch showed nothing until the camera next moved.
#[derive(Hash)]
struct EditorSceneState {
    /// Bit pattern: the level the design plane and z-cut draw at.
    z_level: u64,
    show_xy_grid: bool,
    fly_mode_enabled: bool,
    /// Tie Holes draws drill traces without the depth test (`draw_drill_holes`).
    tying_holes: bool,
    /// Drill & Blast alone draws the surface tie-in connectors.
    shows_tie_ins: bool,
}

impl EditorSceneState {
    fn of(editor: &EditorState) -> Self {
        Self {
            z_level: editor.z_level.to_bits(),
            show_xy_grid: editor.show_xy_grid,
            fly_mode_enabled: editor.fly_mode_enabled,
            tying_holes: editor.tying_holes(),
            shows_tie_ins: editor.shows_tie_ins(),
        }
    }
}

fn scene_cache_needs_render(cached_key: Option<u64>, next_key: u64, content_changed: bool, gpu_work_pending: bool) -> bool {
    cached_key != Some(next_key) || content_changed || gpu_work_pending
}

pub(crate) struct RenderInput<'frame> {
    pub(crate) editor: &'frame mut EditorState,
    pub(crate) document: &'frame mut Document,
    pub(crate) triangulations: &'frame [OpenTriangulation],
    pub(crate) block_models: &'frame [OpenBlockModel],
    pub(crate) drill_holes: &'frame [OpenDrillHoleDataset],
    pub(crate) point_clouds: &'frame [OpenPointCloud],
    pub(crate) rasters: &'frame [OpenRasterTexture],
    pub(crate) project: &'frame UiProjectView,
}

impl<'a> Graphics<'a> {
    pub(crate) fn render(&mut self, input: RenderInput<'_>) -> Result<UiFrameOutput, RenderSurfaceError> {
        let RenderInput {
            editor,
            document,
            triangulations,
            block_models,
            drill_holes,
            point_clouds,
            rasters,
            project,
        } = input;
        // Only scene content forces the cached scene to be re-rendered. The
        // editor overlay is drawn over the cache every frame by
        // `render_editor_overlay_pass`, so `overlay_dirty` deliberately does
        // not appear here - that is what keeps a cursor-following tool preview
        // off the critical path of a full scene render.
        let mut scene_content_changed = self.geometry_dirty;
        self.vertical_exaggeration = editor.vertical_exaggeration.clamp(0.1, 20.0);
        let slice_visible_half_length = slice_visible_half_length(self.projection.zoom, self.screen_size());
        if self.slice_view.is_some() {
            self.refresh_scene_bounds(document, triangulations, block_models, drill_holes, point_clouds, &editor.hidden_handles);
        }
        if let Some(slice) = self.slice_view.as_mut() {
            // Slice mode sets the clip planes itself; the scene-fitting passes below must not run.
            slice.width = editor.slice_width_input.clamp(0.1, 1.0e6);
            slice.move_speed = editor.slice_speed_input.clamp(0.0, 1.0e6);
            slice.rotate_speed = editor.slice_rotate_input.clamp(1.0, 720.0).to_radians();
            // Camera's own depth range, not the slab (fragment shaders clip to that directly) - kept wide enough a tilted or overhead view won't clip away geometry the slab would show.
            let forward = self.camera.forward();
            let strike = DVec3::new(slice.direction.x, slice.direction.y, 0.0);
            // Scene bounds are model elevations, the slice centre a display one - stretch bounds by the vertical exaggeration before comparing.
            let origin_z = self.scene_origin.z;
            let exaggeration = self.vertical_exaggeration;
            let display_z = |z: f64| origin_z + (z - origin_z) * exaggeration;
            let scene_vertical_half_spread = self
                .cached_scene_bounds
                .map(|(min, max)| (display_z(min.z) - slice.center.z).abs().max((display_z(max.z) - slice.center.z).abs()));
            let vertical_extent = scene_vertical_half_spread.map_or(self.projection.zoom, |spread| self.projection.zoom.max(spread));
            let tilt_depth = slice_visible_half_length * strike.dot(forward).abs() + vertical_extent * forward.z.abs();
            self.projection.set_symmetric_depth_extent(slice.width * 0.5 + tilt_depth);
            editor.slice_center = [slice.center.x, slice.center.y, slice.center.z];
            editor.slice_direction = [slice.direction.x, slice.direction.y];
            editor.slice_half_length = slice_visible_half_length;
        } else {
            self.fit_depth_to_scene(document, triangulations, block_models, drill_holes, point_clouds, &editor.hidden_handles);
            self.include_tool_previews_in_depth(editor);
            self.include_batter_berm_preview_in_depth(editor);
        }
        editor.debug_clip_plane_distances = Some(self.projection.clip_planes());
        // Uploaded every frame; outside a section this is `None`, which is what switches the shader clip off.
        self.upload_camera_uniform(editor.block_model_interaction_resolution_divisor, self.section_slab());
        let grid_uniform = GridUniform::new(
            self.scene_origin,
            editor.renderer_background_color,
            &self.camera,
            &self.projection,
            self.vertical_exaggeration,
            self.fly_mode_enabled,
        );
        self.queue.write_buffer(&self.grid_buffer, 0, bytemuck::bytes_of(&grid_uniform));
        // Advance non-blocking volume-usage readbacks. Their callbacks only
        // send a small bitset through a channel; residency changes are applied
        // later by the normal streaming pass.
        if self.block_model_gpu.has_pending_feedback() {
            let _ = self.device.poll(wgpu::PollType::Poll);
            self.block_model_gpu.poll_volume_feedback();
        }
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output) | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Timeout => return Err(RenderSurfaceError::Timeout),
            wgpu::CurrentSurfaceTexture::Occluded => return Err(RenderSurfaceError::Occluded),
            wgpu::CurrentSurfaceTexture::Outdated => return Err(RenderSurfaceError::Outdated),
            wgpu::CurrentSurfaceTexture::Lost => return Err(RenderSurfaceError::Lost),
            wgpu::CurrentSurfaceTexture::Validation => return Err(RenderSurfaceError::Validation),
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format.add_srgb_suffix()),
            ..Default::default()
        });
        // Non-sRGB view of the same texture for the egui pass; egui applies
        // gamma itself, so it wants raw byte writes (see `Gui::new`).
        let gui_view = output.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format.remove_srgb_suffix()),
            ..Default::default()
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("Render Encoder") });

        let scale_factor = self.window.scale_factor() as f32;
        let gpu_work_was_pending = self.point_cloud_gpu.has_pending_uploads() || self.block_model_gpu.has_pending_builds();
        self.raster_gpu
            .sync(&self.device, &self.queue, self.scene_origin, rasters, &self.raster_surface_bind_group_layout);
        self.triangulation_gpu.sync(
            &self.device,
            &self.queue,
            self.scene_origin,
            scale_factor,
            triangulations,
            rasters,
            editor,
            &self.surface_style_bind_group_layout,
            &self.surface_chunk_bind_group_layout,
            &self.edge_style_bind_group_layout,
        );
        self.block_model_gpu.sync(
            &self.device,
            &self.queue,
            self.scene_origin,
            scale_factor,
            block_models,
            editor,
            &self.surface_style_bind_group_layout,
            &self.block_model_volume_bind_group_layout,
            &self.edge_style_bind_group_layout,
        );
        self.drill_hole_gpu.sync(&self.device, self.scene_origin, drill_holes, editor);
        self.point_cloud_gpu.sync(
            &self.device,
            &self.queue,
            &mut encoder,
            self.scene_origin,
            scale_factor,
            glam::Mat4::from_cols_array_2d(&self.camera_uniform.view_proj),
            (self.camera.position - self.scene_origin).as_vec3(),
            point_clouds,
            editor,
            &self.edge_style_bind_group_layout,
        );
        self.design_point_gpu.sync(
            &self.device,
            &self.queue,
            document,
            &editor.hidden_handles,
            self.scene_origin,
            scale_factor,
            editor.show_points || editor.active_tool == crate::ui::state::ActiveTool::DeletePoints,
            self.geometry_dirty || self.cached_document_revision != document.revision(),
        );
        let render_style_key = editor.render_style_key();
        if self.cached_render_style_key != Some(render_style_key) {
            self.cached_render_style_key = Some(render_style_key);
            self.geometry_dirty = true;
            self.overlay_dirty = true;
        }
        let needs_geometry_rebuild = self.geometry_dirty || self.cached_document_revision != document.revision() || (self.cached_scale_factor - scale_factor).abs() > f32::EPSILON;

        if needs_geometry_rebuild {
            scene_content_changed = true;
            // Reconcile the static stroke chunks first so the stream rebuild
            // below knows which objects they own and can skip them.
            self.static_strokes.sync(&self.device, &self.queue, document, editor, self.scene_origin, scale_factor);
            rebuild_document_scene(DocumentSceneBuildInput {
                editor,
                document,
                static_ids: self.static_strokes.claimed(),
                fill_cache: &mut self.polyline_fill_cache,
                text_system: &mut self.text_system,
                lyon_buffer: &mut self.lyon_buffer,
                stroke_vertex_buf: &mut self.stroke_vertex_buf,
                stroke_index_buf: &mut self.stroke_index_buf,
                text_vertex_buf: &mut self.text_vertex_buf,
                text_index_buf: &mut self.text_index_buf,
                text_draw_batches: &mut self.text_draw_batches,
                pick_records: &mut self.pick_records,
                text_pick_records: &mut self.text_pick_records,
                document_draw_batches: &mut self.document_draw_batches,
                scene_origin: self.scene_origin,
                scale_factor,
            });

            self.upload_scene_stream_buffers();

            self.cached_scale_factor = scale_factor;
            self.cached_document_revision = document.revision();
            self.geometry_dirty = false;
        }

        // Per-frame pass for the live drawing tools; the static scene above no
        // longer rebuilds while they run. Rebuilt while a tool is active and
        // once more after it deactivates (to clear the buffers).
        let dynamic_active = editor.batter_berm_dialog_open;
        if dynamic_active || !self.dynamic_vertex_buf.is_empty() {
            rebuild_dynamic_scene(DynamicSceneBuildInput {
                editor,
                dynamic_vertex_buf: &mut self.dynamic_vertex_buf,
                dynamic_index_buf: &mut self.dynamic_index_buf,
                scene_origin: self.scene_origin,
                scale_factor,
            });
            Self::clamp_stream_geometry(&self.device, &mut self.dynamic_vertex_buf, &mut self.dynamic_index_buf, "Dynamic Scene Buffer");
            if !self.dynamic_vertex_buf.is_empty() {
                Self::ensure_stream_capacity(
                    &self.device,
                    &mut self.dynamic_vertex_gpu,
                    &mut self.dynamic_vertex_capacity,
                    self.dynamic_vertex_buf.len(),
                    size_of::<StrokeVertex>(),
                    wgpu::BufferUsages::VERTEX,
                    "Dynamic Scene Vertex Buffer",
                );
                self.queue.write_buffer(&self.dynamic_vertex_gpu, 0, bytemuck::cast_slice(&self.dynamic_vertex_buf));
            }
            if !self.dynamic_index_buf.is_empty() {
                Self::ensure_stream_capacity(
                    &self.device,
                    &mut self.dynamic_index_gpu,
                    &mut self.dynamic_index_capacity,
                    self.dynamic_index_buf.len(),
                    size_of::<u32>(),
                    wgpu::BufferUsages::INDEX,
                    "Dynamic Scene Index Buffer",
                );
                self.queue.write_buffer(&self.dynamic_index_gpu, 0, bytemuck::cast_slice(&self.dynamic_index_buf));
            }
        }

        let measurement_state = (
            matches!(
                editor.active_tool,
                crate::ui::state::ActiveTool::MeasureDistance | crate::ui::state::ActiveTool::MeasureBatterAngle
            ),
            editor.measurement_start,
            editor.measurement_end,
            editor.batter_angle_points.clone(),
        );
        if measurement_state != self.cached_measurement_state {
            self.cached_measurement_state = measurement_state;
            self.overlay_dirty = true;
        }
        if editor.poly_finish_dialog != self.cached_poly_finish_dialog {
            self.cached_poly_finish_dialog = editor.poly_finish_dialog;
            self.overlay_dirty = true;
        }

        if self.overlay_dirty {
            let overlay_vp = self.view_proj();
            let overlay_screen = self.screen_size();
            rebuild_editor_overlay(OverlaySceneBuildInput {
                editor,
                document,
                overlay_vertex_buf: &mut self.overlay_vertex_buf,
                overlay_index_buf: &mut self.overlay_index_buf,
                view_proj: overlay_vp,
                screen_size: overlay_screen,
                scene_origin: self.scene_origin,
                scale_factor,
            });

            Self::clamp_stream_geometry(&self.device, &mut self.overlay_vertex_buf, &mut self.overlay_index_buf, "Editor Overlay Buffer");
            if !self.overlay_vertex_buf.is_empty() {
                Self::ensure_stream_capacity(
                    &self.device,
                    &mut self.overlay_vertex_gpu,
                    &mut self.overlay_vertex_capacity,
                    self.overlay_vertex_buf.len(),
                    size_of::<StrokeVertex>(),
                    wgpu::BufferUsages::VERTEX,
                    "Editor Overlay Vertex Buffer",
                );
                self.queue.write_buffer(&self.overlay_vertex_gpu, 0, bytemuck::cast_slice(&self.overlay_vertex_buf));
            }
            if !self.overlay_index_buf.is_empty() {
                Self::ensure_stream_capacity(
                    &self.device,
                    &mut self.overlay_index_gpu,
                    &mut self.overlay_index_capacity,
                    self.overlay_index_buf.len(),
                    size_of::<u32>(),
                    wgpu::BufferUsages::INDEX,
                    "Editor Overlay Index Buffer",
                );
                self.queue.write_buffer(&self.overlay_index_gpu, 0, bytemuck::cast_slice(&self.overlay_index_buf));
            }
            self.overlay_dirty = false;
        }

        if needs_geometry_rebuild && self.frame_index.wrapping_sub(self.last_text_cache_trim_frame) >= TEXT_CACHE_TRIM_INTERVAL_FRAMES {
            self.text_system.text_cache.trim();
            self.last_text_cache_trim_frame = self.frame_index;
        }

        let primary_frustum = frustum::Frustum::from_view_proj(glam::Mat4::from_cols_array_2d(&self.camera_uniform.view_proj));
        let scene_key = main_scene_cache_key(
            &self.camera_uniform,
            editor,
            document,
            triangulations,
            block_models,
            drill_holes,
            point_clouds,
            rasters,
            self.drill_hole_gpu.content_key(),
        );
        let gpu_work_pending = gpu_work_was_pending
            || self.point_cloud_gpu.has_pending_uploads()
            || self.block_model_gpu.has_pending_builds()
            || self.block_model_gpu.has_visible_pending_streaming(&primary_frustum, &editor.hidden_handles);
        let render_scene = scene_cache_needs_render(self.scene_cache_key, scene_key, scene_content_changed, gpu_work_pending);
        let sample_volume_feedback = render_scene && self.frame_index.is_multiple_of(VOLUME_FEEDBACK_INTERVAL_FRAMES);
        if sample_volume_feedback {
            let phase = (self.frame_index / VOLUME_FEEDBACK_INTERVAL_FRAMES) % 64;
            self.block_model_gpu
                .clear_visible_volume_feedback(&self.queue, &mut encoder, phase as u32, &primary_frustum, &editor.hidden_handles);
        }

        // The scene renders into its own cache texture and is reused for as
        // long as its key holds; the overlay pass then puts this frame's
        // editor content over it and resolves the result to the surface. On a
        // cache hit that is the whole of the scene's cost.
        if render_scene {
            let cache_view = self.scene_cache.view.clone();
            self.render_scene_pass(
                &mut encoder,
                &cache_view,
                self.viewport_rect,
                editor,
                triangulations,
                block_models,
                drill_holes,
                point_clouds,
                rasters,
                true,
            );
            self.scene_cache_key = Some(scene_key);
        }
        self.render_editor_overlay_pass(&mut encoder, &view, self.viewport_rect, editor, !render_scene);

        // One-shot viewport export: re-render the scene (without the egui
        // chrome) into an offscreen texture and queue a readback on this
        // frame's encoder; the PNG is written after submit below.
        let pending_screenshot = self
            .pending_screenshot
            .take()
            .map(|path| self.encode_screenshot_capture(&mut encoder, editor, triangulations, block_models, drill_holes, point_clouds, rasters, path));

        // Render the in-viewport plan preview through the same shaded scene
        // pass as the main and detached viewports. The egui panel samples this
        // offscreen texture below.
        self.render_embedded_slice_preview(document, triangulations, block_models, drill_holes, point_clouds, rasters, editor);

        // Keep the engineering-drawing dialog's map preview current.
        self.refresh_plot_preview(editor, document, triangulations, block_models, drill_holes, point_clouds, rasters);

        self.update_tool_projections(editor, document, drill_holes);

        let orbit_marker_screen = self.orbit_marker_screen_pos();
        let camera_active = self.is_camera_active();
        let camera_forward = self.camera.forward();
        let camera_up = self.camera.up();
        let world_per_physical_pixel = (!self.projection.is_perspective()).then(|| 2.0 * self.projection.zoom / f64::from(self.viewport_rect.height.max(1)));
        let ui_output = self.gui.render(
            &self.window,
            &self.device,
            &self.queue,
            &mut encoder,
            &gui_view,
            editor,
            document,
            project,
            block_models,
            drill_holes,
            [self.size.width, self.size.height],
            orbit_marker_screen,
            camera_active,
            [camera_forward.x as f32, camera_forward.y as f32, camera_forward.z as f32],
            [camera_up.x as f32, camera_up.y as f32, camera_up.z as f32],
            world_per_physical_pixel,
        );
        // Apply this frame's egui layout for next frame's scene pass - the
        // scene renders before egui lays out, so it's always one frame behind
        // (see `apply_canvas_rect`).
        self.apply_canvas_rect(ui_output.canvas_rect);
        // Keep the startup view framed on the window the splash is centred
        // on, until the splash goes - see `track_startup_view_framing`.
        self.track_startup_view_framing(project.needs_startup_dialog);
        if ui_output.geometry_dirty {
            self.invalidate_geometry();
        }
        // Keep redrawing through the interaction cooldown so the volume
        // raycaster's reduced-quality frames are always followed by a
        // full-quality one once the camera settles / resizing stops.
        if self.interaction_active() {
            self.window.request_redraw();
        }
        // Continue draining the bounded point-cloud upload queue without
        // treating background upload progress as camera interaction.
        if self.point_cloud_gpu.has_pending_uploads()
            || self.block_model_gpu.has_pending_builds()
            || self.block_model_gpu.has_pending_feedback()
            || self.block_model_gpu.has_visible_pending_streaming(&primary_frustum, &editor.hidden_handles)
        {
            self.window.request_redraw();
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        if sample_volume_feedback
            && self
                .block_model_gpu
                .schedule_visible_volume_feedback(&self.device, &self.queue, &primary_frustum, &editor.hidden_handles)
        {
            self.window.request_redraw();
        }
        self.queue.present(output);
        if let Some(capture) = pending_screenshot {
            self.finish_screenshot_capture(capture);
        }
        self.frame_index = self.frame_index.wrapping_add(1);

        Ok(ui_output)
    }
}
