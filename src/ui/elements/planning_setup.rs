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
    model::{
        BenchInterval, BenchingPlan, Document, ReserveAggregation, ReserveFieldId, SolidEdit, SolidKind,
        block_model::{OpenBlockModel, ReserveMappingSource},
    },
    ui::{
        EditorState, UiProjectView, chrome,
        dialogs::solids::{block_model_label, block_model_options, kind_label, triangulation_label, triangulation_options},
        elements::properties::committed,
        fonts::bold,
        state::{PlanningPage, SolidsStep, UiCommand},
        unthemed_icon,
        widgets::{
            context_menu::{ContextMenuAction, context_menu_popup},
            data_grid::{DataGrid, GridNumber, GridRow, PropertyTable, grid_number_row, grid_row, grid_style_row, property_table_height},
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

pub(crate) fn draw_steps(ui: &mut egui::Ui, editor: &mut EditorState, page: PlanningPage, commands: &mut Vec<UiCommand>) {
    if page == PlanningPage::Solids {
        draw_solids_steps(ui, editor, commands);
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

fn draw_solids_steps(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let mut step = editor.planning_solids_step;
    striped_list(ui, |ui| {
        let mut markers = Vec::with_capacity(SolidsStep::ALL.len());
        for entry in SolidsStep::ALL {
            let status = &editor.planning_stages[entry.index()];
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                let entry_response = ExplorerEntry::new(egui::Id::new(entry.tree_id()), bold(&entry.label()))
                    .leading_icon(step_icon(status.state), stage_tint(ui, status.state))
                    .header_aligned_icon()
                    .selected(step == entry)
                    .show(ui);
                if let Some(rect) = entry_response.icon_rect {
                    markers.push((rect, status.state));
                }
                let response = entry_response.response;
                if response.clicked() {
                    step = entry;
                }
                draw_stage_menu(&response, entry, editor.planning_run_active, commands);
            });
        }
        // Paint after all rows so their backgrounds cannot cover the links.
        // Green reaches the first red or yellow badge. Red then reaches the
        // first yellow badge, where the connecting line stops.
        let mut failed = false;
        for pair in markers.windows(2) {
            use crate::app::planning_pipeline::StageState;
            let (previous, state) = pair[0];
            let (next, _) = pair[1];
            failed |= matches!(state, StageState::Failed | StageState::Blocked);
            if !matches!(state, StageState::Complete | StageState::Failed | StageState::Blocked) {
                break;
            }
            let color = if failed {
                egui::Color32::from_rgb(0xDC, 0x45, 0x45)
            } else {
                egui::Color32::from_rgb(0x2E, 0xAD, 0x62)
            };
            // The circular badges occupy 12.2 px inside their 16 px SVGs.
            let start = previous.center() + egui::vec2(0.0, 6.1);
            let end = next.center() - egui::vec2(0.0, 6.1);
            if end.y > start.y {
                ui.painter().line_segment([start, end], egui::Stroke::new(2.0, color));
            }
        }
    });
    editor.planning_solids_step = step;
}

/// Run Step and Run All: a light muted green, filled for the one and outlined
/// for the other.
const RUN_STEP_TINT: egui::Color32 = egui::Color32::from_rgb(0x76, 0xC3, 0x8D);
/// Run All: the same green as Run - the pair is one control, told apart by
/// the filled head against the outlined pair rather than by shade.
const RUN_ALL_TINT: egui::Color32 = RUN_STEP_TINT;
/// Cancel: a soft red, muted well below the error red the failed stages carry
/// so it is a button rather than an alarm.
const CANCEL_TINT: egui::Color32 = egui::Color32::from_rgb(0xCB, 0x63, 0x63);

/// Contents of the separate run-control island at the top of the sidebar.
pub(crate) fn draw_solids_run_controls(ui: &mut egui::Ui, editor: &EditorState, commands: &mut Vec<UiCommand>) {
    use crate::{app::planning_pipeline::StageState, ui::widgets::toolbar::ToolbarButton};

    // The icons are white line art, so the tint is the button's whole colour.
    // A disabled run is dimmed rather than greyed: the colour still says which
    // button it is while it waits for the run to finish.
    let run_enabled = !editor.planning_run_active;
    let tint = |color: egui::Color32, enabled: bool| if enabled { color } else { color.gamma_multiply(0.35) };

    let side = ui.available_height();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let step = editor.planning_solids_step;
        if !editor.is_solids_view()
            && ui
                .add_enabled(
                    !editor.planning_run_active,
                    ToolbarButton::new(
                        egui::Image::new(crate::ui::unthemed_icon!("play.svg")).tint(tint(RUN_STEP_TINT, run_enabled)),
                        tr!("stage-run-step"),
                    )
                    .button_side(side)
                    .id_salt("planning_run_through"),
                )
                .clicked()
        {
            commands.push(UiCommand::RunPlanningStage(step));
        }
        if ui
            .add_enabled(
                !editor.planning_run_active,
                ToolbarButton::new(
                    egui::Image::new(crate::ui::unthemed_icon!("play_all.svg")).tint(tint(RUN_ALL_TINT, run_enabled)),
                    tr!("stage-run-all"),
                )
                .button_side(side)
                .id_salt("planning_run_all"),
            )
            .clicked()
        {
            commands.push(UiCommand::RunAllPlanningStages);
        }
        if ui
            .add_enabled(
                editor.planning_run_active,
                ToolbarButton::new(
                    egui::Image::new(crate::ui::unthemed_icon!("stop.svg")).tint(tint(CANCEL_TINT, editor.planning_run_active)),
                    tr!("stage-cancel"),
                )
                .button_side(side)
                .id_salt("planning_run_cancel"),
            )
            .clicked()
        {
            commands.push(UiCommand::CancelPlanningRun);
        }
        let completed = editor.planning_stages.iter().filter(|stage| stage.state == StageState::Complete).count();
        let active = SolidsStep::ALL.into_iter().find(|step| editor.planning_stages[step.index()].state == StageState::Running);
        let reported = active.unwrap_or(if editor.is_solids_view() { SolidsStep::DigStrips } else { step });
        let status = &editor.planning_stages[reported.index()];
        let label = tr!("stage-progress", done = completed, total = SolidsStep::ALL.len());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::ui::widgets::progress::draw_planning_progress(ui, &label, completed as f32 / SolidsStep::ALL.len() as f32);
        })
        .response
        .on_hover_ui(|ui| {
            ui.label(reported.label());
            stage_tooltip(ui, status);
            if !editor.planning_snapshot_status.is_empty() {
                ui.separator();
                ui.label(&editor.planning_snapshot_status);
            }
        });
    });
}

