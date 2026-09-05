//! Shared CAD swatches and RGB picker, retaining each caller's colour representation.

use egui::{Color32, Popup, PopupCloseBehavior, Response, Sense, Stroke, StrokeKind, Ui, color_picker::Alpha, ecolor::Hsva};

use crate::{i18n::tr, rendering::color::COLOR_TABLE};

const PALETTE_WIDTH: f32 = 275.0;
const CELL_GAP: f32 = 1.0;
const CELL_SIZE: egui::Vec2 = egui::vec2((PALETTE_WIDTH - 23.0 * CELL_GAP) / 24.0, 14.0);
const PRIMARY_CELL_SIZE: egui::Vec2 = egui::vec2((PALETTE_WIDTH - 14.0 * CELL_GAP) / 15.0, 17.0);

#[derive(Clone)]
struct PickerState<T> {
    value: T,
    hsva: Hsva,
    rgb_tab: bool,
    index: Option<u8>,
    number: String,
}

impl<T> PickerState<T> {
    fn sync_index(&mut self) {
        let rgb = self.hsva.to_srgb();
        // Keep the chosen index when multiple ACI entries have the same RGB.
        if !self.index.is_some_and(|index| COLOR_TABLE[index as usize] == rgb) {
            self.index = (1..=255).find(|index| COLOR_TABLE[*index as usize] == rgb);
        }
        self.number = self.index.map_or_else(String::new, |index| index.to_string());
    }

    fn select(&mut self, index: u8) {
        let alpha = self.hsva.a;
        self.hsva = Hsva::from_srgb(COLOR_TABLE[index as usize]);
        self.hsva.a = alpha;
        self.index = Some(index);
    }
}

pub(crate) fn edit_srgba(ui: &mut Ui, color: &mut Color32, alpha: Alpha) -> Response {
    edit_button(ui, color, alpha, Hsva::from, Color32::from)
}

pub(crate) fn edit_rgb(ui: &mut Ui, color: &mut [f32; 3]) -> Response {
    edit_button(ui, color, Alpha::Opaque, Hsva::from_rgb, |hsva| hsva.to_rgb())
}

pub(crate) fn edit_rgba_premultiplied(ui: &mut Ui, color: &mut [f32; 4]) -> Response {
    edit_button(
        ui,
        color,
        Alpha::OnlyBlend,
        |[r, g, b, a]| Hsva::from_rgba_premultiplied(r, g, b, a),
        |hsva| hsva.to_rgba_premultiplied(),
    )
}

pub(crate) fn edit_srgba_unmultiplied(ui: &mut Ui, color: &mut [u8; 4]) -> Response {
    edit_button(
        ui,
        color,
        Alpha::OnlyBlend,
        |[r, g, b, a]| {
            // Avoid premultiplication so RGB survives even at zero opacity.
            let mut hsva = Hsva::from_srgb([r, g, b]);
            hsva.a = f32::from(a) / 255.0;
            hsva
        },
        |hsva| hsva.to_srgba_unmultiplied(),
    )
}

