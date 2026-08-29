//! Georeferenced raster import, lifetime and triangulation assignment.

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[cfg(target_arch = "wasm32")]
use crate::model::raster::decode_raster_bytes;
#[cfg(not(target_arch = "wasm32"))]
use crate::model::raster::{decode_raster, is_supported_raster_path};
use crate::{
    app::App,
    model::raster::{OpenRasterTexture, RasterTextureId},
    userspace_log,
};

const MAX_RASTER_PREVIEW_DIMENSION: u32 = 4096;

fn raster_preview_dimension_limit(downscale: bool, hardware_limit: u32) -> u32 {
    if downscale { MAX_RASTER_PREVIEW_DIMENSION.min(hardware_limit) } else { hardware_limit }
}

impl<'a> App<'a> {
    /// Decode raster bytes chosen in the browser into retained project data.
    /// `display_path` is filename-only provenance for this import transaction.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn open_raster_input(&mut self, input: crate::model::input::InputFile, display_path: std::path::PathBuf) {
        let name = input.source.name.clone();
        let hardware_limit = self.graphics.as_ref().map(|graphics| graphics.max_raster_texture_dimension()).unwrap_or(u32::MAX);
        let preview_limit = raster_preview_dimension_limit(self.editor.downscale_raster_previews, hardware_limit);
        self.spawn_job(
            format!("Loading {name}…"),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                decode_raster_bytes(&input.source.name, &input.bytes, preview_limit)
            },
            move |app, result| match result {
                Ok(mut loaded) => {
                    loaded.path = display_path;
                    app.add_loaded_raster(loaded);
                }
                Err(error) => userspace_log!("Failed to load raster {name}: {error:#}"),
            },
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_raster_path(&mut self, path: &Path) -> Result<()> {
        if !is_supported_raster_path(path) {
            anyhow::bail!("Unsupported raster file: {}", path.display());
        }
        if !path.is_file() {
            anyhow::bail!("Raster file does not exist: {}", path.display());
        }
        self.open_raster_path(path.to_path_buf());
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_raster_path(&mut self, path: PathBuf) {
        if self.pending_raster_loads.iter().any(|(_, pending, _, _)| *pending == path) {
            return;
        }

        // Decoding an image is one call into the image crate, so there is
        // nothing to count: the bar reports the file by name and marquees.
        let (ticket, _progress) = self.begin_reported_task(format!("Loading {}", crate::app::file_name(&path)));
        let (tx, rx) = std::sync::mpsc::channel();
        let console_report = crate::logging::retain_current_report();
        let worker_console_report = console_report.as_ref().map(crate::logging::ConsoleReportHandle::child);
        self.pending_raster_loads.push((ticket, path.clone(), rx, console_report));
        let window = self.window.clone();
        let hardware_limit = self.graphics.as_ref().map(|graphics| graphics.max_raster_texture_dimension()).unwrap_or(u32::MAX);
        let preview_limit = raster_preview_dimension_limit(self.editor.downscale_raster_previews, hardware_limit);
        crate::app::jobs::spawn_pool_task(move || {
            let compute = || crate::app::jobs::run_compute_catching_panic(|| decode_raster(&path, preview_limit));
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

    pub(crate) fn poll_raster_loads(&mut self) {
        let receivers = std::mem::take(&mut self.pending_raster_loads);
        let mut still_pending = Vec::new();
        for (ticket, path, receiver, console_report) in receivers {
            let result = receiver.try_recv();
            if matches!(result, Err(std::sync::mpsc::TryRecvError::Empty)) {
                still_pending.push((ticket, path, receiver, console_report));
                continue;
            }
            let complete = || match result {
                Ok(Ok(loaded)) => {
                    self.finish_background_task(ticket, false);
                    self.add_loaded_raster(loaded);
                }
                Ok(Err(error)) => {
                    crate::userspace_warn!("Failed to load raster {}: {error:#}", path.display());
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => unreachable!(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    crate::userspace_warn!("Raster loader disconnected for {}", path.display());
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
        self.pending_raster_loads = still_pending;
    }

    pub(super) fn add_loaded_raster(&mut self, loaded: crate::model::raster::LoadedRasterTexture) {
        let id = RasterTextureId(self.next_raster_texture_id);
        self.next_raster_texture_id += 1;
        let name = crate::model::project::unique_item_name(loaded.name, self.raster_textures.iter().map(|item| item.name.as_str()));
        userspace_log!(
            "Loaded raster {} via {} ({}x{}, preview {}x{})",
            name,
            loaded.driver_name,
            loaded.source_size[0],
            loaded.source_size[1],
            loaded.preview_size[0],
            loaded.preview_size[1]
        );
        self.raster_textures.push(OpenRasterTexture {
            id,
            state: crate::model::project::ProjectItemState::dirty(loaded.path.file_name().map(|name| name.to_string_lossy().into_owned())),
            name,
            visible: true,
            source_size: loaded.source_size,
            preview_size: loaded.preview_size,
            full_rgba: loaded.full_rgba,
            rgba: loaded.rgba,
            world_to_uv: loaded.world_to_uv,
            projection: loaded.projection,
            driver_name: loaded.driver_name,
        });
        self.touch_active_project_content();
        self.redraw_requested = true;
    }

    pub(crate) fn clear_active_triangulation_raster(&mut self) -> Result<()> {
        let triangulation_id = self.active_triangulation.context("Select a triangulation first")?;
        let triangulation = self
            .triangulations
            .iter_mut()
            .find(|triangulation| triangulation.id == triangulation_id)
            .context("The active triangulation is no longer loaded")?;
        if triangulation.raster_texture.take().is_some() {
            triangulation.state.touch();
            self.touch_active_project_content();
        }
        self.redraw_requested = true;
        Ok(())
    }

    pub(crate) fn toggle_raster_visible(&mut self, id: RasterTextureId) {
        let Some(raster) = self.raster_textures.iter_mut().find(|raster| raster.id == id) else {
            return;
        };
        raster.visible = !raster.visible;
        raster.state.touch();
        self.touch_active_project_content();
        self.redraw_requested = true;
    }

    /// Drop runtime use of the raster while retaining its project-owned pixels.
    pub(crate) fn unload_raster(&mut self, id: RasterTextureId) {
        for raster in &mut self.raster_textures {
            if raster.id == id {
                raster.state.loaded = false;
            }
        }
        self.redraw_requested = true;
    }

    pub(crate) fn remove_raster(&mut self, id: RasterTextureId) {
        let mut changed = self.raster_textures.iter().any(|raster| raster.id == id);
        self.raster_textures.retain(|raster| raster.id != id);
        for triangulation in &mut self.triangulations {
            if triangulation.raster_texture == Some(id) {
                triangulation.raster_texture = None;
                triangulation.state.touch();
                changed = true;
            }
        }
        if changed {
            self.touch_active_project_content();
            self.persist_session();
            self.redraw_requested = true;
        }
    }

    /// Drape the raster over every loaded triangulation whose
    /// world-XY extent overlaps the raster footprint. Until draped, a loaded
    /// raster only shows as a flat plan-view image.
    pub(crate) fn drape_raster_over_surfaces(&mut self, id: RasterTextureId) -> Result<()> {
        let raster = self.raster_textures.iter().find(|raster| raster.id == id).context("Load the texture before draping it")?;
        let raster_id = raster.id;
        let raster_name = raster.name.clone();
        let world_to_uv = raster.world_to_uv;
        let mut overlaps_any = false;
        let mut changed = false;
        for triangulation in &mut self.triangulations {
            let bounds = triangulation.mesh.bounds();
            if !raster_overlaps_extent(world_to_uv, [bounds.min.x, bounds.min.y], [bounds.max.x, bounds.max.y]) {
                continue;
            }
            overlaps_any = true;
            if triangulation.raster_texture != Some(raster_id) {
                triangulation.raster_texture = Some(raster_id);
                triangulation.state.touch();
                userspace_log!("Draped raster {} over triangulation {} (overlapping extents)", raster_name, triangulation.name);
                changed = true;
            }
        }
        if !overlaps_any {
            anyhow::bail!("No loaded triangulation overlaps the extents of {raster_name}");
        }
        if changed {
            self.touch_active_project_content();
            self.redraw_requested = true;
        }
        Ok(())
    }

    /// Undrape every raster at once, returning all of them to the flat
    /// plan-view image. Menu-bar counterpart to per-raster [`Self::undrape_raster`].
    pub(crate) fn undrape_all_rasters(&mut self) {
        let mut count = 0usize;
        for triangulation in &mut self.triangulations {
            if triangulation.raster_texture.take().is_some() {
                triangulation.state.touch();
                count += 1;
            }
        }
        if count > 0 {
            userspace_log!("Undraped rasters from {count} triangulation(s)");
            self.touch_active_project_content();
            self.redraw_requested = true;
        }
    }

    /// Remove the raster from every triangulation it is draped
    /// over, returning it to the flat plan-view image.
    pub(crate) fn undrape_raster(&mut self, id: RasterTextureId) {
        let mut changed = false;
        for triangulation in &mut self.triangulations {
            if triangulation.raster_texture == Some(id) {
                triangulation.raster_texture = None;
                triangulation.state.touch();
                changed = true;
            }
        }
        if changed {
            self.touch_active_project_content();
            self.redraw_requested = true;
        }
    }
}

/// Whether a raster's world footprint overlaps the XY extent `[min, max]`.
/// Maps the extent's corners through the affine world-to-UV transform and
/// intersects their UV bounding box with the raster's [0,1]² UV square -
/// conservative for rotated geotransforms, exact for north-up ones.
fn raster_overlaps_extent(world_to_uv: [f64; 6], min: [f64; 2], max: [f64; 2]) -> bool {
    let [a, b, c, d, e, f] = world_to_uv;
    let corners = [[min[0], min[1]], [max[0], min[1]], [min[0], max[1]], [max[0], max[1]]];
    let mut uv_min = [f64::INFINITY; 2];
    let mut uv_max = [f64::NEG_INFINITY; 2];
    for [x, y] in corners {
        let u = a * x + b * y + c;
        let v = d * x + e * y + f;
        uv_min = [uv_min[0].min(u), uv_min[1].min(v)];
        uv_max = [uv_max[0].max(u), uv_max[1].max(v)];
    }
    // NaN bounds (empty mesh) fail these comparisons, so nothing is draped.
    uv_min[0] <= 1.0 && uv_max[0] >= 0.0 && uv_min[1] <= 1.0 && uv_max[1] >= 0.0
}
