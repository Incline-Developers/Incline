//! Triangulation creation and processing dialogs.

use crate::{
    model::{Document, Object, ObjectId, SceneEntityId, point_cloud::PointCloudId, triangulation::TriangulationId},
    rendering::color::{color32_to_rgba, rgba_to_color32},
    ui::{
        state::{ContourOutputLayer, EditorState, TriCreatePhase, TriPolygonClipMode, TriSurfaceCutSide, TriSurfaceType, TriangulationPickTarget, UiCommand, UiProjectView},
        widgets::menu::{DragableMenu, MenuField, MenuFieldBool, MenuFieldCombo, MenuFieldF64, MenuFieldText, MenuFieldU32},
    },
};

/// Reset the Create Triangulation workflow state.
fn tri_reset_state(editor: &mut EditorState) {
    editor.tri_create_open = false;
    editor.tri_create_phase = TriCreatePhase::MainDialog;
    editor.tri_create_picker_px = None;
    editor.tri_hover_handles.clear();
    editor.tri_selected_object_ids.clear();
    editor.tri_selected_layer_ids.clear();
    editor.tri_name_input.clear();
    editor.tri_surface_type = TriSurfaceType::Surface;
    editor.selected_handles.clear();
}

/// Returns a human-readable label for an object (type + layer name).
fn object_label(obj: &Object, document: &Document) -> String {
    let layer_name = document.layer(obj.layer()).map(|l| l.name.as_str()).unwrap_or("?");
    match obj {
        Object::Point { .. } => format!("Point on '{layer_name}'"),
        Object::Polyline { closed: true, .. } => format!("Polygon on '{layer_name}'"),
        Object::Polyline { closed: false, .. } => format!("String on '{layer_name}'"),
        Object::Text { content, .. } => format!("Text \"{content}\" on '{layer_name}'"),
    }
}

fn tri_surface_type_label(surface_type: TriSurfaceType) -> &'static str {
    match surface_type {
        TriSurfaceType::Surface => "Open surface",
        TriSurfaceType::SolidClosed => "Solid – fully closed",
    }
}

const PICK_SELECTOR_WIDTH: f32 = 210.0;
const PICK_BUTTON_WIDTH: f32 = 52.0;
const PICKER_DIALOG_MIN_WIDTH: f32 = 430.0;
const PICKER_DIALOG_MAX_WIDTH: f32 = 450.0;

fn picker_control_width(ui: &egui::Ui) -> f32 {
    PICK_SELECTOR_WIDTH + ui.spacing().item_spacing.x + PICK_BUTTON_WIDTH
}

/// Colour buttons use egui's full interaction width rather than the row
/// height. Keep the contour row's current comfortable numeric widths, then
/// align every other contour control to that actual rendered extent.
fn contour_control_width(ui: &egui::Ui) -> f32 {
    let interact_size = ui.spacing().interact_size;
    picker_control_width(ui) + 2.0 * (interact_size.x - interact_size.y).max(0.0)
}

fn tool_help_panel(ui: &mut egui::Ui, text: impl Into<String>) {
    let width = ui.available_width();
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_width((width - 12.0).max(1.0));
            ui.label(egui::RichText::new(text.into()).italics().weak());
        });
}

/// Rough transient peak memory (bytes) for a terrain TIN build, dominated by the
/// candidate grid and, for the adaptive sampler, the quadtree over it. Deliberately
/// conservative so the dialog can warn before a configuration risks the process.
fn estimate_tin_memory_bytes(target_vertices: usize, sampler: crate::app::commands::triangulation::TerrainSampler, candidate_multiplier: u32) -> u64 {
    use crate::app::commands::triangulation::TerrainSampler;
    let target = target_vertices as u64;
    let (candidate_cells, bytes_per_cell) = match sampler {
        // Cell map only.
        TerrainSampler::Grid => (target, 100u64),
        // Cell map + occupancy set + ~2 quadtree nodes per candidate cell.
        TerrainSampler::Adaptive => (target * u64::from(candidate_multiplier.max(1)), 340u64),
    };
    candidate_cells.saturating_mul(bytes_per_cell) + target.saturating_mul(48)
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GB", bytes / GIB)
    } else {
        format!("{:.0} MB", bytes / MIB)
    }
}

/// Two equal-sized operation choices centred within the same width as a
/// selector plus its Pick button. Returns the clicked choice index.
fn centered_choice_buttons(ui: &mut egui::Ui, row_height: f32, choices: [(&str, bool); 2]) -> (egui::Response, Option<usize>) {
    const BUTTON_WIDTH: f32 = 100.0;
    let width = picker_control_width(ui);
    let gap = ui.spacing().item_spacing.x;
    let leading_space = ((width - BUTTON_WIDTH * 2.0 - gap) * 0.5).max(0.0);
    let mut clicked = None;
    let response = ui
        .allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(leading_space);
            for (index, (label, selected)) in choices.into_iter().enumerate() {
                if ui.add_sized([BUTTON_WIDTH, row_height], egui::Button::new(label).selected(selected)).clicked() {
                    clicked = Some(index);
                }
            }
        })
        .response;
    (response, clicked)
}

fn viewport_pick_status(ui: &mut egui::Ui, editor: &EditorState, empty_text: &str) {
    let text = editor.viewport_pick_hover_label.as_deref().unwrap_or(empty_text);
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_min_width(250.0);
            ui.label(egui::RichText::new(text).italics());
        });
}

fn triangulation_picker_field(
    ui: &mut egui::Ui,
    id_source: &'static str,
    label: &'static str,
    value: &mut Option<TriangulationId>,
    selected_text: &str,
    options: impl IntoIterator<Item = (Option<TriangulationId>, egui::WidgetText)>,
    help_text: &'static str,
) -> bool {
    let width = picker_control_width(ui);
    triangulation_picker_field_with_width(ui, id_source, label, value, selected_text, options, help_text, width)
}

#[allow(clippy::too_many_arguments)]
fn triangulation_picker_field_with_width(
    ui: &mut egui::Ui,
    id_source: &'static str,
    label: &'static str,
    value: &mut Option<TriangulationId>,
    selected_text: &str,
    options: impl IntoIterator<Item = (Option<TriangulationId>, egui::WidgetText)>,
    help_text: &'static str,
    width: f32,
) -> bool {
    let mut pick_clicked = false;
    MenuField::new(label).help_text(help_text).show(ui, |ui, row_height| {
        let selector_width = width - ui.spacing().item_spacing.x - PICK_BUTTON_WIDTH;
        ui.allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt(id_source)
                .selected_text(selected_text)
                .width(selector_width)
                .show_ui(ui, |ui| {
                    for (option, text) in options {
                        ui.selectable_value(value, option, text);
                    }
                })
                .response
                .on_hover_text(selected_text);
            pick_clicked = ui
                .add_sized([PICK_BUTTON_WIDTH, row_height], egui::Button::new("Pick"))
                .on_hover_text("Choose this input by clicking a loaded surface in the viewport")
                .clicked();
        })
        .response
    });
    pick_clicked
}

