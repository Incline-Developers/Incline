//! WGPU render pass helpers.

use super::{frustum::Frustum, *};
use crate::model::point_cloud::POINT_CLOUD_LOD_LEVELS;

/// Cheap conservative whole-model reject before walking any cube chunks.
/// Bounds are stored in world space while the render frustum uses the
/// scene-origin-relative coordinates uploaded to the GPU.
fn block_model_intersects_frustum(block_model: &OpenBlockModel, scene_origin: DVec3, frustum: &Frustum) -> bool {
    block_model
        .visible_world_bounds()
        .is_none_or(|(min, max)| frustum.intersects_aabb((min - scene_origin).as_vec3(), (max - scene_origin).as_vec3()))
}

/// Conservative pixel bounding rectangle `(x, y, w, h)` of a scene-space AABB
/// within the `scaled_width × scaled_height` render sub-rect. Used to scissor
/// the fullscreen volume raycast down to the pixels a model can actually
/// cover, so small-on-screen models stop paying per-pixel ray setup across
/// the whole frame. `None` means the projection cannot bound the box (a
/// corner reaches the eye plane) and the caller must scissor to the full
/// sub-rect; a zero-area rect means the box projects entirely off-screen.
fn aabb_scissor_rect(view_proj: &glam::Mat4, bounds_min: glam::Vec3, bounds_max: glam::Vec3, scaled_width: f32, scaled_height: f32) -> Option<(u32, u32, u32, u32)> {
    let (ndc_min, ndc_max) = projected_aabb_ndc_bounds(view_proj, bounds_min, bounds_max)?;
    // NDC → pixels (y flips), one pixel of padding against raster rounding.
    let x0 = ((ndc_min.x.max(-1.0) + 1.0) * 0.5 * scaled_width - 1.0).floor().max(0.0) as u32;
    let x1 = ((ndc_max.x.min(1.0) + 1.0) * 0.5 * scaled_width + 1.0).ceil().clamp(0.0, scaled_width.ceil()) as u32;
    let y0 = ((1.0 - ndc_max.y.min(1.0)) * 0.5 * scaled_height - 1.0).floor().max(0.0) as u32;
    let y1 = ((1.0 - ndc_min.y.max(-1.0)) * 0.5 * scaled_height + 1.0).ceil().clamp(0.0, scaled_height.ceil()) as u32;
    Some((x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)))
}

fn projected_aabb_ndc_bounds(view_proj: &glam::Mat4, bounds_min: glam::Vec3, bounds_max: glam::Vec3) -> Option<(glam::Vec2, glam::Vec2)> {
    let mut ndc_min = glam::Vec2::splat(f32::MAX);
    let mut ndc_max = glam::Vec2::splat(f32::MIN);
    for corner in 0..8u32 {
        let p = glam::vec3(
            if corner & 1 == 0 { bounds_min.x } else { bounds_max.x },
            if corner & 2 == 0 { bounds_min.y } else { bounds_max.y },
            if corner & 4 == 0 { bounds_min.z } else { bounds_max.z },
        );
        let clip = *view_proj * p.extend(1.0);
        if clip.w <= 1.0e-6 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        ndc_min = ndc_min.min(glam::vec2(ndc.x, ndc.y));
        ndc_max = ndc_max.max(glam::vec2(ndc.x, ndc.y));
    }
    Some((ndc_min, ndc_max))
}

/// Full projected size of an AABB, deliberately not clipped to the viewport.
/// A prefix represents the whole chunk, including its currently off-screen
/// portion, so clipping this area would reduce the visible sample density as
/// the camera zooms through the chunk's bounds.
fn projected_aabb_pixel_size(view_proj: &glam::Mat4, bounds_min: glam::Vec3, bounds_max: glam::Vec3, viewport: glam::Vec2) -> Option<glam::Vec2> {
    let (ndc_min, ndc_max) = projected_aabb_ndc_bounds(view_proj, bounds_min, bounds_max)?;
    Some(((ndc_max - ndc_min).max(glam::Vec2::ZERO) * viewport * 0.5).max(glam::Vec2::ONE))
}

fn point_lod_projected_size(scissor: (u32, u32, u32, u32), full_projected_size: glam::Vec2, viewport: glam::Vec2) -> glam::Vec2 {
    let (x, y, width, height) = scissor;
    // Preserve clipped-area LOD for ordinary edge chunks. Once a chunk spans
    // an entire viewport axis, however, that clipped dimension stops growing
    // while its world-space splats do. Use the full dimension on saturated
    // axes so further zoom cannot reduce density.
    let width = if x == 0 && width >= viewport.x.ceil() as u32 {
        full_projected_size.x
    } else {
        width as f32
    };
    let height = if y == 0 && height >= viewport.y.ceil() as u32 {
        full_projected_size.y
    } else {
        height as f32
    };
    glam::vec2(width, height).max(glam::Vec2::ONE)
}

/// Choose a point LOD with a 20% dead band around each boundary. The dead
/// band prevents tiny camera movements or projection rounding from toggling a
/// chunk between two visibly different densities every frame.
fn point_lod_with_hysteresis(counts: [u32; POINT_CLOUD_LOD_LEVELS], desired: usize, current: usize) -> usize {
    let mut level = current.min(counts.len() - 1);
    while level + 1 < counts.len() && desired.saturating_mul(5) < counts[level + 1] as usize * 4 {
        level += 1;
    }
    while level > 0 && desired.saturating_mul(4) > counts[level] as usize * 5 {
        level -= 1;
    }
    level
}

fn desired_point_splats(projected_width: f32, projected_height: f32, point_size: f32) -> usize {
    let pixel_area = f64::from(projected_width.max(0.0)) * f64::from(projected_height.max(0.0));
    let splat_area = f64::from(point_size.max(1.0)).powi(2);
    let desired = (pixel_area / splat_area).ceil();
    if !desired.is_finite() || desired >= usize::MAX as f64 {
        usize::MAX
    } else {
        (desired as usize).max(1)
    }
}

/// Prefix count needed so the chunk's points, drawn as world-sized splats,
/// touch on screen and leave no gaps. `base_spacing` is the measured
/// full-resolution point spacing (cloud units); halving the point count widens
/// spacing by √2, so the count that keeps projected spacing within one splat is
/// `full_count · (projected_spacing / projected_splat)²`. `splat_pixels` is the
/// unclamped projected splat size, giving the world-to-pixel scale shared by
/// both spacing and splat, so the estimate is distance-invariant while splats
/// stay above a pixel and falls off correctly once they shrink below one.
fn coverage_desired_splats(base_spacing: f32, world_splat_size: f32, splat_pixels: f32, full_count: usize) -> usize {
    if base_spacing <= 0.0 || world_splat_size <= 0.0 || full_count == 0 {
        return full_count.max(1);
    }
    let scale = f64::from(splat_pixels) / f64::from(world_splat_size);
    let projected_spacing = f64::from(base_spacing) * scale;
    let projected_splat = f64::from(splat_pixels).max(1.0);
    let ratio = projected_spacing / projected_splat;
    let desired = (full_count as f64 * ratio * ratio).ceil();
    if !desired.is_finite() { full_count } else { (desired as usize).clamp(1, full_count) }
}

