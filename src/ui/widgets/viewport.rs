use std::{fmt::Debug, hash::Hash};

use crate::{
    model::block_model::{BlockModelSlice, ColorStop, MAX_COLOR_STOPS, MIN_COLOR_STOPS, OpenBlockModel, color_variable_default, render_value_range},
    ui::state::{EditorState, UiCommand},
};

/// A compact tool panel pinned inside the 3D viewport.
///
/// Apears in the bottom left. Used for tool configuration.
pub(crate) struct ViewportDockPanel {
    id: egui::Id,
    title: egui::WidgetText,
    viewport_rect: egui::Rect,
    min_width: f32,
    max_width: f32,
    margin: egui::Vec2,
}

impl ViewportDockPanel {
    pub(crate) fn new(id_source: impl Hash + Debug, title: impl Into<egui::WidgetText>, viewport_rect: egui::Rect) -> Self {
        Self {
            id: egui::Id::new(id_source),
            title: title.into().fallback_text_style(egui::TextStyle::Button),
            viewport_rect,
            min_width: 0.0,
            max_width: 320.0,
            margin: egui::vec2(10.0, 10.0),
        }
    }

    pub(crate) fn min_width(mut self, min_width: f32) -> Self {
        self.min_width = min_width;
        self
    }

    pub(crate) fn max_width(mut self, max_width: f32) -> Self {
        self.max_width = max_width;
        self
    }

    pub(crate) fn show<R>(self, ctx: &egui::Context, add_contents: impl FnOnce(&mut egui::Ui) -> R) {
        let pos = egui::pos2(self.viewport_rect.left() + self.margin.x, self.viewport_rect.bottom() - self.margin.y);
        egui::Area::new(self.id)
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.global_style())
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        if self.min_width > 0.0 {
                            ui.set_min_width(self.min_width);
                        }
                        ui.set_max_width(self.max_width);
                        ui.label(self.title);
                        ui.add_space(4.0);
                        add_contents(ui)
                    })
                    .inner
            });
    }
}

/// Minimum gap (in normalized `t`) enforced between adjacent colour-transfer
/// stops when dragging, so segments never collapse to zero width.
const STOP_EPSILON: f32 = 0.01;
const COLOR_STOP_HANDLE_WIDTH: f32 = 18.0;
const COLOR_PICKER_BUTTON_WIDTH: f32 = 40.0;
const COLOR_PICKER_BUTTON_HEIGHT: f32 = 18.0;
const COLOR_PICKER_EDGE_BUFFER: f32 = 8.0;
const LEGEND_SIDE_GUTTER: f32 = COLOR_PICKER_BUTTON_WIDTH * 0.5 + COLOR_PICKER_EDGE_BUFFER;
/// Fixed outer width of the block-model panel, including its horizontal
/// frame margins. Keeping this independent of the viewport prevents the
/// controls from shifting as the application window is resized.
const COLOR_SCALE_PANEL_WIDTH: f32 = 620.0;
const COLOR_SCALE_PANEL_MARGIN: f32 = 10.0;
const SCALE_BAR_TARGET_WIDTH: f64 = 320.0;
const SCALE_BAR_VIEWPORT_MARGIN: f32 = 10.0;
const SCALE_BAR_LABEL_OVERHANG: f32 = 18.0;
const SCALE_BAR_SEGMENT_FRACTIONS: [f64; 6] = [0.0, 0.05, 0.10, 0.25, 0.50, 1.0];

/// Clamps `raw_t` to `0..1` and, if that lands within `STOP_EPSILON` of an
/// existing stop, nudges it just outside that stop's epsilon band.
///
/// Without this, a double-click meant to insert a new stop near (or, after
/// edge-clamping, exactly on top of) an existing one produces a stop whose
/// `t` is within the later dedup pass's `1e-4` tolerance of the existing
/// stop - so the new stop is silently collapsed back out and insertion
/// appears to do nothing. This is most visible at the bar's edges: clamping
/// an overshot click to exactly `0.0`/`1.0` collides with a stop already
/// sitting at that exact edge (the common case for default colour ramps).
fn nudge_away_from_existing(stops: &[ColorStop], raw_t: f32) -> f32 {
    let mut t = raw_t.clamp(0.0, 1.0);
    for _ in 0..=stops.len() {
        let Some(collision) = stops.iter().find(|stop| (stop.t - t).abs() < STOP_EPSILON) else {
            return t;
        };
        t = if collision.t >= t {
            (collision.t - STOP_EPSILON).max(0.0)
        } else {
            (collision.t + STOP_EPSILON).min(1.0)
        };
    }
    t
}

/// Inserts a new stop at `t` into an already-sorted `stops` vec, keeping it
/// sorted, and returns the index it landed at.
///
/// Inserting in sorted order (rather than appending and re-sorting later) is
/// what keeps positional-neighbour logic correct on the very frame of
/// insertion: the value popup and drag clamps derive a stop's allowed range
/// from `stops[i-1]`/`stops[i+1]`, so a freshly-appended stop parked at the
/// end of the vec would be treated as the right-most one and clamped to the
/// old last stop's position.
fn insert_stop_sorted(stops: &mut Vec<ColorStop>, id: u64, t: f32) -> usize {
    let index = stops.partition_point(|stop| stop.t < t);
    let color = color32_to_straight(interpolate_stops(stops, t));
    stops.insert(index, ColorStop { id, t, color });
    index
}

/// Samples a colour-transfer function's rgba at `t`, mirroring the shader's
/// hard cutoffs. Values below the first stop are transparent; each stop then
/// remains active until the next.
fn interpolate_stops(stops: &[ColorStop], t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    if stops.is_empty() {
        return egui::Color32::TRANSPARENT;
    }
    let first = stops[0];
    let last = stops[stops.len() - 1];
    if t < first.t {
        return egui::Color32::TRANSPARENT;
    }
    if t >= last.t {
        return color32_from_straight(last.color);
    }
    for i in 1..stops.len() {
        if t < stops[i].t {
            return color32_from_straight(stops[i - 1].color);
        }
    }
    color32_from_straight(last.color)
}