pub(crate) fn draw_triangulation_pick_prompt(ui: &mut egui::Ui, editor: &mut EditorState) {
    let Some(target) = editor.triangulation_pick_target else {
        return;
    };
    let mut open = true;
    DragableMenu::new("Pick from View")
        .open(&mut open)
        .min_width(280.0)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui.ctx(), |ui| {
            ui.label(target.prompt());
            ui.label(egui::RichText::new("Only loaded triangulations can be picked.").weak());
            ui.add_space(6.0);
            viewport_pick_status(ui, editor, "Move the cursor over a loaded surface.");
            ui.add_space(6.0);
            if ui.button("Cancel Pick").clicked() {
                editor.triangulation_pick_target = None;
                editor.viewport_pick_hover_label = None;
                editor.tri_hover_handles.clear();
            }
        });
    if !open {
        editor.triangulation_pick_target = None;
        editor.viewport_pick_hover_label = None;
        editor.tri_hover_handles.clear();
    }
}

/// Movable main dialog for the Create Triangulation workflow.
pub(crate) fn draw_tri_create_main_dialog(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document, commands: &mut Vec<UiCommand>) {
    if !editor.tri_create_open || editor.tri_create_phase != TriCreatePhase::MainDialog {
        return;
    }

    let mut open = true;
    DragableMenu::new("Create Triangulation").open(&mut open).min_width(370.0).show(ui.ctx(), |ui| {
        tool_help_panel(ui, "Click objects in the viewport to select/deselect. Drag to box-select.");
        ui.add_space(4.0);

        // --- Selection list ---
        let mut remove_object: Option<ObjectId> = None;
        let mut hover_handles: std::collections::HashSet<SceneEntityId> = std::collections::HashSet::new();

        egui::ScrollArea::vertical().max_height(160.0).id_salt("tri_sel_list").show(ui, |ui| {
            for &oid in &editor.tri_selected_object_ids {
                let label = document.get_object(oid).map(|o| object_label(o, document)).unwrap_or_else(|| format!("Object #{}", oid.0));
                ui.horizontal(|ui| {
                    let resp = ui.selectable_label(false, &label);
                    if resp.hovered() {
                        hover_handles.insert(SceneEntityId::Object(oid));
                    }
                    if ui.button(egui::RichText::new("✖").color(egui::Color32::RED)).clicked() {
                        remove_object = Some(oid);
                    }
                });
            }
        });

        editor.tri_hover_handles = hover_handles;

        if let Some(oid) = remove_object {
            editor.tri_selected_object_ids.retain(|&o| o != oid);
            editor.selected_handles.remove(&SceneEntityId::Object(oid));
        }

        let has_selection = !editor.tri_selected_object_ids.is_empty();
        if !has_selection {
            ui.colored_label(egui::Color32::GRAY, "No objects selected yet.");
        }

        ui.add_space(4.0);
        {
            ui.separator();

            // --- Surface / solid type ---
            let surface_type_label = tri_surface_type_label(editor.tri_surface_type);
            MenuFieldCombo::new(
                "tri_surface_type",
                "Triangulation type",
                &mut editor.tri_surface_type,
                surface_type_label,
                [TriSurfaceType::Surface, TriSurfaceType::SolidClosed].map(|surface_type| (surface_type, tri_surface_type_label(surface_type).into())),
            )
            .help_text(
                "Open surface creates a terrain-style sheet. Solid creates a fully enclosed \
                     mesh and requires input that can form a watertight boundary.",
            )
            .width(210.0)
            .show(ui);
        }

        ui.separator();

        // --- Name + Triangulate ---
        MenuFieldText::new("Output name", &mut editor.tri_name_input)
            .help_text("Name assigned to the generated triangulation.")
            .width(PICK_SELECTOR_WIDTH)
            .hint_text("triangulation name")
            .show(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ready = has_selection && !editor.tri_name_input.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Triangulate")).clicked() {
                let object_ids: Vec<ObjectId> = editor.tri_selected_object_ids.clone();
                let name = editor.tri_name_input.trim().to_owned();
                let surface_type = editor.tri_surface_type;
                let command = UiCommand::ExecuteCreateTriangulation { name, object_ids, surface_type };
                tri_reset_state(editor);
                commands.push(command);
            }
            if ui.button("Cancel").clicked() {
                tri_reset_state(editor);
            }
        });
    });

    if !open {
        tri_reset_state(editor);
    }
}

/// Shown when Create Triangulation needs either corrected input or an explicit
/// recovery policy. Actionable failures use concise explanations because the
/// exact contributing geometry is already highlighted in the viewport.
pub(crate) fn draw_tri_create_failure_dialog(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let Some(failure) = editor.tri_create_failure.clone() else {
        return;
    };

    let mut open = true;
    let mut dismiss = false;
    DragableMenu::new("Triangulation Failed").open(&mut open).min_width(340.0).show(ui.ctx(), |ui| {
        if failure.weld_retry_available {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                "Nearby breakline vertices do not meet at exactly the same position, so the \
                     surface cannot be triangulated.",
            );
            ui.add_space(4.0);
            ui.strong("Recommended: Weld & Retry");
            ui.colored_label(
                egui::Color32::GRAY,
                "Vertices within 5 cm in XY and Z will share one position for this \
                     triangulation. This can shift the generated surface locally by up to 5 cm; \
                     the source polylines are unchanged.",
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Weld & Retry").clicked() {
                    commands.push(UiCommand::ExecuteCreateTriangulationWithWeld {
                        name: failure.name.clone(),
                        object_ids: failure.object_ids.clone(),
                        surface_type: failure.surface_type,
                    });
                    dismiss = true;
                }
                if ui.button("Close").clicked() {
                    dismiss = true;
                }
            });
        } else if failure.upper_surface_retry_available {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                "The highlighted breakline edges cross or overlap in plan at different \
                     elevations. One terrain surface cannot follow both.",
            );
            ui.add_space(4.0);
            ui.strong("Solution: Generate Upper Surface");
            ui.colored_label(
                egui::Color32::GRAY,
                "The higher edge will be enforced at each conflict. Lower conflicting \
                     segments will be ignored as breaklines and the surface will interpolate \
                     through those areas. The source polylines are unchanged.",
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Generate Upper Surface").clicked() {
                    commands.push(UiCommand::ExecuteCreateTriangulationUpperSurface {
                        name: failure.name.clone(),
                        object_ids: failure.object_ids.clone(),
                        surface_type: failure.surface_type,
                        coarse_weld: failure.coarse_weld_applied,
                    });
                    dismiss = true;
                }
                if ui.button("Close").clicked() {
                    dismiss = true;
                }
            });
        } else {
            ui.colored_label(egui::Color32::LIGHT_RED, &failure.message);
            if ui.button("Close").clicked() {
                dismiss = true;
            }
        }
    });

    if dismiss || !open {
        editor.tri_create_failure = None;
    }
}

