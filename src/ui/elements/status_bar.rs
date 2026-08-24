//! Bottom status bar: app identity, selected count, cursor coords and FPS
//! counter. Task progress belongs to the bottom toolbar - see
//! `widgets::progress`.

use thousands::Separable;

use crate::ui::EditorState;

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

/// Draw the bottom status bar panel.
pub(crate) fn draw_status_bar(ui: &mut egui::Ui, editor: &EditorState) -> egui::Rect {
    egui::Panel::bottom("status_bar")
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
                        Some((near, far)) => ui.label(format!("Clip near/far: {near:.3} / {far:.3} m")),
                        None => ui.label("Clip near/far: -- / --"),
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
            });
        })
        .response
        .rect
}