fn color32_from_straight(c: [f32; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn color32_to_straight(c: egui::Color32) -> [f32; 4] {
    unmultiplied_srgba_to_straight(c.to_srgba_unmultiplied())
}

fn straight_to_unmultiplied_srgba(c: [f32; 4]) -> [u8; 4] {
    c.map(|component| (component.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn unmultiplied_srgba_to_straight(c: [u8; 4]) -> [f32; 4] {
    c.map(|component| component as f32 / 255.0)
}

#[derive(Clone, Copy)]
struct UnmultipliedColorPickerState {
    srgba: [u8; 4],
    hsva: egui::ecolor::Hsva,
}

fn hsva_from_unmultiplied_srgba([r, g, b, a]: [u8; 4]) -> egui::ecolor::Hsva {
    // `Hsva::from_srgba_unmultiplied` first constructs a premultiplied
    // `Color32`, which discards RGB precision as alpha approaches zero (and
    // discards RGB completely at zero). Build the opaque colour first so RGB
    // remains independent of alpha.
    let mut hsva = egui::ecolor::Hsva::from_srgb([r, g, b]);
    hsva.a = a as f32 / 255.0;
    hsva
}

/// An unmultiplied sRGBA picker that keeps the RGB channels independent from
/// alpha, including at very low opacity.
fn color_edit_button_srgba_unmultiplied(ui: &mut egui::Ui, srgba: &mut [u8; 4]) -> egui::Response {
    // egui's convenience method routes through its premultiplied `Rgba` and
    // `Color32` types. At low alpha that quantizes the premultiplied channels,
    // so merely dragging the opacity slider can visibly change the colour.
    // Drive the HSVA picker directly instead.
    let state_id = ui.auto_id_with("unmultiplied_color_picker_state");
    let cached = ui.ctx().data_mut(|data| data.get_temp::<UnmultipliedColorPickerState>(state_id));
    let mut hsva = cached
        .filter(|state| state.srgba == *srgba)
        .map(|state| state.hsva)
        .unwrap_or_else(|| hsva_from_unmultiplied_srgba(*srgba));

    let response = egui::color_picker::color_edit_button_hsva(ui, &mut hsva, egui::color_picker::Alpha::OnlyBlend);
    if response.changed() {
        *srgba = hsva.to_srgba_unmultiplied();
    }
    ui.ctx().data_mut(|data| data.insert_temp(state_id, UnmultipliedColorPickerState { srgba: *srgba, hsva }));
    response
}

fn trim_decimal_zeros(mut value: String) -> String {
    if let Some(dot) = value.find('.') {
        while value.ends_with('0') {
            value.pop();
        }
        if value.len() == dot + 1 {
            value.pop();
        }
    }
    if value == "-0" { "0".to_owned() } else { value }
}

/// Formats a grade value with a decimal precision that scales down as the
/// magnitude grows, so labels stay short without losing meaningful digits.
fn format_grade(value: f64) -> String {
    let decimals = if value.abs() >= 1000.0 {
        0
    } else if value.abs() >= 10.0 {
        1
    } else {
        3
    };
    trim_decimal_zeros(format!("{value:.decimals$}"))
}

fn inferred_decimal_places(value: f64) -> usize {
    for decimals in 0..=4 {
        let scale = 10_f64.powi(decimals as i32);
        let rounded = (value * scale).round() / scale;
        let tolerance = 1e-8 * value.abs().max(1.0);
        if (rounded - value).abs() <= tolerance {
            return decimals;
        }
    }
    4
}

fn format_grade_range(min: f64, max: f64) -> String {
    let decimals = inferred_decimal_places(min).max(inferred_decimal_places(max));
    format!("({min:.decimals$} - {max:.decimals$})")
}

/// An interactive colour-scale legend/editor pinned flush to the bottom-right
/// corner of the 3D viewport.
pub(crate) struct ColorScaleLegend<'a> {
    id: egui::Id,
    models: &'a [OpenBlockModel],
    viewport_rect: egui::Rect,
}

impl<'a> ColorScaleLegend<'a> {
    pub(crate) fn new(id_source: impl Hash + Debug, models: &'a [OpenBlockModel], viewport_rect: egui::Rect) -> Self {
        Self {
            id: egui::Id::new(id_source),
            models,
            viewport_rect,
        }
    }

    pub(crate) fn show(self, ctx: &egui::Context, editor: &mut EditorState, commands: &mut Vec<UiCommand>) -> Option<egui::Rect> {
        let visible_models = self.models.iter().filter(|model| model.visible).collect::<Vec<_>>();
        if visible_models.is_empty() {
            return None;
        }

        editor.viewport_block_model_id = editor
            .viewport_block_model_id
            .filter(|id| visible_models.iter().any(|model| model.id == *id))
            .or_else(|| visible_models.first().map(|model| model.id));

        let content_width = COLOR_SCALE_PANEL_WIDTH - COLOR_SCALE_PANEL_MARGIN * 2.0;
        let bar_width = content_width - LEGEND_SIDE_GUTTER * 2.0;
        let response = egui::Area::new(self.id.with("settings_panel"))
            // This is an interactive viewport overlay, so it must live above
            // egui's Background layer. Background-layer areas deliberately do
            // not claim pointer input inside the root viewport, which lets
            // right clicks leak through to camera orbit/context handling.
            .order(egui::Order::Middle)
            // `Area` defaults to movable, which gives its entire rectangle a
            // drag response even when `fixed_pos` is used. Leave interaction
            // to the child controls so middle-drag can pass to the viewport.
            .movable(false)
            .sense(egui::Sense::hover())
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(self.viewport_rect.right_bottom())
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 0, se: 0 })
                    .inner_margin(egui::Margin::symmetric(COLOR_SCALE_PANEL_MARGIN as i8, 8))
                    .show(ui, |ui| {
                        ui.set_min_width(content_width);
                        ui.set_max_width(content_width);
                        self.draw_model_tabs(ui, &visible_models, editor, content_width);
                        if editor.block_model_settings_collapsed {
                            return;
                        }
                        let Some(model) = editor.viewport_block_model_id.and_then(|id| visible_models.iter().copied().find(|model| model.id == id)) else {
                            return;
                        };
                        let range = active_variable_range(editor, model);
                        ui.separator();
                        ui.vertical_centered(|ui| {
                            self.draw_slice_controls(ui, content_width, model, commands);
                            if model_has_selectable_variable(model) {
                                ui.separator();
                                ui.label(egui::RichText::new("Colour mapping").strong().color(ui.visuals().weak_text_color()));
                                ui.add_space(2.0);
                                self.draw_variable_dropdown(ui, content_width, model, editor, commands);
                                ui.add_space(4.0);
                                if model.active_variable_is_categorical() {
                                    self.draw_category_legend(ui, content_width, model, commands);
                                } else if let Some((min, max)) = range {
                                    self.draw_bar(ui, content_width, bar_width, min, max, model, editor, commands);
                                } else {
                                    self.draw_no_data(ui, content_width);
                                }
                            }
                        });
                    });
            });
        Some(response.response.rect)
    }

    fn draw_model_tabs(&self, ui: &mut egui::Ui, visible_models: &[&OpenBlockModel], editor: &mut EditorState, content_width: f32) {
        let minimise_width = if editor.block_model_settings_collapsed { 0.0 } else { 82.0 };
        ui.horizontal(|ui| {
            egui::ScrollArea::horizontal()
                .id_salt(self.id.with("model_tabs_scroll"))
                .max_width((content_width - minimise_width).max(96.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.horizontal(|ui| {
                        for model in visible_models {
                            let active = editor.viewport_block_model_id == Some(model.id);
                            let response = ui
                                .add(
                                    egui::Button::selectable(active && !editor.block_model_settings_collapsed, egui::RichText::new(&model.name).strong())
                                        .min_size(egui::vec2(96.0, 24.0)),
                                )
                                .on_hover_text(if active && editor.block_model_settings_collapsed {
                                    "Show this block model's settings"
                                } else if active {
                                    "Minimise this block model's settings"
                                } else {
                                    "Show this block model's settings"
                                });
                            if response.clicked() {
                                if active {
                                    editor.block_model_settings_collapsed = !editor.block_model_settings_collapsed;
                                } else {
                                    editor.viewport_block_model_id = Some(model.id);
                                    editor.block_model_settings_collapsed = false;
                                }
                            }
                        }
                    });
                });
            if !editor.block_model_settings_collapsed
                && ui
                    .add_sized(egui::vec2(76.0, 22.0), egui::Button::new("Minimise").small())
                    .on_hover_text("Collapse settings to the model tabs")
                    .clicked()
            {
                editor.block_model_settings_collapsed = true;
            }
        });
    }

    fn draw_slice_controls(&self, ui: &mut egui::Ui, content_width: f32, model: &OpenBlockModel, commands: &mut Vec<UiCommand>) {
        let (lower, upper) = model.local_bounds();
        let full = BlockModelSlice { min: lower, max: upper };
        let mut slice = model.slice.unwrap_or(full).clamped_to(lower, upper);
        let mut changed = false;
        let (row, _) = ui.allocate_exact_size(egui::vec2(content_width, 24.0), egui::Sense::hover());
        let widget_height = 22.0;
        let top = row.center().y - widget_height * 0.5;
        let gap = ui.spacing().item_spacing.x;
        let slice_label_width = 34.0;
        let axis_label_width = 14.0;
        let value_width = 58.0;
        let reset_width = 42.0;
        let axis_width = (content_width - slice_label_width - reset_width - gap * 4.0) / 3.0;
        let mut x = row.left();

        ui.put(
            egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(slice_label_width, widget_height)),
            egui::Label::new("Slice"),
        );
        x += slice_label_width + gap;

        for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
            let axis_start = x;
            ui.put(
                egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(axis_label_width, widget_height)),
                egui::Label::new(egui::RichText::new(label).strong()),
            );
            x += axis_label_width + gap;
            let extent = (upper[axis] - lower[axis]).abs();
            let speed = (extent / 500.0).max(0.001);
            changed |= ui
                .put(
                    egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(value_width, widget_height)),
                    egui::DragValue::new(&mut slice.min[axis]).range(lower[axis]..=slice.max[axis]).speed(speed).max_decimals(4),
                )
                .on_hover_text(format!("{label} minimum"))
                .changed();
            x += value_width + gap;
            changed |= ui
                .put(
                    egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(value_width, widget_height)),
                    egui::DragValue::new(&mut slice.max[axis]).range(slice.min[axis]..=upper[axis]).speed(speed).max_decimals(4),
                )
                .on_hover_text(format!("{label} maximum"))
                .changed();
            x = axis_start + axis_width + gap;
        }

        let reset = ui
            .put(
                egui::Rect::from_min_size(egui::pos2(row.right() - reset_width, top), egui::vec2(reset_width, widget_height)),
                egui::Button::new("Reset").small(),
            )
            .on_hover_text("Restore the full model range")
            .clicked();
        if reset {
            commands.push(UiCommand::SetBlockModelSlice { id: model.id, slice: None });
        } else if changed {
            commands.push(UiCommand::SetBlockModelSlice { id: model.id, slice: Some(slice) });
        }
    }

    fn draw_variable_dropdown(&self, ui: &mut egui::Ui, content_width: f32, model: &OpenBlockModel, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
        let current = model.active_color_variable.as_deref().unwrap_or("");
        let selected_text = model
            .active_color_variable
            .as_deref()
            .map(|name| {
                if let Some(variable) = model.model.variable(name)
                    && is_categorical_variable(variable)
                {
                    format!("{name} ({})", format_category_count(variable))
                } else if let Some((min, max)) = cached_variable_range(editor, model, name) {
                    format!("{name} {}", format_grade_range(min, max))
                } else {
                    format!("{name} (no range)")
                }
            })
            .unwrap_or_else(|| "Choose a variable".to_owned());
        let filter_id = self.id.with(("variable_filter", model.id));
        let mut filter = ui.data_mut(|data| data.get_persisted::<String>(filter_id)).unwrap_or_default();

        let popup_id = self.id.with(("variable_popup", model.id));
        let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        let button_response = ui
            .add_sized(egui::vec2(content_width, 22.0), egui::Button::selectable(open, egui::RichText::new(selected_text).strong()))
            .on_hover_text("Choose the active block model variable");

        let _ = egui::Popup::menu(&button_response)
            .id(popup_id)
            .width(content_width)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let response = ui.add(egui::TextEdit::singleline(&mut filter).hint_text("Filter variables").desired_width(content_width - 12.0));
                if response.changed() {
                    ui.data_mut(|data| data.insert_persisted(filter_id, filter.clone()));
                }
                ui.add_space(4.0);

                let needle = filter.trim().to_ascii_lowercase();
                let mut any = false;
                egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                    for variable in model.model.color_variables().into_iter().filter(|variable| !variable.special) {
                        let name = variable.name.as_str();
                        if !needle.is_empty() && !name.to_ascii_lowercase().contains(&needle) {
                            continue;
                        }
                        any = true;
                        let range_text = if is_categorical_variable(variable) {
                            format_category_count(variable)
                        } else {
                            cached_variable_range(editor, model, name)
                                .map(|(min, max)| format_grade_range(min, max))
                                .unwrap_or_else(|| "(no usable range)".to_owned())
                        };
                        let selected = name == current;
                        let row = ui
                            .horizontal(|ui| {
                                let response = ui.selectable_label(selected, egui::RichText::new(name).strong());
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(egui::RichText::new(range_text).color(ui.visuals().weak_text_color()));
                                });
                                response
                            })
                            .inner;
                        if row.clicked() {
                            commands.push(UiCommand::SetBlockModelColorVariable {
                                id: model.id,
                                variable: name.to_owned(),
                            });
                            egui::Popup::close_id(ui.ctx(), popup_id);
                        }
                    }
                });
                if !any {
                    ui.label(egui::RichText::new("No matches").color(ui.visuals().weak_text_color()));
                }
            });
    }

    fn draw_category_legend(&self, ui: &mut egui::Ui, content_width: f32, model: &OpenBlockModel, commands: &mut Vec<UiCommand>) {
        let Some(variable) = model.active_color_variable.as_deref().and_then(|name| model.model.variable(name)) else {
            return;
        };
        let default = color_variable_default(variable);
        let mut stops = model.color_transfer.stops.clone();
        let mut changed = false;
        let categories = variable.strings.iter().take(MAX_COLOR_STOPS).collect::<Vec<_>>();
        ui.allocate_ui_with_layout(egui::vec2(content_width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            for (index, &(&code, label)) in categories.iter().enumerate() {
                let is_default = default == Some(code as f64);
                if model.active_category_code_present(code) == Some(false) {
                    continue;
                }
                let Some(stop_color) = stops.get(index).map(|stop| stop.color) else {
                    continue;
                };
                ui.push_id(("category_color", code), |ui| {
                    ui.horizontal(|ui| {
                        if !is_default || !model.hide_empty_color_values {
                            let mut srgba = straight_to_unmultiplied_srgba(stop_color);
                            if color_edit_button_srgba_unmultiplied(ui, &mut srgba)
                                .on_hover_text(if is_default {
                                    "Edit the colour used for empty values"
                                } else {
                                    "Edit this category colour"
                                })
                                .changed()
                            {
                                let color = unmultiplied_srgba_to_straight(srgba);
                                stops[index].color = color;
                                // Constant categorical columns carry a duplicate
                                // terminal stop so the transfer remains valid.
                                if categories.len() == 1 && stops.len() == 2 {
                                    stops[1].color = color;
                                }
                                changed = true;
                            }
                        }
                        let display_label = if label.trim().is_empty() { "(blank)" } else { label.as_str() };
                        let suffix = if is_default && model.hide_empty_color_values {
                            " (empty · hidden)"
                        } else if is_default {
                            " (empty)"
                        } else {
                            ""
                        };
                        ui.label(format!("{display_label}{suffix}"));
                    });
                });
            }
        });
        if variable.strings.len() > MAX_COLOR_STOPS {
            ui.label(egui::RichText::new(format!("Only the first {MAX_COLOR_STOPS} categories can be coloured")).color(ui.visuals().weak_text_color()));
        }
        if changed {
            commands.push(UiCommand::SetBlockModelColorStops { id: model.id, stops });
        }
    }

    fn draw_no_data(&self, ui: &mut egui::Ui, content_width: f32) {
        let handle_height = 18.0;
        let bar_height = 16.0;
        // Must match `draw_bar`'s `editor_height` so switching to/from a
        // variable with no usable range doesn't resize the legend.
        let editor_height = COLOR_PICKER_BUTTON_HEIGHT + 6.0;
        let label_height = 16.0;
        let (rect, _response) = ui.allocate_exact_size(
            egui::vec2(content_width, handle_height + bar_height + editor_height + label_height + 5.0),
            egui::Sense::hover(),
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No data for this variable",
            egui::FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_bar(&self, ui: &mut egui::Ui, content_width: f32, bar_width: f32, min: f64, max: f64, model: &OpenBlockModel, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
        let handle_height = 18.0;
        let bar_height = 16.0;
        let editor_height = COLOR_PICKER_BUTTON_HEIGHT + 6.0;
        let label_height = 16.0;
        let (rect, _response) = ui.allocate_exact_size(
            egui::vec2(content_width, handle_height + bar_height + editor_height + label_height + 5.0),
            egui::Sense::hover(),
        );
        let handle_row_rect = egui::Rect::from_min_size(rect.min, egui::vec2(content_width, handle_height));
        let bar_rect = egui::Rect::from_min_size(egui::pos2(rect.min.x + LEGEND_SIDE_GUTTER, handle_row_rect.bottom()), egui::vec2(bar_width, bar_height));
        let editor_row_rect = egui::Rect::from_min_size(egui::pos2(rect.min.x, bar_rect.bottom() + 2.0), egui::vec2(content_width, editor_height));

        let mut stops = model.color_transfer.stops.clone();
        let mut changed = false;
        let mut remove_index = None;
        let selected_id = self.id.with(("selected_color_stop", model.id));
        let mut selected_stop = ui.data_mut(|data| data.get_persisted::<usize>(selected_id)).unwrap_or(0).min(stops.len().saturating_sub(1));
        let value_popup_id = self.id.with(("stop_value_popup_open", model.id));
        let mut value_popup_stop = ui
            .data_mut(|data| data.get_persisted::<Option<usize>>(value_popup_id))
            .flatten()
            .filter(|index| *index < stops.len());
        // Set whenever a stop interaction this frame opens/keeps the value
        // popup, so the click-outside close below doesn't immediately undo it
        // (clicking a handle is "outside" the popup's own rect).
        let mut popup_kept_open = false;

        let text_color = ui.visuals().text_color();
        {
            let painter = ui.painter();
            const STRIPS: usize = 96;
            let strip_width = bar_rect.width() / STRIPS as f32;
            for i in 0..STRIPS {
                let t = i as f32 / (STRIPS - 1) as f32;
                let strip_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_rect.left() + i as f32 * strip_width, bar_rect.top()),
                    egui::vec2(strip_width + 0.5, bar_rect.height()),
                );
                painter.rect_filled(strip_rect, 0.0, interpolate_stops(&stops, t));
            }
            painter.rect_stroke(bar_rect, 0.0, egui::Stroke::new(1.0, egui::Color32::from_gray(40)), egui::StrokeKind::Outside);
        }

        let bar_response = ui
            .interact(bar_rect, self.id.with("color_stop_bar"), egui::Sense::click())
            .on_hover_text("Double-click to add a stop here");
        // A double-click on either the bar or a handle requests an insert.
        // We record the target `t` and apply it *after* the handle loop so the
        // insertion never shifts indices mid-iteration, and so it lands in
        // sorted position (see `insert_stop_sorted`).
        let mut pending_insert: Option<f32> = None;
        if bar_response.double_clicked()
            && stops.len() < MAX_COLOR_STOPS
            && let Some(pos) = bar_response.interact_pointer_pos()
        {
            let raw_t = (pos.x - bar_rect.left()) / bar_rect.width().max(1.0);
            pending_insert = Some(nudge_away_from_existing(&stops, raw_t));
        }

        for i in 0..stops.len() {
            let x = bar_rect.left() + bar_rect.width() * stops[i].t;
            let handle_rect = egui::Rect::from_center_size(egui::pos2(x, handle_row_rect.center().y), egui::vec2(COLOR_STOP_HANDLE_WIDTH, handle_height));
            let handle_id = self.id.with(("color_stop_handle", stops[i].id));
            let response = ui
                .interact(handle_rect, handle_id, egui::Sense::click_and_drag())
                .on_hover_text(if stops.len() > MIN_COLOR_STOPS {
                    "Drag to move · Right-click to remove"
                } else {
                    "Drag to move"
                });
            // Handles sit directly above the gradient strip, so a
            // double-click aimed at the bar near an existing stop (most
            // often the last one, at the visually prominent right edge)
            // lands on the handle instead. Without this, that click is
            // swallowed as an ordinary single click that just reselects the
            // handle, and no new stop is ever added.
            if response.double_clicked()
                && stops.len() < MAX_COLOR_STOPS
                && let Some(pos) = response.interact_pointer_pos()
            {
                let raw_t = (pos.x - bar_rect.left()) / bar_rect.width().max(1.0);
                pending_insert = Some(nudge_away_from_existing(&stops, raw_t));
            } else {
                if response.secondary_clicked() && stops.len() > MIN_COLOR_STOPS {
                    remove_index = Some(i);
                }
                if response.clicked() {
                    selected_stop = i;
                    value_popup_stop = Some(i);
                    popup_kept_open = true;
                }
                if response.dragged() {
                    let lower = if i == 0 { 0.0 } else { stops[i - 1].t + STOP_EPSILON };
                    let upper = if i + 1 == stops.len() { 1.0 } else { stops[i + 1].t - STOP_EPSILON };
                    let (lo, hi) = (lower.min(upper), lower.max(upper));
                    if let Some(pos) = response.interact_pointer_pos() {
                        let t = ((pos.x - bar_rect.left()) / bar_rect.width().max(1.0)).clamp(lo, hi);
                        if (stops[i].t - t).abs() > f32::EPSILON {
                            stops[i].t = t;
                            changed = true;
                        }
                    }
                    selected_stop = i;
                    if value_popup_stop.is_some() {
                        value_popup_stop = Some(i);
                        popup_kept_open = true;
                    }
                }
            }
            let painter = ui.painter();
            let marker_color = interpolate_stops(&stops, stops[i].t);
            let center = egui::pos2(x, handle_row_rect.center().y);
            let active = i == selected_stop || response.dragged() || response.hovered();
            painter.line_segment(
                [egui::pos2(x, handle_row_rect.bottom() - 1.0), egui::pos2(x, bar_rect.top())],
                egui::Stroke::new(if active { 1.5 } else { 1.0 }, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
            painter.circle_filled(center, if active { 5.5 } else { 4.5 }, marker_color);
            painter.circle_stroke(
                center,
                if active { 5.5 } else { 4.5 },
                egui::Stroke::new(if active { 1.5 } else { 1.0 }, if active { egui::Color32::BLACK } else { egui::Color32::from_gray(40) }),
            );
        }

        if let Some(t) = pending_insert
            && stops.len() < MAX_COLOR_STOPS
        {
            let new_index = insert_stop_sorted(&mut stops, editor.allocate_color_stop_id(), t);
            selected_stop = new_index;
            value_popup_stop = Some(new_index);
            popup_kept_open = true;
            changed = true;
        }

        if let Some(index) = remove_index {
            stops.remove(index);
            value_popup_stop = match value_popup_stop {
                Some(open_index) if open_index == index => None,
                Some(open_index) if open_index > index => Some(open_index - 1),
                other => other,
            };
            selected_stop = selected_stop.min(stops.len().saturating_sub(1));
            changed = true;
        }

        if !stops.is_empty() {
            if value_popup_stop == Some(selected_stop) {
                let popup_response = self.draw_stop_value_popup(ui, &mut stops, selected_stop, bar_rect, handle_row_rect, min, max, &mut changed);
                // Close the value input when the user clicks anywhere outside
                // it - unless this frame's click was on a stop handle, which
                // (re)opens it for that stop. Clicking inside the popup to edit
                // the value is not "elsewhere", so it stays open.
                if !popup_kept_open && popup_response.is_some_and(|response| response.clicked_elsewhere()) {
                    value_popup_stop = None;
                }
            }

            let selected_x = bar_rect.left() + bar_rect.width() * stops[selected_stop].t;
            let swatch_x = selected_x.clamp(rect.left() + COLOR_PICKER_BUTTON_WIDTH * 0.5, rect.right() - COLOR_PICKER_BUTTON_WIDTH * 0.5);
            let swatch_rect = egui::Rect::from_center_size(
                egui::pos2(swatch_x, editor_row_rect.center().y),
                egui::vec2(COLOR_PICKER_BUTTON_WIDTH, COLOR_PICKER_BUTTON_HEIGHT),
            );
            let mut srgba = straight_to_unmultiplied_srgba(stops[selected_stop].color);
            let response = ui
                .scope_builder(egui::UiBuilder::new().id_salt("color_stop_color_picker").max_rect(swatch_rect), |ui| {
                    ui.spacing_mut().interact_size = egui::vec2(COLOR_PICKER_BUTTON_WIDTH, COLOR_PICKER_BUTTON_HEIGHT);
                    color_edit_button_srgba_unmultiplied(ui, &mut srgba)
                })
                .inner
                .on_hover_text("Click to edit color; right-click to remove");
            let picker_remove_clicked =
                (response.secondary_clicked() || (ui.rect_contains_pointer(swatch_rect) && ui.input(|input| input.pointer.secondary_clicked()))) && stops.len() > MIN_COLOR_STOPS;
            if picker_remove_clicked {
                value_popup_stop = None;
                stops.remove(selected_stop);
                selected_stop = selected_stop.min(stops.len().saturating_sub(1));
                changed = true;
            }
            if response.changed() {
                stops[selected_stop].color = unmultiplied_srgba_to_straight(srgba);
                changed = true;
            }
        }

        if changed {
            let selected_t = stops.get(selected_stop).map(|stop| stop.t);
            let value_popup_t = value_popup_stop.and_then(|index| stops.get(index).map(|stop| stop.t));
            stops.sort_by(|a, b| a.t.total_cmp(&b.t));
            stops.dedup_by(|a, b| (a.t - b.t).abs() < 1e-4);
            selected_stop = selected_t
                .and_then(|t| {
                    stops
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| (a.t - t).abs().total_cmp(&(b.t - t).abs()))
                        .map(|(index, _)| index)
                })
                .unwrap_or(0);
            value_popup_stop = value_popup_t.and_then(|t| {
                stops
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| (a.t - t).abs().total_cmp(&(b.t - t).abs()))
                    .map(|(index, _)| index)
            });
            commands.push(UiCommand::SetBlockModelColorStops { id: model.id, stops });
        }
        ui.data_mut(|data| data.insert_persisted(selected_id, selected_stop));
        ui.data_mut(|data| data.insert_persisted(value_popup_id, value_popup_stop));

        let fractions: &[f32] = if bar_width < 260.0 {
            &[0.0, 1.0]
        } else if bar_width < 420.0 {
            &[0.0, 0.5, 1.0]
        } else {
            &[0.0, 0.25, 0.5, 0.75, 1.0]
        };
        let painter = ui.painter();
        for &t in fractions {
            let x = bar_rect.left() + bar_rect.width() * t;
            let value = normalized_to_value(t, min, max);
            let anchor = if t <= 0.01 {
                egui::Align2::LEFT_TOP
            } else if t >= 0.99 {
                egui::Align2::RIGHT_TOP
            } else {
                egui::Align2::CENTER_TOP
            };
            let label_top = editor_row_rect.bottom() + 1.0;
            painter.text(egui::pos2(x, label_top), anchor, format_grade(value), egui::FontId::proportional(11.0), text_color);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_stop_value_popup(
        &self,
        ui: &mut egui::Ui,
        stops: &mut [ColorStop],
        selected_stop: usize,
        bar_rect: egui::Rect,
        handle_row_rect: egui::Rect,
        min: f64,
        max: f64,
        changed: &mut bool,
    ) -> Option<egui::Response> {
        let stop = stops.get(selected_stop).copied()?;
        let selected_x = bar_rect.left() + bar_rect.width() * stop.t;
        let popup_pos = egui::pos2(selected_x, handle_row_rect.top() - 6.0);
        let area_response = egui::Area::new(self.id.with("stop_value_popup"))
            .order(egui::Order::Middle)
            // The popup is fixed to its stop. Its container must not retain a
            // drag response after the stop slider has been adjusted.
            .movable(false)
            .sense(egui::Sense::hover())
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(popup_pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let lower_t = if selected_stop == 0 { 0.0 } else { stops[selected_stop - 1].t + STOP_EPSILON };
                    let upper_t = if selected_stop + 1 == stops.len() {
                        1.0
                    } else {
                        stops[selected_stop + 1].t - STOP_EPSILON
                    };
                    let mut value = normalized_to_value(stop.t, min, max);
                    let min_value = normalized_to_value(lower_t.min(upper_t), min, max);
                    let max_value = normalized_to_value(lower_t.max(upper_t), min, max);
                    // No forced width: the popup frame hugs the drag value
                    // instead of leaving empty space the number sat
                    // left-aligned against.
                    let response = ui.add(
                        egui::DragValue::new(&mut value)
                            .range(min_value..=max_value)
                            .speed(((max - min).abs() / 250.0).max(0.0001))
                            .max_decimals(6),
                    );
                    if response.changed() {
                        stops[selected_stop].t = value_to_normalized(value, min, max).clamp(lower_t.min(upper_t), lower_t.max(upper_t));
                        *changed = true;
                    }
                });
            });
        // Keep the value editor above its background-layer parent.
        ui.ctx().set_sublayer(ui.layer_id(), area_response.response.layer_id);
        Some(area_response.response)
    }
}

