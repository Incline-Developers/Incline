use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use glam::{DMat4, DVec2, DVec3, DVec4};
use lyon::tessellation::VertexBuffers;
use web_time::Instant;
use wgpu::util::DeviceExt;
use winit::{
    event::*,
    window::{CursorGrabMode, Window},
};

use crate::{
    Size,
    model::{
        Document, Object, SceneEntityId,
        block_model::OpenBlockModel,
        drill_hole::{DrillHoleRef, OpenDrillHoleDataset},
        point_cloud::OpenPointCloud,
        raster::OpenRasterTexture,
        triangulation::OpenTriangulation,
    },
    rendering::{
        BlockInstance, StrokeVertex, SurfaceVertex, Vertex,
        camera::{Camera, CameraController, CameraUniform, FlyCameraController, Projection, screen_to_world_on_plane},
        pick::{PickGeometry, PickRecord, TextPickRecord, pick_nearest, pick_text},
        query::SceneQuery,
        scene::{
            BlockModelGpuCache, DesignPointGpuCache, DrillCollarInstance, DrillHoleGpuCache, DrillSegmentInstance, EdgeInstance, PointCloudGpuCache, PointInstance, PointPosition,
            RasterGpuCache, StaticStrokeCache, TriangulationGpuCache,
            bounds::{scene_bounds, visible_object_aabbs},
            build::{DocumentDrawBatch, DocumentPrimitive, DocumentRenderStage, TextDrawBatch},
        },
        snap::SNAP_THRESHOLD_PX,
        text::TextSystem,
    },
    ui::{
        Gui,
        state::{CursorMode, EditorState, TieInRef, UiFrameOutput, UiProjectView, ViewportRect},
    },
};

pub(crate) mod buffers;
pub(crate) mod camera;
pub(crate) mod frame;
pub(crate) mod frustum;
pub(crate) mod init;
pub(crate) mod passes;
pub(crate) mod plot;
pub(crate) mod projections;
pub(crate) mod screenshot;
pub(crate) mod slice_preview;
pub(crate) mod targets;

pub(super) const TEXT_CACHE_TRIM_INTERVAL_FRAMES: u64 = 300;
pub(super) const MSAA_SAMPLE_COUNT: u32 = 4;
pub(super) const CAMERA_ROTATE_SENSITIVITY: f64 = 0.003;
/// Below this per-buffer limit, large tessellated scenes may be truncated.
pub(super) const COMFORTABLE_MAX_BUFFER_SIZE: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const YELLOW_HIGHLIGHT_COLOR: [f32; 4] = [1.0, 0.85, 0.0, 1.0];
/// Sizing for editable document geometry.
pub(super) const DOC_LINE_WIDTH: f32 = 1.0;
/// Colour for the in-progress stroke preview (committed segments + rubber band).
pub(super) const PREVIEW_COLOR: [f32; 4] = [0.4, 0.85, 1.0, 1.0];
pub(super) const MEASUREMENT_COLOR: [f32; 4] = [1.0, 0.82, 0.15, 1.0];
/// Outline, and default fill, for the screen-space point markers (snap cursor,
/// fuse endpoints, move-vertex handles). Near-black rather than black: linear
/// value for sRGB (20, 20, 20), since the scene renders into an sRGB target.
pub(super) const POINT_MARKER_COLOR: [f32; 4] = [0.007, 0.007, 0.007, 1.0];
/// Fill for the marker naming the point a tool will actually act on: the
/// vertex under a Move or Delete Points cursor, the one being dragged, the
/// endpoint a fuse would close on. The amber the drawn cursor turns when a
/// snap catches (`ui::elements::cursors`), so one colour means "this is the
/// point it has" wherever it shows up. Linear value for sRGB (255, 220, 50).
pub(super) const ACTIVE_POINT_COLOR: [f32; 4] = [1.0, 0.7157, 0.0319, 1.0];
/// Logical em size used by cosmic-text layout; vector outlines are normalized
/// and the document text height scales this into world units.
pub(super) const DOC_TEXT_FONT_SIZE: f32 = 64.0;
pub(super) const TEXT_EDIT_INDICATOR_COLOR: [f32; 4] = [0.15, 0.75, 1.0, 1.0];

