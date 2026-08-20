pub(crate) mod block_model;
#[cfg(target_arch = "wasm32")]
pub(crate) mod browser_data; // Persists browser imports (point clouds, block models, rasters)
pub(crate) mod drawing; // Handles finishing polygons, creating points, etc commands
pub(crate) mod drill_hole;
pub(crate) mod file; // Handles importing, exportings, etc. commands
pub(crate) mod layer; // Handles creating layers, deleting layers, etc. commands
pub(crate) mod omf; // Whole-project Open Mining Format interchange.
pub(crate) mod plot; // Handles printable plot sheets
pub(crate) mod point_cloud; // Handles importing/loading point clouds, etc. commands
pub(crate) mod property; // Handles changing colors, fills, etc. commands
pub(crate) mod raster; // Handles georeferenced image textures.
pub(crate) mod slice; // Handles the vertical slice view mode.
pub(crate) mod text; // Handles text editing commands
pub(crate) mod triangulation; // Handles loading meshes, deleting meshes, etc. commands
pub(crate) mod view; /* Handles resetting camera view, , etc. commands */

use anyhow::Result;

use crate::{
    app::App,
    ui::state::{MoveLayerDialog, TriCreatePhase, UiCommand},
    userspace_error, userspace_warn,
};

impl<'a> App<'a> {
    pub(crate) fn apply_history_step(&mut self, undo: bool) {
        if self.has_pending_move_delta() {
            self.cancel_move_delta();
        }
        let layers_before: std::collections::HashSet<_> = self
            .workspace
            .active_project()
            .into_iter()
            .flat_map(|project| project.pidb.document.layers())
            .map(|layer| layer.id)
            .collect();
        let changed = self.workspace.active_project_mut().is_some_and(|project| {
            if undo {
                self.history.undo(&mut project.pidb.document)
            } else {
                self.history.redo(&mut project.pidb.document)
            }
        });
        if !changed {
            return;
        }
        if let Some(project) = self.workspace.active_project_mut() {
            let layers_after: std::collections::HashSet<_> = project.pidb.document.layers().iter().map(|layer| layer.id).collect();
            project.loaded_layers.retain(|layer_id| layers_after.contains(layer_id));
            project.loaded_layers.extend(layers_after.difference(&layers_before).copied());
        }
        if self.editor.active_layer.is_some_and(|layer_id| self.active_layer() != Some(layer_id)) {
            self.editor.active_layer = None;
        }
        self.editor.selected_handles.clear();
        self.editor.canvas_context_menu_open = false;
        self.cancel_fuse();
        self.cancel_chamfer();
        self.clear_bezier_state();
        self.invalidate_geometry();
    }