pub(crate) fn draw_cut_poly_dialog(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.tri_cut_poly_open || editor.triangulation_pick_target.is_some() {
        return;
    }
    if let Some(object_id) = editor.tri_cut_poly_object_id {
        let boundary_is_valid = document.get_object(object_id).is_some_and(|object| {
            matches!(
                object,
                Object::Polyline {
                    closed: true,
                    verts,
                    ..
                } if verts.len() >= 3
            )
        });
        if !boundary_is_valid {
            editor.tri_cut_poly_object_id = None;
            editor.tri_cut_poly_object_name.clear();
            if editor.tool_highlight_id == Some(object_id) {
                editor.tool_highlight_id = None;
            }
        }
    }

    // While awaiting a viewport pick, show a small floating prompt instead of the full dialog.
    if editor.tri_cut_poly_awaiting_pick {
        let mut open = true;
        DragableMenu::new("Pick Polygon")
            .open(&mut open)
            .min_width(280.0)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui.ctx(), |ui| {
                ui.label("Click a closed polygon in the viewport.");
                ui.add_space(6.0);
                viewport_pick_status(ui, editor, "Move the cursor over a closed polygon.");
                ui.add_space(6.0);
                if ui.button("Cancel Pick").clicked() {
                    editor.tri_cut_poly_awaiting_pick = false;
                    editor.viewport_pick_hover_label = None;
                    editor.tool_highlight_id = editor.tri_cut_poly_object_id;
                }
            });
        if !open {
            editor.tri_cut_poly_awaiting_pick = false;
            editor.tri_cut_poly_open = false;
            editor.viewport_pick_hover_label = None;
            editor.tool_highlight_id = None;
        }
        return;
    }

    let mut open = true;
    DragableMenu::new("Clip Surface by Polygon")
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project.triangulations.iter().filter_map(|t| t.id.map(|id| (id, t.name.as_str()))).collect();
            let tri_label = editor
                .tri_cut_poly_tri_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .unwrap_or("— select —");
            let old_tri_id = editor.tri_cut_poly_tri_id;
            if triangulation_picker_field(
                ui,
                "cut_poly_tri",
                "Surface",
                &mut editor.tri_cut_poly_tri_id,
                tri_label,
                loaded.iter().map(|(id, name)| (Some(*id), (*name).into())),
                "The triangulated surface that will be clipped.",
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::ClipSurface);
                editor.viewport_pick_hover_label = None;
                editor.tri_hover_handles.clear();
            }
            if editor.tri_cut_poly_tri_id != old_tri_id
                && editor.tri_cut_poly_name_auto
                && let Some(name) = editor
                    .tri_cut_poly_tri_id
                    .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
            {
                let path = std::path::Path::new(name);
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
                let ext = path.extension().and_then(|e| e.to_str());
                editor.tri_cut_poly_name_input = if let Some(ext) = ext { format!("{stem}_cut.{ext}") } else { format!("{stem}_cut") };
            }

            ui.add_space(4.0);

            // Polygon picker — viewport click, not a list
            MenuField::new("Boundary polygon")
                .help_text(
                    "A closed polygon whose XY boundary defines the clipping area. Use Pick to \
                     select it in the viewport.",
                )
                .show(ui, |ui, row_height| {
                    let poly_label = if editor.tri_cut_poly_object_id.is_some() {
                        editor.tri_cut_poly_object_name.as_str()
                    } else {
                        "— none picked —"
                    };
                    let width = picker_control_width(ui);
                    ui.allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let mut display = poly_label.to_owned();
                        ui.add_sized([PICK_SELECTOR_WIDTH, row_height], egui::TextEdit::singleline(&mut display).interactive(false))
                            .on_hover_text(poly_label);
                        if ui
                            .add_sized([PICK_BUTTON_WIDTH, row_height], egui::Button::new("Pick"))
                            .on_hover_text("Choose the boundary by clicking a closed polygon in the viewport")
                            .clicked()
                        {
                            commands.push(UiCommand::BeginCutPolyPick);
                        }
                    })
                    .response
                });

            ui.add_space(4.0);

            MenuField::new("Result")
                .help_text(
                    "Keep inside discards surface outside the polygon. Keep outside cuts a \
                     polygon-shaped hole from the surface.",
                )
                .show(ui, |ui, row_height| {
                    let (response, clicked) = centered_choice_buttons(
                        ui,
                        row_height,
                        [
                            (TriPolygonClipMode::KeepInside.label(), editor.tri_cut_poly_mode == TriPolygonClipMode::KeepInside),
                            (TriPolygonClipMode::KeepOutside.label(), editor.tri_cut_poly_mode == TriPolygonClipMode::KeepOutside),
                        ],
                    );
                    if clicked == Some(0) {
                        editor.tri_cut_poly_mode = TriPolygonClipMode::KeepInside;
                    } else if clicked == Some(1) {
                        editor.tri_cut_poly_mode = TriPolygonClipMode::KeepOutside;
                    }
                    response
                });
            tool_help_panel(
                ui,
                match editor.tri_cut_poly_mode {
                    TriPolygonClipMode::KeepInside => "Keeps only the surface within the polygon boundary.",
                    TriPolygonClipMode::KeepOutside => "Removes the surface within the polygon boundary and keeps the rest.",
                },
            );

            ui.add_space(4.0);

            // Output name
            if MenuFieldText::new("Output name", &mut editor.tri_cut_poly_name_input)
                .help_text(
                    "The clip creates a new triangulation with this name; the source surface is \
                     not modified.",
                )
                .width(picker_control_width(ui))
                .hint_text("e.g. mysurf_cut")
                .show(ui)
                .changed()
            {
                editor.tri_cut_poly_name_auto = false;
            }

            ui.add_space(6.0);
            ui.separator();

            let can_run = editor.tri_cut_poly_tri_id.is_some() && editor.tri_cut_poly_object_id.is_some() && !editor.tri_cut_poly_name_input.trim().is_empty();

            ui.horizontal(|ui| {
                if ui.add_enabled(can_run, egui::Button::new("Clip")).clicked()
                    && let (Some(tri_id), Some(poly_id)) = (editor.tri_cut_poly_tri_id, editor.tri_cut_poly_object_id)
                {
                    commands.push(UiCommand::ExecuteCutTriangulationByPolygon {
                        tri_id,
                        polygon_id: poly_id,
                        mode: editor.tri_cut_poly_mode,
                        name: editor.tri_cut_poly_name_input.trim().to_owned(),
                    });
                }
                if ui.button("Cancel").clicked() {
                    editor.tri_cut_poly_open = false;
                    editor.tool_highlight_id = None;
                }
            });
        });
    if !open {
        editor.tri_cut_poly_open = false;
        editor.tool_highlight_id = None;
    }
}