/// A cartographic scale bar pinned to the viewport's bottom-right corner.
///
/// `world_per_point` is measured in metres per egui logical point. The scene
/// is orthographic outside fly mode, so this remains valid while orbiting as
/// well as in plan view.
pub(crate) struct ViewportScaleBar {
    id: egui::Id,
    viewport_rect: egui::Rect,
}

impl ViewportScaleBar {
    pub(crate) fn new(id_source: impl Hash + Debug, viewport_rect: egui::Rect) -> Self {
        Self {
            id: egui::Id::new(id_source),
            viewport_rect,
        }
    }

    pub(crate) fn show(self, ctx: &egui::Context, world_per_point: Option<f64>, viewport_background: [f32; 4], bottom_right_overlay: Option<egui::Rect>) {
        let Some(world_per_point) = world_per_point.filter(|value| value.is_finite() && *value > 0.0) else {
            return;
        };
        let distance = nice_scale_distance(world_per_point * SCALE_BAR_TARGET_WIDTH);
        let bar_width = (distance / world_per_point) as f32;
        let bottom = bottom_right_overlay
            .map(|rect| rect.top() - SCALE_BAR_VIEWPORT_MARGIN)
            .unwrap_or(self.viewport_rect.bottom() - SCALE_BAR_VIEWPORT_MARGIN);
        let anchor = egui::pos2(self.viewport_rect.right() - SCALE_BAR_VIEWPORT_MARGIN, bottom);
        let luminance = 0.2126 * viewport_background[0] + 0.7152 * viewport_background[1] + 0.0722 * viewport_background[2];
        let (ink, outline) = if luminance > 0.45 {
            (egui::Color32::BLACK, egui::Color32::WHITE)
        } else {
            (egui::Color32::WHITE, egui::Color32::BLACK)
        };

        egui::Area::new(self.id)
            .order(egui::Order::Background)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(anchor)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_width + SCALE_BAR_LABEL_OVERHANG * 2.0, 21.0), egui::Sense::hover());
                let painter = ui.painter();

                let bar_rect = egui::Rect::from_min_size(rect.min + egui::vec2(SCALE_BAR_LABEL_OVERHANG, 1.0), egui::vec2(bar_width, 5.0));
                for (index, fractions) in SCALE_BAR_SEGMENT_FRACTIONS.windows(2).enumerate() {
                    let segment = egui::Rect::from_min_max(
                        egui::pos2(bar_rect.left() + bar_width * fractions[0] as f32, bar_rect.top()),
                        egui::pos2(bar_rect.left() + bar_width * fractions[1] as f32, bar_rect.bottom()),
                    );
                    if index % 2 == 0 {
                        painter.rect_filled(segment, 0.0, ink);
                    } else {
                        painter.rect_stroke(segment, 0.0, egui::Stroke::new(1.0, ink), egui::StrokeKind::Inside);
                    }
                }

                let labels = scale_bar_labels(distance);
                let font = egui::FontId::new(11.0, egui::FontFamily::Name("open_sans_bold".into()));
                let label_y = bar_rect.bottom() + 2.0;
                for (index, fraction) in SCALE_BAR_SEGMENT_FRACTIONS.iter().copied().enumerate() {
                    let x = bar_rect.left() + bar_width * fraction as f32;
                    let label = &labels[index];
                    let position = egui::pos2(x, label_y);
                    painter.text(position + egui::vec2(1.0, 1.0), egui::Align2::CENTER_TOP, label, font.clone(), outline);
                    painter.text(position, egui::Align2::CENTER_TOP, label, font.clone(), ink);
                }
            });
    }
}

