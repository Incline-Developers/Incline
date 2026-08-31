#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context as TaskContext, Poll, Waker},
};

use anyhow::{Context, Result};
use rfd::AsyncFileDialog;

#[cfg(not(target_arch = "wasm32"))]
trait FileHandleExt {
    fn into_path(self) -> PathBuf;
}

#[cfg(not(target_arch = "wasm32"))]
impl FileHandleExt for rfd::FileHandle {
    fn into_path(self) -> PathBuf {
        self.path().to_owned()
    }
}

#[cfg(not(target_arch = "wasm32"))]
use crate::{
    app::file_name,
    model::{Layer, Object},
};
use crate::{
    app::{App, commands::omf::ViewOnOpen},
    model::{
        LayerId,
        block_model::BlockModelId,
        formats::{self, MeshFormat},
        project::{self, OpenProject},
        triangulation::TriangulationId,
    },
    ui::state::DataMenu,
    userspace_log, userspace_warn,
};

/// Whether Save still has work to do for a project browser storage has never
/// held a copy of.
///
/// A project opened, imported or restored in the browser starts out identical to
/// the bytes it came from, so "has unsaved changes" is false - but on desktop
/// those bytes are a file that stays put, and in the browser they are nowhere
/// at all until a record exists. Without this, saving an opened project writes
/// nothing and the project is gone on the next reload.
///
/// The project the application starts on is the same case on desktop: it is
/// clean, because nothing has been drawn in it yet, and it is also nowhere,
/// because it has no path. Save has to reach it so the destination chooser can
/// appear.
fn project_needs_first_save(project: &OpenProject) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        !matches!(project.persistence, crate::model::project::ProjectPersistence::BrowserRecord(_))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        project.path.is_none()
    }
}

/// Write a recovery copy of the dirty project into `recovery_dir`, returning
/// success or failure details. Used during fatal shutdown, when the normal
/// guarded exit flow (confirmation dialogs, Save As) is no longer available.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct RecoveryReport {
    pub(crate) written: Vec<PathBuf>,
    pub(crate) failures: Vec<String>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn write_recovery_copy(snapshot: formats::omf::ProjectSnapshot, runtime_id: u32, recovery_dir: &Path) -> Result<RecoveryReport> {
    std::fs::create_dir_all(recovery_dir).with_context(|| format!("create {}", recovery_dir.display()))?;
    let mut written = Vec::new();
    let mut failures = Vec::new();
    let stem = snapshot.name.trim_end_matches(".omf").to_owned();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let path = recovery_dir.join(format!("{stem}-{timestamp}-{runtime_id}.recovery.omf"));
    let progress = crate::model::progress::Progress::new();
    match formats::omf::write_path(snapshot, &path, &progress.phase(0.0, 1.0)) {
        Ok(()) => written.push(path),
        Err(error) => failures.push(format!("project '{stem}' -> {}: {error:#}", path.display())),
    }
    Ok(RecoveryReport { written, failures })
}

fn mesh_format_name_and_extension(format: MeshFormat) -> (&'static str, &'static str) {
    match format {
        MeshFormat::Obj => ("Wavefront OBJ", "obj"),
        MeshFormat::Stl => ("STL", "stl"),
        MeshFormat::Ply => ("PLY", "ply"),
    }
}

#[cfg(target_arch = "wasm32")]
fn mesh_format_mime_type(format: MeshFormat) -> &'static str {
    match format {
        MeshFormat::Obj => "text/plain",
        MeshFormat::Stl => "model/stl",
        MeshFormat::Ply => "application/octet-stream",
    }
}

/// Result of a background file-dialog. Each variant carries the path(s) chosen
/// along with whatever IDs or indices are needed to act on them.
#[derive(Debug)]
// On wasm the native variants are compiled out, leaving only the `Web`-prefixed ones.
#[cfg_attr(target_arch = "wasm32", allow(clippy::enum_variant_names))]
pub(crate) enum FileDialogAction {
    /// Start a new, never-saved project. It carries no path until the first
    /// Save asks for one.
    #[cfg(not(target_arch = "wasm32"))]
    NewProject,
    #[cfg(not(target_arch = "wasm32"))]
    OpenProject(Vec<PathBuf>),
    #[cfg(not(target_arch = "wasm32"))]
    SwitchProject(PathBuf),
    #[cfg(target_arch = "wasm32")]
    WebOpenProject(Vec<std::result::Result<crate::model::input::InputFile, String>>),
    #[cfg(target_arch = "wasm32")]
    WebStoredProject(crate::model::project::ProjectId),
    #[cfg(target_arch = "wasm32")]
    WebNewProject(String),
    /// Import DXF files into the active project.
    #[cfg(not(target_arch = "wasm32"))]
    ImportDxfInto { paths: Vec<PathBuf> },
    #[cfg(not(target_arch = "wasm32"))]
    ImportTriangulation(Vec<PathBuf>),
    #[cfg(not(target_arch = "wasm32"))]
    ImportPointCloud(Vec<PathBuf>),
    #[cfg(not(target_arch = "wasm32"))]
    ImportRaster(Vec<PathBuf>),
    #[cfg(not(target_arch = "wasm32"))]
    SetImportSourcePaths { kind: DataMenu, paths: Vec<PathBuf> },
    #[cfg(target_arch = "wasm32")]
    WebSetImportSourceFiles {
        kind: DataMenu,
        files: Vec<std::result::Result<crate::model::input::InputFile, String>>,
    },
    /// Export a layer from the project that owned it when the chooser opened.
    #[cfg(not(target_arch = "wasm32"))]
    ExportLayerDxf { project_runtime_id: u32, layer: LayerId, path: PathBuf },
    /// Export the project selected when the chooser opened to DXF.
    #[cfg(not(target_arch = "wasm32"))]
    ExportProjectDxf { project_runtime_id: u32, path: PathBuf },
    #[cfg(not(target_arch = "wasm32"))]
    ExportOmf { snapshot: Box<formats::omf::ProjectSnapshot>, path: PathBuf },
    #[cfg(not(target_arch = "wasm32"))]
    ExportTriangulation { id: TriangulationId, path: PathBuf },
    #[cfg(not(target_arch = "wasm32"))]
    ExportBlockModelCsv { id: BlockModelId, path: PathBuf },
    /// Save one open project under a new path.
    #[cfg(not(target_arch = "wasm32"))]
    SaveProjectAs { project_runtime_id: u32, path: PathBuf },
    /// Export the main viewport to a PNG image.
    #[cfg(not(target_arch = "wasm32"))]
    ExportViewportImage(PathBuf),
    /// Render and write the configured plot sheet.
    #[cfg(not(target_arch = "wasm32"))]
    ExportPlotSheet(PathBuf),
    #[cfg(target_arch = "wasm32")]
    WebDownloadDxf {
        project_runtime_id: u32,
        layer: Option<LayerId>,
        file_name: String,
    },
    #[cfg(target_arch = "wasm32")]
    WebDownloadTriangulation {
        id: TriangulationId,
        format: MeshFormat,
        file_name: String,
        close_after: bool,
    },
    #[cfg(target_arch = "wasm32")]
    WebDownloadBlockModelCsv { id: BlockModelId, file_name: String, close_after: bool },
    #[cfg(target_arch = "wasm32")]
    WebViewportImage(String),
}

pub(crate) struct PendingFileDialog {
    future: Pin<Box<dyn Future<Output = Option<FileDialogAction>>>>,
    console_report: Option<crate::logging::ConsoleReportHandle>,
}

/// A durable asset save/export running on a background thread. The worker
/// reports progress through the shared counter its ticket registered (shown in
/// the status bar) and delivers the final outcome over `result_rx`, which
/// `poll_saves` drains each frame.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct PendingSave {
    ticket: crate::app::BackgroundTaskTicket,
    console_report: Option<crate::logging::ConsoleReportHandle>,
    kind: PendingSaveKind,
    path: PathBuf,
    result_rx: mpsc::Receiver<Result<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
enum PendingSaveKind {
    /// Export a copy: leave the open triangulation's metadata unchanged.
    Export {
        name: String,
    },
    /// project snapshot written independently of subsequent in-memory edits.
    Project {
        runtime_id: u32,
        snapshot_hash: u64,
        snapshot_layer_hashes: std::collections::HashMap<u64, u64>,
        asset_token: crate::model::project::SaveToken,
        /// Close once this write finishes. Set when a close request arrives
        /// after the worker has already started and can no longer be stopped.
        close_after: bool,
        /// Original metadata name when this write came from Save As.
        save_as_previous_name: Option<String>,
    },
    DxfExport {
        description: String,
    },
}

#[cfg(target_arch = "wasm32")]
pub(crate) enum PendingSave {}

