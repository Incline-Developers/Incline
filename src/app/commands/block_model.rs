use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use glam::DVec3;

#[cfg(not(target_arch = "wasm32"))]
use crate::app::file_name;
use crate::{
    app::App,
    i18n::tr_format,
    model::{
        Command, ItemRef, ItemStyle, SceneEntityId,
        block_model::{
            BlockBounds, BlockBoundsSource, BlockModelId, BlockModelSource, ColorTransferFunction, LoadedBlockModel, OpenBlockModel, RegularBlockBounds, RenderableBlockIndices,
            compute_world_bounds, is_no_data_sentinel, numeric_variable_default,
        },
        drill_hole::{DrillFieldKind, DrillHoleDataset, DrillHoleId, DrillValue},
        formats::{
            block_model_data::{BlockModelColumn, BlockModelData},
            mesh_data,
        },
        kriging::{KrigingGrid, KrigingSample, OrdinaryKrigingOptions, ordinary_kriging},
    },
    ui::state::{OreFilterMode, TriSurfaceType},
    userspace_log, userspace_warn,
};

const DEFAULT_BLOCK_MODEL_COLOR: [f32; 4] = [0.2, 0.65, 0.95, 1.0];

impl<'a> App<'a> {
    fn clear_block_model_entity_state(&mut self, handle: SceneEntityId) {
        self.editor.selected_handles.remove(&handle);
        self.editor.hidden_handles.remove(&handle);
        self.editor.frozen_handles.remove(&handle);
        self.editor.translucent_handles.remove(&handle);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_block_model_source(&mut self, source: BlockModelSource) -> Result<()> {
        if source.csv_columns.is_none() {
            anyhow::bail!("Only CSV block-model sources are supported");
        }
        if !source.path.is_file() {
            anyhow::bail!("Block model source does not exist: {}", source.path.display());
        }
        userspace_log!("{}", tr_format!(literal = "Imported block model source %path%", path = source.path.display()));
        self.open_block_model_source(source)
    }

    /// Decode block-model bytes chosen in the browser, or read back out of
    /// browser storage, under the display path already held by `source`.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn open_block_model_input(&mut self, input: crate::model::input::InputFile, source: BlockModelSource) -> Result<()> {
        let source_name = input.source.name.clone();
        let model_name = crate::model::project::imported_item_name(std::path::Path::new(&source_name), &crate::i18n::tr!(literal = "Block model"));
        self.spawn_job(
            tr_format!(literal = "Loading %name%…", name = &source_name),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                let crate::model::input::InputFile { bytes, reservation, .. } = input;
                // Ownership of the bytes moves into the CSV parser, whose
                // retained allocation reservation replaces the input reservation.
                drop(reservation);
                loaded_block_model_from_bytes(bytes, model_name, source)
            },
            |app, result| match result {
                Ok(loaded) => app.add_loaded_block_model(loaded),
                Err(error) => userspace_warn!("{}", tr_format!(literal = "Failed to load block model: %error%", error = format!("{error:#}"))),
            },
        );
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_block_model_source(&mut self, source: BlockModelSource) -> Result<()> {
        if self.pending_block_model_loads.iter().any(|(_, pending_source, _, _)| pending_source.path == source.path) {
            return Ok(());
        }

        let source_name = file_name(&source.path);
        let name = crate::model::project::imported_item_name(&source.path, &crate::i18n::tr!(literal = "Block model"));
        let (ticket, progress) = self.begin_reported_task(tr_format!(literal = "Loading %name%", name = &source_name));

        let (tx, rx) = std::sync::mpsc::channel();
        let console_report = crate::logging::retain_current_report();
        let worker_console_report = console_report.as_ref().map(crate::logging::ConsoleReportHandle::child);
        self.pending_block_model_loads.push((ticket, source.clone(), rx, console_report));
        let window = self.window.clone();
        crate::app::jobs::spawn_pool_task(move || {
            let compute = || {
                crate::app::jobs::run_compute_catching_panic(|| -> Result<LoadedBlockModel> {
                    // Each stage below is one pass over the model, so the bar
                    // steps at the boundaries between them; the weights are an
                    // estimate of their relative cost, not a measurement.
                    let mapping = source.csv_columns.as_ref().context("Only CSV block-model sources are supported")?;
                    let bytes = std::fs::read(&source.path).with_context(|| format!("Failed to read CSV block model {}", source.path.display()))?;
                    let parsed =
                        crate::model::formats::csv_block_model::parse(&bytes, mapping).with_context(|| format!("Failed to parse CSV block model {}", source.path.display()))?;
                    let (model, blocks) = (parsed.model, std::sync::Arc::new(parsed.blocks));
                    progress.set_fraction(0.5);
                    let renderable_block_indices = std::sync::Arc::new(model.renderable_block_indices()?);
                    progress.set_fraction(0.6);
                    progress.set_fraction(0.75);
                    let uniform_grid = blocks.uniform_grid();
                    let opaque_surface_blocks = uniform_grid.as_ref().map_or_else(
                        || crate::model::block_model::opaque_irregular_surface_block_count(&blocks, &renderable_block_indices),
                        |grid| crate::model::block_model::opaque_surface_block_count(&blocks, &renderable_block_indices, grid),
                    );
                    progress.set_fraction(0.85);
                    // Computed once here (off the UI thread) so the per-frame
                    // transparency sort and scene-bounds queries never re-walk
                    // every block's rotated corners.
                    let world_bounds = compute_world_bounds(&model, &blocks, &renderable_block_indices);
                    progress.set_fraction(0.95);
                    let (active_color_variable, active_values_cache) = prepare_initial_block_model_color(&model, &renderable_block_indices);
                    progress.set_fraction(1.0);
                    let unsupported = model.unsupported_variables();
                    if !unsupported.is_empty() {
                        // Debug-format the type so padding or unusual characters
                        // in an unrecognized type string show up in the log.
                        let names = unsupported
                            .iter()
                            .map(|variable| format!("{} ({:?})", variable.name, variable.physical_type))
                            .collect::<Vec<_>>()
                            .join(", ");
                        userspace_warn!(
                            "{}",
                            tr_format!(
                                literal = "Block model %path% has %count% variable(s) of an unsupported type that won't be readable: %names%",
                                path = source.path.display(),
                                count = unsupported.len(),
                                names = names
                            )
                        );
                    }
                    Ok(LoadedBlockModel {
                        name,
                        source,
                        model,
                        blocks,
                        renderable_block_indices,
                        uniform_grid,
                        opaque_surface_blocks,
                        world_bounds,
                        active_color_variable,
                        active_values_cache,
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
        Ok(())
    }

    pub(crate) fn poll_block_model_loads(&mut self) {
        let receivers = std::mem::take(&mut self.pending_block_model_loads);
        let mut still_pending = Vec::new();
        for (ticket, source, rx, console_report) in receivers {
            let result = rx.try_recv();
            if matches!(result, Err(std::sync::mpsc::TryRecvError::Empty)) {
                still_pending.push((ticket, source, rx, console_report));
                continue;
            }
            let complete = || match result {
                Ok(Ok(loaded)) => {
                    self.finish_background_task(ticket, true);
                    self.add_loaded_block_model(loaded);
                }
                Ok(Err(error)) => {
                    userspace_warn!("{}", tr_format!(literal = "Failed to load block model: %error%", error = format!("{error:#}")));
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => unreachable!(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    userspace_warn!("{}", tr_format!(literal = "Block model loader disconnected for %path%", path = source.path.display()));
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
        self.pending_block_model_loads = still_pending;
    }

    pub(super) fn add_loaded_block_model(&mut self, loaded: LoadedBlockModel) {
        let dims = loaded.model.metadata.dims;
        let name = crate::model::project::unique_item_name(loaded.name, self.block_models.iter().map(|item| item.name.as_str()));
        userspace_log!(
            "{}",
            tr_format!(
                literal = "Loaded block model '%name%': %blocks% blocks (%renderable% renderable), grid %dimx%x%dimy%x%dimz%, %variables% variables",
                name = name.clone(),
                blocks = loaded.model.metadata.n_blocks,
                renderable = loaded.renderable_block_indices.len(),
                dimx = dims[0],
                dimy = dims[1],
                dimz = dims[2],
                variables = loaded.model.metadata.variables.len(),
            )
        );
        let should_fit = !self.scene_has_renderables();
        let id = BlockModelId(self.next_block_model_id);
        self.next_block_model_id += 1;
        let mut open_model = OpenBlockModel {
            id,
            state: crate::model::project::ProjectItemState::dirty(loaded.source.path.file_name().map(|name| name.to_string_lossy().into_owned())),
            name,
            model: loaded.model,
            blocks: loaded.blocks,
            renderable_block_indices: loaded.renderable_block_indices,
            uniform_grid: loaded.uniform_grid,
            opaque_surface_blocks: loaded.opaque_surface_blocks,
            visible: true,
            color: DEFAULT_BLOCK_MODEL_COLOR,
            slice: None,
            active_color_variable: loaded.active_color_variable,
            color_transfers: std::collections::BTreeMap::new(),
            hide_empty_color_values: true,
            active_values_cache: loaded.active_values_cache,
            world_bounds: loaded.world_bounds,
        };
        open_model.ensure_color_transfer_for_active_variable();
        self.block_models.push(open_model);
        self.touch_active_project_content();
        if should_fit {
            self.fit_view_to_extents();
        }
        self.persist_session();
        // The model renders from block_model_gpu's per-id cache, not the
        // document scene. Avoid re-uploading every vector object here.
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn toggle_block_model_visible(&mut self, id: BlockModelId) {
        let item = ItemRef::BlockModel(id);
        let Some(style) = self.item_style(item) else {
            return;
        };
        let visible = !style.visible();
        self.set_item_style(item, style.with_visible(visible));
    }

    pub(crate) fn set_block_model_color_variable(&mut self, id: BlockModelId, variable: String) {
        let item = ItemRef::BlockModel(id);
        let Some(before) = self.item_style(item) else {
            return;
        };
        let ItemStyle::BlockModel { active_color_variable, .. } = &before else {
            return;
        };
        if active_color_variable.as_deref() == Some(variable.as_str()) {
            return;
        }
        let mut after = before.clone();
        if let ItemStyle::BlockModel { active_color_variable, .. } = &mut after {
            *active_color_variable = Some(variable);
        }
        // Applying the style clears the stale decode and reports the model in
        // the step's effects, which is what queues the job below - on undo and
        // redo as much as here.
        self.execute_edit(Command::SetItemStyle { item, before, after });
    }

    /// Decode the values behind a block model's active colour variable off the
    /// UI thread. Queued whenever a command leaves a model coloured by a
    /// variable whose values are not decoded, so undo and redo restore a
    /// renderable model rather than a blank one.
    pub(crate) fn spawn_block_model_values_decode(&mut self, id: BlockModelId) {
        let Some((variable, model_data, renderable_block_indices)) = self.block_models.iter().find(|model| model.id == id).and_then(|model| {
            let variable = model.active_color_variable.clone()?;
            (!model.active_values_available_for_render()).then(|| (variable, model.model.clone(), std::sync::Arc::clone(&model.renderable_block_indices)))
        }) else {
            return;
        };
        self.request_topology_redraw();

        let requested_variable = variable.clone();
        let compute = move |cancel: &crate::app::jobs::CancelFlag| {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let prepared = OpenBlockModel::prepare_active_values_cache(&model_data, &renderable_block_indices, Some(&requested_variable));
            Ok((requested_variable, prepared))
        };
        let apply = move |app: &mut App, result: Result<(String, crate::model::block_model::ActiveValuesCache)>| match result {
            Ok((decoded_variable, prepared)) => {
                if let Some(model) = app.block_models.iter_mut().find(|model| model.id == id)
                    && model.active_color_variable.as_deref() == Some(decoded_variable.as_str())
                {
                    model.install_active_values_cache(prepared);
                    // Deriving a default ramp for a variable that has none is
                    // the decoder finishing its own work, not a user edit, so
                    // it is not a separate undo step. Undo and redo restore a
                    // style that already carries its ramps, so the ensure call
                    // is a no-op on that path and nothing is dirtied.
                    let had_transfer = model.active_color_variable.as_deref().is_some_and(|variable| model.color_transfers.contains_key(variable));
                    model.ensure_color_transfer_for_active_variable();
                    if !had_transfer {
                        model.state.touch();
                        app.touch_active_project_content();
                    }
                    app.request_topology_redraw();
                }
            }
            Err(error) if error.to_string() != "Cancelled" => {
                userspace_warn!(
                    "{}",
                    tr_format!(
                        literal = "Could not decode block-model colour variable '%variable%': %error%",
                        variable = variable,
                        error = format!("{error:#}")
                    )
                );
            }
            Err(_) => {}
        };
        self.spawn_job("Decoding block-model colour variable…", vec![crate::app::jobs::JobKey::BlockModel(id)], compute, apply);
    }

    pub(crate) fn set_block_model_color_transfer(&mut self, id: BlockModelId, transfer: ColorTransferFunction) {
        self.record_block_model_transfer_change(id, |model| model.set_color_transfer_for_active_variable(transfer));
    }

    pub(crate) fn reset_block_model_color_transfer(&mut self, id: BlockModelId) {
        self.record_block_model_transfer_change(id, OpenBlockModel::reset_color_transfer_for_active_variable);
    }

    /// Run a ramp edit that only the model itself can compute - it needs the
    /// decoded value range - then record the before/after pair as one undo
    /// step, leaving the model holding the `before` state for the command to
    /// apply. Editing in place and recording afterwards would give the history
    /// no `before` to go back to.
    fn record_block_model_transfer_change(&mut self, id: BlockModelId, change: impl FnOnce(&mut OpenBlockModel)) {
        let Some(model) = self.block_models.iter_mut().find(|model| model.id == id) else {
            return;
        };
        let before = ItemStyle::of_block_model(model);
        change(model);
        let after = ItemStyle::of_block_model(model);
        if before == after {
            return;
        }
        if let ItemStyle::BlockModel { color_transfers, .. } = &before {
            model.color_transfers = color_transfers.clone();
        }
        self.execute_edit(Command::SetItemStyle {
            item: ItemRef::BlockModel(id),
            before,
            after,
        });
        self.request_topology_redraw();
    }

    pub(crate) fn set_block_model_slice(&mut self, id: BlockModelId, mut slice: Option<crate::model::block_model::BlockModelSlice>) {
        let Some(model) = self.block_models.iter_mut().find(|model| model.id == id) else {
            return;
        };
        if let Some(value) = slice.as_mut() {
            let (lower, upper) = model.local_bounds();
            *value = value.clamped_to(lower, upper);
            if !value.min.is_finite() || !value.max.is_finite() || value.min.cmpgt(value.max).any() {
                return;
            }
        }
        if model.slice != slice {
            let style = ItemStyle::of_block_model(model);
            self.set_item_style(ItemRef::BlockModel(id), style.with_slice(slice));
            self.request_topology_redraw();
        }
    }

    pub(crate) fn open_create_block_model_dialog(&mut self, preferred: Option<DrillHoleId>) {
        let selected = preferred
            .and_then(|id| self.drill_holes.iter().find(|dataset| dataset.id == id))
            .or_else(|| self.drill_holes.first());
        self.editor.block_model_create_open = true;
        self.editor.kriging_drill_hole_id = selected.map(|dataset| dataset.id);
        let Some(dataset) = selected else {
            return;
        };
        self.editor.kriging_variables = dataset
            .dataset
            .fields
            .iter()
            .find(|field| matches!(field.kind, DrillFieldKind::Numeric { .. }))
            .map(|field| vec![field.key.clone()])
            .unwrap_or_default();
        self.editor.kriging_name_input = format!("{} Block Model", dataset.name);
        if let Some((lower, upper)) = dataset.dataset.bounds {
            let characteristic = (upper - lower).max_element().max(1.0);
            let padding = DVec3::splat(characteristic * 0.025);
            self.editor.kriging_lower = lower - padding;
            self.editor.kriging_upper = upper + padding;
            self.editor.kriging_cell = DVec3::splat((characteristic / 25.0).max(0.001));
            self.editor.kriging_range = (characteristic / 3.0).max(self.editor.kriging_cell.max_element());
        }
        if let Some(DrillFieldKind::Numeric { min, max }) = self
            .editor
            .kriging_variables
            .first()
            .and_then(|variable| dataset.dataset.field(variable))
            .map(|field| &field.kind)
        {
            let spread = max - min;
            self.editor.kriging_sill = (spread * spread / 12.0).max(1.0e-6);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_block_model_ordinary_kriging(
        &mut self,
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
    ) -> Result<()> {
        let dataset = self
            .drill_holes
            .iter()
            .find(|dataset| dataset.id == drill_hole_id)
            .context("The selected Drill Holes collection is no longer loaded")?;
        if variables.is_empty() {
            anyhow::bail!("Select at least one numeric drill-hole variable");
        }
        let mut unique = std::collections::HashSet::new();
        for variable in &variables {
            if !unique.insert(variable) {
                anyhow::bail!("Drill-hole variable '{variable}' was selected more than once");
            }
            if !matches!(dataset.dataset.field(variable).map(|field| &field.kind), Some(DrillFieldKind::Numeric { .. })) {
                anyhow::bail!("Drill-hole variable '{variable}' is not numeric");
            }
        }
        let dataset = std::sync::Arc::clone(&dataset.dataset);
        let output_name = name.clone();
        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| -> Result<LoadedBlockModel> {
            let mut columns = Vec::with_capacity(variables.len());
            let mut grid_result = None;
            let variable_count = variables.len();
            for (index, variable) in variables.iter().enumerate() {
                let samples = collect_kriging_samples(&dataset, variable);
                if samples.len() < min_samples as usize {
                    anyhow::bail!(
                        "Variable '{variable}' has only {} usable interval samples; at least {min_samples} are required",
                        samples.len()
                    );
                }
                let start = index as f32 / variable_count as f32;
                let end = (index + 1) as f32 / variable_count as f32;
                let phase = progress.phase(start, end);
                let result = ordinary_kriging(
                    &samples,
                    KrigingGrid { lower, upper, cell },
                    OrdinaryKrigingOptions {
                        range,
                        sill,
                        nugget,
                        min_samples: min_samples as usize,
                        max_samples: max_samples as usize,
                    },
                    cancel,
                    &phase,
                )?;
                let estimated_blocks = result.estimates.iter().filter(|value| value.is_finite()).count();
                if estimated_blocks == 0 {
                    anyhow::bail!("Variable '{variable}' has no block centres with at least {min_samples} samples inside the search radius");
                }
                grid_result.get_or_insert((result.dims, result.upper, result.estimates.len()));
                columns.push(BlockModelColumn {
                    name: variable.clone(),
                    values: std::sync::Arc::new(result.estimates),
                    categories: None,
                    category_colors: std::collections::BTreeMap::new(),
                });
            }
            let (dims, grid_upper, count) = grid_result.context("No variables were selected for kriging")?;
            let model = BlockModelData::from_regular_grid(lower, cell, dims, columns)?;
            let regular_bounds = RegularBlockBounds::new(lower, grid_upper, dims, None, count).map_err(anyhow::Error::msg)?;
            let blocks = std::sync::Arc::new(BlockBoundsSource::Regular(regular_bounds));
            let renderable_block_indices = std::sync::Arc::new(RenderableBlockIndices::All(count));
            let uniform_grid = blocks.uniform_grid();
            let opaque_surface_blocks = uniform_grid
                .as_ref()
                .and_then(|grid| crate::model::block_model::opaque_surface_block_count(&blocks, &renderable_block_indices, grid));
            let world_bounds = compute_world_bounds(&model, &blocks, &renderable_block_indices);
            let (active_color_variable, active_values_cache) = prepare_initial_block_model_color(&model, &renderable_block_indices);
            let generated_id = uuid::Uuid::new_v4();
            Ok(LoadedBlockModel {
                name: output_name.clone(),
                source: BlockModelSource {
                    path: std::path::PathBuf::from(format!("generated/{generated_id}.blockmodel")),
                    csv_columns: None,
                },
                model,
                blocks,
                renderable_block_indices,
                uniform_grid,
                opaque_surface_blocks,
                world_bounds,
                active_color_variable,
                active_values_cache,
            })
        };
        let apply = move |app: &mut App, result: Result<LoadedBlockModel>| match result {
            Ok(loaded) => {
                userspace_log!("{}", tr_format!(literal = "Created block model '%name%' by Ordinary Kriging", name = loaded.name.clone()));
                app.add_loaded_block_model(loaded);
            }
            Err(error) if error.to_string() != "Cancelled" => {
                userspace_warn!("{}", tr_format!(literal = "Could not create block model: %error%", error = format!("{error:#}")))
            }
            Err(_) => {}
        };
        self.spawn_job_reporting_progress("Ordinary Kriging…", vec![crate::app::jobs::JobKey::DrillHole(drill_hole_id)], compute, apply);
        Ok(())
    }

    /// Make one block model the whole selection.
    ///
    /// The explorer's properties panel shows a block model's colours and slice
    /// only while it is selected, and clicking it in the tree is the other way
    /// (besides picking it in the viewport) to say which one.
    pub(crate) fn select_block_model(&mut self, id: BlockModelId) {
        if !self.block_models.iter().any(|model| model.id == id && model.state.loaded) {
            return;
        }
        self.editor.selected_handles.clear();
        self.editor.selected_handles.insert(SceneEntityId::BlockModel(id));
        self.editor.viewport_block_model_id = Some(id);
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    pub(crate) fn close_block_model(&mut self, id: BlockModelId) {
        let Some(entity_id) = self.block_models.iter_mut().find(|model| model.id == id).map(|model| {
            model.state.loaded = false;
            model.entity_id()
        }) else {
            return;
        };
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::BlockModel(id));
        self.clear_block_model_entity_state(entity_id);
        self.editor.block_model_table_pages.remove(&id);
        self.editor.viewport_block_model_id = self.editor.viewport_block_model_id.filter(|active| *active != id);
        self.editor.block_model_variable_ranges.retain(|(model_id, _), _| *model_id != id);
        if self.active_block_model == Some(id) {
            self.active_block_model = None;
        }
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn remove_block_model(&mut self, id: BlockModelId) {
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::BlockModel(id));
        self.clear_block_model_entity_state(SceneEntityId::BlockModel(id));
        self.delete_project_item(ItemRef::BlockModel(id));
        self.request_topology_redraw();
    }

    pub(crate) fn create_ore_triangulation(&mut self, block_model_id: BlockModelId, variable: String, mode: OreFilterMode, min: f64, max: f64, name: String) -> Result<()> {
        let model = self
            .block_models
            .iter()
            .find(|model| model.id == block_model_id)
            .context("The selected block model is no longer loaded")?;
        // BlockModelData is Arc-backed and the two large geometry arrays are Arcs,
        // so this snapshot is constant-time. Variable decoding, filtering,
        // boundary extraction, mesh construction and BVH building all stay
        // off the UI thread.
        let model_data = model.model.clone();
        let blocks = std::sync::Arc::clone(&model.blocks);
        let renderable_block_indices = std::sync::Arc::clone(&model.renderable_block_indices);
        let model_name = model.name.clone();
        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| -> Result<crate::model::triangulation::GeneratedTriangulationLog> {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let values = model_data.numeric_values(&variable)?;
            let default = model_data.variable(&variable).and_then(numeric_variable_default);
            let selected = ore_block_indices(&values, &renderable_block_indices, default, mode, min, max);
            if selected.is_empty() {
                anyhow::bail!("No blocks match the ore filter");
            }
            // Boundary extraction dominates; the mesh and BVH build that
            // follow it are single calls, so they close out the bar.
            let (vertices, faces) = boundary_mesh_from_blocks(&model_data, &blocks, &selected, cancel, &progress.phase(0.0, 0.8))?;
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let generated = crate::app::commands::triangulation::session::build_generated_triangulation(
                name,
                vertices,
                faces,
                TriSurfaceType::SolidClosed,
                crate::model::triangulation::unique_edges,
            )?;
            Ok(crate::model::triangulation::GeneratedTriangulationLog {
                generated,
                message: crate::i18n::tr_format!(literal = "Generated ore mesh from block model '%name%'", name = &model_name),
            })
        };
        let apply = move |app: &mut App, result: Result<crate::model::triangulation::GeneratedTriangulationLog>| {
            app.apply_generated_triangulation_job(result);
        };
        self.spawn_job_reporting_progress(
            crate::i18n::tr!(literal = "Building ore mesh…"),
            vec![crate::app::jobs::JobKey::BlockModel(block_model_id)],
            compute,
            apply,
        );
        Ok(())
    }
}

fn collect_kriging_samples(dataset: &DrillHoleDataset, variable: &str) -> Vec<KrigingSample> {
    dataset
        .holes
        .iter()
        .flat_map(|hole| {
            hole.intervals.iter().filter_map(move |interval| {
                let DrillValue::Numeric(value) = interval.values.get(variable)? else {
                    return None;
                };
                if !value.is_finite() || !interval.from.is_finite() || !interval.to.is_finite() || interval.to <= interval.from {
                    return None;
                }
                let position = hole.position_at_depth((interval.from + interval.to) * 0.5)?;
                position.is_finite().then_some(KrigingSample { position, value: *value })
            })
        })
        .collect()
}

fn ore_block_indices(values: &[f64], renderable_block_indices: &RenderableBlockIndices, default: Option<f64>, mode: OreFilterMode, min: f64, max: f64) -> Vec<usize> {
    renderable_block_indices
        .iter()
        .filter(|index| {
            let Some(&value) = values.get(*index) else {
                return false;
            };
            if !value.is_finite() || default.is_some_and(|default| (value - default).abs() < 1e-8) || is_no_data_sentinel(value) {
                return false;
            }
            match mode {
                OreFilterMode::GreaterOrEqual => value >= min,
                OreFilterMode::LessOrEqual => value <= min,
                OreFilterMode::Between => value >= min.min(max) && value <= min.max(max),
            }
        })
        .collect()
}

fn preferred_block_model_color_variable(model: &BlockModelData) -> Option<String> {
    let variables = model.numeric_variables();
    variables
        .iter()
        .find(|variable| !variable.special)
        .or_else(|| variables.first())
        .map(|variable| variable.name.clone())
}

fn prepare_initial_block_model_color(model: &BlockModelData, renderable: &RenderableBlockIndices) -> (Option<String>, crate::model::block_model::ActiveValuesCache) {
    let preferred = preferred_block_model_color_variable(model);
    let mut cache = OpenBlockModel::prepare_active_values_cache(model, renderable, preferred.as_deref());
    let preferred_has_range = crate::model::block_model::active_values_cache_has_range(&cache);
    if preferred_has_range {
        return (preferred, cache);
    }

    let categorical = model
        .color_variables()
        .into_iter()
        .find(|variable| !variable.special && matches!(variable.physical_type.as_str(), "namedbyte" | "namedshort"))
        .map(|variable| variable.name.clone());
    if let Some(categorical) = categorical {
        cache = OpenBlockModel::prepare_active_values_cache(model, renderable, Some(&categorical));
        return (Some(categorical), cache);
    }
    (preferred, cache)
}

#[cfg(target_arch = "wasm32")]
fn loaded_block_model_from_bytes(bytes: Vec<u8>, name: String, source: BlockModelSource) -> Result<LoadedBlockModel> {
    let (model, blocks) = if let Some(mapping) = source.csv_columns.as_ref() {
        let parsed = crate::model::formats::csv_block_model::parse(&bytes, mapping).with_context(|| format!("Failed to parse CSV block model {name}"))?;
        (parsed.model, std::sync::Arc::new(parsed.blocks))
    } else if source
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
    {
        let mapping = crate::model::formats::csv_block_model::preview(&bytes)
            .with_context(|| format!("Failed to inspect CSV block model {name}"))?
            .mapping;
        let parsed = crate::model::formats::csv_block_model::parse(&bytes, &mapping).with_context(|| format!("Failed to parse CSV block model {name}"))?;
        (parsed.model, std::sync::Arc::new(parsed.blocks))
    } else {
        anyhow::bail!("Only CSV block-model sources are supported");
    };
    let renderable_block_indices = std::sync::Arc::new(model.renderable_block_indices()?);
    let uniform_grid = blocks.uniform_grid();
    let opaque_surface_blocks = uniform_grid.as_ref().map_or_else(
        || crate::model::block_model::opaque_irregular_surface_block_count(&blocks, &renderable_block_indices),
        |grid| crate::model::block_model::opaque_surface_block_count(&blocks, &renderable_block_indices, grid),
    );
    let world_bounds = compute_world_bounds(&model, &blocks, &renderable_block_indices);
    let (active_color_variable, active_values_cache) = prepare_initial_block_model_color(&model, &renderable_block_indices);
    Ok(LoadedBlockModel {
        name,
        source,
        model,
        blocks,
        renderable_block_indices,
        uniform_grid,
        opaque_surface_blocks,
        world_bounds,
        active_color_variable,
        active_values_cache,
    })
}

fn boundary_mesh_from_blocks(
    model: &BlockModelData,
    blocks: &BlockBoundsSource,
    selected: &[usize],
    cancel: &crate::app::jobs::CancelFlag,
    progress: &crate::model::progress::Phase,
) -> Result<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)> {
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    let mut vertex_map: HashMap<[u64; 3], u32> = HashMap::new();
    let tiles = exterior_block_face_tiles(blocks, selected)?;
    let tile_count = tiles.len() as u64;
    for (index, quad) in tiles.into_iter().enumerate() {
        if index.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            progress.set_items(index as u64, tile_count);
        }
        add_block_face_tile(model, quad, &mut vertices, &mut faces, &mut vertex_map)?;
    }
    Ok((vertices, faces))
}

#[derive(Clone, Copy, Debug)]
struct FaceRect {
    plane: f64,
    u_min: f64,
    u_max: f64,
    v_min: f64,
    v_max: f64,
}

#[derive(Clone, Copy, Debug)]
struct CoveredRect {
    u_min: f64,
    u_max: f64,
    v_min: f64,
    v_max: f64,
}

#[derive(Clone, Copy, Debug)]
struct FaceCandidate {
    block_index: usize,
    rect: CoveredRect,
}

#[derive(Debug)]
enum FaceRectBvh {
    Leaf {
        bounds: CoveredRect,
        entries: Vec<FaceCandidate>,
    },
    Branch {
        bounds: CoveredRect,
        left: Box<FaceRectBvh>,
        right: Box<FaceRectBvh>,
    },
}

impl FaceRectBvh {
    fn build(mut entries: Vec<FaceCandidate>) -> Self {
        const LEAF_SIZE: usize = 8;
        let bounds = candidate_bounds(&entries);
        if entries.len() <= LEAF_SIZE {
            return Self::Leaf { bounds, entries };
        }

        let mut center_min = [f64::INFINITY; 2];
        let mut center_max = [f64::NEG_INFINITY; 2];
        for entry in &entries {
            let center = [entry.rect.u_min * 0.5 + entry.rect.u_max * 0.5, entry.rect.v_min * 0.5 + entry.rect.v_max * 0.5];
            for axis in 0..2 {
                center_min[axis] = center_min[axis].min(center[axis]);
                center_max[axis] = center_max[axis].max(center[axis]);
            }
        }
        let axis = usize::from(center_max[1] - center_min[1] > center_max[0] - center_min[0]);
        entries.sort_by(|a, b| {
            let a_center = if axis == 0 {
                a.rect.u_min * 0.5 + a.rect.u_max * 0.5
            } else {
                a.rect.v_min * 0.5 + a.rect.v_max * 0.5
            };
            let b_center = if axis == 0 {
                b.rect.u_min * 0.5 + b.rect.u_max * 0.5
            } else {
                b.rect.v_min * 0.5 + b.rect.v_max * 0.5
            };
            a_center.total_cmp(&b_center)
        });
        let right_entries = entries.split_off(entries.len() / 2);
        Self::Branch {
            bounds,
            left: Box::new(Self::build(entries)),
            right: Box::new(Self::build(right_entries)),
        }
    }

    fn query(&self, query: CoveredRect, matches: &mut Vec<FaceCandidate>) {
        match self {
            Self::Leaf { bounds, entries } => {
                if !rects_overlap(*bounds, query) {
                    return;
                }
                matches.extend(entries.iter().copied().filter(|entry| rects_overlap(entry.rect, query)));
            }
            Self::Branch { bounds, left, right } => {
                if !rects_overlap(*bounds, query) {
                    return;
                }
                left.query(query, matches);
                right.query(query, matches);
            }
        }
    }
}

fn candidate_bounds(entries: &[FaceCandidate]) -> CoveredRect {
    entries.iter().fold(
        CoveredRect {
            u_min: f64::INFINITY,
            u_max: f64::NEG_INFINITY,
            v_min: f64::INFINITY,
            v_max: f64::NEG_INFINITY,
        },
        |bounds, entry| CoveredRect {
            u_min: bounds.u_min.min(entry.rect.u_min),
            u_max: bounds.u_max.max(entry.rect.u_max),
            v_min: bounds.v_min.min(entry.rect.v_min),
            v_max: bounds.v_max.max(entry.rect.v_max),
        },
    )
}

fn rects_overlap(a: CoveredRect, b: CoveredRect) -> bool {
    a.u_min < b.u_max && b.u_min < a.u_max && a.v_min < b.v_max && b.v_min < a.v_max
}

/// Tiles only the exposed portion of every selected block face. Plane maps
/// find blocks on the opposite side of each face; subdividing at all overlap
/// edges handles one-large-to-many-small sub-block adjacency without leaving
/// coplanar internal partitions in the generated solid.
fn exterior_block_face_tiles(blocks: &BlockBoundsSource, selected: &[usize]) -> Result<Vec<[DVec3; 4]>> {
    let mut seen = HashSet::new();
    let mut selected_indices = Vec::with_capacity(selected.len());
    for &index in selected {
        let block = blocks.get(index).with_context(|| format!("Ore block index {index} is out of range"))?;
        if !block.lower.is_finite() || !block.upper.is_finite() || !block.lower.cmplt(block.upper).all() {
            anyhow::bail!("Ore block {index} has invalid bounds");
        }
        if seen.insert(index) {
            selected_indices.push(index);
        }
    }

    // For each face direction, index the boundary plane of a potential block
    // on the opposite side: upper X blocks cover -X faces, lower X blocks
    // cover +X faces, and likewise for Y/Z.
    let mut plane_entries: [HashMap<u64, Vec<FaceCandidate>>; 6] = std::array::from_fn(|_| HashMap::new());
    for &index in &selected_indices {
        let block = blocks.get(index).with_context(|| format!("Ore block index {index} is out of range"))?;
        let planes = [block.upper.x, block.lower.x, block.upper.y, block.lower.y, block.upper.z, block.lower.z];
        for (face_index, plane) in planes.into_iter().enumerate() {
            let face = block_face_rect(block, face_index);
            plane_entries[face_index].entry(quantize(plane)).or_default().push(FaceCandidate {
                block_index: index,
                rect: CoveredRect {
                    u_min: face.u_min,
                    u_max: face.u_max,
                    v_min: face.v_min,
                    v_max: face.v_max,
                },
            });
        }
    }
    let opposite_planes = plane_entries.map(|groups| groups.into_iter().map(|(plane, entries)| (plane, FaceRectBvh::build(entries))).collect::<HashMap<_, _>>());

    let mut exposed_planes: [HashMap<u64, (f64, Vec<CoveredRect>)>; 6] = std::array::from_fn(|_| HashMap::new());
    for &index in &selected_indices {
        let block = blocks.get(index).with_context(|| format!("Ore block index {index} is out of range"))?;
        for (face_index, plane_index) in opposite_planes.iter().enumerate() {
            let face = block_face_rect(block, face_index);
            let mut covered = Vec::new();
            if let Some(indexed_faces) = plane_index.get(&quantize(face.plane)) {
                let query = CoveredRect {
                    u_min: face.u_min,
                    u_max: face.u_max,
                    v_min: face.v_min,
                    v_max: face.v_max,
                };
                let mut candidates = Vec::new();
                indexed_faces.query(query, &mut candidates);
                for candidate in candidates {
                    if candidate.block_index == index {
                        continue;
                    }
                    let overlap = CoveredRect {
                        u_min: face.u_min.max(candidate.rect.u_min),
                        u_max: face.u_max.min(candidate.rect.u_max),
                        v_min: face.v_min.max(candidate.rect.v_min),
                        v_max: face.v_max.min(candidate.rect.v_max),
                    };
                    if overlap.u_min < overlap.u_max && overlap.v_min < overlap.v_max {
                        covered.push(overlap);
                    }
                }
            }
            exposed_planes[face_index]
                .entry(quantize(face.plane))
                .or_insert_with(|| (face.plane, Vec::new()))
                .1
                .extend(uncovered_face_rects(face, &covered));
        }
    }
    let mut tiles = Vec::new();
    for (face_index, planes) in exposed_planes.into_iter().enumerate() {
        for (_, (plane, exposed)) in planes {
            tiles.extend(
                conforming_rect_tiles(&exposed)
                    .into_iter()
                    .map(|rect| face_tile(plane, rect.u_min, rect.u_max, rect.v_min, rect.v_max, face_index)),
            );
        }
    }
    Ok(tiles)
}

fn block_face_rect(block: BlockBounds, face_index: usize) -> FaceRect {
    match face_index {
        0 => FaceRect {
            plane: block.lower.x,
            u_min: block.lower.y,
            u_max: block.upper.y,
            v_min: block.lower.z,
            v_max: block.upper.z,
        },
        1 => FaceRect {
            plane: block.upper.x,
            u_min: block.lower.y,
            u_max: block.upper.y,
            v_min: block.lower.z,
            v_max: block.upper.z,
        },
        2 => FaceRect {
            plane: block.lower.y,
            u_min: block.lower.x,
            u_max: block.upper.x,
            v_min: block.lower.z,
            v_max: block.upper.z,
        },
        3 => FaceRect {
            plane: block.upper.y,
            u_min: block.lower.x,
            u_max: block.upper.x,
            v_min: block.lower.z,
            v_max: block.upper.z,
        },
        4 => FaceRect {
            plane: block.lower.z,
            u_min: block.lower.x,
            u_max: block.upper.x,
            v_min: block.lower.y,
            v_max: block.upper.y,
        },
        _ => FaceRect {
            plane: block.upper.z,
            u_min: block.lower.x,
            u_max: block.upper.x,
            v_min: block.lower.y,
            v_max: block.upper.y,
        },
    }
}

fn uncovered_face_rects(face: FaceRect, covered: &[CoveredRect]) -> Vec<CoveredRect> {
    let mut uncovered = vec![CoveredRect {
        u_min: face.u_min,
        u_max: face.u_max,
        v_min: face.v_min,
        v_max: face.v_max,
    }];
    for cover in covered {
        let mut next = Vec::new();
        for tile in uncovered {
            subtract_covered_rect(tile, *cover, &mut next);
        }
        uncovered = next;
        if uncovered.is_empty() {
            break;
        }
    }
    uncovered
}

fn subtract_covered_rect(tile: CoveredRect, cover: CoveredRect, output: &mut Vec<CoveredRect>) {
    let intersection = CoveredRect {
        u_min: tile.u_min.max(cover.u_min),
        u_max: tile.u_max.min(cover.u_max),
        v_min: tile.v_min.max(cover.v_min),
        v_max: tile.v_max.min(cover.v_max),
    };
    if intersection.u_min >= intersection.u_max || intersection.v_min >= intersection.v_max {
        output.push(tile);
        return;
    }

    if tile.u_min < intersection.u_min {
        output.push(CoveredRect {
            u_min: tile.u_min,
            u_max: intersection.u_min,
            v_min: tile.v_min,
            v_max: tile.v_max,
        });
    }
    if intersection.u_max < tile.u_max {
        output.push(CoveredRect {
            u_min: intersection.u_max,
            u_max: tile.u_max,
            v_min: tile.v_min,
            v_max: tile.v_max,
        });
    }
    if tile.v_min < intersection.v_min {
        output.push(CoveredRect {
            u_min: intersection.u_min,
            u_max: intersection.u_max,
            v_min: tile.v_min,
            v_max: intersection.v_min,
        });
    }
    if intersection.v_max < tile.v_max {
        output.push(CoveredRect {
            u_min: intersection.u_min,
            u_max: intersection.u_max,
            v_min: intersection.v_max,
            v_max: tile.v_max,
        });
    }
}

/// Retile a planar union on its complete endpoint grid. Splitting a large
/// exterior rectangle at neighboring sub-block corners gives the final mesh
/// matching edges instead of topological T-junctions.
fn conforming_rect_tiles(rects: &[CoveredRect]) -> Vec<CoveredRect> {
    let mut u_cuts = Vec::with_capacity(rects.len() * 2);
    let mut v_cuts = Vec::with_capacity(rects.len() * 2);
    for rect in rects {
        u_cuts.extend([rect.u_min, rect.u_max]);
        v_cuts.extend([rect.v_min, rect.v_max]);
    }
    u_cuts.sort_by(f64::total_cmp);
    v_cuts.sort_by(f64::total_cmp);
    u_cuts.dedup_by(|a, b| *a == *b);
    v_cuts.dedup_by(|a, b| *a == *b);

    let mut cells = HashSet::<(usize, usize)>::new();
    for rect in rects {
        let u_start = u_cuts.binary_search_by(|value| value.total_cmp(&rect.u_min)).expect("rectangle endpoint was inserted");
        let u_end = u_cuts.binary_search_by(|value| value.total_cmp(&rect.u_max)).expect("rectangle endpoint was inserted");
        let v_start = v_cuts.binary_search_by(|value| value.total_cmp(&rect.v_min)).expect("rectangle endpoint was inserted");
        let v_end = v_cuts.binary_search_by(|value| value.total_cmp(&rect.v_max)).expect("rectangle endpoint was inserted");
        for u in u_start..u_end {
            for v in v_start..v_end {
                cells.insert((u, v));
            }
        }
    }
    let mut cells = cells.into_iter().collect::<Vec<_>>();
    cells.sort_unstable();
    cells
        .into_iter()
        .map(|(u, v)| CoveredRect {
            u_min: u_cuts[u],
            u_max: u_cuts[u + 1],
            v_min: v_cuts[v],
            v_max: v_cuts[v + 1],
        })
        .collect()
}

fn face_tile(plane: f64, u_min: f64, u_max: f64, v_min: f64, v_max: f64, face_index: usize) -> [DVec3; 4] {
    match face_index {
        0 => [
            DVec3::new(plane, u_min, v_min),
            DVec3::new(plane, u_max, v_min),
            DVec3::new(plane, u_max, v_max),
            DVec3::new(plane, u_min, v_max),
        ],
        1 => [
            DVec3::new(plane, u_min, v_min),
            DVec3::new(plane, u_min, v_max),
            DVec3::new(plane, u_max, v_max),
            DVec3::new(plane, u_max, v_min),
        ],
        2 => [
            DVec3::new(u_min, plane, v_min),
            DVec3::new(u_min, plane, v_max),
            DVec3::new(u_max, plane, v_max),
            DVec3::new(u_max, plane, v_min),
        ],
        3 => [
            DVec3::new(u_min, plane, v_min),
            DVec3::new(u_max, plane, v_min),
            DVec3::new(u_max, plane, v_max),
            DVec3::new(u_min, plane, v_max),
        ],
        4 => [
            DVec3::new(u_min, v_min, plane),
            DVec3::new(u_max, v_min, plane),
            DVec3::new(u_max, v_max, plane),
            DVec3::new(u_min, v_max, plane),
        ],
        _ => [
            DVec3::new(u_min, v_min, plane),
            DVec3::new(u_min, v_max, plane),
            DVec3::new(u_max, v_max, plane),
            DVec3::new(u_max, v_min, plane),
        ],
    }
}

fn add_block_face_tile(
    model: &BlockModelData,
    quad: [DVec3; 4],
    vertices: &mut Vec<mesh_data::Vertex>,
    faces: &mut Vec<[u32; 3]>,
    vertex_map: &mut HashMap<[u64; 3], u32>,
) -> Result<()> {
    let mut indices = [0u32; 4];
    for (dst, corner) in indices.iter_mut().zip(quad) {
        let world = model.local_to_world(corner);
        let key = point_key(world);
        *dst = if let Some(index) = vertex_map.get(&key) {
            *index
        } else {
            let index = u32::try_from(vertices.len()).map_err(|_| anyhow::anyhow!("Ore triangulation has too many vertices"))?;
            vertices.push(mesh_data::Vertex::new(world.x, world.y, world.z));
            vertex_map.insert(key, index);
            index
        };
    }
    faces.push([indices[0], indices[1], indices[2]]);
    faces.push([indices[0], indices[2], indices[3]]);
    Ok(())
}

fn point_key(point: DVec3) -> [u64; 3] {
    [quantize(point.x), quantize(point.y), quantize(point.z)]
}

fn quantize(value: f64) -> u64 {
    let rounded = (value * 1_000_000.0).round();
    if rounded == 0.0 { 0.0_f64.to_bits() } else { rounded.to_bits() }
}