/// Liang-Barsky segment-vs-AABB test in 2-D screen space.
/// Returns true when the segment [a, b] intersects the rectangle or either endpoint is inside it.
pub(super) fn segment_intersects_rect(a: DVec2, b: DVec2, min_x: f64, max_x: f64, min_y: f64, max_y: f64) -> bool {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let mut t0 = 0.0_f64;
    let mut t1 = 1.0_f64;
    for (p, q) in [(-dx, a.x - min_x), (dx, max_x - a.x), (-dy, a.y - min_y), (dy, max_y - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Dim an RGBA colour by scaling its alpha channel down toward transparency.
pub(super) fn make_translucent(color: &mut [f32; 4]) {
    color[3] *= 0.3;
}
pub(super) const INITIAL_CAMERA_Z_NEAR: f64 = -1.0e4;
pub(super) const INITIAL_CAMERA_Z_FAR: f64 = 1.0e4;

pub(super) fn text_bounds_corners_with_layout_width(pos: DVec3, content: &str, height: f64, rotation: f64, layout_width: f64) -> [DVec3; 4] {
    crate::model::geometry::text_bounds_corners_with_layout_width(pos, content, height, rotation, layout_width)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderSurfaceError {
    Timeout,
    Occluded,
    Outdated,
    Lost,
    Validation,
}

/// Resolved main-viewport scene without egui. UI-only frames copy this image
/// to the swapchain and paint fresh egui shapes over it, avoiding another
/// block-model volume raycast.
pub(super) struct SceneCacheTarget {
    pub(super) texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
}

/// Per-frame style and placement for the viewport's procedural world XY grid.
/// Coordinates supplied to scene shaders are rebased around `scene_origin`,
/// so the shader needs both the absolute XY offset (to keep the grid aligned
/// to world coordinates) and world Z zero in rebased space.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GridUniform {
    pub(super) origin_plane: [f32; 4],
    pub(super) level_params: [f32; 4],
    pub(super) minor_color: [f32; 4],
    pub(super) major_color: [f32; 4],
    pub(super) x_axis_color: [f32; 4],
    pub(super) y_axis_color: [f32; 4],
}

impl GridUniform {
    fn new(scene_origin: DVec3, background: [f32; 4], camera: &Camera, projection: &Projection, vertical_exaggeration: f64, fly_mode_enabled: bool) -> Self {
        fn color(rgb: [u8; 3], alpha: f32) -> [f32; 4] {
            let mut color = crate::rendering::color::rgb_bytes_to_linear_rgba(rgb);
            color[3] = alpha;
            color
        }

        let luminance = background[0] * 0.2126 + background[1] * 0.7152 + background[2] * 0.0722;
        let (minor_color, major_color, x_axis_color, y_axis_color) = if luminance > 0.35 {
            (color([72, 77, 82], 0.28), color([55, 60, 66], 0.46), color([130, 62, 66], 0.78), color([67, 108, 57], 0.78))
        } else {
            (
                color([82, 88, 96], 0.30),
                color([99, 106, 115], 0.46),
                color([112, 55, 60], 0.82),
                color([65, 101, 55], 0.82),
            )
        };

        // Blender selects one grid level for the whole view from camera
        // distance/zoom, then draws the level below and above it. Keeping the
        // choice global is what lets lines converge and disappear naturally
        // instead of maintaining a wallpaper-like density near the horizon.
        // The XY grid represents the world XY plane, independently of the
        // active drawing elevation selected in the toolbar.
        const PLANE_Z: f64 = 0.0;
        let displayed_plane_z = scene_origin.z + (PLANE_Z - scene_origin.z) * vertical_exaggeration;
        let forward_z = camera.forward().z.abs();
        let reference_distance = if projection.is_perspective() {
            let height = (camera.position.z - displayed_plane_z).abs();
            (height * (2.0 - forward_z)).max(projection.zoom * 1.0e-3)
        } else {
            projection.zoom
        }
        .max(1.0e-9);
        let grid_level = reference_distance.log10();

        let plane_scene_z = PLANE_Z - scene_origin.z;
        Self {
            origin_plane: [
                scene_origin.x as f32,
                scene_origin.y as f32,
                plane_scene_z as f32,
                if projection.is_perspective() { 1.0 } else { 0.0 },
            ],
            // y controls grazing-angle suppression. Fly mode deliberately
            // leaves the grid at full opacity even when viewed edge-on.
            level_params: [grid_level as f32, if fly_mode_enabled { 0.0 } else { 1.0 }, 0.0, 0.0],
            minor_color,
            major_color,
            x_axis_color,
            y_axis_color,
        }
    }
}

pub(crate) struct Graphics<'a> {
    // GPU resource owners are declared before device/surface/window so they
    // are dropped first during shutdown.
    pub(super) gui: Gui,
    pub(super) text_system: TextSystem,
    pub(super) surface_render_pipeline: wgpu::RenderPipeline,
    pub(super) transparent_surface_render_pipeline: wgpu::RenderPipeline,
    pub(super) grid_render_pipeline: wgpu::RenderPipeline,
    pub(super) raster_plane_render_pipeline: wgpu::RenderPipeline,
    pub(super) block_model_render_pipeline: wgpu::RenderPipeline,
    pub(super) block_model_volume_pipeline: wgpu::RenderPipeline,
    pub(super) block_model_beam_pipeline: wgpu::RenderPipeline,
    pub(super) block_model_beam_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) block_model_transparency_fallback_pipeline: wgpu::RenderPipeline,
    pub(super) block_model_transparency_composite_pipeline: wgpu::RenderPipeline,
    pub(super) block_model_volume_upscale_pipeline: wgpu::RenderPipeline,
    pub(super) block_model_volume_upscale_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) block_model_transparency_fallback_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) block_model_transparency_composite_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) block_model_volume_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) surface_style_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) surface_chunk_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) raster_surface_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) render_pipeline: wgpu::RenderPipeline,
    pub(super) transparent_document_fill_pipeline: wgpu::RenderPipeline,
    pub(super) xray_render_pipeline: wgpu::RenderPipeline,
    pub(super) opaque_stroke_render_pipeline: wgpu::RenderPipeline,
    pub(super) stroke_render_pipeline: wgpu::RenderPipeline,
    pub(super) edge_render_pipeline: wgpu::RenderPipeline,
    pub(super) point_cloud_colored_render_pipeline: wgpu::RenderPipeline,
    pub(super) point_cloud_uncolored_render_pipeline: wgpu::RenderPipeline,
    pub(super) drill_hole_render_pipeline: wgpu::RenderPipeline,
    pub(super) xray_drill_hole_render_pipeline: wgpu::RenderPipeline,
    pub(super) drill_collar_render_pipeline: wgpu::RenderPipeline,
    pub(super) xray_drill_collar_render_pipeline: wgpu::RenderPipeline,
    pub(super) design_point_render_pipeline: wgpu::RenderPipeline,
    pub(super) edge_style_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) overlay_render_pipeline: wgpu::RenderPipeline,
    pub(super) lyon_vertex_gpu: wgpu::Buffer,
    pub(super) lyon_index_gpu: wgpu::Buffer,
    pub(super) stroke_vertex_gpu: wgpu::Buffer,
    pub(super) stroke_index_gpu: wgpu::Buffer,
    pub(super) overlay_vertex_gpu: wgpu::Buffer,
    pub(super) overlay_index_gpu: wgpu::Buffer,
    pub(super) dynamic_vertex_gpu: wgpu::Buffer,
    pub(super) dynamic_index_gpu: wgpu::Buffer,
    pub(super) text_vertex_gpu: wgpu::Buffer,
    pub(super) text_index_gpu: wgpu::Buffer,
    pub(super) camera_buffer: wgpu::Buffer,
    pub(super) camera_bind_group: wgpu::BindGroup,
    pub(super) grid_buffer: wgpu::Buffer,
    pub(super) grid_bind_group: wgpu::BindGroup,
    pub(super) msaa_color: wgpu::Texture,
    pub(super) msaa_view: wgpu::TextureView,
    pub(super) scene_cache: Option<SceneCacheTarget>,
    pub(super) scene_cache_key: Option<u64>,
    pub(super) depth_texture: wgpu::Texture,
    pub(super) depth_view: wgpu::TextureView,
    pub(super) block_model_transparency_targets: Option<BlockModelTransparencyTargets>,
    pub(super) block_model_volume_target: Option<BlockModelVolumeTarget>,
    slice_preview: Option<slice_preview::DetachedSlicePreview>,
    embedded_slice_preview: Option<slice_preview::EmbeddedSlicePreview>,
    /// Scene fingerprints of the last rendered slice previews; a preview only
    /// re-renders when its key (or its own view state) changes.
    embedded_preview_scene_key: Option<u64>,
    detached_preview_scene_key: Option<u64>,
    pub(super) surface: wgpu::Surface<'a>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) adapter: wgpu::Adapter,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) instance: wgpu::Instance,
    pub(super) queue: wgpu::Queue,
    pub(super) device: wgpu::Device,
    pub(super) window: Arc<Window>,
    pub(super) config: wgpu::SurfaceConfiguration,
    pub(super) sample_count: u32,
    pub(super) size: winit::dpi::PhysicalSize<u32>,
    /// The sub-rect of the window the scene is actually visible through, once
    /// the toolbars/status bar around it are accounted for. Sourced from
    /// egui's layout one frame behind, via `apply_canvas_rect`.
    pub(super) viewport_rect: ViewportRect,
    /// The window-centring offset, in viewport pixels, that the startup view
    /// currently carries - see `track_startup_view_framing`. `None` once the
    /// startup splash has gone, which is what retires the tracking.
    pub(super) startup_view_offset: Option<DVec2>,
    pub(super) lyon_buffer: VertexBuffers<Vertex, u32>,
    pub(super) lyon_vertex_capacity: usize,
    pub(super) lyon_index_capacity: usize,
    pub(super) stroke_vertex_buf: Vec<StrokeVertex>,
    pub(super) stroke_index_buf: Vec<u32>,
    pub(super) stroke_vertex_capacity: usize,
    pub(super) stroke_index_capacity: usize,
    pub(super) overlay_vertex_buf: Vec<StrokeVertex>,
    pub(super) overlay_index_buf: Vec<u32>,
    pub(super) overlay_vertex_capacity: usize,
    pub(super) overlay_index_capacity: usize,
    /// Per-frame stroke geometry for live drawing tools (batter/berm
    /// preview); see `rebuild_dynamic_scene`.
    pub(super) dynamic_vertex_buf: Vec<StrokeVertex>,
    pub(super) dynamic_index_buf: Vec<u32>,
    pub(super) dynamic_vertex_capacity: usize,
    pub(super) dynamic_index_capacity: usize,
    pub(super) text_vertex_buf: Vec<Vertex>,
    pub(super) text_index_buf: Vec<u32>,
    pub(super) text_vertex_capacity: usize,
    pub(super) text_index_capacity: usize,
    pub(super) text_draw_batches: Vec<TextDrawBatch>,
    pub(super) camera: Camera,
    pub(super) camera_uniform: CameraUniform,
    pub(super) camera_controller: CameraController,
    pub(super) fly_camera_controller: FlyCameraController,
    pub(super) projection: Projection,
    pub(super) mouse_pressed: Option<MouseButton>,
    pub(super) fly_mode_enabled: bool,
    pub(super) slice_view: Option<SliceViewState>,
    pub(super) frame_index: u64,
    /// Last frame on which the shaped-text cache was trimmed. Tracking the
    /// elapsed interval avoids requiring a geometry rebuild to land on one
    /// exact multiple of the trim cadence.
    pub(super) last_text_cache_trim_frame: u64,
    /// When the user last interacted with the view (camera drag or window
    /// resize). Drives a short low-quality cooldown for the volume raycaster.
    pub(super) last_interaction: Option<Instant>,
    pub(super) geometry_dirty: bool,
    pub(super) cached_document_revision: u64,
    /// `EditorState::render_style_key` of the last static-scene build. The
    /// renderer compares this itself so selection/style mutations cannot
    /// leave stale baked geometry when a caller skipped invalidation.
    pub(super) cached_render_style_key: Option<u64>,
    pub(super) cached_bounds_document_revision: u64,
    pub(super) cached_scene_bounds: Option<(DVec3, DVec3)>,
    /// Per-object world AABBs (one per visible object), refreshed alongside
    /// `cached_scene_bounds`. Depth-range fitting needs them individually so a
    /// distant model outside the view sideways can be excluded from the clip
    /// range instead of stretching it to millions of units.
    pub(super) cached_object_aabbs: Vec<(DVec3, DVec3)>,
    pub(super) overlay_dirty: bool,
    pub(super) cached_scale_factor: f32,
    pub(super) cached_measurement_state: (bool, Option<DVec3>, Option<DVec3>, Vec<DVec3>),
    pub(super) cached_poly_finish_dialog: bool,
    pub(super) pick_records: Vec<PickRecord>,
    pub(super) text_pick_records: Vec<TextPickRecord>,
    pub(super) document_draw_batches: Vec<DocumentDrawBatch>,
    pub(super) orbit_marker: Option<DVec3>,
    pub(super) scene_origin: DVec3,
    pub(super) vertical_exaggeration: f64,
    pub(super) triangulation_gpu: TriangulationGpuCache,
    /// Chunked, persistently uploaded stroke geometry for stable polylines
    /// (contour output and the like); see `StaticStrokeCache`.
    pub(super) static_strokes: StaticStrokeCache,
    pub(super) block_model_gpu: BlockModelGpuCache,
    pub(super) point_cloud_gpu: PointCloudGpuCache,
    pub(super) drill_hole_gpu: DrillHoleGpuCache,
    /// Full-resolution editable design vertices, rendered as instanced
    /// screen-space markers without a display LOD.
    pub(super) design_point_gpu: DesignPointGpuCache,
    pub(super) raster_gpu: RasterGpuCache,
    /// `(rendered, total)` surface-chunk counts from the last scene pass, for
    /// the developer chunk-debug readout. One frame stale by the time the UI
    /// reads it, which is fine for a debug counter.
    pub(crate) chunk_render_stats: (u32, u32),
    /// Live map render behind the engineering-drawing dialog's preview, and
    /// the framing/scene fingerprint it was rendered for.
    pub(super) plot_preview: Option<plot::PlotPreviewTarget>,
    pub(super) plot_preview_key: Option<u64>,
    /// Destination for a one-shot viewport image export; consumed by the next
    /// `render` call (see `screenshot.rs`).
    pub(super) pending_screenshot: Option<screenshot::ScreenshotTarget>,
}