fn step_icon(state: crate::app::planning_pipeline::StageState) -> egui::ImageSource<'static> {
    match state.icon() {
        "step_complete.svg" => unthemed_icon!("step_complete.svg"),
        "step_error.svg" => unthemed_icon!("step_error.svg"),
        _ => unthemed_icon!("step_pending.svg"),
    }
}

/// Preserve the SVG colours for pending and completed badges.
fn stage_tint(ui: &egui::Ui, state: crate::app::planning_pipeline::StageState) -> egui::Color32 {
    use crate::app::planning_pipeline::StageState;
    match state {
        StageState::Complete => egui::Color32::WHITE,
        StageState::Failed | StageState::Blocked => ui.visuals().error_fg_color,
        _ => egui::Color32::WHITE,
    }
}

fn stage_tooltip(ui: &mut egui::Ui, status: &crate::ui::state::PlanningStageView) {
    ui.label(bold(&status.state.label()));
    if let Some(blocker) = status.blocked_by {
        ui.label(tr!("stage-blocked-by", stage = blocker.label()));
    }
    if let Some(message) = &status.message {
        ui.label(message);
    }
    if let Some(summary) = &status.last_success {
        ui.label(tr!("stage-last-run", generation = summary.generation.to_string(), entities = summary.entities.to_string()));
    }
    if status.diagnostics.is_empty() {
        return;
    }
    ui.separator();
    ui.label(bold(&tr!("stage-diagnostics")));
    for entry in status.diagnostics.iter().take(12) {
        let text = match &entry.entity {
            Some(entity) => format!("{entity}: {}", entry.message),
            None => entry.message.clone(),
        };
        let color = if entry.blocking { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
        ui.label(egui::RichText::new(text).color(color));
    }
    if status.diagnostics.len() > 12 {
        ui.label(egui::RichText::new(format!("… {}", status.diagnostics.len() - 12)).color(ui.visuals().weak_text_color()));
    }
}

fn draw_stage_menu(response: &egui::Response, stage: SolidsStep, running: bool, commands: &mut Vec<UiCommand>) {
    context_menu_popup(response, stage.label(), |ui| {
        if ContextMenuAction::new(tr!("stage-run-step")).enabled(!running).show(ui).clicked() {
            commands.push(UiCommand::RunPlanningStage(stage));
            ui.close();
        }
        if ContextMenuAction::new(tr!("stage-run-all")).enabled(!running).show(ui).clicked() {
            commands.push(UiCommand::RunAllPlanningStages);
            ui.close();
        }
        if ContextMenuAction::new(tr!("stage-cancel")).enabled(running).show(ui).clicked() {
            commands.push(UiCommand::CancelPlanningRun);
            ui.close();
        }
    });
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
            let label = tr_format!(
                literal = "%name% · %aggregation%",
                name = field.name.clone(),
                aggregation = aggregation_summary(document, &field.aggregation)
            );
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
                model.model.metadata.lower.x,
                model.model.metadata.lower.y,
                model.model.metadata.lower.z,
                model.model.metadata.upper.x,
                model.model.metadata.upper.y,
                model.model.metadata.upper.z
            ),
            None,
            None,
        );
        rows.checkbox(&tr!(literal = "Used for reserving"), &mut included);
    });
    if included != model.included_in_reserves {
        commands.push(UiCommand::SetReserveModelIncluded { block_model: model.id, included });
    }
    // A scan that failed, or one whose inputs could not be loaded, leaves the
    // figures absent; this is how it is asked for again without editing the
    // mapping to force a new request key.
    let header_response = ui.interact(header_rect, ui.id().with(("reserve_stats_retry", model.id)), egui::Sense::click());
    context_menu_popup(&header_response, &model.name, |ui| {
        if ContextMenuAction::new(tr!("planning-recompute-stats")).show(ui).clicked() {
            commands.push(UiCommand::RecomputeReserveStats(model.id));
            ui.close();
        }
    });

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
                    commands.push(UiCommand::SetReserveMapping {
                        block_model: model.id,
                        field: field.id,
                        source,
                    });
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
                    let stats = model.reserve_totals.get(&field.id);
                    let number = |value: Option<f64>| value.map_or_else(|| "—".to_owned(), |value| format!("{value:.2}"));
                    let label = if matches!(field.aggregation, ReserveAggregation::WeightedAverage { .. }) {
                        tr!("planning-stat-avg")
                    } else {
                        tr!("planning-stat-sum")
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(ui.spacing().indent);
                        ui.label(format!("{label}: {}", number(stats.and_then(|s| s.total))));
                        ui.label(format!("{}: {}", tr!("planning-stat-min"), number(stats.and_then(|s| s.min))));
                        ui.label(format!("{}: {}", tr!("planning-stat-max"), number(stats.and_then(|s| s.max))));
                    });
                    // A dash says which of the several possible reasons it is:
                    // unmapped, an absent column, a length mismatch, an
                    // unusable weight, a failed scan, one still running, or
                    // simply not scanned yet.
                    let reason = match stats {
                        Some(stats) => stats.issue.as_ref().map(crate::model::ReserveFieldIssue::describe),
                        None => Some(if let Some(error) = &model.reserve_totals_error {
                            tr!("planning-stat-scan-failed", error = error.clone())
                        } else if model.reserve_totals_awaiting_restore {
                            tr!("planning-stat-loading")
                        } else if model.reserve_totals_key.is_some() {
                            tr!("planning-stat-scanning")
                        } else {
                            tr!("planning-stat-not-scanned")
                        }),
                    };
                    if let Some(reason) = reason {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(ui.spacing().indent);
                            ui.label(egui::RichText::new(reason).color(ui.visuals().warn_fg_color));
                        });
                    }
                    if let Some(stats) = stats.filter(|stats| stats.missing_values > 0 || stats.unusable_weights > 0) {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(ui.spacing().indent);
                            ui.label(
                                egui::RichText::new(tr!(
                                    "stage-model-data-gaps",
                                    missing = stats.missing_values.to_string(),
                                    weights = stats.unusable_weights.to_string()
                                ))
                                .color(ui.visuals().weak_text_color()),
                            );
                        });
                    }
                }
                ui.separator();
            }
        });
    });
}

