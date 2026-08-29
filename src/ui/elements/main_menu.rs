//! Top application menu bar (File, Project, Design, Triangulation, Raster, Point Cloud, Block Model, Drill Holes).

use crate::{
    model::{Axis, SceneEntityId},
    ui::{
        EditorState, UiCommand, UiProjectView,
        state::UiProjectEntry,
        widgets::context_menu::{ContextMenuAction, MenuBarMenu, context_menu_separator, context_submenu},
    },
};

/// Draw the top menu bar panel.
///
/// Dropdowns are the same widget as the right-click menus - see
/// [`crate::ui::widgets::context_menu`] - so rows, submenus and separators all
/// come from there rather than from egui's default menu style.
///
/// Returns the panel's bounding rect.
pub(crate) fn draw_main_menu(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::top("main_menu")
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                MenuBarMenu::new("File").show(ui, |ui| {
                    let has_unsaved = project.projects.iter().any(UiProjectEntry::needs_save);
                    let active_project = project.projects.iter().find(|entry| entry.is_active);
                    if ContextMenuAction::new("Save Project").enabled(has_unsaved).show(ui).clicked() {
                        commands.push(UiCommand::SaveProject);
                        ui.close();
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ContextMenuAction::new("Save Project As...").enabled(active_project.is_some()).show(ui).clicked() {
                        if let Some(project) = active_project {
                            commands.push(UiCommand::SaveProjectAs(project.runtime_id));
                        }
                        ui.close();
                    }
                    #[cfg(target_arch = "wasm32")]
                    if ContextMenuAction::new("Download OMF").enabled(!project.projects.is_empty()).show(ui).clicked() {
                        commands.push(UiCommand::DownloadProject);
                        ui.close();
                    }
                    context_menu_separator(ui);
                    if ContextMenuAction::new("New project...").show(ui).clicked() {
                        commands.push(UiCommand::NewProject);
                        ui.close();
                    }
                    if ContextMenuAction::new("Open project...").show(ui).clicked() {
                        commands.push(UiCommand::OpenProject);
                        ui.close();
                    }
                    if ContextMenuAction::new("Close Project").enabled(active_project.is_some()).show(ui).clicked() {
                        if let Some(project) = active_project {
                            commands.push(UiCommand::CloseProject(project.runtime_id));
                        }
                        ui.close();
                    }
                    context_menu_separator(ui);
                    if ContextMenuAction::new("Import...").enabled(active_project.is_some()).show(ui).clicked() {
                        editor.show_import = true;
                        editor.show_export = false;
                        ui.close();
                    }
                    if ContextMenuAction::new("Export...").enabled(active_project.is_some()).show(ui).clicked() {
                        editor.show_import = false;
                        editor.show_export = true;
                        ui.close();
                    }
                    if ContextMenuAction::new("Export Viewport Image...").show(ui).clicked() {
                        commands.push(UiCommand::ExportViewportImage);
                        ui.close();
                    }
                    if ContextMenuAction::new("Export Engineering Drawing...").show(ui).clicked() {
                        commands.push(UiCommand::OpenPlotDialog);
                        ui.close();
                    }
                    context_menu_separator(ui);
                    if ContextMenuAction::new(format!("About {}...", crate::APP_NAME)).show(ui).clicked() {
                        editor.show_about = true;
                        ui.close();
                    }
                    if ContextMenuAction::new(format!("Exit {}", crate::APP_NAME)).show(ui).clicked() {
                        commands.push(UiCommand::RequestExit);
                        ui.close();
                    }
                });

                MenuBarMenu::new("Project").show(ui, |ui| {
                    // Deactivating a dirty project routes through the save/discard/cancel dialog.
                    let active_project = project.projects.iter().find(|entry| entry.is_active);
                    if ContextMenuAction::new("Deactivate Current Project").enabled(active_project.is_some()).show(ui).clicked() {
                        if let Some(project) = active_project {
                            commands.push(UiCommand::CloseProject(project.runtime_id));
                        }
                        ui.close();
                    }
                });

                MenuBarMenu::new("Design").show(ui, |ui| {
                    // Every entry here acts on the current design selection.
                    let has_selection = editor.selected_handles.iter().any(|handle| matches!(handle, SceneEntityId::Object(_)));
                    context_submenu(ui, "Insert Point", has_selection, |ui| {
                        // Needs two or more crossing polylines to insert anything.
                        if ContextMenuAction::new("At intersection").enabled(editor.selection_has_intersections).show(ui).clicked() {
                            commands.push(UiCommand::InsertPointsAtIntersections);
                            ui.close();
                        }
                        if ContextMenuAction::new("At elevation...").show(ui).clicked() {
                            commands.push(UiCommand::OpenInsertPointAtElevationDialog);
                            ui.close();
                        }
                    });
                    context_menu_separator(ui);
                    context_submenu(ui, "Move to", has_selection, |ui| {
                        for axis in [Axis::X, Axis::Y, Axis::Z] {
                            if ContextMenuAction::new(format!("Set {}...", axis.label())).show(ui).clicked() {
                                commands.push(UiCommand::OpenMoveToAxisDialog(axis));
                                ui.close();
                            }
                        }
                    });
                    context_menu_separator(ui);
                    // Unlike the entries above this one runs with nothing
                    // selected: the dialog seeds from the selection when there
                    // is one, and otherwise you pick in the viewport with it open.
                    if ContextMenuAction::new("Create Triangulation...").show(ui).clicked() {
                        commands.push(UiCommand::OpenCreateTriangulation);
                        ui.close();
                    }
                });

                MenuBarMenu::new("Triangulation").show(ui, |ui| {
                    if ContextMenuAction::new("Clip Surface by Polyline...").show(ui).clicked() {
                        commands.push(UiCommand::OpenCutTriangulationByPolyline);
                        ui.close();
                    }
                    if ContextMenuAction::new("Slice Triangulation by Z Range...").show(ui).clicked() {
                        commands.push(UiCommand::OpenCutTriangulationByZ);
                        ui.close();
                    }
                    if ContextMenuAction::new("Trim to Topology...").show(ui).clicked() {
                        commands.push(UiCommand::OpenCutTriangulationBySurface);
                        ui.close();
                    }
                    context_menu_separator(ui);
                    if ContextMenuAction::new("Cut Topology with Pit Shell...").show(ui).clicked() {
                        commands.push(UiCommand::OpenCutTopologyByPitShell);
                        ui.close();
                    }
                    if ContextMenuAction::new("Merge Shell into Topology...").show(ui).clicked() {
                        commands.push(UiCommand::OpenIncludeSolidInTopology);
                        ui.close();
                    }
                    context_menu_separator(ui);
                    if ContextMenuAction::new("Generate Contour Lines...").show(ui).clicked() {
                        commands.push(UiCommand::OpenContourTriangulation);
                        ui.close();
                    }
                });

                MenuBarMenu::new("Raster").show(ui, |ui| {
                    let any_draped = project.raster_textures.iter().any(|raster| raster.is_draped);
                    if ContextMenuAction::new("Undrape All").enabled(any_draped).show(ui).clicked() {
                        commands.push(UiCommand::UndrapeAllRasters);
                        ui.close();
                    }
                });

                MenuBarMenu::new("Point Cloud").show(ui, |ui| {
                    let has_loaded_cloud = project.point_clouds.iter().any(|cloud| cloud.is_loaded);
                    if ContextMenuAction::new("Create Triangulation...").enabled(has_loaded_cloud).show(ui).clicked() {
                        commands.push(UiCommand::OpenPointCloudTin);
                        ui.close();
                    }
                });

                MenuBarMenu::new("Block Model").show(ui, |ui| {
                    if ContextMenuAction::new("Create Ore Triangulation...")
                        .enabled(!project.block_models.is_empty())
                        .show(ui)
                        .clicked()
                    {
                        commands.push(UiCommand::OpenCreateOreTriangulation);
                        ui.close();
                    }
                });

                MenuBarMenu::new("Drill Holes").show(ui, |ui| {
                    let has_loaded_holes = project.drill_holes.iter().any(|dataset| dataset.is_loaded);
                    if ContextMenuAction::new("Create Block Model...").enabled(has_loaded_holes).show(ui).clicked() {
                        commands.push(UiCommand::OpenCreateBlockModel(None));
                        ui.close();
                    }
                });
            });
        })
        .response
        .rect
}
