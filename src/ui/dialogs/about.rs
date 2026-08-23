//! The About dialog: version, copyright, licence, and third-party notices.
//!
//! The GPL asks a program with a graphical interface to make its copyright and
//! warranty notice reachable from the running application; this dialog is that
//! notice, and carries the attribution the bundled MIT and Apache-2.0
//! components require alongside it.

use crate::ui::{
    state::EditorState,
    widgets::menu::{self, DragableMenu, MenuButton},
};

/// Copyright line, kept in step with the one on the startup splash.
const COPYRIGHT: &str = "© 2026 Leo Timmins and Lucas Timmins";

const LICENSE_NOTICE: &str = "Incline is free software: you can redistribute it and/or modify it under the terms of \
    the GNU General Public License version 3 as published by the Free Software Foundation.\n\n\
    Incline is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without \
    even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU \
    General Public License for more details.";

/// Bundled components whose licences require their notices to travel with the
/// binary. Add to this list whenever a dependency with an attribution clause
/// starts shipping inside Incline.
const THIRD_PARTY: &[(&str, &str)] = &[
    ("Open Sans", "Apache License 2.0"),
    ("omf-rust — © 2023 Global Mining Guidelines Group", "MIT License"),
    ("egui, wgpu, winit and other Rust crates", "MIT / Apache-2.0"),
];

const TRADEMARK_NOTICE: &str = "OMF (Open Mining Format) is a data exchange format developed by the Global Mining Guidelines Group. \
    Incline reads and writes OMF files but is not affiliated with, endorsed by, or certified by GMG.";

/// Draw the About dialog when `editor.show_about` is set.
pub(crate) fn draw_about_dialog(ui: &mut egui::Ui, editor: &mut EditorState) {
    if !editor.show_about {
        return;
    }
    let mut open = true;
    DragableMenu::new(format!("About {}", crate::APP_NAME))
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
                    ui.label("A free open-source mine design application.");
                    ui.label(egui::RichText::new(COPYRIGHT).weak());
                });
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(egui::RichText::new(LICENSE_NOTICE).small());
            ui.add_space(4.0);
            ui.hyperlink_to("Read the full licence ↗", "https://www.gnu.org/licenses/gpl-3.0.html");

            ui.add_space(6.0);
            menu::menu_section(ui, "Third-party components");
            for (component, license) in THIRD_PARTY {
                ui.label(egui::RichText::new(format!("• {component} — {license}")).small());
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(egui::RichText::new(TRADEMARK_NOTICE).small().weak());

            ui.add_space(6.0);
            menu::menu_actions(ui, |ui| {
                // Nothing here is confirmed or cancelled, so both keys dismiss.
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                let cancel = menu::dialog_cancel_pressed(ui.ctx());
                if ui.add(MenuButton::new("Close").primary()).clicked() || confirm || cancel {
                    editor.show_about = false;
                }
                if ui.add(MenuButton::new("Source Code")).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab("https://github.com/Incline-Developers/Incline"));
                }
                if ui.add(MenuButton::new("Website")).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab("https://inclinedesign.net"));
                }
            });
        });
    if !open {
        editor.show_about = false;
    }
}
