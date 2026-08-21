//! Import & Export menus.

use crate::{
    model::{
        LayerId,
        block_model::BlockModelId,
        formats::{
            MeshFormat,
            csv_block_model::{CsvColumnRole, validate_mapping},
            csv_drill_hole::{CsvDrillColumnRole, CsvDrillFileRole},
        },
        triangulation::TriangulationId,
    },
    ui::{
        state::{DataMenu, EditorState, UiCommand, UiProjectView},
        widgets::menu::{DragableMenu, MenuFieldBool, MenuFieldCombo, MenuFieldFilePicker},
    },
};

const MENU_HEIGHT: f32 = 450.0;
const EXPLORER_WIDTH: f32 = 250.0;
const FIELD_WIDTH: f32 = 280.0;
const DETAILS_WIDTH: f32 = 680.0;
const MENU_WIDTH: f32 = EXPLORER_WIDTH + 4.0 + DETAILS_WIDTH;

fn draw_entry(ui: &mut egui::Ui, editor: &mut EditorState, title: &str, data_menu: DataMenu) -> egui::Response {
    let response = ui.add(
        egui::Button::new(title)
            .min_size(egui::Vec2::new(EXPLORER_WIDTH - 30., 25.0))
            .selected(editor.data_menu == data_menu),
    );
    if response.clicked() {
        editor.data_menu = data_menu;
    }
    response
}

pub(crate) fn draw_import_menu(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.show_import {
        return;
    }
    if !is_import_menu(editor.data_menu) {
        editor.data_menu = DataMenu::Dxf;
    }

    let mut show_import = editor.show_import;
    let mut close_after_action = false;
    DragableMenu::new("Import").open(&mut show_import).show(ui.ctx(), |ui| {
        ui.set_height(MENU_HEIGHT);
        ui.set_width(MENU_WIDTH);

        ui.horizontal(|ui| {
            draw_import_explorer(ui, editor);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.allocate_ui(egui::Vec2::new(DETAILS_WIDTH, MENU_HEIGHT - 25.0), |ui| {
                    ui.set_width(DETAILS_WIDTH);
                    ui.set_height(MENU_HEIGHT - 25.);
                    draw_import_details(ui, editor, project, commands);
                });
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(DETAILS_WIDTH, ui.spacing().interact_size.y),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let command = import_command(editor);
                        if ui.add_enabled(command.is_some(), egui::Button::new("Import")).clicked()
                            && let Some(command) = command
                        {
                            commands.push(command);
                            close_after_action = true;
                        }
                        if ui.button("Default").clicked() {
                            #[cfg(target_arch = "wasm32")]
                            let import_kind = editor.data_menu;
                            reset_import_defaults(editor, project);
                            #[cfg(target_arch = "wasm32")]
                            commands.push(UiCommand::ClearBrowserImportSelection(import_kind));
                        }
                    },
                );
            });
        });
    });
    if close_after_action {
        show_import = false;
    }
    editor.show_import = show_import;
}

pub(crate) fn draw_export_menu(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.show_export {
        return;
    }
    if !is_export_menu(editor.data_menu) {
        editor.data_menu = DataMenu::Dxf;
    }

    let mut show_export = editor.show_export;
    let mut close_after_action = false;
    DragableMenu::new("Export").open(&mut show_export).show(ui.ctx(), |ui| {
        ui.set_height(MENU_HEIGHT);
        ui.set_width(MENU_WIDTH);

        ui.horizontal(|ui| {
            draw_export_explorer(ui, editor);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.allocate_ui(egui::Vec2::new(DETAILS_WIDTH, MENU_HEIGHT - 25.0), |ui| {
                    ui.set_width(DETAILS_WIDTH);
                    ui.set_height(MENU_HEIGHT - 25.);
                    draw_export_details(ui, editor, project);
                });
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(DETAILS_WIDTH, ui.spacing().interact_size.y),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let command = export_command(editor);
                        if ui.add_enabled(command.is_some(), egui::Button::new("Export")).clicked()
                            && let Some(command) = command
                        {
                            commands.push(command);
                            close_after_action = true;
                        }
                        if ui.button("Default").clicked() {
                            reset_export_defaults(editor, project);
                        }
                    },
                );
            });
        });
    });
    if close_after_action {
        show_export = false;
    }
    editor.show_export = show_export;
}

