//! The Solids Setup subpage's New Field dialog (Field List step).

use crate::{
    i18n::tr,
    model::{Document, ReserveAggregation},
    ui::{
        EditorState,
        state::{ReserveFieldKind, UiCommand},
        widgets::menu::{self, DragableMenu, MenuButton, MenuFieldCombo, MenuFieldText},
    },
};

fn kind_label(kind: ReserveFieldKind) -> String {
    match kind {
        ReserveFieldKind::Sum => tr!(literal = "Sum"),
        ReserveFieldKind::WeightedAverage => tr!(literal = "Weighted average"),
        ReserveFieldKind::Category => tr!(literal = "Category"),
    }
}

/// Draw the dialog that adds one field to the project's Reserves Field List.
///
/// A field is a name plus how it aggregates: summed outright, averaged
/// weighted by another (necessarily summed) field - e.g. a grade weighted by
/// tonnes - or, for `Category`, not aggregated at all: a grouping label (e.g.
/// "Rock Type") each block model maps onto one of its own categorical
/// columns, for a future breakdown of the numeric fields by category.
pub(crate) fn draw_new_reserve_field_dialog(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document, commands: &mut Vec<UiCommand>) {
    if !editor.new_reserve_field_open {
        return;
    }
    let mut open = true;
    let mut close = false;
    DragableMenu::new("new_reserve_field_dialog", tr!(literal = "New Field"))
        .open(&mut open)
        .min_width(300.0)
        .show(ui.ctx(), |ui| {
            MenuFieldText::new(tr!(literal = "Name"), &mut editor.new_reserve_field_name)
                .hint_text(tr!(literal = "Required"))
                .show(ui);
            const KINDS: [ReserveFieldKind; 3] = [ReserveFieldKind::Sum, ReserveFieldKind::WeightedAverage, ReserveFieldKind::Category];
            let selected_kind_text = kind_label(editor.new_reserve_field_kind);
            MenuFieldCombo::new(
                "new_reserve_field_kind",
                tr!(literal = "Type"),
                &mut editor.new_reserve_field_kind,
                selected_kind_text,
                KINDS.into_iter().map(|kind| (kind, kind_label(kind).into())),
            )
            .show(ui);
            let sum_fields: Vec<_> = document
                .reserve_fields()
                .iter()
                .filter(|field| matches!(field.aggregation, ReserveAggregation::Sum))
                .collect();
            if editor.new_reserve_field_kind == ReserveFieldKind::WeightedAverage {
                if editor.new_reserve_field_weight_field.is_none_or(|id| !sum_fields.iter().any(|field| field.id == id)) {
                    editor.new_reserve_field_weight_field = sum_fields.first().map(|field| field.id);
                }
                let selected_text = editor
                    .new_reserve_field_weight_field
                    .and_then(|id| sum_fields.iter().find(|field| field.id == id))
                    .map(|field| field.name.clone())
                    .unwrap_or_else(|| tr!(literal = "None"));
                MenuFieldCombo::new(
                    "new_reserve_field_weight",
                    tr!(literal = "Weighted by"),
                    &mut editor.new_reserve_field_weight_field,
                    selected_text,
                    sum_fields.iter().map(|field| (Some(field.id), field.name.clone().into())),
                )
                .show(ui);
            }
            menu::menu_actions(ui, |ui| {
                let can_add = !editor.new_reserve_field_name.trim().is_empty()
                    && (editor.new_reserve_field_kind != ReserveFieldKind::WeightedAverage || editor.new_reserve_field_weight_field.is_some());
                let submitted = menu::dialog_confirm_pressed(ui.ctx());
                if (submitted || ui.add(MenuButton::new(tr!(literal = "Add Field")).primary().enabled(can_add)).clicked()) && can_add {
                    let aggregation = match editor.new_reserve_field_kind {
                        ReserveFieldKind::Sum => ReserveAggregation::Sum,
                        ReserveFieldKind::WeightedAverage => ReserveAggregation::WeightedAverage {
                            weight_field: editor.new_reserve_field_weight_field.expect("checked by can_add"),
                        },
                        ReserveFieldKind::Category => ReserveAggregation::Category,
                    };
                    commands.push(UiCommand::AddReserveField {
                        name: editor.new_reserve_field_name.trim().to_owned(),
                        aggregation,
                    });
                    close = true;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    close = true;
                }
            });
        });
    if close || !open {
        editor.new_reserve_field_open = false;
        editor.new_reserve_field_name.clear();
        editor.new_reserve_field_kind = ReserveFieldKind::Sum;
        editor.new_reserve_field_weight_field = None;
    }
}