/// Status-bar label for a save. Only the file name is shown: the bar is a
/// fixed width and a full path pushes the part that identifies the file
/// (its name) out of view.
#[cfg(not(target_arch = "wasm32"))]
fn save_label(kind: &PendingSaveKind, path: &Path) -> String {
    let name = file_name(path);
    match kind {
        PendingSaveKind::Export { .. } => format!("Exporting {name}"),
        PendingSaveKind::Project { .. } => format!("Saving {name}"),
        PendingSaveKind::DxfExport { .. } => format!("Exporting {name}"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
const PROJECT_REVERT_JOB_LABEL: &str = "Reverting project…";
#[cfg(not(target_arch = "wasm32"))]
const LAYER_REVERT_JOB_LABEL: &str = "Reverting layer…";

#[cfg(not(target_arch = "wasm32"))]
type LayerSnapshot = (usize, Layer, Vec<(usize, Object)>);

impl<'a> App<'a> {
    /// Register an async file dialog that was created on the main thread. The
    /// app polls completion in `poll_file_dialogs`.
    pub(super) fn spawn_file_dialog<F>(&mut self, future: F)
    where
        F: Future<Output = Option<FileDialogAction>> + 'static,
    {
        self.pending_file_dialogs.push(PendingFileDialog {
            future: Box::pin(future),
            console_report: crate::logging::retain_current_report(),
        });
    }

    /// Drain completed file-dialog futures and execute each resolved action.
    /// Called every frame from `about_to_wait`.
    pub(crate) fn poll_file_dialogs(&mut self) {
        let mut resolved: Vec<(Option<FileDialogAction>, Option<crate::logging::ConsoleReportHandle>)> = Vec::new();
        let waker = Waker::noop();
        let mut cx = TaskContext::from_waker(waker);
        self.pending_file_dialogs.retain_mut(|pending| match pending.future.as_mut().poll(&mut cx) {
            Poll::Ready(action) => {
                resolved.push((action, pending.console_report.take()));
                false
            }
            Poll::Pending => true,
        });
        if resolved.iter().any(|(action, _)| action.is_none()) {
            if self.exit_after_pending_saves {
                self.cancel_exit_request();
            }
            // A cancelled Save As also cancels a close waiting on it.
            self.cancel_close_project();
        }
        for (action, report) in resolved {
            self.redraw_requested = true;
            if let Some(action) = action {
                let execute = || {
                    if let Err(err) = self.execute_file_dialog_action(action) {
                        let msg = format!("{err:#}");
                        userspace_warn!("File dialog action failed: {msg}");
                        if self.exit_after_pending_saves {
                            self.cancel_exit_request();
                        }
                    }
                };
                if let Some(report) = report.as_ref() {
                    report.scope(execute);
                } else {
                    execute();
                }
                drop(report);
            } else if let Some(report) = report {
                report.cancel();
            } else {
                if self.exit_after_pending_saves {
                    self.cancel_exit_request();
                }
            }
        }
        self.try_finish_deferred_exit();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start_open_project_path(&mut self, path: PathBuf, status_label: &'static str) -> Result<()> {
        if self.save_path_is_pending(&path) {
            return Ok(());
        }
        self.pending_project_open_paths.insert(path.clone());
        let reserved_path = path.clone();
        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            progress.set_fraction(0.0);
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            progress.set_fraction(0.1);
            let source_name = file_name(&path);
            let bundle = formats::omf::from_bytes(&source_name, bytes, &progress.phase(0.1, 1.0))?;
            Ok((path, source_name, bundle))
        };
        let apply = move |app: &mut App, result: Result<(PathBuf, String, formats::omf::ImportBundle)>| {
            app.pending_project_open_paths.remove(&reserved_path);
            match result {
                Ok((path, source_name, bundle)) => app.apply_opened_omf_bundle(Some(path), source_name, bundle, ViewOnOpen::Fit),
                Err(error) => userspace_warn!("Could not open {}: {error:#}", reserved_path.display()),
            }
        };
        self.spawn_job_reporting_progress(status_label, vec![crate::app::jobs::JobKey::Anonymous], compute, apply);
        Ok(())
    }

    pub(super) fn execute_file_dialog_action(&mut self, action: FileDialogAction) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        let replaces_project = matches!(
            &action,
            FileDialogAction::NewProject | FileDialogAction::OpenProject(_) | FileDialogAction::SwitchProject(_)
        );
        #[cfg(target_arch = "wasm32")]
        let replaces_project = matches!(
            &action,
            FileDialogAction::WebNewProject(_) | FileDialogAction::WebOpenProject(_) | FileDialogAction::WebStoredProject(_)
        );
        #[cfg(target_arch = "wasm32")]
        if replaces_project && !self.browser_project_loads_pending.is_empty() {
            anyhow::bail!("Wait for the current project switch to finish");
        }
        if replaces_project && !self.project_replacement_bypass && self.has_unsaved_changes_for_exit() {
            self.pending_project_replacement = Some(action);
            self.editor.replace_project_confirm_open = true;
            self.redraw_requested = true;
            return Ok(());
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let FileDialogAction::SaveProjectAs { project_runtime_id, .. } = &action
            && self
                .workspace
                .project_index_for_runtime_id(*project_runtime_id)
                .and_then(|index| self.workspace.projects.get(index))
                .is_some_and(|project| !project.lossy_save_warnings.is_empty() && !project.lossy_save_confirmed)
        {
            self.pending_lossy_save_as = Some(action);
            self.editor.lossy_save_confirm_open = true;
            self.redraw_requested = true;
            return Ok(());
        }
        self.project_replacement_bypass = false;
        match action {
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::NewProject => {
                self.start_untitled_project()?;
                userspace_log!("Created new project");
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::OpenProject(paths) => {
                let Some(path) = paths.into_iter().next() else {
                    return Ok(());
                };
                self.start_open_project_path(path, "Opening project…")
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::SwitchProject(path) => self.start_open_project_path(path, "Switching project…"),
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebOpenProject(files) => {
                let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    let file = files.into_iter().next().context("No OMF file was selected")?.map_err(anyhow::Error::msg)?;
                    let name = file.source.name;
                    let bundle = formats::omf::from_bytes(&name, file.bytes, &progress.phase(0.0, 1.0))?;
                    Ok((name, bundle))
                };
                let apply = |app: &mut App, result: Result<(String, formats::omf::ImportBundle)>| match result {
                    Ok((name, bundle)) => app.apply_opened_omf_bundle(None, name, bundle, ViewOnOpen::Fit),
                    Err(error) => {
                        userspace_warn!("Could not open browser project: {error:#}");
                    }
                };
                self.spawn_job_reporting_progress("Opening browser project…", vec![crate::app::jobs::JobKey::Anonymous], compute, apply);
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebStoredProject(project_id) => {
                if self.workspace.active_project().is_some_and(|project| project.id == project_id) {
                    return Ok(());
                }
                let proxy = self.web_event_loop_proxy.clone().context("browser event loop is unavailable")?;
                if !self.browser_project_loads_pending.insert(project_id) {
                    return Ok(());
                }
                let (ticket, _progress) = self.begin_reported_task("Switching project…");
                wasm_bindgen_futures::spawn_local(async move {
                    let result = crate::app::web_storage::load_project(project_id).await;
                    let _ = proxy.send_event(crate::app::AppEvent::BrowserProjectLoaded { project_id, ticket, result });
                });
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebNewProject(name) => {
                let mut project_file = project::new_empty(None);
                project_file.metadata.name = sanitize_project_name(&name);
                let project = project::open_project(None, project_file)?;
                self.set_active_project(project);
                userspace_log!("Created new browser project");
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ImportDxfInto { paths } => {
                let project = self.workspace.active_project().context("No active .omf to import into")?;
                let runtime_id = project.runtime_id;
                let document_revision = project.project.document.revision();
                let import_count = paths.len();
                let compute = move |cancel: &crate::app::jobs::CancelFlag| {
                    let mut parsed = Vec::with_capacity(paths.len());
                    for path in paths {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        let document = formats::dxf::read_document(&path)?;
                        parsed.push((path, document));
                    }
                    Ok(parsed)
                };
                let apply = move |app: &mut App, result: Result<Vec<(PathBuf, crate::model::Document)>>| {
                    let parsed = match result {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            userspace_warn!("DXF import failed: {error:#}");
                            return;
                        }
                    };
                    let Some(project_index) = app.workspace.project_index_for_runtime_id(runtime_id) else {
                        return;
                    };
                    let project = &mut app.workspace.projects[project_index];
                    let existing: std::collections::HashSet<LayerId> = project.project.document.layers().iter().map(|layer| layer.id).collect();
                    let mut total_added = 0usize;
                    for (path, imported) in parsed {
                        let added = project::merge_document(&mut project.project.document, &imported);
                        userspace_log!("Imported {added} object(s) from {}", path.display());
                        total_added += added;
                    }
                    let new_ids: Vec<LayerId> = project
                        .project
                        .document
                        .layers()
                        .iter()
                        .filter(|layer| !existing.contains(&layer.id))
                        .map(|layer| layer.id)
                        .collect();
                    project.loaded_layers.extend(new_ids.iter().copied());
                    if app.workspace.active_index == Some(project_index)
                        && let Some(&id) = new_ids.first()
                    {
                        app.editor.active_layer = Some(id);
                    }
                    app.invalidate_geometry();
                    app.fit_view_to_extents();
                    userspace_log!("Imported {import_count} DXF(s) into the project: {total_added} object(s)");
                };
                self.spawn_job(
                    "Parsing DXF import…",
                    vec![crate::app::jobs::JobKey::Project { runtime_id, document_revision }],
                    compute,
                    apply,
                );
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ImportTriangulation(paths) => {
                let count = paths.len();
                for path in &paths {
                    self.open_triangulation_path(path)?;
                }
                userspace_log!("Queued {count} triangulation file(s) for import");
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ImportPointCloud(paths) => {
                for path in &paths {
                    self.import_point_cloud_path(path)?;
                }
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ImportRaster(paths) => {
                for path in &paths {
                    self.import_raster_path(path)?;
                }
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::SetImportSourcePaths { kind, paths } => {
                self.editor.import_source_menu = kind;
                self.editor.import_source_paths = paths;
                if kind == DataMenu::CsvBlockModel {
                    let result = self
                        .editor
                        .import_source_paths
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("Choose a CSV block-model file"))
                        .and_then(|path| crate::model::formats::csv_block_model::preview_path(path).map_err(anyhow::Error::new));
                    match result {
                        Ok(preview) => {
                            self.editor.import_csv_preview = Some(preview);
                            self.editor.import_csv_error = None;
                        }
                        Err(error) => {
                            self.editor.import_csv_preview = None;
                            self.editor.import_csv_error = Some(error.to_string());
                        }
                    }
                }
                if kind == DataMenu::CsvDrillHole {
                    self.editor.import_drill_csv.clear();
                    self.editor.import_csv_error = None;
                    for path in &self.editor.import_source_paths {
                        match crate::model::formats::csv_drill_hole::preview_path(path) {
                            Ok(preview) => {
                                let mapping = crate::model::formats::csv_drill_hole::unassigned_mapping(path.clone(), &preview);
                                self.editor.import_drill_csv.push((mapping, preview));
                            }
                            Err(error) => {
                                self.editor.import_csv_error = Some(error.to_string());
                                self.editor.import_drill_csv.clear();
                                break;
                            }
                        }
                    }
                }
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebSetImportSourceFiles { kind, files } => {
                let mut accepted = Vec::new();
                for file in files {
                    match file {
                        Ok(file) => accepted.push(file),
                        Err(error) => userspace_warn!("Could not read selected file: {error}"),
                    }
                }
                self.editor.import_source_menu = kind;
                self.editor.import_source_paths = accepted.iter().map(|file| PathBuf::from(&file.source.name)).collect();
                if kind == DataMenu::CsvBlockModel {
                    match accepted
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("Choose a CSV block-model file"))
                        .and_then(|file| crate::model::formats::csv_block_model::preview(&file.bytes).map_err(anyhow::Error::new))
                    {
                        Ok(preview) => {
                            self.editor.import_csv_preview = Some(preview);
                            self.editor.import_csv_error = None;
                        }
                        Err(error) => {
                            self.editor.import_csv_preview = None;
                            self.editor.import_csv_error = Some(error.to_string());
                        }
                    }
                }
                if kind == DataMenu::CsvDrillHole {
                    self.editor.import_drill_csv.clear();
                    self.editor.import_csv_error = None;
                    for file in &accepted {
                        match crate::model::formats::csv_drill_hole::preview(&file.bytes) {
                            Ok(preview) => {
                                let path = PathBuf::from(&file.source.name);
                                let mapping = crate::model::formats::csv_drill_hole::unassigned_mapping(path, &preview);
                                self.editor.import_drill_csv.push((mapping, preview));
                            }
                            Err(error) => {
                                self.editor.import_csv_error = Some(error.to_string());
                                self.editor.import_drill_csv.clear();
                                break;
                            }
                        }
                    }
                }
                self.web_import_files = Some((kind, accepted));
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportLayerDxf { project_runtime_id, layer, path } => {
                self.commit_export_move_if_needed(project_runtime_id);
                self.ensure_save_path_not_pending(&path)?;
                let project_index = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .context("The project selected for export is no longer open")?;
                let project = &self.workspace.projects[project_index];
                self.spawn_dxf_write(project.project.clone(), Some(layer), path, format!("layer {:?}", layer));
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportProjectDxf { project_runtime_id, path } => {
                self.commit_export_move_if_needed(project_runtime_id);
                self.ensure_save_path_not_pending(&path)?;
                let project_index = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .context("The project selected for export is no longer open")?;
                let project = &self.workspace.projects[project_index];
                self.spawn_dxf_write(project.project.clone(), None, path, "project".to_owned());
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportOmf { snapshot, path } => {
                self.export_omf_snapshot(*snapshot, path);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportTriangulation { id, path } => {
                MeshFormat::from_path(&path).context("Choose a filename ending in .obj, .stl, or .ply")?;
                if self.pending_triangulation_loads.iter().any(|(_, pending, _, _)| pending == &path) {
                    anyhow::bail!("A triangulation import from {} is still in progress", path.display());
                }
                if self.pending_saves.iter().any(|save| save.path == path) {
                    anyhow::bail!("A save to {} is already in progress", path.display());
                }
                let triangulation = self.triangulations.iter().find(|t| t.id == id).context("The selected triangulation is no longer loaded")?;
                let mesh = std::sync::Arc::clone(&triangulation.mesh);
                let name = triangulation.name.clone();
                userspace_log!("Exporting triangulation '{}' to {}", name, path.display());
                self.spawn_triangulation_write(PendingSaveKind::Export { name }, mesh, path);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportBlockModelCsv { id, path } => self.export_block_model_csv_to_path(id, path),
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::SaveProjectAs { project_runtime_id, path } => {
                if self.project_revert_is_pending(project_runtime_id) {
                    anyhow::bail!("Wait for the project revert to finish before saving");
                }
                let project_index = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .context("The selected project is no longer open")?;
                if self.workspace.active_index == Some(project_index) && self.has_pending_move_delta() {
                    self.commit_pending_move();
                }
                self.ensure_project_has_no_pending_text_edit(project_index)?;
                self.ensure_project_save_path_available(project_index, &path)?;
                let new_name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("Incline project").to_owned();
                let (previous_name, snapshot_hash, snapshot_layer_hashes) = {
                    let project = &mut self.workspace.projects[project_index];
                    let previous_name = std::mem::replace(&mut project.project.metadata.name, new_name.clone());
                    let snapshot_hash = project.current_content_hash();
                    let snapshot_layer_hashes = project.current_layer_hashes();
                    project.project.metadata.name = previous_name.clone();
                    (previous_name, snapshot_hash, snapshot_layer_hashes)
                };
                let mut snapshot = self.omf_export_snapshot()?;
                let asset_token = self.project_asset_save_token();
                snapshot.name = new_name;
                let kind = PendingSaveKind::Project {
                    runtime_id: project_runtime_id,
                    snapshot_hash,
                    snapshot_layer_hashes,
                    asset_token,
                    close_after: false,
                    save_as_previous_name: Some(previous_name),
                };
                self.spawn_project_write(kind, snapshot, path);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportViewportImage(mut path) => {
                if path.extension().is_none() {
                    path.set_extension("png");
                }
                let graphics = self.graphics.as_mut().context("The renderer is not initialised yet")?;
                graphics.request_screenshot(path);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportPlotSheet(mut path) => {
                if path.extension().is_none() {
                    path.set_extension("png");
                }
                self.write_plot_sheet(crate::app::commands::plot::PlotTarget::File(path))
            }
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebDownloadDxf {
                project_runtime_id,
                layer,
                file_name,
            } => {
                self.commit_export_move_if_needed(project_runtime_id);
                let project = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .and_then(|index| self.workspace.projects.get(index))
                    .context("The project selected for export is no longer open")?;
                let snapshot = project.project.clone();
                self.spawn_job(
                    "Encoding DXF download…",
                    vec![crate::app::jobs::JobKey::Anonymous],
                    move |cancel| {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        formats::dxf::export_bytes(&snapshot, layer)
                    },
                    move |_app, result| match result {
                        Ok(bytes) => Self::trigger_browser_download(file_name, bytes, "application/dxf", "DXF"),
                        Err(error) => userspace_warn!("DXF download encoding failed: {error:#}"),
                    },
                );
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebDownloadTriangulation {
                id,
                format,
                file_name,
                close_after,
            } => {
                let triangulation = self
                    .triangulations
                    .iter()
                    .find(|triangulation| triangulation.id == id)
                    .context("The selected triangulation is no longer loaded")?;
                let mesh = std::sync::Arc::clone(&triangulation.mesh);
                self.spawn_job(
                    "Encoding triangulation download…",
                    vec![crate::app::jobs::JobKey::Anonymous],
                    move |cancel| {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        formats::write_mesh_bytes(&mesh, format).map_err(|error| anyhow::anyhow!("Could not encode triangulation: {error}"))
                    },
                    move |app, result| match result {
                        Ok(bytes) => {
                            Self::trigger_browser_download(file_name, bytes, mesh_format_mime_type(format), "triangulation");
                            if close_after {
                                app.close_triangulation(id);
                            }
                        }
                        Err(error) => {
                            userspace_warn!("Triangulation download encoding failed: {error:#}")
                        }
                    },
                );
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebDownloadBlockModelCsv { id, file_name, close_after } => {
                let block_model = self
                    .block_models
                    .iter()
                    .find(|model| model.id == id)
                    .context("The selected block model is no longer loaded")?;
                let model = block_model.model.clone();
                let blocks = std::sync::Arc::clone(&block_model.blocks);
                let renderable = std::sync::Arc::clone(&block_model.renderable_block_indices);
                self.spawn_job(
                    "Encoding block-model CSV download…",
                    vec![crate::app::jobs::JobKey::Anonymous],
                    move |cancel| {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        crate::model::formats::csv_block_model::to_bytes(&model, &blocks, &renderable).map_err(anyhow::Error::new)
                    },
                    move |app, result| match result {
                        Ok(bytes) => {
                            Self::trigger_browser_download(file_name, bytes, "text/csv", "block-model CSV");
                            if close_after {
                                app.close_block_model(id);
                            }
                        }
                        Err(error) => userspace_warn!("Block-model CSV encoding failed: {error:#}"),
                    },
                );
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebViewportImage(file_name) => {
                let graphics = self.graphics.as_mut().context("The renderer is not initialised yet")?;
                graphics.request_browser_screenshot(file_name);
                Ok(())
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(super) fn trigger_browser_download(file_name: String, bytes: Vec<u8>, mime_type: &'static str, description: &'static str) {
        match crate::app::web_download::download(&file_name, &bytes, mime_type) {
            Ok(()) => userspace_log!("Downloaded {description}: {file_name}"),
            Err(error) => userspace_warn!("{description} download failed: {error}"),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn start_browser_export(&mut self, action: FileDialogAction) {
        if let Err(error) = self.execute_file_dialog_action(action) {
            userspace_warn!("Could not start browser export: {error:#}");
        }
    }

    /// Spawn a worker thread that writes `mesh` to `path`, streaming progress
    /// back for the status bar. Completion is handled in `poll_saves`.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_triangulation_write(&mut self, kind: PendingSaveKind, mesh: std::sync::Arc<formats::mesh_data::Triangulation>, path: PathBuf) {
        let (ticket, progress) = self.begin_reported_task(save_label(&kind, &path));
        let (result_tx, result_rx) = mpsc::channel();
        self.pending_saves.push(PendingSave {
            ticket,
            console_report: crate::logging::retain_current_report(),
            kind,
            path: path.clone(),
            result_rx,
        });
        let window = self.window.clone();
        crate::app::jobs::spawn_io_task(move || {
            let mut last_redraw: Option<web_time::Instant> = None;
            let result = crate::app::jobs::run_compute_catching_panic(|| {
                formats::write_mesh_with_progress(&mesh, &path, &mut |written, total| {
                    progress.set_items(written, total);
                    // Reporting is a plain atomic store, but waking the event
                    // loop is not: throttle the redraws so a fast write doesn't
                    // flood it.
                    let due = last_redraw.is_none_or(|last| last.elapsed() >= std::time::Duration::from_millis(100));
                    if due {
                        last_redraw = Some(web_time::Instant::now());
                        if let Some(w) = window.as_ref() {
                            w.request_redraw();
                        }
                    }
                })
                .map_err(|err| anyhow::anyhow!("Failed to write {}: {err}", path.display()))
            });
            let _ = result_tx.send(result);
            if let Some(w) = window.as_ref() {
                w.request_redraw();
            }
        });
    }

    /// Drain progress and completion from background saves. Called each frame
    /// alongside the other `poll_*` methods so results land in the same render
    /// they arrive.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn poll_saves(&mut self) {
        let pending = std::mem::take(&mut self.pending_saves);
        let mut still_pending = Vec::new();
        let mut finish_project_actions = false;
        let mut continue_project_replacement = false;

        for mut save in pending {
            let project_save_runtime_id = match &save.kind {
                PendingSaveKind::Project { runtime_id, .. } => Some(*runtime_id),
                _ => None,
            };
            let deferred_project_close = match &save.kind {
                PendingSaveKind::Project {
                    runtime_id, close_after: true, ..
                } => Some(*runtime_id),
                _ => None,
            };
            match save.result_rx.try_recv() {
                Ok(Ok(())) => {
                    let console_report = save.console_report.take();
                    let complete = || {
                        self.finish_background_task(save.ticket, false);
                        self.redraw_requested = true;
                        match save.kind {
                            PendingSaveKind::Export { name } => {
                                userspace_log!("Exported triangulation '{}' to {}", name, save.path.display());
                            }
                            PendingSaveKind::Project {
                                runtime_id,
                                snapshot_hash,
                                snapshot_layer_hashes,
                                asset_token,
                                close_after,
                                save_as_previous_name,
                            } => {
                                if let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) {
                                    self.mark_project_asset_snapshot_saved(&asset_token);
                                    self.workspace.projects[index].lossy_save_warnings.clear();
                                    self.workspace.projects[index].lossy_save_confirmed = false;
                                    if save_as_previous_name.is_some() {
                                        self.workspace.projects[index].path = Some(save.path.clone());
                                        self.workspace.projects[index].project.metadata.name =
                                            save.path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("Incline project").to_owned();
                                        userspace_log!("Saved project as: {}", save.path.display());
                                    } else {
                                        userspace_log!("Saved project: {}", save.path.display());
                                    }
                                    self.workspace.projects[index].mark_snapshot_saved(snapshot_hash, snapshot_layer_hashes);
                                    if save_as_previous_name.is_some() {
                                        self.track_project_path(save.path.clone());
                                    }
                                    self.persist_session();
                                    if self.project_replacement_after_save && self.workspace.active_project().is_some_and(|project| project.runtime_id == runtime_id) {
                                        continue_project_replacement = true;
                                    }
                                    if close_after {
                                        self.editor.pending_close_project = Some(runtime_id);
                                    }
                                    finish_project_actions = true;
                                }
                            }
                            PendingSaveKind::DxfExport { description } => {
                                userspace_log!("Exported {description} to DXF: {}", save.path.display());
                            }
                        }
                    };
                    if let Some(report) = console_report.as_ref() {
                        report.scope(complete);
                    } else {
                        complete();
                    }
                    drop(console_report);
                }
                Ok(Err(e)) => {
                    let console_report = save.console_report.take();
                    let mut complete = || {
                        self.finish_background_task(save.ticket, false);
                        self.redraw_requested = true;
                        let message = format!("{e:#}");
                        userspace_warn!("Save failed: {message}");
                        if let Some(runtime_id) = deferred_project_close
                            && self.workspace.project_index_for_runtime_id(runtime_id).is_some()
                        {
                            self.editor.pending_close_project = Some(runtime_id);
                            finish_project_actions = true;
                        }
                        if self.exit_after_pending_saves {
                            self.cancel_exit_request();
                        }
                        if self.project_replacement_after_save
                            && project_save_runtime_id.is_some_and(|runtime_id| self.workspace.active_project().is_some_and(|project| project.runtime_id == runtime_id))
                        {
                            self.project_replacement_after_save = false;
                            self.editor.replace_project_confirm_open = true;
                        }
                        if let Some(runtime_id) = project_save_runtime_id
                            && let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id)
                            && !self.workspace.projects[index].lossy_save_warnings.is_empty()
                        {
                            self.workspace.projects[index].lossy_save_confirmed = false;
                            self.editor.lossy_save_confirm_open = true;
                        }
                    };
                    if let Some(report) = console_report.as_ref() {
                        report.scope(&mut complete);
                    } else {
                        complete();
                    }
                    drop(console_report);
                }
                Err(mpsc::TryRecvError::Empty) => still_pending.push(save),
                Err(mpsc::TryRecvError::Disconnected) => {
                    let console_report = save.console_report.take();
                    let mut complete = || {
                        self.finish_background_task(save.ticket, false);
                        self.redraw_requested = true;
                        userspace_warn!("Save worker ended without a result");
                        if let Some(runtime_id) = deferred_project_close
                            && self.workspace.project_index_for_runtime_id(runtime_id).is_some()
                        {
                            self.editor.pending_close_project = Some(runtime_id);
                            finish_project_actions = true;
                        }
                        if self.exit_after_pending_saves {
                            self.cancel_exit_request();
                        }
                    };
                    if let Some(report) = console_report.as_ref() {
                        report.scope(&mut complete);
                    } else {
                        complete();
                    }
                    drop(console_report);
                }
            }
        }

        self.pending_saves = still_pending;
        if finish_project_actions && let Err(error) = self.finish_pending_project_actions() {
            userspace_warn!("Could not finish the pending project action: {error:#}");
        }
        if continue_project_replacement && let Err(error) = self.continue_project_replacement() {
            userspace_warn!("Could not replace the current project: {error:#}");
        }
        self.try_finish_deferred_exit();
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn poll_saves(&mut self) {}

    // ── Dialog spawners (non-blocking) ──────────────────────────────────────

    /// Start a new project.
    ///
    /// Nothing is written and nothing is asked: the project lives in memory
    /// until the first Save, which is where the destination chooser appears.
    /// A dirty project still routes through the save/discard/cancel prompt
    /// before it is replaced.
    pub(crate) fn choose_new_project(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            self.editor.new_project_name.clear();
            self.editor.new_project_dialog_open = true;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(error) = self.execute_file_dialog_action(FileDialogAction::NewProject) {
            userspace_warn!("Could not create a new project: {error:#}");
        }
    }

    /// Replace whatever is open with an empty, never-saved project.
    ///
    /// This is what the application starts on and what the welcome splash
    /// leaves behind when it is dismissed, so there is always somewhere to
    /// draw. It deliberately skips the replacement prompt: the callers either
    /// have no project to lose or have already been through it.
    pub(crate) fn start_untitled_project(&mut self) -> Result<()> {
        let mut project = project::open_project(None, project::new_empty(None))?;
        // Nothing has been drawn in it, so it is not unsaved work: quitting or
        // starting another one straight away must not raise the save/discard
        // prompt. Save still reaches it through `project_needs_first_save`.
        project.mark_saved();
        self.set_active_project(project);
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn create_browser_project(&mut self, name: String) -> Result<()> {
        self.execute_file_dialog_action(FileDialogAction::WebNewProject(name))
    }

    pub(crate) fn continue_project_replacement(&mut self) -> Result<()> {
        let Some(action) = self.pending_project_replacement.take() else {
            self.editor.replace_project_confirm_open = false;
            self.project_replacement_after_save = false;
            return Ok(());
        };
        self.editor.replace_project_confirm_open = false;
        self.project_replacement_after_save = false;
        self.project_replacement_bypass = true;
        self.execute_file_dialog_action(action)
    }

    pub(crate) fn save_and_continue_project_replacement(&mut self) -> Result<()> {
        let runtime_id = self.workspace.active_project().context("No active project to save")?.runtime_id;
        self.editor.replace_project_confirm_open = false;
        self.project_replacement_after_save = true;
        if let Err(error) = self.save_project(runtime_id) {
            self.project_replacement_after_save = false;
            self.editor.replace_project_confirm_open = true;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn discard_and_continue_project_replacement(&mut self) -> Result<()> {
        self.clear_editor_transient_state();
        self.continue_project_replacement()
    }

    pub(crate) fn cancel_project_replacement(&mut self) {
        self.pending_project_replacement = None;
        self.project_replacement_after_save = false;
        self.project_replacement_bypass = false;
        self.editor.replace_project_confirm_open = false;
    }

    pub(crate) fn confirm_lossy_project_save(&mut self) -> Result<()> {
        let runtime_id = self.workspace.active_project().context("No active project to save")?.runtime_id;
        if let Some(project) = self.workspace.active_project_mut() {
            project.lossy_save_confirmed = true;
        }
        self.editor.lossy_save_confirm_open = false;
        if let Some(action) = self.pending_lossy_save_as.take() {
            self.execute_file_dialog_action(action)
        } else {
            self.save_project(runtime_id)
        }
    }

    pub(crate) fn cancel_lossy_project_save(&mut self) {
        self.pending_lossy_save_as = None;
        if let Some(project) = self.workspace.active_project_mut() {
            project.lossy_save_confirmed = false;
        }
        if self.project_replacement_after_save {
            self.project_replacement_after_save = false;
            self.editor.replace_project_confirm_open = true;
        }
        if self.exit_after_pending_saves {
            self.cancel_exit_request();
        }
        self.editor.lossy_save_confirm_open = false;
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn activate_tracked_project(&mut self, path: PathBuf) -> Result<()> {
        if self.workspace.active_project().and_then(|project| project.path.as_ref()) == Some(&path) {
            return Ok(());
        }
        if !self.pending_project_open_paths.is_empty() {
            anyhow::bail!("Wait for the project to finish opening");
        }
        self.execute_file_dialog_action(FileDialogAction::SwitchProject(path))
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn activate_tracked_project(&mut self, project_id: crate::model::project::ProjectId) -> Result<()> {
        if self.workspace.active_project().is_some_and(|project| project.id == project_id) {
            return Ok(());
        }
        if !self.browser_project_loads_pending.is_empty() {
            anyhow::bail!("Wait for the browser project to finish opening");
        }
        if self.browser_deletes_pending.contains(&project_id) {
            anyhow::bail!("Wait for the browser project removal to finish");
        }
        self.execute_file_dialog_action(FileDialogAction::WebStoredProject(project_id))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn remove_tracked_project(&mut self, path: PathBuf) -> Result<()> {
        if self.pending_project_open_paths.contains(&path) {
            anyhow::bail!("Wait for the project to finish opening");
        }
        if let Some(runtime_id) = self
            .workspace
            .active_project()
            .filter(|project| project.path.as_ref() == Some(&path))
            .map(|project| project.runtime_id)
        {
            self.editor.remove_project_after_close = true;
            self.request_close_project(runtime_id);
        } else {
            self.tracked_project_paths.retain(|tracked| tracked != &path);
            self.persist_session();
        }
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn remove_tracked_project(&mut self, project_id: crate::model::project::ProjectId) -> Result<()> {
        if self.browser_project_loads_pending.contains(&project_id) {
            anyhow::bail!("Wait for the browser project to finish opening");
        }
        if let Some(runtime_id) = self.workspace.active_project().filter(|project| project.id == project_id).map(|project| project.runtime_id) {
            self.editor.remove_project_after_close = true;
            self.request_close_project(runtime_id);
            Ok(())
        } else {
            self.start_browser_project_delete(project_id, None)
        }
    }

    pub(crate) fn cancel_close_project(&mut self) {
        self.editor.pending_close_project = None;
        self.editor.remove_project_after_close = false;
    }

    pub(crate) fn choose_open_project(&mut self) {
        #[cfg(target_arch = "wasm32")]
        self.spawn_file_dialog(async {
            let handles = AsyncFileDialog::new().add_filter("Project", &["omf"]).pick_files().await?;
            let mut files = Vec::with_capacity(handles.len());
            for handle in handles {
                files.push(crate::model::input::read_browser_handle(handle).await);
            }
            Some(FileDialogAction::WebOpenProject(files))
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async {
            let paths = AsyncFileDialog::new()
                .add_filter("Project", &["omf"])
                .pick_files()
                .await?
                .into_iter()
                .map(FileHandleExt::into_path)
                .collect();
            Some(FileDialogAction::OpenProject(paths))
        });
    }

    pub(crate) fn import_dxf_paths_into(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = paths;
            let files = self.take_web_import_files(DataMenu::Dxf)?;
            let project = self.workspace.active_project().context("No active .omf to import into")?;
            let runtime_id = project.runtime_id;
            let document_revision = project.project.document.revision();
            let compute = move |cancel: &crate::app::jobs::CancelFlag| {
                let mut parsed = Vec::with_capacity(files.len());
                for file in files {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    parsed.push((file.source.name.clone(), formats::dxf::document_from_bytes(&file.source.name, &file.bytes)?));
                }
                Ok(parsed)
            };
            let apply = move |app: &mut App, result: Result<Vec<(String, crate::model::Document)>>| {
                let parsed = match result {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        userspace_warn!("DXF import failed: {error:#}");
                        return;
                    }
                };
                let Some(index) = app.workspace.project_index_for_runtime_id(runtime_id) else {
                    return;
                };
                let project = &mut app.workspace.projects[index];
                let existing: std::collections::HashSet<_> = project.project.document.layers().iter().map(|layer| layer.id).collect();
                let mut total = 0usize;
                for (name, document) in parsed {
                    let added = project::merge_document(&mut project.project.document, &document);
                    total += added;
                    userspace_log!("Imported {added} object(s) from {name}");
                }
                let new_layers: Vec<_> = project
                    .project
                    .document
                    .layers()
                    .iter()
                    .filter(|layer| !existing.contains(&layer.id))
                    .map(|layer| layer.id)
                    .collect();
                project.loaded_layers.extend(new_layers.iter().copied());
                if app.workspace.active_index == Some(index)
                    && let Some(&layer) = new_layers.first()
                {
                    app.editor.active_layer = Some(layer);
                }
                app.invalidate_geometry();
                app.fit_view_to_extents();
                userspace_log!("Imported {total} DXF object(s)");
            };
            self.spawn_job(
                "Parsing browser DXF import…",
                vec![crate::app::jobs::JobKey::Project { runtime_id, document_revision }],
                compute,
                apply,
            );
            Ok(())
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.execute_file_dialog_action(FileDialogAction::ImportDxfInto { paths })
    }

    #[cfg(target_arch = "wasm32")]
    pub(super) fn take_web_import_files(&mut self, kind: DataMenu) -> Result<Vec<crate::model::input::InputFile>> {
        match self.web_import_files.take() {
            Some((stored_kind, files)) if stored_kind == kind => {
                self.editor.import_source_paths.clear();
                self.editor.import_source_menu = DataMenu::None;
                Ok(files)
            }
            Some(other) => {
                self.web_import_files = Some(other);
                anyhow::bail!("Choose the source files again before importing")
            }
            None => anyhow::bail!("Choose source files before importing"),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn take_current_web_import_files(&mut self) -> Result<(DataMenu, Vec<crate::model::input::InputFile>)> {
        let selected = self.web_import_files.take().context("Choose source files before importing")?;
        self.editor.import_source_paths.clear();
        self.editor.import_source_menu = DataMenu::None;
        Ok(selected)
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn clear_browser_import_selection(&mut self, kind: DataMenu) {
        if self.web_import_files.as_ref().is_some_and(|(stored_kind, _)| *stored_kind == kind) {
            self.web_import_files = None;
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_triangulation_sources(&mut self) -> Result<()> {
        let (kind, files) = self.take_current_web_import_files()?;
        if !matches!(kind, DataMenu::Obj | DataMenu::Stl | DataMenu::Ply) {
            anyhow::bail!("Choose triangulation files before importing");
        }
        for file in files {
            self.open_triangulation_input(file);
        }
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_point_cloud_sources(&mut self) -> Result<()> {
        let (kind, files) = self.take_current_web_import_files()?;
        if !matches!(kind, DataMenu::Las | DataMenu::Xyz | DataMenu::Pcd) {
            anyhow::bail!("Choose point-cloud files before importing");
        }
        for file in files {
            let path = crate::app::browser_source_filename(&file.source.name);
            self.open_point_cloud_input(file, path);
        }
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_raster_sources(&mut self) -> Result<()> {
        let files = self.take_web_import_files(DataMenu::Geotiff)?;
        for file in files {
            let path = crate::app::browser_source_filename(&file.source.name);
            self.open_raster_input(file, path);
        }
        Ok(())
    }

    pub(crate) fn choose_import_source_files(&mut self, kind: DataMenu) {
        #[cfg(target_arch = "wasm32")]
        self.spawn_file_dialog(async move {
            let dialog = match kind {
                DataMenu::Dxf => AsyncFileDialog::new().add_filter("AutoCAD DXF", &["dxf"]),
                DataMenu::Omf => AsyncFileDialog::new().add_filter("Open Mining Format", &["omf"]),
                DataMenu::Obj => AsyncFileDialog::new().add_filter("Wavefront OBJ", &["obj"]),
                DataMenu::Stl => AsyncFileDialog::new().add_filter("STL", &["stl"]),
                DataMenu::Ply => AsyncFileDialog::new().add_filter("PLY", &["ply"]),
                DataMenu::Las => AsyncFileDialog::new().add_filter("LiDAR point cloud", &["las", "laz"]),
                DataMenu::Xyz => AsyncFileDialog::new().add_filter("ASCII point cloud", &["xyz", "pts"]),
                DataMenu::Pcd => AsyncFileDialog::new().add_filter("Point Cloud Data", &["pcd"]),
                DataMenu::CsvBlockModel => AsyncFileDialog::new().add_filter("CSV block model", &["csv"]),
                DataMenu::CsvDrillHole => AsyncFileDialog::new().add_filter("Drillhole CSV files", &["csv"]),
                DataMenu::Geotiff => AsyncFileDialog::new().add_filter("GeoTIFF", &["tif", "tiff"]),
                _ => AsyncFileDialog::new(),
            };
            let handles = if kind == DataMenu::CsvBlockModel {
                vec![dialog.pick_file().await?]
            } else {
                dialog.pick_files().await?
            };
            let mut files = Vec::with_capacity(handles.len());
            for handle in handles {
                files.push(crate::model::input::read_browser_handle(handle).await);
            }
            Some(FileDialogAction::WebSetImportSourceFiles { kind, files })
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let dialog = match kind {
                DataMenu::Dxf => AsyncFileDialog::new().add_filter("AutoCAD DXF", &["dxf"]),
                DataMenu::Omf => AsyncFileDialog::new().add_filter("Open Mining Format", &["omf"]),
                DataMenu::Obj => AsyncFileDialog::new().add_filter("Wavefront OBJ", &["obj"]),
                DataMenu::Stl => AsyncFileDialog::new().add_filter("STL", &["stl"]),
                DataMenu::Ply => AsyncFileDialog::new().add_filter("PLY", &["ply"]),
                DataMenu::Las => AsyncFileDialog::new().add_filter("LiDAR point cloud", &["las", "laz"]),
                DataMenu::Xyz => AsyncFileDialog::new().add_filter("ASCII point cloud", &["xyz", "pts"]),
                DataMenu::Pcd => AsyncFileDialog::new().add_filter("Point Cloud Data", &["pcd"]),
                DataMenu::CsvBlockModel => AsyncFileDialog::new().add_filter("CSV block model", &["csv"]),
                DataMenu::CsvDrillHole => AsyncFileDialog::new().add_filter("Drillhole CSV files", &["csv"]),
                DataMenu::Geotiff => AsyncFileDialog::new().add_filter("GeoTIFF", &["tif", "tiff"]),
                _ => AsyncFileDialog::new(),
            };
            let paths: Vec<PathBuf> = if kind == DataMenu::CsvBlockModel {
                vec![dialog.pick_file().await?.into_path()]
            } else {
                dialog.pick_files().await?.into_iter().map(FileHandleExt::into_path).collect()
            };
            Some(FileDialogAction::SetImportSourcePaths { kind, paths })
        });
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_csv_block_model(&mut self, mapping: crate::model::formats::csv_block_model::CsvColumnMapping) -> Result<()> {
        let mut files = self.take_web_import_files(DataMenu::CsvBlockModel)?;
        let file = files.pop().context("Choose a .csv file before importing")?;
        if !files.is_empty() {
            anyhow::bail!("Choose one CSV block-model file at a time");
        }
        self.editor.import_csv_preview = None;
        self.editor.import_csv_error = None;
        let path = crate::app::browser_source_filename(&file.source.name);
        self.open_block_model_input(file, crate::model::block_model::BlockModelSource { path, csv_columns: Some(mapping) })
    }

    pub(crate) fn choose_export_project_dxf(&mut self, project_runtime_id: u32) {
        self.commit_export_move_if_needed(project_runtime_id);
        if self.workspace.project_index_for_runtime_id(project_runtime_id).is_none() {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        self.start_browser_export(FileDialogAction::WebDownloadDxf {
            project_runtime_id,
            layer: None,
            file_name: "project.dxf".to_owned(),
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("DXF", &["dxf"])
                .set_file_name("project.dxf")
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::ExportProjectDxf { project_runtime_id, path })
        });
    }

    pub(crate) fn choose_export_block_model_csv(&mut self, id: BlockModelId) {
        let Some(model) = self.block_models.iter().find(|model| model.id == id) else {
            userspace_warn!("The selected block model is no longer loaded");
            return;
        };
        let stem = Path::new(&model.name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("block_model");
        let default_name = format!("{stem}.csv");
        #[cfg(target_arch = "wasm32")]
        self.start_browser_export(FileDialogAction::WebDownloadBlockModelCsv {
            id,
            file_name: default_name,
            close_after: false,
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let path = AsyncFileDialog::new()
                .add_filter("CSV block model", &["csv"])
                .set_file_name(&default_name)
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::ExportBlockModelCsv { id, path })
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn export_block_model_csv_to_path(&mut self, id: BlockModelId, mut path: PathBuf) -> Result<()> {
        if path.extension().is_none() {
            path.set_extension("csv");
        }
        let block_model = self
            .block_models
            .iter()
            .find(|model| model.id == id)
            .context("The selected block model is no longer loaded")?;
        let model = block_model.model.clone();
        let blocks = std::sync::Arc::clone(&block_model.blocks);
        let renderable = std::sync::Arc::clone(&block_model.renderable_block_indices);
        let display_path = path.clone();
        self.spawn_job(
            format!("Exporting {}…", file_name(&path)),
            vec![crate::app::jobs::JobKey::BlockModel(id)],
            move |cancel| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                crate::model::atomic_file::write_atomic(&path, |file| {
                    crate::model::formats::csv_block_model::write(&model, &blocks, &renderable, file).map_err(anyhow::Error::new)
                })
            },
            move |_app, result| match result {
                Ok(()) => userspace_log!("Exported block-model CSV to {}", display_path.display()),
                Err(error) => userspace_warn!("Block-model CSV export failed: {error:#}"),
            },
        );
        Ok(())
    }

    pub(crate) fn choose_export_layer_dxf(&mut self, layer: LayerId) {
        if self.has_pending_move_delta() {
            self.commit_pending_move();
        }
        let Some(project_index) = self.workspace.project_index_for_layer(layer) else {
            return;
        };
        let project = &self.workspace.projects[project_index];
        let project_runtime_id = project.runtime_id;
        let layer_name = project.project.document.layer(layer).map(|layer| layer.name.clone()).unwrap_or_else(|| "layer".to_string());
        let default_name = format!("{}.dxf", sanitize_file_stem(&layer_name));
        #[cfg(target_arch = "wasm32")]
        self.start_browser_export(FileDialogAction::WebDownloadDxf {
            project_runtime_id,
            layer: Some(layer),
            file_name: default_name,
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("DXF", &["dxf"])
                .set_file_name(&default_name)
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::ExportLayerDxf { project_runtime_id, layer, path })
        });
    }

    pub(crate) fn choose_export_triangulation_as(&mut self, id: TriangulationId, format: MeshFormat) {
        let stem = self
            .triangulations
            .iter()
            .find(|t| t.id == id)
            .map(|triangulation| sanitize_file_stem(&triangulation.name))
            .unwrap_or_else(|| "triangulation".to_owned());
        #[cfg(target_arch = "wasm32")]
        {
            let (_, extension) = mesh_format_name_and_extension(format);
            let default_name = format!("{stem}.{extension}");
            self.start_browser_export(FileDialogAction::WebDownloadTriangulation {
                id,
                format,
                file_name: default_name,
                close_after: false,
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let (name, extension) = mesh_format_name_and_extension(format);
            let default_name = format!("{stem}.{extension}");
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter(name, &[extension])
                .set_file_name(default_name)
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::ExportTriangulation { id, path })
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn spawn_save_project_as_dialog(&mut self, project_runtime_id: u32) {
        if self.project_revert_is_pending(project_runtime_id) {
            userspace_warn!("Wait for the project revert to finish before saving");
            return;
        }
        if self.project_save_is_pending(project_runtime_id) {
            userspace_warn!("Wait for the current project save to finish");
            return;
        }
        if self.workspace.active_project().is_some_and(|project| project.runtime_id == project_runtime_id) {
            self.commit_pending_move();
        }
        if self.workspace.project_index_for_runtime_id(project_runtime_id).is_none() {
            return;
        }
        self.spawn_file_dialog(async move {
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("Project", &["omf"])
                .set_file_name("project.omf")
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::SaveProjectAs { project_runtime_id, path })
        });
    }

    pub(crate) fn spawn_export_viewport_image_dialog(&mut self) {
        #[cfg(target_arch = "wasm32")]
        self.start_browser_export(FileDialogAction::WebViewportImage("viewport.png".to_owned()));
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("PNG image", &["png"])
                .set_file_name("viewport.png")
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::ExportViewportImage(path))
        });
    }

    /// Choose where the composed plot sheet is written.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn spawn_plot_sheet_dialog(&mut self, file_name: String) {
        self.spawn_file_dialog(async move {
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("PNG image", &["png"])
                .set_file_name(&file_name)
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::ExportPlotSheet(path))
        });
    }

    /// Save the project when its stored copy is missing or out of date.
    /// Returns false when Save As is still pending for a never-saved project.
    pub(crate) fn save_dirty_project(&mut self) -> Result<bool> {
        if self.has_pending_move_delta() {
            self.commit_pending_move();
        }
        if self.editor.text_editing_enabled {
            anyhow::bail!("Apply or discard the current text edit before saving the project");
        }
        let dirty_ids: Vec<u32> = self
            .workspace
            .active_project()
            .filter(|project| self.project_content_is_dirty(project.runtime_id) || project_needs_first_save(project))
            .map(|project| project.runtime_id)
            .into_iter()
            .collect();
        for runtime_id in dirty_ids {
            let save_as_required = cfg!(not(target_arch = "wasm32"))
                && self
                    .workspace
                    .project_index_for_runtime_id(runtime_id)
                    .and_then(|index| self.workspace.projects.get(index))
                    .is_some_and(|project| project.path.is_none());
            self.save_project(runtime_id)?;
            if save_as_required {
                // A pathless project has opened a native Save As dialog. Its
                // completion re-enters the deferred-save flow.
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Enter the fatal-shutdown state after an unrecoverable renderer
    /// failure. Unlike the ordinary exit path there is no working surface to
    /// draw confirmation dialogs on, so dirty work is preserved by writing
    /// recovery copies immediately; `about_to_wait` then exits once atomic
    /// background writers have settled.
    pub(crate) fn begin_fatal_shutdown(&mut self, reason: &str) {
        log::error!("Fatal renderer failure: {reason}");
        userspace_warn!("Fatal renderer failure: {reason}");
        // Fold any live move preview into the documents so the recovery
        // copies capture what the user last saw.
        if self.has_pending_move_delta() {
            self.commit_pending_move();
        }
        #[cfg(not(target_arch = "wasm32"))]
        match crate::app::io::data_path("recovery") {
            Ok(recovery_dir) => match self
                .workspace
                .active_project()
                .map(|project| project.runtime_id)
                .filter(|runtime_id| self.project_content_is_dirty(*runtime_id))
            {
                Some(runtime_id) => match self.omf_export_snapshot() {
                    Ok(snapshot) => match write_recovery_copy(snapshot, runtime_id, &recovery_dir) {
                        Ok(report) => {
                            for path in &report.written {
                                log::error!("Recovery copy written: {}", path.display());
                                userspace_warn!("Recovery copy written: {}", path.display());
                            }
                            for failure in &report.failures {
                                log::error!("Recovery copy failed: {failure}");
                                userspace_warn!("Recovery copy failed: {failure}");
                            }
                            log::error!("Recovery copies are in {}; reopen them after restarting", recovery_dir.display());
                        }
                        Err(error) => {
                            log::error!("Could not write recovery copies: {error:#}");
                        }
                    },
                    Err(error) => log::error!("Could not snapshot the dirty project for recovery: {error:#}"),
                },
                None => log::error!("No unsaved project content; nothing to recover"),
            },
            Err(error) => {
                log::error!("No recovery directory available: {error:#}");
            }
        }
        #[cfg(target_arch = "wasm32")]
        log::error!("Browser recovery files are unavailable; saved projects remain in IndexedDB");
        self.persist_session();
        self.fatal_shutdown = true;
        self.redraw_requested = true;
    }

    pub(crate) fn request_exit(&mut self) -> Result<()> {
        self.exit_after_pending_saves = false;
        self.discard_changes_on_deferred_exit = false;
        if self.has_unsaved_changes_for_exit() {
            self.editor.exit_confirm_open = true;
            userspace_log!("User requested exit (project export or unsaved-work confirmation required)");
        } else if !self.pending_saves.is_empty() {
            self.exit_after_pending_saves = true;
            self.discard_changes_on_deferred_exit = true;
            self.redraw_requested = true;
            userspace_log!("Exit deferred until background exports finish");
        } else {
            self.persist_session();
            self.close_requested = true;
            userspace_log!("Exit requested with no unsaved changes");
        }
        Ok(())
    }

    pub(crate) fn save_and_exit(&mut self) -> Result<()> {
        self.exit_after_pending_saves = true;
        self.discard_changes_on_deferred_exit = false;
        let started = self.save_dirty_project()?;
        // The exit prompt has done its job either way: from here the deferred
        // exit is driven by whatever the save is waiting on, and a lossy-OMF
        // confirmation draws its own dialog on top.
        self.editor.exit_confirm_open = false;
        // `save_dirty_project` reporting true only means no Save As dialog is
        // outstanding. The write itself may still be running in the background,
        // and an OMF Incline cannot round-trip replaces the save with a
        // confirmation prompt - exiting on that alone quits without writing.
        if started && !self.project_save_is_in_flight() && self.pending_file_dialogs.is_empty() && !self.has_unsaved_changes_for_exit() {
            self.finish_deferred_exit();
        }
        Ok(())
    }

    pub(crate) fn exit_without_saving(&mut self) {
        self.editor.exit_confirm_open = false;
        if !self.pending_saves.is_empty() {
            self.exit_after_pending_saves = true;
            self.discard_changes_on_deferred_exit = true;
            self.redraw_requested = true;
            userspace_log!("Exit deferred until background exports finish");
            return;
        }
        self.exit_after_pending_saves = false;
        self.discard_changes_on_deferred_exit = false;
        self.persist_session();
        self.close_requested = true;
        userspace_log!("User chose to exit without saving");
    }

    pub(crate) fn cancel_exit_request(&mut self) {
        self.exit_after_pending_saves = false;
        self.discard_changes_on_deferred_exit = false;
        self.editor.exit_confirm_open = false;
    }

    pub(crate) fn has_unsaved_changes_for_exit(&self) -> bool {
        self.workspace.active_project().is_some_and(|project| self.project_content_is_dirty(project.runtime_id)) || self.editor.text_editing_enabled
    }

    /// Whether a project write that has already been handed off is still
    /// running. Native writes sit in `pending_saves`; browser writes report
    /// back through `AppEvent::BrowserProjectSaved` instead.
    pub(crate) fn project_save_is_in_flight(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            !self.pending_saves.is_empty() || !self.browser_saves_pending.is_empty()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            !self.pending_saves.is_empty()
        }
    }

    pub(crate) fn try_finish_deferred_exit(&mut self) {
        if !self.exit_after_pending_saves || !self.pending_file_dialogs.is_empty() || self.project_save_is_in_flight() {
            return;
        }
        // The lossy-OMF confirmation is waiting on the user; re-entering the
        // save path here would reopen it every frame.
        if self.editor.lossy_save_confirm_open {
            return;
        }
        if self.discard_changes_on_deferred_exit {
            self.finish_deferred_exit();
            return;
        }
        match self.save_dirty_project() {
            Ok(true) if !self.has_unsaved_changes_for_exit() => self.finish_deferred_exit(),
            Ok(_) => {}
            Err(error) => {
                userspace_warn!("Could not finish saving before exit: {error:#}");
                self.cancel_exit_request();
            }
        }
    }

    fn finish_deferred_exit(&mut self) {
        self.exit_after_pending_saves = false;
        self.discard_changes_on_deferred_exit = false;
        self.editor.exit_confirm_open = false;
        self.persist_session();
        self.close_requested = true;
    }

    pub(crate) fn save_project(&mut self, runtime_id: u32) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.project_revert_is_pending(runtime_id) {
            anyhow::bail!("Wait for the project revert to finish before saving");
        }
        let index = self.workspace.project_index_for_runtime_id(runtime_id).context("The selected project is no longer open")?;
        if !self.workspace.projects[index].lossy_save_warnings.is_empty() && !self.workspace.projects[index].lossy_save_confirmed {
            self.pending_lossy_save_as = None;
            self.editor.lossy_save_confirm_open = true;
            self.redraw_requested = true;
            return Ok(());
        }
        if self.workspace.active_index == Some(index) && self.has_pending_move_delta() {
            self.commit_pending_move();
        }
        self.ensure_project_has_no_pending_text_edit(index)?;
        #[cfg(target_arch = "wasm32")]
        {
            self.save_browser_project(runtime_id)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(path) = self.workspace.projects[index].path.clone() else {
                self.spawn_save_project_as_dialog(runtime_id);
                return Ok(());
            };
            if self
                .pending_saves
                .iter()
                .any(|save| matches!(save.kind, PendingSaveKind::Project { runtime_id: pending, .. } if pending == runtime_id) || save.path == path)
            {
                return Ok(());
            }
            let (snapshot_hash, snapshot_layer_hashes) = {
                let project = &self.workspace.projects[index];
                (project.current_content_hash(), project.current_layer_hashes())
            };
            let snapshot = self.omf_export_snapshot()?;
            let asset_token = self.project_asset_save_token();
            let kind = PendingSaveKind::Project {
                runtime_id,
                snapshot_hash,
                snapshot_layer_hashes,
                asset_token,
                close_after: false,
                save_as_previous_name: None,
            };
            self.spawn_project_write(kind, snapshot, path);
            Ok(())
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn save_browser_project(&mut self, runtime_id: u32) -> Result<()> {
        let index = self.workspace.project_index_for_runtime_id(runtime_id).context("The selected project is no longer open")?;
        // Once deletion has been confirmed, it owns this project's storage
        // lifecycle. A concurrent Save must not recreate the IndexedDB record
        // after the delete transaction completes.
        let stored_project_delete_pending = match &self.workspace.projects[index].persistence {
            crate::model::project::ProjectPersistence::BrowserRecord(project_id) => self.browser_deletes_pending.contains(project_id),
            _ => false,
        };
        if stored_project_delete_pending || self.browser_delete_after_save.contains(&runtime_id) {
            return Ok(());
        }
        if self.browser_saves_pending.contains(&runtime_id) {
            return Ok(());
        }
        self.ensure_project_has_no_pending_text_edit(index)?;
        let project_id = match self.workspace.projects[index].persistence {
            crate::model::project::ProjectPersistence::BrowserRecord(id) => id,
            _ => uuid::Uuid::new_v4(),
        };
        let (snapshot_hash, snapshot_layer_hashes) = {
            let project = &self.workspace.projects[index];
            (project.current_content_hash(), project.current_layer_hashes())
        };
        let asset_token = self.project_asset_save_token();
        let snapshot = self.omf_export_snapshot()?;
        let name = snapshot.name.clone();
        let proxy = self.web_event_loop_proxy.clone().context("browser event loop is unavailable")?;
        self.browser_saves_pending.insert(runtime_id);
        wasm_bindgen_futures::spawn_local(async move {
            let result = async {
                let progress = crate::model::progress::Progress::new();
                let omf_bytes = formats::omf::to_bytes(snapshot, &progress.phase(0.0, 1.0)).map_err(|error| format!("{error:#}"))?;
                let record = crate::app::web_storage::BrowserProjectRecord {
                    id: project_id,
                    name,
                    omf_bytes,
                    saved_at_ms: js_sys::Date::now() as u64,
                };
                crate::app::web_storage::put_project(&record).await
            }
            .await;
            let _ = proxy.send_event(crate::app::AppEvent::BrowserProjectSaved {
                runtime_id,
                project_id,
                snapshot_hash,
                snapshot_layer_hashes,
                asset_token,
                result,
            });
        });
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_project_write(&mut self, kind: PendingSaveKind, snapshot: formats::omf::ProjectSnapshot, path: PathBuf) {
        let (ticket, progress) = self.begin_reported_task(save_label(&kind, &path));
        let (result_tx, result_rx) = mpsc::channel();
        self.pending_saves.push(PendingSave {
            ticket,
            console_report: crate::logging::retain_current_report(),
            kind,
            path: path.clone(),
            result_rx,
        });
        let window = self.window.clone();
        crate::app::jobs::spawn_io_task(move || {
            let result = crate::app::jobs::run_compute_catching_panic(|| {
                formats::omf::write_path(snapshot, &path, &progress.phase(0.0, 1.0)).with_context(|| format!("Failed to save project {}", path.display()))
            });
            let _ = result_tx.send(result);
            if let Some(window) = window {
                window.request_redraw();
            }
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_dxf_write(&mut self, snapshot: project::ProjectFile, layer: Option<LayerId>, path: PathBuf, description: String) {
        let kind = PendingSaveKind::DxfExport { description };
        let (ticket, _progress) = self.begin_reported_task(save_label(&kind, &path));
        let (result_tx, result_rx) = mpsc::channel();
        self.pending_saves.push(PendingSave {
            ticket,
            console_report: crate::logging::retain_current_report(),
            kind,
            path: path.clone(),
            result_rx,
        });
        let window = self.window.clone();
        crate::app::jobs::spawn_io_task(move || {
            // The DXF writer builds and writes the whole drawing in one call,
            // so there is nothing to count: the bar stays a marquee.
            let result = crate::app::jobs::run_compute_catching_panic(|| match layer {
                Some(layer) => formats::dxf::export_layer(&snapshot, layer, &path),
                None => formats::dxf::export_project(&snapshot, &path),
            });
            let _ = result_tx.send(result);
            if let Some(window) = window {
                window.request_redraw();
            }
        });
    }

    pub(super) fn ensure_project_has_no_pending_text_edit(&self, project_index: usize) -> Result<()> {
        if self.editor.text_editing_enabled
            && self
                .editor
                .editing_labels_id
                .and_then(|object_id| self.workspace.project_index_for_object(object_id))
                .or(self.workspace.active_index)
                == Some(project_index)
        {
            anyhow::bail!("Apply or discard the current text edit before saving this project");
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ensure_project_save_path_available(&self, project_index: usize, path: &Path) -> Result<()> {
        let runtime_id = self.workspace.projects[project_index].runtime_id;
        if self.project_save_is_pending(runtime_id) {
            anyhow::bail!("Wait for the current project save to finish");
        }
        self.ensure_save_path_not_pending(path)?;
        if self
            .workspace
            .projects
            .iter()
            .enumerate()
            .any(|(index, project)| index != project_index && project.path.as_deref() == Some(path))
        {
            anyhow::bail!("Another open project already uses {}", path.display());
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_path_is_pending(&self, path: &Path) -> bool {
        self.pending_saves.iter().any(|save| save.path == path) || self.pending_project_open_paths.contains(path)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ensure_save_path_not_pending(&self, path: &Path) -> Result<()> {
        if self.save_path_is_pending(path) {
            anyhow::bail!("A file operation involving {} is already in progress", path.display());
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn project_save_is_pending(&self, runtime_id: u32) -> bool {
        self.pending_saves.iter().any(|save| {
            matches!(
                save.kind,
                PendingSaveKind::Project {
                    runtime_id: pending,
                    ..
                } if pending == runtime_id
            )
        })
    }

    /// A running file write cannot be cancelled safely. Remember the close
    /// request on that write and finish it only after the result is known.
    #[cfg(not(target_arch = "wasm32"))]
    fn defer_project_close_until_save_finishes(&mut self, runtime_id: u32) -> bool {
        let mut deferred = false;
        for save in &mut self.pending_saves {
            if let PendingSaveKind::Project {
                runtime_id: pending, close_after, ..
            } = &mut save.kind
                && *pending == runtime_id
            {
                *close_after = true;
                deferred = true;
                break;
            }
        }
        if deferred {
            self.editor.pending_close_project = None;
        }
        deferred
    }

    pub(crate) fn request_close_project(&mut self, runtime_id: u32) {
        #[cfg(target_arch = "wasm32")]
        if !self.browser_project_loads_pending.is_empty() {
            userspace_warn!("Wait for the current project switch to finish");
            return;
        }
        let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        {
            if self.project_content_is_dirty(runtime_id) || project_needs_first_save(&self.workspace.projects[index]) || self.editor.text_editing_enabled {
                self.editor.pending_close_project = Some(runtime_id);
            } else {
                self.close_project(runtime_id);
            }
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.defer_project_close_until_save_finishes(runtime_id) {
            userspace_log!("Project will close after its current save finishes");
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.project_content_is_dirty(runtime_id) || self.pending_text_edit_project_index() == Some(index) {
            self.editor.pending_close_project = Some(runtime_id);
        } else {
            self.close_project(runtime_id);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn finish_pending_project_actions(&mut self) -> Result<()> {
        let Some(runtime_id) = self.editor.pending_close_project else {
            return Ok(());
        };
        if self.project_save_is_pending(runtime_id) {
            return Ok(());
        }
        if self.project_content_is_dirty(runtime_id)
            || self
                .workspace
                .project_index_for_runtime_id(runtime_id)
                .is_some_and(|index| self.pending_text_edit_project_index() == Some(index))
        {
            return Ok(());
        }
        #[cfg(target_arch = "wasm32")]
        let result = {
            self.close_project(runtime_id);
            Ok(())
        };
        #[cfg(not(target_arch = "wasm32"))]
        let result = {
            self.close_project(runtime_id);
            Ok(())
        };
        result
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn project_revert_is_pending(&self, runtime_id: u32) -> bool {
        self.pending_jobs.iter().any(|job| {
            (job.label == PROJECT_REVERT_JOB_LABEL || job.label == LAYER_REVERT_JOB_LABEL)
                && job.keys.iter().any(|key| {
                    matches!(
                        key,
                        crate::app::jobs::JobKey::Project {
                            runtime_id: pending,
                            ..
                        } if *pending == runtime_id
                    )
                })
        })
    }

    /// Revert a project to its last saved state by re-parsing it from disk. The
    /// reload happens on a job thread; the swap preserves the project's slot,
    /// active status, and loaded-layer set (matched by stable local layer id).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn discard_project_changes(&mut self, runtime_id: u32) -> Result<()> {
        self.editor.pending_discard_project = None;
        let index = self.workspace.project_index_for_runtime_id(runtime_id).context("The selected .omf is no longer open")?;
        if self.pending_saves.iter().any(|save| {
            matches!(
                save.kind,
                PendingSaveKind::Project {
                    runtime_id: pending,
                    ..
                } if pending == runtime_id
            )
        }) {
            anyhow::bail!("Wait for the project save to finish before discarding changes");
        }
        // Confirmation commits the intent to abandon any current draft. Do
        // this before capturing the job revision; cancelling a new text/move
        // draft may itself restore the document and advance that revision.
        if self.workspace.active_index == Some(index) {
            self.clear_editor_transient_state();
        }
        let path = self.workspace.projects[index]
            .path
            .clone()
            .context("This project has never been saved, so there is nothing to revert to")?;
        // Keyed to the current revision: if the user keeps editing while the
        // file re-parses, the stale revert is dropped instead of clobbering.
        let document_revision = self.workspace.projects[index].project.document.revision();
        let expected_hash = self.workspace.projects[index].current_content_hash();

        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let source_name = file_name(&path);
            let bundle = formats::omf::from_bytes(&source_name, bytes, &progress.phase(0.0, 1.0))?;
            Ok((path, source_name, bundle))
        };
        let apply = move |app: &mut App, result: Result<(PathBuf, String, formats::omf::ImportBundle)>| {
            let (path, source_name, bundle) = match result {
                Ok(loaded) => loaded,
                Err(error) => {
                    userspace_warn!("Could not reload project from disk: {error:#}");
                    return;
                }
            };
            let Some(index) = app.workspace.project_index_for_runtime_id(runtime_id) else {
                return;
            };
            if app.workspace.projects[index].current_content_hash() != expected_hash {
                userspace_warn!("Discard was cancelled because the project changed while the OMF was reloading");
                return;
            }
            app.apply_opened_omf_bundle(Some(path.clone()), source_name, bundle, ViewOnOpen::Keep);
            userspace_log!("Discarded changes: reloaded {}", path.display());
        };
        self.spawn_job_reporting_progress(
            PROJECT_REVERT_JOB_LABEL,
            vec![crate::app::jobs::JobKey::Project { runtime_id, document_revision }],
            compute,
            apply,
        );
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn request_discard_layer_changes(&mut self, layer_id: LayerId) {
        let Some(index) = self.workspace.project_index_for_layer(layer_id) else {
            return;
        };
        let project = &self.workspace.projects[index];
        if project.path.is_none() || !project.dirty_layer_ids().contains(&layer_id) {
            return;
        }
        if self.pending_saves.iter().any(|save| {
            matches!(
                save.kind,
                PendingSaveKind::Project {
                    runtime_id: pending,
                    ..
                } if pending == project.runtime_id
            )
        }) || self.project_revert_is_pending(project.runtime_id)
        {
            userspace_warn!("Wait for the project operation to finish before discarding changes");
            return;
        }
        if let Some(layer) = project.project.document.layer(layer_id) {
            self.editor.pending_discard_layer = Some((layer_id, layer.name.clone()));
        }
    }

    /// Restore one layer from disk while carrying every other dirty layer into
    /// the freshly parsed project. Rebuilding from the saved project also handles
    /// newly created target layers correctly: if no saved layer has that local
    /// id, confirming discard removes it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn discard_layer_changes(&mut self, layer_id: LayerId) -> Result<()> {
        self.editor.pending_discard_layer = None;
        let index = self.workspace.project_index_for_layer(layer_id).context("The selected layer is no longer open")?;
        let runtime_id = self.workspace.projects[index].runtime_id;
        if self.pending_saves.iter().any(|save| {
            matches!(
                save.kind,
                PendingSaveKind::Project {
                    runtime_id: pending,
                    ..
                } if pending == runtime_id
            )
        }) || self.project_revert_is_pending(runtime_id)
        {
            anyhow::bail!("Wait for the project operation to finish before discarding changes");
        }

        const LOCAL_MASK: u64 = u32::MAX as u64;
        let target_local_id = layer_id.0 & LOCAL_MASK;
        let active_layer_local_id = (self.workspace.active_index == Some(index))
            .then_some(self.editor.active_layer)
            .flatten()
            .map(|active| active.0 & LOCAL_MASK);
        if self.workspace.active_index == Some(index) {
            self.clear_editor_transient_state();
        }

        let project = &self.workspace.projects[index];
        let path = project.path.clone().context("This project has never been saved, so its layers cannot be restored")?;
        let target_name = project
            .project
            .document
            .layer(layer_id)
            .map(|layer| layer.name.clone())
            .unwrap_or_else(|| "layer".to_owned());
        let document_revision = project.project.document.revision();

        // Convert the current document back to portable ids, then retain only
        // snapshots for other dirty layers. The target is deliberately omitted
        // so the copy loaded from disk wins.
        let preserved_local_ids = project
            .dirty_layer_ids()
            .into_iter()
            .map(|dirty| dirty.0 & LOCAL_MASK)
            .filter(|local| *local != target_local_id)
            .collect::<std::collections::HashSet<_>>();
        let mut portable_current = project.project.clone();
        portable_current.document.apply_runtime_namespace(0);
        let preserved_layers: Vec<LayerSnapshot> = portable_current
            .document
            .layers()
            .iter()
            .enumerate()
            .filter(|(_, layer)| preserved_local_ids.contains(&layer.id.0))
            .map(|(layer_index, layer)| {
                let objects = portable_current
                    .document
                    .objects()
                    .iter()
                    .enumerate()
                    .filter(|(_, object)| object.layer() == layer.id)
                    .map(|(object_index, object)| (object_index, object.clone()))
                    .collect();
                (layer_index, layer.clone(), objects)
            })
            .collect();

        let compute = move |cancel: &crate::app::jobs::CancelFlag| {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let source_name = file_name(&path);
            let progress = crate::model::progress::Progress::new();
            let bundle = formats::omf::from_bytes(&source_name, bytes, &progress.phase(0.0, 1.0))?;
            let mut design = project::new_empty(Some(path.clone()));
            design.metadata.name = bundle.project_name;
            for imported in bundle.designs {
                project::merge_document_preserve_ids(&mut design.document, &imported.document);
            }
            Ok((path, design))
        };
        let apply = move |app: &mut App, result: Result<(PathBuf, project::ProjectFile)>| {
            let (path, project) = match result {
                Ok(loaded) => loaded,
                Err(error) => {
                    userspace_warn!("Could not reload layer from disk: {error:#}");
                    return;
                }
            };
            let Some(index) = app.workspace.project_index_for_runtime_id(runtime_id) else {
                return;
            };
            if app.workspace.projects[index].project.document.revision() != document_revision {
                userspace_warn!("Layer discard was cancelled because the project changed while the project was reloading");
                return;
            }
            let mut replacement = match project::open_project(Some(path), project) {
                Ok(project) => project,
                Err(error) => {
                    userspace_warn!("Could not restore layer from project: {error:#}");
                    return;
                }
            };
            for (layer_index, layer, objects) in preserved_layers {
                replacement.project.document.replace_layer_snapshot(layer_index, layer, objects);
            }

            let was_active = app.workspace.active_index == Some(index);
            if was_active {
                app.clear_editor_transient_state();
            }
            app.cancel_jobs(|key| {
                matches!(
                    key,
                    crate::app::jobs::JobKey::Project {
                        runtime_id: dependency_id,
                        ..
                    } if *dependency_id == runtime_id
                )
            });
            app.history.remove_project(runtime_id);
            let Some(new_runtime_id) = app.workspace.replace_project(index, replacement) else {
                return;
            };
            if was_active {
                app.history.activate(new_runtime_id);
                app.editor.active_layer = active_layer_local_id.and_then(|local_id| {
                    app.workspace.projects[index]
                        .project
                        .document
                        .layers()
                        .iter()
                        .find(|layer| layer.id.0 & LOCAL_MASK == local_id)
                        .map(|layer| layer.id)
                });
            }
            app.invalidate_geometry();
            userspace_log!("Discarded changes to layer '{target_name}'");
        };
        self.spawn_job(
            LAYER_REVERT_JOB_LABEL,
            vec![crate::app::jobs::JobKey::Project { runtime_id, document_revision }],
            compute,
            apply,
        );
        Ok(())
    }

    pub(crate) fn save_and_close_project(&mut self, runtime_id: u32) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.defer_project_close_until_save_finishes(runtime_id) {
            return Ok(());
        }
        self.editor.pending_close_project = Some(runtime_id);
        self.save_project(runtime_id)?;
        #[cfg(not(target_arch = "wasm32"))]
        return self.finish_pending_project_actions();
        #[cfg(target_arch = "wasm32")]
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn delete_browser_project(&mut self, runtime_id: u32) -> Result<()> {
        let index = self
            .workspace
            .project_index_for_runtime_id(runtime_id)
            .context("The selected browser project is no longer open")?;
        if self.browser_saves_pending.contains(&runtime_id) {
            self.browser_delete_after_save.insert(runtime_id);
            self.editor.pending_close_project = None;
            return Ok(());
        }

        let persistence = self.workspace.projects[index].persistence.clone();
        let crate::model::project::ProjectPersistence::BrowserRecord(project_id) = persistence else {
            self.close_project(runtime_id);
            return Ok(());
        };
        self.start_browser_project_delete(project_id, Some(runtime_id))
    }

    #[cfg(target_arch = "wasm32")]
    fn start_browser_project_delete(&mut self, project_id: crate::model::project::ProjectId, runtime_id: Option<u32>) -> Result<()> {
        let proxy = self.web_event_loop_proxy.clone().context("browser event loop is unavailable")?;
        if !self.browser_deletes_pending.insert(project_id) {
            self.editor.pending_close_project = None;
            return Ok(());
        }
        self.editor.pending_close_project = None;
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::app::web_storage::delete_project(project_id).await;
            let _ = proxy.send_event(crate::app::AppEvent::BrowserProjectDeleted { project_id, runtime_id, result });
        });
        Ok(())
    }

    pub(crate) fn close_project(&mut self, runtime_id: u32) {
        #[cfg(target_arch = "wasm32")]
        if !self.browser_project_loads_pending.is_empty() {
            userspace_warn!("Wait for the current project switch to finish");
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.defer_project_close_until_save_finishes(runtime_id) {
            userspace_warn!("The project will close after its current save finishes");
            return;
        }
        let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) else {
            return;
        };
        self.cancel_jobs(|key| {
            matches!(
                key,
                crate::app::jobs::JobKey::Project {
                    runtime_id: dependency_id,
                    ..
                } if *dependency_id == runtime_id
            )
        });
        self.editor.pending_close_project = None;
        let was_active = self.workspace.active_index == Some(index);
        if was_active {
            #[cfg(not(target_arch = "wasm32"))]
            if self.editor.remove_project_after_close {
                if let Some(path) = self.workspace.projects[index].path.as_ref() {
                    self.tracked_project_paths.retain(|tracked| tracked != path);
                }
                self.editor.remove_project_after_close = false;
            }
            #[cfg(target_arch = "wasm32")]
            if self.editor.remove_project_after_close {
                self.editor.remove_project_after_close = false;
                if let Err(error) = self.delete_browser_project(runtime_id) {
                    userspace_warn!("Could not remove browser project: {error:#}");
                }
                return;
            }
            self.clear_project_owned_data();
            self.startup_dialog_dismissed = false;
            self.persist_session();
            userspace_log!("Closed project runtime id {runtime_id}");
            self.invalidate_geometry();
            // The scene the camera was framed on is gone, and the splash is
            // back: reset to the same view the app starts on.
            self.fit_view_to_extents();
            return;
        }
        self.history.remove_project(runtime_id);
        self.workspace.projects.remove(index);
        match self.workspace.active_index {
            Some(_) if was_active => self.workspace.active_index = None,
            Some(active) if active > index => self.workspace.active_index = Some(active - 1),
            _ => {}
        }
        if was_active {
            self.history.deactivate();
            self.clear_editor_transient_state();
            self.startup_dialog_dismissed = false;
        } else {
            self.editor.selected_handles.retain(|handle| match handle {
                crate::model::SceneEntityId::Object(id) => self.workspace.project_index_for_object(*id).is_some(),
                _ => true,
            });
        }
        self.persist_session();
        userspace_log!("Closed project runtime id {runtime_id}");
        self.invalidate_geometry();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pending_text_edit_project_index(&self) -> Option<usize> {
        if !self.editor.text_editing_enabled {
            return None;
        }
        self.editor
            .editing_labels_id
            .and_then(|object_id| self.workspace.project_index_for_object(object_id))
            .or(self.workspace.active_index)
    }

    fn commit_export_move_if_needed(&mut self, project_runtime_id: u32) {
        if self.move_session_original.as_ref().is_some_and(|session| session.project_runtime_id == project_runtime_id) {
            self.commit_pending_move();
        }
    }
}

/// Normalize a browser project name to a portable filename with one `.omf`
/// extension. The viewport prompt rejects blank input, while the fallback
/// also keeps this helper safe for programmatic callers.
#[cfg(target_arch = "wasm32")]
fn sanitize_project_name(name: &str) -> String {
    let mut stem = name.trim();
    while stem.len() >= ".omf".len() {
        let suffix_start = stem.len() - ".omf".len();
        let Some(suffix) = stem.get(suffix_start..) else {
            break;
        };
        if !suffix.eq_ignore_ascii_case(".omf") {
            break;
        }
        stem = stem.get(..suffix_start).unwrap_or_default().trim_end();
    }
    let stem = if stem.is_empty() { "project".to_owned() } else { sanitize_file_stem(stem) };
    format!("{stem}.omf")
}

/// Replace characters that are invalid in filenames across Windows, macOS, and
/// Linux. Falls back to `"layer"` when the result is empty (e.g. a blank name).
fn sanitize_file_stem(name: &str) -> String {
    const INVALID: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
    let cleaned: String = name.trim().chars().map(|c| if INVALID.contains(&c) { '_' } else { c }).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() { "layer".to_string() } else { trimmed.to_string() }
}
