//! Offscreen map render for plot sheets.
//!
//! A plot needs the scene drawn at an exact engineering scale into a
//! paper-sized image, which is both larger than the window and framed by the
//! plot rather than the interactive camera. The render targets are therefore
//! built at the requested size and swapped in around one scene pass - the same
//! technique the slice preview uses - then read back as RGBA pixels for
//! [`crate::model::plot`] to compose the sheet around.

use std::sync::Arc;

use super::*;
use crate::model::plot::MAX_SHEET_PIXELS;

/// A plan-view framing for the map image.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlotMapRequest {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// World XY the map is centred on. Z only sets the camera's focal depth.
    pub(crate) center: DVec3,
    /// Orthographic half-height in world metres. The half-width follows from
    /// the requested aspect ratio, so a caller that derives both from one
    /// metres-per-pixel figure gets a true-scale image.
    pub(crate) zoom: f64,
}

/// GPU-side capture awaiting readback.
pub(crate) struct PendingPlotMap {
    buffer: Arc<wgpu::Buffer>,
    padded_bytes_per_row: u32,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

impl<'a> Graphics<'a> {
    /// Render the scene into an offscreen image framed by `request` and record
    /// a readback of it. The pixels are collected by
    /// [`Self::resolve_plot_map`] (native) or [`Self::resolve_plot_map_async`]
    /// (browser).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn capture_plot_map(
        &mut self,
        request: PlotMapRequest,
        document: &Document,
        editor: &EditorState,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        rasters: &[OpenRasterTexture],
    ) -> Result<PendingPlotMap> {
        let limit = self.device.limits().max_texture_dimension_2d.min(MAX_SHEET_PIXELS);
        let width = request.width.clamp(1, limit);
        let height = request.height.clamp(1, limit);
        if width != request.width || height != request.height {
            return Err(anyhow!(
                "A {}×{} px map exceeds this device's {limit} px texture limit; lower the plot resolution",
                request.width,
                request.height
            ));
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Plot Map Target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format.add_srgb_suffix(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let format = self.config.format.add_srgb_suffix();
        self.render_map_into(
            &view,
            width,
            height,
            request,
            document,
            editor,
            triangulations,
            block_models,
            drill_holes,
            point_clouds,
            rasters,
        );

        let padded_bytes_per_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
        let buffer = Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Plot Map Readback Buffer"),
            size: u64::from(padded_bytes_per_row) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Plot Map Readback Encoder"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        Ok(PendingPlotMap {
            buffer,
            padded_bytes_per_row,
            width,
            height,
            format,
        })
    }