pub(crate) fn draw_cut_z_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.tri_cut_z_open || editor.triangulation_pick_target.is_some() {
        return;
    }
    let mut open = true;
    DragableMenu::new("Slice Triangulation by Z Range")
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project.triangulations.iter().filter_map(|t| t.id.map(|id| (id, t.name.as_str()))).collect();
            let tri_label = editor
                .tri_cut_z_tri_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .unwrap_or("— select —");
            let old_tri_id = editor.tri_cut_z_tri_id;
            if triangulation_picker_field(
                ui,
                "cut_z_tri",
                "Surface",
                &mut editor.tri_cut_z_tri_id,
                tri_label,
                loaded.iter().map(|(id, name)| (Some(*id), (*name).into())),
                "The surface whose elevation range will be clipped.",
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::SliceSurface);
            }
            if editor.tri_cut_z_tri_id != old_tri_id
                && editor.tri_cut_z_name_auto
                && let Some(name) = editor
                    .tri_cut_z_tri_id
                    .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
            {
                let path = std::path::Path::new(name);
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
                let ext = path.extension().and_then(|e| e.to_str());
                editor.tri_cut_z_name_input = if let Some(ext) = ext { format!("{stem}_slice.{ext}") } else { format!("{stem}_slice") };
            }

            ui.add_space(4.0);

            MenuField::new("Z range")
                .help_text(
                    "Minimum and maximum elevations retained in the output surface. The minimum \
                     must be below the maximum.",
                )
                .show(ui, |ui, row_height| {
                    let width = picker_control_width(ui);
                    let gap = ui.spacing().item_spacing.x;
                    let field_width = (width - gap) * 0.5;
                    ui.allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add_sized(
                            [field_width, row_height],
                            egui::DragValue::new(&mut editor.tri_cut_z_min_input)
                                .range(f64::MIN..=f64::MAX)
                                .speed(0.1)
                                .prefix("Min ")
                                .max_decimals(2),
                        );
                        ui.add_sized(
                            [field_width, row_height],
                            egui::DragValue::new(&mut editor.tri_cut_z_max_input)
                                .range(f64::MIN..=f64::MAX)
                                .speed(0.1)
                                .prefix("Max ")
                                .max_decimals(2),
                        );
                    })
                    .response
                });

            ui.add_space(4.0);

            if MenuFieldText::new("Output name", &mut editor.tri_cut_z_name_input)
                .help_text("Name assigned to the elevation-clipped output surface.")
                .width(picker_control_width(ui))
                .hint_text("e.g. mysurf_slice")
                .show(ui)
                .changed()
            {
                editor.tri_cut_z_name_auto = false;
            }

            ui.add_space(6.0);
            ui.separator();

            let z_min = editor.tri_cut_z_min_input;
            let z_max = editor.tri_cut_z_max_input;
            let valid_z_range = z_min.is_finite() && z_max.is_finite() && z_min < z_max;
            let can_run = editor.tri_cut_z_tri_id.is_some() && valid_z_range && !editor.tri_cut_z_name_input.trim().is_empty();

            ui.horizontal(|ui| {
                if ui.add_enabled(can_run, egui::Button::new("Slice")).clicked()
                    && let Some(tri_id) = editor.tri_cut_z_tri_id
                {
                    commands.push(UiCommand::ExecuteCutTriangulationByZ {
                        tri_id,
                        z_min,
                        z_max,
                        name: editor.tri_cut_z_name_input.trim().to_owned(),
                    });
                }
                if ui.button("Cancel").clicked() {
                    editor.tri_cut_z_open = false;
                }
            });
        });
    if !open {
        editor.tri_cut_z_open = false;
    }
}

