use std::hash::Hash;

use egui::{Color32, Response, Sense, Stroke, Ui, Vec2};
use strum::IntoEnumIterator;

use crate::ui::state::ToolHatch;

/// A consistently styled icon button for application toolbars.
///
/// Draws a rounded rectangle with an optional selected highlight, an icon
/// centered inside, and a tooltip on hover.
pub(crate) struct ToolbarButton {
    icon: egui::Image<'static>,
    tooltip: egui::WidgetText,
    id_salt: Option<egui::Id>,
    selected: bool,
    button_size: Vec2,
    icon_size: Vec2,
    corner_radius: f32,
}

impl ToolbarButton {
    pub(crate) fn new(icon: egui::Image<'static>, tooltip: impl Into<egui::WidgetText>) -> Self {
        Self {
            icon,
            tooltip: tooltip.into(),
            id_salt: None,
            selected: false,
            button_size: egui::vec2(26.0, 26.0),
            icon_size: egui::vec2(22.0, 22.0),
            corner_radius: 3.0,
        }
    }

    /// Square the button off at `side`, with its icon inset by [`ICON_INSET`].
    ///
    /// The viewport bar sizes its buttons from the height it was given rather
    /// than the other way round, so that it comes out exactly as tall as the
    /// menu bar above it.
    pub(crate) fn side(mut self, side: f32) -> Self {
        self.button_size = Vec2::splat(side);
        self.icon_size = Vec2::splat((side - ICON_INSET).max(1.0));
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
        ui.painter().rect_filled(rect, self.corner_radius, fill);

        let icon_rect = egui::Align2::CENTER_CENTER.align_size_within_rect(self.icon_size, rect);
        self.icon.fit_to_exact_size(self.icon_size).paint_at(ui, icon_rect);
        response.on_hover_text(self.tooltip)
    }
}

/// How much smaller than its button a [`ToolbarButton`]'s icon is drawn: the
/// ring of fill that shows around it when the button is selected or hovered.
const ICON_INSET: f32 = 4.0;

/// Corner rounding on a run of tool cells. Shared with the window chrome
/// (`ui::chrome`), so every rounded surface in the window reads as one family.
pub(crate) const GROUP_CORNER_RADIUS: u8 = 3;
/// Side of a tool cell, which is also its run's width: the buttons run edge to
/// edge, so a selected tool's fill has no strip of surface left beside it.
pub(crate) const TOOL_CELL_SIZE: f32 = 30.0;
/// Side of the icon drawn inside a tool cell.
const TOOL_ICON_SIZE: f32 = 19.0;
/// Corner rounding on a tool cell's fill where it has no neighbours to sit
/// flush against.
const TOOL_CELL_CORNER_RADIUS: f32 = GROUP_CORNER_RADIUS as f32;

/// Where a run's cells park their fills until it knows which of them are its
/// end caps.
///
/// A cell's fill is painted before its icon, but only the run knows whether a
/// cell is its first, last, or neither - which is what decides the corners it
/// rounds. So each cell reserves its slot in the paint list and registers
/// here, and the run fills the slots in once it is complete. Runs never nest,
/// so one key serves them all.
#[derive(Clone, Default)]
struct GroupCellFills(Vec<(egui::layers::ShapeIdx, egui::Rect, Color32)>);

fn group_cell_fills_id() -> egui::Id {
    egui::Id::new("toolbar_group_cell_fills")
}

/// A tool button that fills its cell, sized to [`TOOL_CELL_SIZE`].
///
/// Unlike [`ToolbarButton`], whose fill is a small square inside whatever space
/// it is given, this fills its whole cell - so a selected tool reads as a solid
/// block the width of the run rather than a chip floating inside it. That only
/// works inside [`tool_cell_run`]; elsewhere, use [`ToolbarButton`].
pub(crate) struct ToolCellButton {
    icon: egui::Image<'static>,
    tooltip: egui::WidgetText,
    id_salt: Option<egui::Id>,
    selected: bool,
}

