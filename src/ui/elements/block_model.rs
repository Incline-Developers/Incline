use glam::DVec3;

use crate::{
    i18n::{tr, tr_format},
    model::{
        block_model::OpenBlockModel,
        drill_hole::{DrillFieldKind, OpenDrillHoleDataset},
        kriging::KrigingGrid,
    },
    ui::{
        EditorState, UiCommand,
        state::OreFilterMode,
        widgets::menu::{self, DragableMenu, MenuButton, MenuFieldCombo, MenuFieldF64, MenuFieldText, MenuFieldU32, menu_field_label},
    },
};

pub(crate) fn draw_create_block_model_dialog(ui: &mut egui::Ui, editor: &mut EditorState, drill_holes: &[OpenDrillHoleDataset], commands: &mut Vec<UiCommand>) {
    let mut open = true;
    DragableMenu::new(tr!(literal = "Create Block Model"))
        .open(&mut open)
        .min_width(390.0)
        .show(ui.ctx(), |ui| {
            menu::menu_note(
                ui,
                tr!(literal = "Ordinary Kriging estimates numeric drill-hole intervals at each block centre using a spherical variogram."),
            );
            ui.add_space(4.0);

            let selected_label = editor
                .kriging_drill_hole_id
                .and_then(|id| drill_holes.iter().find(|dataset| dataset.id == id))
                .map(|dataset| dataset.name.clone())
                .unwrap_or_else(|| tr!(literal = "Choose Drill Holes"));
            let dataset_changed = MenuFieldCombo::new(
                "kriging_drill_holes",
                tr!(literal = "Drill Holes"),
                &mut editor.kriging_drill_hole_id,
                selected_label,
                drill_holes.iter().map(|dataset| (Some(dataset.id), dataset.name.clone().into())),
            )
            .help_text(tr!(literal = "Loaded Drill Holes collection containing numeric interval values."))
            .show(ui)
            .changed();

            let selected_dataset = editor.kriging_drill_hole_id.and_then(|id| drill_holes.iter().find(|dataset| dataset.id == id));
            if dataset_changed && let Some(dataset) = selected_dataset {
                editor.kriging_variables = dataset
                    .dataset
                    .fields
                    .iter()
                    .find(|field| matches!(field.kind, DrillFieldKind::Numeric { .. }))
                    .map(|field| vec![field.key.clone()])
                    .unwrap_or_default();
                initialise_grid_from_dataset(editor, dataset);
            }

            let numeric_fields: Vec<_> = selected_dataset
                .map(|dataset| dataset.dataset.fields.iter().filter(|field| matches!(field.kind, DrillFieldKind::Numeric { .. })).collect())
                .unwrap_or_default();
            editor.kriging_variables.retain(|selected| numeric_fields.iter().any(|field| field.key == *selected));
            let selected_labels: Vec<_> = numeric_fields
                .iter()
                .filter(|field| editor.kriging_variables.contains(&field.key))
                .map(|field| field.label.as_str())
                .collect();
            let selected_text = match selected_labels.as_slice() {
                [] => tr!(literal = "Choose numeric variables"),
                [label] => (*label).to_owned(),
                labels => tr_format!(literal = "%count% variables selected", count = labels.len()),
            };
            let mut variable_changed = false;
            // Resolved out here: inside the row's own `ui` the column would be
            // measured against this row alone rather than the whole dialog.
            let estimate_variables_label = tr!(literal = "Estimate variables");
            let column_width = menu::field_column_for(ui, &estimate_variables_label, true);
            ui.horizontal(|ui| {
                menu_field_label(
                    ui,
                    estimate_variables_label.clone().into(),
                    Some(tr!(literal = "Numeric interval fields to interpolate. Each selected field becomes one block-model variable.").into()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("kriging_variables")
                        .selected_text(selected_text)
                        .width(column_width)
                        .show_ui(ui, |ui| {
                            ui.horizontal(|ui| {
                                if ui.small_button(tr!(literal = "Select all")).clicked() {
                                    editor.kriging_variables = numeric_fields.iter().map(|field| field.key.clone()).collect();
                                    variable_changed = true;
                                }
                                if ui.small_button(tr!(literal = "Clear")).clicked() {
                                    editor.kriging_variables.clear();
                                    variable_changed = true;
                                }
                            });
                            ui.separator();
                            for field in &numeric_fields {
                                let mut selected = editor.kriging_variables.contains(&field.key);
                                if ui.checkbox(&mut selected, &field.label).changed() {
                                    variable_changed = true;
                                    if selected {
                                        editor.kriging_variables.push(field.key.clone());
                                    } else {
                                        editor.kriging_variables.retain(|key| key != &field.key);
                                    }
                                }
                            }
                        });
                });
            });
            if variable_changed
                && let Some(DrillFieldKind::Numeric { min, max }) = editor
                    .kriging_variables
                    .first()
                    .and_then(|variable| selected_dataset.and_then(|dataset| dataset.dataset.field(variable)))
                    .map(|field| &field.kind)
            {
                let spread = max - min;
                editor.kriging_sill = (spread * spread / 12.0).max(1.0e-6);
            }
            MenuFieldText::new(tr!(literal = "Output name"), &mut editor.kriging_name_input).show(ui);

            menu::menu_section(ui, tr!(literal = "Block grid"));
            vector_fields(
                ui,
                &tr!(literal = "Minimum"),
                &tr!(literal = "Lower X, Y and Z edges of the block-model volume. Block centres begin half a block inside these limits."),
                &mut editor.kriging_lower,
                f64::MIN..=f64::MAX,
            );
            vector_fields(
                ui,
                &tr!(literal = "Maximum"),
                &tr!(literal = "Upper X, Y and Z extent to cover. The last block may extend past this extent when the span is not an exact multiple of block size."),
                &mut editor.kriging_upper,
                f64::MIN..=f64::MAX,
            );
            vector_fields(
                ui,
                &tr!(literal = "Block size"),
                &tr!(literal = "Full X, Y and Z dimensions of each block. Smaller blocks increase detail, computation time and memory use."),
                &mut editor.kriging_cell,
                0.001..=f64::MAX,
            );

            let grid = KrigingGrid {
                lower: editor.kriging_lower,
                upper: editor.kriging_upper,
                cell: editor.kriging_cell,
            };
            let dimensions = grid.dimensions().ok();
            let block_count = dimensions.and_then(|dims| dims.into_iter().try_fold(1usize, |count, dim| count.checked_mul(dim)));
            match (dimensions, block_count) {
                (Some(dims), Some(count)) => {
                    ui.small(tr!("block-grid-summary", x = dims[0], y = dims[1], z = dims[2], count = count));
                }
                _ => {
                    ui.colored_label(ui.visuals().error_fg_color, tr!(literal = "Grid bounds or block sizes are invalid."));
                }
            }

            menu::menu_section(ui, tr!(literal = "Spherical variogram and search"));
            MenuFieldF64::new(tr!(literal = "Range / search radius"), &mut editor.kriging_range, 0.001..=f64::MAX)
                .help_text(tr!(literal = "Samples farther than this distance are excluded; covariance reaches zero at this range."))
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Partial sill"), &mut editor.kriging_sill, 0.000001..=f64::MAX)
                .help_text(tr!(
                    literal = "Spatially correlated variance contributed by the spherical model. Together with the nugget, it sets covariance at zero distance."
                ))
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Nugget"), &mut editor.kriging_nugget, 0.0..=f64::MAX)
                .help_text(tr!(
                    literal =
                        "Variance at effectively zero separation caused by measurement error or variation below the sampling scale. Use zero when no nugget effect is intended."
                ))
                .show(ui);
            MenuFieldU32::new(tr!(literal = "Minimum samples"), &mut editor.kriging_min_samples, 1..=64)
                .help_text(tr!(
                    literal = "Minimum nearby samples required to estimate a block. Blocks with fewer samples inside the search radius are left empty."
                ))
                .show(ui);
            MenuFieldU32::new(tr!(literal = "Maximum samples"), &mut editor.kriging_max_samples, 1..=64)
                .help_text(tr!(
                    literal = "Maximum nearest samples used for each block. Lower values run faster; higher values can smooth estimates and increase computation time."
                ))
                .show(ui);

            let ready = editor.kriging_drill_hole_id.is_some()
                && !editor.kriging_variables.is_empty()
                && !editor.kriging_name_input.trim().is_empty()
                && block_count.is_some()
                && editor.kriging_range.is_finite()
                && editor.kriging_range > 0.0
                && editor.kriging_sill.is_finite()
                && editor.kriging_sill > 0.0
                && editor.kriging_nugget.is_finite()
                && editor.kriging_nugget >= 0.0
                && editor.kriging_min_samples > 0
                && editor.kriging_min_samples <= editor.kriging_max_samples
                && editor.kriging_max_samples <= 64;
            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if ui.add(MenuButton::new(tr!(literal = "Create")).primary().enabled(ready)).clicked() || (confirm && ready) {
                    commands.push(UiCommand::ExecuteCreateBlockModel {
                        drill_hole_id: editor.kriging_drill_hole_id.unwrap(),
                        variables: editor.kriging_variables.clone(),
                        name: editor.kriging_name_input.trim().to_owned(),
                        lower: editor.kriging_lower,
                        upper: editor.kriging_upper,
                        cell: editor.kriging_cell,
                        range: editor.kriging_range,
                        sill: editor.kriging_sill,
                        nugget: editor.kriging_nugget,
                        min_samples: editor.kriging_min_samples,
                        max_samples: editor.kriging_max_samples,
                    });
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    editor.block_model_create_open = false;
                }
            });
        });
    if !open {
        editor.block_model_create_open = false;
    }
}

