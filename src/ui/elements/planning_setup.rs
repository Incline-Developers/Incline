//! Shared layout for the Solids and Schedule Setup subpages.
//!
//! Solids' Setup is the Reserves setup: a project-wide Field List (summed,
//! weighted-average, or category columns, e.g. Tonnes, Fe, and Rock Type)
//! and, per block model opted in via its own checkbox, a mapping of that
//! model's own columns or constants onto the list, with the resulting
//! totals. Schedule's Setup is still scaffold - its step tree and
//! content category list are wired up, but item rows and property fields
//! render their empty grids and fill in with the feature. The grids
//! themselves are the reusable [`data_grid`](crate::ui::widgets::data_grid)
//! widgets.
use crate::{
    i18n::{tr, tr_format},
    model::{Document, ReserveAggregation, ReserveFieldId, block_model::{OpenBlockModel, ReserveMappingSource}},
    ui::{
        EditorState, UiProjectView, chrome,
        elements::properties::committed,
        fonts::bold,
        state::{PlanningPage, UiCommand},
        unthemed_icon,
        widgets::{
            context_menu::{ContextMenuAction, context_menu_popup},
            data_grid::{DataGrid, GridRow, PropertyTable, grid_row, property_table_height},
            explorer::{ExplorerEntry, ExplorerHeader, paint_fixed_stripes, reserve_fixed_stripes},
            menu::{MenuFieldCombo, MenuFieldF64},
        },
    },
};

fn striped_list(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
    egui::ScrollArea::vertical().auto_shrink([false; 2]).min_scrolled_height(0.0).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        let (slot, top) = reserve_fixed_stripes(ui);
        content(ui);
        paint_fixed_stripes(ui, slot, top, crate::ui::widgets::tree_row_colors(ui).1);
    });
}

pub(crate) fn draw_steps(ui: &mut egui::Ui, page: PlanningPage) {
    if page == PlanningPage::Solids {
        draw_solids_steps(ui);
        return;
    }
    let selection_id = egui::Id::new(("planning_configuration_selected", page));
    let mut configuration = ui.data(|data| data.get_temp::<bool>(selection_id)).unwrap_or(false);
    striped_list(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            if ExplorerEntry::new(egui::Id::new("planning_configuration"), bold(&tr!("planning-configuration")))
                .leading_icon(unthemed_icon!("step_complete.svg"), egui::Color32::WHITE)
                .header_aligned_icon()
                .selected(configuration)
                .show(ui)
                .response
                .clicked()
            {
                configuration = true;
            }
        });
        ExplorerHeader::new(egui::Id::new("planning_site_data"), tr!("planning-site-data"))
            .icon(unthemed_icon!("step_pending.svg"))
            .show(ui, |ui| {
                if ExplorerEntry::new(egui::Id::new("planning_site_contents"), bold(&tr!("planning-content")))
                    .leading_icon(unthemed_icon!("step_pending.svg"), egui::Color32::WHITE)
                    .header_aligned_icon()
                    .selected(!configuration)
                    .show(ui)
                    .response
                    .clicked()
                {
                    configuration = false;
                }
            });
    });
    ui.data_mut(|data| data.insert_temp(selection_id, configuration));
}

/// Id of the toggle between the Solids Setup's two steps: `false` selects
/// Field List, `true` selects Block Models.
fn solids_step_id() -> egui::Id {
    egui::Id::new("solids_step_selected")
}

fn draw_solids_steps(ui: &mut egui::Ui) {
    let selection_id = solids_step_id();
    let mut block_models_step = ui.data(|data| data.get_temp::<bool>(selection_id)).unwrap_or(false);
    striped_list(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            if ExplorerEntry::new(egui::Id::new("solids_field_list_step"), bold(&tr!("planning-field-list")))
                .leading_icon(unthemed_icon!("step_pending.svg"), egui::Color32::WHITE)
                .header_aligned_icon()
                .selected(!block_models_step)
                .show(ui)
                .response
                .clicked()
            {
                block_models_step = false;
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            if ExplorerEntry::new(egui::Id::new("solids_block_models_step"), bold(&tr!("planning-block-models")))
                .leading_icon(unthemed_icon!("step_pending.svg"), egui::Color32::WHITE)
                .header_aligned_icon()
                .selected(block_models_step)
                .show(ui)
                .response
                .clicked()
            {
                block_models_step = true;
            }
        });
    });
    ui.data_mut(|data| data.insert_temp(selection_id, block_models_step));
}

/// One-line summary of how a field aggregates, for the Field List grid.
fn aggregation_summary(document: &Document, aggregation: &ReserveAggregation) -> String {
    match aggregation {
        ReserveAggregation::Sum => tr!(literal = "Sum"),
        ReserveAggregation::WeightedAverage { weight_field } => {
            let weight_name = document.reserve_field(*weight_field).map_or_else(|| tr!(literal = "?"), |field| field.name.clone());
            tr_format!(literal = "Weighted avg. by %name%", name = weight_name)
        }
        ReserveAggregation::Category => tr!(literal = "Category"),
    }
}

