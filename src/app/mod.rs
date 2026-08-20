pub(crate) mod canvas; // Handles anything to do with dragging and stuff
pub(crate) mod commands; // Handles UI commands
pub(crate) mod events; // Handles window events
pub(crate) mod io; /* Handles session serialisation */
pub(crate) mod jobs; // Reusable background-compute job queue
pub(crate) mod memory; // Browser address-space budgeting for large allocations
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod release_check; // Checks the website for a newer published release
#[cfg(target_arch = "wasm32")]
pub(crate) mod web_download;
#[cfg(target_arch = "wasm32")]
pub(crate) mod web_storage;

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::Result;
use glam::DVec3;
use web_time::Instant;
#[cfg(target_arch = "wasm32")]
use winit::event_loop::EventLoopProxy;
#[cfg(target_os = "linux")]
use winit::platform::wayland::WindowAttributesExtWayland;
#[cfg(target_arch = "wasm32")]
use winit::platform::web::WindowAttributesExtWebSys;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, *},
    event_loop::ControlFlow,
    keyboard::ModifiersState,
    window::{CursorIcon, Icon, Window},
};

use crate::{
    app::commands::file::PendingFileDialog,
    model::{
        Document, LayerId, Object, ObjectId, SceneEntityId,
        block_model::{BlockModelId, BlockModelSource, OpenBlockModel},
        drill_hole::{DrillHoleSource, OpenDrillHoleDataset},
        formats::MeshFormat,
        pidb::{self, OpenProject, Workspace},
        raster::OpenRasterTexture,
        spatial::ObjectSnapIndex,
        triangulation::{OpenTriangulation, TriangulationId},
    },
    rendering::graphics::Graphics,
    ui::state::{EditorState, UiBlockModelEntry, UiDrillHoleEntry, UiLayerEntry, UiPointCloudEntry, UiProjectEntry, UiProjectView, UiTriangulationEntry},
    userspace_warn,
};
#[cfg(target_arch = "wasm32")]
use crate::{userspace_error, userspace_log};

pub(crate) const PICK_THRESHOLD_PX: f32 = 8.0;

fn rate_interval(rate: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(rate.clamp(1, 1000)))
}

fn window_icon() -> Option<Icon> {
    let image = egui_extras::image::load_svg_bytes(include_bytes!("../../res/logo.svg"), &Default::default())
        .map_err(|error| log::error!("Failed to rasterize window icon: {error}"))
        .ok()?;
    let [width, height] = image.size;
    let rgba = image.pixels.iter().flat_map(egui::Color32::to_srgba_unmultiplied).collect();

    Icon::from_rgba(rgba, width as u32, height as u32)
        .map_err(|error| log::error!("Failed to create window icon: {error}"))
        .ok()
}

struct DragState {
    object_id: ObjectId,
    before: Object,
    plane_z: f64,
    last_world: DVec3,
    moved: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum GizmoDragConstraint {
    Axis {
        axis: DVec3,
        screen_dir: (f32, f32),
        px_per_world_unit: f64,
    },
    Plane {
        axes: [DVec3; 2],
        /// Projected physical-pixel vectors produced by one world unit along
        /// each constrained axis.
        screen_basis: [(f64, f64); 2],
    },
}

pub(crate) struct GizmoDragState {
    pub(crate) constraint: GizmoDragConstraint,
    pub(crate) start_cursor_screen_px: (f32, f32),
    pub(crate) start_delta: DVec3,
}

/// A live Move preview belongs to the project whose objects were captured.
/// Keeping that identity with the originals prevents a later project switch
/// from committing or restoring the preview in a different document.
pub(crate) struct MoveSession {
    pub(crate) project_runtime_id: u32,
    pub(crate) originals: Vec<Object>,
}

/// Stable identity for one background operation. Every pending receiver owns
/// exactly one ticket, so cancellation/completion can settle only its own
/// progress state instead of decrementing a shared counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BackgroundTaskTicket(u64);

type PendingLoad<S, T> = (BackgroundTaskTicket, S, mpsc::Receiver<Result<T>>, Option<crate::logging::ConsoleReportHandle>);

/// A background task the user should see in the status bar, with the live
/// progress its worker reports.
struct ReportedTask {
    ticket: BackgroundTaskTicket,
    label: String,
    progress: crate::model::progress::Progress,
}

#[derive(Default)]
struct BackgroundTaskState {
    next_ticket: u64,
    cpu_pending: HashSet<BackgroundTaskTicket>,
    awaiting_apply: HashSet<BackgroundTaskTicket>,
    gpu_pending: HashSet<BackgroundTaskTicket>,
    /// Tickets that carry a status-bar label, oldest first: the bar reports
    /// the longest-running task rather than flickering between concurrent ones.
    reported: Vec<ReportedTask>,
}

impl BackgroundTaskState {
    fn begin(&mut self) -> BackgroundTaskTicket {
        loop {
            let ticket = BackgroundTaskTicket(self.next_ticket);
            self.next_ticket = self.next_ticket.wrapping_add(1);
            if !self.awaiting_apply.contains(&ticket) && !self.gpu_pending.contains(&ticket) && self.cpu_pending.insert(ticket) {
                return ticket;
            }
        }
    }

    fn report(&mut self, ticket: BackgroundTaskTicket, label: String, progress: crate::model::progress::Progress) {
        self.reported.push(ReportedTask { ticket, label, progress });
    }

    fn stop_reporting(&mut self, ticket: BackgroundTaskTicket) {
        self.reported.retain(|task| task.ticket != ticket);
    }

    /// The task the status bar should show, if any.
    fn status_message(&self) -> Option<crate::ui::state::StatusBarMessage> {
        let task = self.reported.first()?;
        let snapshot = task.progress.snapshot();
        Some(crate::ui::state::StatusBarMessage {
            text: task.label.clone(),
            progress: snapshot.map(|snapshot| snapshot.fraction),
            units: snapshot.and_then(|snapshot| snapshot.units),
        })
    }

    fn settle_cpu(&mut self, ticket: BackgroundTaskTicket, needs_gpu: bool) {
        self.stop_reporting(ticket);
        if !self.cpu_pending.remove(&ticket) {
            debug_assert!(false, "unknown or double-completed background ticket {ticket:?}");
            return;
        }
        let inserted = self.awaiting_apply.insert(ticket);
        debug_assert!(inserted, "background ticket was already awaiting apply");
        let removed = self.awaiting_apply.remove(&ticket);
        debug_assert!(removed, "background apply ticket disappeared");
        if needs_gpu {
            let inserted = self.gpu_pending.insert(ticket);
            debug_assert!(inserted, "background ticket was already pending GPU upload");
        }
    }

    fn cancel(&mut self, ticket: BackgroundTaskTicket) {
        self.stop_reporting(ticket);
        let removed = self.cpu_pending.remove(&ticket) || self.awaiting_apply.remove(&ticket) || self.gpu_pending.remove(&ticket);
        debug_assert!(removed, "unknown or double-cancelled background ticket {ticket:?}");
    }

    fn finish_gpu_uploads(&mut self) {
        self.gpu_pending.clear();
    }

    fn has_gpu_uploads(&self) -> bool {
        !self.gpu_pending.is_empty()
    }

