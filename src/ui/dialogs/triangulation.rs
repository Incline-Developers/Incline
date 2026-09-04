//! Triangulation creation and processing dialogs.

use crate::{
    i18n::{tr, tr_format},
    model::{Document, Object, ObjectId, SceneEntityId, point_cloud::PointCloudId, triangulation::TriangulationId},
    rendering::color::{color32_to_rgba, rgba_to_color32},
    ui::{
        state::{ContourOutputLayer, EditorState, TriCreatePhase, TriPolylineClipMode, TriSurfaceCutSide, TriSurfaceType, TriangulationPickTarget, UiCommand, UiProjectView},
        widgets::menu::{self, DragableMenu, MenuButton, MenuField, MenuFieldBool, MenuFieldCombo, MenuFieldF64, MenuFieldText, MenuFieldU32},
    },
};

/// Reset the Create Triangulation workflow state.
fn tri_reset_state(editor: &mut EditorState) {
    tri_close_dialog(editor);
    editor.selected_handles.clear();
}

/// Close the dialog but leave the viewport selection alone.
///
/// Used when the dialog is dismissed rather than run: the selection was made
/// before it opened, so cancelling should not cost the user that work.
fn tri_close_dialog(editor: &mut EditorState) {
    editor.tri_create_open = false;
    editor.tri_create_phase = TriCreatePhase::MainDialog;
    editor.tri_create_picker_px = None;
    editor.tri_hover_handles.clear();
    editor.tri_selected_object_ids.clear();
    editor.tri_selected_layer_ids.clear();
    editor.tri_name_input.clear();
    editor.tri_surface_type = TriSurfaceType::Surface;
}

