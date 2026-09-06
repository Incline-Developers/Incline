//! Point cloud import, load/unload and explorer commands.

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

use anyhow::Context;
#[cfg(not(target_arch = "wasm32"))]
use anyhow::Result;

#[cfg(not(target_arch = "wasm32"))]
use crate::app::file_name;
#[cfg(not(target_arch = "wasm32"))]
use crate::model::formats::point_cloud::{PointCloudFormat, read_point_cloud};
use crate::{
    app::App,
    i18n::tr_format,
    model::{
        SceneEntityId,
        point_cloud::{LoadedPointCloud, OpenPointCloud, PointCloudId, finite_bounds, prepare_for_render},
    },
    userspace_log, userspace_warn,
};

/// Uniform colour for clouds without per-point colours.
const DEFAULT_POINT_CLOUD_COLOR: [f32; 4] = [0.85, 0.87, 0.9, 1.0];
/// Screen-facing splat width in source/world metres.
const DEFAULT_POINT_SIZE: f32 = 0.1;
/// Share of a point-cloud load spent decoding the file, before the render
/// buffers are built from the decoded points. Decoding reports exactly within
/// its share; the preparation that follows is a single pass.
const DECODE_SHARE: f32 = 0.8;

impl<'a> App<'a> {
    pub(super) fn add_loaded_point_cloud(&mut self, loaded: LoadedPointCloud, visible: bool, color: [f32; 4], point_size: f32) {
        let id = PointCloudId(self.next_point_cloud_id);
        self.next_point_cloud_id += 1;
        let name = crate::model::project::unique_item_name(loaded.name, self.point_clouds.iter().map(|item| item.name.as_str()));
        userspace_log!(
            "{}",
            tr_format!(literal = "Loaded point cloud %name% (%count% points)", name = name.clone(), count = loaded.points.len())
        );
        self.point_clouds.push(OpenPointCloud {
            id,
            state: crate::model::project::ProjectItemState::dirty(loaded.path.file_name().map(|name| name.to_string_lossy().into_owned())).with_loaded(visible),
            name,
            points: loaded.points,
            colors: loaded.colors,
            prepared: loaded.prepared,
            bounds: loaded.bounds,
            color,
            point_size,
        });
        self.touch_active_project_content();
        self.invalidate_topology_bounds_and_redraw();
    }

