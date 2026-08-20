//! Top application menu bar (File, Design, View, Analyse, Open Pit, Triangulation).

use crate::ui::{EditorState, UiCommand, UiProjectView, state::UiProjectEntry};

/// Draw the top menu bar panel.
///
/// Returns the panel's bounding rect.
pub(crate) fn draw_main_menu(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::top("main_menu")
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    let has_unsaved = project.projects.iter().any(UiProjectEntry::needs_save);
                    if ui.add_enabled(has_unsaved, egui::Button::new("Save All")).clicked() {
                        commands.push(UiCommand::SaveAllPidbs);
                        ui.close();
                    }
                    #[cfg(target_arch = "wasm32")]
                    if ui.add_enabled(!project.projects.is_empty(), egui::Button::new("Download All PIDBs")).clicked() {
                        commands.push(UiCommand::DownloadAllPidbs);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("New PIDB...").clicked() {
                        commands.push(UiCommand::NewPidb);
                        ui.close();
                    }
                    if ui.button("Open PIDBs...").clicked() {
                        commands.push(UiCommand::OpenPidb);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Import...").clicked() {
                        editor.show_import = true;
                        editor.show_export = false;
                        ui.close();
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button("Add Triangulation Folder...").clicked() {
                        commands.push(UiCommand::OpenTriangulationFolder);
                        ui.close();
                    }
                    if ui.button("Export...").clicked() {
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
                    if ui.button("Preferences...").clicked() {
                        commands.push(UiCommand::OpenPreferences);
                        ui.close();
                    }
                    if ui.button(format!("Exit {}", crate::APP_NAME)).clicked() {
                        commands.push(UiCommand::RequestExit);
                        ui.close();
                    }
                });

                ui.menu_button("View", |ui| {
                    let mut show_xy_grid = editor.show_xy_grid;
                    if ui.checkbox(&mut show_xy_grid, "Show XY Grid").changed() {
                        commands.push(UiCommand::SetShowXyGrid(show_xy_grid));
                    }

                    let mut show_scale_bar = editor.show_scale_bar;
                    if ui.checkbox(&mut show_scale_bar, "Show Scale Bar").changed() {
                        commands.push(UiCommand::SetShowScaleBar(show_scale_bar));
                    }

                    let mut dark_mode = editor.dark_mode;
                    if ui.checkbox(&mut dark_mode, "Dark Mode").changed() {
                        commands.push(UiCommand::SetDarkMode(dark_mode));
                    }

                    let mut show_console = editor.show_console;
                    if ui.checkbox(&mut show_console, "Show Console").changed() {
                        commands.push(UiCommand::SetShowConsole(show_console));
                    }
                });

                ui.menu_button("Object", |ui| {
                    ui.menu_button("Insert Point", |ui| {
                        if ui.button("At intersection").clicked() {
                            commands.push(UiCommand::InsertPointsAtIntersections);
                            ui.close();
                        }
                        if ui.button("At elevation...").clicked() {
                            commands.push(UiCommand::OpenInsertPointAtElevationDialog);
                            ui.close();
                        }
                    });
                    ui.separator();
                    if ui.button("Set Selection Z Value...").clicked() {
                        commands.push(UiCommand::OpenSetSelectionZValueDialog);
                        ui.close();
                    }
                });

                ui.menu_button("Triangulation", |ui| {
                    if ui.button("Create Triangulation...").clicked() {
                        commands.push(UiCommand::OpenCreateTriangulation);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Clip Surface by Polygon...").clicked() {
                        commands.push(UiCommand::OpenCutTriangulationByPolygon);
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

                ui.menu_button("Survey", |ui| {
                    if ui
                        .add_enabled(project.point_clouds.iter().any(|cloud| cloud.is_loaded), egui::Button::new("Generate Terrain TIN..."))
                        .clicked()
                    {
                        commands.push(UiCommand::OpenPointCloudTin);
                        ui.close();
                    }
                });

                ui.menu_button("Geology", |ui| {
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
            });
        })
        .response
        .rect
}