/// The Solids step's left column: the project's solids, each with the kind of
/// volume it is. Selecting one drives the property table beside it.
fn draw_solid_list(ui: &mut egui::Ui, rect: egui::Rect, editor: &mut EditorState, document: &Document, commands: &mut Vec<UiCommand>) {
    DataGrid::new("planning_solid_list", rect, &tr!("planning-solids"))
        .column_header(&tr!("planning-name"))
        .show(ui, |ui| {
            for solid in document.solids() {
                let label = tr_format!(literal = "%name% · %kind%", name = solid.name.clone(), kind = kind_label(solid.kind));
                let response = grid_row(ui, GridRow::new(&label).selected(editor.planning_selected_solid == Some(solid.id)));
                if response.clicked() && editor.planning_selected_solid != Some(solid.id) {
                    editor.planning_selected_solid = Some(solid.id);
                    editor.planning_selected_bench = None;
                    ui.ctx().request_repaint();
                }
                context_menu_popup(&response, &solid.name, |ui| {
                    if ContextMenuAction::new(tr!(literal = "Rename Solid")).show(ui).clicked() {
                        commands.push(UiCommand::BeginRenameItem(crate::ui::state::RenameTarget::Solid(solid.id)));
                        ui.close();
                    }
                    if ContextMenuAction::new(tr!(literal = "Delete Solid")).show(ui).clicked() {
                        commands.push(UiCommand::DeleteSolid(solid.id));
                        ui.close();
                    }
                });
            }
            let body = ui.available_rect_before_wrap();
            if body.is_positive() {
                let response = ui.interact(body, ui.id().with("new_solid_space"), egui::Sense::click());
                context_menu_popup(&response, tr!("planning-solids"), |ui| {
                    if ContextMenuAction::new(tr!(literal = "New Solid")).show(ui).clicked() {
                        editor.new_solid_open = true;
                        ui.close();
                    }
                });
            }
        });
}

/// The Solids step's right column: the selected solid's surfaces, kind and
/// block model. A pit is reserved against its model, so leaving that unset
/// is flagged on the row; a dump or stockpile is placed material and needs
/// none.
fn draw_solid_properties(ui: &mut egui::Ui, rect: egui::Rect, project: &UiProjectView, solid: &crate::model::Solid, commands: &mut Vec<UiCommand>) {
    PropertyTable::new("planning_solid_properties", rect, &solid.name).show(ui, |rows| {
        rows.header(&tr!("planning-property"), &tr!("planning-value"));
        rows.readonly(&tr!("planning-name"), &solid.name, None, None);

        let mut color = solid.color;
        if rows.color(&tr!(literal = "Colour"), &mut color).changed() {
            commands.push(UiCommand::UpdateSolid {
                solid: solid.id,
                edit: SolidEdit::Color(color),
            });
        }

        let mut kind = solid.kind;
        if rows
            .combo(
                ("solid_kind", solid.id),
                &tr!(literal = "Type"),
                &mut kind,
                &kind_label(solid.kind),
                SolidKind::ALL.into_iter().map(|kind| (kind, kind_label(kind))),
            )
            .changed()
        {
            commands.push(UiCommand::UpdateSolid {
                solid: solid.id,
                edit: SolidEdit::Kind(kind),
            });
        }

        let surfaces = triangulation_options(project);
        let mut surface = solid.surface;
        if rows
            .combo(
                ("solid_surface", solid.id),
                &tr!(literal = "Surface"),
                &mut surface,
                &triangulation_label(project, solid.surface),
                surfaces.clone(),
            )
            .changed()
        {
            commands.push(UiCommand::UpdateSolid {
                solid: solid.id,
                edit: SolidEdit::Surface(surface),
            });
        }

        let mut topography = solid.topography;
        if rows
            .combo(
                ("solid_topography", solid.id),
                &tr!(literal = "Topography"),
                &mut topography,
                &triangulation_label(project, solid.topography),
                surfaces,
            )
            .changed()
        {
            commands.push(UiCommand::UpdateSolid {
                solid: solid.id,
                edit: SolidEdit::Topography(topography),
            });
        }

        let mut block_model = solid.block_model;
        if rows
            .combo(
                ("solid_block_model", solid.id),
                &tr!(literal = "Block Model"),
                &mut block_model,
                &block_model_label(project, solid.block_model),
                block_model_options(project),
            )
            .changed()
        {
            commands.push(UiCommand::UpdateSolid {
                solid: solid.id,
                edit: SolidEdit::BlockModel(block_model),
            });
        }

        if solid.kind.requires_block_model() && solid.block_model.is_none() {
            rows.readonly(&tr!(literal = "Reserves"), &tr!(literal = "Set a block model to reserve this pit"), None, None);
        }
    });
}