impl Graphics<'_> {
    pub(crate) fn max_raster_texture_dimension(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }
}

/// Held-key state for slice navigation: W/S moves the slab along its normal,
/// Q/E rotates the slice line.
#[derive(Debug, Default)]
pub(crate) struct SliceInputState {
    pub(super) forward: bool,
    pub(super) backward: bool,
    pub(super) rotate_left: bool,
    pub(super) rotate_right: bool,
}

impl SliceInputState {
    fn any(&self) -> bool {
        self.forward || self.backward || self.rotate_left || self.rotate_right
    }
}

/// State of the vertical slice viewing mode: an orthographic section view
/// looking horizontally along a user-drawn XY line, clipped to a slab of
/// `width` metres centred on the slice plane. The camera is derived from
/// `center`/`direction` every frame; input mutates this state, never the
/// camera directly.
#[derive(Debug)]
pub(crate) struct SliceViewState {
    /// Slice-line midpoint in display space (world XY, exaggerated Z);
    /// `z` is the current view-centre elevation.
    pub(super) center: DVec3,
    /// Unit direction of the slice line in XY ("strike"). Screen-right maps
    /// to `+direction`; the view direction (slab normal) is
    /// `(-direction.y, direction.x, 0)`.
    pub(super) direction: DVec2,
    /// Slab thickness in metres.
    pub(super) width: f64,
    /// W/S slab movement speed (m/s).
    pub(super) move_speed: f64,
    /// Q/E rotation rate (radians per second).
    pub(super) rotate_speed: f64,
    pub(super) input: SliceInputState,
    /// Accumulated middle-drag pan deltas (controller convention:
    /// `x += -dx`, `y += dy`), consumed each update tick.
    pub(super) pan: DVec2,
    /// Accumulated scroll deltas (pixels, 1 line ≈ 100 px).
    pub(super) scroll: f64,
    /// Camera and ortho zoom to restore on exit.
    pub(super) saved_camera: Camera,
    pub(super) saved_zoom: f64,
}