fn vector_fields(ui: &mut egui::Ui, label: &str, help_text: &str, value: &mut glam::DVec3, range: std::ops::RangeInclusive<f64>) {
    // Three boxes sharing the column the fields above them use, so the grid
    // rows line up with the rest of the dialog rather than sizing themselves
    // to whatever numbers they happen to hold.
    let row_height = ui.spacing().interact_size.y;
    let gap = ui.spacing().item_spacing.x;
    let each = ((menu::field_column_for(ui, label, true) - gap * 2.0) / 3.0).max(36.0);
    ui.horizontal(|ui| {
        menu_field_label(ui, label.into(), Some(help_text.into()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_sized(
                [each, row_height],
                egui::DragValue::new(&mut value.z)
                    .range(range.clone())
                    .prefix(tr!(literal = "Z "))
                    .speed(1.0)
                    .max_decimals(3),
            );
            ui.add_sized(
                [each, row_height],
                egui::DragValue::new(&mut value.y)
                    .range(range.clone())
                    .prefix(tr!(literal = "Y "))
                    .speed(1.0)
                    .max_decimals(3),
            );
            ui.add_sized(
                [each, row_height],
                egui::DragValue::new(&mut value.x).range(range).prefix(tr!(literal = "X ")).speed(1.0).max_decimals(3),
            );
        });
    });
}