fn draw_import_explorer(ui: &mut egui::Ui, editor: &mut EditorState) {
    ui.set_height(MENU_HEIGHT);
    ui.set_width(EXPLORER_WIDTH);
    egui::ScrollArea::new([false, true]).auto_shrink([false, false]).show(ui, |ui| {
        ui.vertical(|ui| {
            egui::CollapsingHeader::new("Interchange").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Open Mining Format (.omf)", DataMenu::Omf);
            });
            egui::CollapsingHeader::new("CAD").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Drawing Exchange Format (.dxf)", DataMenu::Dxf);
            });
            egui::CollapsingHeader::new("Triangulations").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Wavefront OBJ (.obj)", DataMenu::Obj);
                draw_entry(ui, editor, "STL (.stl)", DataMenu::Stl);
                draw_entry(ui, editor, "PLY (.ply)", DataMenu::Ply);
            });
            egui::CollapsingHeader::new("Point Clouds").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "LAS / LAZ (.las, .laz)", DataMenu::Las);
                draw_entry(ui, editor, "ASCII Points (.xyz, .pts)", DataMenu::Xyz);
                draw_entry(ui, editor, "Point Cloud Data (.pcd)", DataMenu::Pcd);
            });
            egui::CollapsingHeader::new("Block Models").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Comma-Separated Values (.csv)", DataMenu::CsvBlockModel);
            });
            egui::CollapsingHeader::new("Drill Holes").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Mapped CSV bundle (.csv)", DataMenu::CsvDrillHole);
            });
            egui::CollapsingHeader::new("Textures").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "GeoTIFF (.tif, .tiff)", DataMenu::Geotiff);
            });
        });
    });
}

fn draw_export_explorer(ui: &mut egui::Ui, editor: &mut EditorState) {
    ui.set_height(MENU_HEIGHT);
    ui.set_width(EXPLORER_WIDTH);
    egui::ScrollArea::new([false, true]).auto_shrink([false, false]).show(ui, |ui| {
        ui.vertical(|ui| {
            egui::CollapsingHeader::new("Interchange").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Open Mining Format (.omf)", DataMenu::Omf);
            });
            egui::CollapsingHeader::new("CAD").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Drawing Exchange Format (.dxf)", DataMenu::Dxf);
            });
            egui::CollapsingHeader::new("Triangulations").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Wavefront OBJ (.obj)", DataMenu::Obj);
                draw_entry(ui, editor, "STL (.stl)", DataMenu::Stl);
                draw_entry(ui, editor, "PLY (.ply)", DataMenu::Ply);
            });
            egui::CollapsingHeader::new("Block Models").default_open(true).show(ui, |ui| {
                draw_entry(ui, editor, "Comma-Separated Values (.csv)", DataMenu::CsvBlockModel);
            });
        });
    });
}