impl ToolCellButton {
    pub(crate) fn new(icon: egui::Image<'static>, tooltip: impl Into<egui::WidgetText>) -> Self {
        Self {
            icon,
            tooltip: tooltip.into(),
            id_salt: None,
            selected: false,
        }
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

impl egui::Widget for ToolCellButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let (auto_id, rect) = ui.allocate_space(Vec2::splat(TOOL_CELL_SIZE));
        let id = self.id_salt.map_or(auto_id, |salt| ui.make_persistent_id(salt));
        let response = ui.interact(rect, id, Sense::click());
        let fill = if self.selected {
            ui.visuals().selection.bg_fill
        } else if response.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            Color32::TRANSPARENT
        };
        // Inside a tile the group rounds the run's end caps and squares off
        // everything between them; on its own the cell rounds all four.
        let slot = ui.painter().add(egui::Shape::Noop);
        let in_group = ui.data_mut(|data| {
            data.get_temp_mut_or_default::<Option<GroupCellFills>>(group_cell_fills_id())
                .as_mut()
                .map(|cells| cells.0.push((slot, rect, fill)))
                .is_some()
        });
        if !in_group {
            ui.painter().set(slot, egui::Shape::rect_filled(rect, TOOL_CELL_CORNER_RADIUS, fill));
        }

        let icon_size = Vec2::splat(TOOL_ICON_SIZE);
        let icon_rect = egui::Align2::CENTER_CENTER.align_size_within_rect(icon_size, rect);
        self.icon.fit_to_exact_size(icon_size).paint_at(ui, icon_rect);
        response.on_hover_text(self.tooltip)
    }
}

/// Draw a run of [`ToolCellButton`]s as one block: the cells butt up against
/// each other and only the run's outer corners are rounded.
pub(crate) fn tool_cell_run<R>(ui: &mut Ui, add_buttons: impl FnOnce(&mut Ui) -> R) -> R {
    ui.data_mut(|data| data.insert_temp(group_cell_fills_id(), Some(GroupCellFills::default())));
    let inner = add_buttons(ui);
    let cells = ui.data_mut(|data| data.remove_temp::<Option<GroupCellFills>>(group_cell_fills_id()).flatten().unwrap_or_default());
    // `ui` is the run's own, not one of the cells': a disabled cell fades its
    // icon, but the block the fills draw stays as solid as the tile did.
    paint_cell_fills(ui, &cells.0);
    inner
}

/// Paint a run's cell fills, rounding only its outer corners so the
/// cells sit flush against each other.
fn paint_cell_fills(ui: &Ui, cells: &[(egui::layers::ShapeIdx, egui::Rect, Color32)]) {
    let last = cells.len().saturating_sub(1);
    for (index, &(slot, rect, fill)) in cells.iter().enumerate() {
        let top = if index == 0 { GROUP_CORNER_RADIUS } else { 0 };
        let bottom = if index == last { GROUP_CORNER_RADIUS } else { 0 };
        let corner_radius = egui::CornerRadius {
            nw: top,
            ne: top,
            sw: bottom,
            se: bottom,
        };
        ui.painter().set(slot, egui::Shape::rect_filled(rect, corner_radius, fill));
    }
}

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

    painter.rect_filled(rect, 0.0, bg);
    painter.rect_stroke(rect, 0.0, border, egui::StrokeKind::Inside);

    match hatch {
        ToolHatch::Clear => {}

        ToolHatch::Solid => {
            painter.rect_filled(rect, 0.0, preview_color);
        }

        ToolHatch::Slashes => {
            draw_hatch_lines(ui, rect, preview_color, false);
        }

        ToolHatch::Crosses => {
            draw_hatch_lines(ui, rect, preview_color, false);
            draw_hatch_lines(ui, rect, preview_color, true);
        }
    }
}

fn draw_hatch_lines(ui: &Ui, rect: egui::Rect, color: Color32, backslash: bool) {
    let painter = ui.painter().with_clip_rect(rect);

    let stroke = Stroke::new(1.2, color);
    let spacing = 6.0;

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

            ui.style_mut().visuals.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
            ui.style_mut().visuals.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
            ui.style_mut().visuals.widgets.active.corner_radius = egui::CornerRadius::ZERO;

            ui.style_mut().visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            ui.style_mut().visuals.widgets.hovered.bg_stroke = Stroke::NONE;
            ui.style_mut().visuals.widgets.active.bg_stroke = Stroke::NONE;

            ui.allocate_ui_with_layout(egui::vec2(self.size.x, self.size.y + 1.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.add_space(2.0);

                let response = egui::color_picker::color_edit_button_srgba(ui, self.color, egui::color_picker::Alpha::Opaque);

                ui.painter().rect_stroke(response.rect, 0.0, Stroke::new(1., Color32::BLACK), egui::StrokeKind::Inside);

                response
            })
            .inner
        })
        .inner
    }
}