pub(crate) fn draw_cut_surface_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.tri_cut_surface_open || editor.triangulation_pick_target.is_some() {
        return;
    }

    let mut open = true;
    DragableMenu::new("Trim to Topology")
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project.triangulations.iter().filter_map(|entry| entry.id.map(|id| (id, entry.name.as_str()))).collect();

            // Match the other topology tools: the reference topology comes
            // first, followed by the surface that will be changed.
            let reference_label = editor
                .tri_cut_surface_reference_id
                .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
                .unwrap_or("— select —");
            let target_id = editor.tri_cut_surface_target_id;
            if triangulation_picker_field(
                ui,
                "cut_surface_reference",
                "Topology",
                &mut editor.tri_cut_surface_reference_id,
                reference_label,
                loaded.iter().filter(|(id, _)| Some(*id) != target_id).map(|(id, name)| (Some(*id), (*name).into())),
                "The reference topology that defines where the other surface is trimmed.",
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::TrimTopology);
            }

            let target_label = editor
                .tri_cut_surface_target_id
                .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
                .unwrap_or("— select —");
            let old_target = editor.tri_cut_surface_target_id;
            let reference_id = editor.tri_cut_surface_reference_id;
            if triangulation_picker_field(
                ui,
                "cut_surface_target",
                "Surface to Trim",
                &mut editor.tri_cut_surface_target_id,
                target_label,
                loaded.iter().filter(|(id, _)| Some(*id) != reference_id).map(|(id, name)| (Some(*id), (*name).into())),
                "The surface that will be changed; the selected topology is left intact.",
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::TrimSurface);
            }

            if editor.tri_cut_surface_target_id != old_target
                && editor.tri_cut_surface_name_auto
                && let Some(name) = editor
                    .tri_cut_surface_target_id
                    .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
            {
                let path = std::path::Path::new(name);
                let stem = path.file_stem().and_then(|value| value.to_str()).unwrap_or(name);
                let ext = path.extension().and_then(|value| value.to_str());
                editor.tri_cut_surface_name_input = if let Some(ext) = ext {
                    format!("{stem}_trimmed.{ext}")
                } else {
                    format!("{stem}_trimmed")
                };
            }

            ui.add_space(4.0);

            MenuField::new("Operation")
                .help_text(
                    "Choose which side of the reference topology to remove from the surface \
                     within their shared XY area.",
                )
                .show(ui, |ui, row_height| {
                    let (response, clicked) = centered_choice_buttons(
                        ui,
                        row_height,
                        [
                            (TriSurfaceCutSide::CutTop.trim_label(), editor.tri_cut_surface_side == TriSurfaceCutSide::CutTop),
                            (TriSurfaceCutSide::CutBottom.trim_label(), editor.tri_cut_surface_side == TriSurfaceCutSide::CutBottom),
                        ],
                    );
                    if clicked == Some(0) {
                        editor.tri_cut_surface_side = TriSurfaceCutSide::CutTop;
                    } else if clicked == Some(1) {
                        editor.tri_cut_surface_side = TriSurfaceCutSide::CutBottom;
                    }
                    response
                });

            tool_help_panel(
                ui,
                format!("Keeps the surface {} the topology within its XY coverage.", editor.tri_cut_surface_side.retained_relation()),
            );

            if MenuFieldText::new("Output name", &mut editor.tri_cut_surface_name_input)
                .help_text("Name assigned to the trimmed output surface.")
                .width(picker_control_width(ui))
                .hint_text("e.g. design_trimmed")
                .show(ui)
                .changed()
            {
                editor.tri_cut_surface_name_auto = false;
            }

            ui.add_space(6.0);
            ui.separator();

            let can_run = editor.tri_cut_surface_target_id.is_some()
                && editor.tri_cut_surface_reference_id.is_some()
                && editor.tri_cut_surface_target_id != editor.tri_cut_surface_reference_id
                && !editor.tri_cut_surface_name_input.trim().is_empty();
            ui.horizontal(|ui| {
                if ui.add_enabled(can_run, egui::Button::new("Trim")).clicked()
                    && let (Some(target_id), Some(reference_id)) = (editor.tri_cut_surface_target_id, editor.tri_cut_surface_reference_id)
                {
                    commands.push(UiCommand::ExecuteCutTriangulationBySurface {
                        target_id,
                        reference_id,
                        side: editor.tri_cut_surface_side,
                        name: editor.tri_cut_surface_name_input.trim().to_owned(),
                    });
                }
                if ui.button("Cancel").clicked() {
                    editor.tri_cut_surface_open = false;
                }
            });
        });

    if !open {
        editor.tri_cut_surface_open = false;
    }
}

pub(crate) fn draw_cut_topology_to_pit_shell_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.tri_cut_pitshell_open || editor.triangulation_pick_target.is_some() {
        return;
    }

    let mut open = true;
    DragableMenu::new("Cut Topology with Pit Shell")
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project.triangulations.iter().filter_map(|entry| entry.id.map(|id| (id, entry.name.as_str()))).collect();

            let topology_label = editor
                .tri_cut_pitshell_topology_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .unwrap_or("— select —");
            let old_topology_id = editor.tri_cut_pitshell_topology_id;
            if triangulation_picker_field(
                ui,
                "cut_pitshell_topology",
                "Topology",
                &mut editor.tri_cut_pitshell_topology_id,
                topology_label,
                loaded
                    .iter()
                    .filter(|(id, _)| Some(*id) != editor.tri_cut_pitshell_pitshell_id)
                    .map(|(id, name)| (Some(*id), (*name).into())),
                "The existing ground topology that will be cut by the pit shell.",
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::CutPitTopology);
            }

            if editor.tri_cut_pitshell_topology_id != old_topology_id
                && editor.tri_cut_pitshell_name_auto
                && let Some(name) = editor
                    .tri_cut_pitshell_topology_id
                    .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
            {
                let path = std::path::Path::new(name);
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
                let ext = path.extension().and_then(|e| e.to_str());
                editor.tri_cut_pitshell_name_input = if let Some(ext) = ext { format!("{stem}_cut.{ext}") } else { format!("{stem}_cut") };
            }

            let pitshell_label = editor
                .tri_cut_pitshell_pitshell_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .unwrap_or("— select —");
            if triangulation_picker_field(
                ui,
                "cut_pitshell_shell",
                "Pit shell",
                &mut editor.tri_cut_pitshell_pitshell_id,
                pitshell_label,
                loaded
                    .iter()
                    .filter(|(id, _)| Some(*id) != editor.tri_cut_pitshell_topology_id)
                    .map(|(id, name)| (Some(*id), (*name).into())),
                "The pit design surface. Only areas where it excavates below the topology are \
                 used for the cut.",
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::CutPitShell);
            }

            ui.add_space(4.0);
            tool_help_panel(
                ui,
                "Removes the topology where the pit shell excavates below it so the shell \
                 fills the hole. The seam follows the true 3D contact line between the \
                 surfaces; topology under parts of the shell that stand above the ground \
                 is kept.",
            );

            if MenuFieldText::new("Output name", &mut editor.tri_cut_pitshell_name_input)
                .help_text("Name assigned to the topology after the pit shell is cut from it.")
                .width(picker_control_width(ui))
                .hint_text("e.g. topo_cut")
                .show(ui)
                .changed()
            {
                editor.tri_cut_pitshell_name_auto = false;
            }

            ui.add_space(6.0);
            ui.separator();

            let can_run = editor.tri_cut_pitshell_topology_id.is_some() && editor.tri_cut_pitshell_pitshell_id.is_some() && !editor.tri_cut_pitshell_name_input.trim().is_empty();

            ui.horizontal(|ui| {
                if ui.add_enabled(can_run, egui::Button::new("Cut")).clicked()
                    && let (Some(topology_id), Some(pit_shell_id)) = (editor.tri_cut_pitshell_topology_id, editor.tri_cut_pitshell_pitshell_id)
                {
                    commands.push(UiCommand::ExecuteCutTopologyByPitShell {
                        topology_id,
                        pit_shell_id,
                        name: editor.tri_cut_pitshell_name_input.trim().to_owned(),
                    });
                }
                if ui.button("Cancel").clicked() {
                    editor.tri_cut_pitshell_open = false;
                    editor.tool_highlight_id = None;
                }
            });
        });

    if !open {
        editor.tri_cut_pitshell_open = false;
        editor.tool_highlight_id = None;
    }
}