fn draw_import_details(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    // Scope ids by page so widgets on different pages that happen to share a
    // rect don't trip egui's id-stability check when switching pages.
    ui.push_id(editor.data_menu, |ui| match editor.data_menu {
        DataMenu::Omf => {
            ui.heading("Import Open Mining Format");
            draw_import_source_picker(ui, editor, commands, "Project", "No .omf chosen");
            ui.small("Imports every supported OMF element: designs and line sets, surfaces, block models, drillholes, point sets, and raster textures.");
        }
        DataMenu::Dxf => draw_import_dxf(ui, editor, project, commands),
        DataMenu::Obj => draw_import_mesh(ui, editor, commands, "Import Wavefront OBJ"),
        DataMenu::Stl => draw_import_mesh(ui, editor, commands, "Import STL"),
        DataMenu::Ply => draw_import_mesh(ui, editor, commands, "Import PLY"),
        DataMenu::Las => draw_import_mesh(ui, editor, commands, "Import LAS/LAZ Point Cloud"),
        DataMenu::Xyz => draw_import_mesh(ui, editor, commands, "Import ASCII Point Cloud"),
        DataMenu::Pcd => draw_import_mesh(ui, editor, commands, "Import PCD Point Cloud"),
        DataMenu::CsvBlockModel => draw_import_csv_block_model(ui, editor, commands),
        DataMenu::Geotiff => draw_import_mesh(ui, editor, commands, "Import GeoTIFF"),
        DataMenu::CsvDrillHole => draw_import_csv_drill_holes(ui, editor, commands),
        DataMenu::None => {}
    });
}

fn draw_export_details(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView) {
    ui.push_id(editor.data_menu, |ui| match editor.data_menu {
        DataMenu::Omf => draw_export_omf(ui, project),
        DataMenu::Dxf => draw_export_dxf(ui, editor, project),
        DataMenu::Obj => draw_export_mesh(ui, editor, project, "Export Wavefront OBJ"),
        DataMenu::Stl => draw_export_mesh(ui, editor, project, "Export STL"),
        DataMenu::Ply => draw_export_mesh(ui, editor, project, "Export PLY"),
        DataMenu::CsvBlockModel => draw_export_csv_block_model(ui, editor, project),
        _ => {}
    });
}

fn draw_export_omf(ui: &mut egui::Ui, project: &UiProjectView) {
    ui.heading("Export Open Mining Format");
    let designs = project.projects.len();
    let triangulations = project.triangulations.iter().filter(|entry| entry.is_loaded).count();
    let block_models = project.block_models.iter().filter(|entry| entry.is_loaded).count();
    let drill_holes = project.drill_holes.iter().filter(|entry| entry.is_loaded).count();
    let point_clouds = project.point_clouds.iter().filter(|entry| entry.is_loaded).count();
    let rasters = project.raster_textures.iter().filter(|entry| entry.is_loaded).count();
    ui.label(format!(
        "Exports all open data in one project: {designs} design document, {triangulations} triangulation(s), {block_models} block model(s), {drill_holes} drillhole dataset(s), {point_clouds} point cloud(s), and {rasters} raster(s)."
    ));
    ui.small("Incline styling and exact design/drillhole semantics are retained as OMF metadata alongside native OMF geometry and attributes.");
}

fn draw_import_dxf(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    ui.heading("Import DXF");
    draw_import_source_picker(ui, editor, commands, "Source file", "No .dxf chosen");
    active_project_label(ui, "Add to project:", project);
}

fn draw_import_mesh(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, heading: &str) {
    ui.heading(heading);
    draw_import_source_picker(ui, editor, commands, "Source file", "No file chosen");
}

fn draw_import_source_picker(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, label: &'static str, empty_text: &'static str) {
    if MenuFieldFilePicker::new(label, selected_import_source_paths(editor))
        .help_text("Choose the source file or files to import.")
        .empty_text(empty_text)
        .button_text("Choose...")
        .width(FIELD_WIDTH)
        .show(ui)
        .changed()
    {
        commands.push(UiCommand::ChooseImportSourceFiles(editor.data_menu));
    }
}