fn nice_scale_distance(target: f64) -> f64 {
    let exponent = target.log10().floor();
    let magnitude = 10.0_f64.powf(exponent);
    let normalized = target / magnitude;
    let multiplier = if normalized < 1.5 {
        1.0
    } else if normalized < 3.5 {
        2.0
    } else if normalized < 7.5 {
        5.0
    } else {
        10.0
    };
    multiplier * magnitude
}

fn format_scale_number(value: f64) -> String {
    let formatted = trim_decimal_zeros(format!("{value:.3}"));
    formatted.strip_prefix("0.").map_or(formatted.clone(), |fraction| format!(".{fraction}"))
}

fn scale_display_unit(metres: f64) -> (f64, &'static str) {
    [(0.001, "km"), (1.0, "m"), (100.0, "cm"), (1000.0, "mm")]
        .into_iter()
        .filter(|(scale, _)| metres * scale >= 0.1)
        .min_by_key(|(scale, _)| {
            SCALE_BAR_SEGMENT_FRACTIONS[1..SCALE_BAR_SEGMENT_FRACTIONS.len() - 1]
                .iter()
                .map(|fraction| format_scale_number(metres * fraction * scale).len())
                .sum::<usize>()
        })
        .unwrap_or((1000.0, "mm"))
}

fn scale_bar_labels(distance: f64) -> [String; SCALE_BAR_SEGMENT_FRACTIONS.len()] {
    let (unit_scale, unit) = scale_display_unit(distance);
    std::array::from_fn(|index| {
        let mut label = format_scale_number(distance * SCALE_BAR_SEGMENT_FRACTIONS[index] * unit_scale);
        if index + 1 == SCALE_BAR_SEGMENT_FRACTIONS.len() {
            label.push_str(unit);
        }
        label
    })
}