    fn is_busy(&self) -> bool {
        !self.cpu_pending.is_empty() || !self.awaiting_apply.is_empty() || !self.gpu_pending.is_empty()
    }
}

pub(crate) struct App<'a> {
    close_requested: bool,
    /// Set when the renderer failed unrecoverably. Distinct from the ordinary
    /// `close_requested` flag: fatal shutdown first writes recovery copies of
    /// every dirty PIDB and waits for background writers to settle.
    fatal_shutdown: bool,
    /// Consecutive surface-validation failures survived via reconfigure.
    /// Reset by a successful frame; beyond the bound the failure is fatal.
    render_validation_recovery_attempts: u32,
    exit_after_pending_saves: bool,
    discard_changes_on_deferred_exit: bool,
    redraw_requested: bool,
    /// Wake deadline requested by egui (cursor blink, tooltip delay, etc.).
    /// Keeping it in the application event loop prevents timed repaints from
    /// being discarded when there is otherwise no window activity.
    next_ui_repaint_deadline: Option<Instant>,
    window: Option<Arc<Window>>,
    graphics: Option<Graphics<'static>>,
    #[cfg(target_arch = "wasm32")]
    web_graphics_state: GraphicsState,
    #[cfg(target_arch = "wasm32")]
    web_graphics_result: Rc<RefCell<Option<Result<Graphics<'static>>>>>,
    #[cfg(target_arch = "wasm32")]
    web_event_loop_proxy: Option<EventLoopProxy<AppEvent>>,
    #[cfg(target_arch = "wasm32")]
    browser_saves_pending: HashSet<u32>,
    #[cfg(target_arch = "wasm32")]
    browser_deletes_pending: HashSet<u32>,
    #[cfg(target_arch = "wasm32")]
    browser_delete_after_save: HashSet<u32>,
    /// Reserved browser-only source labels. Browser APIs expose only a
    /// basename, so duplicate selections receive a stable numbered suffix
    /// before they enter path-keyed scene/source controls.
    #[cfg(target_arch = "wasm32")]
    browser_source_paths: HashSet<PathBuf>,
    /// Every triangulation held in browser storage, loaded or not — the web
    /// build's equivalent of the tracked mesh files on disk. Kept beside the
    /// triangulation list rather than on `OpenTriangulation`, which is shared
    /// with the desktop build.
    #[cfg(target_arch = "wasm32")]
    browser_triangulations: Vec<crate::app::commands::triangulation::browser::BrowserTriangulationEntry>,
    /// Records whose mesh is being read out of IndexedDB, so a second
    /// double-click cannot start the same load twice.
    #[cfg(target_arch = "wasm32")]
    browser_triangulation_loads: HashSet<String>,
    /// Imports held in browser storage, decoded into the scene or not — the
    /// web build's equivalent of the tracked source files on disk. Kept beside
    /// the loaded-data lists, which are shared with the desktop build.
    #[cfg(target_arch = "wasm32")]
    browser_point_clouds: Vec<crate::app::commands::browser_data::BrowserDataEntry>,
    #[cfg(target_arch = "wasm32")]
    browser_block_models: Vec<crate::app::commands::browser_data::BrowserDataEntry>,
    #[cfg(target_arch = "wasm32")]
    browser_rasters: Vec<crate::app::commands::browser_data::BrowserDataEntry>,
    #[cfg(target_arch = "wasm32")]
    browser_drill_holes: Vec<crate::app::commands::browser_data::BrowserDataEntry>,
    /// Import records being read out of IndexedDB, across all three kinds.
    #[cfg(target_arch = "wasm32")]
    browser_data_loads: HashSet<String>,
    /// Latest non-zero window size awaiting surface reconfiguration. Resize
    /// events arrive in bursts while dragging, so intermediate sizes are
    /// deliberately replaced instead of configuring a swapchain for each one.
    pending_resize: Option<winit::dpi::PhysicalSize<u32>>,
    last_render_time: Option<Instant>,
    last_scroll_instant: Option<Instant>,
    last_snap_poll_instant: Option<Instant>,
    editor: EditorState,
    workspace: Workspace,
    /// Explorer/menu snapshot reused while its allocation-free source
    /// fingerprint is unchanged.
    ui_project_view_cache: RefCell<Option<(u64, Arc<UiProjectView>)>>,
    startup_dialog_dismissed: bool,
    triangulations: Vec<OpenTriangulation>,
    triangulation_dirs: Vec<PathBuf>,
    /// Supported mesh paths discovered in each tracked directory. Directory
    /// contents change much less often than the UI redraws, so keep filesystem
    /// scanning and sorting out of the render loop.
    triangulation_dir_entries: BTreeMap<PathBuf, Vec<PathBuf>>,
    triangulation_files: Vec<PathBuf>,
    triangulation_excluded_paths: BTreeSet<PathBuf>,
    active_triangulation: Option<TriangulationId>,
    next_triangulation_id: u64,
    block_models: Vec<OpenBlockModel>,
    block_model_files: Vec<BlockModelSource>,
    active_block_model: Option<BlockModelId>,
    next_block_model_id: u64,
    drill_holes: Vec<OpenDrillHoleDataset>,
    drill_hole_files: Vec<DrillHoleSource>,
    next_drill_hole_id: u64,
    point_clouds: Vec<crate::model::point_cloud::OpenPointCloud>,
    point_cloud_files: Vec<PathBuf>,
    next_point_cloud_id: u64,
    raster_textures: Vec<OpenRasterTexture>,
    raster_files: Vec<PathBuf>,
    next_raster_texture_id: u64,
    empty_document: Document,
    scene_document: Document,
    snap_index: ObjectSnapIndex,
    /// Set by `invalidate_geometry`; the index rebuilds lazily on the next
    /// snap/orbit query via `refresh_snap_index`.
    snap_index_dirty: bool,
    /// `Workspace::composite_key()` of the last `scene_document` build;
    /// `None` forces the next invalidation to rebuild.
    scene_document_key: Option<u64>,
    history: crate::model::History,
    modifiers: ModifiersState,
    drag: Option<DragState>,
    pub(crate) gizmo_drag: Option<GizmoDragState>,
    /// Screen position where the right mouse button was pressed (physical px).
    /// Used to distinguish a quick context-menu click from a camera orbit drag.
    right_press_px: Option<(f32, f32)>,
    /// True after a pending right press has become an active camera orbit drag.
    right_orbit_active: bool,
    /// Pointer state owned by the detached slice-preview window. Kept out of
    /// `EditorState` because it is transient native-window input, not project
    /// or tool state.
    slice_preview_cursor_px: Option<(f64, f64)>,
    slice_preview_middle_down: bool,
    pending_topology_click: Option<(SceneEntityId, DVec3)>,
    move_session_original: Option<MoveSession>,
    background_tasks: BackgroundTaskState,
    pending_triangulation_loads: Vec<PendingLoad<PathBuf, crate::model::triangulation::LoadedTriangulation>>,
    pending_block_model_loads: Vec<PendingLoad<BlockModelSource, crate::model::block_model::LoadedBlockModel>>,
    pending_drill_hole_loads: Vec<PendingLoad<DrillHoleSource, crate::model::drill_hole::LoadedDrillHoleDataset>>,
    pending_point_cloud_loads: Vec<PendingLoad<PathBuf, crate::model::point_cloud::LoadedPointCloud>>,
    pending_raster_loads: Vec<PendingLoad<PathBuf, crate::model::raster::LoadedRasterTexture>>,
    /// PIDB paths currently being parsed. They remain reserved until the job
    /// applies so a Save As/New action cannot change the bytes underneath it.
    #[cfg(not(target_arch = "wasm32"))]
    pending_pidb_open_paths: HashSet<PathBuf>,
    pub(crate) pending_file_dialogs: Vec<PendingFileDialog>,
    /// Triangulation saves/exports running on background threads; drained by
    /// `poll_saves` each frame.
    pending_saves: Vec<commands::file::PendingSave>,
    /// Heavy compute jobs (include/cut/create) running on background threads;
    /// drained by `poll_jobs` each frame.
    pending_jobs: Vec<jobs::BackgroundJob<'a>>,
    /// One-shot website release check. Failures are logged and otherwise
    /// ignored so starting Incline never depends on network availability.
    #[cfg(not(target_arch = "wasm32"))]
    pending_release_check: Option<mpsc::Receiver<Result<Option<String>>>>,
    #[cfg(target_arch = "wasm32")]
    web_import_files: Option<(crate::ui::state::DataMenu, Vec<crate::model::input::InputFile>)>,
    window_focused: bool,
}