fn draw_import_csv_block_model(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    ui.heading("Import CSV Block Model");
    draw_import_source_picker(ui, editor, commands, "Model file", "No .csv chosen");
    if selected_import_source_paths(editor).is_empty() {
        ui.small("Choose a CSV file to map its columns. Text columns are detected as Category; other unmapped columns default to Value.");
        return;
    }
    if let Some(error) = &editor.import_csv_error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    let Some(preview) = editor.import_csv_preview.as_mut() else {
        return;
    };

    ui.separator();
    ui.label("Column mapping");
    egui::ScrollArea::both().auto_shrink([false, false]).max_height(ui.available_height()).show(ui, |ui| {
        egui::Grid::new("csv_block_model_preview").striped(true).min_col_width(110.0).show(ui, |ui| {
            for (column, header) in preview.headers.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.strong(if header.is_empty() { "(blank header)" } else { header });
                    let selected = &mut preview.mapping.roles[column];
                    egui::ComboBox::from_id_salt(("csv_column_role", column))
                        .selected_text(selected.label())
                        .width(105.0)
                        .show_ui(ui, |ui| {
                            for role in CsvColumnRole::ALL {
                                ui.selectable_value(selected, role, role.label());
                            }
                        });
                });
            }
            ui.end_row();
            for row in &preview.rows {
                for column in 0..preview.headers.len() {
                    let value = row.get(column).map(String::as_str).unwrap_or("");
                    ui.label(value);
                }
                ui.end_row();
            }
        });
    });
    if let Err(error) = validate_mapping(&preview.mapping, preview.headers.len()) {
        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
    }
}

fn draw_import_csv_drill_holes(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    ui.heading("Import Drillhole CSV Bundle");
    draw_import_source_picker(ui, editor, commands, "CSV files", "No CSV files chosen");
    if let Some(error) = &editor.import_csv_error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    if editor.import_drill_csv.is_empty() {
        ui.small("Choose one or more CSVs, then assign each file and column a role.");
        return;
    }
    egui::ScrollArea::both().auto_shrink([false, false]).max_height(ui.available_height()).show(ui, |ui| {
        for (file_index, (mapping, preview)) in editor.import_drill_csv.iter_mut().enumerate() {
            ui.push_id(("drill_csv_file", file_index), |ui| {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong(mapping.path.file_name().and_then(|name| name.to_str()).unwrap_or("CSV"));
                    let previous = mapping.role;
                    egui::ComboBox::from_id_salt("file_role").selected_text(file_role_label(mapping.role)).show_ui(ui, |ui| {
                        for role in [
                            CsvDrillFileRole::Unassigned,
                            CsvDrillFileRole::Collar,
                            CsvDrillFileRole::Survey,
                            CsvDrillFileRole::Interval,
                            CsvDrillFileRole::ExplicitSegments,
                        ] {
                            ui.selectable_value(&mut mapping.role, role, file_role_label(role));
                        }
                    });
                    if mapping.role != previous {
                        mapping.columns = crate::model::formats::csv_drill_hole::default_columns(mapping.role, &preview.headers);
                    }
                });
                if mapping.role == CsvDrillFileRole::Unassigned {
                    ui.weak("Choose a file purpose to map its columns.");
                }
                egui::Grid::new("mapping").striped(true).min_col_width(100.0).show(ui, |ui| {
                    for (column, header) in preview.headers.iter().enumerate() {
                        ui.vertical(|ui| {
                            ui.strong(header);
                            if mapping.role == CsvDrillFileRole::Unassigned {
                                ui.weak("Unmapped");
                            } else {
                                let selected = &mut mapping.columns[column];
                                egui::ComboBox::from_id_salt(("column", column))
                                    .selected_text(column_role_label(selected))
                                    .width(115.0)
                                    .show_ui(ui, |ui| {
                                        for column_role in available_column_roles(mapping.role, header) {
                                            let label = column_role_label(&column_role);
                                            ui.selectable_value(selected, column_role, label);
                                        }
                                    });
                            }
                        });
                    }
                    ui.end_row();
                    for row in preview.rows.iter().take(3) {
                        for column in 0..preview.headers.len() {
                            ui.label(row.get(column).map_or("", String::as_str));
                        }
                        ui.end_row();
                    }
                });
            });
        }
    });
}