/// The Solids step's render pane: the solid itself, drawn by the renderer into
/// an offscreen texture (see [`crate::rendering::graphics::solid_preview`])
/// and painted here over the rest of the loaded project.
///
/// A solid with only a design surface shows that surface; one that also names
/// a topography shows the closed volume between the two, with its enclosed
/// volume captioned. Drag to orbit, scroll to zoom.
pub(crate) fn draw_solid_render(ui: &mut egui::Ui, rect: egui::Rect, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let title = tr!(literal = "Preview");
    framed_render_pane(ui, rect, &title, |ui, body| {
        let caption_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        let image_rect = egui::Rect::from_min_max(body.min, egui::pos2(body.right(), (body.bottom() - caption_height).max(body.top())));
        if !image_rect.is_positive() {
            return;
        }

        // Paint first, then claim the area for input. Interaction is taken
        // with an id of its own rather than riding on an `Image` response, so
        // it stays the topmost interactive widget over the pane whichever
        // branch painted it.
        // A rebuild keeps the solid it is replacing on screen, so the image is
        // live for that too - not only once the new one lands.
        let ready = (editor.is_solids_view() && editor.solid_preview_texture.is_some())
            || matches!(
                editor.solid_preview_summary,
                crate::ui::state::SolidPreviewSummary::Ready { .. }
                    | crate::ui::state::SolidPreviewSummary::Building { showing_previous: true }
                    | crate::ui::state::SolidPreviewSummary::LoadingInputs { showing_previous: true }
            );
        match editor.solid_preview_texture {
            Some(texture_id) if ready => {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(texture_id, image_rect, uv, egui::Color32::WHITE);
            }
            _ => {
                ui.painter().rect_filled(image_rect, 0.0, crate::ui::widgets::tree_row_colors(ui).1);
            }
        }
        let response = ui.interact(image_rect, ui.id().with("solid_preview_view"), egui::Sense::click_and_drag());

        let pixels_per_point = ui.ctx().pixels_per_point();
        let size_px = [
            (image_rect.width() * pixels_per_point).round().max(1.0) as u32,
            (image_rect.height() * pixels_per_point).round().max(1.0) as u32,
        ];
        // The renderer works a frame behind this measurement, so a resized
        // column needs one more frame to redraw at its new size.
        let mut view_changed = editor.solid_preview_size_px != size_px;
        editor.solid_preview_size_px = size_px;

        // The main viewport's own mapping: middle drag pans, right drag
        // orbits, left click selects and never moves the camera. Setup's
        // inspection previews keep left-drag orbit, having nothing to select.
        let selects = editor.is_solids_view();
        let delta = response.drag_delta() * pixels_per_point;
        if delta != egui::Vec2::ZERO {
            let delta = [f64::from(delta.x), f64::from(delta.y)];
            if response.dragged_by(egui::PointerButton::Middle) {
                editor.solid_preview_view.pan_by_pixels(delta, f64::from(size_px[1]));
                view_changed = true;
            } else if response.dragged_by(egui::PointerButton::Secondary) || (!selects && response.dragged_by(egui::PointerButton::Primary)) {
                editor.solid_preview_view.orbit_by_pixels(delta, f64::from(size_px[1]));
                view_changed = true;
            }
        }
        if response.hovered() || response.dragged() {
            // In View a click picks the solid under it, so hovering reads as
            // something to click; the grab hand is for the drag that orbits.
            ui.ctx().set_cursor_icon(if response.dragged() {
                egui::CursorIcon::Grabbing
            } else if selects {
                egui::CursorIcon::PointingHand
            } else {
                egui::CursorIcon::Grab
            });
        }
        // Double click resets the camera only where left click is not a
        // selection. Fit and Reset are explicit buttons in View, so a fast
        // second click on a dig block cannot throw the camera away.
        if !selects && response.double_clicked() {
            editor.solid_preview_view = crate::ui::state::SolidPreviewView::default();
            view_changed = true;
        }
        // A click that did not drag selects the solid under it. In View that is
        // how a dig block, which the tree does not list, is picked out for its
        // own figures. Recorded as a fraction of the image: the renderer sizes
        // its target within limits of its own, and a pane outside them is drawn
        // at one size and would otherwise be picked against another.
        if response.clicked()
            && selects
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let local = pointer - image_rect.min;
            editor.solid_preview_pick_uv = Some([local.x / image_rect.width().max(1.0), local.y / image_rect.height().max(1.0)]);
            ui.ctx().request_repaint();
        }
        if response.hovered() {
            let scroll = ui.input(|input| {
                input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::MouseWheel { unit, delta, .. } => Some(match unit {
                            egui::MouseWheelUnit::Point => f64::from(delta.y * pixels_per_point),
                            // One wheel line is about a hundred physical
                            // pixels, matching the main camera's convention.
                            egui::MouseWheelUnit::Line => f64::from(delta.y) * 100.0,
                            egui::MouseWheelUnit::Page => f64::from(delta.y) * f64::from(size_px[1]),
                        }),
                        _ => None,
                    })
                    .sum::<f64>()
            });
            if scroll != 0.0 {
                editor.solid_preview_view.zoom_by_scroll(scroll);
                view_changed = true;
            }
        }
        if view_changed {
            ui.ctx().request_repaint();
        }
        // The viewport's own orientation gizmo, over the preview image and
        // driving the preview's orbit: one gizmo in the app, not two.
        if ready && editor.show_world_axis_gizmo {
            let (_, up) = editor.solid_preview_view.screen_basis();
            let forward = editor.solid_preview_view.forward();
            let gizmo = crate::ui::elements::cursors::draw_orientation_gizmo(
                ui,
                egui::Id::new("solid_preview_orientation_gizmo"),
                image_rect,
                forward.as_vec3().to_array(),
                up.as_vec3().to_array(),
            );
            if let Some(view) = gizmo.clicked {
                editor.solid_preview_view.face(view);
                ui.ctx().request_repaint();
            }
        }

        // A built solid is only a preview until it is asked for by name; a
        // lone surface is already in the project, so there is nothing to add.
        let can_save = !editor.is_solids_view() && matches!(editor.solid_preview_summary, crate::ui::state::SolidPreviewSummary::Ready { volume: Some(_), .. });
        context_menu_popup(&response, &title, |ui| {
            if ContextMenuAction::new(tr!(literal = "Save Solid to Project")).enabled(can_save).show(ui).clicked() {
                commands.push(UiCommand::SaveSolidPreviewToProject);
                ui.close();
            }
            if ContextMenuAction::new(tr!(literal = "Reset View")).show(ui).clicked() {
                commands.push(UiCommand::ResetSolidPreviewView);
                ui.close();
            }
        });

        let caption_rect = egui::Rect::from_min_max(egui::pos2(body.left() + 8.0, image_rect.bottom()), body.max);
        let caption = match &editor.solid_preview_summary {
            crate::ui::state::SolidPreviewSummary::NotRun => tr!("planning-not-run"),
            crate::ui::state::SolidPreviewSummary::Empty => tr!(literal = "Set a surface to inspect this solid"),
            crate::ui::state::SolidPreviewSummary::Unloaded => tr!(literal = "Load this solid's surfaces to inspect it"),
            crate::ui::state::SolidPreviewSummary::LoadingInputs { .. } => tr!(literal = "Loading this solid's surfaces…"),
            crate::ui::state::SolidPreviewSummary::Building { showing_previous: true } => tr!(literal = "Rebuilding solid…"),
            crate::ui::state::SolidPreviewSummary::Building { .. } => tr!(literal = "Building solid…"),
            crate::ui::state::SolidPreviewSummary::Ready { volume: Some(volume), faces, .. } => {
                tr_format!(literal = "%volume% m³ · %faces% faces", volume = format!("{volume:.1}"), faces = faces.to_string())
            }
            crate::ui::state::SolidPreviewSummary::Ready { volume: None, .. } if editor.is_solids_view() => tr!("planning-volume-unavailable"),
            crate::ui::state::SolidPreviewSummary::Ready {
                volume: None,
                waiting_on_unloaded: true,
                ..
            } => tr!(literal = "Surface only · the other surface is not loaded"),
            crate::ui::state::SolidPreviewSummary::Ready { volume: None, faces, .. } => {
                tr_format!(literal = "Surface only · %faces% faces", faces = faces.to_string())
            }
            crate::ui::state::SolidPreviewSummary::Failed(message) => message.clone(),
        };
        let failed = matches!(editor.solid_preview_summary, crate::ui::state::SolidPreviewSummary::Failed(_));
        let color = if failed { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
        ui.put(
            caption_rect,
            egui::Label::new(egui::RichText::new(caption).color(color)).truncate().halign(egui::Align::Min),
        );
    });
}