fn smoothed_point_count(current: u32, target: u32, elapsed_seconds: f64) -> u32 {
    if current == target || elapsed_seconds <= 0.0 {
        return current;
    }
    // Refinement is deliberately gentler than coarsening: added splats change
    // brightness, while shedding excess work should respond promptly to a
    // zoom-out. Exponential movement is frame-rate independent.
    let time_constant = if target > current { 0.18 } else { 0.08 };
    let alpha = 1.0 - (-elapsed_seconds.min(0.1) / time_constant).exp();
    let delta = current.abs_diff(target);
    let step = ((f64::from(delta) * alpha).ceil() as u32).max(1);
    if target > current {
        current.saturating_add(step).min(target)
    } else {
        current.saturating_sub(step).max(target)
    }
}

fn point_billboard_axes(view_proj: glam::Mat4) -> (glam::Vec3, glam::Vec3) {
    let inverse = view_proj.inverse();
    (
        inverse.x_axis.truncate().try_normalize().unwrap_or(glam::Vec3::X),
        inverse.y_axis.truncate().try_normalize().unwrap_or(glam::Vec3::Y),
    )
}

fn projected_world_splat_size(view_proj: glam::Mat4, position: glam::Vec3, world_size: f32, billboard_axes: (glam::Vec3, glam::Vec3), viewport: glam::Vec2) -> f32 {
    let project = |point: glam::Vec3| {
        let clip = view_proj * point.extend(1.0);
        (clip.w > 1.0e-6).then(|| clip.truncate() / clip.w)
    };
    let Some(center) = project(position) else {
        return 1.0;
    };
    let Some(right) = project(position + billboard_axes.0 * world_size) else {
        return 1.0;
    };
    let Some(up) = project(position + billboard_axes.1 * world_size) else {
        return 1.0;
    };
    let right_pixels = ((right.truncate() - center.truncate()) * viewport * 0.5).length();
    let up_pixels = ((up.truncate() - center.truncate()) * viewport * 0.5).length();
    // Left unclamped: callers that want a floor apply it themselves, and the LOD
    // coverage estimate needs the true sub-pixel scale to fall off with distance.
    (right_pixels * up_pixels).sqrt()
}

fn clamped_document_batch_range(range: (u32, u32), available_indices: usize) -> Option<std::ops::Range<u32>> {
    let start = (range.0 as usize).min(available_indices);
    let end = (range.1 as usize).min(available_indices);
    let end = start + (end.saturating_sub(start) / 3) * 3;
    (start < end).then_some(start as u32..end as u32)
}

fn document_primitive_order(primitive: DocumentPrimitive) -> u8 {
    match primitive {
        DocumentPrimitive::Fill => 0,
        DocumentPrimitive::Stroke => 1,
    }
}

