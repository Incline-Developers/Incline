//! Offscreen orbit view of one solid, for the Solids Setup page.
//!
//! The Setup page covers the window, so the main scene pass does not run
//! while it is open (see [`crate::ui::state::EditorState::is_planning_setup`]).
//! This renders the solid being inspected on its own into an offscreen
//! texture that egui then draws into the page's third column - the same
//! technique the slice preview uses, with an orbit camera framing one mesh
//! instead of a plan view of the whole scene.

use std::sync::Arc;

use winit::dpi::PhysicalSize;

use super::{BlockModelTransparencyTargets, BlockModelVolumeTarget, Graphics};
use crate::{
    model::{Document, block_model::OpenBlockModel, drill_hole::OpenDrillHoleDataset, point_cloud::OpenPointCloud, raster::OpenRasterTexture, triangulation::OpenTriangulation},
    rendering::camera::{Camera, CameraUniform, Projection},
    ui::state::{EditorState, SolidPreviewView, ViewportRect},
};

/// Everything the preview draws besides the solid itself: the project content
/// that is loaded, so a solid can be inspected against the design strings and
/// surfaces around it rather than floating on its own.
pub(crate) struct SolidPreviewScene<'frame> {
    pub(crate) document: &'frame Document,
    pub(crate) triangulations: &'frame [OpenTriangulation],
    pub(crate) block_models: &'frame [OpenBlockModel],
    pub(crate) drill_holes: &'frame [OpenDrillHoleDataset],
    pub(crate) point_clouds: &'frame [OpenPointCloud],
    pub(crate) rasters: &'frame [OpenRasterTexture],
}

/// Fraction of the framed mesh's radius left as margin around it.
const FRAMING_MARGIN: f64 = 1.15;

pub(super) struct SolidPreviewTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    gui_view: wgpu::TextureView,
    texture_id: egui::TextureId,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    camera: Camera,
    projection: Projection,
    camera_uniform: CameraUniform,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    msaa_color: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    transparency_targets: Option<BlockModelTransparencyTargets>,
    volume_target: Option<BlockModelVolumeTarget>,
}

