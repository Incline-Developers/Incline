use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use glam::{DMat4, DQuat, DVec2, DVec3, DVec4};
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
        camera::{Camera, CameraController, CameraUniform, FlyCameraController, Projection, SectionSlab, screen_to_world_on_plane, screen_to_world_on_section_plane},
        pick::{PickGeometry, PickRecord, TextPickRecord, pick_nearest, pick_text},
        query::SceneQuery,
        scene::{
            BlockModelGpuCache, DesignPointGpuCache, DrillCollarInstance, DrillHoleGpuCache, DrillSegmentInstance, EdgeInstance, PointCloudGpuCache, PointInstance, PointPosition,
            RasterGpuCache, StaticStrokeCache, TriangulationGpuCache,
            bounds::{scene_bounds, visible_object_aabbs},
            build::{DocumentDrawBatch, DocumentPrimitive, DocumentRenderStage, PolylineFillCache, TextDrawBatch},
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
    pub(super) view: wgpu::TextureView,
    /// The cache bound for reading, for the blit that restores it into the
    /// multisample target at the start of an overlay-only frame.
    pub(super) bind_group: wgpu::BindGroup,
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

/// The section grid's shader inputs; see `section_grid.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SectionGridUniform {
    pub(super) params: [f32; 4],
    pub(super) phase: [f32; 4],
    pub(super) color: [f32; 4],
}

impl SectionGridUniform {
    fn new(
        scene_origin: DVec3,
        background: [f32; 4],
        axis: crate::ui::state::SectionGridAxis,
        axis_spacing: f64,
        elevation_spacing: f64,
        style: &crate::ui::state::SectionGridStyle,
        scale_factor: f64,
    ) -> Self {
        let rule_x = matches!(axis, crate::ui::state::SectionGridAxis::Easting);
        let axis_origin = if rule_x { scene_origin.x } else { scene_origin.y };
        let color = match style.color {
            Some(chosen) => crate::rendering::color::color32_to_rgba(chosen),
            None => {
                let luminance = crate::rendering::color::relative_luminance(background);
                let mut color = crate::rendering::color::rgb_bytes_to_linear_rgba(if luminance > 0.35 { [55, 60, 66] } else { [99, 106, 115] });
                color[3] = 0.45;
                color
            }
        };
        Self {
            params: [if rule_x { 1.0 } else { 0.0 }, axis_spacing as f32, elevation_spacing as f32, 0.0],
            phase: [
                (axis_origin / axis_spacing).fract() as f32,
                (scene_origin.z / elevation_spacing).fract() as f32,
                // Points to the physical pixels the shader measures in.
                (style.thickness * scale_factor) as f32,
                0.0,
            ],
            color,
        }
    }
}

