//! Editor state, UI commands, and project view data types.
//!
//! `EditorState` is the central mutable state struct shared between the
//! rendering pipeline and every UI draw call.  `UiCommand` carries actions
//! back to the application core; `UiProjectView` is a flattened snapshot
//! of the project tree built each frame by the app layer.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use glam::DVec3;
use strum::{Display, EnumIter};

use crate::{
    i18n::tr,
    logging::CommandReportSpec,
    model::{
        Axis, FillStyle, LayerId, ObjectColor, ObjectId, ObjectPoint, SceneEntityId,
        block_model::{BlockModelId, ColorTransferFunction, FIRST_CUSTOM_COLOR_STOP_ID},
        drill_hole::{DrillCategoryColor, DrillColorPreset, DrillColorStop, DrillHoleId, DrillHoleRef, DrillHoleSource},
        formats::{
            MeshFormat,
            csv_block_model::{CsvColumnMapping, CsvPreview},
            csv_drill_hole::{CsvDrillFileMapping, CsvDrillPreview},
        },
        point_cloud::PointCloudId,
        raster::RasterTextureId,
        triangulation::TriangulationId,
    },
};

type OptionalScreenPointPx = Option<(f32, f32)>;

/// Unsaved preference values currently being edited in the Preferences window.
///
/// When the user clicks "Save Changes" these values are applied to `EditorState`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreferencesDraft {
    /// UI language. Not edited in the Preferences panel: the status bar's
    /// picker sends [`UiCommand::SetLanguage`], which comes through here so the
    /// language is saved with everything else - see [`crate::i18n`].
    pub(crate) language: crate::i18n::LanguageChoice,
    pub(crate) renderer_background_color: [f32; 4],
    pub(crate) dark_mode: bool,
    pub(crate) show_console: bool,
    pub(crate) panel_chrome: bool,
    pub(crate) show_world_axis_gizmo: bool,
    pub(crate) show_xy_grid: bool,
    pub(crate) show_scale_bar: bool,
    pub(crate) snap_poll_rate: u32,
    pub(crate) frame_rate_cap: u32,
    pub(crate) resize_frame_rate_cap: u32,
    pub(crate) block_model_interaction_resolution_divisor: u32,
    pub(crate) show_block_model_boundary_highlights: bool,
    pub(crate) downscale_raster_previews: bool,
    pub(crate) frame_counter_enabled: bool,
    pub(crate) debug_chunk_coloring: bool,
    pub(crate) debug_clip_planes: bool,
    pub(crate) plan_orbit_sensitivity: f64,
    pub(crate) plan_zoom_sensitivity: f64,
    pub(crate) plan_invert_vertical_look: bool,
    pub(crate) plan_invert_horizontal_look: bool,
    pub(crate) plan_zoom_towards_cursor: bool,
    pub(crate) fly_field_of_view_degrees: f64,
    pub(crate) fly_mouse_look_sensitivity: f64,
    pub(crate) fly_invert_vertical_look: bool,
    pub(crate) fly_invert_horizontal_look: bool,
    pub(crate) fly_near_clip_limit: f64,
    pub(crate) fly_max_clip_span: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MoveToLayerDialog {
    pub(crate) object_ids: Vec<ObjectId>,
    pub(crate) target_layer: Option<LayerId>,
    pub(crate) copy: bool,
}

impl Default for PreferencesDraft {
    fn default() -> Self {
        Self {
            language: crate::app::io::default_language(),
            renderer_background_color: crate::app::io::default_renderer_background_color(),
            dark_mode: crate::app::io::default_dark_mode(),
            show_console: crate::app::io::default_show_console(),
            panel_chrome: crate::app::io::default_panel_chrome(),
            show_world_axis_gizmo: crate::app::io::default_show_world_axis_gizmo(),
            show_xy_grid: crate::app::io::default_show_xy_grid(),
            show_scale_bar: crate::app::io::default_show_scale_bar(),
            snap_poll_rate: crate::app::io::default_snap_poll_rate(),
            frame_rate_cap: crate::app::io::default_frame_rate_cap(),
            resize_frame_rate_cap: crate::app::io::default_resize_frame_rate_cap(),
            block_model_interaction_resolution_divisor: crate::app::io::default_block_model_interaction_resolution_divisor(),
            show_block_model_boundary_highlights: crate::app::io::default_show_block_model_boundary_highlights(),
            downscale_raster_previews: crate::app::io::default_downscale_raster_previews(),
            frame_counter_enabled: false,
            debug_chunk_coloring: false,
            debug_clip_planes: false,
            plan_orbit_sensitivity: crate::app::io::default_plan_orbit_sensitivity(),
            plan_zoom_sensitivity: crate::app::io::default_plan_zoom_sensitivity(),
            plan_invert_vertical_look: false,
            plan_invert_horizontal_look: false,
            plan_zoom_towards_cursor: crate::app::io::default_plan_zoom_towards_cursor(),
            fly_field_of_view_degrees: crate::app::io::default_fly_field_of_view_degrees(),
            fly_mouse_look_sensitivity: crate::app::io::default_fly_mouse_look_sensitivity(),
            fly_invert_vertical_look: false,
            fly_invert_horizontal_look: false,
            fly_near_clip_limit: crate::app::io::default_fly_near_clip_limit(),
            fly_max_clip_span: crate::app::io::default_fly_max_clip_span(),
        }
    }
}

impl EditorState {
    /// Set (or clear) the status-bar message. Whenever the displayed task
    /// changes, the outgoing one is remembered as `last_finished_task` so the
    /// idle bar keeps reading "<task>: Finished" at 100%.
    pub(crate) fn set_status_message(&mut self, message: Option<StatusBarMessage>) {
        if let Some(previous) = &self.status_message
            && message.as_ref().is_none_or(|next| next.text != previous.text)
        {
            self.last_finished_task = Some(FinishedTask {
                // Labels are written as "Generating contours…"; drop the trailing
                // ellipsis so the idle text isn't "Generating contours…: Finished".
                text: previous.text.trim_end_matches(['.', '…', ' ']).to_owned(),
                // Progress reports are throttled, so the last one received can
                // be short of the total even though the task ran to completion.
                // Only the total is kept; the idle bar shows it as "y of y".
                total_units: previous.units.map(|(_done, total)| total),
            });
        }
        self.status_message = message;
    }

    /// Drop everything selected, in every workspace's terms: whole scene
    /// entities and the individual drill holes Drill & Blast picks.
    pub(crate) fn clear_scene_selection(&mut self) {
        self.selected_handles.clear();
        self.selected_drill_holes.clear();
    }

    fn replace_selection(&mut self, handle: SceneEntityId) {
        self.clear_scene_selection();
        self.selected_handles.insert(handle);
    }

    fn add_selection(&mut self, handle: SceneEntityId) {
        self.selected_handles.insert(handle);
    }

    fn toggle_selection(&mut self, handle: SceneEntityId) {
        if !self.selected_handles.remove(&handle) {
            self.selected_handles.insert(handle);
        }
    }

    /// Lock or unlock one scene entity by name. Layer locks go through
    /// [`Self::locked_layers`] instead, and both are folded into
    /// `frozen_handles` by `App::invalidate_geometry`.
    pub(crate) fn set_entity_locked(&mut self, handle: SceneEntityId, locked: bool) {
        if locked {
            self.explicitly_frozen.insert(handle);
            self.frozen_handles.insert(handle);
            self.selected_handles.remove(&handle);
            if let SceneEntityId::DrillHole(dataset) = handle {
                self.selected_drill_holes.retain(|hole| hole.dataset != dataset);
            }
        } else {
            self.explicitly_frozen.remove(&handle);
            self.frozen_handles.remove(&handle);
        }
    }

    /// Order-independent fingerprint of every editor field baked into the
    /// static document geometry (selection colour, hidden/frozen skips,
    /// translucency, tool highlights).
    ///
    /// `Graphics::render` compares this itself each frame, so selection and
    /// project transitions rebuild the scene even when the mutation site only
    /// requested an overlay refresh - the class of stale-geometry bug this
    /// removes cannot depend on callers picking the right invalidation.
    pub(crate) fn render_style_key(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        fn set_key(handles: &HashSet<SceneEntityId>) -> u64 {
            // XOR-fold per-element hashes: HashSet iteration order is
            // unstable, so the combination must be commutative.
            handles.iter().fold(handles.len() as u64, |acc, handle| {
                let mut hasher = DefaultHasher::new();
                handle.hash(&mut hasher);
                acc ^ hasher.finish()
            })
        }
        let mut hasher = DefaultHasher::new();
        set_key(&self.selected_handles).hash(&mut hasher);
        set_key(&self.hidden_handles).hash(&mut hasher);
        set_key(&self.frozen_handles).hash(&mut hasher);
        set_key(&self.translucent_handles).hash(&mut hasher);
        set_key(&self.tri_hover_handles).hash(&mut hasher);
        self.tool_highlight_id.hash(&mut hasher);
        self.editing_labels_id.hash(&mut hasher);
        hasher.finish()
    }
}

/// What the user-entered value means for an angled offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OffsetMeasure {
    /// Direct horizontal (XY-plane) distance.
    Distance,
    /// The Z component (height).
    Height(HeightMode),
    /// Same as Distance - kept for naming clarity in the UI.
    Width,
}

/// Whether a height value is a relative delta or an absolute reduced level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HeightMode {
    Relative,
    AbsoluteRL,
}

/// Surface/solid type for a created triangulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TriSurfaceType {
    /// Plain open surface
    Surface,
    /// Fully closed solid
    SolidClosed,
}

/// Which side of a reference topology to remove from a surface being trimmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TriSurfaceCutSide {
    /// Remove geometry above the reference topology; keep geometry below it.
    CutTop,
    /// Remove geometry below the reference topology; keep geometry above it.
    CutBottom,
}

/// Which part of a surface to retain when clipping it with an XY polyline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TriPolylineClipMode {
    #[default]
    KeepInside,
    KeepOutside,
}

/// Destination for generated contour polylines in the active project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ContourOutputLayer {
    New(String),
    Existing(LayerId),
}

/// A triangulation selector temporarily being filled from a viewport click.
/// Keeping one shared mode prevents overlapping dialogs from competing for a
/// click and lets Escape cancel the pick without closing the owning tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TriangulationPickTarget {
    ClipSurface,
    SliceSurface,
    TrimTopology,
    TrimSurface,
    CutPitTopology,
    CutPitShell,
    IncludeTopology,
    IncludeShape,
    ContourSurface,
}

impl TriangulationPickTarget {
    pub(crate) fn prompt(self) -> &'static str {
        match self {
            Self::TrimTopology | Self::CutPitTopology | Self::IncludeTopology => "Click the topology in the viewport.",
            Self::CutPitShell => "Click the pit shell in the viewport.",
            Self::IncludeShape => "Click the pit or stockpile solid in the viewport.",
            Self::ClipSurface | Self::SliceSurface | Self::TrimSurface | Self::ContourSurface => "Click the surface in the viewport.",
        }
    }
}

const SLICE_PREVIEW_DEFAULT_WIDTH_POINTS: f64 = 220.0;
const SLICE_PREVIEW_DEFAULT_FILL: f64 = 0.72;
const SLICE_PREVIEW_ZOOM_SENSITIVITY: f64 = 0.005;
const SLICE_PREVIEW_MIN_ZOOM_MULTIPLIER: f64 = 0.01;
const SLICE_PREVIEW_MAX_ZOOM_MULTIPLIER: f64 = 100.0;

/// Independent plan-camera navigation for the vertical-slice overview.
///
/// This state deliberately stores only an XY camera offset and zoom scale. It
/// never mutates the slice centre, direction, width, or line length used by
/// the section view itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SlicePreviewNavigation {
    center_offset_xy: [f64; 2],
    zoom_multiplier: f64,
}

impl Default for SlicePreviewNavigation {
    fn default() -> Self {
        Self {
            center_offset_xy: [0.0; 2],
            zoom_multiplier: 1.0,
        }
    }
}

impl SlicePreviewNavigation {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn center_offset_xy(self) -> [f64; 2] {
        self.center_offset_xy
    }

    pub(crate) fn zoom_multiplier(self) -> f64 {
        self.zoom_multiplier
    }

    /// Pan in physical pixels. The map follows the pointer, matching the main
    /// plan viewport's middle-drag behaviour.
    pub(crate) fn pan_by_pixels(&mut self, delta_px: [f64; 2], current_zoom: f64, viewport_height_px: f64) -> bool {
        if !delta_px.iter().all(|value| value.is_finite()) || !current_zoom.is_finite() || current_zoom <= 0.0 || !viewport_height_px.is_finite() || viewport_height_px <= 0.0 {
            return false;
        }
        let world_per_pixel = 2.0 * current_zoom / viewport_height_px;
        self.center_offset_xy[0] -= delta_px[0] * world_per_pixel;
        self.center_offset_xy[1] += delta_px[1] * world_per_pixel;
        delta_px != [0.0; 2]
    }

    /// Apply a wheel delta while keeping the world point under `cursor_px`
    /// stationary. Positive scroll zooms in, matching the main plan camera.
    pub(crate) fn zoom_at_pixel(&mut self, scroll: f64, cursor_px: [f64; 2], viewport_px: [f64; 2], fitted_zoom: f64) -> bool {
        if !scroll.is_finite()
            || scroll == 0.0
            || !cursor_px.iter().all(|value| value.is_finite())
            || !viewport_px.iter().all(|value| value.is_finite())
            || viewport_px[0] <= 0.0
            || viewport_px[1] <= 0.0
            || !fitted_zoom.is_finite()
            || fitted_zoom <= 0.0
        {
            return false;
        }

        let scale = if scroll > 0.0 {
            1.0 / (1.0 + SLICE_PREVIEW_ZOOM_SENSITIVITY * scroll)
        } else {
            1.0 - SLICE_PREVIEW_ZOOM_SENSITIVITY * scroll
        };
        let old_multiplier = self.zoom_multiplier;
        let new_multiplier = (old_multiplier * scale).clamp(SLICE_PREVIEW_MIN_ZOOM_MULTIPLIER, SLICE_PREVIEW_MAX_ZOOM_MULTIPLIER);
        if new_multiplier == old_multiplier {
            return false;
        }

        let old_zoom = fitted_zoom * old_multiplier;
        let new_zoom = fitted_zoom * new_multiplier;
        let ndc_x = -1.0 + 2.0 * cursor_px[0] / viewport_px[0];
        let ndc_y = 1.0 - 2.0 * cursor_px[1] / viewport_px[1];
        let aspect = viewport_px[0] / viewport_px[1];
        let zoom_delta = old_zoom - new_zoom;
        self.center_offset_xy[0] += ndc_x * aspect * zoom_delta;
        self.center_offset_xy[1] += ndc_y * zoom_delta;
        self.zoom_multiplier = new_multiplier;
        true
    }
}

/// Fitted half-height for the slice overview's stable logical-point scale.
/// Increasing a preview dimension reveals more terrain instead of stretching
/// the same fitted extent.
pub(crate) fn fitted_slice_preview_zoom(slice_half_length: f64, viewport_height_px: u32, scale_factor: f64) -> f64 {
    let half_length = slice_half_length.max(1.0);
    let world_per_point = (half_length * 2.0) / (SLICE_PREVIEW_DEFAULT_WIDTH_POINTS * SLICE_PREVIEW_DEFAULT_FILL);
    let logical_height = f64::from(viewport_height_px.max(1)) / scale_factor.max(1.0e-6);
    (world_per_point * logical_height * 0.5).max(1.0e-4)
}

impl TriPolylineClipMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::KeepInside => "Keep inside",
            Self::KeepOutside => "Keep outside",
        }
    }
}

impl TriSurfaceCutSide {
    /// User-facing result label. These describe the side the output retains,
    /// which is less ambiguous than the historical "cut top/bottom" wording.
    pub(crate) fn trim_label(self) -> &'static str {
        match self {
            Self::CutTop => "Trim below",
            Self::CutBottom => "Trim above",
        }
    }

    pub(crate) fn retained_relation(self) -> &'static str {
        match self {
            Self::CutTop => "at or below",
            Self::CutBottom => "at or above",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OreFilterMode {
    GreaterOrEqual,
    LessOrEqual,
    Between,
}

/// Phase of the Create Triangulation workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TriCreatePhase {
    MainDialog,
}

/// A failed Create Triangulation run, retained for the failure dialog.
#[derive(Clone, Debug)]
pub(crate) struct TriCreateFailure {
    pub(crate) message: String,
    pub(crate) name: String,
    pub(crate) object_ids: Vec<ObjectId>,
    pub(crate) surface_type: TriSurfaceType,
    /// True when welding endpoints at the coarse tolerance would change the
    /// input, i.e. a retry is worth offering.
    pub(crate) weld_retry_available: bool,
    /// True when an open terrain surface can be retried by retaining the
    /// higher of conflicting breakline edges.
    pub(crate) upper_surface_retry_available: bool,
    /// Whether this failed attempt already included the optional coarse weld.
    /// An upper-surface retry must preserve that weld or it can regress to the
    /// preceding near-vertex failure.
    pub(crate) coarse_weld_applied: bool,
    /// Exact failed-input geometry to highlight while this failure is open.
    pub(crate) diagnostic: TriCreateDiagnostic,
}

/// World-space geometry associated with a Create Triangulation failure.
/// Keeping this authoritative lets camera changes reproject the highlight
/// without rerunning or guessing from the human-readable error message.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TriCreateDiagnostic {
    pub(crate) markers_world: Vec<DVec3>,
    pub(crate) segments_world: Vec<[DVec3; 2]>,
}

/// Whether the Batter Berm tool is generating a pit or a stockpile. Combined
/// with [`BatterBermPreviewKey::direction_up`] this picks the horizontal
/// offset side: Pit + Up and Stockpile + Down step outward; Pit + Down and
/// Stockpile + Up step inward. The Direction selector alone sets the vertical
/// step (Up rises, Down falls).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BatterBermMode {
    Pit,
    Stockpile,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BatterBermPreviewKey {
    pub(crate) target_id: ObjectId,
    pub(crate) document_revision: u64,
    pub(crate) width: f64,
    pub(crate) angle: f64,
    pub(crate) bench_height: f64,
    pub(crate) benches: u32,
    pub(crate) mode: BatterBermMode,
    /// `true` = each bench rises (Direction: Up); `false` = each bench falls
    /// (Direction: Down).
    pub(crate) direction_up: bool,
}