/// The bordered, titled frame the render pane shares with the grids beside
/// it, handing its body rect to `content` instead of laying rows out.
fn framed_render_pane(ui: &mut egui::Ui, rect: egui::Rect, title: &str, content: impl FnOnce(&mut egui::Ui, egui::Rect)) {
    ui.scope_builder(egui::UiBuilder::new().id_salt(("planning_framed_pane", title.to_owned())).max_rect(rect), |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(rect));
        ui.painter().rect_filled(rect, 0.0, crate::ui::widgets::tree_row_colors(ui).1);
        let title_height = property_table_height(ui, 0);
        let title_rect = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 0.0), egui::vec2((rect.width() - 8.0).max(0.0), title_height));
        ui.put(title_rect, egui::Label::new(bold(title)).truncate().halign(egui::Align::Min));
        let body = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + title_height), rect.max);
        if body.is_positive() {
            content(ui, body);
        }
        ui.painter().rect_stroke(rect, 0.0, ui.visuals().widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
    });
}

/// Regions and native panel resize handles painted after the workspace.
pub(crate) struct PlanningLayout {
    pub(crate) rect: egui::Rect,
    pub(crate) regions: Vec<egui::Rect>,
    pub(crate) grips: Vec<chrome::Grip>,
}

impl Default for PlanningLayout {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            regions: Vec::new(),
            grips: Vec::new(),
        }
    }
}

fn island<R>(ui: &mut egui::Ui, layout: &mut PlanningLayout, id: &'static str, width: f32, content: impl FnOnce(&mut egui::Ui, egui::Rect) -> R) -> R {
    let response = egui::Panel::left(id)
        .resizable(true)
        .default_size(width)
        .min_size(120.0)
        .show_separator_line(chrome::show_separator_line(ui))
        .frame(chrome::region_frame(ui).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
            content(ui, ui.available_rect_before_wrap())
        });
    layout.regions.push(response.response.rect);
    layout.grips.push(chrome::Grip::new(response.response.rect, chrome::Edge::Right, id));
    response.inner
}

fn central_island(ui: &mut egui::Ui, layout: &mut PlanningLayout, content: impl FnOnce(&mut egui::Ui, egui::Rect)) {
    let response = egui::CentralPanel::default()
        .frame(chrome::region_frame(ui).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| content(ui, ui.available_rect_before_wrap()));
    layout.regions.push(response.response.rect);
}

