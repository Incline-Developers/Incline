pub(crate) mod block_model;
pub(crate) mod drawing; // Handles finishing polylines, creating points, etc commands
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
    ui::state::{TriCreatePhase, UiCommand},
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
            .flat_map(|project| project.project.document.layers())
            .map(|layer| layer.id)
            .collect();
        let changed = self.workspace.active_project_mut().is_some_and(|project| {
            if undo {
                self.history.undo(&mut project.project.document)
            } else {
                self.history.redo(&mut project.project.document)
            }
        });
        if !changed {
            return;
        }
        if let Some(project) = self.workspace.active_project_mut() {
            let layers_after: std::collections::HashSet<_> = project.project.document.layers().iter().map(|layer| layer.id).collect();
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
        let requires_project = matches!(
            &command,
            UiCommand::ImportOmfPaths(_)
                | UiCommand::ImportDxfPathsInto(_)
                | UiCommand::ImportTriangulationPaths(_)
                | UiCommand::ImportPointCloudPaths(_)
                | UiCommand::ImportRasterPaths(_)
                | UiCommand::ChooseImportSourceFiles(_)
                | UiCommand::ImportCsvBlockModel { .. }
                | UiCommand::ImportDrillHole(_)
                | UiCommand::CreateLayer { .. }
                | UiCommand::OpenCreateTriangulation
                | UiCommand::OpenCreateBlockModel(_)
                | UiCommand::OpenCreateOreTriangulation
        );
        if requires_project && !self.workspace.has_active_project() {
            anyhow::bail!("Create or open a project before importing, drawing, or generating data");
        }
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
            UiCommand::NewProject => {
                self.choose_new_project();
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            UiCommand::CreateBrowserProject { name } => self.create_browser_project(name),
            UiCommand::OpenProject => {
                self.choose_open_project();
                Ok(())
            }
            UiCommand::ActivateTrackedProject(project) => self.activate_tracked_project(project),
            UiCommand::RemoveTrackedProject(project) => self.remove_tracked_project(project),
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
            UiCommand::LoadRaster(id) => {
                if let Some(raster) = self.raster_textures.iter_mut().find(|raster| raster.id == id) {
                    raster.state.loaded = true;
                    self.redraw_requested = true;
                } else {
                    userspace_warn!("That raster no longer belongs to the active project");
                }
                Ok(())
            }
            UiCommand::UnloadRaster(id) => {
                self.unload_raster(id);
                Ok(())
            }
            UiCommand::ToggleRasterVisible(id) => {
                self.toggle_raster_visible(id);
                Ok(())
            }
            UiCommand::RemoveRaster(id) => {
                self.remove_raster(id);
                Ok(())
            }
            UiCommand::DrapeRaster(id) => self.drape_raster_over_surfaces(id),
            UiCommand::UndrapeRaster(id) => {
                self.undrape_raster(id);
                Ok(())
            }
            UiCommand::ClearActiveTriangulationRaster => self.clear_active_triangulation_raster(),
            UiCommand::LoadPointCloud(id) => {
                if let Some(cloud) = self.point_clouds.iter_mut().find(|cloud| cloud.id == id) {
                    cloud.state.loaded = true;
                    self.invalidate_topology_bounds_and_redraw();
                } else {
                    userspace_warn!("That point cloud no longer belongs to the active project");
                }
                Ok(())
            }
            UiCommand::ClosePointCloud(id) => {
                self.close_point_cloud(id);
                Ok(())
            }
            UiCommand::TogglePointCloudVisible(id) => {
                self.toggle_point_cloud_visible(id);
                Ok(())
            }
            UiCommand::RemovePointCloud(id) => {
                self.remove_point_cloud(id);
                Ok(())
            }
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
                self.import_block_model_source(crate::model::block_model::BlockModelSource { path, csv_columns: Some(mapping) })
            }
            UiCommand::ExportOmf => self.choose_export_omf(),
            UiCommand::ExportProjectDxf(runtime_id) => {
                self.choose_export_project_dxf(runtime_id);
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
            UiCommand::HideSelection => {
                self.hide_selected_elements();
                Ok(())
            }
            UiCommand::RevealAllElements => {
                if let Some(document) = self.workspace.active_document_mut() {
                    document.reveal_all_objects();
                }
                let mut asset_changed = false;
                for tri in &mut self.triangulations {
                    if !tri.visible {
                        tri.visible = true;
                        tri.state.touch();
                        asset_changed = true;
                    }
                }
                for model in &mut self.block_models {
                    if !model.visible {
                        model.visible = true;
                        model.state.touch();
                        asset_changed = true;
                    }
                }
                for dataset in &mut self.drill_holes {
                    if !dataset.visible {
                        dataset.visible = true;
                        dataset.state.touch();
                        asset_changed = true;
                    }
                }
                for cloud in &mut self.point_clouds {
                    if !cloud.visible {
                        cloud.visible = true;
                        cloud.state.touch();
                        asset_changed = true;
                    }
                }
                if asset_changed {
                    self.touch_active_project_content();
                }
                self.editor.hidden_handles.clear();
                self.editor.frozen_handles.clear();
                self.editor.translucent_handles.clear();
                self.invalidate_geometry();
                Ok(())
            }
            UiCommand::RequestExit => self.request_exit(),
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
                    .and_then(|project| project.project.document.layer(layer_id))
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
            UiCommand::BeginRenameLayer(layer_id) => {
                self.activate_project_for_layer(layer_id);
                let current_name = self
                    .workspace
                    .active_project()
                    .and_then(|p| p.project.document.layer(layer_id))
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
                self.editor.renaming_layer = Some((layer_id, current_name));
                Ok(())
            }
            UiCommand::RenameLayer { layer_id, new_name } => {
                self.activate_project_for_layer(layer_id);
                let Some((before, is_loaded)) = self.workspace.active_project().and_then(|project| {
                    project
                        .project
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
                                &mut project.project.document,
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
                            project.project.document.rename_layer(layer_id, new_name);
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
            UiCommand::SaveProject => self.save_dirty_project().map(|_| ()),
            UiCommand::SaveAndReplaceProject => self.save_and_continue_project_replacement(),
            UiCommand::DiscardAndReplaceProject => self.discard_and_continue_project_replacement(),
            UiCommand::CancelProjectReplacement => {
                self.cancel_project_replacement();
                Ok(())
            }
            UiCommand::ConfirmLossyProjectSave => self.confirm_lossy_project_save(),
            UiCommand::CancelLossyProjectSave => {
                self.cancel_lossy_project_save();
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            UiCommand::DownloadProject => self.download_project(),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::SaveProjectAs(runtime_id) => {
                self.spawn_save_project_as_dialog(runtime_id);
                Ok(())
            }
            UiCommand::CloseProject(runtime_id) => {
                self.editor.remove_project_after_close = false;
                self.request_close_project(runtime_id);
                Ok(())
            }
            UiCommand::SaveAndCloseProject(runtime_id) => self.save_and_close_project(runtime_id),
            UiCommand::CloseProjectForce(runtime_id) => {
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
            UiCommand::CancelCloseProject => {
                self.cancel_close_project();
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::DiscardProjectChanges(runtime_id) => self.discard_project_changes(runtime_id),
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::RequestDiscardLayerChanges(layer_id) => {
                self.request_discard_layer_changes(layer_id);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            UiCommand::DiscardLayerChanges(layer_id) => self.discard_layer_changes(layer_id),
            UiCommand::LoadTriangulation(id) => {
                if let Some(triangulation) = self.triangulations.iter_mut().find(|triangulation| triangulation.id == id) {
                    triangulation.state.loaded = true;
                    self.invalidate_topology_bounds_and_redraw();
                } else {
                    userspace_warn!("That triangulation no longer belongs to the active project");
                }
                Ok(())
            }
            UiCommand::LoadBlockModel(id) => {
                if let Some(model) = self.block_models.iter_mut().find(|model| model.id == id) {
                    model.state.loaded = true;
                    self.invalidate_topology_bounds_and_redraw();
                } else {
                    userspace_warn!("That block model no longer belongs to the active project");
                }
                Ok(())
            }
            UiCommand::CloseBlockModel(id) => {
                self.close_block_model(id);
                Ok(())
            }
            UiCommand::RemoveBlockModel(id) => {
                self.remove_block_model(id);
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
            UiCommand::SetBlockModelColorTransfer { id, transfer } => {
                self.set_block_model_color_transfer(id, transfer);
                Ok(())
            }
            UiCommand::ResetBlockModelColorTransfer { id } => {
                self.reset_block_model_color_transfer(id);
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
            UiCommand::LoadDrillHole(id) => {
                if let Some(dataset) = self.drill_holes.iter_mut().find(|dataset| dataset.id == id) {
                    dataset.state.loaded = true;
                    self.invalidate_topology_bounds_and_redraw();
                } else {
                    userspace_warn!("That drillhole dataset no longer belongs to the active project");
                }
                Ok(())
            }
            UiCommand::CloseDrillHole(id) => {
                self.close_drill_hole(id);
                Ok(())
            }
            UiCommand::RemoveDrillHole(id) => {
                self.remove_drill_hole(id);
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
            UiCommand::SelectBlockModel(id) => {
                self.select_block_model(id);
                Ok(())
            }
            UiCommand::RemoveTriangulation(id) => {
                self.remove_triangulation(id);
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
            UiCommand::BatchSetAxisValue(ids, axis, value) => {
                self.batch_set_axis_value(ids, axis, value);
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
            UiCommand::OpenMoveToAxisDialog(axis) => {
                let selected_objects: Vec<crate::model::ObjectId> = self
                    .editor
                    .selected_handles
                    .iter()
                    .filter_map(|&handle| match handle {
                        crate::model::SceneEntityId::Object(id) => Some(id),
                        _ => None,
                    })
                    .collect();

                if selected_objects.is_empty() {
                    userspace_warn!("Select one or more objects before setting {}", axis.label());
                    return Ok(());
                }

                // Seed Z from the toolbar plane, X and Y from the first selected object.
                let value = if axis == crate::model::Axis::Z {
                    self.editor.z_input
                } else {
                    self.workspace
                        .active_project()
                        .and_then(|project| project.project.document.get_object(selected_objects[0]))
                        .map_or(0.0, |object| object.axis_position(axis))
                };

                self.editor.move_to_axis_dialog = Some(crate::ui::dialogs::MoveToAxisDialog {
                    object_ids: selected_objects,
                    axis,
                    value,
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
            UiCommand::OpenCutTriangulationByPolyline => {
                self.editor.tri_cut_poly_open = true;
                self.editor.tri_cut_poly_name_auto = true;
                self.editor.tri_cut_poly_awaiting_pick = false;
                self.editor.viewport_pick_hover_label = None;
                self.editor.tri_hover_handles.clear();
                self.editor.tri_cut_poly_tri_id = self.active_triangulation;
                self.editor.tri_cut_poly_object_id = None;
                self.editor.tri_cut_poly_object_name = String::new();
                self.editor.tri_cut_poly_mode = crate::ui::state::TriPolylineClipMode::KeepInside;
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
            UiCommand::ExecuteCutTriangulationByPolyline { tri_id, polyline_id, mode, name } => {
                let result = self.cut_triangulation_by_polyline(tri_id, polyline_id, mode, name);
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

    fn hide_selected_elements(&mut self) {
        let selected = self.editor.selected_handles.clone();
        let object_ids = selected
            .iter()
            .filter_map(|handle| match handle {
                crate::model::SceneEntityId::Object(id) => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        if let Some(document) = self.workspace.active_document_mut() {
            for id in object_ids {
                changed |= document.set_object_hidden(id, true);
            }
        }

        let mut asset_changed = false;
        for triangulation in &mut self.triangulations {
            if selected.contains(&triangulation.entity_id()) && triangulation.visible {
                triangulation.visible = false;
                triangulation.state.touch();
                asset_changed = true;
            }
        }
        for model in &mut self.block_models {
            if selected.contains(&model.entity_id()) && model.visible {
                model.visible = false;
                model.state.touch();
                asset_changed = true;
            }
        }
        for dataset in &mut self.drill_holes {
            if selected.contains(&dataset.entity_id()) && dataset.visible {
                dataset.visible = false;
                dataset.state.touch();
                asset_changed = true;
            }
        }
        for cloud in &mut self.point_clouds {
            if selected.contains(&cloud.entity_id()) && cloud.visible {
                cloud.visible = false;
                cloud.state.touch();
                asset_changed = true;
            }
        }
        if asset_changed {
            self.touch_active_project_content();
        }
        changed |= asset_changed;

        // Persisted visibility now owns ordinary Hide Selection. Remove any
        // matching legacy transient overrides so save/reopen has one source of
        // truth for the same action.
        self.editor.hidden_handles.retain(|handle| !selected.contains(handle));
        if changed {
            self.invalidate_geometry();
        }
    }
}