impl SliceViewState {
    pub(super) fn has_pending_updates(&self) -> bool {
        self.input.any() || self.pan != DVec2::ZERO || self.scroll != 0.0
    }

    /// View direction of the section (the slab normal), horizontal by
    /// construction. Chosen so that screen-right equals `+direction`.
    pub(super) fn forward(&self) -> DVec3 {
        slice_view_forward(self.direction)
    }
}

/// View direction for a slice line running along `direction`:
/// `forward × +Z == direction`, so "strike" reads left-to-right on screen.
pub(crate) fn slice_view_forward(direction: DVec2) -> DVec3 {
    DVec3::new(-direction.y, direction.x, 0.0)
}

/// Horizontal world-space half-span visible in an orthographic viewport.
/// The projection zoom is its vertical half-span, so the slice area's length
/// follows zoom after applying the viewport aspect ratio.
pub(super) fn slice_visible_half_length(zoom: f64, screen: Size) -> f64 {
    let aspect = screen.0 as f64 / screen.1.max(1.0) as f64;
    (zoom * aspect).max(1.0e-4)
}

pub(super) struct BlockModelTransparencyTargets {
    pub(super) _accum_textures: Vec<wgpu::Texture>,
    pub(super) accum_views: Vec<wgpu::TextureView>,
    pub(super) transparency_fallback_bind_groups: Vec<wgpu::BindGroup>,
    pub(super) composite_bind_groups: Vec<wgpu::BindGroup>,
}