fn active_variable_range(editor: &mut EditorState, model: &OpenBlockModel) -> Option<(f64, f64)> {
    let name = model.active_color_variable.as_deref()?;
    cached_variable_range(editor, model, name)
}

/// Whether the legend can show this model: it already has an active variable,
/// or it has at least one supported, non-special colour variable the user can pick.
fn model_has_selectable_variable(model: &OpenBlockModel) -> bool {
    model.active_color_variable.is_some() || model.model.color_variables().into_iter().any(|variable| !variable.special)
}

fn cached_variable_range(editor: &mut EditorState, model: &OpenBlockModel, name: &str) -> Option<(f64, f64)> {
    let key = (model.id, name.to_owned());
    if let Some(range) = editor.block_model_variable_ranges.get(&key) {
        return *range;
    }
    let range = if model.active_color_variable.as_deref() == Some(name) {
        model.active_value_range()
    } else {
        model.model.variable(name).and_then(|variable| {
            let default = color_variable_default(variable);
            model
                .model
                .color_values(name)
                .ok()
                .and_then(|values| render_value_range(&values, &model.renderable_block_indices, default))
        })
    };
    editor.block_model_variable_ranges.insert(key, range);
    range
}

fn is_categorical_variable(variable: &crate::model::formats::block_model_data::BlockVariable) -> bool {
    matches!(variable.physical_type.as_str(), "namedbyte" | "namedshort")
}