    /// Decode point-cloud bytes chosen in the browser into retained project
    /// data. `display_path` contains a filename only and is used for
    /// provenance during the import transaction.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn open_point_cloud_input(&mut self, input: crate::model::input::InputFile, display_path: std::path::PathBuf) {
        let source_name = input.source.name.clone();
        let name = crate::model::project::imported_item_name(std::path::Path::new(&source_name), &crate::i18n::tr!(literal = "Point cloud"));
        self.spawn_job_reporting_progress(
            crate::i18n::tr_format!(literal = "Loading %name%", name = &source_name),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                let data = crate::model::formats::point_cloud::read_point_cloud_bytes(&input.source.name, &input.bytes, &progress.phase(0.0, DECODE_SHARE))?;
                if data.points.is_empty() {
                    anyhow::bail!("Point cloud {} contains no points", input.source.name);
                }
                let (min, max) = data
                    .bounds
                    .or_else(|| finite_bounds(&data.points))
                    .with_context(|| format!("Point cloud {} contains no finite points", input.source.name))?;
                let prepared = prepare_for_render(&data.points, data.colors.as_deref(), (min, max));
                let colors = data.colors.map(std::sync::Arc::new);
                progress.set_fraction(1.0);
                Ok(LoadedPointCloud {
                    name,
                    path: display_path,
                    points: std::sync::Arc::new(data.points),
                    colors,
                    prepared: std::sync::Arc::new(prepared),
                    bounds: (min, max),
                })
            },
            move |app, result| match result {
                Ok(loaded) => {
                    let should_fit = !app.scene_has_renderables();
                    app.add_loaded_point_cloud(loaded, true, DEFAULT_POINT_CLOUD_COLOR, DEFAULT_POINT_SIZE);
                    if should_fit {
                        app.fit_view_to_extents();
                    }
                    app.invalidate_topology_bounds_and_redraw();
                }
                Err(error) => userspace_warn!("{}", tr_format!(literal = "Failed to load point cloud: %error%", error = format!("{error:#}"))),
            },
        );
    }

    /// Entry point for point-cloud files chosen in the Import menu.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_point_cloud_path(&mut self, path: &Path) -> Result<()> {
        if PointCloudFormat::from_path(path).is_none() {
            anyhow::bail!("Unsupported point cloud file: {}", path.display());
        }
        if !path.is_file() {
            anyhow::bail!("Point cloud file does not exist: {}", path.display());
        }
        self.open_point_cloud_path(path.to_path_buf());
        Ok(())
    }

    /// Decode a point cloud on a background thread; completion is drained by
    /// `poll_point_cloud_loads`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_point_cloud_path(&mut self, path: PathBuf) {
        if self.pending_point_cloud_loads.iter().any(|(_, pending, _, _)| *pending == path) {
            return;
        }

        let source_name = file_name(&path);
        let name = crate::model::project::imported_item_name(&path, &crate::i18n::tr!(literal = "Point cloud"));
        let (ticket, progress) = self.begin_reported_task(crate::i18n::tr_format!(literal = "Loading %name%", name = &source_name));

        let (tx, rx) = std::sync::mpsc::channel();
        let console_report = crate::logging::retain_current_report();
        let worker_console_report = console_report.as_ref().map(crate::logging::ConsoleReportHandle::child);
        self.pending_point_cloud_loads.push((ticket, path.clone(), rx, console_report));
        let window = self.window.clone();
        crate::app::jobs::spawn_pool_task(move || {
            let compute = || {
                crate::app::jobs::run_compute_catching_panic(|| -> Result<LoadedPointCloud> {
                    let data = read_point_cloud(&path, &progress.phase(0.0, DECODE_SHARE)).with_context(|| format!("Failed to read point cloud {}", path.display()))?;
                    if data.points.is_empty() {
                        anyhow::bail!("Point cloud {} contains no points", path.display());
                    }
                    let (min, max) = data
                        .bounds
                        .or_else(|| finite_bounds(&data.points))
                        .with_context(|| format!("Point cloud {} contains no finite points", path.display()))?;
                    let prepared = prepare_for_render(&data.points, data.colors.as_deref(), (min, max));
                    let colors = data.colors.map(std::sync::Arc::new);
                    progress.set_fraction(1.0);
                    Ok(LoadedPointCloud {
                        name,
                        path,
                        points: std::sync::Arc::new(data.points),
                        colors,
                        prepared: std::sync::Arc::new(prepared),
                        bounds: (min, max),
                    })
                })
            };
            let result = if let Some(report) = worker_console_report.as_ref() {
                report.scope(compute)
            } else {
                compute()
            };
            let _ = tx.send(result);
            if let Some(window) = window {
                window.request_redraw();
            }
        });
    }

    pub(crate) fn poll_point_cloud_loads(&mut self) {
        let receivers = std::mem::take(&mut self.pending_point_cloud_loads);
        let mut still_pending = Vec::new();
        for (ticket, path, rx, console_report) in receivers {
            let result = rx.try_recv();
            if matches!(result, Err(std::sync::mpsc::TryRecvError::Empty)) {
                still_pending.push((ticket, path, rx, console_report));
                continue;
            }
            let complete = || match result {
                Ok(Ok(loaded)) => {
                    let should_fit = !self.scene_has_renderables();
                    self.add_loaded_point_cloud(loaded, true, DEFAULT_POINT_CLOUD_COLOR, DEFAULT_POINT_SIZE);
                    if should_fit {
                        self.fit_view_to_extents();
                    }
                    self.finish_background_task(ticket, true);
                    // Clouds render from point_cloud_gpu's per-id cache, not
                    // the document scene, so only bounds/redraw are stale.
                    self.invalidate_topology_bounds_and_redraw();
                }
                Ok(Err(error)) => {
                    userspace_warn!("{}", tr_format!(literal = "Failed to load point cloud: %error%", error = format!("{error:#}")));
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => unreachable!(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    userspace_warn!("{}", tr_format!(literal = "Point-cloud loader disconnected for %path%", path = path.display()));
                    self.finish_background_task(ticket, false);
                }
            };
            if let Some(report) = console_report.as_ref() {
                report.scope(complete);
            } else {
                complete();
            }
            drop(console_report);
        }
        self.pending_point_cloud_loads = still_pending;
    }

    pub(crate) fn close_point_cloud(&mut self, id: PointCloudId) {
        self.set_item_loaded(crate::model::ItemRef::PointCloud(id), false);
    }

    pub(crate) fn release_pointcloud_runtime(&mut self, id: PointCloudId) {
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::PointCloud(id));
        let entity = SceneEntityId::PointCloud(id);
        self.editor.selected_handles.remove(&entity);
        self.editor.hidden_handles.remove(&entity);
        self.editor.translucent_handles.remove(&entity);
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn remove_point_cloud(&mut self, id: PointCloudId) {
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::PointCloud(id));
        let entity = SceneEntityId::PointCloud(id);
        self.editor.selected_handles.remove(&entity);
        self.editor.hidden_handles.remove(&entity);
        self.editor.explicitly_frozen.remove(&entity);
        self.editor.frozen_handles.remove(&entity);
        self.editor.translucent_handles.remove(&entity);
        self.delete_project_item(crate::model::ItemRef::PointCloud(id));
    }
}
