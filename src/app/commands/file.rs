#[cfg(target_arch = "wasm32")]
use std::collections::HashSet;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context as TaskContext, Poll, Waker},
};
#[cfg(not(target_arch = "wasm32"))]
use std::{process::Command, sync::mpsc};

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

use crate::{
    app::App,
    model::{
        LayerId,
        block_model::BlockModelId,
        formats::{self, MeshFormat},
        pidb::{self, OpenProject},
        triangulation::TriangulationId,
    },
    ui::state::DataMenu,
    userspace_log, userspace_warn,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::{
    app::file_name,
    model::{Layer, Object},
};

#[cfg(not(target_arch = "wasm32"))]
fn is_dxf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dxf"))
}

fn project_has_unsaved_changes(project: &OpenProject) -> bool {
    project.has_unsaved_changes()
}

/// Whether Save still has work to do for a project browser storage has never
/// held a copy of.
///
/// A PIDB opened, imported or restored in the browser starts out identical to
/// the bytes it came from, so "has unsaved changes" is false — but on desktop
/// those bytes are a file that stays put, and in the browser they are nowhere
/// at all until a record exists. Without this, saving an opened PIDB writes
/// nothing and the project is gone on the next reload.
fn project_needs_first_browser_save(project: &OpenProject) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        !matches!(project.persistence, crate::model::pidb::ProjectPersistence::BrowserRecord(_))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = project;
        false
    }
}