impl SolidPreviewTarget {
    fn new(graphics: &mut Graphics<'_>, size: PhysicalSize<u32>) -> Self {
        let size = PhysicalSize::new(size.width.max(1), size.height.max(1));
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            format: graphics.config.format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: graphics.config.view_formats.clone(),
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        let (texture, view, gui_view) = Self::create_color_target(&graphics.device, &config);
        let texture_id = graphics.gui.register_native_texture(&graphics.device, &gui_view);
        let camera = Camera::new(glam::DVec3::new(0.0, 0.0, 10.0), 0.0, 0.0);
        let projection = Projection::new(size.width, size.height, -1000.0, 1000.0);
        let mut camera_uniform = CameraUniform::new();
        camera_uniform.update_viewport(size.width, size.height);
        let camera_buffer = {
            use wgpu::util::DeviceExt;
            graphics.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Solid preview camera"),
                contents: bytemuck::bytes_of(&camera_uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
        };
        let camera_bind_group = graphics.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Solid preview camera bind group"),
            layout: &graphics.render_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let (msaa_color, msaa_view) = Graphics::create_msaa_target(&graphics.device, &config, graphics.sample_count);
        let (depth_texture, depth_view) = Graphics::create_depth_target(&graphics.device, &config, graphics.sample_count);
        Self {
            texture,
            view,
            gui_view,
            texture_id,
            config,
            size,
            camera,
            projection,
            camera_uniform,
            camera_buffer,
            camera_bind_group,
            msaa_color,
            msaa_view,
            depth_texture,
            depth_view,
            transparency_targets: None,
            volume_target: None,
        }
    }

    fn create_color_target(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Solid preview colour target"),
            size: wgpu::Extent3d {
                width: config.width,
                height: config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format.add_srgb_suffix(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[config.format.remove_srgb_suffix()],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // egui samples image bytes as already gamma encoded; the non-sRGB view
        // keeps the hardware from decoding them a second time.
        let gui_view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(config.format.remove_srgb_suffix()),
            ..Default::default()
        });
        (texture, view, gui_view)
    }

    fn recreate_targets(&mut self, graphics: &mut Graphics<'_>, size: PhysicalSize<u32>) {
        self.size = PhysicalSize::new(size.width.max(1), size.height.max(1));
        self.config.width = self.size.width;
        self.config.height = self.size.height;
        self.projection.resize(self.size.width, self.size.height);
        self.camera_uniform.update_viewport(self.size.width, self.size.height);
        (self.texture, self.view, self.gui_view) = Self::create_color_target(&graphics.device, &self.config);
        graphics.gui.update_native_texture(&graphics.device, self.texture_id, &self.gui_view);
        let (msaa_color, msaa_view) = Graphics::create_msaa_target(&graphics.device, &self.config, graphics.sample_count);
        let (depth_texture, depth_view) = Graphics::create_depth_target(&graphics.device, &self.config, graphics.sample_count);
        self.msaa_color = msaa_color;
        self.msaa_view = msaa_view;
        self.depth_texture = depth_texture;
        self.depth_view = depth_view;
        self.transparency_targets = None;
        self.volume_target = None;
    }
}

/// Centre and bounding radius of a mesh, in world units.
pub(super) fn mesh_framing(meshes: &[OpenTriangulation]) -> (glam::DVec3, f64) {
    let mut lower = glam::DVec3::splat(f64::INFINITY);
    let mut upper = glam::DVec3::splat(f64::NEG_INFINITY);
    for mesh in meshes {
        let bounds = mesh.mesh.bounds();
        lower = lower.min(glam::DVec3::new(bounds.min.x, bounds.min.y, bounds.min.z));
        upper = upper.max(glam::DVec3::new(bounds.max.x, bounds.max.y, bounds.max.z));
    }
    if !lower.is_finite() || !upper.is_finite() {
        return (glam::DVec3::ZERO, 1.0);
    }
    let center = (lower + upper) * 0.5;
    // Half the diagonal, so the mesh stays inside the frame at every orbit
    // angle rather than only the one it was fitted at.
    let radius = ((upper - lower).length() * 0.5).max(1e-3);
    (center, radius)
}

impl Graphics<'_> {
    /// Draw the solid being inspected into its own texture, and hand egui the
    /// id to paint it with. Does nothing - and releases the previous id - when
    /// no solid is being previewed.
    pub(crate) fn render_solid_preview(&mut self, preview: &[OpenTriangulation], scene: SolidPreviewScene<'_>, editor: &mut EditorState) {
        if preview.is_empty() {
            editor.solid_preview_texture = None;
            return;
        }

        let requested = PhysicalSize::new(editor.solid_preview_size_px[0].clamp(120, 2048), editor.solid_preview_size_px[1].clamp(120, 2048));
        let mut target = self.solid_preview.take().unwrap_or_else(|| SolidPreviewTarget::new(self, requested));
        let resized = target.size != requested;
        if resized {
            target.recreate_targets(self, requested);
        }
        editor.solid_preview_texture = Some(target.texture_id);

        let (center, radius) = mesh_framing(preview);
        let view = editor.solid_preview_view;
        // Re-render only when something the image depends on changed: the
        // mesh, the orbit, or the frame size. Everything else on this page is
        // egui, which composites over an unchanged texture for free.
        let key = {
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            for mesh in preview {
                (Arc::as_ptr(&mesh.mesh) as usize).hash(&mut hasher);
                mesh.cull_back_faces.hash(&mut hasher);
                if let Some(style) = mesh.flitch_style {
                    (style.pattern as u8).hash(&mut hasher);
                    for channel in style.pattern_color {
                        channel.to_bits().hash(&mut hasher);
                    }
                }
                // Edge styling is part of the image too: highlighting a picked
                // dig block changes only its line colour, and leaving that out
                // meant the preview never redrew to show the pick.
                for channel in mesh.color.iter().chain(&mesh.line_color) {
                    channel.to_bits().hash(&mut hasher);
                }
                mesh.always_show_edges.hash(&mut hasher);
                mesh.line_weight.map(f32::to_bits).hash(&mut hasher);
            }
            // What else is loaded around the solid is part of the image, so
            // the scene's own fingerprint - the camera-free one the slice
            // preview uses - decides a redraw alongside the orbit.
            super::slice_preview::slice_preview_scene_key(
                editor,
                scene.document,
                scene.triangulations,
                scene.block_models,
                scene.drill_holes,
                scene.point_clouds,
                scene.rasters,
            )
            .hash(&mut hasher);
            (requested.width, requested.height).hash(&mut hasher);
            view.hash_into(&mut hasher);
            editor.dig_outlines_key.hash(&mut hasher);
            hasher.finish()
        };
        // A click changes nothing about the image, so it would not re-render on
        // its own - and the pick is resolved against the camera this sets up.
        // Left to the key alone, a picked block stayed unhighlighted until some
        // unrelated change happened to redraw the preview.
        let pick_pending = editor.solid_preview_pick_uv.is_some();
        if resized || pick_pending || self.solid_preview_key != Some(key) {
            self.solid_preview_key = Some(key);
            self.render_solid_preview_inner(&mut target, preview, &scene, center, radius, view, editor);
        }
        self.solid_preview = Some(target);
    }