pub(super) struct BlockModelVolumeTarget {
    pub(super) _texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
    pub(super) params_buffer: wgpu::Buffer,
    pub(super) bind_group: wgpu::BindGroup,
    /// 1/8-resolution conservative ray entry depths from the beam pre-pass;
    /// sampled by the main raycast via `beam_bind_group` (group 3).
    pub(super) _beam_texture: wgpu::Texture,
    pub(super) beam_view: wgpu::TextureView,
    pub(super) beam_bind_group: wgpu::BindGroup,
}

impl<'a> Graphics<'a> {
    pub(crate) fn invalidate_geometry(&mut self) {
        self.geometry_dirty = true;
        self.overlay_dirty = true;
        self.invalidate_scene_bounds();
    }

    /// Drop every per-item GPU cache, for a project replacement.
    ///
    /// Project-owned item ids restart at 0 with each project, so the incoming
    /// project's first triangulation, block model, drill hole, point cloud, and
    /// raster all reuse the ids their predecessors had. These caches are keyed
    /// on those ids and only re-upload when they see a new id or a changed
    /// style, so without an explicit reset the previous project's geometry
    /// keeps rendering under the new project's items. The same applies to the
    /// design-object caches, whose fingerprints are built from object ids and
    /// revisions that likewise restart.
    pub(crate) fn clear_item_caches(&mut self) {
        self.triangulation_gpu.clear();
        self.block_model_gpu.clear();
        self.drill_hole_gpu = Default::default();
        self.point_cloud_gpu = Default::default();
        self.raster_gpu.clear();
        self.static_strokes = Default::default();
        self.design_point_gpu.clear();
        self.invalidate_geometry();
    }