impl GridUniform {
    #[allow(clippy::too_many_arguments)]
    fn new(
        scene_origin: DVec3,
        background: [f32; 4],
        camera: &Camera,
        projection: &Projection,
        vertical_exaggeration: f64,
        fly_mode_enabled: bool,
        style: &crate::ui::state::PlanGridStyle,
        scale_factor: f64,
    ) -> Self {
        fn color(rgb: [u8; 3], alpha: f32) -> [f32; 4] {
            let mut color = crate::rendering::color::rgb_bytes_to_linear_rgba(rgb);
            color[3] = alpha;
            color
        }

        let luminance = crate::rendering::color::relative_luminance(background);
        let (mut minor_color, mut major_color, x_axis_color, y_axis_color) = if luminance > 0.35 {
            (color([72, 77, 82], 0.28), color([55, 60, 66], 0.46), color([130, 62, 66], 0.78), color([67, 108, 57], 0.78))
        } else {
            (
                color([82, 88, 96], 0.30),
                color([99, 106, 115], 0.46),
                color([112, 55, 60], 0.82),
                color([65, 101, 55], 0.82),
            )
        };

        // A chosen colour takes the major lines as given and the minor lines
        // at the palette's minor-to-major opacity; the axes keep their own.
        if let Some(chosen) = style.color {
            let minor_ratio = minor_color[3] / major_color[3];
            major_color = crate::rendering::color::color32_to_rgba(chosen);
            minor_color = major_color;
            minor_color[3] *= minor_ratio;
        }
        // Points beyond the one-pixel width the grid has always had, in the
        // physical pixels the shader measures in.
        let extra_width = ((style.thickness - 1.0) * scale_factor).max(0.0);

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
            level_params: [grid_level as f32, if fly_mode_enabled { 0.0 } else { 1.0 }, extra_width as f32, 0.0],
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
    pub(super) section_grid_render_pipeline: wgpu::RenderPipeline,
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
    pub(super) section_grid_buffer: wgpu::Buffer,
    pub(super) section_grid_bind_group: wgpu::BindGroup,
    pub(super) msaa_color: wgpu::Texture,
    pub(super) msaa_view: wgpu::TextureView,
    pub(super) scene_cache: SceneCacheTarget,
    pub(super) scene_cache_key: Option<u64>,
    pub(super) scene_cache_blit_layout: wgpu::BindGroupLayout,
    pub(super) scene_cache_blit_pipeline: wgpu::RenderPipeline,
    /// The present mode this surface uses with vsync off, if it has one.
    no_vsync_present_mode: Option<wgpu::PresentMode>,
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
    pub(super) polyline_fill_cache: PolylineFillCache,
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
    /// Rides with the eye's foot on the plane (`set_eye`); Q/E turns about it.
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
    pub(super) walk: f64,
    pub(super) orbit: DVec2,
    /// Whether a right drag has passed the click/drag threshold; below it a
    /// right click still finishes the drawn polyline instead of moving the view.
    pub(super) orbit_dragging: bool,
    /// Orbit from square-on, radians: `yaw` about world Z, then `pitch` about
    /// the yawed screen-right axis; both zero is the entered view.
    pub(super) yaw: f64,
    pub(super) pitch: f64,
    /// Which side of the plane the camera is on (`true` = front, the entered
    /// side); `walk_direction` reads it to keep the walk direction correct after an orbit.
    pub(super) viewing_from_front: bool,
    /// Camera and ortho zoom to restore on exit.
    pub(super) saved_camera: Camera,
    pub(super) saved_zoom: f64,
    /// The eye's offset from `center`, along the normal only: the depth the
    /// slide onto the plane could not remove near edge-on, zero otherwise.
    pub(super) view_offset: DVec3,
    /// An in-flight turn to a standard view, eased over a moment.
    pub(super) turn_to: Option<SliceTurn>,
}

/// A section on its way to a standard view: the gizmo's counterpart to the
/// plan view's camera transition, held on the slice state because the section
/// camera is rebuilt from that state every frame - a camera transition would
/// simply be overwritten.
#[derive(Debug)]
pub(super) struct SliceTurn {
    /// Total turn of the section line, radians, and how much of it is spent.
    angle: f64,
    turned: f64,
    /// The orbit to unwind on the way: every standard view a section can be
    /// turned to is square-on, so both of these ease to zero.
    start_yaw: f64,
    start_pitch: f64,
    elapsed: Duration,
}

impl SliceViewState {
    /// Whether the section still has work in hand: navigation the next tick
    /// must consume, or a turn to finish. Drives the redraw loop, so a turn
    /// counts even though nothing is touching the mouse.
    pub(super) fn has_pending_updates(&self) -> bool {
        self.has_pending_input() || self.turn_to.is_some()
    }

    fn has_pending_input(&self) -> bool {
        self.input.any() || self.pan != DVec2::ZERO || self.scroll != 0.0 || self.orbit != DVec2::ZERO || self.walk != 0.0
    }