    /// Resolve a click on the preview image to the solid it landed on.
    ///
    /// Dig blocks are separate solids in this view, so picking one is how a
    /// block that the tree does not list gets selected for its own figures.
    /// The ray is unprojected through the matrix the preview was drawn with,
    /// and the points are rebased to match it.
    ///
    /// Always returns a completed result. A miss has to be distinguishable
    /// from "not resolved yet", because a miss clears the selection.
    fn pick_preview_solid(&self, preview: &[OpenTriangulation], uv: [f32; 2]) -> crate::ui::state::SolidPick {
        // A click outside the image is not a miss on the scene; the caller
        // only ever hands over points inside it, so clamping keeps a click
        // exactly on the right or bottom edge from reading as off-image.
        let uv = uv.map(|value| f64::from(value).clamp(0.0, 1.0));
        let matrix = crate::rendering::camera::scene_view_proj(&self.camera, &self.projection, self.scene_origin, 1.0);
        let inverse = matrix.inverse();
        let at_depth = |depth: f64| {
            let clip = glam::DVec4::new(2.0 * uv[0] - 1.0, 1.0 - 2.0 * uv[1], depth, 1.0);
            let point = inverse * clip;
            point.truncate() / point.w + self.scene_origin
        };
        // Reversed-Z: the near plane is at one.
        let origin = at_depth(1.0);
        let direction = (at_depth(0.0) - origin).normalize_or_zero();
        if direction == glam::DVec3::ZERO {
            return crate::ui::state::SolidPick::Miss;
        }
        let mut nearest: Option<(f64, crate::model::triangulation::TriangulationId)> = None;
        for item in preview {
            let Some(hit) = item.spatial.ray_hit(&item.mesh, origin, direction) else {
                continue;
            };
            let distance = hit.distance(origin);
            if nearest.is_none_or(|(best, _)| distance < best) {
                nearest = Some((distance, item.id));
            }
        }
        nearest.map_or(crate::ui::state::SolidPick::Miss, |(_, id)| crate::ui::state::SolidPick::Hit(id))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_solid_preview_inner(
        &mut self,
        target: &mut SolidPreviewTarget,
        preview: &[OpenTriangulation],
        scene: &SolidPreviewScene<'_>,
        center: glam::DVec3,
        radius: f64,
        view: SolidPreviewView,
        editor: &mut EditorState,
    ) {
        // Orthographic orbit: the eye sits on a sphere around the mesh, far
        // enough out that the whole of it stays in front of the near plane at
        // any angle, and the zoom - not the distance - sets the framing.
        let (sin_pitch, cos_pitch) = view.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = view.yaw.sin_cos();
        // Positive pitch puts the eye above the mesh looking down, so the
        // view direction's Z is the negative of it.
        let forward = glam::DVec3::new(cos_pitch * cos_yaw, cos_pitch * sin_yaw, -sin_pitch).normalize();
        let distance = radius * 4.0;
        target.projection.zoom = radius * FRAMING_MARGIN / view.zoom_multiplier.max(0.05);
        let framed = view.framed_center(center, radius);
        target.camera.look_to(framed - forward * distance, forward, glam::DVec3::Z, distance);

        std::mem::swap(&mut self.camera, &mut target.camera);
        std::mem::swap(&mut self.projection, &mut target.projection);
        std::mem::swap(&mut self.camera_uniform, &mut target.camera_uniform);
        std::mem::swap(&mut self.camera_buffer, &mut target.camera_buffer);
        std::mem::swap(&mut self.camera_bind_group, &mut target.camera_bind_group);
        std::mem::swap(&mut self.msaa_color, &mut target.msaa_color);
        std::mem::swap(&mut self.msaa_view, &mut target.msaa_view);
        std::mem::swap(&mut self.depth_texture, &mut target.depth_texture);
        std::mem::swap(&mut self.depth_view, &mut target.depth_view);
        std::mem::swap(&mut self.block_model_transparency_targets, &mut target.transparency_targets);
        std::mem::swap(&mut self.block_model_volume_target, &mut target.volume_target);
        std::mem::swap(&mut self.config, &mut target.config);
        std::mem::swap(&mut self.size, &mut target.size);

        // The mesh is the only thing in this view, so the depth range follows
        // straight from its own extent rather than the scene's.
        self.projection.set_symmetric_depth_extent(distance + radius * 2.0);
        self.camera_uniform.update_view_proj(&self.camera, &self.projection, self.scene_origin, 1.0);
        self.camera_uniform.update_viewport(self.size.width, self.size.height);
        self.camera_uniform.set_interaction_quality(1.0, 1.0);
        self.queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&self.camera_uniform));