impl<'a> Default for App<'a> {
    fn default() -> Self {
        Self {
            close_requested: false,
            fatal_shutdown: false,
            render_validation_recovery_attempts: 0,
            exit_after_pending_saves: false,
            discard_changes_on_deferred_exit: false,
            redraw_requested: false,
            next_ui_repaint_deadline: None,
            window: None,
            graphics: None,
            #[cfg(target_arch = "wasm32")]
            web_graphics_state: GraphicsState::NotStarted,
            #[cfg(target_arch = "wasm32")]
            web_graphics_result: Rc::new(RefCell::new(None)),
            #[cfg(target_arch = "wasm32")]
            web_event_loop_proxy: None,
            #[cfg(target_arch = "wasm32")]
            browser_saves_pending: HashSet::new(),
            #[cfg(target_arch = "wasm32")]
            browser_deletes_pending: HashSet::new(),
            #[cfg(target_arch = "wasm32")]
            browser_delete_after_save: HashSet::new(),
            #[cfg(target_arch = "wasm32")]
            browser_source_paths: HashSet::new(),
            #[cfg(target_arch = "wasm32")]
            browser_triangulations: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            browser_triangulation_loads: HashSet::new(),
            #[cfg(target_arch = "wasm32")]
            browser_point_clouds: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            browser_block_models: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            browser_rasters: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            browser_drill_holes: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            browser_data_loads: HashSet::new(),
            pending_resize: None,
            last_render_time: None,
            last_scroll_instant: None,
            last_snap_poll_instant: None,
            editor: EditorState::new(),
            workspace: Workspace::default(),
            ui_project_view_cache: RefCell::new(None),
            startup_dialog_dismissed: false,
            triangulations: Vec::new(),
            triangulation_dirs: Vec::new(),
            triangulation_dir_entries: BTreeMap::new(),
            triangulation_files: Vec::new(),
            triangulation_excluded_paths: BTreeSet::new(),
            active_triangulation: None,
            next_triangulation_id: 0,
            block_models: Vec::new(),
            block_model_files: Vec::new(),
            active_block_model: None,
            next_block_model_id: 0,
            drill_holes: Vec::new(),
            drill_hole_files: Vec::new(),
            next_drill_hole_id: 0,
            point_clouds: Vec::new(),
            point_cloud_files: Vec::new(),
            next_point_cloud_id: 0,
            raster_textures: Vec::new(),
            raster_files: Vec::new(),
            next_raster_texture_id: 0,
            empty_document: Document::new(),
            scene_document: Document::new(),
            snap_index: ObjectSnapIndex::default(),
            snap_index_dirty: false,
            scene_document_key: None,
            history: crate::model::History::new(),
            modifiers: ModifiersState::empty(),
            drag: None,
            gizmo_drag: None,
            right_press_px: None,
            right_orbit_active: false,
            slice_preview_cursor_px: None,
            slice_preview_middle_down: false,
            pending_topology_click: None,
            move_session_original: None,
            background_tasks: BackgroundTaskState::default(),
            pending_triangulation_loads: Vec::new(),
            pending_block_model_loads: Vec::new(),
            pending_drill_hole_loads: Vec::new(),
            pending_point_cloud_loads: Vec::new(),
            pending_raster_loads: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            pending_pidb_open_paths: HashSet::new(),
            pending_file_dialogs: Vec::new(),
            pending_saves: Vec::new(),
            pending_jobs: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            pending_release_check: None,
            #[cfg(target_arch = "wasm32")]
            web_import_files: None,
            window_focused: true,
        }
    }
}