fn file_role_label(role: CsvDrillFileRole) -> &'static str {
    match role {
        CsvDrillFileRole::Unassigned => "Choose purpose…",
        CsvDrillFileRole::Collar => "Collar",
        CsvDrillFileRole::Survey => "Survey",
        CsvDrillFileRole::Interval => "Interval",
        CsvDrillFileRole::ExplicitSegments => "Explicit segments",
    }
}

fn column_role_label(role: &CsvDrillColumnRole) -> &'static str {
    match role {
        CsvDrillColumnRole::Ignore => "Ignore",
        CsvDrillColumnRole::Dhid => "DHID",
        CsvDrillColumnRole::East => "East / X",
        CsvDrillColumnRole::North => "North / Y",
        CsvDrillColumnRole::Elevation => "Elevation / Z",
        CsvDrillColumnRole::Depth => "Depth",
        CsvDrillColumnRole::Azimuth => "Azimuth",
        CsvDrillColumnRole::Dip => "Dip",
        CsvDrillColumnRole::From => "FROM",
        CsvDrillColumnRole::To => "TO",
        CsvDrillColumnRole::StartEast => "Start X",
        CsvDrillColumnRole::StartNorth => "Start Y",
        CsvDrillColumnRole::StartElevation => "Start Z",
        CsvDrillColumnRole::EndEast => "End X",
        CsvDrillColumnRole::EndNorth => "End Y",
        CsvDrillColumnRole::EndElevation => "End Z",
        CsvDrillColumnRole::Diameter => "Diameter",
        CsvDrillColumnRole::Attribute(_) => "Attribute",
    }
}

fn available_column_roles(role: CsvDrillFileRole, header: &str) -> Vec<CsvDrillColumnRole> {
    let mut roles = vec![CsvDrillColumnRole::Ignore, CsvDrillColumnRole::Dhid];
    match role {
        CsvDrillFileRole::Unassigned => roles.truncate(1),
        CsvDrillFileRole::Collar => roles.extend([
            CsvDrillColumnRole::East,
            CsvDrillColumnRole::North,
            CsvDrillColumnRole::Elevation,
            CsvDrillColumnRole::Diameter,
        ]),
        CsvDrillFileRole::Survey => roles.extend([
            CsvDrillColumnRole::Depth,
            CsvDrillColumnRole::East,
            CsvDrillColumnRole::North,
            CsvDrillColumnRole::Elevation,
            CsvDrillColumnRole::Azimuth,
            CsvDrillColumnRole::Dip,
        ]),
        CsvDrillFileRole::Interval => roles.extend([CsvDrillColumnRole::From, CsvDrillColumnRole::To, CsvDrillColumnRole::Attribute(header.to_owned())]),
        CsvDrillFileRole::ExplicitSegments => roles.extend([
            CsvDrillColumnRole::From,
            CsvDrillColumnRole::To,
            CsvDrillColumnRole::StartEast,
            CsvDrillColumnRole::StartNorth,
            CsvDrillColumnRole::StartElevation,
            CsvDrillColumnRole::EndEast,
            CsvDrillColumnRole::EndNorth,
            CsvDrillColumnRole::EndElevation,
            CsvDrillColumnRole::Diameter,
            CsvDrillColumnRole::Attribute(header.to_owned()),
        ]),
    }
    roles
}

fn draw_export_dxf(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView) {
    ui.heading("Export DXF");
    MenuFieldBool::new("Export one layer", &mut editor.export_dxf_layer).show(ui);
    if editor.export_dxf_layer {
        ensure_export_layer(editor, project);
        layer_combo(ui, "dxf_export_layer", "Layer:", project, &mut editor.export_layer);
    } else {
        ensure_export_project(editor, project);
        project_combo(ui, "dxf_export_project", "Project:", project, &mut editor.export_project);
    }
}