    pub(crate) fn handle_ui_commands(&mut self, commands: Vec<UiCommand>) {
        let had_commands = !commands.is_empty();
        for command in commands {
            if let Some(spec) = command.console_report_spec() {
                let report_id = crate::logging::begin_command_report(spec);
                let result = crate::logging::with_report_scope(report_id, || self.handle_ui_command(command));
                crate::logging::finish_command_report(report_id, result.as_ref().err());
            } else if let Err(err) = self.handle_ui_command(command) {
                userspace_error!("Command failed: {err:#}");
            }
        }
        if had_commands {
            self.redraw_requested = true;
        }
    }
    /// Dispatch a UI command to the relevant domain handler (in the sibling
    /// `commands::*` modules). This is a thin router; the work lives in those
    /// per-domain `impl` blocks.
    pub(crate) fn handle_ui_command(&mut self, command: UiCommand) -> Result<()> {
        match command {
            UiCommand::SetActiveTool(tool) => {
                self.set_active_tool_from_toolbar(tool);
                Ok(())
            }
            UiCommand::ConfirmDrapeSelection => {
                self.confirm_drape_selection();
                Ok(())
            }
            UiCommand::SetFlyModeEnabled(enabled) => {
                self.set_fly_mode_enabled(enabled);
                Ok(())
            }
            UiCommand::SetSliceModeEnabled(enabled) => {
                self.set_slice_mode_enabled(enabled);
                Ok(())
            }
            UiCommand::NewPidb => {
                self.choose_new_pidb();
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            UiCommand::CreateBrowserPidb { name } => self.create_browser_pidb(name),
            UiCommand::OpenPidb => {
                self.choose_open_pidb();
                Ok(())
            }
            UiCommand::OpenPidbPaths(paths) => {
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = paths;
                    self.open_web_pidb_sources()
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.execute_file_dialog_action(file::FileDialogAction::OpenPidb(paths))
            }
            UiCommand::CloseStartupDialog => {
                self.startup_dialog_dismissed = true;
                Ok(())
            }
            UiCommand::ImportOmfPaths(paths) => {
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = paths;
                    self.import_web_omf_sources()
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.import_omf_paths(paths);
                    Ok(())
                }
            }
            UiCommand::ImportAsPidbPaths(kind, paths) => {
                self.choose_import_as_pidb_paths(kind, paths);
                Ok(())
            }
            UiCommand::ImportDxfPathsInto(paths) => self.import_dxf_paths_into(paths),
            UiCommand::ImportTriangulationPaths(paths) => {
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = paths;
                    self.import_web_triangulation_sources()
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.execute_file_dialog_action(file::FileDialogAction::ImportTriangulation(paths))
            }
            UiCommand::ImportPointCloudPaths(paths) => {
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = paths;
                    self.import_web_point_cloud_sources()
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.execute_file_dialog_action(file::FileDialogAction::ImportPointCloud(paths))
            }
            UiCommand::ImportRasterPaths(paths) => {
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = paths;
                    self.import_web_raster_sources()
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.execute_file_dialog_action(file::FileDialogAction::ImportRaster(paths))
            }
            UiCommand::LoadRaster(path) => {
                #[cfg(target_arch = "wasm32")]
                self.load_browser_data(browser_data::BrowserDataKind::Raster, &path);
                #[cfg(not(target_arch = "wasm32"))]
                self.open_raster_path(path);
                Ok(())
            }
            UiCommand::UnloadRaster(path) => {
                self.unload_raster(&path);
                Ok(())
            }
            UiCommand::RemoveRaster(path) => {
                // In the browser the record is the raster's only home, so
                // removing the entry has to delete it from storage too.
                #[cfg(target_arch = "wasm32")]
                self.delete_browser_data(browser_data::BrowserDataKind::Raster, &path);
                self.remove_raster(&path);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::RevealRaster(path) => self.reveal_in_file_manager(&path),
            UiCommand::DrapeRaster(path) => self.drape_raster_over_surfaces(&path),
            UiCommand::UndrapeRaster(path) => {
                self.undrape_raster(&path);
                Ok(())
            }
            UiCommand::ClearActiveTriangulationRaster => self.clear_active_triangulation_raster(),
            UiCommand::LoadPointCloud(path) => {
                #[cfg(target_arch = "wasm32")]
                self.load_browser_data(browser_data::BrowserDataKind::PointCloud, &path);
                #[cfg(not(target_arch = "wasm32"))]
                self.open_point_cloud_path(path);
                Ok(())
            }
            UiCommand::ClosePointCloud(id) => {
                if self.point_clouds.iter().any(|cloud| cloud.id == id && !cloud.is_saved) {
                    self.editor.point_cloud_close_unsaved = Some(id);
                    return Ok(());
                }
                self.close_point_cloud(id);
                Ok(())
            }
            UiCommand::ClosePointCloudForce(id) => {
                self.editor.point_cloud_close_unsaved = None;
                self.close_point_cloud(id);
                Ok(())
            }
            UiCommand::TogglePointCloudVisible(id) => {
                self.toggle_point_cloud_visible(id);
                Ok(())
            }
            UiCommand::RemovePointCloud(path) => {
                #[cfg(target_arch = "wasm32")]
                self.delete_browser_data(browser_data::BrowserDataKind::PointCloud, &path);
                self.remove_point_cloud(&path);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::RevealPointCloud(id) => self.reveal_point_cloud(id),
            UiCommand::ChooseImportSourceFiles(kind) => {
                self.choose_import_source_files(kind);
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            UiCommand::ClearBrowserImportSelection(kind) => {
                self.clear_browser_import_selection(kind);
                Ok(())
            }
            UiCommand::ImportCsvBlockModel { path, mapping } => {
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = path;
                    self.import_web_csv_block_model(mapping)
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.import_block_model_source(crate::model::block_model::BlockModelSource {
                    path,
                    csv_columns: Some(mapping),
                    generated: false,
                })
            }
            UiCommand::ExportOmf => self.choose_export_omf(),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::OpenTriangulationFolder => {
                self.choose_open_triangulation_folder();
                Ok(())
            }
            UiCommand::ExportPidbDxf(runtime_id) => {
                self.choose_export_pidb_dxf(runtime_id);
                Ok(())
            }
            UiCommand::ExportPidbCopy(runtime_id) => {
                self.choose_export_pidb_copy(runtime_id);
                Ok(())
            }
            UiCommand::ExportViewportImage => {
                self.spawn_export_viewport_image_dialog();
                Ok(())
            }
            UiCommand::ExportLayerDxf(layer) => {
                self.choose_export_layer_dxf(layer);
                Ok(())
            }
            UiCommand::ExportTriangulationAs(id, format) => {
                self.choose_export_triangulation_as(id, format);
                Ok(())
            }
            UiCommand::ExportBlockModelCsv(id) => {
                self.choose_export_block_model_csv(id);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::SaveTriangulationAs(id) => {
                self.spawn_save_triangulation_dialog(id);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::SaveBlockModelAs(id) => {
                self.spawn_save_block_model_dialog(id, false);
                Ok(())
            }
            UiCommand::SaveAndCloseTriangulationAs(id) => {
                self.spawn_save_and_close_triangulation_dialog(id);
                Ok(())
            }
            UiCommand::SaveAndCloseBlockModelAs(id) => {
                self.spawn_save_block_model_dialog(id, true);
                #[cfg(target_arch = "wasm32")]
                {
                    self.editor.block_model_close_unsaved = None;
                }
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::RevealTriangulation(id) => self.reveal_triangulation(id),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::RevealBlockModel(id) => self.reveal_block_model(id),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::RevealPidb(path) => self.reveal_pidb(&path),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::RevealPath(path) => self.reveal_in_file_manager(&path),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::OpenDirectory(path) => self.open_directory_in_file_manager(&path),
            UiCommand::RevealAllTriangulations => {
                for tri in &mut self.triangulations {
                    tri.visible = true;
                }
                self.invalidate_geometry();
                Ok(())
            }
            UiCommand::RequestExit => self.request_exit(),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::SaveAndExit => self.save_and_exit(),
            UiCommand::ExitWithoutSaving => {
                self.exit_without_saving();
                Ok(())
            }
            UiCommand::CancelExit => {
                self.cancel_exit_request();
                Ok(())
            }
            UiCommand::CreateLayer { name } => self.create_layer(name),
            UiCommand::RequestDeleteLayer(layer_id) => {
                self.activate_project_for_layer(layer_id);
                let layer_name = self
                    .workspace
                    .active_project()
                    .and_then(|project| project.pidb.document.layer(layer_id))
                    .map(|layer| layer.name.clone());
                self.editor.pending_delete_layer = layer_name.map(|name| (layer_id, name));
                Ok(())
            }
            UiCommand::DeleteLayer(layer_id) => {
                self.activate_project_for_layer(layer_id);
                self.delete_layer(layer_id)
            }
            UiCommand::DuplicateLayer(layer_id) => {
                self.activate_project_for_layer(layer_id);
                self.duplicate_layer(layer_id);
                Ok(())
            }
            UiCommand::BeginMoveLayer(layer_id) => {
                self.activate_project_for_layer(layer_id);
                let source_id = self.workspace.active_project().map(|project| project.runtime_id);
                let target_project = self
                    .workspace
                    .projects
                    .iter()
                    .map(|project| project.runtime_id)
                    .find(|runtime_id| Some(*runtime_id) != source_id);
                self.editor.move_layer_dialog = Some(MoveLayerDialog { layer_id, target_project });
                Ok(())
            }
            UiCommand::MoveLayerToPidb { layer_id, target_project } => self.move_layer_to_pidb(layer_id, target_project),
            UiCommand::BeginRenameLayer(layer_id) => {
                self.activate_project_for_layer(layer_id);
                let current_name = self
                    .workspace
                    .active_project()
                    .and_then(|p| p.pidb.document.layer(layer_id))
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
                self.editor.renaming_layer = Some((layer_id, current_name));
                Ok(())
            }
            UiCommand::RenameLayer { layer_id, new_name } => {
                self.activate_project_for_layer(layer_id);
                let Some((before, is_loaded)) = self.workspace.active_project().and_then(|project| {
                    project
                        .pidb
                        .document
                        .layer(layer_id)
                        .map(|layer| (layer.name.clone(), project.loaded_layers.contains(&layer_id)))
                }) else {
                    self.editor.renaming_layer = None;
                    return Ok(());
                };
                if before != new_name {
                    if is_loaded {
                        self.editor.active_layer = Some(layer_id);
                        if let Some(project) = self.workspace.active_project_mut() {
                            self.history.execute(
                                &mut project.pidb.document,
                                crate::model::Command::RenameLayer {
                                    id: layer_id,
                                    before,
                                    after: new_name,
                                },
                            );
                        }
                    } else {
                        self.history.clear();
                        if self.editor.active_layer == Some(layer_id) {
                            self.editor.active_layer = None;
                        }
                        if let Some(project) = self.workspace.active_project_mut() {
                            project.pidb.document.rename_layer(layer_id, new_name);
                        }
                    }
                }
                self.editor.renaming_layer = None;
                Ok(())
            }
            UiCommand::LoadLayer(layer) => {
                self.load_layer(layer);
                Ok(())
            }
            UiCommand::UnloadLayer(layer) => {
                self.unload_layer(layer);
                Ok(())
            }
            UiCommand::SelectAllObjectsInLayer(layer) => {
                self.activate_project_for_layer(layer);
                self.select_all_objects_in_layer(layer);
                Ok(())
            }
            UiCommand::LoadAllLayers(runtime_id) => {
                self.load_all_layers(runtime_id);
                Ok(())
            }
            UiCommand::UnloadAllLayers(runtime_id) => {
                self.unload_all_layers(runtime_id);
                Ok(())
            }
            UiCommand::SaveAllPidbs => self.save_all_dirty_projects().map(|_| ()),
            #[cfg(target_arch = "wasm32")]
            UiCommand::DownloadAllPidbs => self.download_all_pidbs(),
            UiCommand::SavePidb(runtime_id) => self.save_project(runtime_id),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::SavePidbAs(runtime_id) => {
                self.spawn_save_pidb_as_dialog(runtime_id);
                Ok(())
            }
            UiCommand::ActivatePidb(runtime_id) => self.activate_pidb(runtime_id),
            UiCommand::ClosePidb(runtime_id) => {
                self.request_close_pidb(runtime_id);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::SaveAndClosePidb(runtime_id) => self.save_and_close_pidb(runtime_id),
            UiCommand::ClosePidbForce(runtime_id) => {
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
            UiCommand::RequestDiscardPidbChanges(runtime_id) => {
                self.request_discard_pidb_changes(runtime_id);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::DiscardPidbChanges(runtime_id) => self.discard_pidb_changes(runtime_id),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::RequestDiscardLayerChanges(layer_id) => {
                self.request_discard_layer_changes(layer_id);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::DiscardLayerChanges(layer_id) => self.discard_layer_changes(layer_id),
            UiCommand::LoadTriangulation(path) => {
                #[cfg(target_arch = "wasm32")]
                {
                    self.load_browser_triangulation(&path);
                    Ok(())
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.open_triangulation_path(&path)
            }
            UiCommand::LoadBlockModel(source) => {
                #[cfg(target_arch = "wasm32")]
                {
                    self.load_browser_data(browser_data::BrowserDataKind::BlockModel, &source.path);
                    Ok(())
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.open_block_model_source(source)
            }
            UiCommand::CloseBlockModel(id) => {
                if self.block_models.iter().any(|model| model.id == id && model.source.generated) {
                    self.editor.block_model_close_unsaved = Some(id);
                    return Ok(());
                }
                self.close_block_model(id);
                Ok(())
            }
            UiCommand::CloseBlockModelForce(id) => {
                self.editor.block_model_close_unsaved = None;
                self.close_block_model(id);
                Ok(())
            }
            UiCommand::RemoveBlockModel(source) => {
                #[cfg(target_arch = "wasm32")]
                self.delete_browser_data(browser_data::BrowserDataKind::BlockModel, &source.path);
                self.remove_block_model(source);
                Ok(())
            }
            UiCommand::ToggleBlockModelVisible(id) => {
                self.toggle_block_model_visible(id);
                Ok(())
            }
            UiCommand::SetBlockModelColorVariable { id, variable } => {
                self.set_block_model_color_variable(id, variable);
                Ok(())
            }
            UiCommand::SetBlockModelColorStops { id, stops } => {
                self.set_block_model_color_stops(id, stops);
                Ok(())
            }
            UiCommand::SetBlockModelSlice { id, slice } => {
                self.set_block_model_slice(id, slice);
                Ok(())
            }
            UiCommand::ImportDrillHole(source) => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.import_drill_hole_source(source)
                }
                #[cfg(target_arch = "wasm32")]
                {
                    self.import_web_drill_hole_source(source)
                }
            }
            UiCommand::LoadDrillHole(source) => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.open_drill_hole_source(source)
                }
                #[cfg(target_arch = "wasm32")]
                {
                    self.load_browser_data(browser_data::BrowserDataKind::DrillHole, source.primary_path());
                    Ok(())
                }
            }
            UiCommand::CloseDrillHole(id) => {
                self.close_drill_hole(id);
                Ok(())
            }
            UiCommand::RemoveDrillHole(source) => {
                #[cfg(target_arch = "wasm32")]
                self.delete_browser_data(browser_data::BrowserDataKind::DrillHole, source.primary_path());
                self.remove_drill_hole(source);
                Ok(())
            }
            UiCommand::ToggleDrillHoleVisible(id) => {
                self.toggle_drill_hole_visible(id);
                Ok(())
            }
            UiCommand::OpenDrillHoleColorDialog(id) => {
                self.editor.drill_hole_color_dialog = Some(id);
                Ok(())
            }
            UiCommand::SetDrillHoleColorField { id, field } => {
                self.set_drill_hole_color_field(id, field);
                Ok(())
            }
            UiCommand::SetDrillHoleColorPreset { id, preset } => {
                self.set_drill_hole_color_preset(id, preset);
                Ok(())
            }
            UiCommand::SetDrillHoleColorStops { id, stops } => {
                self.set_drill_hole_color_stops(id, stops);
                Ok(())
            }
            UiCommand::SetDrillHoleCategoryColors { id, categories } => {
                self.set_drill_hole_category_colors(id, categories);
                Ok(())
            }
            UiCommand::OpenCreateBlockModel(preferred) => {
                self.open_create_block_model_dialog(preferred);
                Ok(())
            }
            UiCommand::ExecuteCreateBlockModel {
                drill_hole_id,
                variables,
                name,
                lower,
                upper,
                cell,
                range,
                sill,
                nugget,
                min_samples,
                max_samples,
            } => {
                let result = self.create_block_model_ordinary_kriging(drill_hole_id, variables, name, lower, upper, cell, range, sill, nugget, min_samples, max_samples);
                if result.is_ok() {
                    self.editor.block_model_create_open = false;
                }
                result
            }
            UiCommand::OpenCreateOreTriangulation => {
                self.editor.ore_triangulation_open = true;
                self.editor.ore_block_model_id = self.active_block_model.or_else(|| self.block_models.first().map(|model| model.id));
                if let Some(model) = self.editor.ore_block_model_id.and_then(|id| self.block_models.iter().find(|model| model.id == id)) {
                    self.editor.ore_variable = model
                        .active_color_variable
                        .clone()
                        .filter(|name| model.model.numeric_variables().into_iter().any(|variable| variable.name == *name))
                        .or_else(|| model.model.numeric_variables().into_iter().find(|var| !var.special).map(|var| var.name.clone()))
                        .unwrap_or_default();
                    let stem = std::path::Path::new(&model.name).file_stem().and_then(|value| value.to_str()).unwrap_or(&model.name);
                    self.editor.ore_name_input = format!("{stem}_ore");
                }
                Ok(())
            }
            UiCommand::ExecuteCreateOreTriangulation {
                block_model_id,
                variable,
                mode,
                min,
                max,
                name,
            } => {
                let result = self.create_ore_triangulation(block_model_id, variable, mode, min, max, name);
                if result.is_ok() {
                    self.editor.ore_triangulation_open = false;
                }
                result
            }
            UiCommand::FinishPolyClose => {
                self.finish_poly_closed();
                Ok(())
            }
            UiCommand::CommitStrokeOpen => {
                self.commit_stroke_open();
                Ok(())
            }
            UiCommand::CommitCircleTypedRadius => {
                self.commit_circle_typed_radius();
                Ok(())
            }
            UiCommand::CancelOffset => {
                self.cancel_offset();
                Ok(())
            }
            UiCommand::CancelRelimit => {
                self.cancel_relimit();
                Ok(())
            }
            UiCommand::ResetView => {
                self.set_slice_mode_enabled(false);
                self.reset_view();
                Ok(())
            }
            UiCommand::SetTopologyWireframes(enabled) => self.set_topology_wireframes(enabled),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::SetSlicePreviewDetached(detached) => {
                self.editor.slice_preview_detached = cfg!(not(target_arch = "wasm32")) && detached && self.editor.slice_mode_enabled;
                self.slice_preview_cursor_px = None;
                self.slice_preview_middle_down = false;
                if !self.editor.slice_preview_detached
                    && let Some(graphics) = self.graphics.as_mut()
                {
                    graphics.close_slice_preview();
                }
                self.redraw_requested = true;
                Ok(())
            }
            UiCommand::SetShowPoints(enabled) => self.set_show_points(enabled),
            UiCommand::SetDarkMode(enabled) => self.set_dark_mode(enabled),
            UiCommand::SetShowConsole(enabled) => self.set_show_console(enabled),
            UiCommand::SetShowXyGrid(enabled) => self.set_show_xy_grid(enabled),
            UiCommand::SetShowScaleBar(enabled) => self.set_show_scale_bar(enabled),
            UiCommand::SetStandardView(view) => {
                // The slice camera is derived from the slice state each frame;
                // a standard-view transition would silently queue and fire on
                // exit, so ignore it while sliced.
                if self.editor.slice_mode_enabled {
                    return Ok(());
                }
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.set_standard_view(view);
                    self.redraw_requested = true;
                }
                Ok(())
            }
            UiCommand::ApplyPreferences(preferences) => self.apply_preferences(preferences),
            UiCommand::OpenPreferences => {
                self.editor.reset_preferences_draft();
                self.editor.preferences_open = true;
                Ok(())
            }
            UiCommand::RemoveTriangulation(path) => {
                // On the browser, "remove" is a deletion from IndexedDB —
                // there is no tracked file to stop tracking.
                #[cfg(target_arch = "wasm32")]
                self.delete_browser_triangulation(&path);
                #[cfg(not(target_arch = "wasm32"))]
                self.remove_triangulation(path);
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            UiCommand::DownloadStoredTriangulation(path) => {
                self.download_stored_browser_triangulation(&path);
                Ok(())
            }
            UiCommand::RemoveTriangulationFolder(dir) => {
                self.remove_triangulation_folder(dir);
                Ok(())
            }
            UiCommand::ActivateTriangulation(id) => {
                self.activate_triangulation(id);
                Ok(())
            }
            UiCommand::ToggleTriangulationVisible(id) => {
                self.toggle_triangulation_visible(id);
                Ok(())
            }
            UiCommand::CloseTriangulation(id) => {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(tri) = self.triangulations.iter().find(|t| t.id == id)
                    && !tri.is_saved
                {
                    self.editor.tri_close_unsaved = Some(id);
                    return Ok(());
                }
                // On the browser, unloading is reversible only once the mesh
                // has reached IndexedDB — until then there is nothing to load
                // it back from, so wait rather than prompt.
                #[cfg(target_arch = "wasm32")]
                if let Some(tri) = self.triangulations.iter().find(|t| t.id == id)
                    && !tri.is_saved
                {
                    userspace_warn!("Wait for '{}' to finish saving to browser storage before unloading it", tri.name);
                    return Ok(());
                }
                self.close_triangulation(id);
                Ok(())
            }
            UiCommand::CloseTriangulationForce(id) => {
                self.editor.tri_close_unsaved = None;
                self.close_triangulation(id);
                Ok(())
            }
            UiCommand::CommitTextEdit(object_id, content, height, rotation_degrees, color) => {
                self.commit_text_edit(object_id, content, height, rotation_degrees, color);
                Ok(())
            }
            UiCommand::CancelTextEdit => {
                self.cancel_text_edit();
                Ok(())
            }
            UiCommand::BatchSetObjectColor(ids, new_color) => {
                self.batch_set_object_color(ids, new_color);
                Ok(())
            }
            UiCommand::BatchSetPolylineClosed(ids, closed) => {
                self.batch_set_polyline_closed(ids, closed);
                Ok(())
            }
            UiCommand::BatchSetObjectFill(ids, new_fill) => {
                self.batch_set_object_fill(ids, new_fill);
                Ok(())
            }
            UiCommand::BatchSetPolylineLineWeight(ids, weight) => {
                self.batch_set_polyline_line_weight(ids, weight);
                Ok(())
            }
            UiCommand::BatchSetZValue(ids, weight) => {
                self.batch_set_z_value(ids, weight);
                Ok(())
            }
            UiCommand::MoveObjectsToLayer { object_ids, target_layer, copy } => {
                self.move_objects_to_layer(object_ids, target_layer, copy);
                Ok(())
            }
            UiCommand::SetTriangulationColor(tri_id, new_color) => {
                self.set_triangulation_color(tri_id, new_color);
                Ok(())
            }
            UiCommand::CloseCanvasContextMenu => {
                self.editor.canvas_context_menu_open = false;
                Ok(())
            }
            UiCommand::ZoomToExtents => {
                self.set_slice_mode_enabled(false);
                self.zoom_to_extents();
                Ok(())
            }
            UiCommand::BeginOffsetPick {
                object_ids,
                horiz_dist,
                z_delta,
                project_to_rl,
                collide_with_triangulation,
            } => {
                self.begin_offset_pick(object_ids, horiz_dist, z_delta, project_to_rl, collide_with_triangulation);
                Ok(())
            }
            UiCommand::RelimitLineResize { source_id, mode, value } => {
                self.relimit_resize(source_id, mode, value);
                Ok(())
            }
            UiCommand::OpenOffsetDialog => {
                self.open_offset_dialog();
                Ok(())
            }
            UiCommand::OpenRelimitDialog => {
                self.open_relimit_dialog();
                Ok(())
            }
            UiCommand::OpenBatterBermDialog => {
                self.open_batter_berm_dialog();
                Ok(())
            }
            UiCommand::OpenSetSelectionZValueDialog => {
                let selected_objects: Vec<crate::model::ObjectId> = self
                    .editor
                    .selected_handles
                    .iter()
                    .filter_map(|&handle| match handle {
                        crate::model::SceneEntityId::Object(id) => Some(id),
                        crate::model::SceneEntityId::Triangulation(_) | crate::model::SceneEntityId::BlockModel(_) => None,
                    })
                    .collect();

                if selected_objects.is_empty() {
                    userspace_warn!("Select one or more objects before setting Z");
                    return Ok(());
                }

                self.editor.set_selection_z_dialog = Some(crate::ui::dialogs::SetSelectionZDialog {
                    object_ids: selected_objects,
                    z_input: self.editor.z_input,
                });
                Ok(())
            }
            UiCommand::InsertPointsAtIntersections => {
                self.insert_points_at_selected_intersections();
                Ok(())
            }
            UiCommand::OpenInsertPointAtElevationDialog => {
                self.open_insert_point_at_elevation_dialog();
                Ok(())
            }
            UiCommand::InsertPointsAtElevation { object_ids, elevation } => {
                self.insert_points_at_elevation(object_ids, elevation);
                Ok(())
            }
            UiCommand::CommitBatterBerm => {
                self.commit_batter_berm();
                Ok(())
            }
            UiCommand::CancelBatterBerm => {
                self.cancel_batter_berm();
                Ok(())
            }
            UiCommand::OpenCreateTriangulation => {
                self.editor.tri_create_open = true;
                self.editor.tri_create_phase = TriCreatePhase::MainDialog;
                self.editor.selected_handles.clear();
                self.editor.tri_selected_object_ids.clear();
                self.editor.tri_selected_layer_ids.clear();
                self.editor.tri_name_input = "surface".to_owned();
                self.editor.tri_hover_handles.clear();
                Ok(())
            }
            UiCommand::ExecuteCreateTriangulation { name, object_ids, surface_type } => self.run_create_triangulation(name, object_ids, surface_type, false, false),
            UiCommand::ExecuteCreateTriangulationWithWeld { name, object_ids, surface_type } => self.run_create_triangulation(name, object_ids, surface_type, true, false),
            UiCommand::ExecuteCreateTriangulationUpperSurface {
                name,
                object_ids,
                surface_type,
                coarse_weld,
            } => self.run_create_triangulation(name, object_ids, surface_type, coarse_weld, true),
            UiCommand::OpenPointCloudTin => {
                self.editor.point_cloud_tin_open = true;
                // Default to the only loaded cloud, or keep a still-valid pick.
                let still_loaded = self.editor.point_cloud_tin_cloud_id.is_some_and(|id| self.point_clouds.iter().any(|cloud| cloud.id == id));
                if !still_loaded {
                    self.editor.point_cloud_tin_cloud_id = self.point_clouds.first().map(|cloud| cloud.id);
                }
                // Keep any name the user already typed; otherwise restore the
                // default rather than opening with an empty, un-runnable field.
                if self.editor.point_cloud_tin_name_input.trim().is_empty() {
                    self.editor.point_cloud_tin_name_input = "Surface".to_owned();
                }
                Ok(())
            }
            UiCommand::ExecutePointCloudTin { cloud_id, params } => self.run_point_cloud_tin(cloud_id, params),
            UiCommand::ConfirmLoadAllTriangulationsInFolder(path) => {
                self.editor.confirm_load_all_folder = Some(path);
                Ok(())
            }
            UiCommand::LoadAllTriangulationsInFolder(path) => self.load_all_triangulations_in_folder(path),
            UiCommand::CloseAllTriangulationsInFolder(path) => {
                self.close_all_triangulations_in_folder(path);
                Ok(())
            }
            UiCommand::OpenCutTriangulationByPolygon => {
                self.editor.tri_cut_poly_open = true;
                self.editor.tri_cut_poly_name_auto = true;
                self.editor.tri_cut_poly_awaiting_pick = false;
                self.editor.viewport_pick_hover_label = None;
                self.editor.tri_hover_handles.clear();
                self.editor.tri_cut_poly_tri_id = self.active_triangulation;
                self.editor.tri_cut_poly_object_id = None;
                self.editor.tri_cut_poly_object_name = String::new();
                self.editor.tri_cut_poly_mode = crate::ui::state::TriPolygonClipMode::KeepInside;
                self.editor.tri_cut_poly_name_input = self
                    .active_triangulation
                    .and_then(|id| self.triangulations.iter().find(|t| t.id == id))
                    .map(|t| {
                        let stem = std::path::Path::new(&t.name).file_stem().and_then(|s| s.to_str()).unwrap_or(&t.name);
                        format!("{stem}_cut")
                    })
                    .unwrap_or_default();
                Ok(())
            }
            UiCommand::BeginCutPolyPick => {
                self.editor.tri_cut_poly_awaiting_pick = true;
                self.editor.viewport_pick_hover_label = None;
                self.editor.tool_highlight_id = None;
                self.invalidate_geometry();
                Ok(())
            }
            UiCommand::ExecuteCutTriangulationByPolygon { tri_id, polygon_id, mode, name } => {
                let result = self.cut_triangulation_by_polygon(tri_id, polygon_id, mode, name);
                if result.is_ok() {
                    self.editor.tri_cut_poly_open = false;
                    self.editor.tool_highlight_id = None;
                }
                result
            }
            UiCommand::OpenCutTriangulationByZ => {
                self.editor.tri_cut_z_open = true;
                self.editor.tri_cut_z_name_auto = true;
                self.editor.tri_cut_z_tri_id = self.active_triangulation;
                let (z_min, z_max) = self
                    .active_triangulation
                    .and_then(|id| self.triangulations.iter().find(|t| t.id == id))
                    .map(|t| {
                        let b = t.mesh.bounds();
                        (b.min.z, b.max.z)
                    })
                    .unwrap_or((0.0, 100.0));
                self.editor.tri_cut_z_min_input = z_min;
                self.editor.tri_cut_z_max_input = z_max;
                self.editor.tri_cut_z_name_input = self
                    .active_triangulation
                    .and_then(|id| self.triangulations.iter().find(|t| t.id == id))
                    .map(|t| {
                        let stem = std::path::Path::new(&t.name).file_stem().and_then(|s| s.to_str()).unwrap_or(&t.name);
                        format!("{stem}_slice")
                    })
                    .unwrap_or_default();
                Ok(())
            }
            UiCommand::ExecuteCutTriangulationByZ { tri_id, z_min, z_max, name } => {
                let result = self.cut_triangulation_by_z(tri_id, z_min, z_max, name);
                if result.is_ok() {
                    self.editor.tri_cut_z_open = false;
                }
                result
            }
            UiCommand::OpenCutTriangulationBySurface => {
                self.editor.tri_cut_surface_open = true;
                self.editor.tri_cut_surface_name_auto = true;
                // Match the other topology tools: the active triangulation is
                // the topology, and the surface that will be changed is chosen
                // explicitly second.
                self.editor.tri_cut_surface_reference_id = self.active_triangulation;
                self.editor.tri_cut_surface_target_id = None;
                self.editor.tri_cut_surface_side = crate::ui::state::TriSurfaceCutSide::CutTop;
                self.editor.tri_cut_surface_name_input.clear();
                Ok(())
            }
            UiCommand::ExecuteCutTriangulationBySurface {
                target_id,
                reference_id,
                side,
                name,
            } => {
                let result = self.cut_triangulation_by_surface(target_id, reference_id, side, name);
                if result.is_ok() {
                    self.editor.tri_cut_surface_open = false;
                }
                result
            }
            UiCommand::OpenCutTopologyByPitShell => {
                self.editor.tri_cut_pitshell_open = true;
                self.editor.tri_cut_pitshell_name_auto = true;
                self.editor.tri_cut_pitshell_topology_id = self.active_triangulation;
                self.editor.tri_cut_pitshell_pitshell_id = None;
                self.editor.tri_cut_pitshell_name_input = self
                    .active_triangulation
                    .and_then(|id| self.triangulations.iter().find(|t| t.id == id))
                    .map(|t| {
                        let stem = std::path::Path::new(&t.name).file_stem().and_then(|s| s.to_str()).unwrap_or(&t.name);
                        format!("{stem}_cut")
                    })
                    .unwrap_or_default();
                Ok(())
            }
            UiCommand::ExecuteCutTopologyByPitShell { topology_id, pit_shell_id, name } => {
                let result = self.cut_topology_by_pit_shell(topology_id, pit_shell_id, name);
                if result.is_ok() {
                    self.editor.tri_cut_pitshell_open = false;
                }
                result
            }
            UiCommand::OpenIncludeSolidInTopology => {
                self.editor.tri_include_solid_open = true;
                self.editor.tri_include_solid_name_auto = true;
                self.editor.tri_include_solid_topology_id = self.active_triangulation;
                self.editor.tri_include_solid_shape_id = None;
                self.editor.tri_include_solid_save_as_two = false;
                self.editor.tri_include_solid_name_input = self
                    .active_triangulation
                    .and_then(|id| self.triangulations.iter().find(|triangulation| triangulation.id == id))
                    .map(|triangulation| {
                        let stem = std::path::Path::new(&triangulation.name)
                            .file_stem()
                            .and_then(|value| value.to_str())
                            .unwrap_or(&triangulation.name);
                        format!("{stem}_with_shape")
                    })
                    .unwrap_or_default();
                Ok(())
            }
            UiCommand::ExecuteIncludeSolidInTopology {
                topology_id,
                shape_id,
                name,
                save_as_two,
            } => {
                let result = self.include_solid_in_topology(topology_id, shape_id, name, save_as_two);
                if result.is_ok() {
                    self.editor.tri_include_solid_open = false;
                }
                result
            }
            UiCommand::OpenContourTriangulation => {
                self.editor.tri_contour_open = true;
                self.editor.tri_contour_tri_id = self.active_triangulation;
                self.editor.tri_contour_target_layer = None;
                self.editor.tri_contour_layer_name_auto = true;
                let surface_name = self.active_triangulation.and_then(|id| {
                    self.triangulations
                        .iter()
                        .find(|triangulation| triangulation.id == id)
                        .map(|triangulation| triangulation.name.clone())
                });
                if let Some(surface_name) = surface_name {
                    self.editor.update_contour_layer_name_from_surface(&surface_name);
                } else {
                    self.editor.tri_contour_layer_name_input = "surface_contour".to_owned();
                }
                Ok(())
            }
            UiCommand::ExecuteContourTriangulation {
                tri_id,
                major_interval,
                minor_interval,
                major_color,
                minor_color,
                z_range,
                output_layer,
            } => {
                let result = self.generate_contour_triangulation(tri_id, major_interval, minor_interval, major_color, minor_color, z_range, output_layer);
                if result.is_ok() {
                    self.editor.tri_contour_open = false;
                }
                result
            }
            UiCommand::PreviewMoveDelta(delta) => {
                self.ensure_move_session_original();
                self.preview_move_delta(delta);
                self.invalidate_geometry();
                Ok(())
            }
            UiCommand::ApplyChamfer => {
                self.apply_chamfer();
                Ok(())
            }
            UiCommand::CancelChamfer => {
                self.cancel_chamfer();
                Ok(())
            }
            UiCommand::ApplyBezier => {
                self.apply_bezier();
                Ok(())
            }
            UiCommand::CancelBezier => {
                self.cancel_bezier();
                Ok(())
            }
            UiCommand::ApplyMoveDelta(delta) => {
                self.apply_move_delta(delta);
                Ok(())
            }
            UiCommand::CancelMoveDelta => {
                self.cancel_move_delta();
                Ok(())
            }
            UiCommand::ConfirmDeleteSelection => {
                if self.editor.active_tool == crate::ui::state::ActiveTool::Move {
                    self.cancel_move_delta();
                }
                self.delete_selection();
                Ok(())
            }
            UiCommand::OpenPlotDialog => {
                self.open_plot_dialog();
                Ok(())
            }
            UiCommand::FitPlotScaleToData => self.fit_plot_scale_to_data(),
            UiCommand::ExportPlotSheet => self.choose_plot_sheet_destination(),
            UiCommand::Undo => {
                self.apply_history_step(true);
                Ok(())
            }
            UiCommand::Redo => {
                self.apply_history_step(false);
                Ok(())
            }
        }
    }
}