/// Which sub-mode the Relimit Line tool is operating in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RelimitMode {
    Intersect,
    AbsoluteLength,
    RelativeLength,
}

/// Which endpoint of a line the Relimit tool will move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TrimEnd {
    Start,
    End,
}

/// Named orientations selectable from the orientation gizmo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StandardView {
    Up,
    Down,
    North,
    South,
    West,
    East,
}

/// One candidate relimit operation, stored entirely in world space so its
/// screen-space hit region and preview can be reprojected every frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RelimitCandidate {
    /// Which source endpoint this operation moves.
    pub(crate) end: TrimEnd,
    /// World-space destination for that endpoint.
    pub(crate) target: DVec3,
    /// True if the line grows (extend, yellow); false if it shrinks (trim, red).
    pub(crate) is_extension: bool,
}

/// One segment in a Fuse-into-polyline chain.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FuseSegment {
    pub(crate) object_id: ObjectId,
    /// When true, the segment's vertices are consumed end→start instead of start→end.
    pub(crate) reversed: bool,
    /// Vertex at which a closed polyline is opened for insertion into the chain.
    pub(crate) start_index: usize,
    pub(crate) closed: bool,
    /// When true, the segment's first ordered vertex was designated as the
    /// join with the chain tail and sits (visually) on it, so commit drops it
    /// instead of keeping a doubled point / micro edge.
    pub(crate) weld_start: bool,
}

/// A transient message shown in the bottom status bar. Generic shape so any
/// background task (save, contour generation, etc.) can drive the same UI slot
/// without per-task plumbing.
#[derive(Clone, Debug)]
pub(crate) struct StatusBarMessage {
    pub(crate) text: String,
    /// `Some(f)` in `0.0..=1.0` fills the bar to that fraction and labels it
    /// with the percentage; `None` is indeterminate (marquee, no percentage).
    pub(crate) progress: Option<f32>,
    /// `Some((done, total))` when the task counts discrete items (mesh
    /// vertices/faces), appending "(x of y)" to the percentage.
    pub(crate) units: Option<(u64, u64)>,
}

/// The most recent task to leave the status bar. Retained so the bar stays
/// visible at 100% once a task ends, instead of blanking out.
#[derive(Clone, Debug)]
pub(crate) struct FinishedTask {
    /// Task label without its trailing ellipsis, e.g. "Generating contours".
    pub(crate) text: String,
    /// Item count the task worked through, shown as "(y of y)" when known.
    pub(crate) total_units: Option<u64>,
}

/// The plane through the three picked points, as the Strike and Dip tool
/// reports it.
///
/// Dip and strike are read off the plane itself rather than off the section
/// the picks happen to cut, so the pair is the plane's own attitude however
/// the three points were placed. Picking a level crest line and a toe point -
/// the way a bench face is measured - gives the batter angle as the dip, which
/// is what this measured before it was named for the plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BatterAngleMeasurement {
    /// Dip: the plane's steepest slope, in degrees below horizontal.
    pub(crate) dip_degrees: f64,
    /// Strike: the bearing of the plane's horizontal line, in degrees
    /// clockwise from grid north, by the right-hand rule - the dip falls 90°
    /// clockwise of it. `None` for a horizontal plane, which has no strike.
    pub(crate) strike_degrees: Option<f64>,
    /// Where the third point sits against the line through the first two, in
    /// plan. The overlay draws the measurement out to here.
    pub(crate) projection: DVec3,
}

pub(crate) fn batter_angle_measurement(points: &[DVec3]) -> Option<BatterAngleMeasurement> {
    let [a, b, c] = points.get(..3)? else {
        return None;
    };
    let ab_xy = b.truncate() - a.truncate();
    let ab_len_sq = ab_xy.length_squared();
    if ab_len_sq <= 1.0e-12 {
        return None;
    }

    let ac_xy = c.truncate() - a.truncate();
    let t = ac_xy.dot(ab_xy) / ab_len_sq;
    let projection_xy = a.truncate() + ab_xy * t;
    let projection_z = a.z + (b.z - a.z) * t;
    let projection = DVec3::new(projection_xy.x, projection_xy.y, projection_z);
    let horizontal = c.truncate().distance(projection_xy);
    let vertical = (c.z - projection_z).abs();
    if horizontal <= 1.0e-9 && vertical <= 1.0e-9 {
        return None;
    }

    // Turned upward, so its horizontal part points down the dip: the plane is
    // z = z0 - (n.x·dx + n.y·dy) / n.z, whose steepest descent runs along
    // (n.x, n.y) once n.z is positive.
    let mut normal = (*b - *a).cross(*c - *a);
    if normal.z < 0.0 {
        normal = -normal;
    }
    let down_dip = normal.truncate();
    let dip_degrees = down_dip.length().atan2(normal.z.abs()).to_degrees();
    // A horizontal plane dips nowhere, so it has no line of strike to name.
    let strike_degrees = (down_dip.length() > 1.0e-12).then(|| {
        // Bearings run clockwise from grid north, which is +Y: east over
        // north, rather than the north-over-east of a mathematical angle.
        let dip_direction = down_dip.x.atan2(down_dip.y).to_degrees();
        (dip_direction - 90.0).rem_euclid(360.0)
    });

    Some(BatterAngleMeasurement {
        dip_degrees,
        strike_degrees,
        projection,
    })
}

pub(crate) const CIRCLE_TYPED_RESET_DISTANCE_POINTS: f32 = 8.0;

/// Transient state for the centre-to-radius phase of MakeCircle.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CircleDraft {
    pub(crate) center: DVec3,
    pub(crate) radius_text: String,
    /// Physical-pixel cursor position at the first typed character.
    pub(crate) typing_origin_screen_px: Option<(f32, f32)>,
    pub(crate) focus_requested: bool,
}

impl CircleDraft {
    pub(crate) fn new(center: DVec3) -> Self {
        Self {
            center,
            radius_text: String::new(),
            typing_origin_screen_px: None,
            focus_requested: true,
        }
    }

    /// A typed radius is deliberately limited to unsigned decimal notation.
    pub(crate) fn typed_radius(&self) -> Option<f64> {
        let text = self.radius_text.as_str();
        let valid_decimal = !text.is_empty()
            && text.chars().any(|ch| ch.is_ascii_digit())
            && text.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
            && text.chars().filter(|&ch| ch == '.').count() <= 1;
        if !valid_decimal {
            return None;
        }
        text.parse::<f64>().ok().filter(|radius| radius.is_finite() && *radius > f64::EPSILON)
    }

    pub(crate) fn mouse_radius(&self, cursor: Option<DVec3>) -> Option<f64> {
        cursor
            .map(|cursor| cursor.truncate().distance(self.center.truncate()))
            .filter(|radius| radius.is_finite() && *radius > f64::EPSILON)
    }

    pub(crate) fn preview_radius(&self, cursor: Option<DVec3>) -> Option<f64> {
        self.typed_radius().or_else(|| self.mouse_radius(cursor))
    }

    pub(crate) fn pointer_commit_radius(&self, cursor: Option<DVec3>) -> Option<f64> {
        self.mouse_radius(cursor)
    }

    pub(crate) fn note_radius_text_changed(&mut self, was_empty: bool, cursor_screen_px: Option<(f32, f32)>) {
        if self.radius_text.is_empty() {
            self.typing_origin_screen_px = None;
        } else if was_empty {
            self.typing_origin_screen_px = cursor_screen_px;
        }
    }

    /// Clear typed-radius mode after a deliberate, DPI-independent pointer move.
    pub(crate) fn reset_typed_for_pointer_movement(&mut self, cursor_screen_px: Option<(f32, f32)>, pixels_per_point: f32) -> bool {
        let (Some(origin), Some(cursor)) = (self.typing_origin_screen_px, cursor_screen_px) else {
            return false;
        };
        let dx = cursor.0 - origin.0;
        let dy = cursor.1 - origin.1;
        let threshold_px = CIRCLE_TYPED_RESET_DISTANCE_POINTS * pixels_per_point.max(1.0);
        if dx * dx + dy * dy <= threshold_px * threshold_px {
            return false;
        }
        self.radius_text.clear();
        self.typing_origin_screen_px = None;
        self.focus_requested = true;
        true
    }
}

/// Plane-handle index used for the Move gizmo's view-aligned ring, which
/// translates in the camera plane instead of a world-axis plane.
pub(crate) const MOVE_GIZMO_VIEW_PLANE: u8 = 3;

/// One frame's screen-space projection of the Move gizmo. Pixel values are
/// physical pixels, matching `cursor_screen_px`, so hit tests and drawing share
/// one source of truth.
#[derive(Clone, Copy)]
pub(crate) struct MoveGizmoScreen {
    pub(crate) center_px: Option<(f32, f32)>,
    /// Arrow tips for X, Y and Z. Foreshortened with the axis, so an axis
    /// tilting away from the camera visibly shortens instead of being forced
    /// out to a fixed length in an unstable direction.
    pub(crate) axis_tip_px: [Option<(f32, f32)>; 3],
    /// Screen pixels spanned by one world unit along each axis.
    pub(crate) axis_px_per_world: [f64; 3],
    /// Per-axis opacity. An axis pointing at the camera fades out and stops
    /// being clickable rather than collapsing to a degenerate stub.
    pub(crate) axis_fade: [f32; 3],
    /// XY, XZ and YZ plane handles, projected from world-space corners.
    pub(crate) plane_quad_px: [Option<[(f32, f32); 4]>; 3],
    /// Per-plane opacity, fading out as the plane turns edge-on.
    pub(crate) plane_fade: [f32; 3],
    /// Radius of the view-plane ring drawn around the centre.
    pub(crate) ring_radius_px: f32,
    /// Camera-plane world axes for a ring drag, with the screen vector one
    /// world unit along each produces.
    pub(crate) view_axes: Option<[DVec3; 2]>,
    pub(crate) view_basis_px: [(f64, f64); 2],
    /// Physical pixels per logical point at the time of projection, so hit
    /// tests can size their slack the same way the gizmo is sized.
    pub(crate) scale_factor: f32,
}

impl Default for MoveGizmoScreen {
    fn default() -> Self {
        Self {
            center_px: None,
            axis_tip_px: [None; 3],
            axis_px_per_world: [1.0; 3],
            axis_fade: [0.0; 3],
            plane_quad_px: [None; 3],
            plane_fade: [0.0; 3],
            ring_radius_px: 0.0,
            view_axes: None,
            view_basis_px: [(1.0, 0.0), (0.0, 1.0)],
            scale_factor: 1.0,
        }
    }
}

/// Explorer item whose name is being edited.
///
/// Design layers live in the `Document` and rename through the undo history;
/// every other kind is an App-owned project item renamed in place, so the one
/// dialog has to carry which of the two it is addressing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenameTarget {
    Layer(LayerId),
    Triangulation(TriangulationId),
    Raster(RasterTextureId),
    PointCloud(PointCloudId),
    BlockModel(BlockModelId),
    DrillHole(DrillHoleId),
}

impl RenameTarget {
    pub(crate) fn kind_label(self) -> &'static str {
        match self {
            Self::Layer(_) => "Layer",
            Self::Triangulation(_) => "Triangulation",
            Self::Raster(_) => "Raster",
            Self::PointCloud(_) => "Point Cloud",
            Self::BlockModel(_) => "Block Model",
            Self::DrillHole(_) => "Drill Holes",
        }
    }

    /// The command that actually performs deletion of this item, issued once
    /// the delete-confirmation dialog is accepted.
    pub(crate) fn remove_command(self) -> UiCommand {
        match self {
            Self::Layer(id) => UiCommand::DeleteLayer(id),
            Self::Triangulation(id) => UiCommand::RemoveTriangulation(id),
            Self::Raster(id) => UiCommand::RemoveRaster(id),
            Self::PointCloud(id) => UiCommand::RemovePointCloud(id),
            Self::BlockModel(id) => UiCommand::RemoveBlockModel(id),
            Self::DrillHole(id) => UiCommand::RemoveDrillHole(id),
        }
    }
}

/// Central mutable editor state.
///
/// Shared between the render pipeline and every UI draw call. Fields are grouped
/// into logical sections below for navigability.
pub(crate) struct EditorState {
    // Selection & visibility
    pub(crate) selected_handles: HashSet<SceneEntityId>,
    /// Individually selected drill holes, which the Drill & Blast workspace
    /// works in place of whole datasets - see [`DrillHoleRef`]. Production
    /// selects the dataset into [`Self::selected_handles`] and leaves this
    /// empty; the two are never populated for the same drill hole at once.
    pub(crate) selected_drill_holes: HashSet<DrillHoleRef>,
    /// Entities removed from view (skipped by the renderer).
    pub(crate) hidden_handles: HashSet<SceneEntityId>,
    /// Entities frozen: still visible, but excluded from editing and snapping.
    ///
    /// Derived: the union of [`Self::explicitly_frozen`] and every design
    /// object sitting on a layer in [`Self::locked_layers`], recomputed by
    /// `App::invalidate_geometry`. Everything that filters picks, snaps and
    /// marquee hits reads this one set, so layer locks need no separate check.
    pub(crate) frozen_handles: HashSet<SceneEntityId>,
    /// Entities the user froze by name: Display > Freeze Selection, or the
    /// explorer's per-row padlock.
    pub(crate) explicitly_frozen: HashSet<SceneEntityId>,
    /// Design layers locked against selection and editing.
    pub(crate) locked_layers: HashSet<LayerId>,
    /// Rasters locked against draping and deletion. Rasters are not scene
    /// entities, so their lock is enforced in the explorer's menus rather than
    /// through [`Self::frozen_handles`].
    pub(crate) locked_rasters: HashSet<RasterTextureId>,
    /// Entities dimmed toward the background colour.
    pub(crate) translucent_handles: HashSet<SceneEntityId>,
    /// Show wireframes on all topology meshes. Selected topology meshes always
    /// show a highlighted wireframe independently of this preference.
    pub(crate) topology_wireframes_enabled: bool,
    /// Show every vertex of all visible design objects. These are kept in a
    /// persistent GPU instance cache rather than a decimated UI overlay.
    pub(crate) show_points: bool,
    /// The UI language in force, which the status bar's picker changes live.
    /// Read only to tick the running language in that picker - what the strings
    /// themselves come from is the loader in [`crate::i18n`].
    pub(crate) language: crate::i18n::LanguageChoice,
    /// Use dark UI visuals and icons (the default) instead of the light theme.
    pub(crate) dark_mode: bool,
    /// Show the console underneath the bottom toolbar.
    pub(crate) show_console: bool,
    /// Dress the panels as rounded regions parted by a gap of window
    /// background. Off, they sit flush and square: see `ui::chrome`.
    pub(crate) panel_chrome: bool,
    /// Show the world-space axis gizmo in the top-right of the viewport.
    pub(crate) show_world_axis_gizmo: bool,
    /// Show the construction grid on the world XY plane at Z=0.
    pub(crate) show_xy_grid: bool,
    /// Show the cartographic distance scale in the viewport.
    pub(crate) show_scale_bar: bool,
    /// Newest published desktop release, when it is newer than this build.
    /// This is transient UI state populated by the startup website check.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) newer_release: Option<String>,
    /// Linear RGBA clear colour used behind the rendered scene.
    pub(crate) renderer_background_color: [f32; 4],
    /// Values the properties panel's settings tabs are editing.
    ///
    /// Settings apply as each edit lands, so this only differs from the live
    /// preferences while a `DragValue` is mid-drag - which is exactly why it
    /// has to survive between frames. `None` means "no edit in flight": the
    /// panel seeds it from the live values.
    pub(crate) preferences_draft: Option<PreferencesDraft>,
    pub(crate) snap_poll_rate: u32,
    pub(crate) frame_rate_cap: u32,
    pub(crate) resize_frame_rate_cap: u32,
    pub(crate) block_model_interaction_resolution_divisor: u32,
    /// Show the Fresnel rim highlight at block-model material boundaries.
    pub(crate) show_block_model_boundary_highlights: bool,
    /// Downscale large GeoTIFFs before retaining and uploading their pixel data.
    pub(crate) downscale_raster_previews: bool,
    pub(crate) frame_counter_enabled: bool,
    pub(crate) measured_fps: Option<f32>,
    /// Developer view: colour each surface chunk distinctly to visualise the
    /// Morton spatial chunking (and drive the chunk-cull stats readout).
    pub(crate) debug_chunk_coloring: bool,
    /// `(rendered, total)` surface chunks from the last frame; shown in the
    /// status bar while `debug_chunk_coloring` is on.
    pub(crate) debug_chunk_stats: Option<(u32, u32)>,
    /// Current projection near/far values, shown in the status bar when the
    /// developer clip-plane readout is enabled.
    pub(crate) debug_clip_plane_distances: Option<(f64, f64)>,
    pub(crate) debug_clip_planes: bool,
    pub(crate) plan_orbit_sensitivity: f64,
    pub(crate) plan_zoom_sensitivity: f64,
    pub(crate) plan_invert_vertical_look: bool,
    pub(crate) plan_invert_horizontal_look: bool,
    pub(crate) plan_zoom_towards_cursor: bool,
    pub(crate) fly_field_of_view_degrees: f64,
    pub(crate) fly_mouse_look_sensitivity: f64,
    pub(crate) fly_invert_vertical_look: bool,
    pub(crate) fly_invert_horizontal_look: bool,
    pub(crate) fly_near_clip_limit: f64,
    pub(crate) fly_max_clip_span: f64,
    /// Whether a background task is running, mirrored from `App` so the UI can
    /// ask egui for the busy cursor. The window's cursor is egui's to set: a
    /// `winit::Window::set_cursor` call from App-side would desync
    /// egui-winit's icon cache and strand whatever the pointer is showing -
    /// including the hidden cursor the viewport draws its own crosshair over.
    pub(crate) background_busy: bool,
    /// Transient status-bar message from a background task (e.g. "Saving to …").
    /// `None` means idle; the field is updated from `poll_saves` / `poll_jobs`
    /// each frame. Set it through [`EditorState::set_status_message`] so the
    /// idle bar can name the task that just ended.
    pub(crate) status_message: Option<StatusBarMessage>,
    /// The most recent task to leave the status bar, shown as "…: Finished" at
    /// 100% while idle. `None` until the first task completes - the progress bar
    /// is hidden entirely until then.
    pub(crate) last_finished_task: Option<FinishedTask>,
    pub(crate) active_tool: ActiveTool,
    /// The cursor each workspace is holding - see [`WorkspaceCursors`]. There
    /// is no cursor mode over the application as a whole: what a pick does is
    /// a question about the workspace it is made in.
    pub(crate) cursors: WorkspaceCursors,
    pub(crate) tool_line_color: [f32; 4],
    pub(crate) tool_line_weight: f32,
    pub(crate) tool_hatch: ToolHatch,
    /// Active drawing layer, if any.
    pub(crate) active_layer: Option<LayerId>,
    /// The drill hole dataset the Drill & Blast workspace works on: what its
    /// editing, tie-in and simulation tools act against. `None` until one is
    /// picked, and dropped again when that dataset is closed or removed.
    pub(crate) active_drill_hole: Option<DrillHoleId>,
    /// Live world coordinate under the cursor (z on the active pick plane).
    pub(crate) cursor_world: Option<DVec3>,
    /// Browser-only viewport prompt shown before creating a named project.
    #[cfg(target_arch = "wasm32")]
    pub(crate) new_project_dialog_open: bool,
    #[cfg(target_arch = "wasm32")]
    pub(crate) new_project_name: String,
    pub(crate) new_layer_dialog_open: bool,
    pub(crate) new_layer_name: String,
    /// Active explorer rename: (target, name_buffer).
    pub(crate) renaming_item: Option<(RenameTarget, String)>,
    /// Layer awaiting destructive deletion confirmation: (layer_id, display_name).
    pub(crate) pending_delete_layer: Option<(LayerId, String)>,
    /// Non-layer explorer item (triangulation, raster, point cloud, block
    /// model, drill hole dataset) awaiting destructive deletion confirmation.
    pub(crate) pending_delete_item: Option<(RenameTarget, String)>,
    /// Vertices accumulated for an in-progress MakeLine / MakePoly stroke.
    pub(crate) pending_stroke: Vec<DVec3>,
    pub(crate) circle_draft: Option<CircleDraft>,
    pub(crate) measurement_start: Option<DVec3>,
    pub(crate) measurement_end: Option<DVec3>,
    pub(crate) batter_angle_points: Vec<DVec3>,