fn draw_export_mesh(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, heading: &str) {
    ui.heading(heading);
    ensure_export_triangulation(editor, project);
    triangulation_combo(ui, "mesh_export_triangulation", "Triangulation:", project, &mut editor.export_triangulation);
}

fn draw_export_csv_block_model(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView) {
    ui.heading("Export CSV Block Model");
    ensure_export_block_model(editor, project);
    block_model_combo(ui, "csv_export_block_model", "Block model:", project, &mut editor.export_block_model);
    ui.small("Exports block centroids as x/y/z, block sizes as dx/dy/dz, followed by resource columns.");
}

/// Imports that merge into an existing project target the active project.
fn active_project_label(ui: &mut egui::Ui, field_label: &str, project: &UiProjectView) {
    let name = project
        .projects
        .iter()
        .find(|entry| entry.is_active)
        .map(|entry| entry.name.as_str())
        .unwrap_or("No active project");
    ui.horizontal(|ui| {
        ui.label(field_label);
        ui.label(name);
    });
}

fn project_combo(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, field_label: &str, project: &UiProjectView, selected: &mut Option<u32>) {
    let selected_label = selected
        .and_then(|runtime_id| project.projects.iter().find(|entry| entry.runtime_id == runtime_id))
        .map(|entry| entry.name.as_str())
        .unwrap_or("Choose a project");
    let options = project.projects.iter().map(|entry| (Some(entry.runtime_id), entry.name.clone().into()));
    MenuFieldCombo::new(id, field_label, selected, selected_label, options).width(FIELD_WIDTH).show(ui);
}

fn layer_combo(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, field_label: &str, project: &UiProjectView, selected: &mut Option<LayerId>) {
    let active_entry = project.projects.iter().find(|entry| entry.is_active);
    let selected_label =
        selected.and_then(|selected| active_entry.and_then(|entry| entry.layers.iter().find(|layer| layer.is_loaded && layer.id == selected).map(|layer| layer.name.clone())));
    let options = active_entry
        .into_iter()
        .flat_map(|entry| entry.layers.iter().filter(|layer| layer.is_loaded).map(|layer| (Some(layer.id), layer.name.clone().into())));
    MenuFieldCombo::new(id, field_label, selected, selected_label.unwrap_or_else(|| "Choose a loaded layer".to_string()), options)
        .width(FIELD_WIDTH)
        .show(ui);
}

fn triangulation_combo(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, field_label: &str, project: &UiProjectView, selected: &mut Option<TriangulationId>) {
    let label = selected
        .and_then(|id| project.triangulations.iter().find(|entry| entry.id == id && entry.is_loaded))
        .map(|entry| entry.name.as_str())
        .unwrap_or("Choose a loaded triangulation");
    MenuFieldCombo::new(
        id,
        field_label,
        selected,
        label,
        project
            .triangulations
            .iter()
            .filter(|entry| entry.is_loaded)
            .map(|entry| (Some(entry.id), entry.name.clone().into())),
    )
    .width(FIELD_WIDTH)
    .show(ui);
}

fn block_model_combo(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, field_label: &str, project: &UiProjectView, selected: &mut Option<BlockModelId>) {
    let label = selected
        .and_then(|id| project.block_models.iter().find(|entry| entry.id == id && entry.is_loaded))
        .map(|entry| entry.name.as_str())
        .unwrap_or("Choose a loaded block model");
    MenuFieldCombo::new(
        id,
        field_label,
        selected,
        label,
        project
            .block_models
            .iter()
            .filter(|entry| entry.is_loaded)
            .map(|entry| (Some(entry.id), entry.name.clone().into())),
    )
    .width(FIELD_WIDTH)
    .show(ui);
}

fn selected_import_source_paths(editor: &EditorState) -> &[std::path::PathBuf] {
    if editor.import_source_menu == editor.data_menu {
        &editor.import_source_paths
    } else {
        &[]
    }
}

