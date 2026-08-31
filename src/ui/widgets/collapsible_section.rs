//! Blender-style collapsible cards for groups of related panel controls.

use super::{shifted, toolbar::GROUP_CORNER_RADIUS};

/// A full-width, persistent collapsible section.
///
/// This is deliberately more substantial than an ordinary section heading:
/// use it where a group is a self-contained mode or concept that benefits
/// from being folded away. Lightweight runs of fields should keep using
/// [`crate::ui::widgets::menu::menu_section`].
pub(crate) struct CollapsibleSection {
    id: egui::Id,
    title: egui::WidgetText,
    default_open: bool,
}

impl CollapsibleSection {
    pub(crate) fn new(id: impl Into<egui::Id>, title: impl Into<egui::WidgetText>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            default_open: false,
        }
    }

    /// Whether a section with no stored state starts expanded.
    pub(crate) fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }

    pub(crate) fn show<R>(self, ui: &mut egui::Ui, add_body: impl FnOnce(&mut egui::Ui) -> R) -> egui::CollapsingResponse<R> {
        let dark = ui.visuals().dark_mode;
        let panel = ui.visuals().panel_fill;
        let card_fill = shifted(panel, if dark { 5 } else { -4 });
        let border = shifted(panel, if dark { 20 } else { -24 });
        // The menu-field style belongs to the controls in the body. Preserve
        // it while giving only the header the flatter, full-width card style.
        let body_style = ui.style().clone();

        egui::Frame::new()
            .fill(card_fill)
            .stroke(egui::Stroke::new(1.0, border))
            .corner_radius(egui::CornerRadius::same(GROUP_CORNER_RADIUS))
            .inner_margin(egui::Margin::ZERO)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.scope(|ui| {
                    ui.spacing_mut().button_padding = egui::vec2(7.0, 4.0);
                    ui.visuals_mut().collapsing_header_frame = true;
                    ui.visuals_mut().widgets.inactive.weak_bg_fill = card_fill;
                    ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;

                    egui::CollapsingHeader::new(self.title)
                        .id_salt(self.id)
                        .default_open(self.default_open)
                        .show_unindented(ui, |ui| {
                            ui.set_style(body_style);
                            egui::Frame::NONE
                                .inner_margin(egui::Margin {
                                    left: 8,
                                    right: 8,
                                    top: 4,
                                    bottom: 8,
                                })
                                .show(ui, add_body)
                                .inner
                        })
                })
                .inner
            })
            .inner
    }
}
