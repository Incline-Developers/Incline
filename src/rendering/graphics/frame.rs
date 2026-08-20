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
) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    slice_preview::slice_preview_scene_key(editor, document, triangulations, block_models, drill_holes, point_clouds, rasters).hash(&mut hasher);
    for dataset in drill_holes {
        dataset.id.hash(&mut hasher);
        dataset.visible.hash(&mut hasher);
        dataset.color.active_field.hash(&mut hasher);
        dataset.color.smooth.hash(&mut hasher);
        for stop in &dataset.color.stops {
            stop.t.to_bits().hash(&mut hasher);
            for color in stop.color {
                color.to_bits().hash(&mut hasher);
            }
        }
        for category in &dataset.color.categories {
            category.value.hash(&mut hasher);
            for color in category.color {
                color.to_bits().hash(&mut hasher);
            }
        }
    }
    editor.z_level.to_bits().hash(&mut hasher);
    editor.show_xy_grid.hash(&mut hasher);
    editor.fly_mode_enabled.hash(&mut hasher);
    hasher.write(bytemuck::bytes_of(camera_uniform));
    hasher.finish()
}

fn scene_cache_needs_render(cache_exists: bool, cached_key: Option<u64>, next_key: u64, content_changed: bool, gpu_work_pending: bool) -> bool {
    !cache_exists || cached_key != Some(next_key) || content_changed || gpu_work_pending
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
        let mut scene_content_changed = self.geometry_dirty || self.overlay_dirty;
        self.vertical_exaggeration = editor.vertical_exaggeration.clamp(0.1, 20.0);
        let slice_visible_half_length = slice_visible_half_length(self.projection.zoom, self.screen_size());
        if let Some(slice) = self.slice_view.as_mut() {
            // Slice mode owns the clip planes: the symmetric depth extent *is*
            // the slab, so the scene-fitting passes below must not run — they
            // would blow the clip range back out to the scene bounds.
            slice.width = editor.slice_width_input.clamp(0.1, 1.0e6);
            slice.move_speed = editor.slice_speed_input.clamp(0.0, 1.0e6);
            slice.rotate_speed = editor.slice_rotate_input.clamp(1.0, 720.0).to_radians();
            self.projection.set_symmetric_depth_extent(slice.width * 0.5);
            editor.slice_center = [slice.center.x, slice.center.y, slice.center.z];
            editor.slice_direction = [slice.direction.x, slice.direction.y];
            editor.slice_half_length = slice_visible_half_length;
        } else {
            self.fit_depth_to_scene(document, triangulations, block_models, drill_holes, point_clouds, &editor.hidden_handles);
            self.include_pending_stroke_in_depth(editor);
            self.include_batter_berm_preview_in_depth(editor);
        }
        editor.debug_clip_plane_distances = Some(self.projection.clip_planes());
        self.upload_camera_uniform(editor.block_model_interaction_resolution_divisor);
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
        self.drill_hole_gpu.sync(&self.device, self.scene_origin, drill_holes);
        self.point_cloud_gpu.sync(
            &self.device,
            &self.queue,
            &mut encoder,
            self.scene_origin,
            scale_factor,
            glam::Mat4::from_cols_array_2d(&self.camera_uniform.view_proj),
            (self.camera.position - self.scene_origin).as_vec3(),
            point_clouds,
            &self.edge_style_bind_group_layout,
        );
        self.design_point_gpu.sync(
            &self.device,
            &self.queue,
            document,
            &editor.hidden_handles,
            self.scene_origin,
            scale_factor,
            editor.show_points || matches!(editor.active_tool, crate::ui::state::ActiveTool::DeleteElement | crate::ui::state::ActiveTool::Move),
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
            scene_content_changed = true;
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
            scene_content_changed = true;
        }
        if editor.poly_finish_dialog != self.cached_poly_finish_dialog {
            self.cached_poly_finish_dialog = editor.poly_finish_dialog;
            self.overlay_dirty = true;
            scene_content_changed = true;
        }

        if self.overlay_dirty {
            scene_content_changed = true;
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
        let scene_key = main_scene_cache_key(&self.camera_uniform, editor, document, triangulations, block_models, drill_holes, point_clouds, rasters);
        let gpu_work_pending = gpu_work_was_pending
            || self.point_cloud_gpu.has_pending_uploads()
            || self.block_model_gpu.has_pending_builds()
            || self.block_model_gpu.has_visible_pending_streaming(&primary_frustum, &editor.hidden_handles);
        let render_scene = scene_cache_needs_render(self.scene_cache.is_some(), self.scene_cache_key, scene_key, scene_content_changed, gpu_work_pending);
        let sample_volume_feedback = render_scene && self.frame_index.is_multiple_of(VOLUME_FEEDBACK_INTERVAL_FRAMES);
        if sample_volume_feedback {
            let phase = (self.frame_index / VOLUME_FEEDBACK_INTERVAL_FRAMES) % 64;
            self.block_model_gpu
                .clear_visible_volume_feedback(&self.queue, &mut encoder, phase as u32, &primary_frustum, &editor.hidden_handles);
        }

        if let Some(cache_view) = self.scene_cache.as_ref().map(|cache| cache.view.clone()) {
            if render_scene {
                self.render_scene_pass(&mut encoder, &cache_view, editor, triangulations, block_models, drill_holes, point_clouds, rasters, true);
                self.scene_cache_key = Some(scene_key);
            }
            let cache_texture = self.scene_cache.as_ref().expect("cache view came from a cache target").texture.clone();
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &cache_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &output.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: self.size.width,
                    height: self.size.height,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            self.render_scene_pass(&mut encoder, &view, editor, triangulations, block_models, drill_holes, point_clouds, rasters, true);
        }

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

        self.update_tool_projections(editor, document);

        let orbit_marker_screen = self.orbit_marker_screen_pos();
        let camera_forward = self.camera.forward();
        let camera_up = self.camera.up();
        let world_per_physical_pixel = (!self.projection.is_perspective()).then(|| 2.0 * self.projection.zoom / f64::from(self.size.height.max(1)));
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
            [camera_forward.x as f32, camera_forward.y as f32, camera_forward.z as f32],
            [camera_up.x as f32, camera_up.y as f32, camera_up.z as f32],
            world_per_physical_pixel,
        );
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
        output.present();
        if let Some(capture) = pending_screenshot {
            self.finish_screenshot_capture(capture);
        }
        self.frame_index = self.frame_index.wrapping_add(1);

        Ok(ui_output)
    }
}
