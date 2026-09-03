//! The About dialog: version, copyright, and licence.
//!
//! The GPL asks a program with a graphical interface to make its copyright and
//! warranty notice reachable from the running application; this dialog is that
//! notice. The bundled MIT and Apache-2.0 components are attributed in the
//! third-party licence file shipped alongside the binary, not here.

use crate::{
    i18n::tr,
    ui::{
        state::EditorState,
        widgets::menu::{self, DragableMenu, MenuButton},
    },
};

/// Draw the About dialog when `editor.show_about` is set.
pub(crate) fn draw_about_dialog(ui: &mut egui::Ui, editor: &mut EditorState) {
    if !editor.show_about {
        return;
    }
    let mut open = true;
    DragableMenu::new(tr!("about-title", app = crate::APP_NAME))
        .open(&mut open)
        .min_width(420.0)
        .max_width(460.0)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui.ctx(), |ui| {
            ui.set_max_width(440.0);

            ui.horizontal(|ui| {
                ui.add(egui::Image::new(egui::include_image!("../../../res/logo.svg")).fit_to_exact_size(egui::vec2(52.0, 52.0)));
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(format!("{} {}", crate::APP_NAME, crate::APP_RELEASE)).heading());
                    ui.label(tr!(literal = "Free Open Source Mine Design"));
                    ui.label(egui::RichText::new(tr!(literal = "Licensed under the GNU General Public License v3.0")).weak());
                });
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(
                egui::RichText::new(tr!(
                    literal = "Incline Design is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License version 3 as published by the Free Software Foundation.\n\nIncline Design is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details."
                ))
                .small(),
            );
            ui.add_space(4.0);
            ui.hyperlink_to(tr!("about-read-full-licence"), "https://www.gnu.org/licenses/gpl-3.0.html");

            ui.add_space(10.0);
            ui.separator();

            menu::menu_actions(ui, |ui| {
                // Nothing here is confirmed or cancelled, so both keys dismiss.
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                let cancel = menu::dialog_cancel_pressed(ui.ctx());
                if ui.add(MenuButton::new(tr!("common-close")).primary()).clicked() || confirm || cancel {
                    editor.show_about = false;
                }
                if ui.add(MenuButton::new(tr!("about-source-code"))).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab("https://github.com/Incline-Developers/Incline"));
                }
                if ui.add(MenuButton::new(tr!("about-website"))).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab("https://inclinedesign.net"));
                }
            });
        });
    if !open {
        editor.show_about = false;
    }
}
