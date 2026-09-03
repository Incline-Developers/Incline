//! The About dialog: version, copyright, and licence.
//!
//! The MIT licence asks that the copyright and warranty notice travel with the
//! software; this dialog is where the running application carries it. The
//! bundled third-party components keep their own notices in the third-party
//! licence file shipped alongside the binary, not here.

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
    DragableMenu::new("about_dialog", tr!("about-title", app = crate::APP_NAME))
        .open(&mut open)
        .min_width(420.0)
        .max_width(460.0)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Image::new(egui::include_image!("../../../res/logo.svg")).fit_to_exact_size(egui::vec2(52.0, 52.0)));
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(format!("{} {}", crate::APP_NAME, crate::APP_RELEASE)).heading());
                    ui.label(tr!(literal = "Free Open Source Mine Design"));
                    ui.label(egui::RichText::new(tr!(literal = "Licensed under the MIT License")).weak());
                });
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(
                egui::RichText::new(tr!(
                    literal = "Copyright (c) 2026 Leo Timmins, Lucas Timmins and the Incline Design contributors. Permission is hereby granted, free of charge, to any person obtaining a copy of this software to deal in it without restriction, subject to the conditions of the MIT License.\n\nIncline Design is provided \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, including but not limited to the warranties of MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE and NONINFRINGEMENT."
                ))
                .small(),
            );
            ui.add_space(4.0);
            ui.hyperlink_to(tr!("about-read-full-licence"), "https://opensource.org/license/mit");

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