    pub(crate) fn invalidate_scene_bounds(&mut self) {
        self.cached_bounds_document_revision = u64::MAX;
        self.cached_scene_bounds = None;
        self.cached_object_aabbs.clear();
    }

    pub(crate) fn invalidate_overlay(&mut self) {
        self.overlay_dirty = true;
    }

    pub(crate) fn gui_input(&mut self, event: &WindowEvent) -> egui_winit::EventResponse {
        let mut response = self.gui.handle_event(&self.window, event);
        // egui's generic `consumed` result can be false on a secondary-button
        // release because no widget is actively using that button. A visible
        // UI surface must still own both halves of a right click so it cannot
        // open the canvas context menu or complete a pending orbit gesture.
        if matches!(event, WindowEvent::MouseInput { button: MouseButton::Right, .. }) && self.gui.pointer_over_ui() {
            response.consumed = true;
        }
        response
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn queue_browser_paste(&mut self, text: String) {
        self.gui.queue_paste(text);
    }

    pub(crate) fn set_fly_mode_enabled(&mut self, enabled: bool) {
        if enabled == self.fly_mode_enabled {
            return;
        }

        if enabled {
            self.camera.sync_angles_from_forward();
            let target_distance = self.camera.position.distance(self.camera.target());
            self.camera.apply_angle_orientation(target_distance);
        } else if self.fly_mode_enabled
            && let Some((min, max)) = self.cached_scene_bounds
        {
            // Free flight can leave the focal point far from the actual model.
            // Orthographic cursor zoom then has to invert an f32 GPU matrix with
            // a huge translation, which creates a dead region around small world
            // coordinates. Re-anchor to the scene while preserving orientation
            // and the pre-fly orthographic scale.
            let center = self.exaggerate_point((min + max) * 0.5);
            self.camera.frame_keep_orientation(center, self.projection.zoom.max(1.0e-4));
        }
        self.fly_camera_controller.clear_input();
        self.projection.set_perspective(enabled);
        self.fly_mode_enabled = enabled;
        self.sync_cursor_grab();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn configure_camera_preferences(
        &mut self,
        plan_orbit_sensitivity: f64,
        plan_zoom_sensitivity: f64,
        plan_invert_vertical_look: bool,
        plan_invert_horizontal_look: bool,
        plan_zoom_towards_cursor: bool,
        fly_field_of_view_degrees: f64,
        fly_mouse_look_sensitivity: f64,
        fly_invert_vertical_look: bool,
        fly_invert_horizontal_look: bool,
        fly_near_clip_limit: f64,
        fly_max_clip_span: f64,
    ) {
        self.camera_controller.configure_preferences(
            plan_orbit_sensitivity,
            plan_zoom_sensitivity,
            plan_invert_vertical_look,
            plan_invert_horizontal_look,
            plan_zoom_towards_cursor,
        );
        self.fly_camera_controller
            .configure_preferences(fly_mouse_look_sensitivity, fly_invert_vertical_look, fly_invert_horizontal_look);
        self.projection.configure_perspective(fly_field_of_view_degrees, fly_near_clip_limit, fly_max_clip_span);
    }

    pub(crate) fn release_mouse_capture(&mut self) {
        self.mouse_pressed = None;
        self.camera_controller.end_orbit();
        self.orbit_marker = None;
        self.fly_camera_controller.clear_input();
        if let Some(slice) = self.slice_view.as_mut() {
            slice.input = SliceInputState::default();
        }
        self.sync_cursor_grab();
    }

    pub(crate) fn needs_continuous_redraw(&self) -> bool {
        self.camera_controller.has_pending_updates()
            || self.fly_camera_controller.has_pending_updates()
            || self.slice_view.as_ref().is_some_and(SliceViewState::has_pending_updates)
    }

    pub(crate) fn point_cloud_uploads_pending(&self) -> bool {
        self.point_cloud_gpu.has_pending_uploads()
    }

    /// Returns true while the user is actively panning or orbiting the camera
    /// (right-mouse drag). Callers can skip expensive per-frame work like snap
    /// queries during camera movement.
    pub(crate) fn is_camera_active(&self) -> bool {
        self.mouse_pressed == Some(MouseButton::Right)
    }

    /// Record that the user is interacting with the view right now (camera
    /// drag or window resize), starting the volume raycaster's low-quality
    /// cooldown.
    pub(super) fn mark_interaction(&mut self) {
        self.last_interaction = Some(Instant::now());
    }

    /// App-level hint that a camera interaction is about to start (a right
    /// press that will promote to an orbit drag after a few pixels of
    /// movement). Arming the low-quality window here means the frames rendered
    /// between the press and the promotion already use the reduced-resolution
    /// volume raycast, instead of stalling the start of the drag behind one or
    /// more full-quality frames of a large model. A quick right click costs
    /// only the standard cooldown before a full-quality frame is rendered.
    pub(crate) fn note_interaction_start(&mut self) {
        self.mark_interaction();
    }

    /// True while the view is being changed (orbit/pan/zoom or resize) or
    /// within a short cooldown afterwards. The cooldown guarantees the last
    /// reduced-quality frame is followed by a full-quality one once motion
    /// stops (see the redraw request in `render`), and covers window resizing,
    /// which does not flow through the camera-motion signals.
    pub(super) fn interaction_active(&self) -> bool {
        const COOLDOWN: Duration = Duration::from_millis(150);
        self.is_camera_active() || self.needs_continuous_redraw() || self.last_interaction.is_some_and(|when| when.elapsed() < COOLDOWN)
    }

    pub(crate) fn begin_right_orbit_drag(&mut self) {
        if !self.fly_mode_enabled {
            self.mouse_pressed = Some(MouseButton::Right);
        }
    }

    pub(crate) fn is_fly_camera_active(&self) -> bool {
        self.fly_mode_enabled && self.mouse_pressed == Some(MouseButton::Right)
    }

    pub(crate) fn should_receive_event_when_gui_consumed(&self, event: &WindowEvent) -> bool {
        // A press can reach the camera just before egui claims a widget drag,
        // while the matching release is then reported as GUI-consumed. Always
        // pass through the release for the button the camera recorded, or
        // `mouse_pressed` remains stuck and subsequent camera buttons are
        // rejected.
        if matches!(event, WindowEvent::MouseInput { state: ElementState::Released, button, .. } if self.mouse_pressed == Some(*button)) {
            return true;
        }

        if self.is_fly_camera_active() || (self.fly_mode_enabled && matches!(event, WindowEvent::MouseInput { button: MouseButton::Right, .. })) {
            return true;
        }

        // egui can continue reporting pointer input as consumed briefly after
        // a slider drag. Middle-button navigation is not used by the normal
        // 3D-view UI, so let its press and release reach the camera anyway.
        // Keep it isolated in slice view, where the embedded minimap owns
        // middle-drag; a release for an already-active camera drag must still
        // get through so the button cannot become stuck.
        matches!(event, WindowEvent::MouseInput { button: MouseButton::Middle, .. }) && (self.slice_view.is_none() || self.mouse_pressed == Some(MouseButton::Middle))
    }

    /// Enter the vertical slice viewing mode. `center` and `direction`
    /// describe the drawn slice line in world coordinates; the current camera
    /// and ortho zoom are saved and restored by `exit_slice_mode`. The caller
    /// must have already disabled fly mode.
    pub(crate) fn enter_slice_mode(&mut self, center_world: DVec3, direction: DVec2, half_length: f64, width: f64, move_speed: f64, rotate_speed: f64) {
        if self.slice_view.is_some() {
            return;
        }
        let saved_camera = self.camera.clone();
        let saved_zoom = self.projection.zoom;
        self.camera_controller.cancel_view_transition();
        self.camera_controller.end_orbit();
        self.orbit_marker = None;

        // Frame the drawn line: it spans the view horizontally with padding.
        let screen = self.screen_size();
        let aspect = (screen.0 as f64 / screen.1.max(1.0) as f64).max(1e-9);
        self.projection.zoom = (half_length.max(1.0) / aspect * 1.1).max(1.0e-4);

        let center = self.exaggerate_point(center_world);
        let slice = SliceViewState {
            center,
            direction: direction.normalize_or(DVec2::X),
            width,
            move_speed,
            rotate_speed,
            input: SliceInputState::default(),
            pan: DVec2::ZERO,
            scroll: 0.0,
            saved_camera,
            saved_zoom,
        };
        self.camera.look_to(slice.center, slice.forward(), DVec3::Z, self.projection.zoom);
        self.slice_view = Some(slice);
    }

    /// Leave slice mode and restore the camera saved on entry. The clip
    /// planes are refit to the scene on the next frame.
    pub(crate) fn exit_slice_mode(&mut self) {
        let Some(slice) = self.slice_view.take() else {
            return;
        };
        self.camera = slice.saved_camera;
        self.projection.zoom = slice.saved_zoom;
    }

    /// Forward a held-key press/release to the slice navigation input:
    /// W/S slab forward/back, Q/E rotate the line.
    pub(crate) fn slice_process_key(&mut self, key: winit::keyboard::KeyCode, pressed: bool) {
        let Some(slice) = self.slice_view.as_mut() else {
            return;
        };
        match key {
            winit::keyboard::KeyCode::KeyW => slice.input.forward = pressed,
            winit::keyboard::KeyCode::KeyS => slice.input.backward = pressed,
            winit::keyboard::KeyCode::KeyQ => slice.input.rotate_left = pressed,
            winit::keyboard::KeyCode::KeyE => slice.input.rotate_right = pressed,
            _ => {}
        }
    }

    pub(crate) fn release_slice_keys(&mut self) {
        if let Some(slice) = self.slice_view.as_mut() {
            slice.input = SliceInputState::default();
        }
    }

    /// Pin the pointer to the window while a fly-mode look is in progress.
    ///
    /// Only the grab is set here. Hiding the pointer is left to the UI, which
    /// asks for [`egui::CursorIcon::None`] while the camera is being flown:
    /// `Window::set_cursor_visible` from this side would leave egui-winit's
    /// icon cache believing something else is on screen, and the next thing
    /// egui hid or showed would be skipped as a no-op.
    fn sync_cursor_grab(&self) {
        let fly_active = self.fly_mode_enabled && self.mouse_pressed == Some(MouseButton::Right);
        if fly_active {
            if self.window.set_cursor_grab(CursorGrabMode::Locked).is_err() {
                let _ = self.window.set_cursor_grab(CursorGrabMode::Confined);
            }
        } else {
            let _ = self.window.set_cursor_grab(CursorGrabMode::None);
        }
    }
}
