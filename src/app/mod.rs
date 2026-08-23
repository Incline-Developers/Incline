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
    collections::{BTreeSet, HashSet},
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
    app::commands::{file::PendingFileDialog, omf::ViewOnOpen},
    model::{
        Document, LayerId, Object, ObjectId, SceneEntityId,
        block_model::{BlockModelId, BlockModelSource, OpenBlockModel},
        drill_hole::{DrillHoleSource, OpenDrillHoleDataset},
        project::{OpenProject, ProjectStore, SaveToken},
        raster::OpenRasterTexture,
        spatial::ObjectSnapIndex,
        triangulation::{OpenTriangulation, TriangulationId},
    },
    rendering::graphics::Graphics,
    ui::state::{EditorState, UiBlockModelEntry, UiDrillHoleEntry, UiLayerEntry, UiPointCloudEntry, UiProjectEntry, UiProjectView, UiTrackedProjectEntry, UiTriangulationEntry},
    userspace_warn,
};
#[cfg(target_arch = "wasm32")]
use crate::{userspace_error, userspace_log};

pub(crate) const PICK_THRESHOLD_PX: f32 = 8.0;

/// Cursor radius, in logical pixels, for grabbing an individual vertex with the
/// Move tool. Deliberately tight - just outside the drawn vertex marker - so a
/// vertex only grabs when the cursor is genuinely on it.
pub(crate) const MOVE_VERTEX_PICK_PX: f32 = 6.0;

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
    /// the dirty project and waits for its background writer to settle.
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
    browser_deletes_pending: HashSet<crate::model::project::ProjectId>,
    #[cfg(target_arch = "wasm32")]
    browser_delete_after_save: HashSet<u32>,
    #[cfg(target_arch = "wasm32")]
    browser_project_loads_pending: HashSet<crate::model::project::ProjectId>,
    #[cfg(target_arch = "wasm32")]
    tracked_browser_projects: Vec<crate::app::web_storage::BrowserProjectSummary>,
    #[cfg(not(target_arch = "wasm32"))]
    tracked_project_paths: Vec<PathBuf>,
    /// Latest non-zero window size awaiting surface reconfiguration. Resize
    /// events arrive in bursts while dragging, so intermediate sizes are
    /// deliberately replaced instead of configuring a swapchain for each one.
    pending_resize: Option<winit::dpi::PhysicalSize<u32>>,
    last_render_time: Option<Instant>,
    last_scroll_instant: Option<Instant>,
    last_snap_poll_instant: Option<Instant>,
    editor: EditorState,
    workspace: ProjectStore,
    /// Explorer/menu snapshot reused while its allocation-free source
    /// fingerprint is unchanged.
    ui_project_view_cache: RefCell<Option<(u64, Arc<UiProjectView>)>>,
    startup_dialog_dismissed: bool,
    triangulations: Vec<OpenTriangulation>,
    active_triangulation: Option<TriangulationId>,
    next_triangulation_id: u64,
    block_models: Vec<OpenBlockModel>,
    active_block_model: Option<BlockModelId>,
    next_block_model_id: u64,
    drill_holes: Vec<OpenDrillHoleDataset>,
    next_drill_hole_id: u64,
    point_clouds: Vec<crate::model::point_cloud::OpenPointCloud>,
    next_point_cloud_id: u64,
    raster_textures: Vec<OpenRasterTexture>,
    next_raster_texture_id: u64,
    empty_document: Document,
    scene_document: Document,
    snap_index: ObjectSnapIndex,
    /// Set by `invalidate_geometry`; the index rebuilds lazily on the next
    /// snap/orbit query via `refresh_snap_index`.
    snap_index_dirty: bool,
    /// `ProjectStore::composite_key()` of the last `scene_document` build;
    /// `None` forces the next invalidation to rebuild.
    scene_document_key: Option<u64>,
    /// Composite + locked-layer fingerprint of the last expansion of
    /// `EditorState::locked_layers` onto `EditorState::frozen_handles`, so
    /// invalidation only walks the document when one of the two changed.
    layer_lock_key: Option<(u64, u64)>,
    /// Selection + composite fingerprint behind `EditorState::selection_has_intersections`,
    /// so the intersection scan only reruns when the selection or the documents change.
    intersection_availability_key: Option<u64>,
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
    /// project paths currently being parsed. They remain reserved until the job
    /// applies so a Save As/New action cannot change the bytes underneath it.
    #[cfg(not(target_arch = "wasm32"))]
    pending_project_open_paths: HashSet<PathBuf>,
    pub(crate) pending_file_dialogs: Vec<PendingFileDialog>,
    /// New/Open action held while the active dirty project waits for an
    /// explicit Save/Discard/Cancel replacement decision.
    pending_project_replacement: Option<commands::file::FileDialogAction>,
    project_replacement_after_save: bool,
    project_replacement_bypass: bool,
    pending_lossy_save_as: Option<commands::file::FileDialogAction>,
    project_asset_baseline: SaveToken,
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
            browser_project_loads_pending: HashSet::new(),
            #[cfg(target_arch = "wasm32")]
            tracked_browser_projects: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            tracked_project_paths: Vec::new(),
            pending_resize: None,
            last_render_time: None,
            last_scroll_instant: None,
            last_snap_poll_instant: None,
            editor: EditorState::new(),
            workspace: ProjectStore::default(),
            ui_project_view_cache: RefCell::new(None),
            startup_dialog_dismissed: false,
            triangulations: Vec::new(),
            active_triangulation: None,
            next_triangulation_id: 0,
            block_models: Vec::new(),
            active_block_model: None,
            next_block_model_id: 0,
            drill_holes: Vec::new(),
            next_drill_hole_id: 0,
            point_clouds: Vec::new(),
            next_point_cloud_id: 0,
            raster_textures: Vec::new(),
            next_raster_texture_id: 0,
            empty_document: Document::new(),
            scene_document: Document::new(),
            snap_index: ObjectSnapIndex::default(),
            snap_index_dirty: false,
            scene_document_key: None,
            layer_lock_key: None,
            intersection_availability_key: None,
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
            pending_project_open_paths: HashSet::new(),
            pending_file_dialogs: Vec::new(),
            pending_project_replacement: None,
            project_replacement_after_save: false,
            project_replacement_bypass: false,
            pending_lossy_save_as: None,
            project_asset_baseline: SaveToken::default(),
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

        let active_project_id = self.workspace.active_project().map(|project| project.runtime_id);
        let command = match action {
            MacMenuAction::SaveProject => Some(UiCommand::SaveProject),
            MacMenuAction::SaveProjectAs => active_project_id.map(UiCommand::SaveProjectAs),
            MacMenuAction::CloseProject => active_project_id.map(UiCommand::CloseProject),
            MacMenuAction::NewProject => Some(UiCommand::NewProject),
            MacMenuAction::OpenProject => Some(UiCommand::OpenProject),
            MacMenuAction::OpenImport => {
                self.editor.show_import = true;
                self.editor.show_export = false;
                None
            }
            MacMenuAction::OpenExport => {
                self.editor.show_import = false;
                self.editor.show_export = true;
                None
            }
            MacMenuAction::ExportViewportImage => Some(UiCommand::ExportViewportImage),
            MacMenuAction::OpenPlotDialog => Some(UiCommand::OpenPlotDialog),
            MacMenuAction::OpenAbout => {
                self.editor.show_about = true;
                None
            }
            MacMenuAction::RequestExit => Some(UiCommand::RequestExit),
            MacMenuAction::ToggleXyGrid => Some(UiCommand::SetShowXyGrid(!self.editor.show_xy_grid)),
            MacMenuAction::ToggleScaleBar => Some(UiCommand::SetShowScaleBar(!self.editor.show_scale_bar)),
            MacMenuAction::ToggleDarkMode => Some(UiCommand::SetDarkMode(!self.editor.dark_mode)),
            MacMenuAction::ToggleConsole => Some(UiCommand::SetShowConsole(!self.editor.show_console)),
            MacMenuAction::InsertPointsAtIntersections => Some(UiCommand::InsertPointsAtIntersections),
            MacMenuAction::OpenInsertPointAtElevation => Some(UiCommand::OpenInsertPointAtElevationDialog),
            MacMenuAction::OpenMoveToX => Some(UiCommand::OpenMoveToAxisDialog(crate::model::Axis::X)),
            MacMenuAction::OpenMoveToY => Some(UiCommand::OpenMoveToAxisDialog(crate::model::Axis::Y)),
            MacMenuAction::OpenMoveToZ => Some(UiCommand::OpenMoveToAxisDialog(crate::model::Axis::Z)),
            MacMenuAction::OpenCreateTriangulation => Some(UiCommand::OpenCreateTriangulation),
            MacMenuAction::OpenCutTriangulationByPolyline => Some(UiCommand::OpenCutTriangulationByPolyline),
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
        {
            let proxy = event_loop_proxy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = crate::app::web_storage::load_session_projects().await;
                let _ = proxy.send_event(AppEvent::BrowserProjectsRestored(result));
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
                .and_then(|project| (project.loaded_layers.contains(&layer) && project.project.document.layer(layer).is_some()).then_some(layer))
        })
    }

    fn editing_ready(&self) -> bool {
        self.workspace.has_active_project() && !self.editor.fly_mode_enabled
    }

    fn set_active_project(&mut self, project: OpenProject) {
        self.clear_project_owned_data();
        let index = self.workspace.add_and_activate(project);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = self.workspace.projects[index].path.clone() {
            self.track_project_path(path);
        }
        self.clear_editor_transient_state();
        self.history.activate(self.workspace.projects[index].runtime_id);
        self.invalidate_geometry();
        self.persist_session();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn track_project_path(&mut self, path: PathBuf) {
        if !self.tracked_project_paths.iter().any(|tracked| tracked == &path) {
            self.tracked_project_paths.push(path);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn track_browser_project(&mut self, id: crate::model::project::ProjectId, name: String) {
        if let Some(project) = self.tracked_browser_projects.iter_mut().find(|project| project.id == id) {
            project.name = name;
        } else {
            self.tracked_browser_projects.push(crate::app::web_storage::BrowserProjectSummary { id, name });
        }
    }

    pub(super) fn touch_active_project_content(&mut self) {
        if let Some(project) = self.workspace.active_project_mut() {
            project.touch_content();
        }
    }

    /// Dirty state for the complete OMF aggregate, including item revisions
    /// and collection membership as well as the Designs document. Keeping
    /// this check at the aggregate boundary ensures the title, close prompt,
    /// and Save command cannot overlook a dirty item row.
    pub(crate) fn project_content_is_dirty(&self, runtime_id: u32) -> bool {
        let Some(project) = self.workspace.active_project().filter(|project| project.runtime_id == runtime_id) else {
            return false;
        };
        let membership_changed = |current: &[u64], saved: &[(u64, u64)]| current.len() != saved.len() || current.iter().any(|id| !saved.iter().any(|(saved_id, _)| saved_id == id));
        project.has_unsaved_changes()
            || self.triangulations.iter().any(|item| item.state.is_dirty())
            || self.block_models.iter().any(|item| item.state.is_dirty())
            || self.drill_holes.iter().any(|item| item.state.is_dirty())
            || self.point_clouds.iter().any(|item| item.state.is_dirty())
            || self.raster_textures.iter().any(|item| item.state.is_dirty())
            || membership_changed(
                &self.triangulations.iter().map(|item| item.id.0).collect::<Vec<_>>(),
                &self.project_asset_baseline.triangulations,
            )
            || membership_changed(
                &self.block_models.iter().map(|item| item.id.0).collect::<Vec<_>>(),
                &self.project_asset_baseline.block_models,
            )
            || membership_changed(&self.drill_holes.iter().map(|item| item.id.0).collect::<Vec<_>>(), &self.project_asset_baseline.drill_holes)
            || membership_changed(
                &self.point_clouds.iter().map(|item| item.id.0).collect::<Vec<_>>(),
                &self.project_asset_baseline.point_clouds,
            )
            || membership_changed(&self.raster_textures.iter().map(|item| item.id.0).collect::<Vec<_>>(), &self.project_asset_baseline.rasters)
    }

    pub(super) fn project_asset_save_token(&self) -> SaveToken {
        SaveToken {
            triangulations: self.triangulations.iter().map(|item| (item.id.0, item.state.revision())).collect(),
            block_models: self.block_models.iter().map(|item| (item.id.0, item.state.revision())).collect(),
            drill_holes: self.drill_holes.iter().map(|item| (item.id.0, item.state.revision())).collect(),
            point_clouds: self.point_clouds.iter().map(|item| (item.id.0, item.state.revision())).collect(),
            rasters: self.raster_textures.iter().map(|item| (item.id.0, item.state.revision())).collect(),
        }
    }

    pub(super) fn mark_project_asset_snapshot_saved(&mut self, token: &SaveToken) {
        for (id, revision) in &token.triangulations {
            if let Some(item) = self.triangulations.iter_mut().find(|item| item.id.0 == *id) {
                item.state.mark_snapshot_saved(*revision);
            }
        }
        for (id, revision) in &token.block_models {
            if let Some(item) = self.block_models.iter_mut().find(|item| item.id.0 == *id) {
                item.state.mark_snapshot_saved(*revision);
            }
        }
        for (id, revision) in &token.drill_holes {
            if let Some(item) = self.drill_holes.iter_mut().find(|item| item.id.0 == *id) {
                item.state.mark_snapshot_saved(*revision);
            }
        }
        for (id, revision) in &token.point_clouds {
            if let Some(item) = self.point_clouds.iter_mut().find(|item| item.id.0 == *id) {
                item.state.mark_snapshot_saved(*revision);
            }
        }
        for (id, revision) in &token.rasters {
            if let Some(item) = self.raster_textures.iter_mut().find(|item| item.id.0 == *id) {
                item.state.mark_snapshot_saved(*revision);
            }
        }
        self.project_asset_baseline = token.clone();
    }

    pub(super) fn mark_all_project_content_saved(&mut self) {
        for item in &mut self.triangulations {
            item.state.mark_saved();
        }
        for item in &mut self.block_models {
            item.state.mark_saved();
        }
        for item in &mut self.drill_holes {
            item.state.mark_saved();
        }
        for item in &mut self.point_clouds {
            item.state.mark_saved();
        }
        for item in &mut self.raster_textures {
            item.state.mark_saved();
        }
        if let Some(project) = self.workspace.active_project_mut() {
            project.mark_saved();
        }
        self.project_asset_baseline = self.project_asset_save_token();
    }

    /// Drop the single project's retained content and every derived runtime
    /// cache before New/Open installs a replacement project. File-dialog
    /// lifecycle code resolves unsaved-work confirmation before calling this.
    fn clear_project_owned_data(&mut self) {
        self.cancel_jobs(|_| true);
        for (ticket, _, _, report) in std::mem::take(&mut self.pending_triangulation_loads) {
            self.cancel_background_task(ticket);
            if let Some(report) = report {
                report.cancel();
            }
        }
        for (ticket, _, _, report) in std::mem::take(&mut self.pending_block_model_loads) {
            self.cancel_background_task(ticket);
            if let Some(report) = report {
                report.cancel();
            }
        }
        for (ticket, _, _, report) in std::mem::take(&mut self.pending_drill_hole_loads) {
            self.cancel_background_task(ticket);
            if let Some(report) = report {
                report.cancel();
            }
        }
        for (ticket, _, _, report) in std::mem::take(&mut self.pending_point_cloud_loads) {
            self.cancel_background_task(ticket);
            if let Some(report) = report {
                report.cancel();
            }
        }
        for (ticket, _, _, report) in std::mem::take(&mut self.pending_raster_loads) {
            self.cancel_background_task(ticket);
            if let Some(report) = report {
                report.cancel();
            }
        }

        self.workspace = ProjectStore::default();
        self.history = crate::model::History::new();
        self.triangulations.clear();
        self.next_triangulation_id = 0;
        self.active_triangulation = None;
        self.block_models.clear();
        self.next_block_model_id = 0;
        self.active_block_model = None;
        self.drill_holes.clear();
        self.next_drill_hole_id = 0;
        self.point_clouds.clear();
        self.next_point_cloud_id = 0;
        self.raster_textures.clear();
        self.next_raster_texture_id = 0;
        self.project_asset_baseline = SaveToken::default();
        self.clear_editor_transient_state();
        self.scene_document_key = None;
        // Item and object ids restart with the replacement project, so the
        // renderer's id-keyed caches would otherwise keep drawing this
        // project's geometry under the next one's items until something
        // that clears them (a view fit) happened to run.
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.clear_item_caches();
        }
        self.redraw_requested = true;
    }

    fn activate_project_index(&mut self, index: usize) {
        if self.workspace.active_index == Some(index) {
            return;
        }
        let active_tool = self.editor.active_tool;
        self.clear_editor_transient_state();
        // Object interaction is allowed to retarget the current tool to a
        // different project. Its project-specific preview state was cleared
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
        // Clear any in-progress move session so it cannot bleed into the new project.
        self.move_session_original = None;
        self.drag = None;
        self.gizmo_drag = None;
        self.editor.gizmo_drag_axis_index = None;
        self.editor.gizmo_drag_plane_index = None;
    }

    fn invalidate_geometry(&mut self) {
        // Project-persistent design visibility is mirrored into the editor's
        // unified scene filter so selection tools that query the retained
        // document directly exclude the same objects as the rendered scene.
        self.editor.hidden_handles.retain(|handle| !matches!(handle, SceneEntityId::Object(_)));
        if let Some(project) = self.workspace.active_project() {
            self.editor.hidden_handles.extend(project.project.document.hidden_object_ids().map(SceneEntityId::Object));
        }
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
        self.expand_layer_locks(composite_key);
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.invalidate_geometry();
        }
        self.redraw_requested = true;
    }

    /// Mirror `EditorState::locked_layers` onto the individual object handles
    /// in `frozen_handles`.
    ///
    /// Picking, snapping and marquee selection all filter on that one set, so
    /// expanding the layer lock here keeps a layer lock from needing its own
    /// check at each of those sites. Objects frozen by name
    /// (`explicitly_frozen`) survive the rebuild; everything else on an
    /// unlocked layer is released.
    ///
    /// Walking the document is only worth doing when the lock set or the
    /// document contents actually changed, which is what `layer_lock_key`
    /// tracks - with no layer locked (the usual case) the whole pass is two
    /// hashes and a `retain` over a small set.
    fn expand_layer_locks(&mut self, composite_key: u64) {
        let locked_key = self.editor.locked_layers.iter().fold(self.editor.locked_layers.len() as u64, |acc, layer| {
            let mut hasher = DefaultHasher::new();
            layer.hash(&mut hasher);
            acc ^ hasher.finish()
        });
        if self.layer_lock_key == Some((composite_key, locked_key)) {
            return;
        }
        self.layer_lock_key = Some((composite_key, locked_key));
        self.editor
            .frozen_handles
            .retain(|handle| !matches!(handle, SceneEntityId::Object(_)) || self.editor.explicitly_frozen.contains(handle));
        if self.editor.locked_layers.is_empty() {
            return;
        }
        let Some(project) = self.workspace.active_project() else {
            return;
        };
        for object in project.project.document.objects() {
            if self.editor.locked_layers.contains(&object.layer()) {
                let handle = SceneEntityId::Object(object.id());
                self.editor.frozen_handles.insert(handle);
                self.editor.selected_handles.remove(&handle);
            }
        }
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
            .any(|project| project.project.document.objects().iter().any(|object| project.loaded_layers.contains(&object.layer())))
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
            .with_title(format!("{} | Slice preview · Wheel zoom · Middle-drag pan · F fit", crate::APP_NAME))
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
        #[cfg(not(target_arch = "wasm32"))]
        self.tracked_project_paths.hash(&mut hasher);
        #[cfg(target_arch = "wasm32")]
        for project in &self.tracked_browser_projects {
            project.id.hash(&mut hasher);
            project.name.hash(&mut hasher);
        }
        for project in &self.workspace.projects {
            project.runtime_id.hash(&mut hasher);
            project.path.hash(&mut hasher);
            #[cfg(target_arch = "wasm32")]
            matches!(project.persistence, crate::model::project::ProjectPersistence::BrowserRecord(_)).hash(&mut hasher);
            project.project.metadata.name.hash(&mut hasher);
            project.lossy_save_warnings.hash(&mut hasher);
            project.has_unsaved_changes().hash(&mut hasher);
            // Edits and successful async save completions can each change the
            // per-layer dirty set independently.
            project.project.document.revision().hash(&mut hasher);
            project.savepoint_revision().hash(&mut hasher);
            let mut loaded_layers: Vec<_> = project.loaded_layers.iter().copied().collect();
            loaded_layers.sort_unstable_by_key(|layer| layer.0);
            loaded_layers.hash(&mut hasher);
            for layer in project.project.document.layers() {
                layer.id.hash(&mut hasher);
                layer.name.hash(&mut hasher);
                layer.visible.hash(&mut hasher);
            }
        }

        self.active_triangulation.hash(&mut hasher);
        for triangulation in &self.triangulations {
            triangulation.id.hash(&mut hasher);
            triangulation.name.hash(&mut hasher);
            (triangulation.visible && !self.editor.hidden_handles.contains(&triangulation.entity_id())).hash(&mut hasher);
            triangulation.raster_texture.hash(&mut hasher);
            triangulation.color.map(f32::to_bits).hash(&mut hasher);
            triangulation.state.loaded.hash(&mut hasher);
            triangulation.state.revision().hash(&mut hasher);
        }

        self.active_block_model.hash(&mut hasher);
        for model in &self.block_models {
            model.id.hash(&mut hasher);
            model.name.hash(&mut hasher);
            model.visible.hash(&mut hasher);
            model.renderable_block_indices.len().hash(&mut hasher);
            model.model.color_variables().into_iter().filter(|variable| !variable.special).count().hash(&mut hasher);
            model.state.loaded.hash(&mut hasher);
            model.state.revision().hash(&mut hasher);
        }

        for dataset in &self.drill_holes {
            dataset.id.hash(&mut hasher);
            dataset.name.hash(&mut hasher);
            dataset.visible.hash(&mut hasher);
            dataset.dataset.holes.len().hash(&mut hasher);
            dataset.dataset.fields.len().hash(&mut hasher);
            dataset.state.loaded.hash(&mut hasher);
            dataset.state.revision().hash(&mut hasher);
        }

        for cloud in &self.point_clouds {
            cloud.id.hash(&mut hasher);
            cloud.name.hash(&mut hasher);
            cloud.visible.hash(&mut hasher);
            cloud.points.len().hash(&mut hasher);
            cloud.state.loaded.hash(&mut hasher);
            cloud.state.revision().hash(&mut hasher);
        }

        for raster in &self.raster_textures {
            raster.id.hash(&mut hasher);
            raster.name.hash(&mut hasher);
            raster.visible.hash(&mut hasher);
            raster.source_size.hash(&mut hasher);
            raster.driver_name.hash(&mut hasher);
            raster.projection.hash(&mut hasher);
            raster.state.loaded.hash(&mut hasher);
            raster.state.revision().hash(&mut hasher);
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
        let project_dirty = self.workspace.active_project().is_some_and(|project| self.project_content_is_dirty(project.runtime_id));
        let projects: Vec<UiProjectEntry> = self
            .workspace
            .projects
            .iter()
            .enumerate()
            .map(|(index, project)| {
                let dirty_layers = project.dirty_layer_ids();
                UiProjectEntry {
                    runtime_id: project.runtime_id,
                    name: project.project.metadata.name.clone(),
                    dirty: project_dirty,
                    designs_dirty: project.designs_dirty(),
                    lossy_save_warnings: project.lossy_save_warnings.clone(),
                    is_active: self.workspace.active_index == Some(index),
                    #[cfg(target_arch = "wasm32")]
                    stored_in_browser: matches!(project.persistence, crate::model::project::ProjectPersistence::BrowserRecord(_)),
                    path: project.path.clone(),
                    layers: project
                        .project
                        .document
                        .layers()
                        .iter()
                        .map(|layer| UiLayerEntry {
                            id: layer.id,
                            name: layer.name.clone(),
                            visible: layer.visible,
                            is_loaded: project.loaded_layers.contains(&layer.id),
                            dirty: dirty_layers.contains(&layer.id),
                        })
                        .collect(),
                }
            })
            .collect();
        #[cfg(not(target_arch = "wasm32"))]
        let mut tracked_projects = {
            let active = self.workspace.active_project();
            self.tracked_project_paths
                .iter()
                .map(|path| {
                    let is_active = active.and_then(|project| project.path.as_ref()).is_some_and(|active_path| active_path == path);
                    let name = if is_active {
                        active.map(|project| project.project.metadata.name.clone()).unwrap_or_else(|| file_name(path))
                    } else {
                        path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("project").to_owned()
                    };
                    UiTrackedProjectEntry {
                        name,
                        is_active,
                        dirty: is_active && project_dirty,
                        path: path.clone(),
                    }
                })
                .collect::<Vec<_>>()
        };
        #[cfg(target_arch = "wasm32")]
        let mut tracked_projects = {
            let active = self.workspace.active_project();
            let mut entries = self
                .tracked_browser_projects
                .iter()
                .map(|stored| UiTrackedProjectEntry {
                    name: stored.name.clone(),
                    is_active: active.is_some_and(|project| project.id == stored.id),
                    dirty: active.is_some_and(|project| project.id == stored.id) && project_dirty,
                    id: stored.id,
                    stored_in_browser: true,
                })
                .collect::<Vec<_>>();
            if let Some(active) = active
                && !entries.iter().any(|entry| entry.id == active.id)
            {
                entries.push(UiTrackedProjectEntry {
                    name: active.project.metadata.name.clone(),
                    is_active: true,
                    dirty: project_dirty,
                    id: active.id,
                    stored_in_browser: false,
                });
            }
            entries
        };
        let mut triangulations = self
            .triangulations
            .iter()
            .map(|tri| UiTriangulationEntry {
                id: tri.id,
                name: tri.name.clone(),
                source_name: tri.state.source_name.clone(),
                visible: tri.visible && !self.editor.hidden_handles.contains(&tri.entity_id()),
                is_active: self.active_triangulation == Some(tri.id),
                is_loaded: tri.state.loaded,
                dirty: tri.state.is_dirty(),
            })
            .collect::<Vec<_>>();
        let mut block_models = self
            .block_models
            .iter()
            .map(|model| UiBlockModelEntry {
                id: model.id,
                name: model.name.clone(),
                source_name: model.state.source_name.clone(),
                visible: model.visible,
                is_loaded: model.state.loaded,
                dirty: model.state.is_dirty(),
                _block_count: model.renderable_block_indices.len(),
                variable_count: model.model.color_variables().into_iter().filter(|variable| !variable.special).count(),
            })
            .collect::<Vec<_>>();
        let mut drill_holes = self
            .drill_holes
            .iter()
            .map(|dataset| UiDrillHoleEntry {
                id: dataset.id,
                name: dataset.name.clone(),
                source_name: dataset.state.source_name.clone(),
                visible: dataset.visible,
                is_loaded: dataset.state.loaded,
                dirty: dataset.state.is_dirty(),
                hole_count: dataset.dataset.holes.len(),
                field_count: dataset.dataset.fields.len(),
            })
            .collect::<Vec<_>>();
        let mut point_clouds = self
            .point_clouds
            .iter()
            .map(|cloud| UiPointCloudEntry {
                id: cloud.id,
                name: cloud.name.clone(),
                source_name: cloud.state.source_name.clone(),
                visible: cloud.visible,
                is_loaded: cloud.state.loaded,
                dirty: cloud.state.is_dirty(),
                point_count: cloud.points.len(),
            })
            .collect::<Vec<_>>();
        let draped_raster_ids: BTreeSet<_> = self.triangulations.iter().filter_map(|triangulation| triangulation.raster_texture).collect();
        let mut raster_textures = self
            .raster_textures
            .iter()
            .map(|raster| crate::ui::state::UiRasterTextureEntry {
                id: raster.id,
                name: raster.name.clone(),
                source_name: raster.state.source_name.clone(),
                visible: raster.visible,
                is_loaded: raster.state.loaded,
                dirty: raster.state.is_dirty(),
                is_draped: draped_raster_ids.contains(&raster.id),
                source_size: raster.source_size,
                driver_name: raster.driver_name.clone(),
                projection: raster.projection.clone(),
            })
            .collect::<Vec<_>>();

        // Explorer display order is natural (alphanumeric) by item name.
        // Sorting the view only leaves retained project order untouched.
        let mut projects = projects;
        for project in &mut projects {
            project.layers.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        }
        projects.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name).then_with(|| a.path.cmp(&b.path)));
        tracked_projects.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        triangulations.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        block_models.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        drill_holes.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        point_clouds.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));
        raster_textures.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.name, &b.name));

        let active_path = self.workspace.active_project().and_then(|p| p.path.clone());
        let same_membership = |current: &[u64], saved: &[(u64, u64)]| current.len() == saved.len() && current.iter().all(|id| saved.iter().any(|(saved_id, _)| saved_id == id));
        let triangulations_membership_dirty = !same_membership(
            &self.triangulations.iter().map(|item| item.id.0).collect::<Vec<_>>(),
            &self.project_asset_baseline.triangulations,
        );
        let block_models_membership_dirty = !same_membership(
            &self.block_models.iter().map(|item| item.id.0).collect::<Vec<_>>(),
            &self.project_asset_baseline.block_models,
        );
        let drill_holes_membership_dirty = !same_membership(&self.drill_holes.iter().map(|item| item.id.0).collect::<Vec<_>>(), &self.project_asset_baseline.drill_holes);
        let point_clouds_membership_dirty = !same_membership(
            &self.point_clouds.iter().map(|item| item.id.0).collect::<Vec<_>>(),
            &self.project_asset_baseline.point_clouds,
        );
        let rasters_membership_dirty = !same_membership(&self.raster_textures.iter().map(|item| item.id.0).collect::<Vec<_>>(), &self.project_asset_baseline.rasters);
        let active_triangulation_for_menu = self
            .active_triangulation
            .and_then(|id| self.triangulations.iter().find(|tri| tri.id == id).map(|tri| (tri.id, tri.color)));
        let view = Arc::new(UiProjectView {
            tracked_projects,
            projects,
            triangulations,
            block_models,
            drill_holes,
            point_clouds,
            raster_textures,
            triangulations_membership_dirty,
            block_models_membership_dirty,
            drill_holes_membership_dirty,
            point_clouds_membership_dirty,
            rasters_membership_dirty,
            has_active_project: self.workspace.has_active_project(),
            active_project_epoch: self.workspace.active_project().map_or(0, |project| u64::from(project.runtime_id)),
            needs_startup_dialog: !self.workspace.has_active_project() && !self.startup_dialog_dismissed,
            active_path,
            active_triangulation_for_menu,
        });
        *self.ui_project_view_cache.borrow_mut() = Some((key, Arc::clone(&view)));
        view
    }

    /// Restore the tracked native project catalog and reopen its active entry.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn load_session_projects(&mut self, session: &io::Session) {
        self.tracked_project_paths.clear();
        for path in &session.project_paths {
            self.track_project_path(path.clone());
        }
        if let Some(path) = session.current_project_path.clone() {
            self.track_project_path(path);
        }
        let Some(path) = session.current_project_path.as_ref().filter(|path| path.is_file()).cloned() else {
            return;
        };
        let result = std::fs::read(&path).map_err(anyhow::Error::from).and_then(|bytes| {
            let source_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("project.omf").to_owned();
            let progress = crate::model::progress::Progress::new();
            crate::model::formats::omf::from_bytes(&source_name, bytes, &progress.phase(0.0, 1.0)).map(|bundle| (source_name, bundle))
        });
        match result {
            Ok((source_name, bundle)) => self.apply_opened_omf_bundle(Some(path), source_name, bundle, ViewOnOpen::Fit),
            Err(error) => log::warn!("Failed to reopen session project: {error:#}"),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn persist_session(&self) {
        let session = io::Session {
            project_paths: self.tracked_project_paths.clone(),
            current_project_path: self.workspace.active_project().and_then(|project| project.path.clone()),
        };
        if let Err(e) = io::save_session(&session) {
            log::warn!("Failed to save session: {e}");
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn persist_session(&self) {
        let id = self.workspace.active_project().and_then(|project| match project.persistence {
            crate::model::project::ProjectPersistence::BrowserRecord(id) => Some(id),
            _ => None,
        });
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = crate::app::web_storage::save_session(id).await {
                userspace_warn!("Failed to save browser session: {error}");
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn browser_source_filename(name: &str) -> PathBuf {
    Path::new(name)
        .file_name()
        .filter(|name| !name.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("browser-file"))
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
        crate::app::web_storage::set_dirty(self.has_unsaved_changes_for_exit());
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
                Ok(restored) => {
                    self.tracked_browser_projects = restored.projects;
                    if let Some(record) = restored.current_project {
                        let source_name = format!("{}.omf", record.name.trim_end_matches(".omf"));
                        let progress = crate::model::progress::Progress::new();
                        match crate::model::formats::omf::from_bytes(&source_name, record.omf_bytes, &progress.phase(0.0, 1.0)) {
                            Ok(bundle) => {
                                self.apply_opened_omf_bundle(None, source_name, bundle, ViewOnOpen::Fit);
                                if let Some(project) = self.workspace.active_project_mut() {
                                    project.id = record.id;
                                    project.persistence = crate::model::project::ProjectPersistence::BrowserRecord(record.id);
                                }
                                // `set_active_project` persists before the
                                // restored browser identity is attached.
                                self.persist_session();
                                userspace_log!("Restored browser project '{}'.", record.name);
                            }
                            Err(error) => userspace_warn!("Could not restore browser project '{}': {error:#}", record.name),
                        }
                    }
                }
                Err(error) => userspace_warn!("Could not restore the browser project: {error}"),
            },
            AppEvent::BrowserProjectLoaded { project_id, ticket, result } => {
                self.finish_background_task(ticket, false);
                match result {
                    Ok(Some(record)) => {
                        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| {
                            if cancel.is_cancelled() {
                                anyhow::bail!("Cancelled");
                            }
                            progress.set_fraction(0.1);
                            let source_name = format!("{}.omf", record.name.trim_end_matches(".omf"));
                            let bundle = crate::model::formats::omf::from_bytes(&source_name, record.omf_bytes, &progress.phase(0.1, 1.0))?;
                            Ok((record.id, record.name, source_name, bundle))
                        };
                        let apply = move |app: &mut App, result| {
                            app.browser_project_loads_pending.remove(&project_id);
                            match result {
                                Ok((record_id, record_name, source_name, bundle)) => {
                                    app.apply_opened_omf_bundle(None, source_name, bundle, ViewOnOpen::Fit);
                                    if let Some(project) = app.workspace.active_project_mut() {
                                        project.id = record_id;
                                        project.persistence = crate::model::project::ProjectPersistence::BrowserRecord(record_id);
                                    }
                                    app.persist_session();
                                    userspace_log!("Activated browser project '{}'.", record_name);
                                }
                                Err(error) => userspace_warn!("Could not activate browser project: {error:#}"),
                            }
                        };
                        self.spawn_job_reporting_progress("Switching project…", vec![crate::app::jobs::JobKey::Anonymous], compute, apply);
                    }
                    Ok(None) => {
                        self.browser_project_loads_pending.remove(&project_id);
                        userspace_warn!("That browser project no longer exists");
                    }
                    Err(error) => {
                        self.browser_project_loads_pending.remove(&project_id);
                        userspace_warn!("Could not load the browser project: {error}");
                    }
                }
            }
            AppEvent::BrowserProjectSaved {
                runtime_id,
                project_id,
                snapshot_hash,
                snapshot_layer_hashes,
                asset_token,
                result,
            } => {
                self.browser_saves_pending.remove(&runtime_id);
                match result {
                    Ok(()) => {
                        if let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) {
                            self.mark_project_asset_snapshot_saved(&asset_token);
                            let name = {
                                let project = &mut self.workspace.projects[index];
                                project.id = project_id;
                                project.persistence = crate::model::project::ProjectPersistence::BrowserRecord(project_id);
                                project.path = None;
                                project.lossy_save_warnings.clear();
                                project.lossy_save_confirmed = false;
                                project.mark_snapshot_saved(snapshot_hash, snapshot_layer_hashes);
                                project.project.metadata.name.clone()
                            };
                            self.track_browser_project(project_id, name.clone());
                            userspace_log!("Saved '{}' to browser storage", name);
                            self.persist_session();
                        }
                        if self.project_replacement_after_save
                            && self.workspace.active_project().is_some_and(|project| project.runtime_id == runtime_id)
                            && let Err(error) = self.continue_project_replacement()
                        {
                            userspace_warn!("Could not replace the current project: {error:#}");
                        }
                        if self.editor.pending_close_project == Some(runtime_id) {
                            self.close_project(runtime_id);
                        }
                    }
                    Err(error) => {
                        if self.project_replacement_after_save && self.workspace.active_project().is_some_and(|project| project.runtime_id == runtime_id) {
                            self.project_replacement_after_save = false;
                            self.editor.replace_project_confirm_open = true;
                        }
                        if let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id)
                            && !self.workspace.projects[index].lossy_save_warnings.is_empty()
                        {
                            self.workspace.projects[index].lossy_save_confirmed = false;
                            self.editor.lossy_save_confirm_open = true;
                        }
                        userspace_warn!("Browser save failed: {error}");
                    }
                }

                if self.browser_delete_after_save.remove(&runtime_id)
                    && let Err(error) = self.delete_browser_project(runtime_id)
                {
                    userspace_warn!("Could not delete browser project: {error:#}");
                }
                self.try_finish_deferred_exit();
            }
            AppEvent::BrowserProjectDeleted { project_id, runtime_id, result } => {
                self.browser_deletes_pending.remove(&project_id);
                match result {
                    Ok(()) => {
                        self.tracked_browser_projects.retain(|project| project.id != project_id);
                        if let Some(runtime_id) = runtime_id
                            && self.workspace.project_index_for_runtime_id(runtime_id).is_some()
                        {
                            self.close_project(runtime_id);
                        }
                        self.persist_session();
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
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn file_name(path: &Path) -> String {
    path.file_name().and_then(|name| name.to_str()).unwrap_or("project.omf").to_string()
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
    BrowserProjectsRestored(std::result::Result<crate::app::web_storage::BrowserSessionProjects, String>),
    BrowserProjectLoaded {
        project_id: crate::model::project::ProjectId,
        ticket: BackgroundTaskTicket,
        result: std::result::Result<Option<crate::app::web_storage::BrowserProjectRecord>, String>,
    },
    BrowserProjectSaved {
        runtime_id: u32,
        project_id: crate::model::project::ProjectId,
        snapshot_hash: u64,
        snapshot_layer_hashes: std::collections::HashMap<u64, u64>,
        asset_token: crate::model::project::SaveToken,
        result: std::result::Result<(), String>,
    },
    BrowserProjectDeleted {
        project_id: crate::model::project::ProjectId,
        runtime_id: Option<u32>,
        result: std::result::Result<(), String>,
    },
    BrowserClipboardPasted(String),
}

#[cfg(not(target_arch = "wasm32"))]
type AppEvent = ();
