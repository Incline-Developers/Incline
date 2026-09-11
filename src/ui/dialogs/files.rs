//! File, project, layer, viewport, and properties dialogs.

use crate::{
    i18n::tr,
    ui::{
        state::EditorState,
        widgets::{
            menu::{self, MenuButton, MenuFieldBool, MenuFieldColor32, MenuFieldF64},
            viewport::ViewportDockPanel,
        },
    },
};

/// Draw the Vertical Exaggeration adjustment dialog.
pub(crate) fn draw_vertical_exaggeration_dialog(ui: &mut egui::Ui, editor: &mut EditorState, viewport_rect: egui::Rect) {
    if !editor.vertical_exaggeration_dialog_open {
        return;
    }
    ViewportDockPanel::new("vertical_exaggeration_panel", tr!(literal = "Vertical Exaggeration"), viewport_rect)
        .min_width(330.0)
        .show(ui.ctx(), |ui| {
            ui.label(tr!(literal = "Scales Z distances visually without changing stored coordinates."));
            ui.add_space(8.0);
            let response = MenuFieldF64::new(tr!(literal = "Z scale ratio"), &mut editor.vertical_exaggeration_input, 0.1..=20.)
                .max_decimals(1)
                .speed(0.1)
                .suffix(tr!(literal = "x"))
                .show(ui);
            ui.add_space(10.0);
            let apply_from_enter = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let cancel_from_escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let apply_clicked = ui.add(MenuButton::new(tr!(literal = "Apply")).primary()).clicked();
                if apply_from_enter || apply_clicked {
                    editor.vertical_exaggeration = editor.vertical_exaggeration_input;
                    editor.vertical_exaggeration_dialog_open = false;
                }
                if ui.add(MenuButton::new(tr!(literal = "Reset to 1×"))).clicked() {
                    editor.vertical_exaggeration = 1.0;
                    editor.vertical_exaggeration_input = 1.0;
                    editor.vertical_exaggeration_dialog_open = false;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || cancel_from_escape {
                    editor.vertical_exaggeration_dialog_open = false;
                }
            });
        });
}

/// The section grid's options, opened by a right click on the grid button:
/// colour, line thickness and the RL spacing, applied on OK or Apply;
/// Cancel drops the fields, an earlier Apply stays.
pub(crate) fn draw_section_grid_dialog(ui: &mut egui::Ui, editor: &mut EditorState, viewport_rect: egui::Rect) {
    let Some(dialog) = editor.section_grid_dialog.as_mut() else {
        return;
    };
    let (mut confirm, mut cancel, mut apply) = (false, false, false);
    ViewportDockPanel::new("section_grid_options", tr!(literal = "RL Grid Options"), viewport_rect)
        .min_width(330.0)
        .show(ui.ctx(), |ui| {
            MenuFieldBool::new(tr!(literal = "Automatic colour"), &mut dialog.auto_color).show(ui);
            if !dialog.auto_color {
                MenuFieldColor32::new(tr!(literal = "Colour"), &mut dialog.color).show(ui);
            }
            MenuFieldF64::new(tr!(literal = "Thickness"), &mut dialog.thickness, 0.5..=6.0)
                .max_decimals(1)
                .speed(0.1)
                .suffix(tr!(literal = " px"))
                .show(ui);
            MenuFieldBool::new(tr!(literal = "Automatic RL spacing"), &mut dialog.auto_spacing).show(ui);
            if !dialog.auto_spacing {
                MenuFieldF64::new(tr!(literal = "RL spacing"), &mut dialog.spacing, crate::rendering::section_grid::MIN_SPACING_M..=10000.0)
                    .max_decimals(1)
                    .speed(1.0)
                    .suffix(tr!(literal = " m"))
                    .show(ui);
            }
            ui.add_space(10.0);
            let confirm_from_enter = menu::dialog_confirm_pressed(ui.ctx());
            let cancel_from_escape = menu::dialog_cancel_pressed(ui.ctx());
            menu::menu_actions(ui, |ui| {
                confirm = ui.add(MenuButton::new(tr!(literal = "OK")).primary()).clicked() || confirm_from_enter;
                cancel = ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || cancel_from_escape;
                apply = ui.add(MenuButton::new(tr!(literal = "Apply"))).clicked();
            });
        });
    if confirm || apply {
        editor.section_grid_style = dialog.style();
    }
    if confirm || cancel {
        editor.section_grid_dialog = None;
    }
}