fn draw_field_list(ui: &mut egui::Ui, rect: egui::Rect, editor: &mut EditorState, document: &Document, commands: &mut Vec<UiCommand>) {
    DataGrid::new("reserve_field_list", rect, &tr!("planning-field-list")).show(ui, |ui| {
        for field in document.reserve_fields() {
            let label = tr_format!(literal = "%name% · %aggregation%", name = field.name.clone(), aggregation = aggregation_summary(document, &field.aggregation));
            let response = grid_row(ui, GridRow::new(&label));
            context_menu_popup(&response, &field.name, |ui| {
                if ContextMenuAction::new(tr!(literal = "Rename Field")).show(ui).clicked() {
                    commands.push(UiCommand::BeginRenameItem(crate::ui::state::RenameTarget::ReserveField(field.id)));
                    ui.close();
                }
                if ContextMenuAction::new(tr!(literal = "Delete Field")).show(ui).clicked() {
                    commands.push(UiCommand::DeleteReserveField(field.id));
                    ui.close();
                }
            });
        }
        let body = ui.available_rect_before_wrap();
        if body.is_positive() {
            let response = ui.interact(body, ui.id().with("new_reserve_field_space"), egui::Sense::click());
            context_menu_popup(&response, tr!("planning-field-list"), |ui| {
                if ContextMenuAction::new(tr!(literal = "New Field")).show(ui).clicked() {
                    editor.new_reserve_field_open = true;
                    ui.close();
                }
            });
        }
    });
    crate::ui::dialogs::reserve_fields::draw_new_reserve_field_dialog(ui, editor, document, commands);
}

/// The Block Models step's left column: the project's block models, with
/// their block count. Selecting one drives the mapping panel beside it.
fn draw_block_model_list(ui: &mut egui::Ui, rect: egui::Rect, editor: &mut EditorState, project: &UiProjectView) {
    DataGrid::new("reserve_block_model_list", rect, &tr!("planning-block-models"))
        .column_header(&tr!("planning-name"))
        .show(ui, |ui| {
            if project.block_models.is_empty() {
                crate::ui::widgets::explorer::explorer_note(ui, tr!(literal = "No block models in this project"));
            }
            for entry in &project.block_models {
                let name = if entry.dirty { format!("{} *", entry.name) } else { entry.name.clone() };
                let label = tr_format!(literal = "%name% · %count% blocks", name = name, count = entry.block_count);
                let response = grid_row(ui, GridRow::new(&label).selected(editor.planning_selected_block_model == Some(entry.id))).on_hover_text(tr_format!(
                    literal = "Extents: %lower% → %upper%",
                    lower = format!("{:.1}, {:.1}, {:.1}", entry.lower.x, entry.lower.y, entry.lower.z),
                    upper = format!("{:.1}, {:.1}, {:.1}", entry.upper.x, entry.upper.y, entry.upper.z)
                ));
                if response.clicked() {
                    editor.planning_selected_block_model = Some(entry.id);
                }
            }
        });
}

/// Current mapping choice for one field, as the mapping combo's own value
/// type: the field's un/mapped state plus, when mapped, its source.
#[derive(Clone, PartialEq)]
enum MappingChoice {
    Unmapped,
    Constant,
    Column(String),
}

impl MappingChoice {
    fn of(mapping: &[crate::model::block_model::ReserveFieldMapping], field: ReserveFieldId) -> Self {
        match mapping.iter().find(|entry| entry.field == field).map(|entry| &entry.source) {
            None => Self::Unmapped,
            Some(ReserveMappingSource::Constant(_)) => Self::Constant,
            Some(ReserveMappingSource::Column(name)) => Self::Column(name.clone()),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Unmapped => tr!(literal = "Unmapped"),
            Self::Constant => tr!(literal = "Constant"),
            Self::Column(name) => name.clone(),
        }
    }
}