    /// Advance an in-flight turn: the section line's step for this tick and
    /// the yaw/pitch to hold. `None` when no turn is running - and navigation
    /// input takes over from one at once, as it does from the plan view's
    /// transition.
    pub(super) fn advance_turn(&mut self, dt: Duration) -> Option<(f64, f64, f64)> {
        if self.has_pending_input() {
            self.turn_to = None;
        }
        let turn = self.turn_to.as_mut()?;
        turn.elapsed += dt;
        let linear = (turn.elapsed.as_secs_f64() / crate::rendering::camera::VIEW_TRANSITION_DURATION.as_secs_f64()).clamp(0.0, 1.0);
        let eased = crate::rendering::camera::ease_out_cubic(linear);
        // The line is turned by steps rather than set outright, so each step goes through `turn` and keeps the eye and a fixed centre with it.
        let step = turn.angle * eased - turn.turned;
        turn.turned += step;
        let (start_yaw, start_pitch) = (turn.start_yaw, turn.start_pitch);
        if linear >= 1.0 {
            self.turn_to = None;
        }
        Some((step, start_yaw * (1.0 - eased), start_pitch * (1.0 - eased)))
    }

    pub(super) fn slab(&self) -> SectionSlab {
        SectionSlab {
            point: self.center,
            normal: self.normal(),
            half_width: self.width * 0.5,
        }
    }

    /// View direction of the section (the slab normal), horizontal by
    /// construction. Chosen so that screen-right equals `+direction`.
    pub(super) fn normal(&self) -> DVec3 {
        slice_view_forward(self.direction)
    }

    /// The eye: `center` plus `view_offset`.
    pub(super) fn camera_position(&self) -> DVec3 {
        self.center + self.view_offset
    }

    /// Place the eye: the anchor takes its foot, the offset keeps its depth.
    pub(super) fn set_eye(&mut self, eye: DVec3) {
        let normal = self.normal();
        let offset = eye - self.center;
        let depth = offset.dot(normal);
        self.center += offset - normal * depth;
        self.view_offset = normal * depth;
    }

    pub(super) fn camera_basis(&self) -> (DVec3, DVec3, DVec3) {
        camera_frame(self.normal(), self.yaw, self.pitch)
    }

    /// Turn the section line `angle` radians about `fixed_centre` when one is
    /// set, else about the anchor, carrying the eye with it. A rigid turn
    /// about a vertical axis, so a fixed centre keeps its pixel through it.
    pub(super) fn turn(&mut self, angle: f64, fixed_centre: Option<DVec3>) {
        let turn = DVec2::from_angle(angle);
        self.direction = turn.rotate(self.direction).normalize_or(self.direction);
        if let Some(centre) = fixed_centre {
            let arm = turn.rotate((self.center - centre).truncate());
            self.center = DVec3::new(centre.x + arm.x, centre.y + arm.y, self.center.z);
        }
        let eye = turn.rotate(self.view_offset.truncate());
        self.view_offset = DVec3::new(eye.x, eye.y, self.view_offset.z);
    }

    pub(super) fn update_viewing_side(&mut self, forward: DVec3) {
        self.viewing_from_front = settled_viewing_side(self.viewing_from_front, forward.dot(self.normal()));
    }
}

/// Camera basis for a section `normal`, yawed about world Z then pitched
/// about the resulting screen-right; right stays horizontal so the frame never collapses looking straight down.
fn camera_frame(normal: DVec3, yaw: f64, pitch: f64) -> (DVec3, DVec3, DVec3) {
    let yawed = DQuat::from_rotation_z(yaw) * normal;
    let right = yawed.cross(DVec3::Z).normalize_or(DVec3::X);
    let forward = DQuat::from_axis_angle(right, pitch) * yawed;
    (forward, right, right.cross(forward).normalize_or(DVec3::Z))
}

/// Hysteresis around edge-on, in degrees: below this margin the camera's
/// recorded side of the plane doesn't flip, since exactly edge-on the sign is arbitrary and would flicker under a barely-moving hand.
const SIDE_FLIP_MARGIN_DEGREES: f64 = 5.0;

fn settled_viewing_side(was_front: bool, incidence: f64) -> bool {
    if incidence.abs() > SIDE_FLIP_MARGIN_DEGREES.to_radians().sin() {
        incidence > 0.0
    } else {
        was_front
    }
}

/// Direction a forward walk moves the plane, away from the eye: `normal`
/// from the front side, `-normal` from the back.
fn walk_direction(normal: DVec3, viewing_from_front: bool) -> DVec3 {
    if viewing_from_front { normal } else { -normal }
}

/// Wheel delta as `(x, y)` pixels; one notch is 100 px on either axis.
fn axis_pixels(delta: &MouseScrollDelta) -> (f64, f64) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 100.0, f64::from(*y) * 100.0),
        MouseScrollDelta::PixelDelta(position) => (position.x, position.y),
    }
}