fn objects_island(ui: &mut egui::Ui, layout: &mut PlanningLayout, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    let id = "planning_objects_island";
    let frame = chrome::region_frame(ui).inner_margin(egui::Margin::ZERO);
    let separator = chrome::show_separator_line(ui);
    let collapsed = egui::Panel::right("planning_objects_collapsed")
        .resizable(true)
        .exact_size(26.0)
        .frame(frame)
        .show_separator_line(separator);
    let expanded = egui::Panel::right(id)
        .resizable(true)
        .default_size(280.0)
        .min_size(140.0)
        .frame(frame)
        .show_separator_line(separator);
    let title = tr!(literal = "Objects");
    let mut open = editor.planning_solid_objects_open;
    // Clicking the collapsed spine opens it as well, which the resize handle's
    // drag and double-click cannot advertise on their own.
    let mut clicked = false;
    let response = egui::Panel::show_switched(ui, &mut open, collapsed, expanded, |ui, expanded| {
        let rect = ui.available_rect_before_wrap();
        ui.set_clip_rect(ui.clip_rect().intersect(rect));
        if expanded {
            framed_render_pane(ui, rect, &title, |ui, body| {
                ui.scope_builder(egui::UiBuilder::new().id_salt("planning_solid_objects").max_rect(body), |ui| {
                    ui.set_clip_rect(ui.clip_rect().intersect(body));
                    ui.set_min_size(body.size());
                    ui.painter().rect_filled(body, 0.0, crate::ui::widgets::tree_row_colors(ui).0);
                    crate::ui::elements::explorer::draw_object_tree(ui, editor, project, commands);
                });
            });
        } else {
            // Collapsed: a narrow spine carrying the title down the page, so
            // the strip still says what dragging it back open would show.
            let response = ui.interact(rect, ui.id().with("planning_objects_spine"), egui::Sense::click());
            if response.hovered() {
                ui.painter().rect_filled(rect, 0.0, ui.visuals().widgets.hovered.bg_fill);
            }
            let galley = ui.painter().layout_no_wrap(title.clone(), egui::FontId::proportional(11.0), ui.visuals().text_color());
            // Rotated a quarter turn so the label reads up the spine.
            let text = egui::epaint::TextShape::new(
                egui::pos2(rect.center().x - galley.size().y * 0.5, rect.top() + 12.0 + galley.size().x),
                galley,
                ui.visuals().text_color(),
            )
            .with_angle(-std::f32::consts::FRAC_PI_2);
            ui.painter().with_clip_rect(rect).add(text);
            if response.clicked() {
                clicked = true;
            }
        }
    });
    editor.planning_solid_objects_open = open || clicked;
    layout.regions.push(response.response.rect);
    layout.grips.push(chrome::Grip::new(response.response.rect, chrome::Edge::Left, id));
}

fn draw_solids_step(ui: &mut egui::Ui, layout: &mut PlanningLayout, editor: &mut EditorState, project: &UiProjectView, document: &Document, commands: &mut Vec<UiCommand>) {
    objects_island(ui, layout, editor, project, commands);
    island(ui, layout, "planning_solids_list_island", 240.0, |ui, rect| {
        draw_solid_list(ui, rect, editor, document, commands)
    });
    island(ui, layout, "planning_solids_properties_island", 320.0, |ui, rect| {
        match editor.planning_selected_solid.and_then(|id| document.solid(id)) {
            Some(solid) => draw_solid_properties(ui, rect, project, solid, commands),
            None => PropertyTable::new("planning_solid_properties_empty", rect, &tr!("planning-properties")).show(ui, |rows| {
                rows.header(&tr!("planning-property"), &tr!("planning-value"));
            }),
        }
    });
    central_island(ui, layout, |ui, rect| draw_solid_render(ui, rect, editor, commands));
    crate::ui::dialogs::solids::draw_new_solid_dialog(ui, editor, project, commands);
}

/// The Benching step's first column: the RL the solid is benched down from,
/// and the bench height for each range beneath it.
///
/// The list reads top down, the way a pit is described: each row is an RL and
/// the bench height that applies below it, and the last row is the RL the
/// bottom range ends at. Inserting a row splits the range it sits in.
fn draw_benching_list(ui: &mut egui::Ui, rect: egui::Rect, plan: &mut BenchingPlan, changed: &mut bool) {
    DataGrid::new("planning_bench_list", rect, &tr!("planning-benching"))
        .column_header(&tr!(literal = "RL · Bench height"))
        .show(ui, |ui| {
            *changed |= grid_number_row(ui, ("bench_top", 0usize), [GridNumber::Edit(&mut plan.top), GridNumber::Blank], None, false).1;
            let mut edit = None;
            for (index, interval) in plan.intervals.iter_mut().enumerate() {
                let (response, edited) = grid_number_row(
                    ui,
                    ("bench_interval", index),
                    [GridNumber::Edit(&mut interval.base), GridNumber::Edit(&mut interval.bench)],
                    None,
                    false,
                );
                *changed |= edited;
                context_menu_popup(&response, tr!(literal = "Range"), |ui| {
                    if ContextMenuAction::new(tr!(literal = "Insert Range Below")).show(ui).clicked() {
                        edit = Some((index, true));
                        ui.close();
                    }
                    if ContextMenuAction::new(tr!(literal = "Delete Range")).show(ui).clicked() {
                        edit = Some((index, false));
                        ui.close();
                    }
                });
            }
            match edit {
                // Splitting a range halves it: the new row takes the lower
                // part and inherits the heights, which is the edit a reader
                // then adjusts rather than one they have to reconstruct.
                Some((index, true)) => {
                    let above = if index == 0 { plan.top } else { plan.intervals[index - 1].base };
                    let interval = plan.intervals[index].clone();
                    let split = (above + interval.base) / 2.0;
                    plan.intervals.insert(index, BenchInterval { base: split, ..interval });
                    *changed = true;
                }
                Some((index, false)) => {
                    plan.intervals.remove(index);
                    *changed = true;
                }
                None => {}
            }
            let body = ui.available_rect_before_wrap();
            if body.is_positive() {
                let response = ui.interact(body, ui.id().with("new_bench_range"), egui::Sense::click());
                context_menu_popup(&response, tr!("planning-benching"), |ui| {
                    if ContextMenuAction::new(tr!(literal = "Add Range")).show(ui).clicked() {
                        let base = plan.intervals.last().map_or(plan.top, |interval| interval.base);
                        plan.intervals.push(BenchInterval {
                            base: base - DEFAULT_RANGE_DEPTH,
                            bench: BenchingPlan::DEFAULT_BENCH,
                            flitch: BenchingPlan::DEFAULT_FLITCH,
                            styles: Vec::new(),
                        });
                        *changed = true;
                        ui.close();
                    }
                });
            }
        });
}

/// The Benching step's second column: the flitch height for each of the same
/// ranges.
///
/// The RLs are the benching list's and are shown greyed, because a flitch
/// divides a bench and cannot start anywhere else. A height that is not a
/// whole number of flitches to the bench is painted in the error colour.
/// What one flitch position is called: the ends are named, the rest counted.
fn flitch_position_label(position: usize, count: usize) -> String {
    if position == 0 {
        return tr!(literal = "Top flitch");
    }
    if position + 1 == count {
        return tr!(literal = "Bottom flitch");
    }
    tr_format!(literal = "Flitch %index%", index = (position + 1).to_string())
}

