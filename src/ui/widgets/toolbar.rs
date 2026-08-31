use std::hash::Hash;

use egui::{Color32, Response, Sense, Stroke, Ui, Vec2};
use strum::IntoEnumIterator;

use crate::ui::state::ToolHatch;

/// A consistently styled icon button for application toolbars.
///
/// Draws a square optional selected highlight, an icon centered inside, and a
/// tooltip on hover. A button that reaches a region edge is rounded by the
/// region chrome along with everything else painted into that corner.
pub(crate) struct ToolbarButton {
    icon: egui::Image<'static>,
    tooltip: egui::WidgetText,
    id_salt: Option<egui::Id>,
    selected: bool,
    button_size: Vec2,
    icon_size: Vec2,
}

impl ToolbarButton {
    pub(crate) fn new(icon: egui::Image<'static>, tooltip: impl Into<egui::WidgetText>) -> Self {
        Self {
            icon,
            tooltip: tooltip.into(),
            id_salt: None,
            selected: false,
            button_size: Vec2::splat(TOOL_CELL_SIZE),
            icon_size: Vec2::splat(TOOL_ICON_SIZE),
        }
    }

    /// Change the square highlight/click target without scaling the icon.
    pub(crate) fn button_side(mut self, side: f32) -> Self {
        self.button_size = Vec2::splat(side);
        self
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Give the clickable response a stable identity when its parent may be
    /// rebuilt in a different order during an egui sizing pass.
    pub(crate) fn id_salt(mut self, id_salt: impl Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(egui::Id::new(id_salt));
        self
    }
}

impl egui::Widget for ToolbarButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let (auto_id, rect) = ui.allocate_space(self.button_size);
        let id = self.id_salt.map_or(auto_id, |salt| ui.make_persistent_id(salt));
        let response = ui.interact(rect, id, Sense::click());
        let fill = if self.selected {
            ui.visuals().selection.bg_fill
        } else if response.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 0.0, fill);

        let icon_rect = egui::Align2::CENTER_CENTER.align_size_within_rect(self.icon_size, rect);
        self.icon.fit_to_exact_size(self.icon_size).paint_at(ui, icon_rect);
        response.on_hover_text(self.tooltip)
    }
}

/// Corner rounding shared by the window chrome and rounded controls.
pub(crate) const GROUP_CORNER_RADIUS: u8 = 3;
/// Visible thickness of every toolbar and side of each button highlight.
pub(crate) const TOOL_CELL_SIZE: f32 = 30.0;
/// Side of every toolbar icon.
pub(crate) const TOOL_ICON_SIZE: f32 = 19.0;

/// Corner rounding on the colour and fill swatches in the viewport bar.
const SWATCH_CORNER_RADIUS: f32 = GROUP_CORNER_RADIUS as f32;
/// How far egui's own colour button is pushed inside the swatch painted over
/// it. It insets its square of colour by a pixel of its own, so this is one
/// short of the radius that has to cover it.
const SWATCH_INSET: f32 = SWATCH_CORNER_RADIUS - 1.0;

pub struct HatchPicker<'a> {
    selected_hatch: &'a mut ToolHatch,
    color: Color32,
    button_size: Vec2,
}