fn edit_button<T: Copy + PartialEq + Send + Sync + 'static>(ui: &mut Ui, value: &mut T, alpha: Alpha, decode: impl Fn(T) -> Hsva, encode: impl Fn(Hsva) -> T) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(ui.spacing().interact_size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::new(egui::WidgetType::ColorButton));
    let state_id = response.id.with("cad_color_state");
    let popup_id = response.id.with("cad_color_popup");
    let was_open = Popup::is_id_open(ui.ctx(), popup_id);
    let mut state = ui.ctx().data_mut(|data| data.get_temp::<PickerState<T>>(state_id)).unwrap_or_else(|| {
        let mut state = PickerState {
            value: *value,
            hsva: decode(*value),
            rgb_tab: false,
            index: None,
            number: String::new(),
        };
        state.sync_index();
        state
    });
    if state.value != *value {
        state.value = *value;
        state.hsva = decode(*value);
        state.sync_index();
    }
    // Older picker state may use egui's negative-alpha additive encoding.
    // Keep the hue and opacity magnitude, but make all new edits normal blending.
    state.hsva.a = state.hsva.a.abs();
    if response.clicked() && !was_open {
        state.rgb_tab = false;
        state.sync_index();
    }

    let mut changed = false;
    Popup::menu(&response).id(popup_id).close_behavior(PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        // Toolbar buttons and compact legends override these locally.
        // Use normal controls inside their popup.
        ui.set_style(ui.ctx().global_style());
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
        ui.spacing_mut().slider_width = PALETTE_WIDTH;
        ui.set_width(PALETTE_WIDTH);
        ui.horizontal(|ui| {
            if state.rgb_tab {
                let mut rgb = state.hsva.to_srgb();
                let mut edited = false;
                for (channel, label) in rgb.iter_mut().zip(["R ", "G ", "B "]) {
                    edited |= ui.add(egui::DragValue::new(channel).range(0..=255).speed(0.5).prefix(label)).changed();
                }
                if edited {
                    let opacity = state.hsva.a;
                    state.hsva = Hsva::from_srgb(rgb);
                    state.hsva.a = opacity;
                    state.sync_index();
                    changed = true;
                }
            } else {
                ui.label(tr!("color-aci"));
                let entry = ui.add(egui::TextEdit::singleline(&mut state.number).desired_width(48.0).char_limit(3));
                if entry.changed()
                    && let Ok(index @ 1..=255) = state.number.parse::<u8>()
                {
                    state.select(index);
                    changed = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.selectable_value(&mut state.rgb_tab, true, tr!("color-rgb"));
                ui.selectable_value(&mut state.rgb_tab, false, tr!("color-index"));
            });
        });
        ui.separator();
        if state.rgb_tab {
            if rgb_picker(ui, &mut state.hsva) {
                state.sync_index();
                changed = true;
            }
        } else if let Some(index) = swatches(ui, state.index) {
            state.select(index);
            state.number = index.to_string();
            changed = true;
        }
        if alpha != Alpha::Opaque {
            let mut opacity = egui::vec2(state.hsva.a, 0.5);
            let response = color_gradient(ui, egui::vec2(PALETTE_WIDTH, 14.0), &mut opacity, |a, _| Hsva { a, ..state.hsva }.into()).on_hover_text(tr!("color-opacity"));
            if response.changed() {
                state.hsva.a = opacity.x;
                changed = true;
            }
        }
    });

    if changed {
        if alpha == Alpha::Opaque {
            state.hsva.a = 1.0;
        }
        *value = encode(state.hsva);
        state.value = *value;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let rounding = visuals.corner_radius.at_most(2);
        ui.painter().rect_filled(rect, rounding, visuals.bg_fill);
        egui::color_picker::show_color_at(ui.painter(), Color32::from(state.hsva), rect.shrink(2.0));
        ui.painter().rect_stroke(rect, rounding, visuals.bg_stroke, StrokeKind::Inside);
    }
    ui.ctx().data_mut(|data| data.insert_temp(state_id, state));
    response.on_hover_text(tr!("color-edit"))
}

/// The graphical RGB controls without egui's representation/copy toolbar.
fn rgb_picker(ui: &mut Ui, hsva: &mut Hsva) -> bool {
    use egui::ecolor::HsvaGamma;

    egui::color_picker::show_color(ui, Color32::from(*hsva), egui::vec2(PALETTE_WIDTH, 14.0));
    let mut gamma = HsvaGamma::from(*hsva);
    let mut sv = egui::vec2(gamma.s, 1.0 - gamma.v);
    let square =
        color_gradient(ui, egui::Vec2::splat(PALETTE_WIDTH), &mut sv, |s, y| HsvaGamma { s, v: 1.0 - y, a: 1.0, ..gamma }.into()).on_hover_text(tr!("color-saturation-value"));
    if square.changed() {
        gamma.s = sv.x;
        gamma.v = 1.0 - sv.y;
    }
    let mut hue = egui::vec2(gamma.h, 0.5);
    let hue_response = color_gradient(ui, egui::vec2(PALETTE_WIDTH, 14.0), &mut hue, |h, _| HsvaGamma { h, s: 1.0, v: 1.0, a: 1.0 }.into()).on_hover_text(tr!("color-hue"));
    if hue_response.changed() {
        gamma.h = hue.x;
    }
    let changed = square.changed() || hue_response.changed();
    if changed {
        *hsva = gamma.into();
    }
    changed
}