impl<'a> Graphics<'a> {
    fn draw_drill_holes<'pass>(
        &'pass self,
        render_pass: &mut wgpu::RenderPass<'pass>,
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &std::collections::HashSet<crate::model::SceneEntityId>,
        xray_enabled: bool,
        draw_traces: bool,
        draw_surface_marks: bool,
    ) {
        if self.drill_hole_gpu.is_empty() {
            return;
        }
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
        if draw_traces {
            render_pass.set_pipeline(if xray_enabled {
                &self.xray_drill_hole_render_pipeline
            } else {
                &self.drill_hole_render_pipeline
            });
            for dataset in drill_holes {
                if !dataset.state.loaded || !dataset.visible || hidden.contains(&dataset.entity_id()) {
                    continue;
                }
                let Some(cached) = self.drill_hole_gpu.get(dataset.id) else {
                    continue;
                };
                let Some(buffer) = cached.buffer.as_ref() else {
                    continue;
                };
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..144, 0..cached.count);
            }
        }
        if !draw_surface_marks {
            return;
        }
        // Surface tie-ins share the cylindrical segment shader with traces,
        // but have their own buffer so Drill & Blast can lift only them above
        // triangulations while leaving the down-hole traces depth tested.
        render_pass.set_pipeline(if xray_enabled {
            &self.xray_drill_hole_render_pipeline
        } else {
            &self.drill_hole_render_pipeline
        });
        for dataset in drill_holes {
            if !dataset.state.loaded || !dataset.visible || hidden.contains(&dataset.entity_id()) {
                continue;
            }
            let Some(cached) = self.drill_hole_gpu.get(dataset.id) else {
                continue;
            };
            let Some(buffer) = cached.tie_buffer.as_ref() else {
                continue;
            };
            render_pass.set_vertex_buffer(0, buffer.slice(..));
            render_pass.draw(0..144, 0..cached.tie_count);
        }
        // Collar markers go over the traces they cap, in their own pass so the
        // pipeline switch happens once rather than per dataset.
        render_pass.set_pipeline(if xray_enabled {
            &self.xray_drill_collar_render_pipeline
        } else {
            &self.drill_collar_render_pipeline
        });
        for dataset in drill_holes {
            if !dataset.state.loaded || !dataset.visible || hidden.contains(&dataset.entity_id()) {
                continue;
            }
            let Some(cached) = self.drill_hole_gpu.get(dataset.id) else {
                continue;
            };
            let Some(buffer) = cached.collar_buffer.as_ref() else {
                continue;
            };
            render_pass.set_vertex_buffer(0, buffer.slice(..));
            render_pass.draw(0..6, 0..cached.collar_count);
        }
    }

    fn draw_document_batches<'pass>(
        &'pass self,
        render_pass: &mut wgpu::RenderPass<'pass>,
        stage: DocumentRenderStage,
        xray_enabled: bool,
        primitive_filter: Option<DocumentPrimitive>,
    ) {
        let mut batches = self
            .document_draw_batches
            .iter()
            .filter(|batch| {
                batch.stage(xray_enabled) == stage
                    // Intrinsic editor overlays are deferred to the final
                    // draw even when global x-ray moves everything else here.
                    && (!xray_enabled
                        || batch.stage(false) != DocumentRenderStage::AlwaysVisible)
                    && primitive_filter.is_none_or(|primitive| batch.primitive == primitive)
            })
            .collect::<Vec<_>>();
        if stage == DocumentRenderStage::Translucent {
            let forward = self.camera.forward();
            let position = self.camera.position;
            batches.sort_by(|a, b| {
                let a_depth = (a.center - position).dot(forward);
                let b_depth = (b.center - position).dot(forward);
                b_depth
                    .total_cmp(&a_depth)
                    .then_with(|| document_primitive_order(a.primitive).cmp(&document_primitive_order(b.primitive)))
            });
        } else {
            // Preserve the traditional fill-before-outline ordering. Stable
            // sorting retains object order within each primitive stream.
            batches.sort_by_key(|batch| document_primitive_order(batch.primitive));
        }

        let mut bound_primitive = None;
        for batch in batches {
            if bound_primitive != Some(batch.primitive) {
                let pipeline = match (stage, batch.primitive, xray_enabled) {
                    (_, DocumentPrimitive::Fill, true) => &self.xray_render_pipeline,
                    (DocumentRenderStage::AlwaysVisible, DocumentPrimitive::Fill, false) => &self.xray_render_pipeline,
                    (DocumentRenderStage::Opaque, DocumentPrimitive::Fill, false) | (DocumentRenderStage::Overlay, DocumentPrimitive::Fill, false) => &self.render_pipeline,
                    (DocumentRenderStage::Translucent, DocumentPrimitive::Fill, false) => &self.transparent_document_fill_pipeline,
                    (_, DocumentPrimitive::Stroke, true) => &self.overlay_render_pipeline,
                    (DocumentRenderStage::AlwaysVisible, DocumentPrimitive::Stroke, false) => &self.overlay_render_pipeline,
                    (DocumentRenderStage::Opaque, DocumentPrimitive::Stroke, false) => &self.opaque_stroke_render_pipeline,
                    (DocumentRenderStage::Translucent, DocumentPrimitive::Stroke, false) | (DocumentRenderStage::Overlay, DocumentPrimitive::Stroke, false) => {
                        &self.stroke_render_pipeline
                    }
                };
                render_pass.set_pipeline(pipeline);
                render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                match batch.primitive {
                    DocumentPrimitive::Fill => {
                        render_pass.set_vertex_buffer(0, self.lyon_vertex_gpu.slice(..));
                        render_pass.set_index_buffer(self.lyon_index_gpu.slice(..), wgpu::IndexFormat::Uint32);
                    }
                    DocumentPrimitive::Stroke => {
                        render_pass.set_vertex_buffer(0, self.stroke_vertex_gpu.slice(..));
                        render_pass.set_index_buffer(self.stroke_index_gpu.slice(..), wgpu::IndexFormat::Uint32);
                    }
                }
                bound_primitive = Some(batch.primitive);
            }

            let available = match batch.primitive {
                DocumentPrimitive::Fill => self.lyon_buffer.indices.len(),
                DocumentPrimitive::Stroke => self.stroke_index_buf.len(),
            };
            if let Some(range) = clamped_document_batch_range(batch.index_range, available) {
                render_pass.draw_indexed(range, 0, 0..1);
            }
        }
    }

    fn draw_text_batches<'pass>(&'pass self, render_pass: &mut wgpu::RenderPass<'pass>, stage: DocumentRenderStage, xray_enabled: bool) {
        let batches = self.text_draw_batches.iter().filter(|batch| batch.stage(xray_enabled) == stage).collect::<Vec<_>>();
        if batches.is_empty() {
            return;
        }
        let pipeline = match stage {
            DocumentRenderStage::Opaque => &self.render_pipeline,
            DocumentRenderStage::Translucent | DocumentRenderStage::Overlay => &self.transparent_document_fill_pipeline,
            DocumentRenderStage::AlwaysVisible => &self.xray_render_pipeline,
        };
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.text_vertex_gpu.slice(..));
        render_pass.set_index_buffer(self.text_index_gpu.slice(..), wgpu::IndexFormat::Uint32);
        for batch in batches {
            if let Some(range) = clamped_document_batch_range(batch.index_range, self.text_index_buf.len()) {
                render_pass.draw_indexed(range, 0, 0..1);
            }
        }
    }

    fn draw_static_document_strokes<'pass>(&'pass self, render_pass: &mut wgpu::RenderPass<'pass>, xray_enabled: bool) {
        if !self.static_strokes.chunks().iter().any(|chunk| chunk.drawable()) {
            return;
        }
        render_pass.set_pipeline(if xray_enabled {
            &self.overlay_render_pipeline
        } else {
            &self.opaque_stroke_render_pipeline
        });
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
        for chunk in self.static_strokes.chunks() {
            if !chunk.drawable() {
                continue;
            }
            let (Some(vertex_gpu), Some(index_gpu)) = (&chunk.vertex_gpu, &chunk.index_gpu) else {
                continue;
            };
            render_pass.set_vertex_buffer(0, vertex_gpu.slice(..));
            render_pass.set_index_buffer(index_gpu.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..chunk.index_count, 0, 0..1);
        }
    }

    /// Scene-origin-relative AABB of a triangulation mesh, in the same space as
    /// the uploaded surface vertices (`world - scene_origin`, no vertical
    /// exaggeration - that lives in `view_proj`). Matches `surface_vertex` so a
    /// frustum built from `camera_uniform.view_proj` culls it correctly.
    fn mesh_scene_aabb(&self, mesh: &crate::model::formats::mesh_data::Triangulation) -> (glam::Vec3, glam::Vec3) {
        let bounds = mesh.bounds();
        let min = DVec3::new(bounds.min.x, bounds.min.y, bounds.min.z) - self.scene_origin;
        let max = DVec3::new(bounds.max.x, bounds.max.y, bounds.max.z) - self.scene_origin;
        (min.as_vec3(), max.as_vec3())
    }

    /// Whether the camera is looking exactly down in orthographic mode - the
    /// only view in which flat plan-view raster images are drawn.
    fn plan_view_active(&self) -> bool {
        !self.projection.is_perspective() && self.camera.forward().z <= -(1.0 - 1.0e-6)
    }

    /// Clamp a viewport rect against the current render target size. `viewport`
    /// reflects last frame's UI layout (see `apply_canvas_rect`), so it can be
    /// briefly stale by a frame relative to a resize that landed this tick -
    /// this guards every `set_viewport` call against going out of bounds.
    fn clamp_viewport_rect(&self, viewport: ViewportRect) -> (u32, u32, u32, u32) {
        let target_width = self.config.width.max(1);
        let target_height = self.config.height.max(1);
        let x = viewport.x.min(target_width - 1);
        let y = viewport.y.min(target_height - 1);
        let width = viewport.width.min(target_width - x).max(1);
        let height = viewport.height.min(target_height - y).max(1);
        (x, y, width, height)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_scene_pass(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        viewport: ViewportRect,
        editor: &EditorState,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        rasters: &[OpenRasterTexture],
        include_editor_overlays: bool,
    ) {
        let bg_color = editor.renderer_background_color;
        let clear_color = [bg_color[0].clamp(0.0, 1.0) as f64, bg_color[1].clamp(0.0, 1.0) as f64, bg_color[2].clamp(0.0, 1.0) as f64];
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Render Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa_view,
                resolve_target: Some(view),
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear_color[0],
                        g: clear_color[1],
                        b: clear_color[2],
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        let (vp_x, vp_y, vp_width, vp_height) = self.clamp_viewport_rect(viewport);
        render_pass.set_viewport(vp_x as f32, vp_y as f32, vp_width as f32, vp_height as f32, 0.0, 1.0);

        // Block model chunks carry their own AABB, so cheaply skip GPU draw
        // calls for chunks that are entirely outside the current view
        // instead of always uploading/drawing every renderable block.
        let view_proj = glam::Mat4::from_cols_array_2d(&self.camera_uniform.view_proj);
        let frustum = Frustum::from_view_proj(view_proj);
        let point_billboard_axes = point_billboard_axes(view_proj);
        let viewport_dims = glam::vec2(vp_width as f32, vp_height as f32);

        // Developer chunk-debug view: colour each surface chunk distinctly and
        // report how many chunks survive frustum culling.
        let debug_chunks = editor.debug_chunk_coloring;
        let mut rendered_chunks: u32 = 0;
        let mut total_chunks: u32 = 0;

        // Undraped rasters show as flat plan-view images: drawn before all
        // scene geometry, pinned to the far plane with depth writes off, and
        // only when the view is exactly top-down orthographic - from any
        // other angle a heightless image would be misleading.
        if self.plan_view_active() {
            let draped: std::collections::HashSet<_> = triangulations.iter().filter_map(|triangulation| triangulation.raster_texture).collect();
            let mut pipeline_bound = false;
            for raster in rasters {
                if draped.contains(&raster.id) || !raster.visible {
                    continue;
                }
                let Some((bind_group, vertex_buffer)) = self.raster_gpu.plane(raster.id) else {
                    continue;
                };
                if !pipeline_bound {
                    render_pass.set_pipeline(&self.raster_plane_render_pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    pipeline_bound = true;
                }
                render_pass.set_bind_group(1, bind_group, &[]);
                render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                render_pass.draw(0..4, 0..1);
            }
        }

        // World XY plane at Z=0. Drawn before all scene geometry with depth
        // writes off, so every object covers it regardless of which side of
        // the plane it sits on - a surface below Z=0 reads as ground under
        // the grid rather than being cut through by it. It is an editor-only
        // aid: plot and slice-preview passes omit it.
        if include_editor_overlays && editor.show_xy_grid && self.slice_view.is_none() {
            render_pass.set_pipeline(&self.grid_render_pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            render_pass.set_bind_group(1, &self.grid_bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }

        // Point splats write depth, so they draw with the opaque geometry -
        // before the transparent surfaces that must blend over them.
        if !self.point_cloud_gpu.is_empty() {
            let display_now = Instant::now();
            let mut colored_pipeline_active = None;
            for point_cloud in point_clouds {
                if !point_cloud.state.loaded || !point_cloud.visible || editor.hidden_handles.contains(&point_cloud.entity_id()) {
                    continue;
                }
                let Some(cached) = self.point_cloud_gpu.get(point_cloud.id) else {
                    continue;
                };
                if colored_pipeline_active != Some(cached.colored) {
                    let pipeline = if cached.colored {
                        &self.point_cloud_colored_render_pipeline
                    } else {
                        &self.point_cloud_uncolored_render_pipeline
                    };
                    render_pass.set_pipeline(pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    colored_pipeline_active = Some(cached.colored);
                }
                render_pass.set_bind_group(1, &cached.style_bind_group, &[]);
                for chunk in cached.chunks.iter().filter_map(Option::as_ref) {
                    let bounds_min = chunk.bounds_min + cached.origin_scene;
                    let bounds_max = chunk.bounds_max + cached.origin_scene;
                    // Projected bounds are conservative and include raster
                    // padding. Unlike a separate plane test, an uncertain box
                    // at the eye plane returns `None` and is always retained.
                    let projected = aabb_scissor_rect(&view_proj, bounds_min, bounds_max, viewport_dims.x, viewport_dims.y);
                    if projected.is_some_and(|(_, _, width, height)| width == 0 || height == 0) {
                        continue;
                    }
                    let desired_estimates = projected
                        .zip(projected_aabb_pixel_size(&view_proj, bounds_min, bounds_max, viewport_dims))
                        .map(|(scissor, full_projected_size)| {
                            let projected_size = point_lod_projected_size(scissor, full_projected_size, viewport_dims);
                            let center = (bounds_min + bounds_max) * 0.5;
                            let splat_pixels = projected_world_splat_size(view_proj, center, cached.point_size, point_billboard_axes, viewport_dims);
                            let projected_area_estimate = desired_point_splats(projected_size.x, projected_size.y, splat_pixels);
                            // Correct the area estimate's uniform-point
                            // assumption using the chunk's measured spacing:
                            // where real points sit farther apart than an ideal
                            // grid, more of them are needed to leave no on-screen
                            // gaps. This is the precomputed equivalent of the old
                            // per-frame screen-space coverage probing, and only
                            // ever raises the count, never lowers it.
                            let selected =
                                coverage_desired_splats(chunk.base_spacing, cached.point_size, splat_pixels, chunk.level_counts[0] as usize).max(projected_area_estimate);
                            (selected, projected_area_estimate)
                        });
                    let desired_points = desired_estimates.map_or(usize::MAX, |(selected, _)| selected);
                    let requested_level = point_lod_with_hysteresis(chunk.level_counts, desired_points, chunk.requested_level.get());
                    let target_level = requested_level.max(chunk.uploaded_level);
                    let target_count = chunk.level_counts[target_level];
                    let mut instance_count = chunk.displayed_count.get();
                    if include_editor_overlays {
                        chunk.requested_level.set(requested_level);
                        // Freshly streamed data should become visible at once:
                        // if a finer prefix has been uploaded since this chunk
                        // was last drawn, snap to it rather than ramping. The
                        // gentle ramp is reserved for camera-driven LOD changes
                        // over already-resident data, where it prevents abrupt
                        // power-of-two brightness steps.
                        let streamed = chunk.displayed_uploaded_level.get() != chunk.uploaded_level;
                        if streamed {
                            chunk.displayed_uploaded_level.set(chunk.uploaded_level);
                            instance_count = target_count;
                            chunk.displayed_count.set(instance_count);
                            chunk.display_target_count.set(target_count);
                            chunk.last_display_update.set(display_now);
                        } else if chunk.display_target_count.get() != target_count {
                            chunk.display_target_count.set(target_count);
                            chunk.last_display_update.set(display_now);
                        } else {
                            let elapsed = display_now.saturating_duration_since(chunk.last_display_update.get()).as_secs_f64();
                            instance_count = smoothed_point_count(instance_count, target_count, elapsed);
                            chunk.displayed_count.set(instance_count);
                            chunk.last_display_update.set(display_now);
                        }
                    }
                    render_pass.set_vertex_buffer(0, chunk.slot.buffer().slice(chunk.slot.vertex_range()));
                    render_pass.draw(0..4, 0..instance_count);
                }
            }
        }

        // Ordinarily drillholes are opaque, depth-writing scene assets. In
        // x-ray mode they move to the late overlay pass instead.
        if !editor.xray_enabled {
            self.draw_drill_holes(
                &mut render_pass,
                drill_holes,
                &editor.hidden_handles,
                false,
                true,
                editor.active_workspace != crate::ui::state::Workspace::DrillAndBlast,
            );
        }

        // Opaque document fills and strokes must establish colour and depth
        // before any translucent surface or block-model composite. X-ray
        // intentionally remains an editor overlay and is deferred.
        if !editor.xray_enabled {
            self.draw_document_batches(&mut render_pass, DocumentRenderStage::Opaque, false, Some(DocumentPrimitive::Fill));
            self.draw_static_document_strokes(&mut render_pass, false);
            self.draw_document_batches(&mut render_pass, DocumentRenderStage::Opaque, false, Some(DocumentPrimitive::Stroke));
        }

        if !self.triangulation_gpu.is_empty() || !self.block_model_gpu.is_empty() {
            render_pass.set_pipeline(&self.surface_render_pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            for triangulation in triangulations {
                let entity = triangulation.entity_id();
                if !triangulation.state.loaded || !triangulation.visible || editor.hidden_handles.contains(&entity) {
                    continue;
                }
                let Some(cached) = self.triangulation_gpu.get(triangulation.id) else {
                    continue;
                };
                if cached.color[3] < 0.999 {
                    continue;
                }
                total_chunks += cached.surface_chunks.len() as u32;
                // Cheap whole-mesh reject before touching individual chunks.
                let (aabb_min, aabb_max) = self.mesh_scene_aabb(&triangulation.mesh);
                if !frustum.intersects_aabb(aabb_min, aabb_max) {
                    continue;
                }
                if !debug_chunks {
                    render_pass.set_bind_group(1, &cached.surface_style_bind_group, &[]);
                }
                render_pass.set_bind_group(3, self.raster_gpu.bind_group(cached.raster_texture), &[]);
                for chunk in &cached.surface_chunks {
                    // Per-chunk frustum cull: chunks are Morton-spatial, so their
                    // AABBs are tight enough for this to reject real geometry.
                    if !frustum.intersects_aabb(chunk.bounds_min, chunk.bounds_max) {
                        continue;
                    }
                    if debug_chunks {
                        render_pass.set_bind_group(1, &chunk.debug_style_bind_group, &[]);
                    }
                    render_pass.set_bind_group(2, &chunk.chunk_bind_group, &[]);
                    rendered_chunks += 1;
                    render_pass.set_vertex_buffer(0, chunk.vertex_buffer.slice(..));
                    render_pass.set_index_buffer(chunk.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..chunk.index_count, 0, 0..1);
                }
            }
            for block_model in block_models {
                let entity = block_model.entity_id();
                if !block_model.state.loaded
                    || !block_model.visible
                    || editor.hidden_handles.contains(&entity)
                    || !block_model_intersects_frustum(block_model, self.scene_origin, &frustum)
                {
                    continue;
                }
                let Some(cached) = self.block_model_gpu.get(block_model.id) else {
                    continue;
                };
                if cached.volume.is_some() {
                    continue;
                }
                if cached.surface_chunks.is_empty() {
                    continue;
                }
                render_pass.set_pipeline(&self.block_model_render_pipeline);
                render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                render_pass.set_bind_group(1, &cached.surface_style_bind_group, &[]);
                // Draw chunks nearest-first: the block shader contains
                // `discard`, so depth writes happen late, but the early depth
                // *test* still rejects fragments behind already-drawn
                // geometry - provided nearer chunks were drawn first. Zoomed
                // in, chunk faces cover most of the viewport and this
                // ordering turns worst-case overdraw shading into cheap
                // depth rejections.
                let camera_scene = (self.camera.position - self.scene_origin).as_vec3();
                let view_forward = self.camera.forward().as_vec3();
                let mut visible_chunks: Vec<_> = cached
                    .surface_chunks
                    .iter()
                    .filter(|chunk| frustum.intersects_aabb(chunk.gpu.bounds_min, chunk.gpu.bounds_max))
                    .collect();
                let chunk_depth = |chunk: &crate::rendering::scene::gpu_cache::CachedBlockModelSurfaceChunk| {
                    let center = (chunk.gpu.bounds_min + chunk.gpu.bounds_max) * 0.5;
                    (center - camera_scene).dot(view_forward)
                };
                visible_chunks.sort_by(|a, b| chunk_depth(a).total_cmp(&chunk_depth(b)));
                for chunk in visible_chunks {
                    // Non-indexed: the shader expands 36 vertices per instance
                    // into a cube. One instance per block.
                    render_pass.set_vertex_buffer(0, chunk.gpu.instance_buffer.slice(..));
                    render_pass.draw(0..36, 0..chunk.gpu.instance_count);
                }
                render_pass.set_pipeline(&self.surface_render_pipeline);
                render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            }
        }

        if !self.triangulation_gpu.is_empty() {
            let mut transparent: Vec<_> = triangulations
                .iter()
                .filter(|triangulation| {
                    let entity = triangulation.entity_id();
                    triangulation.visible && !editor.hidden_handles.contains(&entity) && self.triangulation_gpu.get(triangulation.id).is_some_and(|cached| cached.color[3] < 0.999)
                })
                .collect();
            let forward = self.camera.forward();
            transparent.sort_by(|a, b| {
                let ac = a.mesh.bounds().center();
                let bc = b.mesh.bounds().center();
                let ad = (DVec3::new(ac.x, ac.y, ac.z) - self.camera.position).dot(forward);
                let bd = (DVec3::new(bc.x, bc.y, bc.z) - self.camera.position).dot(forward);
                bd.total_cmp(&ad)
            });
            render_pass.set_pipeline(&self.transparent_surface_render_pipeline);
            for triangulation in transparent {
                let Some(cached) = self.triangulation_gpu.get(triangulation.id) else {
                    continue;
                };
                total_chunks += cached.surface_chunks.len() as u32;
                let (aabb_min, aabb_max) = self.mesh_scene_aabb(&triangulation.mesh);
                if !frustum.intersects_aabb(aabb_min, aabb_max) {
                    continue;
                }
                if !debug_chunks {
                    render_pass.set_bind_group(1, &cached.surface_style_bind_group, &[]);
                }
                render_pass.set_bind_group(3, self.raster_gpu.bind_group(cached.raster_texture), &[]);
                for chunk in &cached.surface_chunks {
                    if !frustum.intersects_aabb(chunk.bounds_min, chunk.bounds_max) {
                        continue;
                    }
                    if debug_chunks {
                        render_pass.set_bind_group(1, &chunk.debug_style_bind_group, &[]);
                    }
                    render_pass.set_bind_group(2, &chunk.chunk_bind_group, &[]);
                    rendered_chunks += 1;
                    render_pass.set_vertex_buffer(0, chunk.vertex_buffer.slice(..));
                    render_pass.set_index_buffer(chunk.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..chunk.index_count, 0, 0..1);
                }
            }
        }

        drop(render_pass);
        // Secondary viewports (slice previews) must not overwrite the main
        // viewport's statistics or advance the shared volume residency
        // streamer: the main pass may already be encoded against the current
        // residency tables, and two cameras would evict each other's bricks
        // every frame. Previews read whatever is resident and fall back to
        // brick aggregates elsewhere.
        if include_editor_overlays {
            self.chunk_render_stats = (rendered_chunks, total_chunks);
        }
        let needs_volume_target = block_models.iter().any(|block_model| {
            let entity = block_model.entity_id();
            block_model.visible
                && !editor.hidden_handles.contains(&entity)
                && block_model_intersects_frustum(block_model, self.scene_origin, &frustum)
                && self
                    .block_model_gpu
                    .get(block_model.id)
                    .is_some_and(|cached| cached.volume.as_ref().is_some_and(|volume| frustum.intersects_aabb(volume.bounds_min, volume.bounds_max)))
        });
        let needs_transparency_target = needs_volume_target
            || block_models.iter().any(|block_model| {
                let entity = block_model.entity_id();
                block_model.visible
                    && !editor.hidden_handles.contains(&entity)
                    && block_model_intersects_frustum(block_model, self.scene_origin, &frustum)
                    && self.block_model_gpu.get(block_model.id).is_some_and(|cached| {
                        cached.volume.is_none()
                            && cached
                                .transparent_surface_chunks
                                .iter()
                                .any(|chunk| frustum.intersects_aabb(chunk.gpu.bounds_min, chunk.gpu.bounds_max))
                    })
            });
        self.update_block_model_optional_targets(needs_transparency_target, needs_volume_target);
        self.render_volume_block_models(encoder, view, viewport, editor, block_models, &frustum, include_editor_overlays);
        self.render_fallback_transparent_block_models(encoder, view, viewport, editor, block_models, &frustum);

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Document Transparency and Overlay Render Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa_view,
                resolve_target: Some(view),
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // A new `wgpu::RenderPass` doesn't inherit the first pass's viewport -
        // reapply it, or this pass's document/edge/overlay geometry renders
        // unclipped across the whole window instead of the visible canvas.
        render_pass.set_viewport(vp_x as f32, vp_y as f32, vp_width as f32, vp_height as f32, 0.0, 1.0);

        if editor.xray_enabled {
            // X-ray deliberately bypasses scene depth and stays above all
            // composited transparency. Drillholes draw first so design strings
            // and their outlines remain the topmost x-ray content.
            self.draw_drill_holes(&mut render_pass, drill_holes, &editor.hidden_handles, true, true, true);
            self.draw_document_batches(&mut render_pass, DocumentRenderStage::AlwaysVisible, true, Some(DocumentPrimitive::Fill));
            self.draw_static_document_strokes(&mut render_pass, true);
            self.draw_document_batches(&mut render_pass, DocumentRenderStage::AlwaysVisible, true, Some(DocumentPrimitive::Stroke));
        } else {
            if editor.active_workspace == crate::ui::state::Workspace::DrillAndBlast {
                self.draw_drill_holes(&mut render_pass, drill_holes, &editor.hidden_handles, true, false, true);
            }
            // Alpha document primitives test the complete opaque depth buffer
            // but never update it, so farther translucent fills still blend.
            self.draw_document_batches(&mut render_pass, DocumentRenderStage::Translucent, false, None);
            self.draw_document_batches(&mut render_pass, DocumentRenderStage::Overlay, false, None);
            if include_editor_overlays {
                self.draw_text_batches(&mut render_pass, DocumentRenderStage::Overlay, false);
            }
        }

        if include_editor_overlays && !self.dynamic_vertex_buf.is_empty() && !self.dynamic_index_buf.is_empty() {
            render_pass.set_pipeline(if editor.xray_enabled || editor.tying_holes() {
                &self.overlay_render_pipeline
            } else {
                &self.stroke_render_pipeline
            });
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.dynamic_vertex_gpu.slice(..));
            render_pass.set_index_buffer(self.dynamic_index_gpu.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..self.dynamic_index_buf.len() as u32, 0, 0..1);
        }

        render_pass.set_pipeline(&self.edge_render_pipeline);
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
        for triangulation in triangulations {
            let entity = triangulation.entity_id();
            if !triangulation.state.loaded || !triangulation.visible || editor.hidden_handles.contains(&entity) {
                continue;
            }
            let Some(cached) = self.triangulation_gpu.get(triangulation.id) else {
                continue;
            };
            if cached.edge_width <= 0.0 || cached.edge_chunks.is_empty() {
                continue;
            }
            render_pass.set_bind_group(1, &cached.edge_style_bind_group, &[]);
            for chunk in &cached.edge_chunks {
                render_pass.set_vertex_buffer(0, chunk.instance_buffer.slice(..));
                render_pass.draw(0..6, 0..chunk.instance_count);
            }
        }
        for block_model in block_models {
            let entity = block_model.entity_id();
            if !block_model.state.loaded
                || !block_model.visible
                || editor.hidden_handles.contains(&entity)
                || !block_model_intersects_frustum(block_model, self.scene_origin, &frustum)
            {
                continue;
            }
            let Some(cached) = self.block_model_gpu.get(block_model.id) else {
                continue;
            };
            if cached.edge_chunks.is_empty() {
                continue;
            }
            render_pass.set_bind_group(1, &cached.edge_style_bind_group, &[]);
            for chunk in &cached.edge_chunks {
                render_pass.set_vertex_buffer(0, chunk.instance_buffer.slice(..));
                render_pass.draw(0..6, 0..chunk.instance_count);
            }
        }

        if include_editor_overlays && !self.design_point_gpu.chunks.is_empty() {
            render_pass.set_pipeline(&self.design_point_render_pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            // Draw a black outer square followed by a smaller white square so
            // vertices stay legible against both light and dark geometry.
            for style in [&self.design_point_gpu.outer_style_bind_group, &self.design_point_gpu.inner_style_bind_group] {
                render_pass.set_bind_group(1, style, &[]);
                for chunk in &self.design_point_gpu.chunks {
                    render_pass.set_vertex_buffer(0, chunk.instance_buffer.slice(..));
                    render_pass.draw(0..4, 0..chunk.instance_count);
                }
            }
        }

        if include_editor_overlays && !self.overlay_vertex_buf.is_empty() && !self.overlay_index_buf.is_empty() {
            render_pass.set_pipeline(&self.overlay_render_pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.overlay_vertex_gpu.slice(..));
            render_pass.set_index_buffer(self.overlay_index_gpu.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..self.overlay_index_buf.len() as u32, 0, 0..1);
        }

        // Active text editing is a true editor overlay: draw the label after
        // scene edges and tool previews, then its box last. In global x-ray
        // mode every text batch follows this same final always-visible path.
        if include_editor_overlays {
            self.draw_text_batches(&mut render_pass, DocumentRenderStage::AlwaysVisible, editor.xray_enabled);
        }
        self.draw_document_batches(&mut render_pass, DocumentRenderStage::AlwaysVisible, false, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_volume_block_models(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        viewport: ViewportRect,
        editor: &EditorState,
        block_models: &[OpenBlockModel],
        frustum: &Frustum,
        stream_residency: bool,
    ) {
        let mut visible_volume_blocks = block_models
            .iter()
            .filter(|block_model| {
                let entity = block_model.entity_id();
                block_model.visible
                    && !editor.hidden_handles.contains(&entity)
                    && block_model_intersects_frustum(block_model, self.scene_origin, frustum)
                    && self
                        .block_model_gpu
                        .get(block_model.id)
                        .is_some_and(|cached| cached.volume.as_ref().is_some_and(|volume| frustum.intersects_aabb(volume.bounds_min, volume.bounds_max)))
            })
            .collect::<Vec<_>>();
        if visible_volume_blocks.is_empty() {
            return;
        }
        // Advance residency only after whole-model visibility is known.
        // Preview cameras reuse the primary viewport's residency and never
        // mutate it, so two cameras cannot thrash each other's brick pools.
        if stream_residency {
            let camera_scene = (self.camera.position - self.scene_origin).as_vec3();
            for block_model in &visible_volume_blocks {
                self.block_model_gpu.stream_volume(&self.queue, camera_scene, block_model.id);
            }
        }
        let volume_target = self.block_model_volume_target.as_ref().expect("visible volume block model must have a volume target");
        let transparency_targets = self
            .block_model_transparency_targets
            .as_ref()
            .expect("visible volume block model must have a depth bind group");
        // Each model raycasts independently and blends One/OneMinusSrcAlpha
        // into the shared target, so correctness requires back-to-front
        // ordering - the same convention as the transparent-triangulation
        // sort above. (Truly interpenetrating volumes would need a merged
        // march; distinct models composited far-to-near is the honest
        // approximation.)
        let forward = self.camera.forward();
        let camera_position = self.camera.position;
        let depth_along_view = |block_model: &OpenBlockModel| {
            block_model
                .world_bounds()
                .map(|(min, max)| ((min + max) * 0.5 - camera_position).dot(forward))
                .unwrap_or(0.0)
        };
        visible_volume_blocks.sort_by(|a, b| depth_along_view(b).total_cmp(&depth_along_view(a)));

        // Draw the raycast into an off-screen target at `render_scale` of full
        // resolution (reduced while the camera moves), then upscale it over the
        // scene. The target is full-size; we fill only the top-left sub-rect so
        // the scale can change per frame with no reallocation. Keeping the two
        // in lockstep: the raycast shader derives the same sub-rect from
        // `camera.viewport.xy * render_scale` - that uniform tracks the visible
        // scene viewport (see `apply_canvas_rect`), not the swapchain, so this
        // must too. Take it from this pass's own `viewport` rather than
        // `self.viewport_rect`: secondary cameras (slice previews, plot
        // exports) render a full-surface viewport into their own swapped-in
        // targets, sized to the swapped-in `config`, while `viewport_rect`
        // still describes the main window's canvas. The clamp is what keeps
        // every scissor below inside the target.
        let render_scale = self.camera_uniform.render_scale();
        let (_, _, vp_width, vp_height) = self.clamp_viewport_rect(viewport);
        let scaled_width = (vp_width as f32 * render_scale).max(1.0);
        let scaled_height = (vp_height as f32 * render_scale).max(1.0);
        self.queue
            .write_buffer(&volume_target.params_buffer, 0, bytemuck::cast_slice(&[scaled_width, scaled_height, 0.0f32, 0.0f32]));

        // Beam pre-pass: conservative per-8x8-tile ray entry depths (see
        // fs_beam in block_model_volume.wgsl). Cleared to 0 ("no bound"), so
        // skipping this pass entirely is a valid runtime fallback.
        // Enablement is per-model (`beam_enabled`) and
        // must match the `lod.w` uniform flag the main pass reads: whenever
        // any visible volume will sample the beam texture, this pass must at
        // least clear it so stale depths from earlier frames can't corrupt
        // the march.
        let enable_volume_beam = visible_volume_blocks.iter().any(|block_model| {
            self.block_model_gpu
                .get(block_model.id)
                .and_then(|cached| cached.volume.as_ref())
                .is_some_and(|volume| volume.beam_enabled())
        });
        if enable_volume_beam {
            let view_proj = glam::Mat4::from_cols_array_2d(&self.camera_uniform.view_proj);
            let beam_width = (scaled_width / 8.0).ceil().max(1.0);
            let beam_height = (scaled_height / 8.0).ceil().max(1.0);
            let beam_full_rect = (0u32, 0u32, beam_width as u32, beam_height as u32);
            let mut beam_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Block Model Volume Beam Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &volume_target.beam_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // One shared beam texture serves every volume draw, so with two
            // or more overlapping volumes a later draw could raise the entry
            // depth past an earlier model's content (R32Float min-blending
            // is an optional feature). Restrict the optimization to the
            // single-volume case; the cleared texture is a no-op otherwise.
            if visible_volume_blocks.len() == 1 {
                beam_pass.set_viewport(0.0, 0.0, beam_width, beam_height, 0.0, 1.0);
                beam_pass.set_pipeline(&self.block_model_beam_pipeline);
                beam_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                for block_model in &visible_volume_blocks {
                    let Some(cached) = self.block_model_gpu.get(block_model.id) else {
                        continue;
                    };
                    let Some(volume) = cached.volume.as_ref() else {
                        continue;
                    };
                    // The model's scissor rect, shrunk to beam resolution
                    // with one texel of padding on each side.
                    let (x, y, w, h) = aabb_scissor_rect(&view_proj, volume.bounds_min, volume.bounds_max, scaled_width, scaled_height)
                        .map(|(x, y, w, h)| {
                            let x0 = (x / 8).saturating_sub(1);
                            let y0 = (y / 8).saturating_sub(1);
                            let x1 = ((x + w).div_ceil(8) + 1).min(beam_full_rect.2);
                            let y1 = ((y + h).div_ceil(8) + 1).min(beam_full_rect.3);
                            (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
                        })
                        .unwrap_or(beam_full_rect);
                    if w == 0 || h == 0 {
                        continue;
                    }
                    beam_pass.set_scissor_rect(x, y, w, h);
                    beam_pass.set_bind_group(1, &volume.bind_group, &[]);
                    beam_pass.draw(0..3, 0..1);
                }
            }
        }

        {
            let mut volume_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Block Model Volume Raycast Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &volume_target.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            volume_pass.set_viewport(0.0, 0.0, scaled_width, scaled_height, 0.0, 1.0);
            volume_pass.set_pipeline(&self.block_model_volume_pipeline);
            volume_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            volume_pass.set_bind_group(2, &transparency_targets.transparency_fallback_bind_groups[0], &[]);
            volume_pass.set_bind_group(3, &volume_target.beam_bind_group, &[]);
            let view_proj = glam::Mat4::from_cols_array_2d(&self.camera_uniform.view_proj);
            let full_rect = (0u32, 0u32, scaled_width.ceil() as u32, scaled_height.ceil() as u32);
            for block_model in visible_volume_blocks {
                let Some(cached) = self.block_model_gpu.get(block_model.id) else {
                    continue;
                };
                let Some(volume) = cached.volume.as_ref() else {
                    continue;
                };
                // Scissor the fullscreen triangle to the model's projected
                // bounds; set per draw because scissor state persists across
                // draws within the pass.
                let (x, y, w, h) = aabb_scissor_rect(&view_proj, volume.bounds_min, volume.bounds_max, scaled_width, scaled_height).unwrap_or(full_rect);
                if w == 0 || h == 0 {
                    continue;
                }
                volume_pass.set_scissor_rect(x, y, w, h);
                volume_pass.set_bind_group(1, &volume.bind_group, &[]);
                volume_pass.draw(0..3, 0..1);
            }
        }

        let mut upscale_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Block Model Volume Upscale Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa_view,
                resolve_target: Some(view),
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let (vp_x, vp_y, vp_width, vp_height) = self.clamp_viewport_rect(viewport);
        upscale_pass.set_viewport(vp_x as f32, vp_y as f32, vp_width as f32, vp_height as f32, 0.0, 1.0);
        upscale_pass.set_pipeline(&self.block_model_volume_upscale_pipeline);
        upscale_pass.set_bind_group(0, &volume_target.bind_group, &[]);
        upscale_pass.draw(0..3, 0..1);
    }

    fn render_fallback_transparent_block_models(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        viewport: ViewportRect,
        editor: &EditorState,
        block_models: &[OpenBlockModel],
        frustum: &Frustum,
    ) {
        let visible_transparent_blocks = block_models
            .iter()
            .filter(|block_model| {
                let entity = block_model.entity_id();
                block_model.visible
                    && !editor.hidden_handles.contains(&entity)
                    && block_model_intersects_frustum(block_model, self.scene_origin, frustum)
                    && self.block_model_gpu.get(block_model.id).is_some_and(|cached| {
                        cached.volume.is_none()
                            && cached
                                .transparent_surface_chunks
                                .iter()
                                .any(|chunk| frustum.intersects_aabb(chunk.gpu.bounds_min, chunk.gpu.bounds_max))
                    })
            })
            .collect::<Vec<_>>();
        if visible_transparent_blocks.is_empty() {
            return;
        }
        let transparency_targets = self
            .block_model_transparency_targets
            .as_ref()
            .expect("visible transparent block model must have transparency targets");

        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Block Model Transparency Clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &transparency_targets.accum_views[0],
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        {
            let mut transparency_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Block Model Order-Independent Transparency Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &transparency_targets.accum_views[0],
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let (vp_x, vp_y, vp_width, vp_height) = self.clamp_viewport_rect(viewport);
            transparency_pass.set_viewport(vp_x as f32, vp_y as f32, vp_width as f32, vp_height as f32, 0.0, 1.0);
            transparency_pass.set_pipeline(&self.block_model_transparency_fallback_pipeline);
            transparency_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            transparency_pass.set_bind_group(2, &transparency_targets.transparency_fallback_bind_groups[0], &[]);
            for block_model in &visible_transparent_blocks {
                let Some(cached) = self.block_model_gpu.get(block_model.id) else {
                    continue;
                };
                transparency_pass.set_bind_group(1, &cached.surface_style_bind_group, &[]);
                for chunk in &cached.transparent_surface_chunks {
                    if !frustum.intersects_aabb(chunk.gpu.bounds_min, chunk.gpu.bounds_max) {
                        continue;
                    }
                    transparency_pass.set_vertex_buffer(0, chunk.gpu.instance_buffer.slice(..));
                    transparency_pass.draw(0..36, 0..chunk.gpu.instance_count);
                }
            }
        }

        let mut composite_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Block Model Transparency Composite Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa_view,
                resolve_target: Some(view),
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let (vp_x, vp_y, vp_width, vp_height) = self.clamp_viewport_rect(viewport);
        composite_pass.set_viewport(vp_x as f32, vp_y as f32, vp_width as f32, vp_height as f32, 0.0, 1.0);
        composite_pass.set_pipeline(&self.block_model_transparency_composite_pipeline);
        composite_pass.set_bind_group(0, &transparency_targets.composite_bind_groups[0], &[]);
        composite_pass.draw(0..3, 0..1);
    }
}