    /// Render the scene, framed by `request`, into `view`.
    ///
    /// The scene pass draws through the renderer's own MSAA, depth and
    /// block-model targets, so purpose-built ones at the plot's size are
    /// swapped in around the pass and the viewport's are restored afterwards.
    #[allow(clippy::too_many_arguments)]
    fn render_map_into(
        &mut self,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        request: PlotMapRequest,
        document: &Document,
        editor: &EditorState,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        rasters: &[OpenRasterTexture],
    ) {
        let mut plot_config = self.config.clone();
        plot_config.width = width;
        plot_config.height = height;

        let (mut msaa_color, mut msaa_view) = Self::create_msaa_target(&self.device, &plot_config, self.sample_count);
        let (mut depth_texture, mut depth_view) = Self::create_depth_target(&self.device, &plot_config, self.sample_count);
        let mut camera = self.camera.clone();
        let mut projection = self.projection.clone();
        let mut size = winit::dpi::PhysicalSize::new(width, height);
        // Optional block-model targets are sized to `config`; taking them out
        // forces the pass to rebuild them for the plot and the next frame to
        // rebuild them for the window.
        let mut transparency_targets = None;
        let mut volume_target = None;

        let zoom = request.zoom.max(1.0e-4);
        projection.resize(width, height);
        projection.set_perspective(false);
        projection.zoom = zoom;
        camera.reset_to_plan_view(self.exaggerate_point(request.center), zoom);

        std::mem::swap(&mut self.camera, &mut camera);
        std::mem::swap(&mut self.projection, &mut projection);
        std::mem::swap(&mut self.msaa_color, &mut msaa_color);
        std::mem::swap(&mut self.msaa_view, &mut msaa_view);
        std::mem::swap(&mut self.depth_texture, &mut depth_texture);
        std::mem::swap(&mut self.depth_view, &mut depth_view);
        std::mem::swap(&mut self.block_model_transparency_targets, &mut transparency_targets);
        std::mem::swap(&mut self.block_model_volume_target, &mut volume_target);
        std::mem::swap(&mut self.config, &mut plot_config);
        std::mem::swap(&mut self.size, &mut size);

        self.fit_depth_to_scene(document, triangulations, block_models, drill_holes, point_clouds, &editor.hidden_handles);
        self.camera_uniform
            .update_view_proj(&self.camera, &self.projection, self.scene_origin, self.vertical_exaggeration);
        self.camera_uniform.update_viewport(width, height);
        // Plots are never interactive, so the volume raycaster always runs at
        // full quality regardless of what the viewport was last doing.
        self.camera_uniform.set_interaction_quality(1.0, 1.0);
        self.queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&self.camera_uniform));

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("Plot Map Encoder") });
        self.render_scene_pass(&mut encoder, view, editor, triangulations, block_models, drill_holes, point_clouds, rasters, false);
        self.queue.submit([encoder.finish()]);

        std::mem::swap(&mut self.camera, &mut camera);
        std::mem::swap(&mut self.projection, &mut projection);
        std::mem::swap(&mut self.msaa_color, &mut msaa_color);
        std::mem::swap(&mut self.msaa_view, &mut msaa_view);
        std::mem::swap(&mut self.depth_texture, &mut depth_texture);
        std::mem::swap(&mut self.depth_view, &mut depth_view);
        std::mem::swap(&mut self.block_model_transparency_targets, &mut transparency_targets);
        std::mem::swap(&mut self.block_model_volume_target, &mut volume_target);
        std::mem::swap(&mut self.config, &mut plot_config);
        std::mem::swap(&mut self.size, &mut size);

        // Restore the viewport camera on the GPU; the plot borrowed its
        // buffer. `upload_camera_uniform` refreshes the view-projection but
        // deliberately leaves the viewport fields alone, so restore those
        // explicitly as well. Otherwise the next viewport pass reconstructs
        // screen-space rays (most visibly for the XY grid) using the plot
        // texture's dimensions.
        self.camera_uniform.update_viewport(self.size.width, self.size.height);
        self.upload_camera_uniform(editor.block_model_interaction_resolution_divisor);
        // The cached scene image was rendered for the window, and the depth
        // range has just been refitted, so make the next frame redraw it.
        self.scene_cache_key = None;
    }

    /// Block until the captured map is readable and return it as RGBA8.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn resolve_plot_map(&self, capture: PendingPlotMap) -> Result<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        capture.buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|error| anyhow!("GPU poll failed: {error}"))?;
        rx.recv()
            .map_err(|_| anyhow!("Plot readback callback dropped"))?
            .map_err(|error| anyhow!("Plot buffer map failed: {error}"))?;
        let pixels = unpack_mapped_rgba(&capture)?;
        capture.buffer.unmap();
        Ok(pixels)
    }

    /// Hand the captured map to `deliver` once the browser has mapped it.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn resolve_plot_map_async(&self, capture: PendingPlotMap, deliver: impl FnOnce(Result<Vec<u8>>) + 'static) {
        let buffer = Arc::clone(&capture.buffer);
        let callback_buffer = Arc::clone(&buffer);
        buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let pixels = result
                .map_err(|error| anyhow!("Plot buffer map failed: {error}"))
                .and_then(|()| unpack_mapped_rgba(&capture));
            callback_buffer.unmap();
            deliver(pixels);
        });
        let _ = self.device.poll(wgpu::PollType::Poll);
    }
}

/// Strip the row padding wgpu required and normalise the surface format to
/// opaque RGBA8.
fn unpack_mapped_rgba(capture: &PendingPlotMap) -> Result<Vec<u8>> {
    let swap_bgra = match capture.format.remove_srgb_suffix() {
        wgpu::TextureFormat::Bgra8Unorm => true,
        wgpu::TextureFormat::Rgba8Unorm => false,
        other => return Err(anyhow!("Unsupported surface format for plot export: {other:?}")),
    };
    let padded = capture.buffer.get_mapped_range(..);
    let row_bytes = capture.width as usize * 4;
    let mut rgba = Vec::with_capacity(row_bytes * capture.height as usize);
    for row in padded.chunks_exact(capture.padded_bytes_per_row as usize) {
        rgba.extend_from_slice(&row[..row_bytes]);
    }
    drop(padded);
    if swap_bgra {
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
    }
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok(rgba)
}

// ── Live dialog preview ────────────────────────────────────────────────────

/// Longest edge, in pixels, of the sheet the dialog preview is rendered for.
/// The map is a fraction of this and is displayed smaller still, so it is
/// oversampled enough to stay crisp without costing a real plot render.
const PREVIEW_SHEET_PIXELS: f64 = 720.0;