    // Text editing
    /// Chars accumulated for an in-progress MakeText.
    pub(crate) pending_text: String,
    pub(crate) pending_text_height: f64,
    pub(crate) pending_text_rotation_degrees: f64,
    pub(crate) text_edit_dialog_px: Option<(f32, f32)>,
    /// Keep the text menu anchored while egui initializes its window state.
    pub(crate) text_edit_position_frames: u8,
    pub(crate) text_edit_focus_requested: bool,
    pub(crate) text_edit_created: bool,
    /// Dirty state of the active project before the new-text AddObject was committed.
    /// Whether the text properties popup is open.
    pub(crate) text_editing_enabled: bool,
    /// The ObjectId of the actively edited text label.
    pub(crate) editing_labels_id: Option<ObjectId>,

    // Cursor & snapping
    /// Z plane used for all placement operations (point, line, poly vertices).
    pub(crate) z_level: f64,
    /// Editable Z level value used by the toolbar and Design > Move to > Set Z.
    pub(crate) z_input: f64,
    /// True when the current `cursor_world` is a snapped position (not raw ray).
    pub(crate) cursor_snapped: bool,
    /// Where the snapped point lands on the window, in physical pixels, while
    /// [`EditorState::cursor_snapped`] is set. Projected in
    /// `update_tool_projections` like every other tool overlay, and read by
    /// the drawn cursor so it can mark the target it caught rather than only
    /// reporting that it caught one.
    pub(crate) snap_marker_px: Option<(f32, f32)>,
    /// Physical-pixel cursor position, updated on every CursorMoved event.
    pub(crate) cursor_screen_px: Option<(f32, f32)>,

    // Dialog state (position snapshots)
    /// When true, show the polyline finish dialog near the cursor.
    pub(crate) poly_finish_dialog: bool,
    /// The polyline finish dialog only accepts its Enter shortcut once the
    /// Enter press that opened it has been released - otherwise a held key
    /// confirms "Open" the same instant the dialog appears.
    pub(crate) poly_finish_dialog_confirm_armed: bool,
    /// Screen position (physical px) where the polyline finish dialog was opened.
    /// Snapshotted once so the dialog doesn't follow the cursor.
    pub(crate) poly_finish_dialog_px: Option<(f32, f32)>,
    /// When true, show the canvas right-click context menu.
    pub(crate) canvas_context_menu_open: bool,
    /// Physical-pixel position where the canvas context menu was opened.
    pub(crate) canvas_context_menu_px: Option<(f32, f32)>,
    /// Selected polylines and the in-progress line-weight value for the
    /// Design properties tab. The value must survive across frames while its
    /// `DragValue` is being dragged.
    pub(crate) design_line_weight_input: Option<(Vec<ObjectId>, f32)>,
    /// Objects described by the explorer's Design properties tab: whatever the
    /// last non-empty selection held, kept after that selection is cleared so
    /// the tab stays put. Entries are dropped once the object is gone.
    pub(crate) property_objects: Vec<ObjectId>,
    /// Scene entities described by the generic Object properties tab. Like
    /// the type-specific memories, this keeps the last non-empty selection so
    /// the panel does not go blank merely because the canvas was deselected.
    pub(crate) property_entities: Vec<SceneEntityId>,
    pub(crate) move_to_layer_dialog: Option<MoveToLayerDialog>,
    pub(crate) move_to_axis_dialog: Option<crate::ui::dialogs::MoveToAxisDialog>,
    /// Whether the selected polylines cross anywhere, refreshed by
    /// `App::refresh_intersection_availability` before each frame's UI. Gates
    /// Design > Insert Point > At intersection.
    pub(crate) selection_has_intersections: bool,
    pub(crate) insert_point_at_elevation_dialog: Option<crate::ui::dialogs::InsertPointAtElevationDialog>,

    // Display overrides
    pub(crate) xray_enabled: bool,
    pub(crate) vertical_exaggeration_dialog_open: bool,
    pub(crate) vertical_exaggeration: f64,
    pub(crate) vertical_exaggeration_input: f64,
    pub(crate) fly_mode_enabled: bool, // Not sure if this belongs here

    // Vertical slice view
    /// First click of the slice-line placement, if any.
    pub(crate) slice_pending_start: Option<DVec3>,
    /// Mirror of the graphics slice mode (like `fly_mode_enabled`).
    pub(crate) slice_mode_enabled: bool,
    /// True while the full-resolution plan preview is in a native window.
    pub(crate) slice_preview_detached: bool,
    /// GPU texture rendered with the normal shaded plan-view scene pass.
    pub(crate) slice_preview_texture: Option<egui::TextureId>,
    /// Physical pixel size requested by the embedded preview on its last UI frame.
    pub(crate) slice_preview_size_px: [u32; 2],
    /// Pan/zoom of the plan preview only; independent of the slice geometry.
    pub(crate) slice_preview_navigation: SlicePreviewNavigation,
    /// Slab thickness in metres, live-applied from the slice panel.
    pub(crate) slice_width_input: f64,
    /// W/S slab movement speed (m/s).
    pub(crate) slice_speed_input: f64,
    /// Q/E rotation rate (degrees per second).
    pub(crate) slice_rotate_input: f64,
    /// Current slice centre (display space), mirrored per frame for the plan preview.
    pub(crate) slice_center: [f64; 3],
    /// Current slice-line direction in XY, mirrored per frame for the plan preview.
    pub(crate) slice_direction: [f64; 2],
    /// Half-width of the section currently visible in the main viewport.
    pub(crate) slice_half_length: f64,

    // Selection box
    /// Physical-pixel bounds of an in-progress box selection.
    pub(crate) selection_box_start_px: Option<(f32, f32)>,
    pub(crate) selection_box_current_px: Option<(f32, f32)>,

    // Drape to Topology tool
    pub(crate) drape_phase: DrapePhase,
    /// Design objects retained while the second selection step chooses surfaces.
    pub(crate) drape_object_ids: Vec<ObjectId>,

    // Offset Element tool
    pub(crate) offset_dialog_open: bool,
    pub(crate) offset_target_id: Option<ObjectId>,
    pub(crate) offset_target_ids: Vec<ObjectId>,
    pub(crate) offset_angle_degrees: f64,
    pub(crate) offset_measure: OffsetMeasure,
    pub(crate) offset_value_input: f64,
    /// Phase 2: dialog closed, waiting for canvas click to pick side.
    pub(crate) offset_awaiting_side_pick: bool,
    /// Absolute horizontal offset distance (sign determined by cursor position).
    pub(crate) offset_horiz_dist: f64,
    /// Z shift to apply to all vertices (0 for horizontal, ±height for batter).
    pub(crate) offset_z_delta: f64,
    /// When set, overrides `offset_horiz_dist`/`offset_z_delta`: project each vertex
    /// individually (by its own elevation) along `tan(angle)` so the whole result
    /// lands flat at `target_rl`. Fields are `(tan_angle, target_rl)`.
    pub(crate) offset_project_to_rl: Option<(f64, f64)>,
    /// Clamp offset vertices to the first visible triangulation they hit along
    /// the requested offset vector.
    pub(crate) offset_collide_with_triangulation: bool,
    /// Preview polyline vertices in world coordinates.
    pub(crate) offset_preview_world: Vec<DVec3>,
    /// Source vertices matching `offset_preview_world`, used for offset guide connectors.
    pub(crate) offset_source_world: Vec<DVec3>,
    /// Preview polyline vertices projected to physical-pixel screen
    /// coordinates this frame. One entry per source vertex; `None` marks a
    /// vertex outside the camera depth range so indexed consumers stay
    /// aligned with `offset_preview_world`.
    pub(crate) offset_preview_screen_px: Vec<Option<(f32, f32)>>,
    /// Source vertices projected to physical-pixel screen coordinates this
    /// frame; index-aligned with `offset_source_world` (`None` = clipped).
    pub(crate) offset_source_screen_px: Vec<Option<(f32, f32)>>,
    /// Preview screen ranges as `(start, end, closed)` so multiple offset
    /// previews are drawn independently.
    pub(crate) offset_preview_ranges: Vec<(usize, usize, bool)>,
    /// Whether the preview geometry is closed (polyline) or open (polyline).
    pub(crate) offset_preview_closed: bool,

    // Relimit Line tool
    pub(crate) relimit_dialog_open: bool,
    pub(crate) relimit_source_id: Option<ObjectId>,
    pub(crate) relimit_mode: RelimitMode,
    pub(crate) relimit_value_input: f64,
    /// Phase 0: tool active, no source selected yet - waiting for canvas click to pick source.
    pub(crate) relimit_awaiting_source_pick: bool,
    /// Phase 1: source selected, dialog closed - waiting for canvas click to pick target line.
    pub(crate) relimit_waiting_for_pick: bool,
    /// Phase 2: intersection computed, user confirms which end to move.
    pub(crate) relimit_confirming_end: bool,
    pub(crate) relimit_second_id: Option<ObjectId>,
    /// All valid relimit operations for the current source/target pair. The
    /// user selects one by hovering near its projected changed segment.
    pub(crate) relimit_candidates: Vec<RelimitCandidate>,
    /// Currently active intersection (set from hover zone, used for commit).
    pub(crate) relimit_intersection_3d: Option<DVec3>,
    pub(crate) relimit_hover_end: TrimEnd,
    /// Phase 2 - preview segment: moving endpoint → intersection, yellow=extension red=reduction.
    pub(crate) relimit_preview_from_px: Option<(f32, f32)>,
    pub(crate) relimit_preview_to_px: Option<(f32, f32)>,
    pub(crate) relimit_preview_is_extension: bool,
    /// Which end to move in AbsoluteLength / RelativeLength modes (default End).
    pub(crate) relimit_resize_end: TrimEnd,
    /// Screen position of the chosen resize endpoint sphere indicator (raw pixels).
    pub(crate) relimit_resize_end_px: Option<(f32, f32)>,

    // Fuse Into Polyline tool
    pub(crate) fuse_segments: Vec<FuseSegment>,
    /// Line that has been picked but is waiting for the user to click one of its endpoint markers.
    pub(crate) fuse_awaiting_endpoint: Option<ObjectId>,
    /// Selectable vertices for `fuse_awaiting_endpoint`. Open lines expose their
    /// two endpoints; closed polylines expose every vertex.
    pub(crate) fuse_endpoint_markers: Vec<(usize, DVec3)>,
    /// The open tail of the current chain - where the next segment will attach.
    pub(crate) fuse_chain_tail: Option<DVec3>,
    /// Opposite endpoint offered to close a single open source polyline.
    pub(crate) fuse_close_marker: Option<DVec3>,
    // Split At Points tool
    pub(crate) split_poly_id: Option<ObjectId>,
    pub(crate) split_selected_verts: [Option<usize>; 2],
    /// One entry per polyline vertex; `None` marks a vertex outside the camera
    /// depth range, keeping `split_selected_verts` indices aligned.
    pub(crate) split_poly_verts_screen_px: Vec<Option<(f32, f32)>>,

    // Chamfer tool
    pub(crate) chamfer_radius: f64,
    pub(crate) chamfer_segments: u32,
    /// Maximum chamfer radius for the currently selected corner (f64::MAX = unlimited).
    pub(crate) chamfer_max_radius: f64,
    /// Which closed polyline is being chamfered (set on corner click).
    pub(crate) chamfer_poly_id: Option<ObjectId>,
    /// Which vertex index of `chamfer_poly_id` is the selected corner.
    pub(crate) chamfer_corner_index: Option<usize>,
    /// Preview of chamfered polyline projected to screen (raw pixels). Computed each frame by
    /// the renderer. One entry per preview vertex (`None` = clipped) so segments
    /// between visible neighbours stay adjacent.
    pub(crate) chamfer_preview_screen_px: Vec<Option<(f32, f32)>>,
    /// Screen position of the nearest hoverable chamfer corner (raw pixels, before a corner is
    /// picked).
    pub(crate) chamfer_hover_corner_px: Option<(f32, f32)>,
    /// Screen position of the vertex that Move or Delete would act on (raw pixels).
    pub(crate) tool_hover_vertex_px: Option<(f32, f32)>,
    /// Same vertex in world space, so the overlay can draw the shared point marker for it.
    pub(crate) tool_hover_vertex_world: Option<DVec3>,
    /// Screen position of the first chamfer corner vertex (raw pixels).
    pub(crate) chamfer_gizmo_corner_px: Option<(f32, f32)>,
    /// Screen direction of the gizmo bisector (used when radius=0 so arrow is still visible).
    pub(crate) chamfer_gizmo_bisector_px: Option<(f32, f32)>,
    /// Screen position of the radius handle (raw pixels).
    pub(crate) chamfer_gizmo_handle_px: Option<(f32, f32)>,
    pub(crate) chamfer_gizmo_hovered: bool,
    /// Normalised screen direction of the reference edge (raw pixels).
    pub(crate) chamfer_gizmo_edge_screen_dir: Option<(f32, f32)>,
    pub(crate) chamfer_gizmo_px_per_world: f64,
    pub(crate) chamfer_gizmo_drag_start_px: Option<(f32, f32)>,
    pub(crate) chamfer_gizmo_drag_start_radius: f64,

    // Move tool gizmo
    pub(crate) move_vertex_target: Option<(ObjectId, ObjectPoint)>,
    /// Projected Move gizmo for the current frame.
    pub(crate) move_gizmo: MoveGizmoScreen,
    pub(crate) move_gizmo_hovered_axis: Option<u8>,
    /// Hovered plane handle: 0 = XY, 1 = XZ, 2 = YZ, `MOVE_GIZMO_VIEW_PLANE` = ring.
    pub(crate) move_gizmo_hovered_plane: Option<u8>,
    pub(crate) gizmo_drag_axis_index: Option<u8>,
    pub(crate) gizmo_drag_plane_index: Option<u8>,
    pub(crate) move_panel_delta: [f64; 3],
    /// Last delta that was actually applied as a preview (to avoid redundant rebuilds).
    pub(crate) move_panel_last_preview: [f64; 3],

    /// Shared tool-highlight - draws this object in the selection colour regardless of selection.
    /// Used by tools for selected targets and canvas hover feedback.
    pub(crate) tool_highlight_id: Option<ObjectId>,

    /// Color to use when creating or editing text (populated from the object on edit start).
    pub(crate) pending_text_color: [f32; 4],

    /// When true, show the "save before quit?" confirmation dialog.
    pub(crate) exit_confirm_open: bool,
    pub(crate) delete_confirm_open: bool,
    pub(crate) can_undo: bool,
    pub(crate) can_redo: bool,
    /// Dirty project awaiting save/discard confirmation before it is closed.
    pub(crate) pending_close_project: Option<u32>,
    /// The pending close was started by Remove Project in the explorer. Once
    /// the close is allowed to proceed, desktop forgets the tracked path and
    /// the browser deletes the project's persisted record.
    pub(crate) remove_project_after_close: bool,
    /// A New/Open action is waiting for a Save/Discard/Cancel choice.
    pub(crate) replace_project_confirm_open: bool,
    pub(crate) lossy_save_confirm_open: bool,
    /// Dirty project awaiting confirmation before its changes are discarded
    /// (reverted to the last saved state on disk).
    pub(crate) pending_discard_project: Option<u32>,
    /// Dirty layer awaiting confirmation before only that layer is restored
    /// from its owning project on disk: (layer id, display name).
    pub(crate) pending_discard_layer: Option<(LayerId, String)>,