        if let Some(uv) = editor.solid_preview_pick_uv.take() {
            // Resolved against the target actually drawn to, which the clamp
            // above may have sized differently from the pane the click landed
            // in. UVs survive that; pane pixels did not.
            editor.solid_preview_picked = Some(self.pick_preview_solid(preview, uv));
        }

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Solid preview encoder"),
        });
        // The solid is not a project item, so it joins the project's own
        // surfaces here rather than in the list the explorer and the main
        // viewport read. Built once per re-render, not once per frame.
        //
        // Source sheets coincide with the generated floor/roof. Suppress
        // them only in this inspector; project visibility stays unchanged.
        let mut surfaces = Vec::with_capacity(scene.triangulations.len() + preview.len());
        surfaces.extend(scene.triangulations.iter().filter(|mesh| !editor.solid_preview_sources.contains(&mesh.id)).cloned());
        surfaces.extend(preview.iter().cloned());
        self.render_scene_pass(
            &mut encoder,
            &target.view,
            ViewportRect::full(self.size.width, self.size.height),
            editor,
            &surfaces,
            scene.block_models,
            scene.drill_holes,
            scene.point_clouds,
            scene.rasters,
            false,
        );
        self.queue.submit(std::iter::once(encoder.finish()));

        std::mem::swap(&mut self.camera, &mut target.camera);
        std::mem::swap(&mut self.projection, &mut target.projection);
        std::mem::swap(&mut self.camera_uniform, &mut target.camera_uniform);
        std::mem::swap(&mut self.camera_buffer, &mut target.camera_buffer);
        std::mem::swap(&mut self.camera_bind_group, &mut target.camera_bind_group);
        std::mem::swap(&mut self.msaa_color, &mut target.msaa_color);
        std::mem::swap(&mut self.msaa_view, &mut target.msaa_view);
        std::mem::swap(&mut self.depth_texture, &mut target.depth_texture);
        std::mem::swap(&mut self.depth_view, &mut target.depth_view);
        std::mem::swap(&mut self.block_model_transparency_targets, &mut target.transparency_targets);
        std::mem::swap(&mut self.block_model_volume_target, &mut target.volume_target);
        std::mem::swap(&mut self.config, &mut target.config);
        std::mem::swap(&mut self.size, &mut target.size);
    }
}