impl<'a> HatchPicker<'a> {
    pub fn new(selected_hatch: &'a mut ToolHatch, color: Color32) -> Self {
        Self {
            selected_hatch,
            color,
            button_size: egui::vec2(26.0, 22.0),
        }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let id = ui.make_persistent_id("hatch_picker");

        let response = egui::ComboBox::from_id_salt(id)
            .selected_text("")
            .width(0.0)
            .show_ui(ui, |ui| {
                ui.set_min_width(120.0);

                for hatch in ToolHatch::iter() {
                    if hatch_picker_row(ui, hatch, *self.selected_hatch, self.color).clicked() {
                        *self.selected_hatch = hatch;
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text(format!("Fill type: {}", self.selected_hatch));

        let swatch_rect = egui::Rect::from_center_size(response.rect.center(), self.button_size);

        draw_hatch_swatch(ui, swatch_rect, *self.selected_hatch, self.color);

        response
    }
}

fn hatch_picker_row(ui: &mut Ui, hatch: ToolHatch, selected_hatch: ToolHatch, color: Color32) -> Response {
    let row_size = egui::vec2(ui.available_width().max(120.0), 38.0);
    let (rect, response) = ui.allocate_exact_size(row_size, Sense::click());

    let selected = hatch == selected_hatch;
    let visuals = if selected {
        &ui.visuals().widgets.active
    } else if response.hovered() {
        &ui.visuals().widgets.hovered
    } else {
        &ui.visuals().widgets.inactive
    };

    if selected || response.hovered() {
        ui.painter().rect_filled(rect, 2.0, visuals.bg_fill);
    }

    let swatch_rect = egui::Rect::from_min_size(rect.min + egui::vec2(5.0, 3.0), egui::vec2(32.0, 32.0));

    draw_hatch_swatch(ui, swatch_rect, hatch, color);

    ui.painter().text(
        egui::pos2(swatch_rect.right() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        hatch.to_string(),
        egui::TextStyle::Button.resolve(ui.style()),
        visuals.text_color(),
    );

    response
}

fn draw_hatch_swatch(ui: &Ui, rect: egui::Rect, hatch: ToolHatch, _color: Color32) {
    let painter = ui.painter();

    let bg = if ui.visuals().dark_mode { Color32::BLACK } else { Color32::WHITE };
    let preview_color = if ui.visuals().dark_mode { Color32::WHITE } else { Color32::BLACK };
    let border = Stroke::new(1.0, Color32::BLACK);

    painter.rect_filled(rect, SWATCH_CORNER_RADIUS, bg);

    match hatch {
        ToolHatch::Clear => {}

        ToolHatch::Solid => {
            painter.rect_filled(rect, SWATCH_CORNER_RADIUS, preview_color);
        }

        ToolHatch::Slashes => {
            draw_hatch_lines(ui, rect, preview_color, false);
        }

        ToolHatch::Crosses => {
            draw_hatch_lines(ui, rect, preview_color, false);
            draw_hatch_lines(ui, rect, preview_color, true);
        }
    }

    // Last, so it closes off whatever the pattern left in the corners.
    painter.rect_stroke(rect, SWATCH_CORNER_RADIUS, border, egui::StrokeKind::Inside);
}

fn draw_hatch_lines(ui: &Ui, rect: egui::Rect, color: Color32, backslash: bool) {
    let stroke = Stroke::new(1.2, color);
    let spacing = 6.0;

    // egui clips to rectangles only, so the pattern is drawn once per band of
    // the swatch, each band narrowed to what the rounded corners leave of it.
    for band in rounded_bands(rect, SWATCH_CORNER_RADIUS) {
        let painter = ui.painter().with_clip_rect(band);

        let height = rect.height();
        let mut x = rect.left() - height;

        while x < rect.right() + height {
            let (a, b) = if backslash {
                (egui::pos2(x, rect.top()), egui::pos2(x + height, rect.bottom()))
            } else {
                (egui::pos2(x, rect.bottom()), egui::pos2(x + height, rect.top()))
            };

            painter.line_segment([a, b], stroke);
            x += spacing;
        }
    }
}

/// Slice `rect` into horizontal bands that between them cover it rounded off at
/// `radius`: one band for its straight middle, then a pixel-tall band per row
/// of each corner arc, inset to where the arc has reached.
fn rounded_bands(rect: egui::Rect, radius: f32) -> impl Iterator<Item = egui::Rect> {
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0).max(0.0);
    let middle = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + radius), egui::pos2(rect.right(), rect.bottom() - radius));

    let rows = radius.ceil() as usize;
    let corners = (0..rows).flat_map(move |row| {
        let top = row as f32;
        let bottom = (top + 1.0).min(radius);
        // The band is only as wide as its outer edge, where the arc is furthest in.
        let arc = radius - (radius * radius - (radius - top) * (radius - top)).max(0.0).sqrt();
        let (left, right) = (rect.left() + arc, rect.right() - arc);
        [
            egui::Rect::from_min_max(egui::pos2(left, rect.top() + top), egui::pos2(right, rect.top() + bottom)),
            egui::Rect::from_min_max(egui::pos2(left, rect.bottom() - bottom), egui::pos2(right, rect.bottom() - top)),
        ]
    });

    std::iter::once(middle).chain(corners)
}

pub struct ColorSquarePicker<'a> {
    color: &'a mut Color32,
    size: Vec2,
}

impl<'a> ColorSquarePicker<'a> {
    pub fn new(color: &'a mut Color32) -> Self {
        Self {
            color,
            size: egui::vec2(28.0, 22.0),
        }
    }
    pub fn show(self, ui: &mut Ui) -> Response {
        ui.scope(|ui| {
            ui.spacing_mut().interact_size = self.size;
            ui.spacing_mut().button_padding = egui::vec2(0.0, 0.0);

            // egui's colour button paints a square swatch and caps its own
            // rounding at two pixels, so it is drawn inset far enough
            // ([`SWATCH_INSET`] covers the one pixel it insets by itself) to
            // disappear under the swatch painted here, which rounds with the
            // rest of the window.
            let widgets = &mut ui.style_mut().visuals.widgets;
            for widget in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.open] {
                widget.corner_radius = egui::CornerRadius::ZERO;
                widget.bg_stroke = Stroke::NONE;
                widget.bg_fill = Color32::TRANSPARENT;
                widget.expansion = -SWATCH_INSET;
            }

            ui.allocate_ui_with_layout(egui::vec2(self.size.x, self.size.y + 1.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.add_space(2.0);

                let response = egui::color_picker::color_edit_button_srgba(ui, self.color, egui::color_picker::Alpha::Opaque);

                let painter = ui.painter();
                painter.rect_filled(response.rect, SWATCH_CORNER_RADIUS, *self.color);
                painter.rect_stroke(response.rect, SWATCH_CORNER_RADIUS, Stroke::new(1., Color32::BLACK), egui::StrokeKind::Inside);

                response
            })
            .inner
        })
        .inner
    }
}
