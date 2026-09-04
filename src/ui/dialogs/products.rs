//! The Drill & Blast palette's New Product dialog.

use crate::{
    i18n::tr,
    ui::{
        EditorState,
        state::UiCommand,
        widgets::menu::{self, DragableMenu, MenuButton, MenuFieldColor32, MenuFieldText, MenuFieldU32},
    },
};

/// Longest delay the entry accepts. Nothing in a bench pattern fires minutes
/// after the hole beside it, and the bound keeps a mistyped figure from
/// becoming a product.
const MAX_DELAY_MS: u32 = 10_000;

/// Draw the dialog that adds one product to the palette.
///
/// The three fields are all a delay is: when it fires, what it is called, and
/// the colour a tie-in drawn with it takes.
pub(crate) fn draw_new_product_dialog(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    if !editor.new_delay_product_open {
        return;
    }
    let mut open = true;
    let mut close = false;
    DragableMenu::new("new_product_dialog", tr!(literal = "New Product"))
        .open(&mut open)
        .min_width(300.0)
        .show(ui.ctx(), |ui| {
            MenuFieldU32::new(tr!(literal = "Delay"), &mut editor.new_delay_product_delay_ms, 0..=MAX_DELAY_MS)
                .suffix(tr!(literal = " ms"))
                .help_text(tr!(literal = "Milliseconds between one hole firing and the next."))
                .show(ui);
            MenuFieldText::new(tr!(literal = "Name"), &mut editor.new_delay_product_name)
                .hint_text(tr!(literal = "Required"))
                .show(ui);
            MenuFieldColor32::new(tr!(literal = "Colour"), &mut editor.new_delay_product_color).show(ui);
            menu::menu_actions(ui, |ui| {
                // A product with no delay is not one, and the palette shows the
                // name under every card, so neither field is optional.
                let can_add = editor.new_delay_product_delay_ms > 0 && !editor.new_delay_product_name.trim().is_empty();
                let submitted = menu::dialog_confirm_pressed(ui.ctx());
                if (submitted || ui.add(MenuButton::new(tr!(literal = "Add Product")).primary().enabled(can_add)).clicked()) && can_add {
                    commands.push(UiCommand::AddDelayProduct {
                        delay_ms: editor.new_delay_product_delay_ms,
                        name: editor.new_delay_product_name.trim().to_owned(),
                        color: editor.new_delay_product_color,
                    });
                    close = true;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    close = true;
                }
            });
        });
    if close || !open {
        editor.new_delay_product_open = false;
    }
}

/// Edit the initiation delay belonging to the collar clicked by the
/// Initiation Point tool. Existing points can be removed here as well; the
/// Delete key remains reserved for selected tie-in connectors.
pub(crate) fn draw_initiation_dialog(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let Some(dialog) = editor.initiation_dialog.as_mut() else {
        return;
    };
    let mut open = true;
    let mut close = false;
    let target = dialog.target;
    let existing = dialog.existing;
    let title = crate::i18n::tr_format!(literal = "Initiation · %name%", name = &dialog.hole_name);
    DragableMenu::new("initiation_dialog", title).open(&mut open).min_width(300.0).show(ui.ctx(), |ui| {
        MenuFieldU32::new(tr!(literal = "Delay"), &mut dialog.delay_ms, 0..=MAX_DELAY_MS)
            .suffix(" ms")
            .help_text(tr!(literal = "How long after the shot is fired this collar initiates the round."))
            .show(ui);
        menu::menu_actions(ui, |ui| {
            let confirm_label = if existing { tr!(literal = "Update") } else { tr!(literal = "Add Initiation") };
            if menu::dialog_confirm_pressed(ui.ctx()) || ui.add(MenuButton::new(confirm_label).primary()).clicked() {
                commands.push(UiCommand::SetInitiation {
                    target,
                    delay_ms: Some(dialog.delay_ms),
                });
                close = true;
            }
            if existing && ui.add(MenuButton::new(tr!(literal = "Remove"))).clicked() {
                commands.push(UiCommand::SetInitiation { target, delay_ms: None });
                close = true;
            }
            if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                close = true;
            }
        });
    });
    if close || !open {
        editor.initiation_dialog = None;
    }
}