fn reset_import_defaults(editor: &mut EditorState, _project: &UiProjectView) {
    if editor.import_source_menu == editor.data_menu {
        editor.import_source_menu = DataMenu::None;
        editor.import_source_paths.clear();
    }
    match editor.data_menu {
        DataMenu::CsvBlockModel => {
            editor.import_csv_preview = None;
            editor.import_csv_error = None;
        }
        DataMenu::CsvDrillHole => {
            editor.import_drill_csv.clear();
            editor.import_csv_error = None;
        }
        _ => {}
    }
}

fn reset_export_defaults(editor: &mut EditorState, project: &UiProjectView) {
    match editor.data_menu {
        DataMenu::Dxf => {
            editor.export_dxf_layer = false;
            editor.export_layer = first_loaded_layer(project);
            editor.export_project = active_project(project);
        }
        DataMenu::Obj | DataMenu::Stl | DataMenu::Ply => {
            editor.export_triangulation = first_loaded_triangulation(project);
        }
        DataMenu::CsvBlockModel => {
            editor.export_block_model = first_loaded_block_model(project);
        }
        _ => {}
    }
}

fn ensure_export_layer(editor: &mut EditorState, project: &UiProjectView) {
    if !has_loaded_layer(project, editor.export_layer) {
        editor.export_layer = first_loaded_layer(project);
    }
}

fn ensure_export_project(editor: &mut EditorState, project: &UiProjectView) {
    if !has_project(project, editor.export_project) {
        editor.export_project = active_project(project);
    }
}

fn ensure_export_triangulation(editor: &mut EditorState, project: &UiProjectView) {
    if !has_loaded_triangulation(project, editor.export_triangulation) {
        editor.export_triangulation = first_loaded_triangulation(project);
    }
}

fn ensure_export_block_model(editor: &mut EditorState, project: &UiProjectView) {
    if !has_loaded_block_model(project, editor.export_block_model) {
        editor.export_block_model = first_loaded_block_model(project);
    }
}

fn import_command(editor: &EditorState) -> Option<UiCommand> {
    let source_paths = selected_import_source_paths(editor);
    match editor.data_menu {
        DataMenu::Omf => (!source_paths.is_empty()).then(|| UiCommand::ImportOmfPaths(source_paths.to_vec())),
        DataMenu::Dxf => (!source_paths.is_empty()).then(|| UiCommand::ImportDxfPathsInto(source_paths.to_vec())),
        DataMenu::Obj | DataMenu::Stl | DataMenu::Ply => (!source_paths.is_empty()).then(|| UiCommand::ImportTriangulationPaths(source_paths.to_vec())),
        DataMenu::Las | DataMenu::Xyz | DataMenu::Pcd => (!source_paths.is_empty()).then(|| UiCommand::ImportPointCloudPaths(source_paths.to_vec())),
        DataMenu::Geotiff => (!source_paths.is_empty()).then(|| UiCommand::ImportRasterPaths(source_paths.to_vec())),
        DataMenu::CsvBlockModel => {
            let path = source_paths.first()?.clone();
            let preview = editor.import_csv_preview.as_ref()?;
            validate_mapping(&preview.mapping, preview.headers.len()).ok()?;
            Some(UiCommand::ImportCsvBlockModel {
                path,
                mapping: preview.mapping.clone(),
            })
        }
        DataMenu::CsvDrillHole
            if editor.import_csv_error.is_none()
                && !editor.import_drill_csv.is_empty()
                && editor.import_drill_csv.iter().all(|(mapping, _)| mapping.role != CsvDrillFileRole::Unassigned) =>
        {
            let name = editor
                .import_drill_csv
                .first()?
                .0
                .path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Drill holes")
                .to_owned();
            Some(UiCommand::ImportDrillHole(crate::model::drill_hole::DrillHoleSource::Csv {
                name: if editor.import_drill_csv.len() > 1 {
                    format!("{name} + {} files", editor.import_drill_csv.len() - 1)
                } else {
                    name
                },
                files: editor.import_drill_csv.iter().map(|(mapping, _)| mapping.clone()).collect(),
                browser_path: None,
            }))
        }
        DataMenu::CsvDrillHole | DataMenu::None => None,
    }
}