    // Create Triangulation workflow
    pub(crate) tri_create_open: bool,
    pub(crate) tri_create_phase: TriCreatePhase,
    /// A failed Create Triangulation run, kept so the failure dialog can
    /// offer a coarse-weld retry with the same inputs.
    pub(crate) tri_create_failure: Option<TriCreateFailure>,
    /// Per-frame projections of the current failure's world-space diagnostic.
    pub(crate) tri_create_diagnostic_markers_screen_px: Vec<(f32, f32)>,
    pub(crate) tri_create_diagnostic_segments_screen_px: Vec<[OptionalScreenPointPx; 2]>,
    /// Frozen cursor position (physical px) where the picker Area was opened.
    pub(crate) tri_create_picker_px: Option<(f32, f32)>,
    /// Objects highlighted yellow on canvas during selection hover.
    pub(crate) tri_hover_handles: HashSet<SceneEntityId>,
    /// Individually confirmed objects for triangulation.
    pub(crate) tri_selected_object_ids: Vec<ObjectId>,
    /// Confirmed layers - all their objects will be triangulated.
    pub(crate) tri_selected_layer_ids: Vec<LayerId>,
    pub(crate) tri_name_input: String,
    pub(crate) tri_surface_type: TriSurfaceType,

    /// Active viewport-to-field triangulation picker, if any.
    pub(crate) triangulation_pick_target: Option<TriangulationPickTarget>,
    /// Live description of the valid object under the cursor while a dialog
    /// field is being filled from the viewport.
    pub(crate) viewport_pick_hover_label: Option<String>,

    // Cut Triangulation by Polyline
    pub(crate) tri_cut_poly_open: bool,
    pub(crate) tri_cut_poly_awaiting_pick: bool,
    pub(crate) tri_cut_poly_tri_id: Option<TriangulationId>,
    pub(crate) tri_cut_poly_object_id: Option<ObjectId>,
    pub(crate) tri_cut_poly_object_name: String,
    pub(crate) tri_cut_poly_mode: TriPolylineClipMode,
    pub(crate) tri_cut_poly_name_input: String,
    pub(crate) tri_cut_poly_name_auto: bool,

    // Cut Triangulation by Z Range
    pub(crate) tri_cut_z_open: bool,
    pub(crate) tri_cut_z_tri_id: Option<TriangulationId>,
    pub(crate) tri_cut_z_min_input: f64,
    pub(crate) tri_cut_z_max_input: f64,
    pub(crate) tri_cut_z_name_input: String,
    pub(crate) tri_cut_z_name_auto: bool,

    // Trim Surface to Topology
    pub(crate) tri_cut_surface_open: bool,
    pub(crate) tri_cut_surface_target_id: Option<TriangulationId>,
    pub(crate) tri_cut_surface_reference_id: Option<TriangulationId>,
    pub(crate) tri_cut_surface_side: TriSurfaceCutSide,
    pub(crate) tri_cut_surface_name_input: String,
    pub(crate) tri_cut_surface_name_auto: bool,

    // Cut Topology to Pit Shell
    pub(crate) tri_cut_pitshell_open: bool,
    pub(crate) tri_cut_pitshell_topology_id: Option<TriangulationId>,
    pub(crate) tri_cut_pitshell_pitshell_id: Option<TriangulationId>,
    pub(crate) tri_cut_pitshell_name_input: String,
    pub(crate) tri_cut_pitshell_name_auto: bool,

    // Include Pit/Stockpile Solid in Topology
    pub(crate) tri_include_solid_open: bool,
    pub(crate) tri_include_solid_topology_id: Option<TriangulationId>,
    pub(crate) tri_include_solid_shape_id: Option<TriangulationId>,
    pub(crate) tri_include_solid_name_input: String,
    pub(crate) tri_include_solid_name_auto: bool,
    pub(crate) tri_include_solid_save_as_two: bool,
    /// Hide and unload the source topology and solid once the merge completes.
    pub(crate) tri_include_solid_hide_old: bool,

    // Contour Generation
    pub(crate) tri_contour_open: bool,
    pub(crate) tri_contour_tri_id: Option<TriangulationId>,
    pub(crate) tri_contour_major_interval_input: f64,
    pub(crate) tri_contour_minor_interval_input: f64,
    pub(crate) tri_contour_major_color: [f32; 4],
    pub(crate) tri_contour_minor_color: [f32; 4],
    pub(crate) tri_contour_use_z_range: bool,
    pub(crate) tri_contour_z_min_input: f64,
    pub(crate) tri_contour_z_max_input: f64,
    /// `None` creates a new layer; `Some` appends to an existing active project layer.
    pub(crate) tri_contour_target_layer: Option<LayerId>,
    pub(crate) tri_contour_layer_name_input: String,
    pub(crate) tri_contour_layer_name_auto: bool,
    pub(crate) point_cloud_tin_open: bool,
    pub(crate) point_cloud_tin_cloud_id: Option<PointCloudId>,
    pub(crate) point_cloud_tin_name_input: String,
    /// Maximum terrain TIN edge length. 0 disables edge filtering.
    pub(crate) point_cloud_tin_max_edge: f64,
    /// Budget entered as a percentage of the cloud rather than an absolute count.
    pub(crate) point_cloud_tin_budget_is_percent: bool,
    /// Percentage of the source cloud sampled for terrain reconstruction.
    pub(crate) point_cloud_tin_percent: f64,
    /// Absolute vertex budget when not entering a percentage.
    pub(crate) point_cloud_tin_limit: u32,
    /// Which sampler distributes the vertex budget.
    pub(crate) point_cloud_tin_sampler: crate::app::commands::triangulation::TerrainSampler,
    /// Candidate fine cells per budgeted vertex for the adaptive sampler.
    pub(crate) point_cloud_tin_candidate_mult: u32,
    /// Bridge holes and boundary concavities narrower than this (0 = only gaps).
    pub(crate) point_cloud_tin_hole_fill: f64,

    // Block Models
    pub(crate) block_model_table_pages: HashMap<BlockModelId, usize>,
    /// Block model described by the explorer's Block Model properties tab. Set
    /// by the last non-empty selection and kept after that selection is
    /// cleared, until the model is closed or another selection replaces it.
    pub(crate) viewport_block_model_id: Option<BlockModelId>,
    /// Triangulation described by the explorer's Triangulation properties tab.
    /// Kept after the selection that set it is cleared, exactly as
    /// `viewport_block_model_id` is, until the surface is closed or another
    /// selection replaces it.
    pub(crate) viewport_triangulation_id: Option<TriangulationId>,
    pub(crate) block_model_variable_ranges: HashMap<(BlockModelId, String), Option<(f64, f64)>>,
    pub(crate) next_color_stop_id: u64,
    /// Dataset owning the movable drillhole colour popup, when open.
    pub(crate) drill_hole_color_dialog: Option<DrillHoleId>,
    pub(crate) block_model_create_open: bool,
    pub(crate) kriging_drill_hole_id: Option<DrillHoleId>,
    pub(crate) kriging_variables: Vec<String>,
    pub(crate) kriging_name_input: String,
    pub(crate) kriging_lower: DVec3,
    pub(crate) kriging_upper: DVec3,
    pub(crate) kriging_cell: DVec3,
    pub(crate) kriging_range: f64,
    pub(crate) kriging_sill: f64,
    pub(crate) kriging_nugget: f64,
    pub(crate) kriging_min_samples: u32,
    pub(crate) kriging_max_samples: u32,
    pub(crate) ore_triangulation_open: bool,
    pub(crate) ore_block_model_id: Option<BlockModelId>,
    pub(crate) ore_variable: String,
    pub(crate) ore_filter_mode: OreFilterMode,
    pub(crate) ore_min_input: f64,
    pub(crate) ore_max_input: f64,
    pub(crate) ore_name_input: String,

    // Plot sheets
    pub(crate) plot_dialog: Option<crate::ui::dialogs::plot::PlotDialog>,
    /// Live map render shown inside the export dialog's sheet preview, kept up
    /// to date by the renderer while the dialog is open.
    pub(crate) plot_preview_texture: Option<egui::TextureId>,

    // Batter Berm tool
    pub(crate) batter_berm_dialog_open: bool,
    pub(crate) batter_berm_target_id: Option<ObjectId>,
    pub(crate) batter_berm_width: f64,
    pub(crate) batter_berm_angle: f64,
    pub(crate) batter_berm_bench_height: f64,
    pub(crate) batter_berm_benches: u32,
    /// Maximum number of complete, compliant batter-and-berm levels for the
    /// current boundary and design parameters.
    pub(crate) batter_berm_max_benches: u32,
    pub(crate) batter_berm_mode: BatterBermMode,
    /// Direction selector: `true` = each bench rises (Up), `false` = each bench
    /// falls (Down). Together with [`Self::batter_berm_mode`] this decides
    /// whether the rings step inward or outward - see [`BatterBermMode`].
    pub(crate) batter_berm_direction_up: bool,
    /// All iteration rings in world coords: [toe_ring_0, berm_ring_0, toe_ring_1, berm_ring_1, …]
    pub(crate) batter_berm_rings_world: Vec<Vec<DVec3>>,
    pub(crate) batter_berm_source_world: Vec<DVec3>,
    /// Screen projections preserve one entry per world vertex. `None` means
    /// that vertex is outside the camera depth range, so adjacent vertices
    /// must not be joined across the gap.
    pub(crate) batter_berm_rings_screen_px: Vec<Vec<OptionalScreenPointPx>>,
    pub(crate) batter_berm_source_screen_px: Vec<OptionalScreenPointPx>,
    pub(crate) batter_berm_preview_closed: bool,
    pub(crate) batter_berm_preview_key: Option<BatterBermPreviewKey>,

    // Bezier tool
    pub(crate) bezier_poly_id: Option<ObjectId>,
    /// Whether the selected source is a closed polyline rather than an open polyline.
    pub(crate) bezier_poly_closed: bool,
    /// Directed [span start, span end] indices of the path being replaced.
    pub(crate) bezier_selected_verts: [Option<usize>; 2],
    /// For a closed polyline, replace the longer of the two paths between the
    /// selected vertices. Open polylines have only one path, so this is ignored.
    pub(crate) bezier_replace_longer: bool,
    /// World-space coordinates of control point 1 (near the first selected vertex).
    pub(crate) bezier_cp1: [f64; 3],
    /// World-space coordinates of control point 2 (near the second selected vertex).
    pub(crate) bezier_cp2: [f64; 3],
    /// Number of line segments used to approximate the bezier curve.
    pub(crate) bezier_segments: u32,
    /// Screen-space positions of every vertex in the selected polyline (for
    /// white dot indicators). One entry per source vertex (`None` = clipped)
    /// so `bezier_selected_verts` indices always address the right vertex.
    pub(crate) bezier_poly_verts_screen_px: Vec<Option<(f32, f32)>>,
    pub(crate) bezier_cp1_screen_px: Option<(f32, f32)>,
    pub(crate) bezier_cp2_screen_px: Option<(f32, f32)>,
    /// Screen-space positions of the source span that the curve will replace.
    pub(crate) bezier_span_screen_px: Vec<Option<(f32, f32)>>,
    /// Screen-space positions of the dashed yellow replacement curve, one
    /// entry per sample (`None` = clipped).
    pub(crate) bezier_preview_screen_px: Vec<Option<(f32, f32)>>,
    /// Which CP handle is being dragged (0 = cp1, 1 = cp2), None if not dragging.
    pub(crate) bezier_dragging_cp: Option<u8>,
    /// Which CP handle is hovered (0 = cp1, 1 = cp2), None if neither.
    pub(crate) bezier_hover_cp: Option<u8>,
    pub(crate) bezier_dialog_open: bool,
    /// Which section the explorer's properties panel is showing.
    pub(crate) active_property_tab: PropertyTab,
    /// The workspace tab selected in the menu bar.
    pub(crate) active_workspace: Workspace,
    /// The Drill & Blast workspace's stored products, in the order the palette
    /// lays them out.
    pub(crate) delay_products: Vec<DelayProduct>,
    /// Id the next product added to that palette takes.
    pub(crate) next_delay_product_id: u64,
    /// Whether the palette's New Product dialog is open.
    pub(crate) new_delay_product_open: bool,
    /// What that dialog has been filled in with so far.
    pub(crate) new_delay_product_delay_ms: u32,
    pub(crate) new_delay_product_name: String,
    pub(crate) new_delay_product_color: egui::Color32,
    pub(crate) show_import: bool,
    pub(crate) show_export: bool,
    /// Whether the About dialog is open.
    pub(crate) show_about: bool,
    /// What filetype should be selected in the import/export menu
    pub(crate) data_menu: DataMenu,
    pub(crate) import_source_menu: DataMenu,
    pub(crate) import_source_paths: Vec<PathBuf>,
    pub(crate) import_csv_preview: Option<CsvPreview>,
    pub(crate) import_csv_error: Option<String>,
    pub(crate) import_drill_csv: Vec<(CsvDrillFileMapping, CsvDrillPreview)>,
    pub(crate) export_dxf_layer: bool,
    /// Runtime id of the open project selected for whole-project export.
    pub(crate) export_project: Option<u32>,
    pub(crate) export_layer: Option<LayerId>,
    pub(crate) export_triangulation: Option<TriangulationId>,
    pub(crate) export_block_model: Option<BlockModelId>,
}

impl EditorState {
    /// What a pick snaps to in the workspace that is up.
    ///
    /// Only production draws, and only its cursor run offers snapping, so
    /// every other workspace picks plainly - the same thing
    /// [`CursorMode::Select`] means there.
    pub(crate) fn snap_cursor_mode(&self) -> CursorMode {
        match self.active_workspace {
            Workspace::Production => self.cursors.production,
            Workspace::DrillAndBlast | Workspace::Geology => CursorMode::Select,
        }
    }

    /// Step the active workspace's cursor one along its own run - what the
    /// mouse's forward and back buttons do - and report whether it moved.
    ///
    /// A workspace with no cursor run of its own has nowhere to step, so the
    /// buttons do nothing there rather than quietly changing another
    /// workspace's cursor behind its back.
    pub(crate) fn cycle_workspace_cursor(&mut self, forward: bool) -> bool {
        match self.active_workspace {
            Workspace::Production => {
                let mode = &mut self.cursors.production;
                *mode = if forward { mode.next() } else { mode.previous() };
                true
            }
            Workspace::DrillAndBlast => {
                let cursor = &mut self.cursors.blast;
                *cursor = if forward { cursor.next() } else { cursor.previous() };
                true
            }
            Workspace::Geology => false,
        }
    }

    /// Dialogs that take Enter as their confirm shortcut.
    ///
    /// The GUI only reports a key press as consumed when a text field holds
    /// focus, so without asking here the viewport tools would act on the same
    /// press before the dialog is drawn. Keep this in step with the dialogs
    /// that call [`crate::ui::widgets::menu::dialog_confirm_pressed`].
    pub(crate) fn dialog_owns_confirm_key(&self) -> bool {
        if self.viewport_pick_in_progress() {
            return false;
        }
        self.poly_finish_dialog || self.dialog_owns_both_keys()
    }

    /// Dialogs that take Escape as their cancel shortcut. The startup splash
    /// is absent on purpose: nothing in the tool chain reacts to Escape while
    /// it is up, so its own handler is enough.
    pub(crate) fn dialog_owns_cancel_key(&self) -> bool {
        if self.viewport_pick_in_progress() {
            return false;
        }
        self.dialog_owns_both_keys()
    }

    /// A dialog is parked waiting on a click in the 3D viewport. Escape belongs
    /// to the pick (it returns to the dialog), and Enter means nothing.
    fn viewport_pick_in_progress(&self) -> bool {
        self.triangulation_pick_target.is_some() || self.tri_cut_poly_awaiting_pick || self.canvas_context_menu_open || self.text_editing_enabled
    }

    /// The common case: a dialog that confirms on Enter and cancels on Escape.
    fn dialog_owns_both_keys(&self) -> bool {
        self.exit_confirm_open
            || self.replace_project_confirm_open
            || self.lossy_save_confirm_open
            || self.delete_confirm_open
            || self.pending_delete_layer.is_some()
            || self.pending_delete_item.is_some()
            || self.pending_close_project.is_some()
            || self.pending_discard_project.is_some()
            || self.pending_discard_layer.is_some()
            || self.show_about
            || self.show_import
            || self.show_export
            || self.drill_hole_color_dialog.is_some()
            || self.plot_dialog.is_some()
            || self.move_to_layer_dialog.is_some()
            || self.move_to_axis_dialog.is_some()
            || self.insert_point_at_elevation_dialog.is_some()
            || self.new_layer_dialog_open
            || self.new_delay_product_open
            || self.renaming_item.is_some()
            || self.tri_create_open
            || self.tri_create_failure.is_some()
            || self.tri_cut_poly_open
            || self.tri_cut_z_open
            || self.tri_cut_surface_open
            || self.tri_cut_pitshell_open
            || self.tri_include_solid_open
            || self.tri_contour_open
            || self.point_cloud_tin_open
            || self.block_model_create_open
            || self.ore_triangulation_open
            || {
                #[cfg(target_arch = "wasm32")]
                {
                    self.new_project_dialog_open
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    false
                }
            }
    }

    /// Open the palette's New Product dialog on a blank entry.
    ///
    /// The last one entered is not kept: a product is added once and the next
    /// one is a different delay, so the dialog starts where the built-ins do
    /// rather than on whatever was typed last.
    pub(crate) fn begin_new_delay_product(&mut self) {
        self.new_delay_product_delay_ms = 0;
        self.new_delay_product_name.clear();
        self.new_delay_product_color = NEW_DELAY_PRODUCT_COLOR;
        self.new_delay_product_open = true;
    }

    pub(crate) fn update_contour_layer_name_from_surface(&mut self, surface_name: &str) {
        if !self.tri_contour_layer_name_auto {
            return;
        }
        let stem = std::path::Path::new(surface_name)
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Surface");
        self.tri_contour_layer_name_input = format!("{} Contours", stem.trim());
    }

