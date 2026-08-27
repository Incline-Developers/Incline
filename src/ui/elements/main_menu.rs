//! Top application menu bar (File, Project, Design, Triangulation, Raster, Point Cloud, Block Model, Drill Holes).

use crate::{
    model::{Axis, SceneEntityId},
    ui::{EditorState, UiCommand, UiProjectView, state::UiProjectEntry},
};

/// Draw the top menu bar panel.
///
/// Returns the panel's bounding rect.
pub(crate) fn draw_main_menu(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::top("main_menu")
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    let has_unsaved = project.projects.iter().any(UiProjectEntry::needs_save);
                    let active_project = project.projects.iter().find(|entry| entry.is_active);
                    if ui.add_enabled(has_unsaved, egui::Button::new("Save Project")).clicked() {
                        commands.push(UiCommand::SaveProject);
                        ui.close();
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.add_enabled(active_project.is_some(), egui::Button::new("Save Project As...")).clicked() {
                        if let Some(project) = active_project {
                            commands.push(UiCommand::SaveProjectAs(project.runtime_id));
                        }
                        ui.close();
                    }
                    #[cfg(target_arch = "wasm32")]
                    if ui.add_enabled(!project.projects.is_empty(), egui::Button::new("Download OMF")).clicked() {
                        commands.push(UiCommand::DownloadProject);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("New project...").clicked() {
                        commands.push(UiCommand::NewProject);
                        ui.close();
                    }
                    if ui.button("Open project...").clicked() {
                        commands.push(UiCommand::OpenProject);
                        ui.close();
                    }
                    if ui.add_enabled(active_project.is_some(), egui::Button::new("Close Project")).clicked() {
                        if let Some(project) = active_project {
                            commands.push(UiCommand::CloseProject(project.runtime_id));
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.add_enabled(active_project.is_some(), egui::Button::new("Import...")).clicked() {
                        editor.show_import = true;
                        editor.show_export = false;
                        ui.close();
                    }
                    if ui.add_enabled(active_project.is_some(), egui::Button::new("Export...")).clicked() {
                        editor.show_import = false;
                        editor.show_export = true;
                        ui.close();
                    }
                    if ui.button("Export Viewport Image...").clicked() {
                        commands.push(UiCommand::ExportViewportImage);
                        ui.close();
                    }
                    if ui.button("Export Engineering Drawing...").clicked() {
                        commands.push(UiCommand::OpenPlotDialog);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(format!("About {}...", crate::APP_NAME)).clicked() {
                        editor.show_about = true;
                        ui.close();
                    }
                    if ui.button(format!("Exit {}", crate::APP_NAME)).clicked() {
                        commands.push(UiCommand::RequestExit);
                        ui.close();
                    }
                });

                ui.add_enabled_ui(false, |ui| {
                    ui.menu_button("Project", |_ui| {});
                });

                ui.menu_button("Design", |ui| {
                    // Every entry here acts on the current design selection.
                    let has_selection = editor.selected_handles.iter().any(|handle| matches!(handle, SceneEntityId::Object(_)));
                    ui.add_enabled_ui(has_selection, |ui| {
                        ui.menu_button("Insert Point", |ui| {
                            // Needs two or more crossing polylines to insert anything.
                            if ui.add_enabled(editor.selection_has_intersections, egui::Button::new("At intersection")).clicked() {
                                commands.push(UiCommand::InsertPointsAtIntersections);
                                ui.close();
                            }
                            if ui.button("At elevation...").clicked() {
                                commands.push(UiCommand::OpenInsertPointAtElevationDialog);
                                ui.close();
                            }
                        });
                        ui.separator();
                        ui.menu_button("Move to", |ui| {
                            for axis in [Axis::X, Axis::Y, Axis::Z] {
                                if ui.button(format!("Set {}...", axis.label())).clicked() {
                                    commands.push(UiCommand::OpenMoveToAxisDialog(axis));
                                    ui.close();
                                }
                            }
                        });
                    });
                });

                ui.menu_button("Triangulation", |ui| {
                    if ui.button("Create Triangulation...").clicked() {
                        commands.push(UiCommand::OpenCreateTriangulation);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Clip Surface by Polyline...").clicked() {
                        commands.push(UiCommand::OpenCutTriangulationByPolyline);
                        ui.close();
                    }
                    if ui.button("Slice Triangulation by Z Range...").clicked() {
                        commands.push(UiCommand::OpenCutTriangulationByZ);
                        ui.close();
                    }
                    if ui.button("Trim to Topology...").clicked() {
                        commands.push(UiCommand::OpenCutTriangulationBySurface);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Cut Topology with Pit Shell...").clicked() {
                        commands.push(UiCommand::OpenCutTopologyByPitShell);
                        ui.close();
                    }
                    if ui.button("Merge Shell into Topology...").clicked() {
                        commands.push(UiCommand::OpenIncludeSolidInTopology);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Generate Contour Lines...").clicked() {
                        commands.push(UiCommand::OpenContourTriangulation);
                        ui.close();
                    }
                });

                ui.add_enabled_ui(false, |ui| {
                    ui.menu_button("Raster", |_ui| {});
                });

                ui.menu_button("Point Cloud", |ui| {
                    if ui
                        .add_enabled(project.point_clouds.iter().any(|cloud| cloud.is_loaded), egui::Button::new("Generate Terrain TIN..."))
                        .clicked()
                    {
                        commands.push(UiCommand::OpenPointCloudTin);
                        ui.close();
                    }
                });

                ui.menu_button("Block Model", |ui| {
                    if ui
                        .add_enabled(project.drill_holes.iter().any(|dataset| dataset.is_loaded), egui::Button::new("Create Block Model..."))
                        .clicked()
                    {
                        commands.push(UiCommand::OpenCreateBlockModel(None));
                        ui.close();
                    }
                    if ui.add_enabled(!project.block_models.is_empty(), egui::Button::new("Create Ore Triangulation...")).clicked() {
                        commands.push(UiCommand::OpenCreateOreTriangulation);
                        ui.close();
                    }
                });

                ui.add_enabled_ui(false, |ui| {
                    ui.menu_button("Drill Holes", |_ui| {});
                });
            });
        })
        .response
        .rect
}