/// Compact summary of a viewport selection, grouped by object kind
/// ("5 polylines, 2 strings"). Preferred over per-item chip lists.
///
/// Each noun is pluralised by its own count through Fluent, so a language with
/// more than the two English plural forms (Russian's one/few/many) reads right.
fn selection_summary(object_ids: &[ObjectId], document: &Document) -> String {
    let (mut points, mut polylines, mut strings, mut texts) = (0i64, 0i64, 0i64, 0i64);
    for &oid in object_ids {
        match document.get_object(oid) {
            Some(Object::Point { .. }) => points += 1,
            Some(Object::Polyline { closed: true, .. }) => polylines += 1,
            Some(Object::Polyline { closed: false, .. }) => strings += 1,
            Some(Object::Text { .. }) => texts += 1,
            None => {}
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if polylines > 0 {
        parts.push(tr!("tri-count-polylines", count = polylines));
    }
    if strings > 0 {
        parts.push(tr!("tri-count-strings", count = strings));
    }
    if points > 0 {
        parts.push(tr!("tri-count-points", count = points));
    }
    if texts > 0 {
        parts.push(tr!("tri-count-texts", count = texts));
    }
    if parts.is_empty() {
        let total = object_ids.len() as i64;
        tr!("tri-count-objects", count = total)
    } else {
        parts.join(", ")
    }
}

fn tri_surface_type_label(surface_type: TriSurfaceType) -> String {
    match surface_type {
        TriSurfaceType::Surface => tr!("tri-type-open-surface"),
        TriSurfaceType::SolidClosed => tr!("tri-type-solid-closed"),
    }
}

const PICK_SELECTOR_WIDTH: f32 = 210.0;
const PICK_BUTTON_WIDTH: f32 = 52.0;
const PICKER_DIALOG_MIN_WIDTH: f32 = 430.0;
const PICKER_DIALOG_MAX_WIDTH: f32 = 450.0;

fn pick_button_label() -> String {
    tr!(literal = "Pick")
}

fn pick_button_width(ui: &egui::Ui, label: &str) -> f32 {
    menu::natural_button_width(ui, label, PICK_BUTTON_WIDTH)
}

fn picker_control_width(ui: &egui::Ui) -> f32 {
    let label = pick_button_label();
    PICK_SELECTOR_WIDTH + ui.spacing().item_spacing.x + pick_button_width(ui, &label)
}

/// Colour buttons use egui's full interaction width rather than the row
/// height. Keep the contour row's current comfortable numeric widths, then
/// align every other contour control to that actual rendered extent.
fn contour_control_width(ui: &egui::Ui) -> f32 {
    let interact_size = ui.spacing().interact_size;
    picker_control_width(ui) + 2.0 * (interact_size.x - interact_size.y).max(0.0)
}

fn tool_help_panel(ui: &mut egui::Ui, text: impl Into<String>) {
    menu::menu_note(ui, text);
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
fn centered_choice_buttons(ui: &mut egui::Ui, row_height: f32, choices: [(String, bool); 2]) -> (egui::Response, Option<usize>) {
    const BUTTON_WIDTH: f32 = 100.0;
    let gap = ui.spacing().item_spacing.x;
    let button_width = choices
        .iter()
        .map(|(label, _)| menu::natural_button_width(ui, label, BUTTON_WIDTH))
        .fold(BUTTON_WIDTH, f32::max);
    let buttons_width = button_width * 2.0 + gap;
    let width = picker_control_width(ui).max(buttons_width);
    let leading_space = ((width - buttons_width) * 0.5).max(0.0);
    let mut clicked = None;
    let response = ui
        .allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(leading_space);
            for (index, (label, selected)) in choices.into_iter().enumerate() {
                if ui.add(MenuButton::new(label).selected(selected).min_width(button_width)).clicked() {
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
    label: impl Into<egui::WidgetText>,
    value: &mut Option<TriangulationId>,
    selected_text: impl Into<egui::WidgetText>,
    options: impl IntoIterator<Item = (Option<TriangulationId>, egui::WidgetText)>,
    help_text: impl Into<egui::WidgetText>,
) -> bool {
    let width = picker_control_width(ui);
    triangulation_picker_field_with_width(ui, id_source, label, value, selected_text, options, help_text, width)
}

#[allow(clippy::too_many_arguments)]
fn triangulation_picker_field_with_width(
    ui: &mut egui::Ui,
    id_source: &'static str,
    label: impl Into<egui::WidgetText>,
    value: &mut Option<TriangulationId>,
    selected_text: impl Into<egui::WidgetText>,
    options: impl IntoIterator<Item = (Option<TriangulationId>, egui::WidgetText)>,
    help_text: impl Into<egui::WidgetText>,
    width: f32,
) -> bool {
    let selected_text = selected_text.into();
    let pick_label = pick_button_label();
    let pick_width = pick_button_width(ui, &pick_label);
    let mut pick_clicked = false;
    MenuField::new(label).help_text(help_text).show(ui, |ui, row_height, _| {
        let selector_width = width - ui.spacing().item_spacing.x - pick_width;
        ui.allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt(id_source)
                .selected_text(selected_text.clone())
                .width(selector_width)
                .show_ui(ui, |ui| {
                    for (option, text) in options {
                        ui.selectable_value(value, option, text);
                    }
                })
                .response
                .on_hover_text(selected_text);
            pick_clicked = ui
                .add(MenuButton::new(pick_label).min_width(pick_width))
                .on_hover_text(tr!(literal = "Choose this input by clicking a loaded surface in the viewport"))
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
    DragableMenu::new("triangulation_pick_from_view_dialog", tr!(literal = "Pick from View"))
        .open(&mut open)
        .min_width(280.0)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui.ctx(), |ui| {
            ui.label(target.prompt());
            ui.label(egui::RichText::new(tr!(literal = "Only loaded triangulations can be picked.")).weak());
            ui.add_space(6.0);
            viewport_pick_status(ui, editor, &tr!(literal = "Move the cursor over a loaded surface."));
            ui.add_space(6.0);
            if ui.add(MenuButton::new(tr!(literal = "Cancel Pick"))).clicked() {
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
    DragableMenu::new("create_triangulation_selection_dialog", tr!("tri-create-title"))
        .open(&mut open)
        .min_width(370.0)
        .show(ui.ctx(), |ui| {
            tool_help_panel(ui, tr!("tri-create-help"));
            ui.add_space(4.0);

            // --- Selection summary ---
            let has_selection = !editor.tri_selected_object_ids.is_empty();
            let mut clear_selection = false;
            let mut hover_selection = false;

            if has_selection {
                let summary = tr!("tri-selection-selected", summary = selection_summary(&editor.tri_selected_object_ids, document));
                ui.horizontal(|ui| {
                    hover_selection = ui.label(summary).hovered();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(MenuButton::new(tr!("common-clear"))).clicked() {
                            clear_selection = true;
                        }
                    });
                });
            } else {
                ui.colored_label(egui::Color32::GRAY, tr!("tri-selection-none"));
            }

            // Hovering the summary highlights the whole selection in the viewport.
            editor.tri_hover_handles = if hover_selection {
                editor.tri_selected_object_ids.iter().map(|&oid| SceneEntityId::Object(oid)).collect()
            } else {
                std::collections::HashSet::new()
            };

            if clear_selection {
                for oid in editor.tri_selected_object_ids.drain(..) {
                    editor.selected_handles.remove(&SceneEntityId::Object(oid));
                }
                editor.tri_hover_handles.clear();
            }

            ui.add_space(4.0);
            {
                ui.separator();

                // --- Surface / solid type ---
                let surface_type_label = tri_surface_type_label(editor.tri_surface_type);
                MenuFieldCombo::new(
                    "tri_surface_type",
                    tr!("tri-create-type-label"),
                    &mut editor.tri_surface_type,
                    surface_type_label,
                    [TriSurfaceType::Surface, TriSurfaceType::SolidClosed].map(|surface_type| (surface_type, tri_surface_type_label(surface_type).into())),
                )
                .help_text(tr!("tri-create-type-help"))
                .width(210.0)
                .show(ui);
            }

            ui.separator();

            // --- Name + Triangulate ---
            MenuFieldText::new(tr!("tri-create-output-name"), &mut editor.tri_name_input)
                .help_text(tr!("tri-create-output-name-help"))
                .width(PICK_SELECTOR_WIDTH)
                .hint_text(tr!("tri-create-output-name-hint"))
                .show(ui);
            menu::menu_actions(ui, |ui| {
                let ready = has_selection && !editor.tri_name_input.trim().is_empty();
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                let cancel = menu::dialog_cancel_pressed(ui.ctx());
                if ui.add(MenuButton::new(tr!("tri-create-run")).primary().enabled(ready)).clicked() || (confirm && ready) {
                    let object_ids: Vec<ObjectId> = editor.tri_selected_object_ids.clone();
                    let name = editor.tri_name_input.trim().to_owned();
                    let surface_type = editor.tri_surface_type;
                    let command = UiCommand::ExecuteCreateTriangulation { name, object_ids, surface_type };
                    tri_reset_state(editor);
                    commands.push(command);
                }
                if ui.add(MenuButton::new(tr!("common-cancel"))).clicked() || cancel {
                    tri_close_dialog(editor);
                }
            });
        });

    if !open {
        tri_close_dialog(editor);
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
    DragableMenu::new("triangulation_failed_dialog", tr!(literal = "Triangulation Failed"))
        .open(&mut open)
        .min_width(340.0)
        .show(ui.ctx(), |ui| {
            if failure.weld_retry_available {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    tr!(literal = "Nearby breakline vertices do not meet at exactly the same position, so the \
                     surface cannot be triangulated."),
                );
                ui.add_space(4.0);
                ui.strong(tr!(literal = "Recommended: Weld & Retry"));
                ui.colored_label(
                    egui::Color32::GRAY,
                    tr!(literal = "Vertices within 5 cm in XY and Z will share one position for this \
                     triangulation. This can shift the generated surface locally by up to 5 cm; \
                     the source polylines are unchanged."),
                );
                menu::menu_actions(ui, |ui| {
                    if ui.add(MenuButton::new(tr!(literal = "Weld & Retry")).primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                        commands.push(UiCommand::ExecuteCreateTriangulationWithWeld {
                            name: failure.name.clone(),
                            object_ids: failure.object_ids.clone(),
                            surface_type: failure.surface_type,
                        });
                        dismiss = true;
                    }
                    if ui.add(MenuButton::new(tr!(literal = "Close"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                        dismiss = true;
                    }
                });
            } else if failure.upper_surface_retry_available {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    tr!(literal = "The highlighted breakline edges cross or overlap in plan at different \
                     elevations. One terrain surface cannot follow both."),
                );
                ui.add_space(4.0);
                ui.strong(tr!(literal = "Solution: Generate Upper Surface"));
                ui.colored_label(
                    egui::Color32::GRAY,
                    tr!(literal = "The higher edge will be enforced at each conflict. Lower conflicting \
                     segments will be ignored as breaklines and the surface will interpolate \
                     through those areas. The source polylines are unchanged."),
                );
                menu::menu_actions(ui, |ui| {
                    if ui.add(MenuButton::new(tr!(literal = "Generate Upper Surface")).primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                        commands.push(UiCommand::ExecuteCreateTriangulationUpperSurface {
                            name: failure.name.clone(),
                            object_ids: failure.object_ids.clone(),
                            surface_type: failure.surface_type,
                            coarse_weld: failure.coarse_weld_applied,
                        });
                        dismiss = true;
                    }
                    if ui.add(MenuButton::new(tr!(literal = "Close"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                        dismiss = true;
                    }
                });
            } else {
                ui.colored_label(egui::Color32::LIGHT_RED, &failure.message);
                // Only one button, so Enter and Escape both dismiss.
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                let cancel = menu::dialog_cancel_pressed(ui.ctx());
                if ui.add(MenuButton::new(tr!(literal = "Close"))).clicked() || confirm || cancel {
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
        DragableMenu::new("clip_surface_pick_polyline_dialog", tr!(literal = "Pick Polyline"))
            .open(&mut open)
            .min_width(280.0)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui.ctx(), |ui| {
                ui.label(tr!(literal = "Click a closed polyline in the viewport."));
                ui.add_space(6.0);
                viewport_pick_status(ui, editor, &tr!(literal = "Move the cursor over a closed polyline."));
                ui.add_space(6.0);
                if ui.add(MenuButton::new(tr!(literal = "Cancel Pick"))).clicked() {
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
    DragableMenu::new("clip_surface_by_polyline_dialog", tr!(literal = "Clip Surface by Polyline"))
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project
                .triangulations
                .iter()
                .filter(|entry| entry.is_loaded)
                .map(|entry| (entry.id, entry.name.as_str()))
                .collect();
            let tri_label = editor
                .tri_cut_poly_tri_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            let old_tri_id = editor.tri_cut_poly_tri_id;
            if triangulation_picker_field(
                ui,
                "cut_poly_tri",
                tr!(literal = "Surface"),
                &mut editor.tri_cut_poly_tri_id,
                tri_label,
                loaded.iter().map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "The triangulated surface that will be clipped."),
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

            // Polyline picker - viewport click, not a list
            MenuField::new(tr!(literal = "Boundary polyline"))
                .help_text(tr!(literal = "A closed polyline whose XY boundary defines the clipping area. Use Pick to \
                     select it in the viewport."))
                .show(ui, |ui, row_height, _| {
                    let poly_label = if editor.tri_cut_poly_object_id.is_some() {
                        editor.tri_cut_poly_object_name.clone()
                    } else {
                        tr!(literal = "None picked")
                    };
                    let pick_label = pick_button_label();
                    let pick_width = pick_button_width(ui, &pick_label);
                    let width = picker_control_width(ui);
                    ui.allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let mut display = poly_label.clone();
                        ui.add_sized([PICK_SELECTOR_WIDTH, row_height], egui::TextEdit::singleline(&mut display).interactive(false))
                            .on_hover_text(poly_label);
                        if ui
                            .add(MenuButton::new(pick_label).min_width(pick_width))
                            .on_hover_text(tr!(literal = "Choose the boundary by clicking a closed polyline in the viewport"))
                            .clicked()
                        {
                            commands.push(UiCommand::BeginCutPolyPick);
                        }
                    })
                    .response
                });

            ui.add_space(4.0);

            MenuField::new(tr!(literal = "Result"))
                .help_text(tr!(literal = "Keep inside discards surface outside the polyline. Keep outside cuts a \
                     polyline-shaped hole from the surface."))
                .show(ui, |ui, row_height, _| {
                    let (response, clicked) = centered_choice_buttons(
                        ui,
                        row_height,
                        [
                            (TriPolylineClipMode::KeepInside.label(), editor.tri_cut_poly_mode == TriPolylineClipMode::KeepInside),
                            (TriPolylineClipMode::KeepOutside.label(), editor.tri_cut_poly_mode == TriPolylineClipMode::KeepOutside),
                        ],
                    );
                    if clicked == Some(0) {
                        editor.tri_cut_poly_mode = TriPolylineClipMode::KeepInside;
                    } else if clicked == Some(1) {
                        editor.tri_cut_poly_mode = TriPolylineClipMode::KeepOutside;
                    }
                    response
                });
            tool_help_panel(
                ui,
                match editor.tri_cut_poly_mode {
                    TriPolylineClipMode::KeepInside => tr!(literal = "Keeps only the surface within the polyline boundary."),
                    TriPolylineClipMode::KeepOutside => tr!(literal = "Removes the surface within the polyline boundary and keeps the rest."),
                },
            );

            ui.add_space(4.0);

            // Output name
            if MenuFieldText::new(tr!(literal = "Output name"), &mut editor.tri_cut_poly_name_input)
                .help_text(tr!(literal = "The clip creates a new triangulation with this name; the source surface is \
                     not modified."))
                .width(picker_control_width(ui))
                .hint_text(tr!(literal = "e.g. mysurf_cut"))
                .show(ui)
                .changed()
            {
                editor.tri_cut_poly_name_auto = false;
            }

            ui.add_space(6.0);
            ui.separator();

            let can_run = editor.tri_cut_poly_tri_id.is_some() && editor.tri_cut_poly_object_id.is_some() && !editor.tri_cut_poly_name_input.trim().is_empty();

            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if (ui.add(MenuButton::new(tr!(literal = "Clip")).primary().enabled(can_run)).clicked() || (confirm && can_run))
                    && let (Some(tri_id), Some(poly_id)) = (editor.tri_cut_poly_tri_id, editor.tri_cut_poly_object_id)
                {
                    commands.push(UiCommand::ExecuteCutTriangulationByPolyline {
                        tri_id,
                        polyline_id: poly_id,
                        mode: editor.tri_cut_poly_mode,
                        name: editor.tri_cut_poly_name_input.trim().to_owned(),
                    });
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new("slice_triangulation_z_dialog", tr!(literal = "Slice Triangulation by Z Range"))
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project
                .triangulations
                .iter()
                .filter(|entry| entry.is_loaded)
                .map(|entry| (entry.id, entry.name.as_str()))
                .collect();
            let tri_label = editor
                .tri_cut_z_tri_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            let old_tri_id = editor.tri_cut_z_tri_id;
            if triangulation_picker_field(
                ui,
                "cut_z_tri",
                tr!(literal = "Surface"),
                &mut editor.tri_cut_z_tri_id,
                tri_label,
                loaded.iter().map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "The surface whose elevation range will be clipped."),
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

            MenuField::new(tr!(literal = "Z range"))
                .help_text(tr!(literal = "Minimum and maximum elevations retained in the output surface. The minimum \
                     must be below the maximum."))
                .show(ui, |ui, row_height, _| {
                    let width = picker_control_width(ui);
                    let gap = ui.spacing().item_spacing.x;
                    let field_width = (width - gap) * 0.5;
                    ui.allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add_sized(
                            [field_width, row_height],
                            egui::DragValue::new(&mut editor.tri_cut_z_min_input)
                                .range(f64::MIN..=f64::MAX)
                                .speed(0.1)
                                .prefix(tr!(literal = "Min "))
                                .max_decimals(2),
                        );
                        ui.add_sized(
                            [field_width, row_height],
                            egui::DragValue::new(&mut editor.tri_cut_z_max_input)
                                .range(f64::MIN..=f64::MAX)
                                .speed(0.1)
                                .prefix(tr!(literal = "Max "))
                                .max_decimals(2),
                        );
                    })
                    .response
                });

            ui.add_space(4.0);

            if MenuFieldText::new(tr!(literal = "Output name"), &mut editor.tri_cut_z_name_input)
                .help_text(tr!(literal = "Name assigned to the elevation-clipped output surface."))
                .width(picker_control_width(ui))
                .hint_text(tr!(literal = "e.g. mysurf_slice"))
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

            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if (ui.add(MenuButton::new(tr!(literal = "Slice")).primary().enabled(can_run)).clicked() || (confirm && can_run))
                    && let Some(tri_id) = editor.tri_cut_z_tri_id
                {
                    commands.push(UiCommand::ExecuteCutTriangulationByZ {
                        tri_id,
                        z_min,
                        z_max,
                        name: editor.tri_cut_z_name_input.trim().to_owned(),
                    });
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new("trim_surface_to_topology_dialog", tr!(literal = "Trim to Topology"))
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project
                .triangulations
                .iter()
                .filter(|entry| entry.is_loaded)
                .map(|entry| (entry.id, entry.name.as_str()))
                .collect();

            // Match the other topology tools: the reference topology comes
            // first, followed by the surface that will be changed.
            let reference_label = editor
                .tri_cut_surface_reference_id
                .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            let target_id = editor.tri_cut_surface_target_id;
            if triangulation_picker_field(
                ui,
                "cut_surface_reference",
                tr!(literal = "Topology"),
                &mut editor.tri_cut_surface_reference_id,
                reference_label,
                loaded.iter().filter(|(id, _)| Some(*id) != target_id).map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "The reference topology that defines where the other surface is trimmed."),
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::TrimTopology);
            }

            let target_label = editor
                .tri_cut_surface_target_id
                .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            let old_target = editor.tri_cut_surface_target_id;
            let reference_id = editor.tri_cut_surface_reference_id;
            if triangulation_picker_field(
                ui,
                "cut_surface_target",
                tr!(literal = "Surface to Trim"),
                &mut editor.tri_cut_surface_target_id,
                target_label,
                loaded.iter().filter(|(id, _)| Some(*id) != reference_id).map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "The surface that will be changed; the selected topology is left intact."),
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

            MenuField::new(tr!(literal = "Operation"))
                .help_text(tr!(literal = "Choose which side of the reference topology to remove from the surface \
                     within their shared XY area."))
                .show(ui, |ui, row_height, _| {
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
                tr_format!(
                    literal = "Keeps the surface %relation% the topology within its XY coverage.",
                    relation = editor.tri_cut_surface_side.retained_relation()
                ),
            );

            if MenuFieldText::new(tr!(literal = "Output name"), &mut editor.tri_cut_surface_name_input)
                .help_text(tr!(literal = "Name assigned to the trimmed output surface."))
                .width(picker_control_width(ui))
                .hint_text(tr!(literal = "e.g. design_trimmed"))
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
            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if (ui.add(MenuButton::new(tr!(literal = "Trim")).primary().enabled(can_run)).clicked() || (confirm && can_run))
                    && let (Some(target_id), Some(reference_id)) = (editor.tri_cut_surface_target_id, editor.tri_cut_surface_reference_id)
                {
                    commands.push(UiCommand::ExecuteCutTriangulationBySurface {
                        target_id,
                        reference_id,
                        side: editor.tri_cut_surface_side,
                        name: editor.tri_cut_surface_name_input.trim().to_owned(),
                    });
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new("cut_topology_with_pit_shell_dialog", tr!(literal = "Cut Topology with Pit Shell"))
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project
                .triangulations
                .iter()
                .filter(|entry| entry.is_loaded)
                .map(|entry| (entry.id, entry.name.as_str()))
                .collect();

            let topology_label = editor
                .tri_cut_pitshell_topology_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            let old_topology_id = editor.tri_cut_pitshell_topology_id;
            if triangulation_picker_field(
                ui,
                "cut_pitshell_topology",
                tr!(literal = "Topology"),
                &mut editor.tri_cut_pitshell_topology_id,
                topology_label,
                loaded
                    .iter()
                    .filter(|(id, _)| Some(*id) != editor.tri_cut_pitshell_pitshell_id)
                    .map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "The existing ground topology that will be cut by the pit shell."),
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
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            if triangulation_picker_field(
                ui,
                "cut_pitshell_shell",
                tr!(literal = "Pit shell"),
                &mut editor.tri_cut_pitshell_pitshell_id,
                pitshell_label,
                loaded
                    .iter()
                    .filter(|(id, _)| Some(*id) != editor.tri_cut_pitshell_topology_id)
                    .map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "The pit design surface. Only areas where it excavates below the topology are \
                 used for the cut."),
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::CutPitShell);
            }

            ui.add_space(4.0);
            tool_help_panel(
                ui,
                tr!(literal = "Removes the topology where the pit shell excavates below it so the shell \
                 fills the hole. The seam follows the true 3D contact line between the \
                 surfaces; topology under parts of the shell that stand above the ground \
                 is kept."),
            );

            if MenuFieldText::new(tr!(literal = "Output name"), &mut editor.tri_cut_pitshell_name_input)
                .help_text(tr!(literal = "Name assigned to the topology after the pit shell is cut from it."))
                .width(picker_control_width(ui))
                .hint_text(tr!(literal = "e.g. topo_cut"))
                .show(ui)
                .changed()
            {
                editor.tri_cut_pitshell_name_auto = false;
            }

            ui.add_space(6.0);
            ui.separator();

            let can_run = editor.tri_cut_pitshell_topology_id.is_some() && editor.tri_cut_pitshell_pitshell_id.is_some() && !editor.tri_cut_pitshell_name_input.trim().is_empty();

            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if (ui.add(MenuButton::new(tr!(literal = "Cut")).primary().enabled(can_run)).clicked() || (confirm && can_run))
                    && let (Some(topology_id), Some(pit_shell_id)) = (editor.tri_cut_pitshell_topology_id, editor.tri_cut_pitshell_pitshell_id)
                {
                    commands.push(UiCommand::ExecuteCutTopologyByPitShell {
                        topology_id,
                        pit_shell_id,
                        name: editor.tri_cut_pitshell_name_input.trim().to_owned(),
                    });
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new("merge_shell_into_topology_dialog", tr!(literal = "Merge Shell into Topology"))
        .open(&mut open)
        .min_width(PICKER_DIALOG_MIN_WIDTH)
        .max_width(PICKER_DIALOG_MAX_WIDTH)
        .show(ui.ctx(), |ui| {
            let loaded: Vec<(TriangulationId, &str)> = project
                .triangulations
                .iter()
                .filter(|entry| entry.is_loaded)
                .map(|entry| (entry.id, entry.name.as_str()))
                .collect();

            let topology_label = editor
                .tri_include_solid_topology_id
                .and_then(|id| loaded.iter().find(|(loaded_id, _)| *loaded_id == id).map(|(_, name)| *name))
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            let old_topology = editor.tri_include_solid_topology_id;
            if triangulation_picker_field(
                ui,
                "include_solid_topology",
                tr!(literal = "Topology"),
                &mut editor.tri_include_solid_topology_id,
                topology_label,
                loaded.iter().map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "The base topology that will receive the pit or stockpile shape."),
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
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            let topology_id = editor.tri_include_solid_topology_id;
            if triangulation_picker_field(
                ui,
                "include_solid_shape",
                tr!(literal = "Pit/stockpile solid"),
                &mut editor.tri_include_solid_shape_id,
                shape_label,
                loaded.iter().filter(|(id, _)| Some(*id) != topology_id).map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "A closed pit or stockpile solid whose exposed boundary will be included in the \
                 result."),
            ) {
                editor.triangulation_pick_target = Some(TriangulationPickTarget::IncludeShape);
            }

            if MenuFieldText::new(tr!(literal = "Output name"), &mut editor.tri_include_solid_name_input)
                .help_text(tr!(literal = "Name assigned to the merged topology and pit/stockpile result."))
                .width(picker_control_width(ui))
                .hint_text(tr!(literal = "e.g. topo_with_pit"))
                .show(ui)
                .changed()
            {
                editor.tri_include_solid_name_auto = false;
            }
            MenuFieldBool::new(tr!(literal = "Save as two entities"), &mut editor.tri_include_solid_save_as_two)
                .help_text(tr!(literal = "Keep the clipped topology and included shape as separate triangulations instead \
                 of combining them into one entity."))
                .show(ui);
            MenuFieldBool::new(tr!(literal = "Hide and unload sources"), &mut editor.tri_include_solid_hide_old)
                .help_text(tr!(
                    literal = "Once the merge succeeds, unload the source topology and solid so only the merged result stays in the scene."
                ))
                .show(ui);

            ui.add_space(6.0);
            ui.separator();

            let can_run = editor.tri_include_solid_topology_id.is_some()
                && editor.tri_include_solid_shape_id.is_some()
                && editor.tri_include_solid_topology_id != editor.tri_include_solid_shape_id
                && !editor.tri_include_solid_name_input.trim().is_empty();
            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if (ui.add(MenuButton::new(tr!(literal = "Merge")).primary().enabled(can_run)).clicked() || (confirm && can_run))
                    && let (Some(topology_id), Some(shape_id)) = (editor.tri_include_solid_topology_id, editor.tri_include_solid_shape_id)
                {
                    commands.push(UiCommand::ExecuteIncludeSolidInTopology {
                        topology_id,
                        shape_id,
                        name: editor.tri_include_solid_name_input.trim().to_owned(),
                        save_as_two: editor.tri_include_solid_save_as_two,
                        hide_old: editor.tri_include_solid_hide_old,
                    });
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new("generate_contour_lines_dialog", tr!(literal = "Generate Contour Lines"))
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
                .filter(|entry| entry.is_loaded)
                .map(|entry| (entry.id, entry.name.as_str()))
                .collect();
            let tri_label = editor
                .tri_contour_tri_id
                .and_then(|id| loaded.iter().find(|(lid, _)| *lid == id).map(|(_, n)| *n))
                .map(str::to_owned)
                .unwrap_or_else(|| tr!(literal = "Select…"));
            let old_tri_id = editor.tri_contour_tri_id;
            if triangulation_picker_field_with_width(
                ui,
                "contour_tri",
                tr!(literal = "Surface"),
                &mut editor.tri_contour_tri_id,
                tri_label,
                loaded.iter().map(|(id, name)| (Some(*id), (*name).into())),
                tr!(literal = "The surface from which contour lines will be generated."),
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
            MenuField::new(tr!(literal = "Intervals & colours"))
                .help_text(tr!(
                    literal = "Minor controls ordinary contours. Major controls emphasized contours and \
                     must use an interval at least as large as Minor."
                ))
                .show(ui, |ui, row_height, _| {
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
                                    .prefix(tr!(literal = "Minor "))
                                    .max_decimals(3),
                            );
                            ui.color_edit_button_srgba(&mut minor_color);
                            ui.add_sized(
                                [value_width, row_height],
                                egui::DragValue::new(&mut editor.tri_contour_major_interval_input)
                                    .range(1e-6..=f64::MAX)
                                    .speed(0.1)
                                    .prefix(tr!(literal = "Major "))
                                    .max_decimals(3),
                            );
                            ui.color_edit_button_srgba(&mut major_color);
                        },
                    )
                    .response
                });
            editor.tri_contour_minor_color = color32_to_rgba(minor_color);
            editor.tri_contour_major_color = color32_to_rgba(major_color);

            MenuField::new(tr!(literal = "Limit Z range"))
                .help_text(tr!(
                    literal = "When enabled, generate contours only between the specified minimum and \
                     maximum elevations."
                ))
                .show(ui, |ui, row_height, _| {
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
                                        .prefix(tr!(literal = "Min "))
                                        .max_decimals(2),
                                );
                                ui.add_sized(
                                    [value_width, row_height],
                                    egui::DragValue::new(&mut editor.tri_contour_z_max_input)
                                        .range(f64::MIN..=f64::MAX)
                                        .speed(0.1)
                                        .prefix(tr!(literal = "Max "))
                                        .max_decimals(2),
                                );
                            } else {
                                ui.weak(tr!(literal = "Use the full surface elevation range"));
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
                .map(|layer| layer.name.clone())
                .unwrap_or_else(|| tr!(literal = "New layer"));
            MenuFieldCombo::new(
                "contour_output_layer",
                tr!(literal = "Output layer"),
                &mut editor.tri_contour_target_layer,
                output_layer_label,
                std::iter::once((None, tr!(literal = "New layer").into())).chain(
                    active_layers
                        .iter()
                        .map(|layer| (Some(layer.id), layer.name.clone().into())),
                ),
            )
            .help_text(tr!(
                literal = "Create a new layer for the contours or append them to an existing layer in the \
                 active project."
            ))
            .width(control_width)
            .show(ui);

            if editor.tri_contour_target_layer.is_none()
                && MenuFieldText::new(tr!(literal = "New layer name"), &mut editor.tri_contour_layer_name_input)
                    .help_text(tr!(literal = "Name assigned to the newly created contour layer."))
                    .width(control_width)
                    .hint_text(tr!(literal = "e.g. surface_contour"))
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
                ui.colored_label(egui::Color32::LIGHT_RED, tr!(literal = "That layer already exists; select it above or choose another name."));
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

            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if (ui.add(MenuButton::new(tr!(literal = "Generate")).primary().enabled(can_run)).clicked() || (confirm && can_run))
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
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new("point_cloud_create_triangulation_dialog", tr!(literal = "Create Triangulation"))
        .open(&mut open)
        .min_width(400.0)
        .show(ui.ctx(), |ui| {
            tool_help_panel(
                ui,
                tr!(literal = "Reconstruct a triangulated terrain surface from a point cloud. The adaptive \
                 sampler spends the vertex budget where the ground is most complex and keeps \
                 planar areas sparse."),
            );
            ui.add_space(4.0);

            let loaded: Vec<(PointCloudId, &str, usize)> = project
                .point_clouds
                .iter()
                .filter(|cloud| cloud.is_loaded)
                .map(|cloud| (cloud.id, cloud.name.as_str(), cloud.point_count))
                .collect();
            if loaded.is_empty() {
                tool_help_panel(ui, tr!(literal = "No point clouds are loaded. Import one via File ▸ Import first."));
            }
            let selected = editor.point_cloud_tin_cloud_id.and_then(|id| loaded.iter().find(|(lid, ..)| *lid == id).copied());
            let cloud_label = selected.map(|(_, name, _)| name.to_owned()).unwrap_or_else(|| tr!(literal = "Select…"));
            MenuFieldCombo::new(
                "point_cloud_tin_cloud",
                tr!(literal = "Point cloud"),
                &mut editor.point_cloud_tin_cloud_id,
                cloud_label,
                loaded.iter().map(|(id, name, _)| (Some(*id), (*name).into())),
            )
            .help_text(tr!(literal = "The loaded point cloud whose points will be reconstructed into a terrain surface."))
            .width(220.0)
            .show(ui);

            let sampler_label = match editor.point_cloud_tin_sampler {
                TerrainSampler::Adaptive => tr!(literal = "Adaptive (quadtree)"),
                TerrainSampler::Grid => tr!(literal = "Uniform grid"),
            };
            MenuFieldCombo::new(
                "point_cloud_tin_sampler",
                tr!(literal = "Method"),
                &mut editor.point_cloud_tin_sampler,
                sampler_label,
                [
                    (TerrainSampler::Adaptive, tr!(literal = "Adaptive (quadtree)").into()),
                    (TerrainSampler::Grid, tr!(literal = "Uniform grid").into()),
                ],
            )
            .help_text(tr!(literal = "Adaptive concentrates vertices on complex terrain via plane-fit error; uniform \
                 spreads them evenly. More methods may be added in future."))
            .width(220.0)
            .show(ui);

            ui.add_space(4.0);
            ui.separator();

            // Budget: percentage (fractions allowed) or an absolute vertex count.
            let budget_label = if editor.point_cloud_tin_budget_is_percent {
                tr!(literal = "Percentage of cloud")
            } else {
                tr!(literal = "Vertex count")
            };
            MenuFieldCombo::new(
                "point_cloud_tin_budget_mode",
                tr!(literal = "Budget by"),
                &mut editor.point_cloud_tin_budget_is_percent,
                budget_label,
                [(true, tr!(literal = "Percentage of cloud").into()), (false, tr!(literal = "Vertex count").into())],
            )
            .help_text(tr!(literal = "Cap the surface by a share of the source points or by an exact vertex count."))
            .width(220.0)
            .show(ui);

            if editor.point_cloud_tin_budget_is_percent {
                MenuFieldF64::new(tr!(literal = "Percentage"), &mut editor.point_cloud_tin_percent, 0.001..=100.0)
                    .help_text(tr!(literal = "Share of source points to keep. Fractions such as 0.125% are allowed."))
                    .speed(0.05)
                    .suffix(tr!(literal = "%"))
                    .max_decimals(3)
                    .width(220.0)
                    .show(ui);
            } else {
                MenuFieldU32::new(tr!(literal = "Vertex count"), &mut editor.point_cloud_tin_limit, 3..=50_000_000)
                    .help_text(tr!(literal = "Exact number of surface vertices to target. Very large values build slowly \
                     and use significant memory."))
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
                    tr_format!(
                        literal = "Up to %target% of %point_count% points will become surface vertices \
                         (%percent%%).",
                        target = target,
                        point_count = point_count,
                        percent = format!("{percent:.3}")
                    ),
                );
            }

            if editor.point_cloud_tin_sampler == TerrainSampler::Adaptive {
                MenuFieldU32::new(tr!(literal = "Candidate detail"), &mut editor.point_cloud_tin_candidate_mult, 1..=8)
                    .help_text(tr!(literal = "Candidate fine cells per budgeted vertex. Higher gives the adaptive sampler \
                     more freedom to place detail, but is slower to build."))
                    .suffix(tr!(literal = "×"))
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
                        tr!(literal = "Reduce the budget or candidate detail if your machine has less RAM.")
                    } else {
                        tr!(literal = "This exceeds a safe limit; reduce the budget or candidate detail to continue.")
                    };
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .corner_radius(3.0)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(tr!("tri-estimated-memory", estimate = format_bytes(estimate), detail = tail)).color(color));
                        });
                }
            }

            ui.add_space(4.0);
            ui.separator();

            MenuFieldF64::new(tr!(literal = "Max edge length"), &mut editor.point_cloud_tin_max_edge, 0.0..=1_000_000.0)
                .help_text(tr!(literal = "Reject reconstructed triangle edges longer than this distance. Use 0 for no \
                 edge-length limit."))
                .speed(1.0)
                .suffix(tr!(literal = "m"))
                .width(220.0)
                .show(ui);

            MenuFieldF64::new(tr!(literal = "Fill holes up to"), &mut editor.point_cloud_tin_hole_fill, 0.0..=10_000.0)
                .help_text(tr!(literal = "Bridge gaps and boundary concavities narrower than this across the surface. 0 \
                 still bridges gaps up to roughly the sampling cell size; larger values fill \
                 bigger holes and erode boundary concavities."))
                .speed(0.5)
                .suffix(tr!(literal = "m"))
                .max_decimals(2)
                .width(220.0)
                .show(ui);

            ui.add_space(4.0);
            ui.separator();

            MenuFieldText::new(tr!(literal = "Output name"), &mut editor.point_cloud_tin_name_input)
                .help_text(tr!(literal = "Name assigned to the reconstructed triangulation."))
                .width(220.0)
                .hint_text(tr!(literal = "Surface"))
                .show(ui);
            menu::menu_actions(ui, |ui| {
                let can_run = selected.is_some() && !editor.point_cloud_tin_name_input.trim().is_empty() && memory_ok;
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if (ui.add(MenuButton::new(tr!(literal = "Generate")).primary().enabled(can_run)).clicked() || (confirm && can_run))
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
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    editor.point_cloud_tin_open = false;
                }
            });
        });
    if !open {
        editor.point_cloud_tin_open = false;
    }
}