fn color_gradient(ui: &mut Ui, size: egui::Vec2, position: &mut egui::Vec2, color_at: impl Fn(f32, f32) -> Color32) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let is_square = size.x == size.y;
    if let Some(pointer) = response.interact_pointer_pos() {
        let next = egui::vec2(
            ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            if is_square { ((pointer.y - rect.top()) / rect.height()).clamp(0.0, 1.0) } else { 0.5 },
        );
        if *position != next {
            *position = next;
            response.mark_changed();
        }
    }
    if ui.is_rect_visible(rect) {
        if color_at(0.0, 0.0).a() < 255 || color_at(1.0, 1.0).a() < 255 {
            paint_alpha_checker(ui.painter(), rect);
        }
        // Sample both axes: a four-corner mesh introduces a diagonal seam
        // because triangle interpolation cannot represent this colour surface.
        const STEPS: u32 = 36;
        let rows = if is_square { STEPS } else { 1 };
        let mut mesh = egui::Mesh::default();
        for row in 0..=rows {
            for column in 0..=STEPS {
                let x = column as f32 / STEPS as f32;
                let y = row as f32 / rows as f32;
                mesh.colored_vertex(rect.min + egui::vec2(x * rect.width(), y * rect.height()), color_at(x, y));
                if row < rows && column < STEPS {
                    let index = row * (STEPS + 1) + column;
                    mesh.add_triangle(index, index + 1, index + STEPS + 1);
                    mesh.add_triangle(index + 1, index + STEPS + 1, index + STEPS + 2);
                }
            }
        }
        ui.painter().add(egui::Shape::mesh(mesh));
        let marker = rect.min + egui::vec2(position.x * rect.width(), position.y * rect.height());
        ui.painter().circle_stroke(marker, 5.0, Stroke::new(3.0, Color32::BLACK));
        ui.painter().circle_stroke(marker, 5.0, Stroke::new(1.0, Color32::WHITE));
    }
    response
}

/// Shared transparency background for colour controls and data legends.
pub(super) fn paint_alpha_checker(painter: &egui::Painter, rect: egui::Rect) {
    const SQUARE: f32 = 4.0;
    let dark = egui::Color32::from_gray(90);
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(130));
    let mut y = rect.top();
    let mut row = 0;
    while y < rect.bottom() {
        let mut x = rect.left() + if row % 2 == 0 { 0.0 } else { SQUARE };
        while x < rect.right() {
            let square = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(SQUARE, SQUARE)).intersect(rect);
            if square.is_positive() {
                painter.rect_filled(square, 0.0, dark);
            }
            x += SQUARE * 2.0;
        }
        y += SQUARE;
        row += 1;
    }
}

fn swatches(ui: &mut Ui, selected: Option<u8>) -> Option<u8> {
    let mut picked = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(CELL_GAP, CELL_GAP);
        // Keep palette rows at their actual cell height, independent of the
        // normal minimum height used by text fields and other controls.
        ui.spacing_mut().interact_size.y = CELL_SIZE.y;
        ui.horizontal(|ui| {
            // Basic hues followed by all neutral indices from white to dark gray.
            for index in [1, 2, 3, 4, 5, 6, 7, 255, 254, 253, 9, 252, 251, 8, 250] {
                if swatch(ui, index, selected, PRIMARY_CELL_SIZE).clicked() {
                    picked = Some(index);
                }
            }
        });
        // ACI alternates saturated colours and tints. Separate those into
        // two blocks so each hue column runs smoothly from light to dark.
        for tint in 0..2 {
            ui.add_space(6.0);
            for shade in 0..5 {
                ui.horizontal(|ui| {
                    for hue in 0..24 {
                        let index = 10 + hue * 10 + shade * 2 + tint;
                        if swatch(ui, index, selected, CELL_SIZE).clicked() {
                            picked = Some(index);
                        }
                    }
                });
            }
        }
    });
    picked
}

fn swatch(ui: &mut Ui, index: u8, selected: Option<u8>, size: egui::Vec2) -> Response {
    let [r, g, b] = COLOR_TABLE[index as usize];
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(r, g, b));
    if selected == Some(index) || response.hovered() || response.has_focus() {
        ui.painter().rect_stroke(rect, 0.0, Stroke::new(3.0, Color32::BLACK), StrokeKind::Inside);
        ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected == Some(index), index.to_string()));
    response.on_hover_text(tr!("color-aci-value", index = index.to_string()))
}