/// The Block Models step's right column: the selected model's mapping of the
/// project's Field List onto its own columns/constants, and the computed
/// totals that follow from it.
fn draw_block_model_mapping(ui: &mut egui::Ui, rect: egui::Rect, document: &Document, model: &OpenBlockModel, commands: &mut Vec<UiCommand>) {
    let header_height = property_table_height(ui, 3).min(rect.height());
    let header_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), header_height));
    let fields_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), header_rect.bottom() + 6.0), rect.max);

    let mut included = model.included_in_reserves;
    PropertyTable::new("reserve_block_model_mapping", header_rect, &model.name).show(ui, |rows| {
        rows.header(&tr!("planning-property"), &tr!("planning-value"));
        rows.readonly(
            &tr!(literal = "Extents"),
            &format!(
                "{:.1}, {:.1}, {:.1} → {:.1}, {:.1}, {:.1}",
                model.model.metadata.lower.x, model.model.metadata.lower.y, model.model.metadata.lower.z, model.model.metadata.upper.x, model.model.metadata.upper.y, model.model.metadata.upper.z
            ),
            None,
            None,
        );
        rows.checkbox(&tr!(literal = "Used for reserving"), &mut included);
    });
    if included != model.included_in_reserves {
        commands.push(UiCommand::SetReserveModelIncluded { block_model: model.id, included });
    }

    let numeric_columns: Vec<_> = model.model.numeric_variables().into_iter().map(|variable| variable.name.clone()).collect();
    let categorical_columns: Vec<_> = model.model.categorical_variables().into_iter().map(|variable| variable.name.clone()).collect();
    ui.scope_builder(egui::UiBuilder::new().max_rect(fields_rect), |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(fields_rect));
        egui::ScrollArea::vertical().id_salt("reserve_mapping_fields").auto_shrink([false; 2]).show(ui, |ui| {
            for field in document.reserve_fields() {
                let is_category = matches!(field.aggregation, ReserveAggregation::Category);
                let current = MappingChoice::of(&model.reserve_mapping, field.id);
                let mut choice = current.clone();
                let mut options = vec![(MappingChoice::Unmapped, tr!(literal = "Unmapped").into())];
                if !is_category {
                    options.push((MappingChoice::Constant, tr!(literal = "Constant").into()));
                }
                let columns = if is_category { &categorical_columns } else { &numeric_columns };
                options.extend(columns.iter().map(|name| (MappingChoice::Column(name.clone()), name.clone().into())));
                MenuFieldCombo::new(("reserve_mapping_kind", model.id, field.id), field.name.clone(), &mut choice, current.label(), options).show(ui);
                if choice != current {
                    let source = match &choice {
                        MappingChoice::Unmapped => None,
                        MappingChoice::Constant => Some(ReserveMappingSource::Constant(0.0)),
                        MappingChoice::Column(name) => Some(ReserveMappingSource::Column(name.clone())),
                    };
                    commands.push(UiCommand::SetReserveMapping { block_model: model.id, field: field.id, source });
                }
                if matches!(current, MappingChoice::Constant) {
                    let mut value = model
                        .reserve_mapping
                        .iter()
                        .find(|entry| entry.field == field.id)
                        .and_then(|entry| match &entry.source {
                            ReserveMappingSource::Constant(value) => Some(*value),
                            ReserveMappingSource::Column(_) => None,
                        })
                        .unwrap_or(0.0);
                    let response = MenuFieldF64::new(tr!(literal = "Value"), &mut value, f64::MIN..=f64::MAX).show(ui);
                    if committed(&response) {
                        commands.push(UiCommand::SetReserveMapping {
                            block_model: model.id,
                            field: field.id,
                            source: Some(ReserveMappingSource::Constant(value)),
                        });
                    }
                }
                if !is_category {
                    let total = model.reserve_totals.get(&field.id);
                    ui.horizontal(|ui| {
                        ui.add_space(ui.spacing().indent);
                        ui.label(match total {
                            Some(total) if model.included_in_reserves => tr_format!(literal = "Total: %total%", total = format!("{total:.2}")),
                            Some(_) => tr!(literal = "Total: excluded from reserves"),
                            None => tr!(literal = "Total: —"),
                        });
                    });
                }
                ui.separator();
            }
        });
    });
}

fn draw_solids_details(ui: &mut egui::Ui, rect: egui::Rect, editor: &mut EditorState, project: &UiProjectView, document: &Document, block_models: &[OpenBlockModel], commands: &mut Vec<UiCommand>) {
    let block_models_step = ui.data(|data| data.get_temp::<bool>(solids_step_id())).unwrap_or(false);
    if !block_models_step {
        let table = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width().min(560.0), rect.height()));
        draw_field_list(ui, table, editor, document, commands);
        ui.allocate_rect(rect, egui::Sense::hover());
        return;
    }
    let gap = 12.0;
    let left_width = (rect.width() * 0.3).clamp(200.0, 320.0);
    let left = egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + left_width, rect.bottom()));
    let right = egui::Rect::from_min_max(egui::pos2(left.right() + gap, rect.top()), rect.max);
    draw_block_model_list(ui, left, editor, project);
    if let Some(model) = editor.planning_selected_block_model.and_then(|id| block_models.iter().find(|model| model.id == id)) {
        draw_block_model_mapping(ui, right, document, model, commands);
    } else {
        PropertyTable::new("reserve_block_model_mapping_empty", right, &tr!("planning-block-models")).show(ui, |rows| {
            rows.header(&tr!("planning-property"), &tr!("planning-value"));
        });
    }
    ui.allocate_rect(rect, egui::Sense::hover());
}