fn category_count(variable: &crate::model::formats::block_model_data::BlockVariable) -> usize {
    variable.strings.len()
}

fn format_category_count(variable: &crate::model::formats::block_model_data::BlockVariable) -> String {
    let count = category_count(variable);
    format!("{count} categor{}", if count == 1 { "y" } else { "ies" })
}

fn normalized_to_value(t: f32, min: f64, max: f64) -> f64 {
    min + (max - min) * t.clamp(0.0, 1.0) as f64
}

fn value_to_normalized(value: f64, min: f64, max: f64) -> f32 {
    if (max - min).abs() <= f64::EPSILON {
        0.0
    } else {
        ((value - min) / (max - min)).clamp(0.0, 1.0) as f32
    }
}

/// A compact notification pinned to the top of the 3D viewport.
pub(crate) struct ViewportLabel {
    id: egui::Id,
    text: String,
    viewport_rect: egui::Rect,
    margin: f32,
}

impl ViewportLabel {
    pub(crate) fn new(id_source: impl Hash + Debug, text: impl Into<String>, viewport_rect: egui::Rect) -> Self {
        Self {
            id: egui::Id::new(id_source),
            text: text.into(),
            viewport_rect,
            margin: 12.0,
        }
    }

    pub(crate) fn show(self, ctx: &egui::Context) {
        let pos = egui::pos2(self.viewport_rect.center().x, self.viewport_rect.top() + self.margin);
        egui::Area::new(self.id)
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(255, 246, 218))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(226, 210, 164)))
                    .corner_radius(egui::CornerRadius::same(4))
                    .inner_margin(egui::Margin::symmetric(10, 5))
                    .show(ui, |ui| {
                        let text = egui::RichText::new(self.text).color(egui::Color32::from_rgb(52, 43, 25));
                        ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend));
                    });
            });
    }
}

