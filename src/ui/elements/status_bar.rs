//! Bottom status bar: app identity, selected count, cursor coords, FPS counter
//! and, at the far right, the language picker. Task progress belongs to the
//! bottom toolbar - see `widgets::progress`.

use thousands::Separable;

use crate::{
    i18n::{LanguageChoice, tr},
    ui::{
        EditorState, UiCommand, themed_icon,
        widgets::{
            context_menu::{ContextMenuAction, dropdown_menu},
            toolbar::GROUP_CORNER_RADIUS,
        },
    },
};

/// Extra padding around a coordinate readout's worst-case value.
const COORD_FIELD_PADDING: f32 = 10.0;
/// Shrink factor on the coordinate fields: the worst-case template is wider
/// than the values normally seen, so the fields don't need its full width.
const COORD_FIELD_SCALE: f32 = 0.9;

/// Fixed width for each coordinate readout, measured from a worst-case value so
/// the fields (and everything after them) don't shift as the cursor moves.
fn coord_field_width(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap("RL: -8,888,888.88".to_owned(), font, egui::Color32::PLACEHOLDER);
    (galley.size().x + COORD_FIELD_PADDING) * COORD_FIELD_SCALE
}

/// One left-aligned readout occupying a fixed width regardless of its value.
fn coord_field(ui: &mut egui::Ui, width: f32, text: String) {
    let height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let color = ui.visuals().text_color();
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.painter().with_clip_rect(rect).text(rect.left_center(), egui::Align2::LEFT_CENTER, text, font, color);
}

/// Side of the globe on the language picker. Smaller than a toolbar icon: it
/// sits beside body text, not in a grid of tools.
const LANGUAGE_ICON_SIZE: f32 = 13.0;
/// Space between the language's name and that globe.
const LANGUAGE_ICON_GAP: f32 = 5.0;
/// Space either side of the picker's contents, so its hover fill reads as a
/// button rather than as a box drawn tight around the text.
const LANGUAGE_PADDING: f32 = 6.0;
/// Width of the menu the picker opens. Narrower than a menu-bar dropdown: it
/// holds one short endonym per row.
const LANGUAGE_MENU_WIDTH: f32 = 150.0;

/// The language picker at the right end of the bar: the running language named
/// in its own script, and a menu of the rest.
///
/// It is here rather than in the Preferences panel because someone who has
/// landed in a language they cannot read needs to be able to find it without
/// navigating one. Switching is live - see [`crate::i18n`].
fn draw_language_picker(ui: &mut egui::Ui, editor: &EditorState, commands: &mut Vec<UiCommand>) {
    let label = editor.language.endonym();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), egui::Color32::PLACEHOLDER);
    let size = egui::vec2(
        galley.size().x + LANGUAGE_ICON_GAP + LANGUAGE_ICON_SIZE + LANGUAGE_PADDING * 2.0,
        galley.size().y.max(LANGUAGE_ICON_SIZE),
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(rect, GROUP_CORNER_RADIUS, ui.visuals().widgets.hovered.bg_fill);
        }
        let icon_rect = egui::Align2::RIGHT_CENTER.align_size_within_rect(egui::Vec2::splat(LANGUAGE_ICON_SIZE), rect.shrink2(egui::vec2(LANGUAGE_PADDING, 0.0)));
        egui::Image::new(themed_icon!(ui, "language.svg"))
            .fit_to_exact_size(icon_rect.size())
            .paint_at(ui, icon_rect);
        let color = ui.visuals().text_color();
        ui.painter().text(
            egui::pos2(icon_rect.left() - LANGUAGE_ICON_GAP, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            label,
            font,
            color,
        );
    }
    let title = tr!("status-language");
    dropdown_menu(&response, title, LANGUAGE_MENU_WIDTH, |ui| {
        for choice in LanguageChoice::ALL {
            // Ticked rather than selected: the menu is a set of switches with
            // one on, the same shape the View menu's toggles have.
            if ContextMenuAction::new(choice.endonym()).checked(choice == editor.language).show(ui).clicked() {
                commands.push(UiCommand::SetLanguage(choice));
                ui.close();
            }
        }
    });
}

/// Draw the bottom status bar panel.
pub(crate) fn draw_status_bar(ui: &mut egui::Ui, editor: &EditorState, commands: &mut Vec<UiCommand>) -> egui::Rect {
    // The other bar that spans the window rather than sitting in a region, and
    // dressed the same way: the gap's colour, and no separator line.
    egui::Panel::bottom("status_bar")
        .show_separator_line(crate::ui::chrome::show_separator_line(ui))
        .frame(crate::ui::chrome::window_bar_frame(ui))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{} {}", crate::APP_NAME, crate::APP_RELEASE));
                ui.separator();
                ui.label(format!("Selected: {}", editor.selected_handles.len()));
                ui.separator();
                if editor.frame_counter_enabled {
                    match editor.measured_fps {
                        Some(fps) => ui.label(format!("FPS: {fps:.1}")),
                        None => ui.label("FPS: --"),
                    };
                    ui.separator();
                }
                if editor.debug_chunk_coloring {
                    match editor.debug_chunk_stats {
                        Some((rendered, total)) => ui.label(format!("Chunks: {rendered}/{total} ({} culled)", total.saturating_sub(rendered))),
                        None => ui.label("Chunks: --"),
                    };
                    ui.separator();
                }
                if editor.debug_clip_planes {
                    match editor.debug_clip_plane_distances {
                        Some((near, far)) => ui.label(format!("Clip near/far/Δ: {near:.3} / {far:.3} / {:.3} m", far - near)),
                        None => ui.label("Clip near/far/Δ: -- / -- / --"),
                    };
                    ui.separator();
                }
                let coord_width = coord_field_width(ui);
                match editor.cursor_world {
                    Some(p) => {
                        for (axis, value) in [("X", p.x), ("Y", p.y), ("Z", p.z)] {
                            coord_field(ui, coord_width, format!("{axis}: {}", format!("{value:.2}").separate_with_commas()));
                        }
                    }
                    None => {
                        for axis in ["X", "Y", "Z"] {
                            coord_field(ui, coord_width, format!("{axis}: --"));
                        }
                    }
                }
                // The picker is pinned to the far end of the bar, so it does not
                // move as the readouts before it come and go.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| draw_language_picker(ui, editor, commands));
            });
        })
        .response
        .rect
}