fn category_labels() -> [String; 4] {
    [tr!("planning-dumps"), tr!("planning-stockpiles"), tr!("planning-loaders"), tr!("planning-trucks")]
}

fn draw_content_categories(ui: &mut egui::Ui, rect: egui::Rect, page: PlanningPage) -> usize {
    let category_id = egui::Id::new(("planning_site_category", page));
    let mut category = ui.data(|data| data.get_temp::<usize>(category_id)).unwrap_or(0).min(3);
    DataGrid::new("planning_categories", rect, &tr!("planning-content"))
        .column_header(&tr!("planning-content-type"))
        .show(ui, |ui| {
            for (index, label) in category_labels().iter().enumerate() {
                if grid_row(ui, GridRow::new(label).selected(category == index)).clicked() {
                    category = index;
                }
            }
        });
    ui.data_mut(|data| data.insert_temp(category_id, category));
    category
}

fn draw_items(ui: &mut egui::Ui, rect: egui::Rect, category: usize) {
    let label = &category_labels()[category];
    let new_label = match category {
        0 => tr!("planning-new-dump"),
        1 => tr!("planning-new-stockpile"),
        2 => tr!("planning-new-loader"),
        _ => tr!("planning-new-truck"),
    };
    DataGrid::new("planning_items", rect, label).column_header(&tr!("planning-name")).show(ui, |ui| {
        // No item rows yet; the whole body is free space that offers "New ...".
        let body = ui.available_rect_before_wrap();
        if body.is_positive() {
            let response = ui.interact(body, ui.id().with(("new_item_space", category)), egui::Sense::click());
            context_menu_popup(&response, label, |ui| {
                ContextMenuAction::new(new_label.clone()).enabled(false).show(ui);
            });
        }
    });
}

fn draw_item_properties(ui: &mut egui::Ui, rect: egui::Rect) {
    PropertyTable::new("planning_properties", rect, &tr!("planning-properties")).show(ui, |rows| {
        rows.header(&tr!("planning-property"), &tr!("planning-value"));
        // Fields arrive once an item is selectable.
    });
}

fn draw_configuration(ui: &mut egui::Ui, rect: egui::Rect, page: PlanningPage) {
    let name_id = egui::Id::new(("planning_schedule_name", page));
    let mut schedule_name = ui.data(|data| data.get_temp::<String>(name_id)).unwrap_or_default();
    PropertyTable::new("planning_configuration", rect, &tr!("planning-configuration")).show(ui, |rows| {
        rows.header(&tr!("planning-property"), &tr!("planning-value"));
        rows.field(&tr!("planning-schedule-name"), &mut schedule_name, None);
    });
    ui.data_mut(|data| data.insert_temp(name_id, schedule_name));
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_details(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    project: &UiProjectView,
    document: &Document,
    block_models: &[OpenBlockModel],
    commands: &mut Vec<UiCommand>,
    page: PlanningPage,
) -> egui::Rect {
    egui::CentralPanel::default()
        .frame(chrome::region_frame(ui))
        .show(ui, |ui| {
            let available = ui.available_rect_before_wrap();
            let area = available.shrink2(egui::vec2(0.0, 10.0_f32.min(available.height() * 0.5)));
            if page == PlanningPage::Solids {
                draw_solids_details(ui, area, editor, project, document, block_models, commands);
                return;
            }
            let configuration = ui
                .data(|data| data.get_temp::<bool>(egui::Id::new(("planning_configuration_selected", page))))
                .unwrap_or(false);
            if configuration {
                let table = egui::Rect::from_min_size(area.min, egui::vec2(area.width().min(520.0), area.height()));
                draw_configuration(ui, table, page);
                ui.allocate_rect(area, egui::Sense::hover());
                return;
            }
            let gap = 12.0;
            let column_space = (area.width() - gap * 2.0).max(0.0);
            let left_width = (column_space * 0.25).min(260.0);
            let left = egui::Rect::from_min_max(area.min, egui::pos2(area.left() + left_width, area.bottom()));
            let middle = egui::Rect::from_min_size(egui::pos2(left.right() + gap, area.top()), egui::vec2((column_space * 0.32).min(360.0), area.height()));
            let properties_origin = egui::pos2(middle.right() + gap, area.top());
            let properties = egui::Rect::from_min_size(properties_origin, egui::vec2((area.right() - properties_origin.x).clamp(0.0, 440.0), area.height()));

            let category = draw_content_categories(ui, left, page);
            draw_items(ui, middle, category);
            draw_item_properties(ui, properties);
            ui.allocate_rect(area, egui::Sense::hover());
        })
        .response
        .rect
}