    /// Clear every project/object-owned interaction session in one lifecycle
    /// transition. Numeric tool preferences remain intact, but no source id,
    /// preview geometry, projected handle, or modal draft can refer to the
    /// project that was just left.
    pub(crate) fn clear_project_transients(&mut self) {
        self.selected_handles.clear();
        self.selected_drill_holes.clear();
        self.hidden_handles.clear();
        self.frozen_handles.clear();
        self.explicitly_frozen.clear();
        self.locked_layers.clear();
        self.locked_rasters.clear();
        self.translucent_handles.clear();
        self.active_layer = None;
        self.active_drill_hole = None;
        self.active_tool = ActiveTool::None;
        #[cfg(target_arch = "wasm32")]
        {
            self.new_project_dialog_open = false;
        }
        self.new_layer_dialog_open = false;
        self.renaming_item = None;
        self.pending_delete_layer = None;
        self.pending_delete_item = None;
        self.pending_discard_layer = None;
        self.selection_box_start_px = None;
        self.selection_box_current_px = None;
        self.drape_phase = DrapePhase::Designs;
        self.drape_object_ids.clear();
        self.pending_stroke.clear();
        self.circle_draft = None;
        self.poly_finish_dialog = false;
        self.poly_finish_dialog_confirm_armed = false;
        self.poly_finish_dialog_px = None;
        self.canvas_context_menu_open = false;
        self.canvas_context_menu_px = None;
        self.design_line_weight_input = None;
        self.property_objects.clear();
        self.property_entities.clear();
        self.move_to_layer_dialog = None;
        self.move_to_axis_dialog = None;
        self.insert_point_at_elevation_dialog = None;
        self.measurement_start = None;
        self.measurement_end = None;
        self.batter_angle_points.clear();
        self.text_editing_enabled = false;
        self.editing_labels_id = None;
        self.pending_text.clear();
        self.text_edit_dialog_px = None;
        self.text_edit_position_frames = 0;
        self.text_edit_focus_requested = false;
        self.text_edit_created = false;
        self.slice_pending_start = None;
        self.slice_preview_navigation.reset();

        self.offset_dialog_open = false;
        self.offset_target_id = None;
        self.offset_target_ids.clear();
        self.offset_awaiting_side_pick = false;
        self.offset_project_to_rl = None;
        self.offset_preview_world.clear();
        self.offset_source_world.clear();
        self.offset_preview_screen_px.clear();
        self.offset_source_screen_px.clear();
        self.offset_preview_ranges.clear();

        self.relimit_dialog_open = false;
        self.relimit_source_id = None;
        self.relimit_awaiting_source_pick = false;
        self.relimit_waiting_for_pick = false;
        self.relimit_confirming_end = false;
        self.relimit_second_id = None;
        self.relimit_candidates.clear();
        self.relimit_intersection_3d = None;
        self.relimit_preview_from_px = None;
        self.relimit_preview_to_px = None;
        self.relimit_resize_end_px = None;

        self.fuse_segments.clear();
        self.fuse_awaiting_endpoint = None;
        self.fuse_endpoint_markers.clear();
        self.fuse_chain_tail = None;
        self.fuse_close_marker = None;
        self.split_poly_id = None;
        self.split_selected_verts = [None, None];
        self.split_poly_verts_screen_px.clear();

        self.chamfer_poly_id = None;
        self.chamfer_corner_index = None;
        self.chamfer_preview_screen_px.clear();
        self.chamfer_hover_corner_px = None;
        self.tool_hover_vertex_px = None;
        self.tool_hover_vertex_world = None;
        self.chamfer_gizmo_corner_px = None;
        self.chamfer_gizmo_bisector_px = None;
        self.chamfer_gizmo_handle_px = None;
        self.chamfer_gizmo_hovered = false;
        self.chamfer_gizmo_edge_screen_dir = None;
        self.chamfer_gizmo_drag_start_px = None;

        self.move_vertex_target = None;
        self.move_gizmo = MoveGizmoScreen::default();
        self.move_gizmo_hovered_axis = None;
        self.move_gizmo_hovered_plane = None;
        self.gizmo_drag_axis_index = None;
        self.gizmo_drag_plane_index = None;
        self.move_panel_delta = [0.0; 3];
        self.move_panel_last_preview = [0.0; 3];
        self.tool_highlight_id = None;

        self.batter_berm_dialog_open = false;
        self.batter_berm_target_id = None;
        self.batter_berm_rings_world.clear();
        self.batter_berm_source_world.clear();
        self.batter_berm_rings_screen_px.clear();
        self.batter_berm_source_screen_px.clear();
        self.batter_berm_preview_key = None;

        self.bezier_poly_id = None;
        self.bezier_poly_closed = false;
        self.bezier_selected_verts = [None, None];
        self.bezier_replace_longer = false;
        self.bezier_poly_verts_screen_px.clear();
        self.bezier_cp1_screen_px = None;
        self.bezier_cp2_screen_px = None;
        self.bezier_span_screen_px.clear();
        self.bezier_preview_screen_px.clear();
        self.bezier_dragging_cp = None;
        self.bezier_hover_cp = None;
        self.bezier_dialog_open = false;

        self.tri_hover_handles.clear();
        self.tri_selected_object_ids.clear();
        self.tri_selected_layer_ids.clear();
        self.tri_create_failure = None;
        self.tri_create_diagnostic_markers_screen_px.clear();
        self.tri_create_diagnostic_segments_screen_px.clear();
        self.triangulation_pick_target = None;
        self.viewport_pick_hover_label = None;
        self.tri_cut_poly_open = false;
        self.tri_cut_poly_awaiting_pick = false;
        self.tri_cut_poly_object_id = None;
        self.tri_cut_poly_object_name.clear();
    }

    pub(crate) fn current_preferences(&self) -> PreferencesDraft {
        PreferencesDraft {
            language: self.language,
            renderer_background_color: self.renderer_background_color,
            dark_mode: self.dark_mode,
            show_console: self.show_console,
            panel_chrome: self.panel_chrome,
            show_world_axis_gizmo: self.show_world_axis_gizmo,
            show_xy_grid: self.show_xy_grid,
            show_scale_bar: self.show_scale_bar,
            snap_poll_rate: self.snap_poll_rate,
            frame_rate_cap: self.frame_rate_cap,
            resize_frame_rate_cap: self.resize_frame_rate_cap,
            block_model_interaction_resolution_divisor: self.block_model_interaction_resolution_divisor,
            show_block_model_boundary_highlights: self.show_block_model_boundary_highlights,
            downscale_raster_previews: self.downscale_raster_previews,
            frame_counter_enabled: self.frame_counter_enabled,
            debug_chunk_coloring: self.debug_chunk_coloring,
            debug_clip_planes: self.debug_clip_planes,
            plan_orbit_sensitivity: self.plan_orbit_sensitivity,
            plan_zoom_sensitivity: self.plan_zoom_sensitivity,
            plan_invert_vertical_look: self.plan_invert_vertical_look,
            plan_invert_horizontal_look: self.plan_invert_horizontal_look,
            plan_zoom_towards_cursor: self.plan_zoom_towards_cursor,
            fly_field_of_view_degrees: self.fly_field_of_view_degrees,
            fly_mouse_look_sensitivity: self.fly_mouse_look_sensitivity,
            fly_invert_vertical_look: self.fly_invert_vertical_look,
            fly_invert_horizontal_look: self.fly_invert_horizontal_look,
            fly_near_clip_limit: self.fly_near_clip_limit,
            fly_max_clip_span: self.fly_max_clip_span,
        }
    }

    pub(crate) fn allocate_color_stop_id(&mut self) -> u64 {
        let id = self.next_color_stop_id;
        self.next_color_stop_id = self.next_color_stop_id.saturating_add(1);
        id
    }

    pub(crate) fn new() -> Self {
        Self {
            selected_handles: HashSet::new(),
            selected_drill_holes: HashSet::new(),
            hidden_handles: HashSet::new(),
            frozen_handles: HashSet::new(),
            explicitly_frozen: HashSet::new(),
            locked_layers: HashSet::new(),
            locked_rasters: HashSet::new(),
            translucent_handles: HashSet::new(),
            topology_wireframes_enabled: false,
            show_points: false,
            language: crate::app::io::default_language(),
            dark_mode: crate::app::io::default_dark_mode(),
            show_console: crate::app::io::default_show_console(),
            panel_chrome: crate::app::io::default_panel_chrome(),
            show_world_axis_gizmo: crate::app::io::default_show_world_axis_gizmo(),
            show_xy_grid: crate::app::io::default_show_xy_grid(),
            show_scale_bar: crate::app::io::default_show_scale_bar(),
            #[cfg(not(target_arch = "wasm32"))]
            newer_release: None,
            renderer_background_color: crate::app::io::default_renderer_background_color(),
            preferences_draft: None,
            snap_poll_rate: crate::app::io::default_snap_poll_rate(),
            frame_rate_cap: crate::app::io::default_frame_rate_cap(),
            resize_frame_rate_cap: crate::app::io::default_resize_frame_rate_cap(),
            block_model_interaction_resolution_divisor: crate::app::io::default_block_model_interaction_resolution_divisor(),
            show_block_model_boundary_highlights: crate::app::io::default_show_block_model_boundary_highlights(),
            downscale_raster_previews: crate::app::io::default_downscale_raster_previews(),
            frame_counter_enabled: false,
            measured_fps: None,
            debug_chunk_coloring: false,
            debug_chunk_stats: None,
            debug_clip_plane_distances: None,
            debug_clip_planes: false,
            plan_orbit_sensitivity: crate::app::io::default_plan_orbit_sensitivity(),
            plan_zoom_sensitivity: crate::app::io::default_plan_zoom_sensitivity(),
            plan_invert_vertical_look: false,
            plan_invert_horizontal_look: false,
            plan_zoom_towards_cursor: crate::app::io::default_plan_zoom_towards_cursor(),
            fly_field_of_view_degrees: crate::app::io::default_fly_field_of_view_degrees(),
            fly_mouse_look_sensitivity: crate::app::io::default_fly_mouse_look_sensitivity(),
            fly_invert_vertical_look: false,
            fly_invert_horizontal_look: false,
            fly_near_clip_limit: crate::app::io::default_fly_near_clip_limit(),
            fly_max_clip_span: crate::app::io::default_fly_max_clip_span(),
            background_busy: false,
            status_message: None,
            last_finished_task: None,
            active_tool: ActiveTool::None,
            cursors: WorkspaceCursors::default(),
            tool_line_color: [1.0, 1.0, 1.0, 1.0],
            tool_line_weight: 1.0,
            tool_hatch: ToolHatch::Clear,
            active_layer: None,
            active_drill_hole: None,
            cursor_world: None,
            #[cfg(target_arch = "wasm32")]
            new_project_dialog_open: false,
            #[cfg(target_arch = "wasm32")]
            new_project_name: String::new(),
            new_layer_dialog_open: false,
            new_layer_name: "Design".to_owned(),
            renaming_item: None,
            pending_delete_layer: None,
            pending_delete_item: None,
            pending_stroke: Vec::new(),
            circle_draft: None,
            measurement_start: None,
            measurement_end: None,
            batter_angle_points: Vec::new(),
            pending_text: String::new(),
            pending_text_height: 15.0,
            pending_text_rotation_degrees: 0.0,
            text_edit_dialog_px: None,
            text_edit_position_frames: 0,
            text_edit_focus_requested: false,
            text_edit_created: false,
            text_editing_enabled: false,
            editing_labels_id: None,
            cursor_snapped: false,
            snap_marker_px: None,
            z_level: 0.0,
            z_input: 0.0,
            cursor_screen_px: None,
            poly_finish_dialog: false,
            poly_finish_dialog_confirm_armed: false,
            poly_finish_dialog_px: None,
            canvas_context_menu_open: false,
            canvas_context_menu_px: None,
            design_line_weight_input: None,
            property_objects: Vec::new(),
            property_entities: Vec::new(),
            move_to_layer_dialog: None,
            move_to_axis_dialog: None,
            selection_has_intersections: false,
            insert_point_at_elevation_dialog: None,
            xray_enabled: false,
            vertical_exaggeration_dialog_open: false,
            vertical_exaggeration: 1.0,
            vertical_exaggeration_input: 1.,
            fly_mode_enabled: false,
            slice_pending_start: None,
            slice_mode_enabled: false,
            slice_preview_detached: false,
            slice_preview_texture: None,
            slice_preview_size_px: [440, 440],
            slice_preview_navigation: SlicePreviewNavigation::default(),
            slice_width_input: 25.0,
            slice_speed_input: 50.0,
            slice_rotate_input: 45.0,
            slice_center: [0.0; 3],
            slice_direction: [1.0, 0.0],
            slice_half_length: 0.0,
            selection_box_start_px: None,
            selection_box_current_px: None,
            drape_phase: DrapePhase::Designs,
            drape_object_ids: Vec::new(),
            offset_dialog_open: false,
            offset_target_id: None,
            offset_target_ids: Vec::new(),
            offset_angle_degrees: 60.0,
            offset_measure: OffsetMeasure::Distance,
            offset_value_input: 0.0,
            offset_awaiting_side_pick: false,
            offset_horiz_dist: 0.0,
            offset_z_delta: 0.0,
            offset_project_to_rl: None,
            offset_collide_with_triangulation: false,
            offset_preview_world: Vec::new(),
            offset_source_world: Vec::new(),
            offset_preview_screen_px: Vec::new(),
            offset_source_screen_px: Vec::new(),
            offset_preview_ranges: Vec::new(),
            offset_preview_closed: false,
            relimit_dialog_open: false,
            relimit_source_id: None,
            relimit_mode: RelimitMode::Intersect,
            relimit_value_input: 0.0,
            relimit_awaiting_source_pick: false,
            relimit_waiting_for_pick: false,
            relimit_confirming_end: false,
            relimit_candidates: Vec::new(),
            relimit_preview_from_px: None,
            relimit_preview_to_px: None,
            relimit_preview_is_extension: true,
            relimit_resize_end: TrimEnd::End,
            relimit_resize_end_px: None,
            relimit_second_id: None,
            relimit_intersection_3d: None,
            relimit_hover_end: TrimEnd::End,
            fuse_segments: Vec::new(),
            fuse_awaiting_endpoint: None,
            fuse_endpoint_markers: Vec::new(),
            fuse_chain_tail: None,
            fuse_close_marker: None,
            split_poly_id: None,
            split_selected_verts: [None; 2],
            split_poly_verts_screen_px: Vec::new(),
            chamfer_radius: 1.0,
            chamfer_segments: 8,
            chamfer_max_radius: f64::MAX,
            chamfer_poly_id: None,
            chamfer_corner_index: None,
            chamfer_preview_screen_px: Vec::new(),
            chamfer_hover_corner_px: None,
            tool_hover_vertex_px: None,
            tool_hover_vertex_world: None,
            chamfer_gizmo_corner_px: None,
            chamfer_gizmo_bisector_px: None,
            chamfer_gizmo_handle_px: None,
            chamfer_gizmo_hovered: false,
            chamfer_gizmo_edge_screen_dir: None,
            chamfer_gizmo_px_per_world: 1.0,
            chamfer_gizmo_drag_start_px: None,
            chamfer_gizmo_drag_start_radius: 0.0,
            move_vertex_target: None,
            move_gizmo: MoveGizmoScreen::default(),
            move_gizmo_hovered_axis: None,
            move_gizmo_hovered_plane: None,
            gizmo_drag_axis_index: None,
            gizmo_drag_plane_index: None,
            move_panel_delta: [0.0; 3],
            move_panel_last_preview: [f64::NAN; 3],
            tool_highlight_id: None,
            pending_text_color: [1.0, 1.0, 1.0, 1.0],
            exit_confirm_open: false,
            delete_confirm_open: false,
            can_undo: false,
            can_redo: false,
            pending_close_project: None,
            remove_project_after_close: false,
            replace_project_confirm_open: false,
            lossy_save_confirm_open: false,
            pending_discard_project: None,
            pending_discard_layer: None,
            tri_create_open: false,
            tri_create_phase: TriCreatePhase::MainDialog,
            tri_create_failure: None,
            tri_create_diagnostic_markers_screen_px: Vec::new(),
            tri_create_diagnostic_segments_screen_px: Vec::new(),
            tri_create_picker_px: None,
            tri_hover_handles: HashSet::new(),
            tri_selected_object_ids: Vec::new(),
            tri_selected_layer_ids: Vec::new(),
            tri_name_input: String::new(),
            tri_surface_type: TriSurfaceType::Surface,
            triangulation_pick_target: None,
            viewport_pick_hover_label: None,
            tri_cut_poly_open: false,
            tri_cut_poly_awaiting_pick: false,
            tri_cut_poly_tri_id: None,
            tri_cut_poly_object_id: None,
            tri_cut_poly_object_name: String::new(),
            tri_cut_poly_mode: TriPolylineClipMode::KeepInside,
            tri_cut_poly_name_input: String::new(),
            tri_cut_poly_name_auto: true,
            tri_cut_z_open: false,
            tri_cut_z_tri_id: None,
            tri_cut_z_min_input: 0.0,
            tri_cut_z_max_input: 100.0,
            tri_cut_z_name_input: String::new(),
            tri_cut_z_name_auto: true,
            tri_cut_surface_open: false,
            tri_cut_surface_target_id: None,
            tri_cut_surface_reference_id: None,
            tri_cut_surface_side: TriSurfaceCutSide::CutTop,
            tri_cut_surface_name_input: String::new(),
            tri_cut_surface_name_auto: true,
            tri_cut_pitshell_open: false,
            tri_cut_pitshell_topology_id: None,
            tri_cut_pitshell_pitshell_id: None,
            tri_cut_pitshell_name_input: String::new(),
            tri_cut_pitshell_name_auto: true,
            tri_include_solid_open: false,
            tri_include_solid_topology_id: None,
            tri_include_solid_shape_id: None,
            tri_include_solid_name_input: String::new(),
            tri_include_solid_name_auto: true,
            tri_include_solid_save_as_two: false,
            tri_include_solid_hide_old: true,
            tri_contour_open: false,
            tri_contour_tri_id: None,
            tri_contour_major_interval_input: 10.0,
            tri_contour_minor_interval_input: 2.0,
            tri_contour_major_color: [1.0, 0.5, 0.0, 1.0],
            tri_contour_minor_color: [0.8, 0.8, 0.8, 1.0],
            tri_contour_use_z_range: false,
            tri_contour_z_min_input: 0.0,
            tri_contour_z_max_input: 100.0,
            tri_contour_target_layer: None,
            tri_contour_layer_name_input: "Surface Contours".to_owned(),
            tri_contour_layer_name_auto: true,
            point_cloud_tin_open: false,
            point_cloud_tin_cloud_id: None,
            point_cloud_tin_name_input: "Surface".to_owned(),
            point_cloud_tin_max_edge: 0.0,
            point_cloud_tin_budget_is_percent: true,
            point_cloud_tin_percent: 1.0,
            point_cloud_tin_limit: 1_000_000,
            point_cloud_tin_sampler: crate::app::commands::triangulation::TerrainSampler::Adaptive,
            point_cloud_tin_candidate_mult: 2,
            point_cloud_tin_hole_fill: 0.0,
            block_model_table_pages: HashMap::new(),
            viewport_block_model_id: None,
            viewport_triangulation_id: None,
            block_model_variable_ranges: HashMap::new(),
            next_color_stop_id: FIRST_CUSTOM_COLOR_STOP_ID,
            drill_hole_color_dialog: None,
            block_model_create_open: false,
            kriging_drill_hole_id: None,
            kriging_variables: Vec::new(),
            kriging_name_input: "Kriged Block Model".to_owned(),
            kriging_lower: DVec3::ZERO,
            kriging_upper: DVec3::splat(100.0),
            kriging_cell: DVec3::splat(10.0),
            kriging_range: 100.0,
            kriging_sill: 1.0,
            kriging_nugget: 0.0,
            kriging_min_samples: 4,
            kriging_max_samples: 16,
            ore_triangulation_open: false,
            ore_block_model_id: None,
            ore_variable: String::new(),
            ore_filter_mode: OreFilterMode::GreaterOrEqual,
            ore_min_input: 0.0,
            ore_max_input: 1.0,
            ore_name_input: String::new(),
            plot_dialog: None,
            plot_preview_texture: None,
            batter_berm_dialog_open: false,
            batter_berm_target_id: None,
            batter_berm_width: 8.0,
            batter_berm_angle: 60.0,
            batter_berm_bench_height: 12.0,
            batter_berm_benches: 1,
            batter_berm_max_benches: 0,
            batter_berm_mode: BatterBermMode::Pit,
            // Down keeps the historical default (Pit + Down = inward + down).
            batter_berm_direction_up: false,
            batter_berm_rings_world: Vec::new(),
            batter_berm_source_world: Vec::new(),
            batter_berm_rings_screen_px: Vec::new(),
            batter_berm_source_screen_px: Vec::new(),
            batter_berm_preview_closed: false,
            batter_berm_preview_key: None,
            bezier_poly_id: None,
            bezier_poly_closed: false,
            bezier_selected_verts: [None; 2],
            bezier_replace_longer: false,
            bezier_cp1: [0.0; 3],
            bezier_cp2: [0.0; 3],
            bezier_segments: 16,
            bezier_poly_verts_screen_px: Vec::new(),
            bezier_cp1_screen_px: None,
            bezier_cp2_screen_px: None,
            bezier_span_screen_px: Vec::new(),
            bezier_preview_screen_px: Vec::new(),
            bezier_dragging_cp: None,
            bezier_hover_cp: None,
            bezier_dialog_open: false,
            active_property_tab: PropertyTab::Object,
            active_workspace: Workspace::Production,
            delay_products: builtin_delay_products(),
            next_delay_product_id: builtin_delay_products().len() as u64,
            new_delay_product_open: false,
            new_delay_product_delay_ms: 0,
            new_delay_product_name: String::new(),
            new_delay_product_color: NEW_DELAY_PRODUCT_COLOR,
            show_import: false,
            show_export: false,
            show_about: false,
            data_menu: DataMenu::None,
            import_source_menu: DataMenu::None,
            import_source_paths: Vec::new(),
            import_csv_preview: None,
            import_csv_error: None,
            import_drill_csv: Vec::new(),
            export_dxf_layer: false,
            export_project: None,
            export_layer: None,
            export_triangulation: None,
            export_block_model: None,
        }
    }