fn pattern_label(pattern: crate::model::FillStyle) -> String {
    match pattern {
        crate::model::FillStyle::Clear => tr!(literal = "None"),
        crate::model::FillStyle::Crosses => tr!(literal = "Crosses"),
        crate::model::FillStyle::Slashes => tr!(literal = "Slashes"),
        crate::model::FillStyle::Solid => tr!(literal = "Solid"),
    }
}

fn draw_flitching_list(ui: &mut egui::Ui, rect: egui::Rect, plan: &mut BenchingPlan, solid_color: [f32; 4], changed: &mut bool) {
    DataGrid::new("planning_flitch_list", rect, &tr!(literal = "Flitching"))
        .column_header(&tr!(literal = "RL · Flitch height"))
        .show(ui, |ui| {
            let top = plan.top;
            grid_number_row(ui, ("flitch_top", 0usize), [GridNumber::Fixed(top), GridNumber::Blank], None, false);
            for (index, interval) in plan.intervals.iter_mut().enumerate() {
                let error = if interval.flitch > 0.0 && (interval.bench / interval.flitch).ceil() > 64.0 {
                    Some(tr!("planning-too-many-flitches"))
                } else {
                    (!interval.flitch_divides_bench()).then(|| {
                        tr_format!(
                            literal = "A %bench% m bench is not a whole number of %flitch% m flitches",
                            bench = format!("{:.2}", interval.bench),
                            flitch = format!("{:.2}", interval.flitch)
                        )
                    })
                };
                let base = interval.base;
                *changed |= grid_number_row(
                    ui,
                    ("flitch_interval", index),
                    [GridNumber::Fixed(base), GridNumber::Edit(&mut interval.flitch)],
                    error.as_deref(),
                    false,
                )
                .1;

                // One row per flitch position in the range's bench, top down,
                // carrying how that position is drawn wherever it recurs.
                let count = interval.flitch_count();
                if interval.styles.len() != count {
                    // Changing a height changes how many flitches a bench has.
                    // Positions that survive keep what they were given; new
                    // ones take the default shade for where they now sit.
                    interval.styles = (0..count)
                        .map(|position| {
                            interval
                                .styles
                                .get(position)
                                .copied()
                                .unwrap_or_else(|| crate::model::FlitchStyle::default_for(solid_color, position, count))
                        })
                        .collect();
                    *changed = true;
                }
                for (position, style) in interval.styles.iter_mut().enumerate() {
                    let label = flitch_position_label(position, count);
                    *changed |= grid_style_row(
                        ui,
                        ("flitch_style", index, position),
                        &label,
                        &mut style.color,
                        &mut style.pattern_color,
                        &mut style.pattern,
                        pattern_label,
                    );
                }
            }
        });
}

/// The Benching step's third column: every bench the plan produces, bottom up,
/// with its flitches grouped underneath it.
///
/// Selecting a row picks that slice out in the preview.
fn draw_bench_results(ui: &mut egui::Ui, rect: egui::Rect, plan: &BenchingPlan, editor: &mut EditorState) {
    DataGrid::new("planning_bench_results", rect, &tr!(literal = "Results"))
        .column_header(&tr!(literal = "Bench · Flitch"))
        .show(ui, |ui| {
            // A plan may run past the solid at either end - it is snapped out
            // to whole benches, and the topography cuts the solid short of the
            // design. Only the slices that actually hold some of it are worth
            // listing, or picking out in the preview.
            let occupied = editor.solid_preview_z_range;
            let holds_solid = |base: f64, top: f64| occupied.is_none_or(|(lowest, highest)| top > lowest + BAND_EPSILON && base < highest - BAND_EPSILON);
            let benches: Vec<_> = plan.benches().into_iter().filter(|bench| holds_solid(bench.base, bench.top())).collect();
            if benches.is_empty() {
                let note = if plan.intervals.is_empty() {
                    tr!(literal = "No benches yet - add a range")
                } else {
                    tr!(literal = "No bench holds any of this solid")
                };
                crate::ui::widgets::explorer::explorer_note(ui, note);
                return;
            }
            // Bottom up, so the list reads the way a pit is mined and the way
            // its benches are named.
            for bench in benches.iter().rev() {
                let selection = crate::ui::state::BenchSelection {
                    base: bench.base,
                    top: bench.top(),
                    is_flitch: false,
                };
                let label = tr_format!(
                    literal = "%base% → %top% (%height% m)",
                    base = format!("{:.2}", bench.base),
                    top = format!("{:.2}", bench.top()),
                    height = format!("{:.2}", bench.height)
                );
                if grid_row(ui, GridRow::new(&label).selected(editor.planning_selected_bench == Some(selection))).clicked() {
                    editor.planning_selected_bench = (editor.planning_selected_bench != Some(selection)).then_some(selection);
                }
                for flitch in bench.flitches.iter().rev().filter(|flitch| holds_solid(flitch.base, flitch.top())) {
                    let selection = crate::ui::state::BenchSelection {
                        base: flitch.base,
                        top: flitch.top(),
                        is_flitch: true,
                    };
                    let label = tr_format!(literal = "    %base% → %top%", base = format!("{:.2}", flitch.base), top = format!("{:.2}", flitch.top()));
                    if grid_row(ui, GridRow::new(&label).selected(editor.planning_selected_bench == Some(selection))).clicked() {
                        editor.planning_selected_bench = (editor.planning_selected_bench != Some(selection)).then_some(selection);
                    }
                }
            }
        });
}

/// Slack on the solid's own extent when deciding whether a bench holds any of
/// it, so a bench boundary that lands exactly on the crest or the floor does
/// not list an empty slice.
const BAND_EPSILON: f64 = 1e-3;