pub(crate) fn draw_include_solid_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.tri_include_solid_open || editor.triangulation_pick_target.is_some() {
        return;
    }

    let mut open = true;
    DragableMenu::new("Merge Shell into Topology")
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project.triangulations.iter().filter_map(|entry| entry.id.map(|id| (id, entry.name.as_str()))).collect();

            let topology_label = editor
                .tri_include_solid_topology_id
                .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
                .unwrap_or("— select —");
            let old_topology = editor.tri_include_solid_topology_id;
            if triangulation_picker_field(
                ui,
                "include_solid_topology",
                "Topology",
                &mut editor.tri_include_solid_topology_id,
                topology_label,
                loaded.iter().map(|(id, name)| (Some(*id), (*name).into())),
                "The base topology that will receive the pit or stockpile shape.",
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::IncludeTopology);
            }

            if editor.tri_include_solid_topology_id != old_topology {
                if editor.tri_include_solid_shape_id == editor.tri_include_solid_topology_id {
                    editor.tri_include_solid_shape_id = None;
                }
                if editor.tri_include_solid_name_auto
                    && let Some(name) = editor
                        .tri_include_solid_topology_id
                        .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
                {
                    let stem = std::path::Path::new(name).file_stem().and_then(|value| value.to_str()).unwrap_or(name);
                    editor.tri_include_solid_name_input = format!("{stem}_with_shape");
                }
            }

            let shape_label = editor
                .tri_include_solid_shape_id
                .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
                .unwrap_or("— select —");
            let topology_id = editor.tri_include_solid_topology_id;
            if triangulation_picker_field(
                ui,
                "include_solid_shape",
                "Pit/stockpile solid",
                &mut editor.tri_include_solid_shape_id,
                shape_label,
                loaded.iter().filter(|(id, _)| Some(*id) != topology_id).map(|(id, name)| (Some(*id), (*name).into())),
                "A closed pit or stockpile solid whose exposed boundary will be included in the \
                 result.",
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::IncludeShape);
            }

            if MenuFieldText::new("Output name", &mut editor.tri_include_solid_name_input)
                .help_text("Name assigned to the merged topology and pit/stockpile result.")
                .width(picker_control_width(ui))
                .hint_text("e.g. topo_with_pit")
                .show(ui)
                .changed()
            {
                editor.tri_include_solid_name_auto = false;
            }
            MenuFieldBool::new("Save as two entities", &mut editor.tri_include_solid_save_as_two)
                .help_text(
                    "Keep the clipped topology and included shape as separate triangulations instead \
                 of combining them into one entity.",
                )
                .show(ui);

            ui.add_space(6.0);
            ui.separator();

            let can_run = editor.tri_include_solid_topology_id.is_some()
                && editor.tri_include_solid_shape_id.is_some()
                && editor.tri_include_solid_topology_id != editor.tri_include_solid_shape_id
                && !editor.tri_include_solid_name_input.trim().is_empty();
            ui.horizontal(|ui| {
                if ui.add_enabled(can_run, egui::Button::new("Merge")).clicked()
                    && let (Some(topology_id), Some(shape_id)) = (editor.tri_include_solid_topology_id, editor.tri_include_solid_shape_id)
                {
                    commands.push(UiCommand::ExecuteIncludeSolidInTopology {
                        topology_id,
                        shape_id,
                        name: editor.tri_include_solid_name_input.trim().to_owned(),
                        save_as_two: editor.tri_include_solid_save_as_two,
                    });
                }
                if ui.button("Cancel").clicked() {
                    editor.tri_include_solid_open = false;
                }
            });
        });

    if !open {
        editor.tri_include_solid_open = false;
    }
}