    /// Left-click that landed on empty space: clears the selection.
    pub(crate) fn on_canvas_click(&mut self, _world: DVec3) {
        self.clear_scene_selection();
    }

    /// Left-click that landed on entity geometry: selects it and reports the
    /// picked world point (with the geometry's true Z).
    pub(crate) fn on_canvas_pick(&mut self, handle: SceneEntityId, world: DVec3, mode: SelectionMode) {
        self.cursor_world = Some(world);
        match mode {
            SelectionMode::Replace => self.replace_selection(handle),
            SelectionMode::Add => self.add_selection(handle),
            SelectionMode::Toggle => self.toggle_selection(handle),
        }
    }

    /// Left-click that landed on one drill hole, in a workspace that works a
    /// hole at a time: selects the hole rather than the dataset holding it.
    pub(crate) fn on_drill_hole_pick(&mut self, hole: DrillHoleRef, world: DVec3, mode: SelectionMode) {
        self.cursor_world = Some(world);
        match mode {
            SelectionMode::Replace => {
                self.clear_scene_selection();
                self.selected_drill_holes.insert(hole);
            }
            SelectionMode::Add => {
                self.selected_drill_holes.insert(hole);
            }
            SelectionMode::Toggle => {
                if !self.selected_drill_holes.remove(&hole) {
                    self.selected_drill_holes.insert(hole);
                }
            }
        }
    }

    /// Apply a display action to the current selection. Returns `true` when the
    /// rendered geometry must be rebuilt.
    pub(crate) fn apply_action(&mut self, action: EditorAction) -> bool {
        match action {
            EditorAction::FreezeSelection => {
                let newly_frozen = std::mem::take(&mut self.selected_handles);
                let count = newly_frozen.len();
                self.frozen_handles.extend(newly_frozen.iter().copied());
                self.explicitly_frozen.extend(newly_frozen.iter().copied());
                self.tri_selected_object_ids.retain(|object_id| !newly_frozen.contains(&SceneEntityId::Object(*object_id)));
                crate::logging::report_completed_action(CommandReportSpec::new("Lock Selection", format!("{count} object(s)")), format!("Locked {count} object(s)"));
                // Deselecting removes selection highlights and can move a
                // cached stroke between scene streams, so rebuild geometry.
                count > 0
            }
        }
    }
}

/// How a canvas pick modifies the selection set.
#[derive(Clone, Copy)]
pub(crate) enum SelectionMode {
    Replace,
    Add,
    Toggle,
}

/// Current selection step of the Drape to Topology tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrapePhase {
    Designs,
    Topologies,
}

/// Currently active tool or `None` when no tool is engaged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActiveTool {
    None,
    MakePoint,
    MakeLine,
    MakePoly,
    MakeCircle,
    MakeText,
    DeletePoints,
    MeasureDistance,
    MeasureBatterAngle,
    OffsetElement,
    DrapeToTopology,
    RelimitLine,
    ExplodePolyline,
    FuseIntoPolyline,
    SplitAtPoints,
    Move,
    Chamfer,
    BatterBermOffset,
    Bezier,
    VerticalSlice,
}

/// Immediate commands applied to the current selection (or whole drawing).
#[derive(PartialEq, Clone, Copy)]
pub(crate) enum EditorAction {
    FreezeSelection,
}

/// Cursor interaction mode for canvas picks.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CursorMode {
    Select,
    SnapToSurface,
    SnapToLine,
    SnapToPoint,
}

/// What a click in the scene does while the Drill & Blast workspace is up.
///
/// The production workspace's [`CursorMode`] is about what a pick *snaps* to
/// while something is being drawn; this is about what a pick is *for*, which
/// is the question Drill & Blast asks instead. `Select` is a plain pick with
/// nothing armed - the same thing a click did before there were modes here -
/// and `TieHoles` is the mode a round is tied in under, which is why the delay
/// palette is only live while it is the one selected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BlastCursor {
    Select,
    TieHoles,
}

impl BlastCursor {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Select => Self::TieHoles,
            Self::TieHoles => Self::Select,
        }
    }

    /// With two cursors in the run, either way round lands on the other one.
    pub(crate) fn previous(self) -> Self {
        self.next()
    }
}

/// The cursor every workspace keeps, one field each.
///
/// A cursor mode belongs to the discipline whose tools read it - production
/// snaps for the tools it draws with, Drill & Blast arms a tie-in - so leaving
/// one workspace and coming back finds its cursor where it was left rather
/// than wherever another workspace's run last put it. Geology has no cursor
/// of its own yet, and so has nothing here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WorkspaceCursors {
    /// What a production pick snaps to.
    pub(crate) production: CursorMode,
    /// What a Drill & Blast pick is for.
    pub(crate) blast: BlastCursor,
}

impl Default for WorkspaceCursors {
    fn default() -> Self {
        Self {
            production: CursorMode::Select,
            blast: BlastCursor::Select,
        }
    }
}

impl CursorMode {
    pub(crate) fn next(self) -> Self {
        match self {
            CursorMode::Select => CursorMode::SnapToSurface,
            CursorMode::SnapToSurface => CursorMode::SnapToLine,
            CursorMode::SnapToLine => CursorMode::SnapToPoint,
            CursorMode::SnapToPoint => CursorMode::Select,
        }
    }

    pub(crate) fn previous(self) -> Self {
        match self {
            CursorMode::Select => CursorMode::SnapToPoint,
            CursorMode::SnapToSurface => CursorMode::Select,
            CursorMode::SnapToLine => CursorMode::SnapToSurface,
            CursorMode::SnapToPoint => CursorMode::SnapToLine,
        }
    }
}

/// Fill pattern for closed polylines.
#[derive(PartialEq, Clone, Copy, EnumIter, Debug, Display)]
pub(crate) enum ToolHatch {
    Clear,
    Crosses,
    Slashes,
    Solid,
}

impl ToolHatch {
    pub(crate) fn to_fill_style(self) -> FillStyle {
        match self {
            ToolHatch::Clear => FillStyle::Clear,
            ToolHatch::Crosses => FillStyle::Crosses,
            ToolHatch::Slashes => FillStyle::Slashes,
            ToolHatch::Solid => FillStyle::Solid,
        }
    }
}

/// One of the view preferences the View menu switches on and off.
///
/// The menu carries the few that are reached often enough to want a row of
/// their own; the whole set stays in the Interface preferences tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewToggle {
    Console,
    DarkMode,
    XyGrid,
}

impl ViewToggle {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Console => "Show Console",
            Self::DarkMode => "Dark Mode",
            Self::XyGrid => "XY Grid",
        }
    }

    /// Read this toggle out of a preferences snapshot.
    pub(crate) fn get(self, preferences: &PreferencesDraft) -> bool {
        match self {
            Self::Console => preferences.show_console,
            Self::DarkMode => preferences.dark_mode,
            Self::XyGrid => preferences.show_xy_grid,
        }
    }

    pub(crate) fn set(self, preferences: &mut PreferencesDraft, value: bool) {
        match self {
            Self::Console => preferences.show_console = value,
            Self::DarkMode => preferences.dark_mode = value,
            Self::XyGrid => preferences.show_xy_grid = value,
        }
    }
}

/// Commands sent from the UI back to the application core.
///
/// Each variant represents an action the user triggered through the UI
/// (button clicks, menu selections, dialog confirmations).  The app layer
/// matches on these in its event loop.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UiCommand {
    SetActiveTool(ActiveTool),
    /// Drop everything selected, and rebuild the geometry that was drawing it
    /// as selected. Switching workspaces sends this: a selection belongs to
    /// the discipline it was made in, and the tabs are not a way of carrying
    /// one across.
    ClearSelection,
    SetFlyModeEnabled(bool),
    SetSliceModeEnabled(bool),
    #[cfg(not(target_arch = "wasm32"))]
    SetSlicePreviewDetached(bool),
    NewProject,
    #[cfg(target_arch = "wasm32")]
    CreateBrowserProject {
        name: String,
    },
    OpenProject,
    #[cfg(not(target_arch = "wasm32"))]
    ActivateTrackedProject(PathBuf),
    #[cfg(target_arch = "wasm32")]
    ActivateTrackedProject(crate::model::project::ProjectId),
    #[cfg(not(target_arch = "wasm32"))]
    RemoveTrackedProject(PathBuf),
    #[cfg(target_arch = "wasm32")]
    RemoveTrackedProject(crate::model::project::ProjectId),
    /// Open the file manager on the active project's own file. Nothing the
    /// browser can do, so it is not offered there.
    #[cfg(not(target_arch = "wasm32"))]
    ShowProjectInFileManager,
    CloseStartupDialog,
    ImportOmfPaths(Vec<PathBuf>),
    ImportDxfPathsInto(Vec<PathBuf>),
    ImportTriangulationPaths(Vec<PathBuf>),
    ImportPointCloudPaths(Vec<PathBuf>),
    ImportRasterPaths(Vec<PathBuf>),
    LoadRaster(RasterTextureId),
    UnloadRaster(RasterTextureId),
    ToggleRasterVisible(RasterTextureId),
    /// Lock/unlock a raster against draping, undraping and deletion.
    ToggleRasterLocked(RasterTextureId),
    RemoveRaster(RasterTextureId),
    DrapeRaster(RasterTextureId),
    UndrapeRaster(RasterTextureId),
    /// Undrape every raster from every triangulation it covers.
    UndrapeAllRasters,
    ClearActiveTriangulationRaster,
    LoadPointCloud(PointCloudId),
    ClosePointCloud(PointCloudId),
    TogglePointCloudVisible(PointCloudId),
    RemovePointCloud(PointCloudId),
    ChooseImportSourceFiles(DataMenu),
    #[cfg(target_arch = "wasm32")]
    ClearBrowserImportSelection(DataMenu),
    ImportCsvBlockModel {
        path: PathBuf,
        mapping: CsvColumnMapping,
    },
    ExportOmf,
    ExportProjectDxf(u32),
    ExportViewportImage,
    ExportLayerDxf(LayerId),
    ExportTriangulationAs(TriangulationId, MeshFormat),
    ExportBlockModelCsv(BlockModelId),
    RequestExit,
    SaveAndExit,
    ExitWithoutSaving,
    CancelExit,
    SaveAndReplaceProject,
    DiscardAndReplaceProject,
    CancelProjectReplacement,
    ConfirmLossyProjectSave,
    CancelLossyProjectSave,
    CreateLayer {
        name: String,
    },
    /// Add a product to the Drill & Blast palette, as the New Product dialog
    /// filled it in.
    AddDelayProduct {
        delay_ms: u32,
        name: String,
        color: egui::Color32,
    },
    /// Drop one stored product from that palette.
    DeleteDelayProduct(DelayProductId),
    FinishPolyClose,
    CommitStrokeOpen,
    CommitCircleTypedRadius,
    CancelOffset,
    ConfirmDrapeSelection,
    CancelRelimit,
    ResetView,
    SetTopologyWireframes(bool),
    SetShowPoints(bool),
    SetStandardView(StandardView),
    ApplyPreferences(PreferencesDraft),
    /// Switch the UI language from the status bar's picker. Applied live and
    /// saved into the config, exactly as any other preference is.
    SetLanguage(crate::i18n::LanguageChoice),
    /// Flip one view preference from the View menu. The application reads the
    /// current value rather than the UI sending one, so the row and the
    /// Interface tab cannot disagree about what is being toggled.
    ToggleViewOption(ViewToggle),
    /// Make one block model the selection, so its properties tab is shown.
    SelectBlockModel(BlockModelId),
    SaveProject,
    #[cfg(not(target_arch = "wasm32"))]
    SaveProjectAs(u32),
    SaveAndCloseProject(u32),
    CloseProjectForce(u32),
    CancelCloseProject,
    #[cfg(not(target_arch = "wasm32"))]
    DiscardProjectChanges(u32),
    #[cfg(not(target_arch = "wasm32"))]
    RequestDiscardLayerChanges(LayerId),
    #[cfg(not(target_arch = "wasm32"))]
    DiscardLayerChanges(LayerId),
    RequestDeleteLayer(LayerId),
    DeleteLayer(LayerId),
    /// Open the destructive-deletion confirmation for a non-layer explorer item.
    RequestDeleteItem(RenameTarget),
    DuplicateLayer(LayerId),
    RenameItem {
        target: RenameTarget,
        new_name: String,
    },
    BeginRenameItem(RenameTarget),
    /// Preview the move delta without committing (updates the live document view).
    PreviewMoveDelta(DVec3),
    /// Apply a world-space delta to all selected objects.
    ApplyChamfer,
    CancelChamfer,
    ApplyBezier,
    CancelBezier,
    ApplyMoveDelta(DVec3),
    CancelMoveDelta,
    LoadLayer(LayerId),
    UnloadLayer(LayerId),
    /// Show/hide a design layer without unloading it.
    ToggleLayerVisible(LayerId),
    /// Lock/unlock every object on a design layer against selection and editing.
    ToggleLayerLocked(LayerId),
    /// Lock/unlock one scene entity against selection and editing.
    ToggleEntityLocked(SceneEntityId),
    /// Show or hide every loaded item in one explorer section.
    SetSectionVisible(ExplorerSection, bool),
    /// Lock or unlock every loaded item in one explorer section.
    SetSectionLocked(ExplorerSection, bool),
    SelectAllObjectsInLayer(LayerId),
    ActivateTriangulation(TriangulationId),
    ToggleTriangulationVisible(TriangulationId),
    CloseTriangulation(TriangulationId),
    /// Batch variants - produce a single history entry for multi-select changes.
    BatchSetObjectColor(Vec<ObjectId>, ObjectColor),
    BatchSetPolylineClosed(Vec<ObjectId>, bool),
    BatchSetObjectFill(Vec<ObjectId>, FillStyle),
    BatchSetPolylineLineWeight(Vec<ObjectId>, f32),
    MoveObjectsToLayer {
        object_ids: Vec<ObjectId>,
        target_layer: LayerId,
        copy: bool,
    },
    BatchSetAxisValue(Vec<ObjectId>, Axis, f64),
    CommitTextEdit(ObjectId, String, f64, f64, [f32; 4]),
    CancelTextEdit,
    SetTriangulationColor(TriangulationId, [f32; 4]),
    CloseCanvasContextMenu,
    LoadTriangulation(TriangulationId),
    LoadBlockModel(BlockModelId),
    CloseBlockModel(BlockModelId),
    RemoveBlockModel(BlockModelId),
    ToggleBlockModelVisible(BlockModelId),
    SetBlockModelColorVariable {
        id: BlockModelId,
        variable: String,
    },
    /// Replace the active variable's colormap wholesale. The legend edits a
    /// copy and hands the whole thing back, so one command covers moving a
    /// boundary, recolouring a band, and switching colormap kind.
    SetBlockModelColorTransfer {
        id: BlockModelId,
        transfer: ColorTransferFunction,
    },
    /// Discard the active variable's ramp and rebuild it from the data.
    ResetBlockModelColorTransfer {
        id: BlockModelId,
    },
    SetBlockModelSlice {
        id: BlockModelId,
        slice: Option<crate::model::block_model::BlockModelSlice>,
    },
    ImportDrillHole(DrillHoleSource),
    LoadDrillHole(DrillHoleId),
    CloseDrillHole(DrillHoleId),
    RemoveDrillHole(DrillHoleId),
    ToggleDrillHoleVisible(DrillHoleId),
    OpenDrillHoleColorDialog(DrillHoleId),
    SetDrillHoleColorField {
        id: DrillHoleId,
        field: Option<String>,
    },
    SetDrillHoleColorPreset {
        id: DrillHoleId,
        preset: DrillColorPreset,
    },
    SetDrillHoleColorStops {
        id: DrillHoleId,
        stops: Vec<DrillColorStop>,
    },
    SetDrillHoleCategoryColors {
        id: DrillHoleId,
        categories: Vec<DrillCategoryColor>,
    },
    OpenCreateBlockModel(Option<DrillHoleId>),
    ExecuteCreateBlockModel {
        drill_hole_id: DrillHoleId,
        variables: Vec<String>,
        name: String,
        lower: DVec3,
        upper: DVec3,
        cell: DVec3,
        range: f64,
        sill: f64,
        nugget: f64,
        min_samples: u32,
        max_samples: u32,
    },
    OpenCreateOreTriangulation,
    ExecuteCreateOreTriangulation {
        block_model_id: BlockModelId,
        variable: String,
        mode: OreFilterMode,
        min: f64,
        max: f64,
        name: String,
    },
    RemoveTriangulation(TriangulationId),
    HideSelection,
    ZoomToExtents,
    /// Dialog "Apply" pressed - begin the canvas side-pick phase.
    BeginOffsetPick {
        object_ids: Vec<ObjectId>,
        /// Absolute horizontal offset distance (sign determined by cursor).
        horiz_dist: f64,
        /// Z shift to apply to all new vertices.
        z_delta: f64,
        /// When set, overrides `horiz_dist`/`z_delta`: project each vertex
        /// individually along `(tan_angle, target_rl)` so it lands flat at
        /// `target_rl` (angled batter projection to an absolute RL).
        project_to_rl: Option<(f64, f64)>,
        /// Clamp each offset vertex to the first visible triangulation hit
        /// between the source vertex and the requested offset endpoint.
        collide_with_triangulation: bool,
    },
    RelimitLineResize {
        source_id: ObjectId,
        mode: RelimitMode,
        value: f64,
    },
    /// Triggers the app to select the first valid polyline from the selection and open the offset
    /// dialog.
    OpenOffsetDialog,
    /// Triggers the app to select the first valid line from the selection and open the relimit
    /// dialog.
    OpenRelimitDialog,
    /// Triggers the app to select the first valid polyline and open the batter berm dialog.
    OpenBatterBermDialog,
    /// Dialog "Apply" pressed - commit all batter berm rings using the current panel state.
    CommitBatterBerm,
    CancelBatterBerm,
    /// Open the Create Triangulation main dialog.
    OpenCreateTriangulation,
    /// Open the Move to dialog that sets one axis of every selected object.
    OpenMoveToAxisDialog(Axis),
    /// Insert vertices at every plan-view crossing between selected polylines.
    InsertPointsAtIntersections,
    /// Open the elevation input for inserting vertices into selected polylines.
    OpenInsertPointAtElevationDialog,
    /// Insert vertices where selected polylines cross an elevation.
    InsertPointsAtElevation {
        object_ids: Vec<ObjectId>,
        elevation: f64,
    },
    /// Run CDT on the supplied object list and add the result as a loaded triangulation.
    ExecuteCreateTriangulation {
        name: String,
        object_ids: Vec<ObjectId>,
        surface_type: TriSurfaceType,
    },
    /// Retry a failed Create Triangulation with breakline endpoints welded
    /// at the coarse (cm-scale) tolerance the failure dialog offered.
    ExecuteCreateTriangulationWithWeld {
        name: String,
        object_ids: Vec<ObjectId>,
        surface_type: TriSurfaceType,
    },
    /// Retry a failed terrain triangulation by enforcing the higher edge at
    /// conflicting plan-view crossings and omitting the lower edge segment.
    ExecuteCreateTriangulationUpperSurface {
        name: String,
        object_ids: Vec<ObjectId>,
        surface_type: TriSurfaceType,
        coarse_weld: bool,
    },
    /// Open the point cloud terrain TIN dialog (Survey menu).
    OpenPointCloudTin,
    /// Reconstruct a terrain TIN from a point cloud.
    ExecutePointCloudTin {
        cloud_id: PointCloudId,
        params: crate::app::commands::triangulation::TerrainTinParams,
    },
    /// User confirmed deletion of all selected objects via the confirm dialog.
    ConfirmDeleteSelection,
    /// Open the "Cut Triangulation by Polyline" dialog.
    OpenCutTriangulationByPolyline,
    /// Enter polyline-pick mode for the cut-by-polyline tool.
    BeginCutPolyPick,
    /// Execute the clip against the polyline boundary in XY.
    ExecuteCutTriangulationByPolyline {
        tri_id: TriangulationId,
        polyline_id: ObjectId,
        mode: TriPolylineClipMode,
        name: String,
    },
    /// Open the "Cut Triangulation by Z Range" dialog.
    OpenCutTriangulationByZ,
    /// Execute the Z-range cut, clipping faces at the boundary planes.
    ExecuteCutTriangulationByZ {
        tri_id: TriangulationId,
        z_min: f64,
        z_max: f64,
        name: String,
    },
    /// Open the "Trim to Topology" dialog.
    OpenCutTriangulationBySurface,
    /// Trim one surface against a topology in the vertical direction.
    ExecuteCutTriangulationBySurface {
        target_id: TriangulationId,
        reference_id: TriangulationId,
        side: TriSurfaceCutSide,
        name: String,
    },
    /// Open the "Cut Topology to Pit Shell" dialog.
    OpenCutTopologyByPitShell,
    /// Trim a topology to the region outside a pit shell's true 3D footprint.
    ExecuteCutTopologyByPitShell {
        topology_id: TriangulationId,
        pit_shell_id: TriangulationId,
        name: String,
    },
    /// Open the "Include Pit/Stockpile Solid" dialog.
    OpenIncludeSolidInTopology,
    /// Replace the topology footprint with a pit or stockpile solid.
    ExecuteIncludeSolidInTopology {
        topology_id: TriangulationId,
        shape_id: TriangulationId,
        name: String,
        save_as_two: bool,
        hide_old: bool,
    },
    /// Open the "Generate Contour Lines" dialog.
    OpenContourTriangulation,
    Undo,
    Redo,
    /// Execute contour generation and store lines in the requested active project layer.
    ExecuteContourTriangulation {
        tri_id: TriangulationId,
        major_interval: f64,
        minor_interval: f64,
        major_color: [f32; 4],
        minor_color: [f32; 4],
        /// Optional `(min, max)` RL band to contour instead of the full mesh.
        z_range: Option<(f64, f64)>,
        output_layer: ContourOutputLayer,
    },

    // ── Plot sheets ──
    /// Open the "Export Engineering Drawing" dialog.
    OpenPlotDialog,
    /// Set the plot scale to the smallest conventional one that fits the data.
    FitPlotScaleToData,
    /// Render and write the configured plot sheet.
    ExportPlotSheet,
}