fn export_command(editor: &EditorState) -> Option<UiCommand> {
    match editor.data_menu {
        DataMenu::Omf => Some(UiCommand::ExportOmf),
        DataMenu::Dxf if editor.export_dxf_layer => editor.export_layer.map(UiCommand::ExportLayerDxf),
        DataMenu::Dxf => editor.export_project.map(UiCommand::ExportProjectDxf),
        DataMenu::Obj | DataMenu::Stl | DataMenu::Ply => {
            let format = mesh_format(editor.data_menu)?;
            editor.export_triangulation.map(|id| UiCommand::ExportTriangulationAs(id, format))
        }
        DataMenu::CsvBlockModel => editor.export_block_model.map(UiCommand::ExportBlockModelCsv),
        _ => None,
    }
}

fn mesh_format(data_menu: DataMenu) -> Option<MeshFormat> {
    match data_menu {
        DataMenu::Obj => Some(MeshFormat::Obj),
        DataMenu::Stl => Some(MeshFormat::Stl),
        DataMenu::Ply => Some(MeshFormat::Ply),
        _ => None,
    }
}

fn is_import_menu(data_menu: DataMenu) -> bool {
    matches!(
        data_menu,
        DataMenu::Omf
            | DataMenu::Dxf
            | DataMenu::Obj
            | DataMenu::Stl
            | DataMenu::Ply
            | DataMenu::Las
            | DataMenu::Xyz
            | DataMenu::Pcd
            | DataMenu::CsvBlockModel
            | DataMenu::CsvDrillHole
            | DataMenu::Geotiff
    )
}

fn is_export_menu(data_menu: DataMenu) -> bool {
    matches!(
        data_menu,
        DataMenu::Omf | DataMenu::Dxf | DataMenu::Obj | DataMenu::Stl | DataMenu::Ply | DataMenu::CsvBlockModel
    )
}

fn first_loaded_layer(project: &UiProjectView) -> Option<LayerId> {
    project
        .projects
        .iter()
        .find(|entry| entry.is_active)
        .and_then(|entry| entry.layers.iter().find(|layer| layer.is_loaded).map(|layer| layer.id))
}

fn active_project(project: &UiProjectView) -> Option<u32> {
    project.projects.iter().find(|entry| entry.is_active).map(|entry| entry.runtime_id)
}

fn first_loaded_triangulation(project: &UiProjectView) -> Option<TriangulationId> {
    project.triangulations.iter().find(|entry| entry.is_loaded).map(|entry| entry.id)
}

fn first_loaded_block_model(project: &UiProjectView) -> Option<BlockModelId> {
    project.block_models.iter().find(|entry| entry.is_loaded).map(|entry| entry.id)
}

fn has_loaded_layer(project: &UiProjectView, selected: Option<LayerId>) -> bool {
    selected.is_some_and(|selected| {
        project
            .projects
            .iter()
            .any(|entry| entry.is_active && entry.layers.iter().any(|layer| layer.is_loaded && layer.id == selected))
    })
}

fn has_project(project: &UiProjectView, selected: Option<u32>) -> bool {
    selected.is_some_and(|runtime_id| project.projects.iter().any(|entry| entry.runtime_id == runtime_id))
}

fn has_loaded_triangulation(project: &UiProjectView, selected: Option<TriangulationId>) -> bool {
    selected.is_some_and(|id| project.triangulations.iter().any(|entry| entry.is_loaded && entry.id == id))
}

fn has_loaded_block_model(project: &UiProjectView, selected: Option<BlockModelId>) -> bool {
    selected.is_some_and(|id| project.block_models.iter().any(|entry| entry.is_loaded && entry.id == id))
}