impl<'a> App<'a> {
    #[cfg(target_os = "macos")]
    fn handle_mac_menu_action(&mut self, action: crate::mac::MacMenuAction) {
        use crate::{mac::MacMenuAction, ui::state::UiCommand};

        let command = match action {
            MacMenuAction::SaveAllPidbs => Some(UiCommand::SaveAllPidbs),
            MacMenuAction::NewPidb => Some(UiCommand::NewPidb),
            MacMenuAction::OpenPidb => Some(UiCommand::OpenPidb),
            MacMenuAction::OpenImport => {
                self.editor.show_import = true;
                self.editor.show_export = false;
                None
            }
            MacMenuAction::OpenTriangulationFolder => Some(UiCommand::OpenTriangulationFolder),
            MacMenuAction::OpenExport => {
                self.editor.show_import = false;
                self.editor.show_export = true;
                None
            }
            MacMenuAction::ExportViewportImage => Some(UiCommand::ExportViewportImage),
            MacMenuAction::OpenPlotDialog => Some(UiCommand::OpenPlotDialog),
            MacMenuAction::OpenPreferences => Some(UiCommand::OpenPreferences),
            MacMenuAction::RequestExit => Some(UiCommand::RequestExit),
            MacMenuAction::ToggleXyGrid => Some(UiCommand::SetShowXyGrid(!self.editor.show_xy_grid)),
            MacMenuAction::ToggleScaleBar => Some(UiCommand::SetShowScaleBar(!self.editor.show_scale_bar)),
            MacMenuAction::ToggleDarkMode => Some(UiCommand::SetDarkMode(!self.editor.dark_mode)),
            MacMenuAction::ToggleConsole => Some(UiCommand::SetShowConsole(!self.editor.show_console)),
            MacMenuAction::InsertPointsAtIntersections => Some(UiCommand::InsertPointsAtIntersections),
            MacMenuAction::OpenInsertPointAtElevation => Some(UiCommand::OpenInsertPointAtElevationDialog),
            MacMenuAction::OpenSetSelectionZ => Some(UiCommand::OpenSetSelectionZValueDialog),
            MacMenuAction::OpenCreateTriangulation => Some(UiCommand::OpenCreateTriangulation),
            MacMenuAction::OpenCutTriangulationByPolygon => Some(UiCommand::OpenCutTriangulationByPolygon),
            MacMenuAction::OpenCutTriangulationByZ => Some(UiCommand::OpenCutTriangulationByZ),
            MacMenuAction::OpenCutTriangulationBySurface => Some(UiCommand::OpenCutTriangulationBySurface),
            MacMenuAction::OpenCutTopologyByPitShell => Some(UiCommand::OpenCutTopologyByPitShell),
            MacMenuAction::OpenIncludeSolidInTopology => Some(UiCommand::OpenIncludeSolidInTopology),
            MacMenuAction::OpenContourTriangulation => Some(UiCommand::OpenContourTriangulation),
            MacMenuAction::OpenPointCloudTin => Some(UiCommand::OpenPointCloudTin),
            MacMenuAction::OpenCreateBlockModel => Some(UiCommand::OpenCreateBlockModel(None)),
            MacMenuAction::OpenCreateOreTriangulation => Some(UiCommand::OpenCreateOreTriangulation),
        };

        if let Some(command) = command {
            self.handle_ui_commands(vec![command]);
        } else {
            self.redraw_requested = true;
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn new() -> Result<Self> {
        let mut app = App::default();

        let session = match io::load_session() {
            Ok(session) => session,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => io::Session::default(),
            Err(e) => {
                userspace_warn!("Failed to load session file: {e}");
                io::Session::default()
            }
        };
        let config = match io::load_config() {
            Ok(config) => config,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => io::Config::default(),
            Err(e) => {
                userspace_warn!("Failed to load config file: {e}");
                io::Config::default()
            }
        };

        app.load_session_projects(&session);
        app.apply_config(config);

        Ok(app)
    }

    fn apply_config(&mut self, config: io::Config) {
        self.editor.dark_mode = config.dark_mode;
        self.editor.show_console = config.show_console;
        self.editor.show_world_axis_gizmo = config.show_world_axis_gizmo;
        self.editor.show_xy_grid = config.show_xy_grid;
        self.editor.show_scale_bar = config.show_scale_bar;
        self.editor.renderer_background_color = config.renderer_background_color;
        self.editor.snap_poll_rate = config.snap_poll_rate.clamp(5, 1000);
        self.editor.frame_rate_cap = config.frame_rate_cap.clamp(20, 1000);
        self.editor.resize_frame_rate_cap = config.resize_frame_rate_cap.clamp(20, 1000);
        self.editor.block_model_interaction_resolution_divisor = config.block_model_interaction_resolution_divisor.clamp(1, 64);
        self.editor.show_block_model_boundary_highlights = config.show_block_model_boundary_highlights;
        self.editor.downscale_raster_previews = config.downscale_raster_previews;
        self.editor.frame_counter_enabled = config.frame_counter_enabled;
        self.editor.debug_chunk_coloring = config.debug_chunk_coloring;
        self.editor.debug_clip_planes = config.debug_clip_planes;
        self.editor.plan_orbit_sensitivity = io::finite_clamped(config.plan_orbit_sensitivity, 0.0001, 0.02, io::default_plan_orbit_sensitivity());
        self.editor.plan_zoom_sensitivity = io::finite_clamped(config.plan_zoom_sensitivity, 0.0001, 0.05, io::default_plan_zoom_sensitivity());
        self.editor.plan_invert_vertical_look = config.plan_invert_vertical_look;
        self.editor.plan_invert_horizontal_look = config.plan_invert_horizontal_look;
        self.editor.plan_zoom_towards_cursor = config.plan_zoom_towards_cursor;
        self.editor.fly_field_of_view_degrees = io::finite_clamped(config.fly_field_of_view_degrees, 20.0, 120.0, io::default_fly_field_of_view_degrees());
        self.editor.fly_mouse_look_sensitivity = io::finite_clamped(config.fly_mouse_look_sensitivity, 0.0001, 0.02, io::default_fly_mouse_look_sensitivity());
        self.editor.fly_invert_vertical_look = config.fly_invert_vertical_look;
        self.editor.fly_invert_horizontal_look = config.fly_invert_horizontal_look;
        self.editor.fly_near_clip_limit = io::finite_clamped(config.fly_near_clip_limit, 0.01, 100.0, io::default_fly_near_clip_limit());
        self.editor.fly_max_clip_span = io::finite_clamped(config.fly_max_clip_span, 100.0, 1_000_000.0, io::default_fly_max_clip_span());
        self.configure_graphics_camera_preferences();
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn web_new(event_loop_proxy: EventLoopProxy<AppEvent>) -> Result<Self> {
        let mut app = App {
            web_event_loop_proxy: Some(event_loop_proxy.clone()),
            ..App::default()
        };
        match io::load_config() {
            Ok(config) => app.apply_config(config),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => userspace_warn!("Failed to load browser preferences: {error}"),
        }
        crate::app::web_storage::install_dirty_guard();
        crate::app::web_storage::install_paste_listener(event_loop_proxy.clone());
        // Each restore is its own task: they are independent reads, and running
        // them concurrently means one slow store does not hold up the rest of
        // the explorer. Entries appear as each one lands.
        {
            let proxy = event_loop_proxy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = crate::app::web_storage::load_session_projects().await;
                let _ = proxy.send_event(AppEvent::BrowserProjectsRestored(result));
            });
        }
        {
            let proxy = event_loop_proxy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = crate::app::web_storage::list_assets(crate::app::web_storage::BrowserStore::Triangulations).await;
                let _ = proxy.send_event(AppEvent::BrowserTriangulationsRestored(result));
            });
        }
        for kind in crate::app::commands::browser_data::BrowserDataKind::ALL {
            let proxy = event_loop_proxy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = crate::app::web_storage::list_assets(kind.store()).await;
                let _ = proxy.send_event(AppEvent::BrowserDataRestored { kind, result });
            });
        }
        Ok(app)
    }

    #[cfg(target_arch = "wasm32")]
    fn web_graphics_initialization(&mut self, window: Arc<Window>) {
        if !matches!(&self.web_graphics_state, GraphicsState::NotStarted) {
            return;
        }

        let Some(proxy) = self.web_event_loop_proxy.clone() else {
            let err = "browser event-loop is unavailable".to_string();
            userspace_error!("{err}");
            crate::show_web_startup_error(&err);

            self.web_graphics_state = GraphicsState::Failed;
            self.close_requested = true;
            return;
        };

        self.web_graphics_state = GraphicsState::Initializing;

        let result_slot = Rc::clone(&self.web_graphics_result);

        wasm_bindgen_futures::spawn_local(async move {
            let result = Graphics::new(window).await;

            *result_slot.borrow_mut() = Some(result);

            let _ = proxy.send_event(AppEvent::GraphicsInitializationFinished);
        });
    }

    fn active_document(&self) -> &Document {
        self.workspace.active_document().unwrap_or(&self.empty_document)
    }

    pub(crate) fn activate_project_for_object(&mut self, object_id: ObjectId) -> bool {
        let Some(index) = self.workspace.project_index_for_object(object_id) else {
            return false;
        };
        self.activate_project_index(index);
        true
    }

    pub(crate) fn activate_project_for_layer(&mut self, layer_id: LayerId) -> bool {
        let Some(index) = self.workspace.project_index_for_layer(layer_id) else {
            return false;
        };
        self.activate_project_index(index);
        true
    }

    fn active_layer(&self) -> Option<LayerId> {
        self.editor.active_layer.and_then(|layer| {
            self.workspace
                .active_project()
                .and_then(|project| (project.loaded_layers.contains(&layer) && project.pidb.document.layer(layer).is_some()).then_some(layer))
        })
    }

    fn editing_ready(&self) -> bool {
        self.workspace.has_active_project() && !self.editor.fly_mode_enabled
    }

    fn set_active_project(&mut self, project: OpenProject) {
        let index = self.workspace.add_and_activate(project);
        self.clear_editor_transient_state();
        self.history.activate(self.workspace.projects[index].runtime_id);
        self.invalidate_geometry();
        self.persist_session();
    }

    fn activate_project_index(&mut self, index: usize) {
        if self.workspace.active_index == Some(index) {
            return;
        }
        let active_tool = self.editor.active_tool;
        self.clear_editor_transient_state();
        // Object interaction is allowed to retarget the current tool to a
        // different PIDB. Its project-specific preview state was cleared
        // above, but the chosen tool itself remains armed.
        self.editor.active_tool = active_tool;
        self.workspace.set_active_index(index);
        self.history.activate(self.workspace.projects[index].runtime_id);
        self.persist_session();
        self.invalidate_overlay();
    }

    fn clear_editor_transient_state(&mut self) {
        // Resolve document-backed drafts while their source identity is still
        // available. These helpers locate the owning project explicitly, so
        // this is also safe when a newly opened project has already become
        // active.
        if self.has_pending_move_delta() {
            self.restore_move_session_original();
        }
        if self.editor.text_editing_enabled {
            self.cancel_text_edit();
        }
        self.editor.clear_project_transients();
        self.pending_topology_click = None;
        // Clear any in-progress move session so it cannot bleed into the new PIDB.
        self.move_session_original = None;
        self.drag = None;
        self.gizmo_drag = None;
        self.editor.gizmo_drag_axis_index = None;
        self.editor.gizmo_drag_plane_index = None;
    }

    fn invalidate_geometry(&mut self) {
        // Many of the ~90 invalidation sites fire for editor-state reasons
        // (selection, tool changes) with the documents untouched; the
        // composite clone and snap index only need refreshing when the
        // workspace contents actually changed.
        let composite_key = self.workspace.composite_key();
        if Some(composite_key) != self.scene_document_key {
            self.scene_document = self.workspace.scene_document();
            self.scene_document_key = Some(composite_key);
            // The snap index rebuild is deferred to the next snap/orbit
            // query: many edits never snap before the next edit, and the
            // BVH build is the expensive part.
            self.snap_index_dirty = true;
        }
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.invalidate_geometry();
        }
        self.redraw_requested = true;
    }

    /// Request a redraw for topology-only style/selection changes without
    /// rebuilding the document vector scene.
    ///
    /// Triangulations and block models render from their own per-item GPU
    /// caches (`triangulation_gpu` / `block_model_gpu`), which re-sync every
    /// frame with per-id dirty checks.
    fn request_topology_redraw(&mut self) {
        self.redraw_requested = true;
    }

    /// Request a topology redraw and refresh cached scene bounds, without
    /// rebuilding the document vector scene.
    fn invalidate_topology_bounds_and_redraw(&mut self) {
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.invalidate_scene_bounds();
        }
        self.request_topology_redraw();
    }

    /// Rebuild the snap index from the current scene document if an edit
    /// invalidated it. Call before handing `self.snap_index` to a query.
    fn refresh_snap_index(&mut self) {
        if self.snap_index_dirty {
            self.snap_index = ObjectSnapIndex::build(&self.scene_document);
            self.snap_index_dirty = false;
        }
    }

    fn invalidate_overlay(&mut self) {
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.invalidate_overlay();
        }
        self.redraw_requested = true;
    }

    pub(crate) fn begin_topology_load(&mut self) -> BackgroundTaskTicket {
        let ticket = self.background_tasks.begin();
        self.update_background_task_cursor();
        self.redraw_requested = true;
        ticket
    }

    /// Begin a background task that reports itself in the status bar. The
    /// returned [`Progress`](crate::model::progress::Progress) is cloned into
    /// the worker, which reports through it; the bar samples it each frame and
    /// stops showing the task when its ticket settles or is cancelled.
    pub(crate) fn begin_reported_task(&mut self, label: impl Into<String>) -> (BackgroundTaskTicket, crate::model::progress::Progress) {
        let ticket = self.begin_topology_load();
        let progress = crate::model::progress::Progress::new();
        self.background_tasks.report(ticket, label.into(), progress.clone());
        (ticket, progress)
    }

    /// Point the status bar at the longest-running reported task (or clear it
    /// once none are left). Called once per frame after the `poll_*` drains.
    pub(crate) fn refresh_status_message(&mut self) {
        let message = self.background_tasks.status_message();
        // Workers only wake the loop when they finish, so keep redrawing while
        // one is running: otherwise a determinate bar would freeze part-way
        // and only jump to its final value at the end.
        self.redraw_requested |= message.is_some();
        self.editor.set_status_message(message);
    }

    /// Move one CPU ticket through the UI-apply phase and either settle it or
    /// retain it until the renderer confirms its GPU upload is complete.
    pub(crate) fn finish_background_task(&mut self, ticket: BackgroundTaskTicket, needs_gpu: bool) {
        self.background_tasks.settle_cpu(ticket, needs_gpu);
        self.update_background_task_cursor();
    }

    pub(crate) fn cancel_background_task(&mut self, ticket: BackgroundTaskTicket) {
        self.background_tasks.cancel(ticket);
        self.update_background_task_cursor();
    }

    /// GPU-upload completion for the load pipeline: called after a render in
    /// which all renderer upload queues are empty.
    pub(crate) fn finish_topology_load(&mut self) {
        self.background_tasks.finish_gpu_uploads();
        self.update_background_task_cursor();
    }

    pub(crate) fn topology_uploads_pending(&self) -> bool {
        self.background_tasks.has_gpu_uploads()
    }

    pub(crate) fn background_tasks_pending(&self) -> bool {
        self.background_tasks.is_busy()
    }

    fn update_background_task_cursor(&self) {
        if let Some(window) = &self.window {
            window.set_cursor(if self.background_tasks.is_busy() { CursorIcon::Progress } else { CursorIcon::Default });
        }
    }

    fn fit_view_to_extents(&mut self) {
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
    }

    /// One definition of scene emptiness for load/apply and camera fitting.
    /// Evaluate immediately before installing a completed async result so
    /// concurrent loaders cannot all act on a stale start-time snapshot.
    pub(crate) fn scene_has_renderables(&self) -> bool {
        self.workspace
            .projects
            .iter()
            .any(|project| project.pidb.document.objects().iter().any(|object| project.loaded_layers.contains(&object.layer())))
            || !self.triangulations.is_empty()
            || !self.block_models.is_empty()
            || !self.drill_holes.is_empty()
            || !self.point_clouds.is_empty()
            || !self.raster_textures.is_empty()
    }

    fn teardown_window(&mut self) {
        #[cfg(target_arch = "wasm32")]
        crate::app::web_storage::set_dirty(false);
        self.graphics = None;
        self.window = None;
        self.pending_resize = None;
        self.last_render_time = None;
        self.redraw_requested = false;
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn sync_slice_preview_window(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if !self.editor.slice_preview_detached || self.graphics.as_ref().is_some_and(|graphics| graphics.slice_preview_window_id().is_some()) {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(format!("{} — Slice preview · Wheel zoom · Middle-drag pan · F fit", crate::APP_NAME))
            .with_window_icon(window_icon())
            .with_min_inner_size(winit::dpi::PhysicalSize::new(320, 240))
            .with_inner_size(winit::dpi::PhysicalSize::new(800, 700));
        #[cfg(target_os = "linux")]
        let attributes = attributes.with_name(crate::APP_ID, crate::APP_ID);
        let result = event_loop.create_window(attributes).map(Arc::new).map_err(anyhow::Error::from).and_then(|window| {
            self.graphics
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("renderer is not initialized"))?
                .open_slice_preview(window)
        });
        if let Err(error) = result {
            log::error!("Failed to detach top-down preview: {error:#}");
            self.editor.slice_preview_detached = false;
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn sync_slice_preview_window(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        self.editor.slice_preview_detached = false;
    }

    fn project_view_key(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.workspace.active_index.hash(&mut hasher);
        self.startup_dialog_dismissed.hash(&mut hasher);
        for project in &self.workspace.projects {
            project.runtime_id.hash(&mut hasher);
            project.path.hash(&mut hasher);
            #[cfg(target_arch = "wasm32")]
            matches!(project.persistence, crate::model::pidb::ProjectPersistence::BrowserRecord(_)).hash(&mut hasher);
            project.pidb.metadata.name.hash(&mut hasher);
            project.has_unsaved_changes().hash(&mut hasher);
            // Edits and successful async save completions can each change the
            // per-layer dirty set independently.
            project.pidb.document.revision().hash(&mut hasher);
            project.savepoint_revision().hash(&mut hasher);
            let mut loaded_layers: Vec<_> = project.loaded_layers.iter().copied().collect();
            loaded_layers.sort_unstable_by_key(|layer| layer.0);
            loaded_layers.hash(&mut hasher);
            for layer in project.pidb.document.layers() {
                layer.id.hash(&mut hasher);
                layer.name.hash(&mut hasher);
            }
        }

        self.triangulation_files.hash(&mut hasher);
        self.triangulation_dirs.hash(&mut hasher);
        self.triangulation_dir_entries.hash(&mut hasher);
        self.active_triangulation.hash(&mut hasher);
        for triangulation in &self.triangulations {
            triangulation.id.hash(&mut hasher);
            triangulation.name.hash(&mut hasher);
            triangulation.path.hash(&mut hasher);
            (triangulation.visible && !self.editor.hidden_handles.contains(&triangulation.entity_id())).hash(&mut hasher);
            triangulation.is_saved.hash(&mut hasher);
            triangulation.raster_texture.hash(&mut hasher);
            triangulation.color.map(f32::to_bits).hash(&mut hasher);
        }

        self.block_model_files.hash(&mut hasher);
        self.active_block_model.hash(&mut hasher);
        for model in &self.block_models {
            model.id.hash(&mut hasher);
            model.name.hash(&mut hasher);
            model.source.hash(&mut hasher);
            model.visible.hash(&mut hasher);
            model.renderable_block_indices.len().hash(&mut hasher);
            model.model.color_variables().into_iter().filter(|variable| !variable.special).count().hash(&mut hasher);
        }

        self.drill_hole_files.hash(&mut hasher);
        for dataset in &self.drill_holes {
            dataset.id.hash(&mut hasher);
            dataset.name.hash(&mut hasher);
            dataset.source.hash(&mut hasher);
            dataset.visible.hash(&mut hasher);
            dataset.dataset.holes.len().hash(&mut hasher);
            dataset.dataset.fields.len().hash(&mut hasher);
        }

        self.point_cloud_files.hash(&mut hasher);
        for cloud in &self.point_clouds {
            cloud.id.hash(&mut hasher);
            cloud.name.hash(&mut hasher);
            cloud.path.hash(&mut hasher);
            cloud.visible.hash(&mut hasher);
            cloud.points.len().hash(&mut hasher);
        }

        self.raster_files.hash(&mut hasher);
        for raster in &self.raster_textures {
            raster.id.hash(&mut hasher);
            raster.name.hash(&mut hasher);
            raster.path.hash(&mut hasher);
            raster.source_size.hash(&mut hasher);
            raster.driver_name.hash(&mut hasher);
            raster.projection.hash(&mut hasher);
        }
        hasher.finish()
    }

    fn project_view(&self) -> Arc<UiProjectView> {
        let key = self.project_view_key();
        if let Some((cached_key, view)) = self.ui_project_view_cache.borrow().as_ref()
            && *cached_key == key
        {
            return Arc::clone(view);
        }
        let projects: Vec<UiProjectEntry> = self
            .workspace
            .projects
            .iter()
            .enumerate()
            .map(|(index, project)| {
                let dirty_layers = project.dirty_layer_ids();
                UiProjectEntry {
                    runtime_id: project.runtime_id,
                    name: project.pidb.metadata.name.clone(),
                    dirty: project.has_unsaved_changes(),
                    is_active: self.workspace.active_index == Some(index),
                    #[cfg(target_arch = "wasm32")]
                    stored_in_browser: matches!(project.persistence, crate::model::pidb::ProjectPersistence::BrowserRecord(_)),
                    path: project.path.clone(),
                    layers: project
                        .pidb
                        .document
                        .layers()
                        .iter()
                        .map(|layer| UiLayerEntry {
                            id: layer.id,
                            name: layer.name.clone(),
                            is_loaded: project.loaded_layers.contains(&layer.id),
                            dirty: dirty_layers.contains(&layer.id),
                        })
                        .collect(),
                }
            })
            .collect();
        let loaded_by_path: BTreeMap<PathBuf, &OpenTriangulation> = self.triangulations.iter().map(|tri| (tri.path.clone(), tri)).collect();
        let mut triangulations = Vec::new();
        // Individually-opened files — no group, shown flat and removable.
        let mut seen_paths: BTreeSet<PathBuf> = BTreeSet::new();
        for path in &self.triangulation_files {
            if seen_paths.contains(path) {
                continue;
            }
            seen_paths.insert(path.clone());
            if let Some(tri) = loaded_by_path.get(path) {
                triangulations.push(UiTriangulationEntry {
                    id: Some(tri.id),
                    name: tri.name.clone(),
                    visible: tri.visible && !self.editor.hidden_handles.contains(&tri.entity_id()),
                    is_active: self.active_triangulation == Some(tri.id),
                    is_loaded: true,
                    is_saved: tri.is_saved,
                    path: path.clone(),
                    group: None,
                });
            } else {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
                triangulations.push(UiTriangulationEntry {
                    id: None,
                    name,
                    visible: false,
                    is_active: false,
                    is_loaded: false,
                    is_saved: true,
                    path: path.clone(),
                    group: None,
                });
            }
        }
        // Directory-scanned files — grouped under their source dir.
        for dir in &self.triangulation_dirs {
            if let Some(entries) = self.triangulation_dir_entries.get(dir) {
                for path in entries {
                    if !seen_paths.contains(path) {
                        seen_paths.insert(path.clone());
                        if let Some(tri) = loaded_by_path.get(path) {
                            triangulations.push(UiTriangulationEntry {
                                id: Some(tri.id),
                                name: tri.name.clone(),
                                visible: tri.visible && !self.editor.hidden_handles.contains(&tri.entity_id()),
                                is_active: self.active_triangulation == Some(tri.id),
                                is_loaded: true,
                                is_saved: tri.is_saved,
                                path: path.clone(),
                                group: Some(dir.clone()),
                            });
                        } else {
                            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_owned();
                            triangulations.push(UiTriangulationEntry {
                                id: None,
                                name,
                                visible: false,
                                is_active: false,
                                is_loaded: false,
                                is_saved: true,
                                path: path.clone(),
                                group: Some(dir.clone()),
                            });
                        }
                    }
                }
            }
        }
        // Include in-memory (generated) triangulations not covered by any file/dir list.
        for tri in &self.triangulations {
            if !seen_paths.contains(&tri.path) {
                triangulations.push(UiTriangulationEntry {
                    id: Some(tri.id),
                    name: tri.name.clone(),
                    visible: tri.visible && !self.editor.hidden_handles.contains(&tri.entity_id()),
                    is_active: self.active_triangulation == Some(tri.id),
                    is_loaded: true,
                    is_saved: tri.is_saved,
                    path: tri.path.clone(),
                    group: None,
                });
            }
        }
        let loaded_block_by_path: BTreeMap<PathBuf, &OpenBlockModel> = self.block_models.iter().map(|model| (model.source.path.clone(), model)).collect();
        let mut block_models = Vec::new();
        let mut seen_block_models = BTreeSet::new();
        for source in &self.block_model_files {
            if !seen_block_models.insert(source.path.clone()) {
                continue;
            }
            if let Some(model) = loaded_block_by_path.get(&source.path) {
                block_models.push(UiBlockModelEntry {
                    id: Some(model.id),
                    name: model.name.clone(),
                    source: model.source.clone(),
                    visible: model.visible,
                    is_active: self.active_block_model == Some(model.id),
                    is_loaded: true,
                    _block_count: model.renderable_block_indices.len(),
                    variable_count: model.model.color_variables().into_iter().filter(|variable| !variable.special).count(),
                });
            } else {
                let name = source.path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_owned();
                block_models.push(UiBlockModelEntry {
                    id: None,
                    name,
                    source: source.clone(),
                    visible: false,
                    is_active: false,
                    is_loaded: false,
                    _block_count: 0,
                    variable_count: 0,
                });
            }
        }
        for model in &self.block_models {
            if !seen_block_models.contains(&model.source.path) {
                block_models.push(UiBlockModelEntry {
                    id: Some(model.id),
                    name: model.name.clone(),
                    source: model.source.clone(),
                    visible: model.visible,
                    is_active: self.active_block_model == Some(model.id),
                    is_loaded: true,
                    _block_count: model.renderable_block_indices.len(),
                    variable_count: model.model.color_variables().into_iter().filter(|variable| !variable.special).count(),
                });
            }
        }
        let loaded_drill_holes: BTreeMap<DrillHoleSource, &OpenDrillHoleDataset> = self.drill_holes.iter().map(|dataset| (dataset.source.clone(), dataset)).collect();
        let mut drill_holes = Vec::new();
        let mut seen_drill_holes = BTreeSet::new();
        for source in &self.drill_hole_files {
            if !seen_drill_holes.insert(source.clone()) {
                continue;
            }
            if let Some(dataset) = loaded_drill_holes.get(source) {
                drill_holes.push(UiDrillHoleEntry {
                    id: Some(dataset.id),
                    name: dataset.name.clone(),
                    visible: dataset.visible,
                    is_loaded: true,
                    source: source.clone(),
                    hole_count: dataset.dataset.holes.len(),
                    field_count: dataset.dataset.fields.len(),
                });
            } else {
                drill_holes.push(UiDrillHoleEntry {
                    id: None,
                    name: source.display_name(),
                    visible: false,
                    is_loaded: false,
                    source: source.clone(),
                    hole_count: 0,
                    field_count: 0,
                });
            }
        }

        let loaded_cloud_by_path: BTreeMap<PathBuf, &crate::model::point_cloud::OpenPointCloud> = self.point_clouds.iter().map(|cloud| (cloud.path.clone(), cloud)).collect();
        let mut point_clouds = Vec::new();
        let mut seen_point_clouds = BTreeSet::new();
        for path in &self.point_cloud_files {
            if !seen_point_clouds.insert(path.clone()) {
                continue;
            }
            if let Some(cloud) = loaded_cloud_by_path.get(path) {
                point_clouds.push(UiPointCloudEntry {
                    id: Some(cloud.id),
                    name: cloud.name.clone(),
                    path: path.clone(),
                    visible: cloud.visible,
                    is_loaded: true,
                    point_count: cloud.points.len(),
                });
            } else {
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_owned();
                point_clouds.push(UiPointCloudEntry {
                    id: None,
                    name,
                    path: path.clone(),
                    visible: false,
                    is_loaded: false,
                    point_count: 0,
                });
            }
        }
        for cloud in &self.point_clouds {
            if !seen_point_clouds.contains(&cloud.path) {
                point_clouds.push(UiPointCloudEntry {
                    id: Some(cloud.id),
                    name: cloud.name.clone(),
                    path: cloud.path.clone(),
                    visible: cloud.visible,
                    is_loaded: true,
                    point_count: cloud.points.len(),
                });
            }
        }

        let loaded_raster_by_path: BTreeMap<PathBuf, &OpenRasterTexture> = self.raster_textures.iter().map(|raster| (raster.path.clone(), raster)).collect();
        // Highlight only rasters a loaded triangulation is actually using;
        // dormant session assignments waiting to be restored don't count.
        let draped_raster_ids: BTreeSet<_> = self.triangulations.iter().filter_map(|triangulation| triangulation.raster_texture).collect();
        let mut raster_textures = Vec::new();
        let mut seen_rasters = BTreeSet::new();
        for path in &self.raster_files {
            if !seen_rasters.insert(path.clone()) {
                continue;
            }
            if let Some(raster) = loaded_raster_by_path.get(path) {
                raster_textures.push(crate::ui::state::UiRasterTextureEntry {
                    name: raster.name.clone(),
                    path: path.clone(),
                    is_loaded: true,
                    is_draped: draped_raster_ids.contains(&raster.id),
                    source_size: raster.source_size,
                    driver_name: raster.driver_name.clone(),
                    projection: raster.projection.clone(),
                });
            } else {
                raster_textures.push(crate::ui::state::UiRasterTextureEntry {
                    name: path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_owned(),
                    path: path.clone(),
                    is_loaded: false,
                    is_draped: false,
                    source_size: [0, 0],
                    driver_name: String::new(),
                    projection: String::new(),
                });
            }
        }
        // A browser raster is normally listed through `raster_files`, which
        // its stored record registers on import. Surface any loaded raster
        // that is missing one anyway — a failed write leaves the image usable
        // for this session, and its drape, undrape, unload and remove controls
        // have to stay reachable.
        #[cfg(target_arch = "wasm32")]
        for raster in &self.raster_textures {
            if !seen_rasters.insert(raster.path.clone()) {
                continue;
            }
            raster_textures.push(crate::ui::state::UiRasterTextureEntry {
                name: raster.name.clone(),
                path: raster.path.clone(),
                is_loaded: true,
                is_draped: draped_raster_ids.contains(&raster.id),
                source_size: raster.source_size,
                driver_name: raster.driver_name.clone(),
                projection: raster.projection.clone(),
            });
        }

        // Explorer display order: natural (alphanumeric) by name, with
        // triangulation folders sorted above loose files. Sorting the view
        // only — underlying model order is untouched.
        let mut projects = projects;
        for project in &mut projects {
            project.layers.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        }
        projects.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name).then_with(|| a.path.cmp(&b.path)));
        triangulations.sort_by(|a, b| crate::natural_sort::grouped_file_cmp(a.group.as_deref(), &a.name, b.group.as_deref(), &b.name));
        block_models.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        drill_holes.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        point_clouds.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        raster_textures.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));

        let active_path = self.workspace.active_project().and_then(|p| p.path.clone());
        let active_triangulation_for_menu = self
            .active_triangulation
            .and_then(|id| self.triangulations.iter().find(|tri| tri.id == id).map(|tri| (tri.id, tri.color)));
        let view = Arc::new(UiProjectView {
            projects,
            triangulations,
            block_models,
            drill_holes,
            point_clouds,
            raster_textures,
            has_active_project: self.workspace.has_active_project(),
            needs_startup_dialog: !self.workspace.has_active_project() && !self.startup_dialog_dismissed,
            active_path,
            active_triangulation_for_menu,
        });
        *self.ui_project_view_cache.borrow_mut() = Some((key, Arc::clone(&view)));
        view
    }

    /// Restore projects from a saved session. Called from `main` after startup.
    /// Non-existent or invalid paths are skipped independently. All restored
    /// PIDBs are parsed and kept open; the saved active path restores only the
    /// editing target.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn load_session_projects(&mut self, session: &io::Session) {
        let had_active = self.workspace.has_active_project();
        for path in &session.project_paths {
            if !path.exists() {
                log::warn!("Session project no longer exists, skipping: {}", path.display());
                continue;
            }
            if self.workspace.project_index_for_path(path).is_some() {
                continue;
            }
            match pidb::load(path).and_then(|pidb| pidb::open_project(Some(path.clone()), pidb)) {
                Ok(project) => {
                    self.workspace.add_inactive(project);
                }
                Err(error) => {
                    log::warn!("Failed to reopen session project {}: {error}", path.display());
                }
            }
        }
        if !had_active
            && let Some(active_path) = &session.active_path
            && let Some(index) = self.workspace.project_index_for_path(active_path)
        {
            self.workspace.set_active_index(index);
        }
        if let Some(project) = self.workspace.active_project() {
            self.history.activate(project.runtime_id);
        }

        // Restore folder-scanned triangulation directories.
        self.triangulation_excluded_paths = session.triangulation_excluded_paths.iter().cloned().collect();
        for path in &session.triangulation_paths {
            if path.is_dir() && !self.triangulation_dirs.contains(path) {
                self.triangulation_dirs.push(path.clone());
            }
        }
        self.triangulation_dirs.sort();
        self.triangulation_dirs.dedup();
        self.refresh_triangulation_dir_entries();

        // Restore individually-opened triangulation files.
        for path in &session.triangulation_file_paths {
            if MeshFormat::from_path(path).is_some() && path.is_file() && !self.triangulation_excluded_paths.contains(path) && !self.triangulation_files.contains(path) {
                self.triangulation_files.push(path.clone());
            }
        }

        for source in &session.block_model_sources {
            if source.csv_columns.is_some() && source.path.is_file() && !self.block_model_files.contains(source) {
                self.block_model_files.push(source.clone());
            }
        }
        for source in &session.drill_hole_sources {
            let exists = match source {
                DrillHoleSource::LegacyDhd { .. } => false,
                DrillHoleSource::Csv { files, .. } => files.iter().all(|mapping| mapping.path.is_file()),
                DrillHoleSource::Omf { .. } => false,
            };
            if exists && !self.drill_hole_files.contains(source) {
                self.drill_hole_files.push(source.clone());
            }
        }

        for path in &session.point_cloud_file_paths {
            if path.is_file() && !self.point_cloud_files.contains(path) {
                self.point_cloud_files.push(path.clone());
            }
        }
        for path in &session.raster_file_paths {
            if path.is_file() && !self.raster_files.contains(path) {
                self.raster_files.push(path.clone());
            }
        }
    }

    fn scan_triangulation_dir(dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<_> = match fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| MeshFormat::from_path(path).is_some())
                .collect(),
            Err(error) => {
                userspace_warn!("Could not scan triangulation folder {}: {error}", dir.display());
                Vec::new()
            }
        };
        paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        paths
    }

    fn refresh_triangulation_dir_entries(&mut self) {
        self.triangulation_dir_entries = self
            .triangulation_dirs
            .iter()
            .map(|dir| {
                let files = Self::scan_triangulation_dir(dir)
                    .into_iter()
                    .filter(|path| !self.triangulation_excluded_paths.contains(path))
                    .collect();
                (dir.clone(), files)
            })
            .collect();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn persist_session(&self) {
        let mut paths: Vec<PathBuf> = self.workspace.projects.iter().filter_map(|project| project.path.clone()).collect();
        paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        let active_path = self.workspace.active_project().and_then(|p| p.path.clone());
        let triangulation_paths: Vec<PathBuf> = {
            let deduped: BTreeSet<_> = self.triangulation_dirs.iter().cloned().collect();
            deduped.into_iter().collect()
        };
        let triangulation_file_paths: Vec<PathBuf> = {
            let deduped: BTreeSet<_> = self.triangulation_files.iter().cloned().collect();
            deduped.into_iter().collect()
        };
        let triangulation_excluded_paths: Vec<PathBuf> = self.triangulation_excluded_paths.iter().cloned().collect();
        let block_model_sources: Vec<BlockModelSource> = {
            let deduped: BTreeSet<_> = self.block_model_files.iter().cloned().collect();
            deduped.into_iter().collect()
        };
        let drill_hole_sources: Vec<DrillHoleSource> = {
            let deduped: BTreeSet<_> = self.drill_hole_files.iter().cloned().collect();
            deduped.into_iter().collect()
        };
        let point_cloud_file_paths: Vec<PathBuf> = {
            let deduped: BTreeSet<_> = self.point_cloud_files.iter().cloned().collect();
            deduped.into_iter().collect()
        };
        let raster_file_paths: Vec<PathBuf> = {
            let deduped: BTreeSet<_> = self.raster_files.iter().cloned().collect();
            deduped.into_iter().collect()
        };
        let session = io::Session {
            project_paths: paths,
            active_path,
            triangulation_paths,
            triangulation_file_paths,
            triangulation_excluded_paths,
            block_model_sources,
            drill_hole_sources,
            point_cloud_file_paths,
            raster_file_paths,
        };
        if let Err(e) = io::save_session(&session) {
            log::warn!("Failed to save session: {e}");
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn persist_session(&self) {
        let ids: Vec<_> = self
            .workspace
            .projects
            .iter()
            .filter_map(|project| match project.persistence {
                crate::model::pidb::ProjectPersistence::BrowserRecord(id) => Some(id),
                _ => None,
            })
            .collect();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = crate::app::web_storage::save_session(&ids).await {
                userspace_warn!("Failed to save browser session: {error}");
            }
        });
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn allocate_browser_source_path(&mut self, name: &str) -> PathBuf {
        let visible_name = Path::new(name)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("browser-file");
        let original = PathBuf::from(visible_name);
        if self.browser_source_paths.insert(original.clone()) {
            return original;
        }

        let path = Path::new(visible_name);
        let stem = path.file_stem().and_then(|stem| stem.to_str()).filter(|stem| !stem.is_empty()).unwrap_or("browser-file");
        let extension = path.extension().and_then(|extension| extension.to_str());
        for copy in 2_u32.. {
            let candidate = match extension {
                Some(extension) if !extension.is_empty() => PathBuf::from(format!("{stem} ({copy}).{extension}")),
                _ => PathBuf::from(format!("{stem} ({copy})")),
            };
            if self.browser_source_paths.insert(candidate.clone()) {
                return candidate;
            }
        }
        unreachable!("the browser source suffix counter is unbounded")
    }
}

impl<'a> ApplicationHandler<AppEvent> for App<'a> {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window_attributes = Window::default_attributes()
            .with_title(crate::APP_NAME.to_string())
            .with_window_icon(window_icon())
            .with_min_inner_size(winit::dpi::PhysicalSize::new(900, 500));

        #[cfg(not(target_arch = "wasm32"))]
        let window_attributes = window_attributes.with_inner_size(winit::dpi::PhysicalSize::new(900, 500)).with_maximized(true);

        #[cfg(target_arch = "wasm32")]
        let window_attributes = window_attributes.with_append(true);

        #[cfg(target_os = "linux")]
        let window_attributes = window_attributes.with_name(crate::APP_ID, crate::APP_ID);

        let window = match event_loop.create_window(window_attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                log::error!("Failed to create window: {e}");
                #[cfg(target_arch = "wasm32")]
                crate::show_web_startup_error(&format!("failed to create the browser window: {e}"));
                self.close_requested = true;
                return;
            }
        };
        self.window = Some(window.clone());

        #[cfg(target_os = "macos")]
        crate::mac::install_menu_bar();

        #[cfg(not(target_arch = "wasm32"))]
        match pollster::block_on(Graphics::new(window.clone())) {
            Ok(graphics) => {
                self.graphics = Some(graphics);
                self.redraw_requested = true;
                self.fit_view_to_extents();
                self.start_release_check();
            }
            Err(e) => {
                log::error!("Failed to initialize graphics: {e:?}");
                self.close_requested = true;
            }
        }

        #[cfg(target_arch = "wasm32")]
        self.web_graphics_initialization(window);
    }

    fn window_event(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, _window_id: winit::window::WindowId, event: WindowEvent) {
        self.handle_window_event(event_loop, _window_id, event);
    }

    fn device_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, _device_id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event
            && let Some(graphics) = self.graphics.as_mut()
            && graphics.process_mouse_motion(delta.0, delta.1)
        {
            self.redraw_requested = true;
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        #[cfg(target_arch = "wasm32")]
        crate::app::web_storage::set_dirty(
            // IndexedDB is useful session recovery, but it is not a durable
            // user-owned PIDB file. Keep the browser's leave-page warning
            // armed until every open PIDB has been explicitly closed.
            !self.workspace.projects.is_empty()
                || self.triangulations.iter().any(|tri| !tri.is_saved)
                || self.block_models.iter().any(|model| model.source.generated)
                || self.editor.text_editing_enabled,
        );
        #[cfg(target_os = "macos")]
        for action in crate::mac::drain_actions() {
            self.handle_mac_menu_action(action);
        }

        // Background writers must be observed before honoring an exit request;
        // otherwise a completed-but-unpolled export can be terminated here.
        self.poll_saves();
        if self.fatal_shutdown {
            // The renderer is unusable, recovery copies have already been
            // written; exit as soon as atomic background writers settle so an
            // active export is not terminated mid-write.
            if self.pending_saves.is_empty() {
                self.teardown_window();
                event_loop.exit();
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16)));
            }
            return;
        }
        if self.close_requested {
            self.teardown_window();
            event_loop.exit();
            return;
        }

        self.poll_file_dialogs();
        let now = Instant::now();
        if self.next_ui_repaint_deadline.is_some_and(|deadline| deadline <= now) {
            self.next_ui_repaint_deadline = None;
            self.redraw_requested = true;
        }
        let continuous_redraw = self.graphics.as_ref().is_some_and(Graphics::needs_continuous_redraw);

        if (self.redraw_requested || continuous_redraw)
            && let Some(window) = self.window.as_ref()
        {
            let frame_interval = if self.pending_resize.is_some() {
                rate_interval(self.editor.resize_frame_rate_cap)
            } else {
                rate_interval(self.editor.frame_rate_cap)
            };
            if let Some(last_render) = self.last_render_time {
                let deadline = last_render + frame_interval;
                if now < deadline {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                    return;
                }
            }
            self.redraw_requested = false;
            window.request_redraw();
        }

        let task_poll_deadline =
            (!self.pending_file_dialogs.is_empty() || !self.pending_saves.is_empty() || !self.pending_jobs.is_empty()).then(|| now + Duration::from_millis(16));
        let wake_deadline = match (task_poll_deadline, self.next_ui_repaint_deadline) {
            (Some(task), Some(ui)) => Some(task.min(ui)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        if let Some(deadline) = wake_deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn exiting(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        self.teardown_window();
    }

    #[cfg_attr(not(target_arch = "wasm32"), expect(unused_variables))]
    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, event: AppEvent) {
        #[cfg(target_arch = "wasm32")]
        match event {
            AppEvent::GraphicsInitializationFinished => {
                let result = self.web_graphics_result.borrow_mut().take();

                match result {
                    Some(Ok(graphics)) => {
                        self.graphics = Some(graphics);
                        self.web_graphics_state = GraphicsState::Ready;
                        self.redraw_requested = true;
                        crate::show_web_startup_ready();

                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    Some(Err(error)) => {
                        let err = format!("failed to initialize WebGPU: {error:#}");
                        userspace_error!("{err}");
                        crate::show_web_startup_error(&err);
                        self.web_graphics_state = GraphicsState::Failed;
                        self.close_requested = true;
                    }
                    None => {
                        let err = "graphics completion event had no result".to_string();
                        userspace_error!("{err}");
                        crate::show_web_startup_error(&err);
                        self.web_graphics_state = GraphicsState::Failed;
                        self.close_requested = true;
                    }
                }
            }
            AppEvent::BrowserProjectsRestored(result) => match result {
                Ok(records) => {
                    let mut restored = 0usize;
                    for record in records {
                        match pidb::open_browser_project(record.id, record.pidb) {
                            Ok(project) => {
                                self.workspace.add_inactive(project);
                                restored += 1;
                            }
                            Err(error) => userspace_warn!("Could not restore browser project '{}': {error:#}", record.name),
                        }
                    }
                    // Session restoration deliberately has no active editing
                    // target and no loaded layers or transient selections.
                    self.workspace.active_index = None;
                    self.clear_editor_transient_state();
                    self.history = crate::model::History::new();
                    if restored > 0 {
                        self.invalidate_geometry();
                        userspace_log!("Restored {restored} saved browser project(s)");
                    }
                }
                Err(error) => userspace_warn!("Could not restore browser projects: {error}"),
            },
            AppEvent::BrowserProjectSaved {
                runtime_id,
                project_id,
                snapshot_hash,
                snapshot_layer_hashes,
                result,
            } => {
                self.browser_saves_pending.remove(&runtime_id);
                match result {
                    Ok(()) => {
                        if let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) {
                            let project = &mut self.workspace.projects[index];
                            project.id = project_id;
                            project.persistence = crate::model::pidb::ProjectPersistence::BrowserRecord(project_id);
                            project.path = None;
                            project.mark_snapshot_saved(snapshot_hash, snapshot_layer_hashes);
                            userspace_log!("Saved '{}' to browser storage", project.pidb.metadata.name);
                            self.persist_session();
                        }
                    }
                    Err(error) => userspace_warn!("Browser save failed: {error}"),
                }

                if self.browser_delete_after_save.remove(&runtime_id)
                    && let Err(error) = self.delete_browser_project(runtime_id)
                {
                    userspace_warn!("Could not delete browser project: {error:#}");
                }
            }
            AppEvent::BrowserProjectDeleted { runtime_id, result } => {
                self.browser_deletes_pending.remove(&runtime_id);
                match result {
                    Ok(()) => {
                        if self.workspace.project_index_for_runtime_id(runtime_id).is_some() {
                            self.close_pidb(runtime_id);
                        }
                        userspace_log!("Deleted browser project");
                    }
                    Err(error) => {
                        userspace_warn!("Browser project deletion failed: {error}");
                    }
                }
            }
            AppEvent::BrowserClipboardPasted(text) => {
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.queue_browser_paste(text);
                    self.redraw_requested = true;
                }
            }
            AppEvent::BrowserTriangulationsRestored(result) => match result {
                Ok(summaries) => self.catalogue_browser_triangulations(summaries),
                Err(error) => userspace_warn!("Could not list browser triangulations: {error}"),
            },
            AppEvent::BrowserTriangulationFetched { record_id, result } => self.apply_fetched_browser_triangulation(record_id, result),
            AppEvent::BrowserTriangulationStored { id, record_id, name, result } => self.apply_stored_browser_triangulation(id, record_id, name, result),
            AppEvent::BrowserDataRestored { kind, result } => match result {
                Ok(summaries) => self.catalogue_browser_data(kind, summaries),
                Err(error) => userspace_warn!("Could not list stored browser data: {error}"),
            },
            AppEvent::BrowserDataFetched { kind, record_id, result } => self.apply_fetched_browser_data(kind, record_id, result),
            AppEvent::BrowserDataStored { kind, record_id, name, result } => self.apply_stored_browser_data(kind, record_id, name, result),
        }
    }
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name().and_then(|name| name.to_str()).unwrap_or("project.pidb").to_string()
}