impl UiCommand {
    /// Friendly activity-console metadata for meaningful user actions.
    ///
    /// This match is deliberately exhaustive: adding a command requires an
    /// explicit decision to report it or treat it as transient UI plumbing.
    pub(crate) fn console_report_spec(&self) -> Option<crate::logging::CommandReportSpec> {
        use crate::logging::CommandReportSpec;

        let report = |title: &str, summary: String| Some(CommandReportSpec::new(title, summary));
        match self {
            Self::SetActiveTool(_)
            | Self::ClearSelection
            | Self::CloseStartupDialog
            | Self::CancelCloseProject
            | Self::CancelExit
            | Self::CancelProjectReplacement
            | Self::CancelLossyProjectSave
            | Self::CancelOffset
            | Self::ConfirmDrapeSelection
            | Self::CancelRelimit
            | Self::ApplyPreferences(_)
            | Self::ToggleViewOption(_)
            | Self::SelectBlockModel(_)
            | Self::BeginRenameItem(_)
            | Self::PreviewMoveDelta(_)
            | Self::CancelChamfer
            | Self::CancelBezier
            | Self::CancelMoveDelta
            | Self::CancelTextEdit
            | Self::CloseCanvasContextMenu
            | Self::OpenCreateBlockModel(_)
            | Self::OpenCreateOreTriangulation
            | Self::OpenOffsetDialog
            | Self::OpenRelimitDialog
            | Self::OpenBatterBermDialog
            | Self::CancelBatterBerm
            | Self::OpenCreateTriangulation
            | Self::OpenMoveToAxisDialog(_)
            | Self::OpenInsertPointAtElevationDialog
            | Self::OpenPointCloudTin
            | Self::OpenCutTriangulationByPolyline
            | Self::BeginCutPolyPick
            | Self::OpenCutTriangulationByZ
            | Self::OpenCutTriangulationBySurface
            | Self::OpenCutTopologyByPitShell
            | Self::OpenIncludeSolidInTopology
            | Self::OpenContourTriangulation
            | Self::OpenPlotDialog
            | Self::FitPlotScaleToData
            | Self::SetBlockModelColorTransfer { .. }
            | Self::ResetBlockModelColorTransfer { .. }
            | Self::SetDrillHoleColorStops { .. }
            | Self::SetDrillHoleCategoryColors { .. }
            | Self::OpenDrillHoleColorDialog(_)
            | Self::SetBlockModelSlice { .. }
            | Self::ChooseImportSourceFiles(_)
            | Self::RequestDeleteLayer(_)
            | Self::RequestDeleteItem(_) => None,

            #[cfg(target_arch = "wasm32")]
            Self::ClearBrowserImportSelection(_) => None,

            #[cfg(not(target_arch = "wasm32"))]
            Self::RequestDiscardLayerChanges(_) => None,

            Self::SetLanguage(choice) => report("Language", choice.endonym().to_owned()),
            Self::SetFlyModeEnabled(enabled) => report("Fly Mode", if *enabled { "Enabled" } else { "Disabled" }.to_owned()),
            Self::SetSliceModeEnabled(enabled) => report("Slice Mode", if *enabled { "Enabled" } else { "Disabled" }.to_owned()),
            #[cfg(not(target_arch = "wasm32"))]
            Self::SetSlicePreviewDetached(detached) => report("Slice Preview", if *detached { "Detached" } else { "Docked" }.to_owned()),
            Self::NewProject => report("Create Project", "Untitled project".to_owned()),
            #[cfg(target_arch = "wasm32")]
            Self::CreateBrowserProject { name } => report("Create Project", name.clone()),
            Self::OpenProject => report("Open Project", "Choose one or more files".to_owned()),
            #[cfg(not(target_arch = "wasm32"))]
            Self::ActivateTrackedProject(path) => report("Activate Project", path.display().to_string()),
            #[cfg(target_arch = "wasm32")]
            Self::ActivateTrackedProject(id) => report("Activate Project", id.to_string()),
            #[cfg(not(target_arch = "wasm32"))]
            Self::RemoveTrackedProject(path) => report("Remove Project", path.display().to_string()),
            #[cfg(target_arch = "wasm32")]
            Self::RemoveTrackedProject(id) => report("Remove Project", id.to_string()),
            #[cfg(not(target_arch = "wasm32"))]
            Self::ShowProjectInFileManager => report("Show Project", "Open the containing folder".to_owned()),
            Self::ImportOmfPaths(paths) => report("Import OMF", format!("{} file(s)", paths.len())),
            Self::ImportDxfPathsInto(paths) => report("Import DXF", format!("{} file(s)", paths.len())),
            Self::ImportTriangulationPaths(paths) => report("Import Triangulation", format!("{} file(s)", paths.len())),
            Self::ImportPointCloudPaths(paths) => report("Import Point Cloud", format!("{} file(s)", paths.len())),
            Self::ImportRasterPaths(paths) => report("Import Raster", format!("{} file(s)", paths.len())),
            Self::LoadRaster(id) => report("Load Raster", format!("{id:?}")),
            Self::UnloadRaster(id) => report("Unload Raster", format!("{id:?}")),
            Self::ToggleRasterVisible(id) => report("Set Raster Visibility", format!("{id:?}")),
            Self::ToggleRasterLocked(id) => report("Set Raster Lock", format!("{id:?}")),
            Self::RemoveRaster(id) => report("Remove Raster", format!("{id:?}")),
            Self::DrapeRaster(id) => report("Drape Raster", format!("{id:?}")),
            Self::UndrapeRaster(id) => report("Undrape Raster", format!("{id:?}")),
            Self::UndrapeAllRasters => report("Undrape Rasters", "Removed from every triangulation".to_owned()),
            Self::ClearActiveTriangulationRaster => report("Clear Raster", "Removed from active triangulation".to_owned()),
            Self::LoadPointCloud(id) => report("Load Point Cloud", format!("{id:?}")),
            Self::ClosePointCloud(id) => report("Close Point Cloud", format!("{id:?}")),
            Self::TogglePointCloudVisible(id) => report("Set Point Cloud Visibility", format!("{id:?}")),
            Self::RemovePointCloud(id) => report("Remove Point Cloud", format!("{id:?}")),
            Self::ImportCsvBlockModel { path, .. } => report("Import CSV Block Model", path.display().to_string()),
            Self::ExportOmf => report("Export OMF", "All open Incline Design data".to_owned()),
            Self::ExportProjectDxf(id) => report("Export Project to DXF", format!("Project {id}")),
            Self::ExportViewportImage => report("Export Viewport Image", "Choose a destination".to_owned()),
            Self::ExportLayerDxf(id) => report("Export Layer to DXF", format!("{id:?}")),
            Self::ExportTriangulationAs(id, format) => report("Export Triangulation", format!("{id:?} · {format:?}")),
            Self::ExportBlockModelCsv(id) => report("Export Block Model CSV", format!("{id:?}")),
            Self::RequestExit => report("Exit Incline Design", "Checking unsaved work".to_owned()),
            Self::SaveAndExit => report("Save and Exit", "Saving the current project".to_owned()),
            Self::ExitWithoutSaving => report("Exit Without Saving", "Discarding unsaved changes".to_owned()),
            Self::CreateLayer { name } => report("Create Layer", name.clone()),
            Self::AddDelayProduct { delay_ms, name, .. } => report("Add Product", format!("{delay_ms} ms · {name}")),
            Self::DeleteDelayProduct(id) => report("Delete Product", format!("{id:?}")),
            Self::FinishPolyClose => report("Create Polyline", "Finish closed polyline".to_owned()),
            Self::CommitStrokeOpen => report("Create Line", "Finish open polyline".to_owned()),
            Self::CommitCircleTypedRadius => report("Create Circle", "Use typed radius".to_owned()),
            Self::ResetView => report("Reset View", "Fit to extents".to_owned()),
            Self::SetTopologyWireframes(enabled) => report("Set Topology Wireframes", if *enabled { "Shown" } else { "Hidden" }.to_owned()),
            Self::SetShowPoints(enabled) => report("Set Point Visibility", if *enabled { "Shown" } else { "Hidden" }.to_owned()),
            Self::SetStandardView(view) => report("Set Standard View", format!("{view:?}")),
            Self::SaveProject => report("Save Project", "Current project".to_owned()),
            Self::SaveAndReplaceProject => report("Save and Replace Project", "Current project".to_owned()),
            Self::DiscardAndReplaceProject => report("Discard and Replace Project", "Current project".to_owned()),
            Self::ConfirmLossyProjectSave => report("Confirm OMF Rewrite", "Save despite unsupported content".to_owned()),
            #[cfg(not(target_arch = "wasm32"))]
            Self::SaveProjectAs(id) => report("Save Project As", format!("Project {id}")),
            Self::CloseProjectForce(id) => report("Close Project", format!("Project {id}")),
            Self::SaveAndCloseProject(id) => report("Save and Close Project", format!("Project {id}")),
            #[cfg(not(target_arch = "wasm32"))]
            Self::DiscardProjectChanges(id) => report("Discard Project Changes", format!("Project {id}")),
            #[cfg(not(target_arch = "wasm32"))]
            Self::DiscardLayerChanges(id) => report("Discard Layer Changes", format!("{id:?}")),
            Self::DeleteLayer(id) => report("Delete Layer", format!("{id:?}")),
            Self::DuplicateLayer(id) => report("Duplicate Layer", format!("{id:?}")),
            Self::RenameItem { target, new_name } => report(&format!("Rename {}", target.kind_label()), format!("{target:?} to “{new_name}”")),
            Self::ApplyChamfer => report("Chamfer", "Apply to selection".to_owned()),
            Self::ApplyBezier => report("Create Bezier Curve", "Apply to selection".to_owned()),
            Self::ApplyMoveDelta(delta) => report("Move Selection", format!("{delta}")),
            Self::LoadLayer(id) => report("Set Layer Visibility", format!("{id:?} shown")),
            Self::UnloadLayer(id) => report("Set Layer Visibility", format!("{id:?} hidden")),
            Self::ToggleLayerVisible(id) => report("Set Layer Visibility", format!("{id:?}")),
            Self::ToggleLayerLocked(id) => report("Set Layer Lock", format!("{id:?}")),
            Self::ToggleEntityLocked(handle) => report("Set Entity Lock", format!("{handle:?}")),
            Self::SetSectionVisible(section, visible) => report(if *visible { "Reveal All" } else { "Hide All" }, format!("{} section", section.label())),
            Self::SetSectionLocked(section, locked) => report(if *locked { "Lock All" } else { "Unlock All" }, format!("{} section", section.label())),
            Self::SelectAllObjectsInLayer(id) => report("Select Layer Objects", format!("{id:?}")),
            Self::ActivateTriangulation(id) => report("Set Current Triangulation", format!("{id:?}")),
            Self::ToggleTriangulationVisible(id) => report("Set Triangulation Visibility", format!("{id:?}")),
            Self::CloseTriangulation(id) => report("Unload Triangulation", format!("{id:?}")),
            Self::BatchSetObjectColor(ids, _) => report("Set Object Colour", format!("{} object(s)", ids.len())),
            Self::BatchSetPolylineClosed(ids, closed) => report("Set Polyline Closed", format!("{} object(s) · {closed}", ids.len())),
            Self::BatchSetObjectFill(ids, _) => report("Set Object Fill", format!("{} object(s)", ids.len())),
            Self::BatchSetPolylineLineWeight(ids, weight) => report("Set Line Weight", format!("{} object(s) · {weight}", ids.len())),
            Self::MoveObjectsToLayer { object_ids, target_layer, copy } => report(
                if *copy { "Copy Objects to Layer" } else { "Move Objects to Layer" },
                format!("{} object(s) · {target_layer:?}", object_ids.len()),
            ),
            Self::BatchSetAxisValue(ids, axis, value) => report("Move to Axis Value", format!("{} object(s) · {} {value}", ids.len(), axis.label())),
            Self::CommitTextEdit(id, _, _, _, _) => report("Edit Text", format!("{id:?}")),
            Self::SetTriangulationColor(id, _) => report("Set Triangulation Colour", format!("{id:?}")),
            Self::LoadTriangulation(id) => report("Load Triangulation", format!("{id:?}")),
            Self::LoadBlockModel(id) => report("Load Block Model", format!("{id:?}")),
            Self::CloseBlockModel(id) => report("Unload Block Model", format!("{id:?}")),
            Self::RemoveBlockModel(id) => report("Remove Block Model", format!("{id:?}")),
            Self::ToggleBlockModelVisible(id) => report("Set Block Model Visibility", format!("{id:?}")),
            Self::SetBlockModelColorVariable { variable, .. } => report("Set Block Model Variable", variable.clone()),
            Self::ImportDrillHole(source) => report("Import Drillholes", source.display_name()),
            Self::LoadDrillHole(id) => report("Load Drillholes", format!("{id:?}")),
            Self::CloseDrillHole(id) => report("Close Drillholes", format!("{id:?}")),
            Self::RemoveDrillHole(id) => report("Remove Drillholes", format!("{id:?}")),
            Self::ToggleDrillHoleVisible(id) => report("Set Drillhole Visibility", format!("{id:?}")),
            Self::SetDrillHoleColorField { field, .. } => report("Colour Drillholes", field.clone().unwrap_or_else(|| "Uniform white".to_owned())),
            Self::SetDrillHoleColorPreset { preset, .. } => report("Set Drillhole Colour Preset", preset.label().to_owned()),
            Self::ExecuteCreateBlockModel { name, .. } => report("Create Block Model", name.clone()),
            Self::ExecuteCreateOreTriangulation { name, .. } => report("Create Ore Triangulation", name.clone()),
            Self::ExportPlotSheet => report("Export Engineering Drawing", "Choose a destination".to_owned()),
            Self::RemoveTriangulation(id) => report("Remove Triangulation", format!("{id:?}")),
            Self::HideSelection => report("Hide Selection", "Selected scene elements".to_owned()),
            Self::ZoomToExtents => report("Zoom to Extents", "Preserve view angle".to_owned()),
            Self::BeginOffsetPick { object_ids, .. } => report("Offset", format!("{} object(s)", object_ids.len())),
            Self::RelimitLineResize { source_id, .. } => report("Relimit Line", format!("{source_id:?}")),
            Self::CommitBatterBerm => report("Create Batter Berm", "Apply generated rings".to_owned()),
            Self::InsertPointsAtIntersections => report("Insert Intersection Points", "Selected polylines".to_owned()),
            Self::InsertPointsAtElevation { object_ids, elevation } => report("Insert Points at Elevation", format!("{} object(s) · Z {elevation}", object_ids.len())),
            Self::ExecuteCreateTriangulation { name, object_ids, .. }
            | Self::ExecuteCreateTriangulationWithWeld { name, object_ids, .. }
            | Self::ExecuteCreateTriangulationUpperSurface { name, object_ids, .. } => report("Create Triangulation", format!("{name} · {} object(s)", object_ids.len())),
            Self::ExecutePointCloudTin { cloud_id, .. } => report("Create Point Cloud TIN", format!("{cloud_id:?}")),
            Self::ConfirmDeleteSelection => report("Delete Selection", "Selected objects".to_owned()),
            Self::ExecuteCutTriangulationByPolyline { name, .. } => report("Cut Triangulation by Polyline", name.clone()),
            Self::ExecuteCutTriangulationByZ { name, z_min, z_max, .. } => report("Cut Triangulation by Z", format!("{name} · {z_min} to {z_max}")),
            Self::ExecuteCutTriangulationBySurface { name, .. } => report("Trim Triangulation to Surface", name.clone()),
            Self::ExecuteCutTopologyByPitShell { name, .. } => report("Cut Topology to Pit Shell", name.clone()),
            Self::ExecuteIncludeSolidInTopology { name, .. } => report("Merge Shell into Topology", name.clone()),
            Self::Undo => report("Undo", "Previous edit".to_owned()),
            Self::Redo => report("Redo", "Next edit".to_owned()),
            Self::ExecuteContourTriangulation {
                major_interval, minor_interval, ..
            } => report("Generate Contours", format!("Major {major_interval} · minor {minor_interval}")),
        }
    }
}