pub(crate) fn draw_contour_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.tri_contour_open || editor.triangulation_pick_target.is_some() {
        return;
    }
    let mut open = true;
    DragableMenu::new("Generate Contour Lines")
        .open(&mut open)
        // The combined interval row has four controls; give its long label
        // and info marker a clear gutter without changing control alignment.
        .min_width(PICKER_DIALOG_MIN_WIDTH + 62.0)
        .max_width(PICKER_DIALOG_MAX_WIDTH + 62.0)
        .show(ui.ctx(), |ui| {
            let control_width = contour_control_width(ui);
            let loaded: Vec<(TriangulationId, &str)> = project
                .triangulations
                .iter()
                .filter_map(|t| t.id.map(|id| (id, t.name.as_str())))
                .collect();
            let tri_label = editor
                .tri_contour_tri_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .unwrap_or("— select —");
            let old_tri_id = editor.tri_contour_tri_id;
            if triangulation_picker_field_with_width(
                ui,
                "contour_tri",
                "Surface",
                &mut editor.tri_contour_tri_id,
                tri_label,
                loaded.iter().map(|(id, name)| (Some(*id), (*name).into())),
                "The surface from which contour lines will be generated.",
                control_width,
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::ContourSurface);
            }
            if editor.tri_contour_tri_id != old_tri_id
                && let Some(name) = editor.tri_contour_tri_id.and_then(|id| {
                    loaded
                        .iter()
                        .find(|(loaded_id, _)| *loaded_id == id)
                        .map(|(_, name)| *name)
                })
            {
                editor.update_contour_layer_name_from_surface(name);
            }

            ui.add_space(4.0);

            let mut minor_color = rgba_to_color32(editor.tri_contour_minor_color);
            let mut major_color = rgba_to_color32(editor.tri_contour_major_color);
            MenuField::new("Intervals & colours")
                .help_text(
                    "Minor controls ordinary contours. Major controls emphasized contours and \
                     must use an interval at least as large as Minor.",
                )
                .show(ui, |ui, row_height| {
                    let width = control_width;
                    let gap = ui.spacing().item_spacing.x;
                    let colour_width = ui.spacing().interact_size.x;
                    let value_width = (width - colour_width * 2.0 - gap * 3.0) * 0.5;
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, row_height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add_sized(
                                [value_width, row_height],
                                egui::DragValue::new(&mut editor.tri_contour_minor_interval_input)
                                    .range(1e-6..=f64::MAX)
                                    .speed(0.1)
                                    .prefix("Minor ")
                                    .max_decimals(3),
                            );
                            ui.color_edit_button_srgba(&mut minor_color);
                            ui.add_sized(
                                [value_width, row_height],
                                egui::DragValue::new(&mut editor.tri_contour_major_interval_input)
                                    .range(1e-6..=f64::MAX)
                                    .speed(0.1)
                                    .prefix("Major ")
                                    .max_decimals(3),
                            );
                            ui.color_edit_button_srgba(&mut major_color);
                        },
                    )
                    .response
                });
            editor.tri_contour_minor_color = color32_to_rgba(minor_color);
            editor.tri_contour_major_color = color32_to_rgba(major_color);

            MenuField::new("Limit Z range")
                .help_text(
                    "When enabled, generate contours only between the specified minimum and \
                     maximum elevations.",
                )
                .show(ui, |ui, row_height| {
                    let width = control_width;
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, row_height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add_sized(
                                [row_height, row_height],
                                egui::Checkbox::new(&mut editor.tri_contour_use_z_range, ""),
                            );
                            if editor.tri_contour_use_z_range {
                                let gap = ui.spacing().item_spacing.x;
                                let value_width = (width - row_height - gap * 2.0) * 0.5;
                                ui.add_sized(
                                    [value_width, row_height],
                                    egui::DragValue::new(&mut editor.tri_contour_z_min_input)
                                        .range(f64::MIN..=f64::MAX)
                                        .speed(0.1)
                                        .prefix("Min ")
                                        .max_decimals(2),
                                );
                                ui.add_sized(
                                    [value_width, row_height],
                                    egui::DragValue::new(&mut editor.tri_contour_z_max_input)
                                        .range(f64::MIN..=f64::MAX)
                                        .speed(0.1)
                                        .prefix("Max ")
                                        .max_decimals(2),
                                );
                            } else {
                                ui.weak("Use the full surface elevation range");
                            }
                        },
                    )
                    .response
                });

            ui.add_space(4.0);

            let active_project = project.projects.iter().find(|entry| entry.is_active);
            let active_layers = active_project
                .map(|entry| entry.layers.as_slice())
                .unwrap_or(&[]);
            if editor
                .tri_contour_target_layer
                .is_some_and(|id| !active_layers.iter().any(|layer| layer.id == id))
            {
                editor.tri_contour_target_layer = None;
            }
            let output_layer_label = editor
                .tri_contour_target_layer
                .and_then(|id| active_layers.iter().find(|layer| layer.id == id))
                .map(|layer| layer.name.as_str())
                .unwrap_or("New layer");
            MenuFieldCombo::new(
                "contour_output_layer",
                "Output layer",
                &mut editor.tri_contour_target_layer,
                output_layer_label,
                std::iter::once((None, "New layer".into())).chain(
                    active_layers
                        .iter()
                        .map(|layer| (Some(layer.id), layer.name.clone().into())),
                ),
            )
            .help_text(
                "Create a new layer for the contours or append them to an existing layer in the \
                 active PIDB.",
            )
            .width(control_width)
            .show(ui);

            if editor.tri_contour_target_layer.is_none()
                && MenuFieldText::new("New layer name", &mut editor.tri_contour_layer_name_input)
                    .help_text("Name assigned to the newly created contour layer.")
                    .width(control_width)
                    .hint_text("e.g. surface_contour")
                    .show(ui)
                    .changed()
            {
                editor.tri_contour_layer_name_auto = false;
            }
            let new_layer_name = editor.tri_contour_layer_name_input.trim().to_owned();
            let new_layer_name_conflicts = editor.tri_contour_target_layer.is_none()
                && active_layers
                    .iter()
                    .any(|layer| layer.name == new_layer_name);
            if editor.tri_contour_target_layer.is_none() && new_layer_name_conflicts {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    "That layer already exists; select it above or choose another name.",
                );
            }

            ui.add_space(6.0);
            ui.separator();

            let minor_interval = editor.tri_contour_minor_interval_input;
            let major_interval = editor.tri_contour_major_interval_input;
            let valid_intervals = minor_interval.is_finite()
                && major_interval.is_finite()
                && minor_interval >= 1e-6
                && major_interval >= 1e-6
                && major_interval >= minor_interval;
            let z_range = editor.tri_contour_use_z_range.then_some((
                editor.tri_contour_z_min_input,
                editor.tri_contour_z_max_input,
            ));
            let valid_z_range =
                z_range.is_none_or(|(lo, hi)| lo.is_finite() && hi.is_finite() && lo < hi);
            let can_run = editor.tri_contour_tri_id.is_some()
                && valid_intervals
                && valid_z_range
                && project.has_active_project
                && (editor.tri_contour_target_layer.is_some()
                    || (!new_layer_name.is_empty() && !new_layer_name_conflicts));

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can_run, egui::Button::new("Generate"))
                    .clicked()
                    && let Some(tri_id) = editor.tri_contour_tri_id
                {
                    let output_layer = editor
                        .tri_contour_target_layer
                        .map(ContourOutputLayer::Existing)
                        .unwrap_or_else(|| ContourOutputLayer::New(new_layer_name.clone()));
                    commands.push(UiCommand::ExecuteContourTriangulation {
                        tri_id,
                        major_interval,
                        minor_interval,
                        major_color: editor.tri_contour_major_color,
                        minor_color: editor.tri_contour_minor_color,
                        z_range,
                        output_layer,
                    });
                }
                if ui.button("Cancel").clicked() {
                    editor.tri_contour_open = false;
                }
            });
        });
    if !open {
        editor.tri_contour_open = false;
    }
}