/// Fixed, titleless shaded plan preview shown while slice mode is active.
pub(crate) struct ViewportMiniMap {
    id: egui::Id,
    viewport_rect: egui::Rect,
}

impl ViewportMiniMap {
    pub(crate) fn new(id_source: impl Hash + Debug, viewport_rect: egui::Rect) -> Self {
        Self {
            id: egui::Id::new(id_source),
            viewport_rect,
        }
    }

    pub(crate) fn show(self, ctx: &egui::Context, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
        #[cfg(target_arch = "wasm32")]
        let _ = commands;

        if editor.slice_preview_detached {
            return;
        }

        // Keep the embedded preview proportional to the main viewport. It is
        // deliberately not user-resizable: resizing an egui window captures
        // pointer interaction and makes the following middle-drag feel as if
        // the 3D canvas needs to be focused again.
        let preview_size = egui::vec2((self.viewport_rect.width() * 0.24).max(160.0), (self.viewport_rect.height() * 0.24).max(160.0));
        let preview_pos = egui::pos2(
            (self.viewport_rect.right() - preview_size.x - 10.0).max(self.viewport_rect.left()),
            self.viewport_rect.top() + 104.0,
        );
        let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::ZERO);
        egui::Window::new("")
            .id(self.id)
            .order(egui::Order::Foreground)
            .fixed_pos(preview_pos)
            .fixed_size(preview_size)
            .movable(false)
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .frame(frame)
            .show(ctx, |ui| {
                let size = ui.available_size().max(egui::vec2(120.0, 120.0));
                let response = if let Some(texture_id) = editor.slice_preview_texture {
                    ui.add(
                        egui::Image::new(egui::load::SizedTexture::new(texture_id, size))
                            .fit_to_exact_size(size)
                            .sense(egui::Sense::click_and_drag()),
                    )
                } else {
                    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                    ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
                    response
                };
                let pixels_per_point = ctx.pixels_per_point();
                editor.slice_preview_size_px = [
                    (response.rect.width() * pixels_per_point).round().max(1.0) as u32,
                    (response.rect.height() * pixels_per_point).round().max(1.0) as u32,
                ];

                let fitted_zoom = crate::ui::state::fitted_slice_preview_zoom(editor.slice_half_length, editor.slice_preview_size_px[1], f64::from(pixels_per_point));
                let current_zoom = fitted_zoom * editor.slice_preview_navigation.zoom_multiplier();
                let mut navigation_changed = false;
                if response.dragged_by(egui::PointerButton::Middle) {
                    let delta = response.drag_delta() * pixels_per_point;
                    navigation_changed |=
                        editor
                            .slice_preview_navigation
                            .pan_by_pixels([f64::from(delta.x), f64::from(delta.y)], current_zoom, f64::from(editor.slice_preview_size_px[1]));
                }

                if response.hovered() {
                    let scroll = ui.input(|input| {
                        input
                            .events
                            .iter()
                            .filter_map(|event| match event {
                                egui::Event::MouseWheel { unit, delta, .. } => Some(match unit {
                                    egui::MouseWheelUnit::Point => f64::from(delta.y * pixels_per_point),
                                    // Match the native camera's convention that one wheel
                                    // line is approximately one hundred physical pixels.
                                    egui::MouseWheelUnit::Line => f64::from(delta.y) * 100.0,
                                    egui::MouseWheelUnit::Page => f64::from(delta.y) * f64::from(editor.slice_preview_size_px[1]),
                                }),
                                _ => None,
                            })
                            .sum::<f64>()
                    });
                    if scroll != 0.0 {
                        let pointer = response.hover_pos().unwrap_or(response.rect.center());
                        let cursor_px = [
                            f64::from((pointer.x - response.rect.left()) * pixels_per_point),
                            f64::from((pointer.y - response.rect.top()) * pixels_per_point),
                        ];
                        navigation_changed |= editor.slice_preview_navigation.zoom_at_pixel(
                            scroll,
                            cursor_px,
                            [f64::from(editor.slice_preview_size_px[0]), f64::from(editor.slice_preview_size_px[1])],
                            fitted_zoom,
                        );
                    }
                }

                if navigation_changed {
                    ctx.request_repaint();
                }

                #[cfg(not(target_arch = "wasm32"))]
                if response.clicked() {
                    commands.push(UiCommand::SetSlicePreviewDetached(true));
                }
                #[cfg(not(target_arch = "wasm32"))]
                response.on_hover_text("Middle-drag to pan · Scroll to zoom · Click to detach");
                #[cfg(target_arch = "wasm32")]
                response.on_hover_text("Middle-drag to pan · Scroll to zoom");
            });
    }
}