/// A pixel rect in physical (not logical/points) coordinates - typically the
/// portion of the window the 3D scene is actually visible through, once the
/// toolbars and status bar around it are accounted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewportRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl ViewportRect {
    pub(crate) fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width: width.max(1),
            height: height.max(1),
        }
    }
}

/// Output produced by a single UI frame.
pub(crate) struct UiFrameOutput {
    /// Delay requested by egui for its next frame. `None` means egui has no
    /// pending timed repaint; zero requests another frame immediately.
    pub(crate) repaint_after: Option<std::time::Duration>,
    pub(crate) geometry_dirty: bool,
    pub(crate) commands: Vec<UiCommand>,
    /// The scene canvas rect from this frame's layout, in physical pixels -
    /// one frame stale by the time the renderer consumes it, since the scene
    /// pass runs before this frame's egui layout. See
    /// `Graphics::apply_canvas_rect`.
    pub(crate) canvas_rect: ViewportRect,
}

/// One loaded layer shown in the explorer tree.
#[derive(Clone, Debug)]
pub(crate) struct UiLayerEntry {
    pub(crate) id: LayerId,
    pub(crate) name: String,
    /// Drawn in the viewport. Independent of loading: an unloaded layer keeps
    /// the visibility it will come back with.
    pub(crate) visible: bool,
    pub(crate) is_loaded: bool,
    pub(crate) dirty: bool,
}

/// The one open project shown in the explorer tree.
#[derive(Clone, Debug)]
pub(crate) struct UiProjectEntry {
    pub(crate) runtime_id: u32,
    pub(crate) name: String,
    pub(crate) dirty: bool,
    pub(crate) designs_dirty: bool,
    pub(crate) lossy_save_warnings: Vec<String>,
    pub(crate) is_active: bool,
    /// Persisted in browser IndexedDB despite having no host filesystem path.
    #[cfg(target_arch = "wasm32")]
    pub(crate) stored_in_browser: bool,
    pub(crate) layers: Vec<UiLayerEntry>,
    /// `None` only for a never-saved active project.
    pub(crate) path: Option<PathBuf>,
}

/// A project Incline Design remembers, listed under Recent on the welcome splash.
/// Only the active entry has a decoded [`UiProjectEntry`].
#[derive(Clone, Debug)]
pub(crate) struct UiTrackedProjectEntry {
    pub(crate) name: String,
    pub(crate) is_active: bool,
    pub(crate) dirty: bool,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) path: PathBuf,
    #[cfg(target_arch = "wasm32")]
    pub(crate) id: crate::model::project::ProjectId,
    #[cfg(target_arch = "wasm32")]
    pub(crate) stored_in_browser: bool,
}

impl UiProjectEntry {
    /// Whether Save would write anything for this project.
    ///
    /// Unsaved edits, or nowhere to have written them yet: a project opened
    /// from a file is backed by one that stays put either way, but the project
    /// the application starts on has no path, and in the browser an unedited
    /// project is still unsaved work until storage holds a copy. Save stays
    /// available in both of those so the first one can ask where to go.
    pub(crate) fn needs_save(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            self.dirty || !self.stored_in_browser
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.dirty || self.path.is_none()
        }
    }
}

/// One collapsible group of the explorer tree, as targeted by the bulk
/// show/hide/lock actions on its heading's right-click menu, and by
/// [`Workspace::opens_section`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExplorerSection {
    Designs,
    Triangulations,
    Rasters,
    PointClouds,
    BlockModels,
    DrillHoles,
}

impl ExplorerSection {
    /// Heading text, used to name the section in console reports.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Designs => "Designs",
            Self::Triangulations => "Triangulations",
            Self::Rasters => "Rasters",
            Self::PointClouds => "Point Clouds",
            Self::BlockModels => "Block Models",
            Self::DrillHoles => "Drill Holes",
        }
    }
}

/// One project-owned point cloud shown in the explorer tree.
#[derive(Clone, Debug)]
pub(crate) struct UiPointCloudEntry {
    pub(crate) id: PointCloudId,
    pub(crate) name: String,
    pub(crate) source_name: Option<String>,
    pub(crate) visible: bool,
    pub(crate) is_loaded: bool,
    pub(crate) dirty: bool,
    pub(crate) point_count: usize,
    pub(crate) bounds: Option<(DVec3, DVec3)>,
}

#[derive(Clone, Debug)]
pub(crate) struct UiRasterTextureEntry {
    pub(crate) id: RasterTextureId,
    pub(crate) name: String,
    pub(crate) source_name: Option<String>,
    pub(crate) visible: bool,
    pub(crate) is_loaded: bool,
    pub(crate) dirty: bool,
    /// Currently draped over at least one triangulation.
    pub(crate) is_draped: bool,
    pub(crate) source_size: [u32; 2],
    pub(crate) driver_name: String,
    pub(crate) projection: String,
}

#[derive(Clone, Debug)]
pub(crate) struct UiTriangulationEntry {
    pub(crate) id: TriangulationId,
    pub(crate) name: String,
    pub(crate) source_name: Option<String>,
    pub(crate) visible: bool,
    pub(crate) is_active: bool,
    pub(crate) is_loaded: bool,
    pub(crate) dirty: bool,
    /// Face colour, shown and edited by the Triangulation properties tab.
    pub(crate) color: [f32; 4],
    pub(crate) vertex_count: usize,
    pub(crate) triangle_count: usize,
    pub(crate) bounds: Option<(DVec3, DVec3)>,
}

#[derive(Clone, Debug)]
pub(crate) struct UiBlockModelEntry {
    pub(crate) id: BlockModelId,
    pub(crate) name: String,
    pub(crate) source_name: Option<String>,
    pub(crate) visible: bool,
    pub(crate) is_loaded: bool,
    pub(crate) dirty: bool,
    pub(crate) _block_count: usize,
    pub(crate) variable_count: usize,
    pub(crate) bounds: Option<(DVec3, DVec3)>,
}

#[derive(Clone, Debug)]
pub(crate) struct UiDrillHoleEntry {
    pub(crate) id: crate::model::drill_hole::DrillHoleId,
    pub(crate) name: String,
    pub(crate) source_name: Option<String>,
    pub(crate) visible: bool,
    pub(crate) is_loaded: bool,
    pub(crate) dirty: bool,
    pub(crate) hole_count: usize,
    pub(crate) field_count: usize,
    pub(crate) bounds: Option<(DVec3, DVec3)>,
}

/// Active triangulation id and face colour, as surfaced to the canvas context menu.
pub(crate) type TriangulationMenuStyle = (TriangulationId, [f32; 4]);

/// Flattened snapshot of the project tree, built each frame by the app layer.
#[derive(Clone, Debug, Default)]
pub(crate) struct UiProjectView {
    pub(crate) tracked_projects: Vec<UiTrackedProjectEntry>,
    pub(crate) projects: Vec<UiProjectEntry>,
    pub(crate) triangulations: Vec<UiTriangulationEntry>,
    pub(crate) block_models: Vec<UiBlockModelEntry>,
    pub(crate) drill_holes: Vec<UiDrillHoleEntry>,
    pub(crate) point_clouds: Vec<UiPointCloudEntry>,
    pub(crate) raster_textures: Vec<UiRasterTextureEntry>,
    pub(crate) triangulations_membership_dirty: bool,
    pub(crate) block_models_membership_dirty: bool,
    pub(crate) drill_holes_membership_dirty: bool,
    pub(crate) point_clouds_membership_dirty: bool,
    pub(crate) rasters_membership_dirty: bool,
    pub(crate) has_active_project: bool,
    pub(crate) needs_startup_dialog: bool,
    /// Full filesystem path of the currently active project, if any.
    pub(crate) active_path: Option<PathBuf>,
    /// Active triangulation id and face colour, used by the context menu.
    pub(crate) active_triangulation_for_menu: Option<TriangulationMenuStyle>,
}

/// How many remembered projects a Recent list offers before the file chooser
/// is the better tool for finding one.
pub(crate) const RECENT_PROJECT_LIMIT: usize = 10;

impl UiProjectView {
    /// The remembered projects a Recent list offers, most recently opened
    /// first.
    ///
    /// The open project is left out: it is not somewhere to go back to, and
    /// both lists that read this - the welcome splash and File > Open Recent -
    /// are ways of leaving it.
    pub(crate) fn recent_projects(&self) -> impl Iterator<Item = &UiTrackedProjectEntry> {
        self.tracked_projects.iter().filter(|entry| !entry.is_active).take(RECENT_PROJECT_LIMIT)
    }
}

/// A workspace: one of the discipline-shaped arrangements of the window the
/// menu bar's tabs switch between.
///
/// The tab decides what the viewport bar carries, the way Blender's workspace
/// tabs decide what its editors show. Production, Drill & Blast and Geology are
/// built out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Workspace {
    Production,
    DrillAndBlast,
    Geology,
}

impl Workspace {
    /// Every workspace, in the order the tabs are drawn.
    pub(crate) const ALL: [Self; 3] = [Self::Production, Self::DrillAndBlast, Self::Geology];

    pub(crate) fn label(self) -> String {
        match self {
            Self::Production => tr!("ws-production"),
            Self::DrillAndBlast => tr!("ws-drill-and-blast"),
            Self::Geology => tr!("ws-geology"),
        }
    }

    /// Whether the tab can be selected at all yet.
    pub(crate) fn implemented(self) -> bool {
        matches!(self, Self::Production | Self::DrillAndBlast | Self::Geology)
    }

    /// Whether this workspace carries the mine production tools.
    ///
    /// The drawing toolbar, the cursor modes, the design menus and the layer / Z
    /// / colour / fill settings those tools draw with all belong to production
    /// alone. A workspace without them keeps what is true everywhere: the
    /// project actions, the camera controls, the switches over how the scene is
    /// drawn, and the editors of its own discipline.
    pub(crate) fn has_production_tools(self) -> bool {
        matches!(self, Self::Production)
    }

    /// Whether the viewport bar's centre run carries anything in this
    /// workspace: production's drawing settings, or the drill hole Drill &
    /// Blast works on. A workspace with neither leaves the middle of the bar
    /// empty.
    pub(crate) fn has_centre_settings(self) -> bool {
        matches!(self, Self::Production | Self::DrillAndBlast)
    }
}

/// Identity of one product in the Drill & Blast palette.
///
/// Handed out by [`EditorState::next_delay_product_id`], so a product keeps
/// its identity as others around it are added and deleted and the palette's
/// right-click menu can name the one it acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DelayProductId(pub(crate) u64);

/// One product the Drill & Blast workspace keeps.
///
/// Interhole delays are the only kind so far: a firing time in milliseconds,
/// the name it is ordered by, and the colour a tie-in is drawn in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DelayProduct {
    pub(crate) id: DelayProductId,
    /// Milliseconds between one hole firing and the next.
    pub(crate) delay_ms: u32,
    pub(crate) name: String,
    pub(crate) color: egui::Color32,
}

impl DelayProduct {
    /// The product as the config file holds it: everything but the id, which
    /// is handed out afresh each run.
    pub(crate) fn to_stored(&self) -> crate::app::io::StoredDelayProduct {
        crate::app::io::StoredDelayProduct {
            delay_ms: self.delay_ms,
            name: self.name.clone(),
            color: self.color.to_srgba_unmultiplied(),
        }
    }
}

/// Colour a product being entered starts on, until the user picks another.
const NEW_DELAY_PRODUCT_COLOR: egui::Color32 = egui::Color32::from_rgb(0x6E, 0xC1, 0xF0);

/// Stored products as the palette holds them: ids handed out in order, and
/// the whole palette sorted by delay - which is the order it is read in, and
/// the order a hand-edited config file need not have been written in.
pub(crate) fn delay_products_from_stored(stored: &[crate::app::io::StoredDelayProduct]) -> Vec<DelayProduct> {
    let mut products: Vec<_> = stored
        .iter()
        .enumerate()
        .map(|(index, product)| DelayProduct {
            id: DelayProductId(index as u64),
            delay_ms: product.delay_ms,
            name: product.name.clone(),
            color: egui::Color32::from_rgba_unmultiplied(product.color[0], product.color[1], product.color[2], product.color[3]),
        })
        .collect();
    // Stable, so two products on the same delay keep the order they were
    // added in.
    products.sort_by_key(|product| product.delay_ms);
    products
}

/// The palette a fresh installation starts with.
pub(crate) fn builtin_delay_products() -> Vec<DelayProduct> {
    delay_products_from_stored(&crate::app::io::default_delay_products())
}

/// A section of the explorer's properties panel.
///
/// The first four are the application settings. Object is always available
/// and describes the last selection; the last three add type-specific fields
/// and only appear while there is something they apply to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropertyTab {
    Interface,
    Camera,
    Performance,
    Developer,
    Object,
    BlockModel,
    Triangulation,
    Design,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DataMenu {
    None,
    Omf,
    Dxf,
    Obj,
    Stl,
    Ply,
    Las,
    Xyz,
    Pcd,
    CsvBlockModel,
    CsvDrillHole,
    Geotiff,
}