/// Survey point-cloud reconstruction: build an open terrain TIN from XY/Z
/// samples.
pub(crate) fn draw_point_cloud_tin_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.point_cloud_tin_open {
        return;
    }
    use crate::app::commands::triangulation::{TerrainBudget, TerrainSampler, TerrainTinParams, terrain_budget_target};

    let mut open = true;
    DragableMenu::new("Generate Terrain TIN").open(&mut open).min_width(400.0).show(ui.ctx(), |ui| {
        tool_help_panel(
            ui,
            "Reconstruct a triangulated terrain surface from a point cloud. The adaptive \
                 sampler spends the vertex budget where the ground is most complex and keeps \
                 planar areas sparse.",
        );
        ui.add_space(4.0);

        let loaded: Vec<(PointCloudId, &str, usize)> = project
            .point_clouds
            .iter()
            .filter_map(|cloud| cloud.id.map(|id| (id, cloud.name.as_str(), cloud.point_count)))
            .collect();
        if loaded.is_empty() {
            tool_help_panel(ui, "No point clouds are loaded. Import one via File ▸ Import first.");
        }
        let selected = editor.point_cloud_tin_cloud_id.and_then(|id| loaded.iter().find(|(lid, ..)| *lid == id).copied());
        let cloud_label = selected.map(|(_, name, _)| name).unwrap_or("— select —");
        MenuFieldCombo::new(
            "point_cloud_tin_cloud",
            "Point cloud",
            &mut editor.point_cloud_tin_cloud_id,
            cloud_label,
            loaded.iter().map(|(id, name, _)| (Some(*id), (*name).into())),
        )
        .help_text("The loaded point cloud whose points will be reconstructed into a terrain surface.")
        .width(220.0)
        .show(ui);

        let sampler_label = match editor.point_cloud_tin_sampler {
            TerrainSampler::Adaptive => "Adaptive (quadtree)",
            TerrainSampler::Grid => "Uniform grid",
        };
        MenuFieldCombo::new(
            "point_cloud_tin_sampler",
            "Method",
            &mut editor.point_cloud_tin_sampler,
            sampler_label,
            [(TerrainSampler::Adaptive, "Adaptive (quadtree)".into()), (TerrainSampler::Grid, "Uniform grid".into())],
        )
        .help_text(
            "Adaptive concentrates vertices on complex terrain via plane-fit error; uniform \
                 spreads them evenly. More methods may be added in future.",
        )
        .width(220.0)
        .show(ui);

        ui.add_space(4.0);
        ui.separator();

        // Budget: percentage (fractions allowed) or an absolute vertex count.
        let budget_label = if editor.point_cloud_tin_budget_is_percent {
            "Percentage of cloud"
        } else {
            "Vertex count"
        };
        MenuFieldCombo::new(
            "point_cloud_tin_budget_mode",
            "Budget by",
            &mut editor.point_cloud_tin_budget_is_percent,
            budget_label,
            [(true, "Percentage of cloud".into()), (false, "Vertex count".into())],
        )
        .help_text("Cap the surface by a share of the source points or by an exact vertex count.")
        .width(220.0)
        .show(ui);

        if editor.point_cloud_tin_budget_is_percent {
            MenuFieldF64::new("Percentage", &mut editor.point_cloud_tin_percent, 0.001..=100.0)
                .help_text("Share of source points to keep. Fractions such as 0.125% are allowed.")
                .speed(0.05)
                .suffix("%")
                .max_decimals(3)
                .width(220.0)
                .show(ui);
        } else {
            MenuFieldU32::new("Vertex count", &mut editor.point_cloud_tin_limit, 3..=50_000_000)
                .help_text(
                    "Exact number of surface vertices to target. Very large values build slowly \
                     and use significant memory.",
                )
                .speed(1000.0)
                .width(220.0)
                .show(ui);
        }

        let budget = if editor.point_cloud_tin_budget_is_percent {
            TerrainBudget::Percent(editor.point_cloud_tin_percent)
        } else {
            TerrainBudget::Count(editor.point_cloud_tin_limit as usize)
        };
        if let Some((.., point_count)) = selected {
            let target = terrain_budget_target(point_count, budget);
            let percent = if point_count > 0 { target as f64 * 100.0 / point_count as f64 } else { 0.0 };
            tool_help_panel(
                ui,
                format!(
                    "Up to {target} of {point_count} points will become surface vertices \
                         ({percent:.3}%)."
                ),
            );
        }

        if editor.point_cloud_tin_sampler == TerrainSampler::Adaptive {
            MenuFieldU32::new("Candidate detail", &mut editor.point_cloud_tin_candidate_mult, 1..=8)
                .help_text(
                    "Candidate fine cells per budgeted vertex. Higher gives the adaptive sampler \
                     more freedom to place detail, but is slower to build.",
                )
                .suffix("×")
                .width(220.0)
                .show(ui);
        }

        // Estimated transient memory, so an over-ambitious budget can be
        // caught before it risks the process rather than after.
        let mut memory_ok = true;
        if let Some((.., point_count)) = selected {
            const WARN_BYTES: u64 = 6 * 1024 * 1024 * 1024;
            const HARD_BYTES: u64 = 48 * 1024 * 1024 * 1024;
            let target = terrain_budget_target(point_count, budget);
            let estimate = estimate_tin_memory_bytes(target, editor.point_cloud_tin_sampler, editor.point_cloud_tin_candidate_mult);
            if estimate >= WARN_BYTES {
                memory_ok = estimate < HARD_BYTES;
                let color = if memory_ok { ui.visuals().warn_fg_color } else { ui.visuals().error_fg_color };
                let tail = if memory_ok {
                    "Reduce the budget or candidate detail if your machine has less RAM."
                } else {
                    "This exceeds a safe limit; reduce the budget or candidate detail to continue."
                };
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(3.0)
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(format!("Estimated peak memory ~{}. {tail}", format_bytes(estimate))).color(color));
                    });
            }
        }

        ui.add_space(4.0);
        ui.separator();

        MenuFieldF64::new("Max edge length", &mut editor.point_cloud_tin_max_edge, 0.0..=1_000_000.0)
            .help_text(
                "Reject reconstructed triangle edges longer than this distance. Use 0 for no \
                 edge-length limit.",
            )
            .speed(1.0)
            .suffix("m")
            .width(220.0)
            .show(ui);

        MenuFieldF64::new("Fill holes up to", &mut editor.point_cloud_tin_hole_fill, 0.0..=10_000.0)
            .help_text(
                "Bridge gaps and boundary concavities narrower than this across the surface. 0 \
                 still bridges gaps up to roughly the sampling cell size; larger values fill \
                 bigger holes and erode boundary concavities.",
            )
            .speed(0.5)
            .suffix("m")
            .max_decimals(2)
            .width(220.0)
            .show(ui);

        ui.add_space(4.0);
        ui.separator();

        MenuFieldText::new("Output name", &mut editor.point_cloud_tin_name_input)
            .help_text("Name assigned to the reconstructed triangulation.")
            .width(220.0)
            .hint_text("Surface")
            .show(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let can_run = selected.is_some() && !editor.point_cloud_tin_name_input.trim().is_empty() && memory_ok;
            if ui.add_enabled(can_run, egui::Button::new("Generate")).clicked()
                && let Some((cloud_id, ..)) = selected
            {
                commands.push(UiCommand::ExecutePointCloudTin {
                    cloud_id,
                    params: TerrainTinParams {
                        name: editor.point_cloud_tin_name_input.trim().to_owned(),
                        budget,
                        max_edge: editor.point_cloud_tin_max_edge,
                        sampler: editor.point_cloud_tin_sampler,
                        candidate_multiplier: editor.point_cloud_tin_candidate_mult,
                        hole_fill_distance: editor.point_cloud_tin_hole_fill,
                    },
                });
                editor.point_cloud_tin_open = false;
            }
            if ui.button("Cancel").clicked() {
                editor.point_cloud_tin_open = false;
            }
        });
    });
    if !open {
        editor.point_cloud_tin_open = false;
    }
}