fn initialise_grid_from_dataset(editor: &mut EditorState, dataset: &OpenDrillHoleDataset) {
    let Some((lower, upper)) = dataset.dataset.bounds else {
        return;
    };
    let span = upper - lower;
    let characteristic = span.max_element().max(1.0);
    let padding = DVec3::splat(characteristic * 0.025);
    editor.kriging_lower = lower - padding;
    editor.kriging_upper = upper + padding;
    editor.kriging_cell = DVec3::splat((characteristic / 25.0).max(0.001));
    editor.kriging_range = (characteristic / 3.0).max(editor.kriging_cell.max_element());
    if let Some(DrillFieldKind::Numeric { min, max }) = editor
        .kriging_variables
        .first()
        .and_then(|variable| dataset.dataset.field(variable))
        .map(|field| &field.kind)
    {
        let spread = max - min;
        editor.kriging_sill = (spread * spread / 12.0).max(1.0e-6);
    }
}

pub(crate) fn draw_ore_triangulation_dialog(ui: &mut egui::Ui, editor: &mut EditorState, block_models: &[OpenBlockModel], commands: &mut Vec<UiCommand>) {
    let mut open = true;
    DragableMenu::new(tr!(literal = "Create Ore Triangulation"))
        .open(&mut open)
        .min_width(340.0)
        .show(ui.ctx(), |ui| {
            let selected_label = editor
                .ore_block_model_id
                .and_then(|id| block_models.iter().find(|model| model.id == id))
                .map(|model| model.name.clone())
                .unwrap_or_else(|| tr!(literal = "Choose a block model"));
            MenuFieldCombo::new(
                "ore_block_model",
                tr!(literal = "Block model"),
                &mut editor.ore_block_model_id,
                selected_label,
                block_models.iter().map(|model| (Some(model.id), model.name.clone().into())),
            )
            .show(ui);

            let variables: Vec<String> = editor
                .ore_block_model_id
                .and_then(|id| block_models.iter().find(|model| model.id == id))
                .map(|model| model.model.numeric_variables().into_iter().filter(|var| !var.special).map(|var| var.name.clone()).collect())
                .unwrap_or_default();
            if !variables.is_empty() && !variables.contains(&editor.ore_variable) {
                editor.ore_variable = variables[0].clone();
            }
            let variable_label = if editor.ore_variable.is_empty() {
                tr!(literal = "Choose a numeric variable")
            } else {
                editor.ore_variable.clone()
            };
            MenuFieldCombo::new(
                "ore_variable",
                tr!(literal = "Variable"),
                &mut editor.ore_variable,
                variable_label.as_str(),
                variables.iter().map(|name| (name.clone(), name.clone().into())),
            )
            .show(ui);

            let ge_threshold_label = tr!(literal = ">= threshold");
            let le_threshold_label = tr!(literal = "<= threshold");
            let between_label = tr!(literal = "Between");
            let mode_label = match editor.ore_filter_mode {
                OreFilterMode::GreaterOrEqual => ge_threshold_label.clone(),
                OreFilterMode::LessOrEqual => le_threshold_label.clone(),
                OreFilterMode::Between => between_label.clone(),
            };
            MenuFieldCombo::new(
                "ore_filter_mode",
                tr!(literal = "Filter"),
                &mut editor.ore_filter_mode,
                mode_label,
                [
                    (OreFilterMode::GreaterOrEqual, ge_threshold_label.into()),
                    (OreFilterMode::LessOrEqual, le_threshold_label.into()),
                    (OreFilterMode::Between, between_label.into()),
                ],
            )
            .show(ui);

            MenuFieldF64::new(tr!(literal = "Threshold / min"), &mut editor.ore_min_input, f64::MIN..=f64::MAX).show(ui);
            if editor.ore_filter_mode == OreFilterMode::Between {
                MenuFieldF64::new(tr!(literal = "Max"), &mut editor.ore_max_input, f64::MIN..=f64::MAX).show(ui);
            }
            MenuFieldText::new(tr!(literal = "Output name"), &mut editor.ore_name_input).show(ui);

            let min = editor.ore_min_input;
            let max = editor.ore_max_input;
            let ready = editor.ore_block_model_id.is_some()
                && !editor.ore_variable.is_empty()
                && !editor.ore_name_input.trim().is_empty()
                && min.is_finite()
                && (editor.ore_filter_mode != OreFilterMode::Between || max.is_finite());
            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if ui.add(MenuButton::new(tr!(literal = "Create")).primary().enabled(ready)).clicked() || (confirm && ready) {
                    commands.push(UiCommand::ExecuteCreateOreTriangulation {
                        block_model_id: editor.ore_block_model_id.unwrap(),
                        variable: editor.ore_variable.clone(),
                        mode: editor.ore_filter_mode,
                        min,
                        max: if editor.ore_filter_mode == OreFilterMode::Between { max } else { min },
                        name: editor.ore_name_input.trim().to_owned(),
                    });
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    editor.ore_triangulation_open = false;
                }
            });
        });
    if !open {
        editor.ore_triangulation_open = false;
    }
}
