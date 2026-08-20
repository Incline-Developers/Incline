//! Point cloud import, load/unload and explorer commands.

use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

use anyhow::Context;
#[cfg(not(target_arch = "wasm32"))]
use anyhow::Result;

#[cfg(not(target_arch = "wasm32"))]
use crate::app::file_name;
#[cfg(not(target_arch = "wasm32"))]
use crate::model::formats::point_cloud::{PointCloudFormat, read_point_cloud};
use crate::{
    app::App,
    model::point_cloud::{LoadedPointCloud, OpenPointCloud, PointCloudId, finite_bounds, prepare_for_render},
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
        userspace_log!("Loaded point cloud {} ({} points)", loaded.name, loaded.points.len());
        self.point_clouds.push(OpenPointCloud {
            id,
            name: loaded.name,
            path: loaded.path,
            points: loaded.points,
            colors: loaded.colors,
            prepared: loaded.prepared,
            bounds: loaded.bounds,
            visible,
            color,
            point_size,
        });
        self.invalidate_topology_bounds_and_redraw();
    }

    /// Decode point-cloud bytes chosen in the browser, or read back out of
    /// browser storage, under the display name `display_path` already holds.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn open_point_cloud_input(&mut self, input: crate::model::input::InputFile, display_path: std::path::PathBuf) {
        let name = input.source.name.clone();
        if self.point_clouds.iter().any(|cloud| cloud.path == display_path) {
            return;
        }
        self.spawn_job_reporting_progress(
            format!("Loading {name}"),
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
                    name: input.source.name.clone(),
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
                Err(error) => userspace_warn!("Failed to load point cloud: {error:#}"),
            },
        );
    }

    /// Entry point for point-cloud files chosen in the Import menu: register
    /// the path in the session (so it appears in the explorer) and load it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_point_cloud_path(&mut self, path: &Path) -> Result<()> {
        if PointCloudFormat::from_path(path).is_none() {
            anyhow::bail!("Unsupported point cloud file: {}", path.display());
        }
        if !path.is_file() {
            anyhow::bail!("Point cloud file does not exist: {}", path.display());
        }
        if !self.point_cloud_files.contains(&path.to_path_buf()) {
            self.point_cloud_files.push(path.to_path_buf());
        }
        self.persist_session();
        self.open_point_cloud_path(path.to_path_buf());
        Ok(())
    }

    /// Decode a point cloud on a background thread; completion is drained by
    /// `poll_point_cloud_loads`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_point_cloud_path(&mut self, path: PathBuf) {
        if self.point_clouds.iter().any(|cloud| cloud.path == path) {
            return;
        }
        if self.pending_point_cloud_loads.iter().any(|(_, pending, _, _)| *pending == path) {
            return;
        }

        let name = file_name(&path);
        let (ticket, progress) = self.begin_reported_task(format!("Loading {name}"));

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
                    userspace_warn!("Failed to load point cloud: {error:#}");
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => unreachable!(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    userspace_warn!("Point-cloud loader disconnected for {}", path.display());
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

    pub(crate) fn toggle_point_cloud_visible(&mut self, id: PointCloudId) {
        let Some(cloud) = self.point_clouds.iter_mut().find(|cloud| cloud.id == id) else {
            return;
        };
        cloud.visible = !cloud.visible;
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn close_point_cloud(&mut self, id: PointCloudId) {
        self.point_clouds.retain(|cloud| cloud.id != id);
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::PointCloud(id));
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn remove_point_cloud(&mut self, path: &Path) {
        let pending = std::mem::take(&mut self.pending_point_cloud_loads);
        for (ticket, pending_path, receiver, console_report) in pending {
            if pending_path == path {
                self.cancel_background_task(ticket);
                if let Some(report) = console_report {
                    report.cancel();
                }
            } else {
                self.pending_point_cloud_loads.push((ticket, pending_path, receiver, console_report));
            }
        }
        if let Some(removed_id) = self.point_clouds.iter().find(|cloud| cloud.path == path).map(|cloud| cloud.id) {
            self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::PointCloud(removed_id));
        }
        self.point_clouds.retain(|cloud| cloud.path != path);
        self.point_cloud_files.retain(|existing| existing != path);
        self.persist_session();
        self.invalidate_topology_bounds_and_redraw();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reveal_point_cloud(&mut self, id: PointCloudId) -> Result<()> {
        let path = self
            .point_clouds
            .iter()
            .find(|cloud| cloud.id == id)
            .map(|cloud| cloud.path.clone())
            .context("The point cloud is no longer loaded")?;
        self.reveal_in_file_manager(&path)
    }
}
