use crate::{
    i18n::tr,
    model::drill_hole::{DrillColorPreset, DrillFieldKind, OpenDrillHoleDataset},
    ui::{
        state::{EditorState, UiCommand},
        widgets::menu::{self, DragableMenu, MenuButton, MenuField, MenuFieldCombo},
    },
};

pub(crate) fn draw_drill_hole_color_dialog(ui: &mut egui::Ui, editor: &mut EditorState, datasets: &[OpenDrillHoleDataset], commands: &mut Vec<UiCommand>) {
    let Some(id) = editor.drill_hole_color_dialog else {
        return;
    };
    let Some(dataset) = datasets.iter().find(|dataset| dataset.id == id) else {
        editor.drill_hole_color_dialog = None;
        return;
    };
    let mut open = true;
    DragableMenu::new("drill_hole_colour_dialog", tr!("drill-hole-colour-title", name = dataset.name.clone()))
        .open(&mut open)
        .min_width(400.0)
        .max_width(480.0)
        .show(ui.ctx(), |ui| {
            let active_label = dataset
                .color
                .active_field
                .as_deref()
                .and_then(|key| dataset.dataset.field(key))
                .map_or_else(|| tr!(literal = "Uniform white"), |field| field.label.clone());
            let mut active_field = dataset.color.active_field.clone();
            let field_options = std::iter::once((None, tr!(literal = "Uniform white").into()))
                .chain(dataset.dataset.fields.iter().map(|field| (Some(field.key.clone()), field.label.clone().into())));
            if MenuFieldCombo::new(("drill_hole_field", id), tr!(literal = "Field"), &mut active_field, active_label, field_options)
                .show(ui)
                .changed()
            {
                commands.push(UiCommand::SetDrillHoleColorField { id, field: active_field });
            }

            let Some(field) = dataset.color.active_field.as_deref().and_then(|key| dataset.dataset.field(key)) else {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(tr!(literal = "All rendered intervals are opaque white.")).weak());
                return;
            };
            menu::menu_section(ui, tr!(literal = "Colour scale"));
            match &field.kind {
                DrillFieldKind::Numeric { min, max } => {
                    let mut preset = dataset.color.preset;
                    if MenuFieldCombo::new(
                        ("drill_hole_preset", id),
                        tr!(literal = "Preset"),
                        &mut preset,
                        dataset.color.preset.label(),
                        DrillColorPreset::ALL.map(|preset| (preset, preset.label().into())),
                    )
                    .show(ui)
                    .changed()
                    {
                        commands.push(UiCommand::SetDrillHoleColorPreset { id, preset });
                    }
                    ui.label(
                        egui::RichText::new(if dataset.color.smooth {
                            tr!(literal = "Smooth interpolation")
                        } else {
                            tr!(literal = "Stepped bands")
                        })
                        .weak(),
                    );
                    ui.add_space(2.0);
                    let mut stops = dataset.color.stops.clone();
                    let mut changed = false;
                    let mut remove = None;
                    let can_remove = stops.len() > 2;
                    for (index, stop) in stops.iter_mut().enumerate() {
                        MenuField::new(tr!("drill-hole-colour-stop", index = ((index + 1) as u32))).show(ui, |ui, _, _| {
                            changed |= egui::color_picker::color_edit_button_rgb(ui, &mut stop.color).changed();
                            let actual = if (*max - *min).abs() <= f64::EPSILON {
                                *min
                            } else {
                                *min + f64::from(stop.t) * (*max - *min)
                            };
                            ui.monospace(format!("{actual:.6}"));
                            changed |= ui
                                .add_sized([145.0, ui.spacing().interact_size.y], egui::Slider::new(&mut stop.t, 0.0..=1.0).show_value(false))
                                .changed();
                            if can_remove && ui.small_button(tr!(literal = "−")).clicked() {
                                remove = Some(index);
                            }
                        });
                    }
                    if let Some(index) = remove {
                        stops.remove(index);
                        changed = true;
                    }
                    ui.horizontal(|ui| {
                        if stops.len() < 12 && ui.add(MenuButton::new(tr!(literal = "Add stop"))).clicked() {
                            let index = stops.len() / 2;
                            let left = stops[index.saturating_sub(1)];
                            let right = stops[index.min(stops.len() - 1)];
                            stops.push(crate::model::drill_hole::DrillColorStop {
                                t: (left.t + right.t) * 0.5,
                                color: [
                                    (left.color[0] + right.color[0]) * 0.5,
                                    (left.color[1] + right.color[1]) * 0.5,
                                    (left.color[2] + right.color[2]) * 0.5,
                                ],
                            });
                            changed = true;
                        }
                        if ui.add(MenuButton::new(tr!(literal = "Reset preset"))).clicked() {
                            commands.push(UiCommand::SetDrillHoleColorPreset { id, preset: dataset.color.preset });
                        }
                    });
                    if changed {
                        commands.push(UiCommand::SetDrillHoleColorStops { id, stops });
                    }
                }
                DrillFieldKind::Categorical { .. } => {
                    ui.label(egui::RichText::new(tr!(literal = "Categories beyond the first 12 and missing values remain white.")).weak());
                    ui.add_space(2.0);
                    let mut categories = dataset.color.categories.clone();
                    let mut changed = false;
                    for category in &mut categories {
                        MenuField::new(&category.value).show(ui, |ui, _, _| {
                            changed |= egui::color_picker::color_edit_button_rgb(ui, &mut category.color).changed();
                        });
                    }
                    if changed {
                        commands.push(UiCommand::SetDrillHoleCategoryColors { id, categories });
                    }
                }
            }
        });
    // Colour edits apply live, so there is no confirm step: both keys dismiss.
    let dismissed = {
        let confirm = menu::dialog_confirm_pressed(ui.ctx());
        let cancel = menu::dialog_cancel_pressed(ui.ctx());
        confirm || cancel
    };
    if !open || dismissed {
        editor.drill_hole_color_dialog = None;
    }
}