pub(crate) fn scroll_pixels(delta: &MouseScrollDelta) -> f64 {
    axis_pixels(delta).1
}

/// Wheel delta in pixels, preferring `y` like `scroll_pixels`, but falling
/// back to `x`: Shift+wheel arrives as a horizontal delta (`y` zero) on Chrome and on macOS.
pub(crate) fn scroll_pixels_either_axis(delta: &MouseScrollDelta) -> f64 {
    let (x, y) = axis_pixels(delta);
    if y != 0.0 { y } else { x }
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

/// Bound the infinite section slab over the scene's display-space extents.
/// The overview's visible line length must not limit depth: orbiting can bring
/// geometry farther along the section into view even at a very small zoom.
fn slice_depth_half_extent(center: DVec3, strike: DVec3, forward: DVec3, half_width: f64, bounds: Option<(DVec3, DVec3)>) -> f64 {
    let mut tilt_depth: f64 = 0.0;
    if let Some((min, max)) = bounds {
        for i in 0..8 {
            let corner = DVec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            );
            let delta = corner - center;
            let depth = delta.dot(strike) * strike.dot(forward) + delta.z * forward.z;
            tilt_depth = tilt_depth.max(depth.abs());
        }
    }
    half_width + tilt_depth + 1.0
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
            // Drop the armed orbit: otherwise the next pointer move would rotate the restored view.
            slice.orbit_dragging = false;
            slice.orbit = DVec2::ZERO;
        }
        self.sync_cursor_grab();
    }

    /// Whether this surface can present without waiting for the display.
    pub(crate) fn supports_vsync_off(&self) -> bool {
        self.no_vsync_present_mode.is_some()
    }

    /// Present in step with the display, or as fast as frames are produced.
    ///
    /// Only the surface configuration changes, so unlike a resize this keeps
    /// every attachment and cache; a no-op when the mode is already the one
    /// asked for, which is what makes it safe to call on every preference
    /// commit.
    pub(crate) fn set_vsync_enabled(&mut self, enabled: bool) {
        let mode = if enabled {
            wgpu::PresentMode::Fifo
        } else {
            self.no_vsync_present_mode.unwrap_or(wgpu::PresentMode::Fifo)
        };
        if self.config.present_mode == mode {
            return;
        }
        self.config.present_mode = mode;
        self.surface.configure(&self.device, &self.config);
        crate::userspace_log!("{}", crate::i18n::tr_format!(literal = "Surface presentation mode: %mode%", mode = format!("{mode:?}")));
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
            walk: 0.0,
            orbit: DVec2::ZERO,
            orbit_dragging: false,
            yaw: 0.0,
            pitch: 0.0,
            viewing_from_front: true,
            saved_camera,
            saved_zoom,
            view_offset: DVec3::ZERO,
            turn_to: None,
        };
        let (forward, _, up) = slice.camera_basis();
        self.camera.look_to(slice.center, forward, up, self.projection.zoom);
        self.slice_view = Some(slice);
    }

    pub(crate) fn slice_view_moving(&self) -> bool {
        self.slice_view.as_ref().is_some_and(SliceViewState::has_pending_updates)
    }

    /// Leave slice mode and restore the camera saved on entry. The clip
    /// planes are refit to the scene on the next frame.
    pub(crate) fn exit_slice_mode(&mut self) {
        let Some(slice) = self.slice_view.take() else {
            return;
        };
        self.camera = slice.saved_camera;
        self.projection.zoom = slice.saved_zoom;
        if self.mouse_pressed == Some(MouseButton::Right) && !self.fly_mode_enabled {
            self.mouse_pressed = None;
        }
    }

    pub(crate) fn begin_slice_orbit_drag(&mut self, initial: DVec2) -> bool {
        let Some(slice) = self.slice_view.as_mut() else {
            return false;
        };
        slice.orbit_dragging = true;
        slice.orbit += initial;
        self.begin_right_orbit_drag();
        true
    }

    pub(crate) fn slice_walk_scroll(&mut self, delta: &MouseScrollDelta) -> bool {
        let Some(slice) = self.slice_view.as_mut() else {
            return false;
        };
        slice.walk += scroll_pixels_either_axis(delta);
        self.mark_interaction();
        true
    }

    pub(crate) fn section_slab(&self) -> Option<SectionSlab> {
        self.slice_view.as_ref().map(SliceViewState::slab)
    }

    /// Turn the section camera to a standard view: the section line itself
    /// turns, so the camera ends square-on looking along that axis - the turn
    /// Q/E make, taken in one eased step.
    ///
    /// Straight up and down are refused. A vertical section cannot be turned
    /// to face them, and a view that grazes the plane has no point under the
    /// cursor, so the gizmo drops those arms while sliced rather than offer a
    /// turn that lands nowhere.
    pub(crate) fn set_slice_standard_view(&mut self, view: crate::ui::state::StandardView) -> bool {
        let (forward, _) = camera::standard_view_basis(view);
        let Some(slice) = self.slice_view.as_mut() else {
            return false;
        };
        let Some(target) = forward.truncate().try_normalize() else {
            return false;
        };
        // The turn runs on the update tick, which reads the live rotation centre; the shortest way round, since `angle_to` never exceeds half a turn.
        slice.turn_to = Some(SliceTurn {
            angle: slice.normal().truncate().angle_to(target),
            turned: 0.0,
            start_yaw: slice.yaw,
            start_pitch: slice.pitch,
            elapsed: Duration::ZERO,
        });
        true
    }

    /// Squares the section camera to its plane; undoes only the orbit,
    /// leaving direction, slab position, pan and zoom untouched.
    pub(crate) fn reset_slice_view(&mut self, rotation_centre: Option<DVec3>) -> bool {
        let fixed_centre = rotation_centre.map(|centre| self.exaggerate_point(centre));
        let Some(slice) = self.slice_view.as_mut() else {
            return false;
        };
        // Squaring up outright takes over from a turn still in flight.
        slice.turn_to = None;
        // Squaring up is a turn like any other: a fixed centre keeps its pixel through it.
        let anchored = fixed_centre.map(|centre| (centre, camera::eye_offset_from(centre, slice.camera_position(), slice.camera_basis())));
        slice.yaw = 0.0;
        slice.pitch = 0.0;
        slice.orbit = DVec2::ZERO;
        slice.orbit_dragging = false;
        slice.viewing_from_front = true;
        let (forward, right, up) = slice.camera_basis();
        let eye = match anchored {
            Some((centre, (screen_x, screen_y, depth))) => camera::eye_keeping_centre(centre, screen_x, screen_y, depth, (forward, right, up)),
            None => slice.camera_position(),
        };
        // Square-on, the view meets the plane head-on, so the eye always lands back on it.
        slice.set_eye(camera::slide_onto_plane(eye, slice.center, forward, slice.normal()));
        self.camera.look_to(slice.camera_position(), forward, up, self.projection.zoom.max(1.0));
        true
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
