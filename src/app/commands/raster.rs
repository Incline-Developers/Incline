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
    i18n::tr_format,
    model::{
        Command, ItemRef,
        raster::{OpenRasterTexture, RasterTextureId},
    },
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
            crate::i18n::tr_format!(literal = "Loading %name%…", name = &name),
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
                Err(error) => userspace_log!(
                    "{}",
                    tr_format!(literal = "Failed to load raster %name%: %error%", name = name, error = format!("{error:#}"))
                ),
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
        let (ticket, _progress) = self.begin_reported_task(crate::i18n::tr_format!(literal = "Loading %name%", name = crate::app::file_name(&path)));
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
                    crate::userspace_warn!(
                        "{}",
                        tr_format!(literal = "Failed to load raster %path%: %error%", path = path.display(), error = format!("{error:#}"))
                    );
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => unreachable!(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    crate::userspace_warn!("{}", tr_format!(literal = "Raster loader disconnected for %path%", path = path.display()));
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
            "{}",
            tr_format!(
                literal = "Loaded raster %name% via %driver% (%srcx%x%srcy%, preview %prevx%x%prevy%)",
                name = name.clone(),
                driver = loaded.driver_name.clone(),
                srcx = loaded.source_size[0],
                srcy = loaded.source_size[1],
                prevx = loaded.preview_size[0],
                prevy = loaded.preview_size[1]
            )
        );
        self.raster_textures.push(OpenRasterTexture {
            id,
            state: crate::model::project::ProjectItemState::dirty(loaded.path.file_name().map(|name| name.to_string_lossy().into_owned())),
            name,
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
        if triangulation.raster_texture.is_some() {
            self.set_triangulation_drapes(&[triangulation_id], None);
        }
        self.redraw_requested = true;
        Ok(())
    }

    /// Set (or clear) the draped raster on several triangulations as one undo
    /// step. Returns how many actually changed.
    fn set_triangulation_drapes(&mut self, triangulations: &[crate::model::triangulation::TriangulationId], raster: Option<RasterTextureId>) -> usize {
        let commands: Vec<Command> = triangulations
            .iter()
            .filter_map(|id| self.item_style_command(ItemRef::Triangulation(*id), |style| style.with_raster_texture(raster)))
            .collect();
        let changed = commands.len();
        if !commands.is_empty() {
            self.execute_edit(Command::Batch(commands));
            self.redraw_requested = true;
        }
        changed
    }

    /// Unload raster pixels while retaining its project entry and drape assignments.
    pub(crate) fn unload_raster(&mut self, id: RasterTextureId) {
        self.set_item_loaded(crate::model::ItemRef::Raster(id), false);
    }

    pub(crate) fn release_raster_runtime(&mut self, _id: RasterTextureId) {
        self.redraw_requested = true;
    }

    pub(crate) fn remove_raster(&mut self, id: RasterTextureId) {
        if !self.raster_textures.iter().any(|raster| raster.id == id) {
            return;
        }
        // Undraping first means undo re-adds the raster and then puts the
        // drapes back onto it, rather than restoring drapes that point at a
        // texture that is not there yet.
        let draped: Vec<_> = self
            .triangulations
            .iter()
            .filter(|triangulation| triangulation.raster_texture == Some(id))
            .map(|triangulation| triangulation.id)
            .collect();
        let mut commands: Vec<Command> = draped
            .iter()
            .filter_map(|triangulation| self.item_style_command(ItemRef::Triangulation(*triangulation), |style| style.with_raster_texture(None)))
            .collect();
        commands.push(Command::DeleteItem {
            item: ItemRef::Raster(id),
            index: 0,
            removed: None,
        });
        self.execute_edit(Command::Batch(commands));
        self.redraw_requested = true;
    }

    /// Drape the raster over every loaded triangulation whose
    /// world-XY extent overlaps the raster footprint. Until draped, a loaded
    /// raster only shows as a flat plan-view image.
    pub(crate) fn drape_raster_over_surfaces(&mut self, id: RasterTextureId) -> Result<()> {
        let raster = self.raster_textures.iter().find(|raster| raster.id == id).context("Load the texture before draping it")?;
        let raster_id = raster.id;
        let raster_name = raster.name.clone();
        let world_to_uv = raster.world_to_uv;
        let overlapping: Vec<_> = self
            .triangulations
            .iter()
            .filter(|triangulation| {
                let bounds = triangulation.mesh.bounds();
                raster_overlaps_extent(world_to_uv, [bounds.min.x, bounds.min.y], [bounds.max.x, bounds.max.y])
            })
            .map(|triangulation| (triangulation.id, triangulation.name.clone()))
            .collect();
        if overlapping.is_empty() {
            anyhow::bail!("{}", tr_format!(literal = "No loaded triangulation overlaps the extents of %name%", name = raster_name));
        }
        for (id, name) in &overlapping {
            if self
                .triangulations
                .iter()
                .any(|triangulation| triangulation.id == *id && triangulation.raster_texture != Some(raster_id))
            {
                userspace_log!(
                    "{}",
                    tr_format!(
                        literal = "Draped raster %raster% over triangulation %triangulation% (overlapping extents)",
                        raster = raster_name.clone(),
                        triangulation = name
                    )
                );
            }
        }
        let ids: Vec<_> = overlapping.into_iter().map(|(id, _)| id).collect();
        self.set_triangulation_drapes(&ids, Some(raster_id));
        Ok(())
    }

    /// Undrape every raster at once, returning all of them to the flat
    /// plan-view image. Menu-bar counterpart to per-raster [`Self::undrape_raster`].
    pub(crate) fn undrape_all_rasters(&mut self) {
        let draped: Vec<_> = self
            .triangulations
            .iter()
            .filter(|triangulation| triangulation.raster_texture.is_some())
            .map(|triangulation| triangulation.id)
            .collect();
        let count = self.set_triangulation_drapes(&draped, None);
        if count > 0 {
            userspace_log!("{}", tr_format!(literal = "Undraped rasters from %count% triangulation(s)", count = count));
        }
    }

    /// Remove the raster from every triangulation it is draped
    /// over, returning it to the flat plan-view image.
    pub(crate) fn undrape_raster(&mut self, id: RasterTextureId) {
        let draped: Vec<_> = self
            .triangulations
            .iter()
            .filter(|triangulation| triangulation.raster_texture == Some(id))
            .map(|triangulation| triangulation.id)
            .collect();
        self.set_triangulation_drapes(&draped, None);
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