/// Write a recovery copy of every dirty project into `recovery_dir`,
/// returning every success and failure. Used during fatal shutdown, when the normal
/// guarded exit flow (confirmation dialogs, Save As) is no longer available.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct RecoveryReport {
    pub(crate) written: Vec<PathBuf>,
    pub(crate) failures: Vec<String>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn write_recovery_copies(workspace: &pidb::Workspace, recovery_dir: &Path) -> Result<RecoveryReport> {
    std::fs::create_dir_all(recovery_dir).with_context(|| format!("create {}", recovery_dir.display()))?;
    let mut written = Vec::new();
    let mut failures = Vec::new();
    for project in &workspace.projects {
        if !project.has_unsaved_changes() {
            continue;
        }
        let stem = project
            .path
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|stem| stem.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| project.pidb.metadata.name.trim_end_matches(".pidb").to_owned());
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let path = recovery_dir.join(format!("{stem}-{timestamp}-{}.recovery.pidb", project.runtime_id));
        match pidb::save(&path, &project.pidb) {
            Ok(()) => written.push(path),
            Err(error) => failures.push(format!("project '{}' -> {}: {error:#}", project.pidb.metadata.name, path.display())),
        }
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
    #[cfg(not(target_arch = "wasm32"))]
    NewPidb(PathBuf),
    #[cfg(not(target_arch = "wasm32"))]
    OpenPidb(Vec<PathBuf>),
    #[cfg(target_arch = "wasm32")]
    WebOpenPidb(Vec<std::result::Result<crate::model::input::InputFile, String>>),
    /// Import DXF files into the active project.
    #[cfg(not(target_arch = "wasm32"))]
    ImportDxfInto { paths: Vec<PathBuf> },
    /// (source_path, pidb_save_path).
    #[cfg(not(target_arch = "wasm32"))]
    ImportAsPidb(Vec<(PathBuf, PathBuf)>),
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
    #[cfg(not(target_arch = "wasm32"))]
    OpenTriangulationFolder(PathBuf),
    /// Export a layer from the project that owned it when the chooser opened.
    #[cfg(not(target_arch = "wasm32"))]
    ExportLayerDxf { project_runtime_id: u32, layer: LayerId, path: PathBuf },
    /// Export the project selected when the chooser opened to DXF.
    #[cfg(not(target_arch = "wasm32"))]
    ExportPidbDxf { project_runtime_id: u32, path: PathBuf },
    /// Export a copy of the project selected when the chooser opened.
    #[cfg(not(target_arch = "wasm32"))]
    ExportPidbCopy { project_runtime_id: u32, path: PathBuf },
    #[cfg(not(target_arch = "wasm32"))]
    ExportOmf { snapshot: formats::omf::ExportSnapshot, path: PathBuf },
    #[cfg(not(target_arch = "wasm32"))]
    ExportTriangulation { id: TriangulationId, path: PathBuf },
    #[cfg(not(target_arch = "wasm32"))]
    ExportBlockModelCsv { id: BlockModelId, path: PathBuf },
    #[cfg(not(target_arch = "wasm32"))]
    SaveBlockModelAs { id: BlockModelId, path: PathBuf, close_after: bool },
    #[cfg(not(target_arch = "wasm32"))]
    SaveTriangulationAs { id: TriangulationId, path: PathBuf },
    #[cfg(not(target_arch = "wasm32"))]
    SaveAndCloseTriangulationAs { id: TriangulationId, path: PathBuf },
    /// Save one open project under a new path.
    #[cfg(not(target_arch = "wasm32"))]
    SavePidbAs { project_runtime_id: u32, path: PathBuf },
    /// Export the main viewport to a PNG image.
    #[cfg(not(target_arch = "wasm32"))]
    ExportViewportImage(PathBuf),
    /// Render and write the configured plot sheet.
    #[cfg(not(target_arch = "wasm32"))]
    ExportPlotSheet(PathBuf),
    #[cfg(target_arch = "wasm32")]
    WebDownloadPidb { project_runtime_id: u32, file_name: String },
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
    /// Save/Save-As: on success update the triangulation's path/name/is_saved
    /// and register the file in the session; optionally close it afterwards.
    Save {
        id: TriangulationId,
        close_after: bool,
    },
    /// Export a copy: leave the open triangulation's metadata unchanged.
    Export {
        name: String,
    },
    /// Save a generated block model as a reloadable CSV source.
    BlockModel {
        id: BlockModelId,
        close_after: bool,
        mapping: crate::model::formats::csv_block_model::CsvColumnMapping,
    },
    /// PIDB snapshot written independently of subsequent in-memory edits.
    Pidb {
        runtime_id: u32,
        snapshot_hash: u64,
        snapshot_layer_hashes: std::collections::HashMap<u64, u64>,
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
        PendingSaveKind::Save { .. } => format!("Saving {name}"),
        PendingSaveKind::Export { .. } => format!("Exporting {name}"),
        PendingSaveKind::BlockModel { .. } => format!("Saving {name}"),
        PendingSaveKind::Pidb { .. } => format!("Saving {name}"),
        PendingSaveKind::DxfExport { .. } => format!("Exporting {name}"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
const PIDB_REVERT_JOB_LABEL: &str = "Reverting PIDB…";
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
            self.editor.pending_close_pidb = None;
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

    pub(super) fn execute_file_dialog_action(&mut self, action: FileDialogAction) -> Result<()> {
        match action {
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::NewPidb(path) => {
                if self.workspace.project_index_for_path(&path).is_some() {
                    anyhow::bail!("That PIDB is already open: {}", path.display());
                }
                self.ensure_save_path_not_pending(&path)?;
                let pidb = pidb::new_empty(Some(path.clone()));
                pidb::save(&path, &pidb)?;
                let project = pidb::open_project(Some(path.clone()), pidb)?;
                self.set_active_project(project);
                userspace_log!("Created new PIDB: {}", path.display());
                self.persist_session();
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::OpenPidb(paths) => {
                let paths: Vec<_> = paths
                    .into_iter()
                    .filter(|path| self.workspace.project_index_for_path(path).is_none() && !self.save_path_is_pending(path))
                    .collect();
                if paths.is_empty() {
                    return Ok(());
                }
                self.pending_pidb_open_paths.extend(paths.iter().cloned());
                let reserved_paths = paths.clone();
                let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| {
                    // One file at a time, so files opened is an exact count.
                    let total = paths.len() as u64;
                    let mut loaded = Vec::with_capacity(paths.len());
                    for (index, path) in paths.into_iter().enumerate() {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        progress.set_items(index as u64, total);
                        let result = pidb::load(&path).map_err(|error| format!("{error:#}"));
                        loaded.push((path, result));
                    }
                    Ok(loaded)
                };
                let apply = move |app: &mut App, result: Result<Vec<(PathBuf, std::result::Result<pidb::PidbFile, String>)>>| {
                    for path in &reserved_paths {
                        app.pending_pidb_open_paths.remove(path);
                    }
                    let Ok(loaded) = result else {
                        return;
                    };
                    let mut count = 0usize;
                    for (path, result) in loaded {
                        match result.and_then(|pidb| pidb::open_project(Some(path.clone()), pidb).map_err(|error| format!("{error:#}"))) {
                            Ok(project) => {
                                if app.workspace.project_index_for_path(&path).is_none() {
                                    app.workspace.add_inactive(project);
                                    count += 1;
                                }
                            }
                            Err(error) => {
                                userspace_warn!("Could not open {}: {error}", path.display());
                            }
                        }
                    }
                    if count > 0 {
                        app.invalidate_geometry();
                        app.persist_session();
                    }
                    userspace_log!("Added {count} PIDB file(s)");
                };
                self.spawn_job_reporting_progress("Opening PIDB files…", vec![crate::app::jobs::JobKey::Anonymous], compute, apply);
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            FileDialogAction::WebOpenPidb(files) => {
                let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| {
                    let total = files.len() as u64;
                    let mut parsed = Vec::with_capacity(files.len());
                    for (index, file) in files.into_iter().enumerate() {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        progress.set_items(index as u64, total);
                        match file {
                            Ok(file) => {
                                let name = file.source.name.clone();
                                parsed.push((name.clone(), pidb::load_bytes(&name, &file.bytes)));
                            }
                            Err(error) => parsed.push(("selected PIDB".to_owned(), Err(anyhow::anyhow!(error)))),
                        }
                    }
                    Ok(parsed)
                };
                let apply = |app: &mut App, result: Result<Vec<(String, Result<crate::model::pidb::PidbFile>)>>| {
                    let parsed = match result {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            userspace_warn!("Could not open browser PIDBs: {error:#}");
                            return;
                        }
                    };
                    let mut count = 0usize;
                    for (name, pidb_file) in parsed {
                        match pidb_file.and_then(|pidb| pidb::open_imported_project(name.clone(), pidb)) {
                            Ok(project) => {
                                app.workspace.add_inactive(project);
                                count += 1;
                            }
                            Err(error) => userspace_warn!("Could not open {name}: {error:#}"),
                        }
                    }
                    if count > 0 {
                        app.invalidate_geometry();
                    }
                    userspace_log!("Added {count} browser PIDB file(s)");
                };
                self.spawn_job_reporting_progress("Opening browser PIDB files…", vec![crate::app::jobs::JobKey::Anonymous], compute, apply);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ImportDxfInto { paths } => {
                let project = self.workspace.active_project().context("No active .pidb to import into")?;
                let runtime_id = project.runtime_id;
                let document_revision = project.pidb.document.revision();
                let import_count = paths.len();
                let compute = move |cancel: &crate::app::jobs::CancelFlag| {
                    let mut parsed = Vec::with_capacity(paths.len());
                    for path in paths {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        let document = pidb::parse_dxf_document(&path)?;
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
                    let existing: std::collections::HashSet<LayerId> = project.pidb.document.layers().iter().map(|layer| layer.id).collect();
                    let mut total_added = 0usize;
                    for (path, imported) in parsed {
                        let added = pidb::merge_document(&mut project.pidb.document, &imported);
                        userspace_log!("Imported {added} object(s) from {}", path.display());
                        total_added += added;
                    }
                    let new_ids: Vec<LayerId> = project
                        .pidb
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
                    if total_added > 0
                        && let Err(error) = app.save_project(runtime_id)
                    {
                        userspace_warn!("Imported geometry is dirty but could not be saved: {error:#}");
                    }
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
            FileDialogAction::ImportAsPidb(pairs) => {
                let count = pairs.len();
                for (source_path, pidb_path) in &pairs {
                    if self.workspace.project_index_for_path(pidb_path).is_some() {
                        anyhow::bail!("That PIDB is already open: {}", pidb_path.display());
                    }
                    self.ensure_save_path_not_pending(pidb_path)?;
                    let mut pidb_data = pidb::pidb_from_dxf_path(source_path)?;
                    pidb_data.metadata.name = pidb_path.file_name().and_then(|n| n.to_str()).unwrap_or("Imported.pidb").to_string();
                    pidb::save(pidb_path, &pidb_data)?;
                    let project = pidb::open_project(Some(pidb_path.clone()), pidb_data)?;
                    self.set_active_project(project);
                    let layer_ids: Vec<LayerId> = self
                        .workspace
                        .active_project()
                        .into_iter()
                        .flat_map(|project| project.pidb.document.layers())
                        .map(|layer| layer.id)
                        .collect();
                    if let Some(project) = self.workspace.active_project_mut() {
                        project.loaded_layers.extend(layer_ids.iter().copied());
                    }
                    self.editor.active_layer = layer_ids.first().copied();
                    self.invalidate_geometry();
                    self.fit_view_to_extents();
                }
                userspace_log!("Imported {count} file(s) as .pidb");
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ImportTriangulation(paths) => {
                let count = paths.len();
                for path in &paths {
                    if !self.triangulation_files.contains(path) {
                        self.triangulation_files.push(path.clone());
                        self.open_triangulation_path(path)?;
                    } else {
                        self.open_triangulation_path(path)?;
                    }
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
            FileDialogAction::OpenTriangulationFolder(dir) => {
                if !dir.is_dir() {
                    return Ok(());
                }
                let entries = Self::scan_triangulation_dir(&dir);
                if entries.is_empty() {
                    return Ok(());
                }
                let total_files = entries.len();
                let mut added_dirs = 0usize;
                if !entries.is_empty() {
                    if !self.triangulation_dirs.contains(&dir) {
                        self.triangulation_dirs.push(dir.clone());
                        added_dirs += 1;
                    }
                    self.triangulation_dir_entries.insert(dir.clone(), entries);
                }
                self.triangulation_dirs.sort();
                self.triangulation_dirs.dedup();
                self.persist_session();
                userspace_log!("Added triangulation folder: {} ({} files across {added_dirs} folders)", dir.display(), total_files);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportLayerDxf { project_runtime_id, layer, path } => {
                self.commit_export_move_if_needed(project_runtime_id);
                self.ensure_save_path_not_pending(&path)?;
                let project_index = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .context("The PIDB selected for export is no longer open")?;
                let project = &self.workspace.projects[project_index];
                self.spawn_dxf_write(project.pidb.clone(), Some(layer), path, format!("layer {:?}", layer));
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportPidbDxf { project_runtime_id, path } => {
                self.commit_export_move_if_needed(project_runtime_id);
                self.ensure_save_path_not_pending(&path)?;
                let project_index = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .context("The PIDB selected for export is no longer open")?;
                let project = &self.workspace.projects[project_index];
                self.spawn_dxf_write(project.pidb.clone(), None, path, "PIDB".to_owned());
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportPidbCopy { project_runtime_id, path } => {
                self.commit_export_move_if_needed(project_runtime_id);
                self.ensure_save_path_not_pending(&path)?;
                let project_index = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .context("The PIDB selected for export is no longer open")?;
                let project = &self.workspace.projects[project_index];
                if self.workspace.project_index_for_path(&path).is_some() {
                    anyhow::bail!("Choose a path not used by an open PIDB");
                }
                let mut exported = project.pidb.clone();
                exported.metadata.name = file_name(&path);
                pidb::save(&path, &exported)?;
                userspace_log!("Exported PIDB copy to {}", path.display());
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportOmf { snapshot, path } => {
                self.export_omf_snapshot(snapshot, path);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::ExportTriangulation { id, path } => {
                MeshFormat::from_path(&path).context("Choose a filename ending in .obj, .stl, or .ply")?;
                if self.triangulations.iter().any(|other| other.id != id && other.path == path) || self.pending_triangulation_loads.iter().any(|(_, p, _, _)| p == &path) {
                    anyhow::bail!("Another loaded triangulation already uses {}", path.display());
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
            FileDialogAction::SaveBlockModelAs { id, path, close_after } => {
                self.commit_block_model_save(id, path, close_after)?;
                if close_after {
                    self.editor.block_model_close_unsaved = None;
                }
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::SaveTriangulationAs { id, path } => {
                self.commit_triangulation_save(id, path, false)?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::SaveAndCloseTriangulationAs { id, path } => {
                // The close happens in poll_saves once the background save
                // succeeds; dismiss the unsaved-close prompt immediately.
                self.commit_triangulation_save(id, path, true)?;
                self.editor.tri_close_unsaved = None;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            FileDialogAction::SavePidbAs { project_runtime_id, path } => {
                if self.project_revert_is_pending(project_runtime_id) {
                    anyhow::bail!("Wait for the PIDB revert to finish before saving");
                }
                let project_index = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .context("The selected .pidb is no longer open")?;
                self.ensure_project_has_no_pending_text_edit(project_index)?;
                self.ensure_pidb_save_path_available(project_index, &path)?;
                let project = &mut self.workspace.projects[project_index];
                let previous_name = std::mem::replace(&mut project.pidb.metadata.name, file_name(&path));
                let snapshot = project.pidb.clone();
                let snapshot_hash = project.current_content_hash();
                let snapshot_layer_hashes = project.current_layer_hashes();
                self.spawn_pidb_write(project_runtime_id, snapshot_hash, snapshot_layer_hashes, snapshot, path, Some(previous_name));
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
            FileDialogAction::WebDownloadPidb { project_runtime_id, file_name } => {
                self.commit_export_move_if_needed(project_runtime_id);
                let project = self
                    .workspace
                    .project_index_for_runtime_id(project_runtime_id)
                    .and_then(|index| self.workspace.projects.get(index))
                    .context("The PIDB selected for export is no longer open")?;
                let snapshot = project.pidb.clone();
                self.spawn_job(
                    "Encoding PIDB download…",
                    vec![crate::app::jobs::JobKey::Anonymous],
                    move |cancel| {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        pidb::to_bytes(&snapshot)
                    },
                    move |_app, result| match result {
                        Ok(bytes) => Self::trigger_browser_download(file_name, bytes, "application/octet-stream", "PIDB"),
                        Err(error) => userspace_warn!("PIDB download encoding failed: {error:#}"),
                    },
                );
                Ok(())
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
                    .context("The PIDB selected for export is no longer open")?;
                let snapshot = project.pidb.clone();
                self.spawn_job(
                    "Encoding DXF download…",
                    vec![crate::app::jobs::JobKey::Anonymous],
                    move |cancel| {
                        if cancel.is_cancelled() {
                            anyhow::bail!("Cancelled");
                        }
                        pidb::export_to_dxf_bytes(&snapshot, layer)
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

    #[cfg(not(target_arch = "wasm32"))]
    /// Start a background save of the mesh for `id` to `path`. Metadata and
    /// session updates happen in `poll_saves` once the worker finishes. Shared
    /// by SaveTriangulationAs and SaveAndCloseTriangulationAs actions.
    #[cfg(not(target_arch = "wasm32"))]
    fn commit_triangulation_save(&mut self, id: TriangulationId, path: PathBuf, close_after: bool) -> Result<()> {
        if self.triangulations.iter().any(|other| other.id != id && other.path == path) || self.pending_triangulation_loads.iter().any(|(_, p, _, _)| p == &path) {
            anyhow::bail!("Another loaded triangulation already uses {}", path.display());
        }
        if self
            .pending_saves
            .iter()
            .any(|save| save.path == path || matches!(save.kind, PendingSaveKind::Save { id: pending, .. } if pending == id))
        {
            anyhow::bail!("A save involving {} is already in progress", path.display());
        }
        MeshFormat::from_path(&path).context("Choose a filename ending in .obj, .stl, or .ply")?;
        let triangulation = self.triangulations.iter().find(|t| t.id == id).context("The triangulation is no longer loaded")?;
        let mesh = std::sync::Arc::clone(&triangulation.mesh);
        userspace_log!("Saving triangulation '{}' to {}", triangulation.name, path.display());
        self.spawn_triangulation_write(PendingSaveKind::Save { id, close_after }, mesh, path);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn commit_block_model_save(&mut self, id: BlockModelId, mut path: PathBuf, close_after: bool) -> Result<()> {
        if path.extension().is_none() {
            path.set_extension("csv");
        }
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
        {
            anyhow::bail!("Choose a filename ending in .csv");
        }
        if self.block_models.iter().any(|other| other.id != id && other.source.path == path)
            || self.block_model_files.iter().any(|source| source.path == path)
            || self.pending_block_model_loads.iter().any(|(_, source, _, _)| source.path == path)
        {
            anyhow::bail!("Another block model already uses {}", path.display());
        }
        if self
            .pending_saves
            .iter()
            .any(|save| save.path == path || matches!(save.kind, PendingSaveKind::BlockModel { id: pending, .. } if pending == id))
        {
            anyhow::bail!("A save involving {} is already in progress", path.display());
        }
        let block_model = self
            .block_models
            .iter()
            .find(|model| model.id == id)
            .context("The selected block model is no longer loaded")?;
        let model = block_model.model.clone();
        let blocks = std::sync::Arc::clone(&block_model.blocks);
        let renderable = std::sync::Arc::clone(&block_model.renderable_block_indices);
        let mapping = crate::model::formats::csv_block_model::export_mapping(&model);
        userspace_log!("Saving block model '{}' to {}", block_model.name, path.display());
        self.spawn_block_model_write(PendingSaveKind::BlockModel { id, close_after, mapping }, model, blocks, renderable, path);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_block_model_write(
        &mut self,
        kind: PendingSaveKind,
        model: crate::model::formats::block_model_data::BlockModelData,
        blocks: std::sync::Arc<crate::model::block_model::BlockBoundsSource>,
        renderable: std::sync::Arc<crate::model::block_model::RenderableBlockIndices>,
        path: PathBuf,
    ) {
        let (ticket, _progress) = self.begin_reported_task(format!("Saving {}", file_name(&path)));
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
                crate::model::atomic_file::write_atomic(&path, |file| {
                    crate::model::formats::csv_block_model::write(&model, &blocks, &renderable, file).map_err(anyhow::Error::new)
                })
            });
            let _ = result_tx.send(result);
            if let Some(window) = window.as_ref() {
                window.request_redraw();
            }
        });
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
        let mut finish_pidb_actions = false;

        for mut save in pending {
            let deferred_pidb_close = match &save.kind {
                PendingSaveKind::Pidb {
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
                            PendingSaveKind::Save { id, close_after } => {
                                if let Some(tri) = self.triangulations.iter_mut().find(|t| t.id == id) {
                                    let saved_name = tri.name.clone();
                                    tri.path = save.path.clone();
                                    tri.name = file_name(&save.path);
                                    tri.is_saved = true;
                                    if !self.triangulation_files.contains(&save.path) {
                                        self.triangulation_files.push(save.path.clone());
                                    }
                                    self.triangulation_excluded_paths.remove(&save.path);
                                    userspace_log!("Saved triangulation '{}' to {}", saved_name, save.path.display());
                                    self.persist_session();
                                    if close_after {
                                        self.close_triangulation(id);
                                    }
                                }
                            }
                            PendingSaveKind::Export { name } => {
                                userspace_log!("Exported triangulation '{}' to {}", name, save.path.display());
                            }
                            PendingSaveKind::BlockModel { id, close_after, mapping } => {
                                let saved = self.block_models.iter_mut().find(|model| model.id == id).map(|model| {
                                    let previous_name = model.name.clone();
                                    model.name = file_name(&save.path);
                                    model.source = crate::model::block_model::BlockModelSource {
                                        path: save.path.clone(),
                                        csv_columns: Some(mapping),
                                        generated: false,
                                    };
                                    (previous_name, model.source.clone())
                                });
                                if let Some((previous_name, source)) = saved {
                                    if !self.block_model_files.contains(&source) {
                                        self.block_model_files.push(source);
                                    }
                                    userspace_log!("Saved block model '{}' to {}", previous_name, save.path.display());
                                    self.persist_session();
                                    if close_after {
                                        self.close_block_model(id);
                                    }
                                }
                            }
                            PendingSaveKind::Pidb {
                                runtime_id,
                                snapshot_hash,
                                snapshot_layer_hashes,
                                close_after,
                                save_as_previous_name,
                            } => {
                                if let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) {
                                    self.workspace.projects[index].mark_snapshot_saved(snapshot_hash, snapshot_layer_hashes);
                                    if save_as_previous_name.is_some() {
                                        self.workspace.projects[index].path = Some(save.path.clone());
                                        userspace_log!("Saved PIDB as: {}", save.path.display());
                                    } else {
                                        userspace_log!("Saved PIDB: {}", save.path.display());
                                    }
                                    self.persist_session();
                                    if close_after {
                                        self.editor.pending_close_pidb = Some(runtime_id);
                                    }
                                    finish_pidb_actions = true;
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
                        if let PendingSaveKind::Pidb {
                            runtime_id,
                            save_as_previous_name: Some(previous_name),
                            ..
                        } = &save.kind
                            && let Some(index) = self.workspace.project_index_for_runtime_id(*runtime_id)
                            && self.workspace.projects[index].pidb.metadata.name == file_name(&save.path)
                        {
                            self.workspace.projects[index].pidb.metadata.name = previous_name.clone();
                        }
                        let message = format!("{e:#}");
                        userspace_warn!("Save failed: {message}");
                        if let Some(runtime_id) = deferred_pidb_close
                            && self.workspace.project_index_for_runtime_id(runtime_id).is_some()
                        {
                            self.editor.pending_close_pidb = Some(runtime_id);
                            finish_pidb_actions = true;
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
                Err(mpsc::TryRecvError::Empty) => still_pending.push(save),
                Err(mpsc::TryRecvError::Disconnected) => {
                    let console_report = save.console_report.take();
                    let mut complete = || {
                        self.finish_background_task(save.ticket, false);
                        self.redraw_requested = true;
                        userspace_warn!("Save worker ended without a result");
                        if let Some(runtime_id) = deferred_pidb_close
                            && self.workspace.project_index_for_runtime_id(runtime_id).is_some()
                        {
                            self.editor.pending_close_pidb = Some(runtime_id);
                            finish_pidb_actions = true;
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
        if finish_pidb_actions && let Err(error) = self.finish_pending_pidb_actions() {
            userspace_warn!("Could not finish the pending PIDB action: {error:#}");
        }
        self.try_finish_deferred_exit();
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn poll_saves(&mut self) {}

    // ── Dialog spawners (non-blocking) ──────────────────────────────────────

    pub(crate) fn choose_new_pidb(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            self.editor.new_pidb_name.clear();
            self.editor.new_pidb_dialog_open = true;
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async {
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("ProInspector database", &["pidb"])
                .set_file_name("new_project.pidb")
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::NewPidb(path))
        });
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn create_browser_pidb(&mut self, name: String) -> Result<()> {
        let mut pidb_file = pidb::new_empty(None);
        pidb_file.metadata.name = sanitize_pidb_name(&name);
        let project = pidb::open_project(None, pidb_file)?;
        self.set_active_project(project);
        userspace_log!("Created new browser PIDB");
        // Browser projects are persisted only after an explicit Save action.
        // Keeping the new project dirty also feeds the before-unload guard.
        Ok(())
    }

    pub(crate) fn choose_open_pidb(&mut self) {
        #[cfg(target_arch = "wasm32")]
        self.spawn_file_dialog(async {
            let handles = AsyncFileDialog::new().add_filter("ProInspector database", &["pidb"]).pick_files().await?;
            let mut files = Vec::with_capacity(handles.len());
            for handle in handles {
                files.push(crate::model::input::read_browser_handle(handle).await);
            }
            Some(FileDialogAction::WebOpenPidb(files))
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async {
            let paths = AsyncFileDialog::new()
                .add_filter("ProInspector database", &["pidb"])
                .pick_files()
                .await?
                .into_iter()
                .map(FileHandleExt::into_path)
                .collect();
            Some(FileDialogAction::OpenPidb(paths))
        });
    }

    pub(crate) fn import_dxf_paths_into(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = paths;
            let files = self.take_web_import_files(DataMenu::Dxf)?;
            let project = self.workspace.active_project().context("No active .pidb to import into")?;
            let runtime_id = project.runtime_id;
            let document_revision = project.pidb.document.revision();
            let compute = move |cancel: &crate::app::jobs::CancelFlag| {
                let mut parsed = Vec::with_capacity(files.len());
                for file in files {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    parsed.push((file.source.name.clone(), pidb::parse_dxf_document_bytes(&file.source.name, &file.bytes)?));
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
                let existing: std::collections::HashSet<_> = project.pidb.document.layers().iter().map(|layer| layer.id).collect();
                let mut total = 0usize;
                for (name, document) in parsed {
                    let added = pidb::merge_document(&mut project.pidb.document, &document);
                    total += added;
                    userspace_log!("Imported {added} object(s) from {name}");
                }
                let new_layers: Vec<_> = project
                    .pidb
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
    pub(crate) fn open_web_pidb_sources(&mut self) -> Result<()> {
        let files = self.take_web_import_files(DataMenu::Pidb)?;
        self.execute_file_dialog_action(FileDialogAction::WebOpenPidb(files.into_iter().map(Ok).collect()))
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
            let path = self.persist_browser_import(crate::app::commands::browser_data::BrowserDataKind::PointCloud, &file);
            self.open_point_cloud_input(file, path);
        }
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_raster_sources(&mut self) -> Result<()> {
        let files = self.take_web_import_files(DataMenu::Geotiff)?;
        for file in files {
            let path = self.persist_browser_import(crate::app::commands::browser_data::BrowserDataKind::Raster, &file);
            self.open_raster_input(file, path);
        }
        Ok(())
    }

    /// Reserve an import's explorer name and copy its bytes into browser
    /// storage, so the entry outlives both an unload and the tab.
    #[cfg(target_arch = "wasm32")]
    fn persist_browser_import(&mut self, kind: crate::app::commands::browser_data::BrowserDataKind, file: &crate::model::input::InputFile) -> PathBuf {
        let path = self.allocate_browser_source_path(&file.source.name);
        self.store_browser_data(kind, &file.source.name, &path, &file.bytes);
        path
    }

    pub(crate) fn choose_import_source_files(&mut self, kind: DataMenu) {
        #[cfg(target_arch = "wasm32")]
        self.spawn_file_dialog(async move {
            let dialog = match kind {
                DataMenu::Dxf => AsyncFileDialog::new().add_filter("AutoCAD DXF", &["dxf"]),
                DataMenu::Omf => AsyncFileDialog::new().add_filter("Open Mining Format", &["omf"]),
                DataMenu::Pidb => AsyncFileDialog::new().add_filter("ProInspector database", &["pidb"]),
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
                DataMenu::Pidb => AsyncFileDialog::new().add_filter("ProInspector database", &["pidb"]),
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

    pub(crate) fn choose_import_as_pidb_paths(&mut self, kind: DataMenu, source_paths: Vec<PathBuf>) {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = source_paths;
            let result = (|| -> Result<()> {
                let files = self.take_web_import_files(kind)?;
                let mut imported = 0usize;
                for file in &files {
                    let mut pidb_data = pidb::pidb_from_dxf_bytes(&file.source.name, &file.bytes)?;
                    let stem = Path::new(&file.source.name).file_stem().and_then(|stem| stem.to_str()).unwrap_or("Imported");
                    pidb_data.metadata.name = format!("{stem}.pidb");
                    let project = pidb::open_project(None, pidb_data)?;
                    self.set_active_project(project);
                    if let Some(project) = self.workspace.active_project_mut() {
                        project.loaded_layers.extend(project.pidb.document.layers().iter().map(|layer| layer.id));
                    }
                    imported += 1;
                }
                self.invalidate_geometry();
                self.fit_view_to_extents();
                userspace_log!("Imported {imported} browser file(s) as PIDB projects");
                Ok(())
            })();
            if let Err(error) = result {
                userspace_warn!("Import failed: {error:#}");
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let mut pairs = Vec::new();
            for source_path in source_paths {
                if kind != DataMenu::Dxf || !is_dxf_path(&source_path) {
                    continue;
                }
                let stem = source_path.file_stem().and_then(|s| s.to_str()).unwrap_or("imported").to_owned();
                let default_name = format!("{stem}.pidb");
                let Some(pidb_path) = AsyncFileDialog::new()
                    .add_filter("ProInspector database", &["pidb"])
                    .set_file_name(&default_name)
                    .save_file()
                    .await
                else {
                    continue;
                };
                pairs.push((source_path, pidb_path.into_path()));
            }
            if pairs.is_empty() { None } else { Some(FileDialogAction::ImportAsPidb(pairs)) }
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
        let path = self.allocate_browser_source_path(&file.source.name);
        let metadata_json = serde_json::to_string(&mapping).context("Could not preserve the CSV column mapping")?;
        self.store_browser_data_with_metadata(
            crate::app::commands::browser_data::BrowserDataKind::BlockModel,
            &file.source.name,
            &path,
            &file.bytes,
            Some(metadata_json),
        );
        self.open_block_model_input(
            file,
            crate::model::block_model::BlockModelSource {
                path,
                csv_columns: Some(mapping),
                generated: false,
            },
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn choose_open_triangulation_folder(&mut self) {
        self.spawn_file_dialog(async {
            let dir = AsyncFileDialog::new().pick_folder().await?.into_path();
            Some(FileDialogAction::OpenTriangulationFolder(dir))
        });
    }

    pub(crate) fn choose_export_pidb_dxf(&mut self, project_runtime_id: u32) {
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
            Some(FileDialogAction::ExportPidbDxf { project_runtime_id, path })
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

    pub(crate) fn spawn_save_block_model_dialog(&mut self, id: BlockModelId, close_after: bool) {
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
            close_after,
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let path = AsyncFileDialog::new()
                .add_filter("CSV block model", &["csv"])
                .set_file_name(&default_name)
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::SaveBlockModelAs { id, path, close_after })
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

    pub(crate) fn choose_export_pidb_copy(&mut self, project_runtime_id: u32) {
        self.commit_export_move_if_needed(project_runtime_id);
        let Some(project_index) = self.workspace.project_index_for_runtime_id(project_runtime_id) else {
            return;
        };
        let project = &self.workspace.projects[project_index];
        let default_name = project
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(|name| name.to_owned())
            .unwrap_or_else(|| {
                let stem = Path::new(&project.pidb.metadata.name).file_stem().and_then(|name| name.to_str()).unwrap_or("project");
                format!("{}.pidb", sanitize_file_stem(stem))
            });
        #[cfg(target_arch = "wasm32")]
        self.start_browser_export(FileDialogAction::WebDownloadPidb {
            project_runtime_id,
            file_name: default_name,
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("ProInspector database", &["pidb"])
                .set_file_name(&default_name)
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::ExportPidbCopy { project_runtime_id, path })
        });
    }

    /// Download a portable `.pidb` copy of every open browser project. Each
    /// project stays as an individual PIDB instead of being hidden inside an
    /// archive, so any one of the downloads can be reopened directly.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn download_all_pidbs(&mut self) -> Result<()> {
        if self.workspace.projects.is_empty() {
            return Ok(());
        }
        if self.has_pending_move_delta() {
            self.commit_pending_move();
        }
        if let Some(active_index) = self.workspace.active_index {
            self.ensure_project_has_no_pending_text_edit(active_index)?;
        }

        let file_names = unique_pidb_download_names(self.workspace.projects.iter().map(|project| project.pidb.metadata.name.as_str()));
        let snapshots: Vec<_> = self
            .workspace
            .projects
            .iter()
            .zip(file_names)
            .map(|(project, file_name)| (file_name, project.pidb.clone()))
            .collect();

        self.spawn_job(
            "Encoding PIDB downloads…",
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel| {
                let mut encoded = Vec::with_capacity(snapshots.len());
                for (file_name, snapshot) in snapshots {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    encoded.push((file_name, pidb::to_bytes(&snapshot)?));
                }
                Ok(encoded)
            },
            move |_app, result| match result {
                Ok(downloads) => {
                    for (file_name, bytes) in downloads {
                        Self::trigger_browser_download(file_name, bytes, "application/octet-stream", "PIDB");
                    }
                }
                Err(error) => userspace_warn!("PIDB download encoding failed: {error:#}"),
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
        let layer_name = project.pidb.document.layer(layer).map(|layer| layer.name.clone()).unwrap_or_else(|| "layer".to_string());
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
            .and_then(|t| t.path.file_stem().and_then(|s| s.to_str()))
            .unwrap_or("triangulation")
            .to_owned();
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

    /// Desktop-only: the browser keeps generated triangulations in IndexedDB,
    /// so it has nothing to "save as" — the equivalent action there is
    /// downloading a copy, which needs no path from the user.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn spawn_save_triangulation_dialog(&mut self, id: TriangulationId) {
        let stem = self
            .triangulations
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| Path::new(&t.name).file_stem().and_then(|s| s.to_str()))
            .unwrap_or("triangulation")
            .to_owned();
        self.spawn_file_dialog(async move {
            let default_name = format!("{stem}.obj");
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("Wavefront OBJ", &["obj"])
                .add_filter("STL", &["stl"])
                .add_filter("PLY", &["ply"])
                .set_file_name(&default_name)
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::SaveTriangulationAs { id, path })
        });
    }

    pub(crate) fn spawn_save_and_close_triangulation_dialog(&mut self, id: TriangulationId) {
        let stem = self
            .triangulations
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| Path::new(&t.name).file_stem().and_then(|s| s.to_str()))
            .unwrap_or("triangulation")
            .to_owned();
        #[cfg(target_arch = "wasm32")]
        self.start_browser_export(FileDialogAction::WebDownloadTriangulation {
            id,
            format: MeshFormat::Obj,
            file_name: format!("{stem}.obj"),
            close_after: true,
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let default_name = format!("{stem}.obj");
            let path: PathBuf = AsyncFileDialog::new()
                .add_filter("Wavefront OBJ", &["obj"])
                .add_filter("STL", &["stl"])
                .add_filter("PLY", &["ply"])
                .set_file_name(&default_name)
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::SaveAndCloseTriangulationAs { id, path })
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn spawn_save_pidb_as_dialog(&mut self, project_runtime_id: u32) {
        if self.project_revert_is_pending(project_runtime_id) {
            userspace_warn!("Wait for the PIDB revert to finish before saving");
            return;
        }
        if self.pidb_save_is_pending(project_runtime_id) {
            userspace_warn!("Wait for the current PIDB save to finish");
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
                .add_filter("ProInspector database", &["pidb"])
                .set_file_name("project.pidb")
                .save_file()
                .await?
                .into_path();
            Some(FileDialogAction::SavePidbAs { project_runtime_id, path })
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

    // ── Save helpers used in exit / internal chains ────────────────────────

    /// Open Save As for an unsaved triangulation. Returns false because the
    /// dialog completes later through `poll_file_dialogs`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn save_triangulation_as(&mut self, id: TriangulationId) -> Result<bool> {
        self.triangulations.iter().find(|t| t.id == id).context("The triangulation is no longer loaded")?;
        self.spawn_save_triangulation_dialog(id);
        Ok(false)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reveal_pidb(&mut self, path: &Path) -> Result<()> {
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("explorer");
            command.arg(format!("/select,{}", path.display()));
            command
        };
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut command = Command::new("open");
            command.arg("-R").arg(path);
            command
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut command = {
            let mut command = Command::new("xdg-open");
            command.arg(path.parent().unwrap_or_else(|| Path::new(".")));
            command.env("LANG", "C.UTF-8").env("LC_ALL", "C.UTF-8");
            command
        };

        command.spawn().context("Could not open the system file explorer")?;
        userspace_log!("Revealed PIDB in file explorer: {}", path.display());
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reveal_triangulation(&mut self, id: TriangulationId) -> Result<()> {
        let triangulation = self
            .triangulations
            .iter()
            .find(|triangulation| triangulation.id == id && triangulation.is_saved)
            .context("The triangulation has not been saved")?;
        let path = &triangulation.path;

        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("explorer");
            command.arg(format!("/select,{}", path.display()));
            command
        };
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut command = Command::new("open");
            command.arg("-R").arg(path);
            command
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut command = {
            let mut command = Command::new("xdg-open");
            command.arg(path.parent().unwrap_or_else(|| Path::new(".")));
            command.env("LANG", "C.UTF-8").env("LC_ALL", "C.UTF-8");
            command
        };

        command.spawn().context("Could not open the system file explorer")?;
        userspace_log!("Revealed triangulation '{}' in file explorer", triangulation.name);
        Ok(())
    }

    /// Open the platform file manager with `path` selected (or its parent
    /// directory shown, where selection isn't supported).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reveal_in_file_manager(&self, path: &Path) -> Result<()> {
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("explorer");
            command.arg(format!("/select,{}", path.display()));
            command
        };
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut command = Command::new("open");
            command.arg("-R").arg(path);
            command
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut command = {
            let mut command = Command::new("xdg-open");
            command.arg(path.parent().unwrap_or_else(|| Path::new(".")));
            command.env("LANG", "C.UTF-8").env("LC_ALL", "C.UTF-8");
            command
        };

        command.spawn().context("Could not open the system file explorer")?;
        userspace_log!("Revealed '{}' in file explorer", file_name(path));
        Ok(())
    }

    /// Open the platform file manager showing `directory` itself (unlike
    /// [`Self::reveal_in_file_manager`], which shows the parent with the
    /// entry selected).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_directory_in_file_manager(&self, directory: &Path) -> Result<()> {
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("explorer");
            command.arg(directory);
            command
        };
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut command = Command::new("open");
            command.arg(directory);
            command
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut command = {
            let mut command = Command::new("xdg-open");
            command.arg(directory);
            command.env("LANG", "C.UTF-8").env("LC_ALL", "C.UTF-8");
            command
        };

        command.spawn().context("Could not open the system file explorer")?;
        userspace_log!("Opened '{}' in file explorer", directory.display());
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reveal_block_model(&mut self, id: BlockModelId) -> Result<()> {
        let block_model = self.block_models.iter().find(|model| model.id == id).context("The block model is no longer loaded")?;
        let path = &block_model.source.path;

        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("explorer");
            command.arg(format!("/select,{}", path.display()));
            command
        };
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut command = Command::new("open");
            command.arg("-R").arg(path);
            command
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut command = {
            let mut command = Command::new("xdg-open");
            command.arg(path.parent().unwrap_or_else(|| Path::new(".")));
            command.env("LANG", "C.UTF-8").env("LC_ALL", "C.UTF-8");
            command
        };

        command.spawn().context("Could not open the system file explorer")?;
        userspace_log!("Revealed block model in file explorer: {}", path.display());
        Ok(())
    }

    /// Save every PIDB whose stored copy is missing or out of date. Returns
    /// false when Save As is still pending for a never-saved project.
    pub(crate) fn save_all_dirty_projects(&mut self) -> Result<bool> {
        if self.has_pending_move_delta() {
            self.commit_pending_move();
        }
        let dirty_ids: Vec<u32> = self
            .workspace
            .projects
            .iter()
            .filter(|project| project.has_unsaved_changes() || project_needs_first_browser_save(project))
            .map(|project| project.runtime_id)
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
                // completion re-enters the deferred-save flow, which then
                // prompts the next project; never stack multiple dialogs.
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
            Ok(recovery_dir) => match write_recovery_copies(&self.workspace, &recovery_dir) {
                Ok(report) if report.written.is_empty() && report.failures.is_empty() => {
                    log::error!("No unsaved projects; nothing to recover");
                }
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
        let browser_has_open_pidbs = cfg!(target_arch = "wasm32") && !self.workspace.projects.is_empty();
        if browser_has_open_pidbs || self.has_unsaved_changes_for_exit() {
            self.editor.exit_confirm_open = true;
            userspace_log!("User requested exit (PIDB export or unsaved-work confirmation required)");
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

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn save_and_exit(&mut self) -> Result<()> {
        self.exit_after_pending_saves = true;
        self.discard_changes_on_deferred_exit = false;
        if self.save_all_dirty_projects()? && self.save_all_unsaved_triangulations()? && self.save_all_unsaved_block_models()? {
            self.finish_deferred_exit();
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_all_unsaved_triangulations(&mut self) -> Result<bool> {
        let unsaved: Vec<TriangulationId> = self.triangulations.iter().filter(|tri| !tri.is_saved).map(|tri| tri.id).collect();
        for id in unsaved {
            if !self.save_triangulation_as(id)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Browser triangulations go to IndexedDB as they are generated, so exit
    /// never has to chase a save for them.
    #[cfg(target_arch = "wasm32")]
    fn save_all_unsaved_triangulations(&mut self) -> Result<bool> {
        Ok(true)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_all_unsaved_block_models(&mut self) -> Result<bool> {
        if let Some(id) = self.block_models.iter().find(|model| model.source.generated).map(|model| model.id) {
            self.spawn_save_block_model_dialog(id, false);
            return Ok(false);
        }
        Ok(true)
    }

    #[cfg(target_arch = "wasm32")]
    fn save_all_unsaved_block_models(&mut self) -> Result<bool> {
        Ok(true)
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

    fn has_unsaved_changes_for_exit(&self) -> bool {
        // A browser triangulation that is still writing to IndexedDB reads as
        // unsaved, but the write needs no user decision, so it must not hold
        // the exit confirmation open.
        let unsaved_triangulations = cfg!(not(target_arch = "wasm32")) && self.triangulations.iter().any(|tri| !tri.is_saved);
        let unsaved_block_models = self.block_models.iter().any(|model| model.source.generated);
        self.workspace.projects.iter().any(project_has_unsaved_changes) || unsaved_triangulations || unsaved_block_models || self.editor.text_editing_enabled
    }

    fn try_finish_deferred_exit(&mut self) {
        if !self.exit_after_pending_saves || !self.pending_file_dialogs.is_empty() || !self.pending_saves.is_empty() {
            return;
        }
        if self.discard_changes_on_deferred_exit {
            self.finish_deferred_exit();
            return;
        }
        match self
            .save_all_dirty_projects()
            .and_then(|pidbs_done| if pidbs_done { self.save_all_unsaved_triangulations() } else { Ok(false) })
            .and_then(|triangulations_done| if triangulations_done { self.save_all_unsaved_block_models() } else { Ok(false) })
        {
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
            anyhow::bail!("Wait for the PIDB revert to finish before saving");
        }
        let index = self.workspace.project_index_for_runtime_id(runtime_id).context("The selected .pidb is no longer open")?;
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
                self.spawn_save_pidb_as_dialog(runtime_id);
                return Ok(());
            };
            if self
                .pending_saves
                .iter()
                .any(|save| matches!(save.kind, PendingSaveKind::Pidb { runtime_id: pending, .. } if pending == runtime_id) || save.path == path)
            {
                return Ok(());
            }
            let project = &self.workspace.projects[index];
            let snapshot = project.pidb.clone();
            let snapshot_hash = project.current_content_hash();
            let snapshot_layer_hashes = project.current_layer_hashes();
            self.spawn_pidb_write(runtime_id, snapshot_hash, snapshot_layer_hashes, snapshot, path, None);
            Ok(())
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn save_browser_project(&mut self, runtime_id: u32) -> Result<()> {
        let index = self.workspace.project_index_for_runtime_id(runtime_id).context("The selected .pidb is no longer open")?;
        // Once deletion has been confirmed, it owns this project's storage
        // lifecycle. A concurrent Save must not recreate the IndexedDB record
        // after the delete transaction completes.
        if self.browser_deletes_pending.contains(&runtime_id) || self.browser_delete_after_save.contains(&runtime_id) {
            return Ok(());
        }
        if self.browser_saves_pending.contains(&runtime_id) {
            return Ok(());
        }
        self.ensure_project_has_no_pending_text_edit(index)?;
        let project = &mut self.workspace.projects[index];
        let project_id = match project.persistence {
            crate::model::pidb::ProjectPersistence::BrowserRecord(id) => id,
            _ => uuid::Uuid::new_v4(),
        };
        // Browser storage is a serialization boundary exactly like a `.pidb`
        // on disk, so the ids have to leave the runtime namespace on the way
        // out. The hashes below are namespace-invariant either way.
        let snapshot = pidb::portable_pidb(&project.pidb);
        let snapshot_hash = project.current_content_hash();
        let snapshot_layer_hashes = project.current_layer_hashes();
        let record = crate::app::web_storage::BrowserProjectRecord {
            id: project_id,
            name: snapshot.metadata.name.clone(),
            pidb: snapshot,
            saved_at_ms: js_sys::Date::now() as u64,
        };
        let proxy = self.web_event_loop_proxy.clone().context("browser event loop is unavailable")?;
        self.browser_saves_pending.insert(runtime_id);
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::app::web_storage::put_project(&record).await;
            let _ = proxy.send_event(crate::app::AppEvent::BrowserProjectSaved {
                runtime_id,
                project_id,
                snapshot_hash,
                snapshot_layer_hashes,
                result,
            });
        });
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_pidb_write(
        &mut self,
        runtime_id: u32,
        snapshot_hash: u64,
        snapshot_layer_hashes: std::collections::HashMap<u64, u64>,
        snapshot: pidb::PidbFile,
        path: PathBuf,
        save_as_previous_name: Option<String>,
    ) {
        let kind = PendingSaveKind::Pidb {
            runtime_id,
            snapshot_hash,
            snapshot_layer_hashes,
            close_after: false,
            save_as_previous_name,
        };
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
                pidb::save_with_progress(&path, &snapshot, &progress).with_context(|| format!("Failed to save PIDB {}", path.display()))
            });
            let _ = result_tx.send(result);
            if let Some(window) = window {
                window.request_redraw();
            }
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_dxf_write(&mut self, snapshot: pidb::PidbFile, layer: Option<LayerId>, path: PathBuf, description: String) {
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
                Some(layer) => pidb::export_layer_to_dxf(&snapshot, layer, &path),
                None => pidb::export_to_dxf(&snapshot, &path),
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
            anyhow::bail!("Apply or discard the current text edit before saving this PIDB");
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ensure_pidb_save_path_available(&self, project_index: usize, path: &Path) -> Result<()> {
        let runtime_id = self.workspace.projects[project_index].runtime_id;
        if self.pidb_save_is_pending(runtime_id) {
            anyhow::bail!("Wait for the current PIDB save to finish");
        }
        self.ensure_save_path_not_pending(path)?;
        if self
            .workspace
            .projects
            .iter()
            .enumerate()
            .any(|(index, project)| index != project_index && project.path.as_deref() == Some(path))
        {
            anyhow::bail!("Another open PIDB already uses {}", path.display());
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_path_is_pending(&self, path: &Path) -> bool {
        self.pending_saves.iter().any(|save| save.path == path) || self.pending_pidb_open_paths.contains(path)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ensure_save_path_not_pending(&self, path: &Path) -> Result<()> {
        if self.save_path_is_pending(path) {
            anyhow::bail!("A file operation involving {} is already in progress", path.display());
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pidb_save_is_pending(&self, runtime_id: u32) -> bool {
        self.pending_saves.iter().any(|save| {
            matches!(
                save.kind,
                PendingSaveKind::Pidb {
                    runtime_id: pending,
                    ..
                } if pending == runtime_id
            )
        })
    }

    /// A running file write cannot be cancelled safely. Remember the close
    /// request on that write and finish it only after the result is known.
    #[cfg(not(target_arch = "wasm32"))]
    fn defer_pidb_close_until_save_finishes(&mut self, runtime_id: u32) -> bool {
        let mut deferred = false;
        for save in &mut self.pending_saves {
            if let PendingSaveKind::Pidb {
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
            self.editor.pending_close_pidb = None;
        }
        deferred
    }

    pub(crate) fn activate_pidb(&mut self, runtime_id: u32) -> Result<()> {
        let index = self.workspace.project_index_for_runtime_id(runtime_id).context("The selected .pidb is no longer open")?;
        self.activate_project_index(index);
        userspace_log!("Activated PIDB runtime id {runtime_id}");
        Ok(())
    }

    pub(crate) fn request_close_pidb(&mut self, runtime_id: u32) {
        let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        {
            let _ = index;
            // Deletion is destructive even for a clean project, so the web UI
            // always asks for confirmation.
            self.editor.pending_close_pidb = Some(runtime_id);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.defer_pidb_close_until_save_finishes(runtime_id) {
            userspace_log!("PIDB will close after its current save finishes");
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.workspace.projects[index].has_unsaved_changes() || self.pending_text_edit_project_index() == Some(index) {
            self.editor.pending_close_pidb = Some(runtime_id);
        } else {
            self.close_pidb(runtime_id);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn finish_pending_pidb_actions(&mut self) -> Result<()> {
        let Some(runtime_id) = self.editor.pending_close_pidb else {
            return Ok(());
        };
        if self.pidb_save_is_pending(runtime_id) {
            return Ok(());
        }
        if self
            .workspace
            .project_index_for_runtime_id(runtime_id)
            .and_then(|index| self.workspace.projects.get(index))
            .is_some_and(OpenProject::has_unsaved_changes)
            || self
                .workspace
                .project_index_for_runtime_id(runtime_id)
                .is_some_and(|index| self.pending_text_edit_project_index() == Some(index))
        {
            return Ok(());
        }
        #[cfg(target_arch = "wasm32")]
        let result = self.delete_browser_project(runtime_id);
        #[cfg(not(target_arch = "wasm32"))]
        let result = {
            self.close_pidb(runtime_id);
            Ok(())
        };
        result
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn request_discard_pidb_changes(&mut self, runtime_id: u32) {
        let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) else {
            return;
        };
        if self.pending_saves.iter().any(|save| {
            matches!(
                save.kind,
                PendingSaveKind::Pidb {
                    runtime_id: pending,
                    ..
                } if pending == runtime_id
            )
        }) {
            userspace_warn!("Wait for the PIDB save to finish before discarding changes");
            return;
        }
        let project = &self.workspace.projects[index];
        if project.path.is_some() && project.has_unsaved_changes() {
            self.editor.pending_discard_pidb = Some(runtime_id);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn project_revert_is_pending(&self, runtime_id: u32) -> bool {
        self.pending_jobs.iter().any(|job| {
            (job.label == PIDB_REVERT_JOB_LABEL || job.label == LAYER_REVERT_JOB_LABEL)
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

    /// Revert a PIDB to its last saved state by re-parsing it from disk. The
    /// reload happens on a job thread; the swap preserves the project's slot,
    /// active status, and loaded-layer set (matched by stable local layer id).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn discard_pidb_changes(&mut self, runtime_id: u32) -> Result<()> {
        self.editor.pending_discard_pidb = None;
        let index = self.workspace.project_index_for_runtime_id(runtime_id).context("The selected .pidb is no longer open")?;
        if self.pending_saves.iter().any(|save| {
            matches!(
                save.kind,
                PendingSaveKind::Pidb {
                    runtime_id: pending,
                    ..
                } if pending == runtime_id
            )
        }) {
            anyhow::bail!("Wait for the PIDB save to finish before discarding changes");
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
            .context("This PIDB has never been saved, so there is nothing to revert to")?;
        // Keyed to the current revision: if the user keeps editing while the
        // file re-parses, the stale revert is dropped instead of clobbering.
        let document_revision = self.workspace.projects[index].pidb.document.revision();

        let compute = move |cancel: &crate::app::jobs::CancelFlag| {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let pidb = pidb::load(&path)?;
            Ok((path, pidb))
        };
        let apply = move |app: &mut App, result: Result<(PathBuf, pidb::PidbFile)>| {
            let (path, pidb) = match result {
                Ok(loaded) => loaded,
                Err(error) => {
                    userspace_warn!("Could not reload PIDB from disk: {error:#}");
                    return;
                }
            };
            let Some(index) = app.workspace.project_index_for_runtime_id(runtime_id) else {
                return;
            };
            let replacement = match pidb::open_project(Some(path.clone()), pidb) {
                Ok(project) => project,
                Err(error) => {
                    userspace_warn!("Could not reload {}: {error:#}", path.display());
                    return;
                }
            };
            let was_active = app.workspace.active_index == Some(index);
            // Project-backed drafts must be resolved while the old namespace
            // still exists; doing this after the swap can target unrelated
            // objects that happen to reuse a local id.
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
            }
            // Old-namespace object ids no longer resolve; drop them from the
            // selection instead of leaving dangling handles.
            app.editor.selected_handles.retain(|handle| match handle {
                crate::model::SceneEntityId::Object(id) => app.workspace.project_index_for_object(*id).is_some(),
                _ => true,
            });
            app.invalidate_geometry();
            userspace_log!("Discarded changes: reloaded {}", path.display());
        };
        self.spawn_job(
            PIDB_REVERT_JOB_LABEL,
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
                PendingSaveKind::Pidb {
                    runtime_id: pending,
                    ..
                } if pending == project.runtime_id
            )
        }) || self.project_revert_is_pending(project.runtime_id)
        {
            userspace_warn!("Wait for the PIDB operation to finish before discarding changes");
            return;
        }
        if let Some(layer) = project.pidb.document.layer(layer_id) {
            self.editor.pending_discard_layer = Some((layer_id, layer.name.clone()));
        }
    }

    /// Restore one layer from disk while carrying every other dirty layer into
    /// the freshly parsed project. Rebuilding from the saved PIDB also handles
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
                PendingSaveKind::Pidb {
                    runtime_id: pending,
                    ..
                } if pending == runtime_id
            )
        }) || self.project_revert_is_pending(runtime_id)
        {
            anyhow::bail!("Wait for the PIDB operation to finish before discarding changes");
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
        let path = project.path.clone().context("This PIDB has never been saved, so its layers cannot be restored")?;
        let target_name = project.pidb.document.layer(layer_id).map(|layer| layer.name.clone()).unwrap_or_else(|| "layer".to_owned());
        let document_revision = project.pidb.document.revision();

        // Convert the current document back to portable ids, then retain only
        // snapshots for other dirty layers. The target is deliberately omitted
        // so the copy loaded from disk wins.
        let preserved_local_ids = project
            .dirty_layer_ids()
            .into_iter()
            .map(|dirty| dirty.0 & LOCAL_MASK)
            .filter(|local| *local != target_local_id)
            .collect::<std::collections::HashSet<_>>();
        let mut portable_current = project.pidb.clone();
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
            let pidb = pidb::load(&path)?;
            Ok((path, pidb))
        };
        let apply = move |app: &mut App, result: Result<(PathBuf, pidb::PidbFile)>| {
            let (path, pidb) = match result {
                Ok(loaded) => loaded,
                Err(error) => {
                    userspace_warn!("Could not reload layer from disk: {error:#}");
                    return;
                }
            };
            let Some(index) = app.workspace.project_index_for_runtime_id(runtime_id) else {
                return;
            };
            if app.workspace.projects[index].pidb.document.revision() != document_revision {
                userspace_warn!("Layer discard was cancelled because the project changed while the PIDB was reloading");
                return;
            }
            let mut replacement = match pidb::open_project(Some(path), pidb) {
                Ok(project) => project,
                Err(error) => {
                    userspace_warn!("Could not restore layer from PIDB: {error:#}");
                    return;
                }
            };
            for (layer_index, layer, objects) in preserved_layers {
                replacement.pidb.document.replace_layer_snapshot(layer_index, layer, objects);
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
                        .pidb
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

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn save_and_close_pidb(&mut self, runtime_id: u32) -> Result<()> {
        if self.defer_pidb_close_until_save_finishes(runtime_id) {
            return Ok(());
        }
        self.editor.pending_close_pidb = Some(runtime_id);
        self.save_project(runtime_id)?;
        self.finish_pending_pidb_actions()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn delete_browser_project(&mut self, runtime_id: u32) -> Result<()> {
        let index = self
            .workspace
            .project_index_for_runtime_id(runtime_id)
            .context("The selected browser project is no longer open")?;
        if self.browser_deletes_pending.contains(&runtime_id) {
            self.editor.pending_close_pidb = None;
            return Ok(());
        }
        if self.browser_saves_pending.contains(&runtime_id) {
            self.browser_delete_after_save.insert(runtime_id);
            self.editor.pending_close_pidb = None;
            return Ok(());
        }

        let persistence = self.workspace.projects[index].persistence.clone();
        let crate::model::pidb::ProjectPersistence::BrowserRecord(project_id) = persistence else {
            self.close_pidb(runtime_id);
            return Ok(());
        };
        let proxy = self.web_event_loop_proxy.clone().context("browser event loop is unavailable")?;
        self.browser_deletes_pending.insert(runtime_id);
        self.editor.pending_close_pidb = None;
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::app::web_storage::delete_project(project_id).await;
            let _ = proxy.send_event(crate::app::AppEvent::BrowserProjectDeleted { runtime_id, result });
        });
        Ok(())
    }

    pub(crate) fn close_pidb(&mut self, runtime_id: u32) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.defer_pidb_close_until_save_finishes(runtime_id) {
            userspace_warn!("The PIDB will close after its current save finishes");
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
        self.editor.pending_close_pidb = None;
        let was_active = self.workspace.active_index == Some(index);
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
        userspace_log!("Closed PIDB runtime id {runtime_id}");
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

/// Normalize a browser project name to a portable filename with one `.pidb`
/// extension. The viewport prompt rejects blank input, while the fallback
/// also keeps this helper safe for programmatic callers.
#[cfg(target_arch = "wasm32")]
fn sanitize_pidb_name(name: &str) -> String {
    let mut stem = name.trim();
    while stem.len() >= ".pidb".len() {
        let suffix_start = stem.len() - ".pidb".len();
        let Some(suffix) = stem.get(suffix_start..) else {
            break;
        };
        if !suffix.eq_ignore_ascii_case(".pidb") {
            break;
        }
        stem = stem.get(..suffix_start).unwrap_or_default().trim_end();
    }
    let stem = if stem.is_empty() { "project".to_owned() } else { sanitize_file_stem(stem) };
    format!("{stem}.pidb")
}

/// Produce portable, case-insensitively unique names for a browser batch.
/// Downloads often land on case-insensitive filesystems, and two open PIDBs
/// are allowed to have the same metadata name.
#[cfg(target_arch = "wasm32")]
fn unique_pidb_download_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut used = HashSet::new();
    names
        .into_iter()
        .map(|name| {
            let base = sanitize_pidb_name(name);
            if used.insert(base.to_ascii_lowercase()) {
                return base;
            }

            let stem = base.strip_suffix(".pidb").unwrap_or("project");
            for copy in 2_u32.. {
                let candidate = format!("{stem} ({copy}).pidb");
                if used.insert(candidate.to_ascii_lowercase()) {
                    return candidate;
                }
            }
            unreachable!("the PIDB download suffix counter is unbounded")
        })
        .collect()
}

/// Replace characters that are invalid in filenames across Windows, macOS, and
/// Linux. Falls back to `"layer"` when the result is empty (e.g. a blank name).
fn sanitize_file_stem(name: &str) -> String {
    const INVALID: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
    let cleaned: String = name.trim().chars().map(|c| if INVALID.contains(&c) { '_' } else { c }).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() { "layer".to_string() } else { trimmed.to_string() }
}
