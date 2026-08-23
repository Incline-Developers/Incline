//! File, project, layer, viewport, and properties dialogs.

use crate::ui::{
    state::EditorState,
    widgets::{
        menu::{MenuButton, MenuFieldF64},
        viewport::ViewportDockPanel,
    },
};

/// Draw the Vertical Exaggeration adjustment dialog.
pub(crate) fn draw_vertical_exaggeration_dialog(ui: &mut egui::Ui, editor: &mut EditorState, viewport_rect: egui::Rect) {
    if !editor.vertical_exaggeration_dialog_open {
        return;
    }
    ViewportDockPanel::new("vertical_exaggeration_panel", "Vertical Exaggeration", viewport_rect)
        .min_width(330.0)
        .show(ui.ctx(), |ui| {
            ui.label("Scales Z distances visually without changing stored coordinates.");
            ui.add_space(8.0);
            let response = MenuFieldF64::new("Z scale ratio", &mut editor.vertical_exaggeration_input, 0.1..=20.)
                .max_decimals(1)
                .width(100.)
                .speed(0.1)
                .suffix("x")
                .show(ui);
            ui.add_space(10.0);
            let apply_from_enter = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let cancel_from_escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let apply_clicked = ui.add(MenuButton::new("Apply").primary()).clicked();
                if apply_from_enter || apply_clicked {
                    editor.vertical_exaggeration = editor.vertical_exaggeration_input;
                    editor.vertical_exaggeration_dialog_open = false;
                }
                if ui.button("Reset to 1×").clicked() {
                    editor.vertical_exaggeration = 1.0;
                    editor.vertical_exaggeration_input = 1.0;
                    editor.vertical_exaggeration_dialog_open = false;
                }
                if ui.button("Cancel").clicked() || cancel_from_escape {
                    editor.vertical_exaggeration_dialog_open = false;
                }
            });
        });
}