/// Depth a range added by hand takes, so it lands visibly below the one above
/// rather than collapsed onto it. Its heights are the plan's own defaults.
const DEFAULT_RANGE_DEPTH: f64 = 120.0;

fn draw_benching_step(ui: &mut egui::Ui, layout: &mut PlanningLayout, editor: &mut EditorState, project: &UiProjectView, document: &Document, commands: &mut Vec<UiCommand>) {
    objects_island(ui, layout, editor, project, commands);
    island(ui, layout, "planning_bench_solids_island", 240.0, |ui, rect| {
        draw_solid_list(ui, rect, editor, document, commands)
    });
    let selected = editor.planning_selected_solid.and_then(|id| document.solid(id));
    let solid_id = selected.map(|solid| solid.id);
    let solid_color = selected.map_or([1.0; 4], |solid| solid.color);
    let mut plan = selected.map(|solid| solid.benching.clone()).unwrap_or_default();
    let mut changed = false;

    // This column only arranges two independent islands; it has no shared frame.
    let settings_id = "planning_bench_settings_column";
    let settings = egui::Panel::left(settings_id)
        .resizable(true)
        .default_size(240.0)
        .min_size(140.0)
        .show_separator_line(chrome::show_separator_line(ui))
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            let flitch_id = "planning_flitching_island";
            let flitch = egui::Panel::bottom(flitch_id)
                .resizable(true)
                .default_size(ui.available_height() * 0.6)
                .min_size(100.0)
                .show_separator_line(chrome::show_separator_line(ui))
                .frame(chrome::region_frame(ui).inner_margin(egui::Margin::ZERO))
                .show(ui, |ui| {
                    let rect = ui.available_rect_before_wrap();
                    if solid_id.is_some() {
                        draw_flitching_list(ui, rect, &mut plan, solid_color, &mut changed);
                    } else {
                        PropertyTable::new("planning_flitching_empty", rect, &tr!(literal = "Flitching")).show(ui, |rows| {
                            rows.header(&tr!("planning-property"), &tr!("planning-value"));
                        });
                    }
                });
            layout.regions.push(flitch.response.rect);
            layout.grips.push(chrome::Grip::new(flitch.response.rect, chrome::Edge::Top, flitch_id));
            central_island(ui, layout, |ui, rect| {
                if solid_id.is_some() {
                    draw_benching_list(ui, rect, &mut plan, &mut changed);
                } else {
                    PropertyTable::new("planning_benching_empty", rect, &tr!("planning-benching")).show(ui, |rows| {
                        rows.header(&tr!("planning-property"), &tr!("planning-value"));
                        rows.readonly(&tr!("planning-solids"), &tr!("planning-select-solid"), None, None);
                    });
                }
            });
        });
    layout.grips.push(chrome::Grip::new(settings.response.rect, chrome::Edge::Right, settings_id));
    island(ui, layout, "planning_bench_results_island", 240.0, |ui, rect| {
        if solid_id.is_some() {
            draw_bench_results(ui, rect, &plan, editor);
        } else {
            PropertyTable::new("planning_bench_results_empty", rect, &tr!(literal = "Results")).show(ui, |rows| {
                rows.header(&tr!("planning-property"), &tr!("planning-value"));
            });
        }
    });
    central_island(ui, layout, |ui, rect| draw_solid_render(ui, rect, editor, commands));
    crate::ui::dialogs::solids::draw_new_solid_dialog(ui, editor, project, commands);
    if let Some(solid_id) = solid_id
        && changed
    {
        plan.intervals.sort_by(|a, b| b.base.total_cmp(&a.base));
        commands.push(UiCommand::UpdateSolid {
            solid: solid_id,
            edit: SolidEdit::Benching(plan),
        });
    }
}

fn draw_solids_details(
    ui: &mut egui::Ui,
    layout: &mut PlanningLayout,
    editor: &mut EditorState,
    project: &UiProjectView,
    document: &Document,
    block_models: &[OpenBlockModel],
    commands: &mut Vec<UiCommand>,
) {
    match editor.planning_solids_step {
        SolidsStep::Blasting | SolidsStep::DigStrips => {}
        // One table with nothing beside it, so it is the workspace rather
        // than a column of it.
        SolidsStep::FieldList => central_island(ui, layout, |ui, rect| draw_field_list(ui, rect, editor, document, commands)),
        SolidsStep::Solids => draw_solids_step(ui, layout, editor, project, document, commands),
        SolidsStep::Benching => draw_benching_step(ui, layout, editor, project, document, commands),
        SolidsStep::BlockModels => {
            island(ui, layout, "planning_models_island", 280.0, |ui, rect| draw_block_model_list(ui, rect, editor, project));
            central_island(ui, layout, |ui, rect| {
                if let Some(model) = editor.planning_selected_block_model.and_then(|id| block_models.iter().find(|model| model.id == id)) {
                    draw_block_model_mapping(ui, rect, document, model, commands);
                } else {
                    PropertyTable::new("reserve_block_model_mapping_empty", rect, &tr!("planning-block-models")).show(ui, |rows| {
                        rows.header(&tr!("planning-property"), &tr!("planning-value"));
                    });
                }
            });
        }
    }
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
) -> PlanningLayout {
    let mut layout = PlanningLayout::default();
    let response = egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
        if page == PlanningPage::Solids {
            draw_solids_details(ui, &mut layout, editor, project, document, block_models, commands);
            return;
        }
        let configuration = ui
            .data(|data| data.get_temp::<bool>(egui::Id::new(("planning_configuration_selected", page))))
            .unwrap_or(false);
        if configuration {
            central_island(ui, &mut layout, |ui, rect| draw_configuration(ui, rect, page));
            return;
        }
        let category = island(ui, &mut layout, "planning_categories_island", 260.0, |ui, rect| draw_content_categories(ui, rect, page));
        island(ui, &mut layout, "planning_items_island", 320.0, |ui, rect| draw_items(ui, rect, category));
        central_island(ui, &mut layout, draw_item_properties);
    });
    layout.rect = response.response.rect;
    layout
}