/// The map image behind the export dialog's preview: a persistent texture
/// egui samples directly, so nothing is ever read back to the CPU and the
/// browser and native paths behave identically.
pub(in crate::rendering) struct PlotPreviewTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Held so the view egui samples through outlives the registration.
    _gui_view: wgpu::TextureView,
    texture_id: egui::TextureId,
    size: (u32, u32),
}

impl<'a> Graphics<'a> {
    /// Keep `editor.plot_preview_texture` pointing at an up-to-date render of
    /// the map the export would produce.
    ///
    /// The render only repeats when the framing or the scene changes, so
    /// leaving the dialog open costs nothing per frame.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn refresh_plot_preview(
        &mut self,
        editor: &mut EditorState,
        document: &Document,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        rasters: &[OpenRasterTexture],
    ) {
        use crate::ui::dialogs::plot::PlotCentre;

        let Some(dialog) = editor.plot_dialog.clone() else {
            // Release the target with the dialog; a plot-sized texture is not
            // worth holding for a window the user has closed.
            self.plot_preview = None;
            self.plot_preview_key = None;
            editor.plot_preview_texture = None;
            return;
        };
        let Ok(sheet) = crate::model::plot::layout(&dialog.spec(Vec::new())) else {
            editor.plot_preview_texture = None;
            return;
        };

        let scale = PREVIEW_SHEET_PIXELS / f64::from(sheet.sheet_width_px.max(sheet.sheet_height_px)).max(1.0);
        let width = ((sheet.map.width * scale).round() as u32).max(1);
        let height = ((sheet.map.height * scale).round() as u32).max(1);

        // The main pass refreshes these bounds each frame, so reuse them
        // rather than re-walking every mesh while the dialog sits open.
        let extents = self
            .cached_scene_bounds
            .or_else(|| self.scene_extents(document, triangulations, block_models, drill_holes, point_clouds, &editor.hidden_handles));
        let center = match dialog.centre {
            PlotCentre::AllData => extents.map(|(min, max)| (min + max) * 0.5),
            PlotCentre::CurrentView => Some(self.view_center()),
            PlotCentre::Explicit => Some(DVec3::new(dialog.centre_east, dialog.centre_north, extents.map_or(0.0, |(min, max)| (min.z + max.z) * 0.5))),
        };
        let Some(center) = center else {
            // Nothing is loaded, so there is no framing to render.
            editor.plot_preview_texture = None;
            return;
        };
        let zoom = (sheet.map.height * sheet.world_per_px * 0.5).max(1.0e-4);

        let key = {
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            super::slice_preview::slice_preview_scene_key(editor, document, triangulations, block_models, drill_holes, point_clouds, rasters).hash(&mut hasher);
            (width, height).hash(&mut hasher);
            for value in [center.x, center.y, center.z, zoom] {
                value.to_bits().hash(&mut hasher);
            }
            hasher.finish()
        };
        if self.plot_preview_key == Some(key)
            && let Some(preview) = self.plot_preview.as_ref()
            && preview.size == (width, height)
        {
            editor.plot_preview_texture = Some(preview.texture_id);
            return;
        }

        let preview = match self.plot_preview.take() {
            Some(preview) if preview.size == (width, height) => preview,
            _ => self.create_plot_preview_target(width, height),
        };
        self.render_map_into(
            &preview.view,
            width,
            height,
            PlotMapRequest { width, height, center, zoom },
            document,
            editor,
            triangulations,
            block_models,
            drill_holes,
            point_clouds,
            rasters,
        );
        self.plot_preview_key = Some(key);
        editor.plot_preview_texture = Some(preview.texture_id);
        self.plot_preview = Some(preview);
        // Guarantee a frame is presented with the image just rendered, even if
        // nothing else would have asked for one. The key now matches, so this
        // costs exactly one extra frame rather than looping.
        self.window.request_redraw();
    }

    fn create_plot_preview_target(&mut self, width: u32, height: u32) -> PlotPreviewTarget {
        let format = self.config.format;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Plot Preview Target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.add_srgb_suffix(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[format.remove_srgb_suffix()],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // egui treats sampled image bytes as gamma encoded, so it reads through
        // a non-sRGB view to avoid a second hardware decode.
        let gui_view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(format.remove_srgb_suffix()),
            ..Default::default()
        });
        let texture_id = self.gui.register_native_texture(&self.device, &gui_view);
        PlotPreviewTarget {
            _texture: texture,
            view,
            _gui_view: gui_view,
            texture_id,
            size: (width, height),
        }
    }
}