#[cfg(target_arch = "wasm32")]
enum GraphicsState {
    NotStarted,
    Initializing,
    Ready,
    Failed,
}

#[cfg(target_arch = "wasm32")]
pub(crate) enum AppEvent {
    GraphicsInitializationFinished,
    BrowserProjectsRestored(std::result::Result<Vec<crate::app::web_storage::BrowserProjectRecord>, String>),
    BrowserProjectSaved {
        runtime_id: u32,
        project_id: crate::model::pidb::ProjectId,
        snapshot_hash: u64,
        snapshot_layer_hashes: std::collections::HashMap<u64, u64>,
        result: std::result::Result<(), String>,
    },
    BrowserProjectDeleted {
        runtime_id: u32,
        result: std::result::Result<(), String>,
    },
    BrowserClipboardPasted(String),
    BrowserTriangulationsRestored(std::result::Result<Vec<crate::app::web_storage::BrowserAssetSummary>, String>),
    BrowserTriangulationFetched {
        record_id: String,
        result: std::result::Result<Option<crate::app::web_storage::BrowserAssetRecord>, String>,
    },
    BrowserTriangulationStored {
        id: crate::model::triangulation::TriangulationId,
        record_id: String,
        name: String,
        result: std::result::Result<(), String>,
    },
    BrowserDataRestored {
        kind: crate::app::commands::browser_data::BrowserDataKind,
        result: std::result::Result<Vec<crate::app::web_storage::BrowserAssetSummary>, String>,
    },
    BrowserDataFetched {
        kind: crate::app::commands::browser_data::BrowserDataKind,
        record_id: String,
        result: std::result::Result<Option<crate::app::web_storage::BrowserAssetRecord>, String>,
    },
    BrowserDataStored {
        kind: crate::app::commands::browser_data::BrowserDataKind,
        record_id: String,
        name: String,
        result: std::result::Result<(), String>,
    },
}

#[cfg(not(target_arch = "wasm32"))]
type AppEvent = ();
