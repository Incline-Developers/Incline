use std::{fmt::Debug, hash::Hash};

use crate::{
    model::block_model::{BlockModelSlice, Boundary, ColorTransferFunction, MAX_GRADIENT_ENTRIES, OpenBlockModel, color_variable_default, render_value_range},
    ui::{
        state::{EditorState, UiCommand},
        widgets::menu,
    },
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
                // The same card as a floating menu, minus the drag bar's close
                // button: a docked tool panel is one of the menu family, and
                // the fields inside it are the menu fields.
                let surface = menu::menu_surface(ui.visuals());
                egui::Frame::new()
                    .fill(surface)
                    .stroke(menu::menu_border(ui.visuals()))
                    .corner_radius(egui::CornerRadius::same(menu::MENU_CORNER_RADIUS))
                    .show(ui, |ui| {
                        menu::apply_menu_style(ui, surface);
                        if self.min_width > 0.0 {
                            ui.set_min_width(self.min_width);
                        }
                        ui.set_max_width(self.max_width);
                        let title_rect = ui.allocate_exact_size(egui::vec2(self.min_width, menu::TITLE_BAR_HEIGHT), egui::Sense::hover()).0;
                        let inner = egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 8)).show(ui, add_contents).inner;
                        let mut title_rect = title_rect;
                        title_rect.max.x = ui.min_rect().right();
                        menu::draw_menu_heading(ui, &self.title, title_rect, surface);
                        inner
                    })
                    .inner
            });
    }
}

/// The small "Reset" button in the Slice and Colour-mapping section headers.
///
/// Sized to its own label with tight padding and `Extend` wrap - the properties
/// panel sets a global `Truncate` that would otherwise clip it to "Re…".
fn reset_section_button(ui: &mut egui::Ui, tooltip: &str) -> bool {
    ui.scope(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
        ui.add(egui::Button::new(egui::RichText::new("Reset").small())).on_hover_text(tooltip.to_owned()).clicked()
    })
    .inner
}

/// A colormap as the ramp widget manipulates it.
///
/// The widget is a bar, so dragging along it is naturally a `0..1` position,
/// while a [`Boundary`] stores an absolute data value. Both are carried, and
/// `value` is only recomputed when a handle actually moves - so recolouring a
/// band whose boundary sits outside the current render range (an imported
/// colormap authored wider than its data) does not quietly clamp it into range.
///
/// OMF's `gradient[0]` - the band under the first boundary - is not editable
/// here: the widget always writes [`BELOW_FIRST_BOUNDARY`], so the first
/// boundary acts as a grade cutoff that hides everything beneath it.
struct UiRamp {
    stops: Vec<UiStop>,
    interpolate: bool,
}

#[derive(Clone, Copy)]
struct UiStop {
    id: u64,
    /// Position along the bar, `0..1`, clamped from `value`.
    t: f32,
    /// Absolute value in the variable's data units.
    value: f64,
    /// See [`Boundary::inclusive`].
    inclusive: bool,
    /// The colour of the band at or above this boundary - `gradient[i + 1]`.
    color: [f32; 4],
}

/// The colour of everything below the first boundary. Always transparent, so
/// blocks under the cutoff are not drawn.
const BELOW_FIRST_BOUNDARY: [f32; 4] = [0.0; 4];

impl UiRamp {
    /// Project a colormap onto the bar. A continuous colormap's evenly spaced
    /// gradient becomes evenly spaced handles, so both kinds edit identically;
    /// `to_transfer` puts each back into its own shape.
    fn from_transfer(transfer: &ColorTransferFunction, min: f64, max: f64) -> Self {
        match transfer {
            // Categories are drawn by `draw_category_legend`, never here.
            ColorTransferFunction::Category { .. } => Self {
                stops: Vec::new(),
                interpolate: false,
            },
            ColorTransferFunction::Continuous { range, gradient } => {
                let (low, high) = *range;
                let last = gradient.len().saturating_sub(1).max(1);
                Self {
                    stops: gradient
                        .iter()
                        .enumerate()
                        .map(|(index, &color)| {
                            let value = low + (high - low) * index as f64 / last as f64;
                            UiStop {
                                id: index as u64 + 1,
                                t: value_to_normalized(value, min, max),
                                value,
                                inclusive: false,
                                color,
                            }
                        })
                        .collect(),
                    interpolate: true,
                }
            }
            ColorTransferFunction::Discrete { boundaries, gradient } => Self {
                stops: boundaries
                    .iter()
                    .enumerate()
                    .map(|(index, boundary)| UiStop {
                        id: boundary.id,
                        t: value_to_normalized(boundary.value, min, max),
                        value: boundary.value,
                        inclusive: boundary.inclusive,
                        color: gradient.get(index + 1).copied().unwrap_or([0.0; 4]),
                    })
                    .collect(),
                interpolate: false,
            },
        }
    }

    fn to_transfer(&self) -> ColorTransferFunction {
        if self.interpolate {
            // A continuous gradient is evenly spaced by definition, so the
            // handles only get to set the span; their colours are resampled
            // onto an even grid across it.
            let (low, high) = (self.stops.first().map_or(0.0, |stop| stop.value), self.stops.last().map_or(1.0, |stop| stop.value));
            let samples = self.stops.len().max(2);
            let span = high - low;
            return ColorTransferFunction::Continuous {
                range: (low, high),
                gradient: (0..samples).map(|index| self.sample(low + span * index as f64 / (samples - 1) as f64)).collect(),
            };
        }
        ColorTransferFunction::Discrete {
            boundaries: self
                .stops
                .iter()
                .map(|stop| Boundary {
                    id: stop.id,
                    value: stop.value,
                    inclusive: stop.inclusive,
                })
                .collect(),
            gradient: std::iter::once(BELOW_FIRST_BOUNDARY).chain(self.stops.iter().map(|stop| stop.color)).collect(),
        }
    }

    /// Linear interpolation across the handles, clamped at both ends. Only used
    /// when resampling a continuous gradient.
    fn sample(&self, value: f64) -> [f32; 4] {
        match self.stops.iter().position(|stop| stop.value > value) {
            None => self.stops.last().map_or(BELOW_FIRST_BOUNDARY, |stop| stop.color),
            Some(0) => self.stops[0].color,
            Some(upper) => {
                let (low, high) = (self.stops[upper - 1], self.stops[upper]);
                let span = high.value - low.value;
                let f = if span.abs() <= f64::EPSILON { 0.0 } else { (value - low.value) / span };
                crate::model::block_model::lerp_rgba(low.color, high.color, f as f32)
            }
        }
    }

    /// The colour the bar shows at `t`, mirroring `ramp_rgba`.
    fn color_at_t(&self, t: f32) -> egui::Color32 {
        let Some(first) = self.stops.first() else {
            return color32_from_straight(BELOW_FIRST_BOUNDARY);
        };
        if t < first.t {
            return color32_from_straight(BELOW_FIRST_BOUNDARY);
        }
        let last = self.stops[self.stops.len() - 1];
        if t >= last.t {
            return color32_from_straight(last.color);
        }
        for pair in self.stops.windows(2) {
            if t < pair[1].t {
                if !self.interpolate {
                    return color32_from_straight(pair[0].color);
                }
                let span = pair[1].t - pair[0].t;
                let f = if span <= f32::EPSILON { 0.0 } else { (t - pair[0].t) / span };
                return color32_from_straight(crate::model::block_model::lerp_rgba(pair[0].color, pair[1].color, f));
            }
        }
        color32_from_straight(last.color)
    }
}

impl UiStop {
    fn set_t(&mut self, t: f32, min: f64, max: f64) {
        self.t = t;
        self.value = normalized_to_value(t, min, max);
    }
}

/// Minimum gap (in normalized `t`) enforced between adjacent colour-transfer
/// stops when dragging, so segments never collapse to zero width.
const STOP_EPSILON: f32 = 0.01;
/// Side of a boundary handle's square hit target.
const COLOR_STOP_HANDLE_SIZE: f32 = 18.0;
const COLOR_PICKER_BUTTON_WIDTH: f32 = 40.0;
const COLOR_PICKER_BUTTON_HEIGHT: f32 = 18.0;
/// The gradient bar and its columns: handles to its left, value labels and
/// the colour picker to its right.
const LEGEND_BAR_HEIGHT: f32 = 400.0;
const LEGEND_BAR_THICKNESS: f32 = 16.0;
/// Column drawn left of each boundary handle: the boundary's value in the
/// variable's own units (an editable number box once clicked), then the `≤`
/// marker when the boundary is inclusive.
const LEGEND_STOP_VALUE_WIDTH: f32 = 46.0;
/// Wide enough for a handle, the `≤` marker beside it, and the value column.
const LEGEND_HANDLE_COLUMN_WIDTH: f32 = COLOR_STOP_HANDLE_SIZE + 8.0 + LEGEND_STOP_VALUE_WIDTH;
const LEGEND_COLUMN_GAP: f32 = 6.0;
const LEGEND_LABEL_FRACTIONS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
/// Width of the per-category share column drawn left of each legend swatch.
const LEGEND_CATEGORY_PERCENT_WIDTH: f32 = 34.0;
/// Floor on the width the block-model properties lay themselves out in, so a
/// very narrow explorer panel scrolls horizontally rather than collapsing the
/// legend's columns into each other.
const MIN_CONTENT_WIDTH: f32 = 200.0;
const SCALE_BAR_TARGET_WIDTH: f64 = 320.0;
const SCALE_BAR_VIEWPORT_MARGIN: f32 = 10.0;
const SCALE_BAR_LABEL_OVERHANG: f32 = 18.0;
const SCALE_BAR_SEGMENT_FRACTIONS: [f64; 6] = [0.0, 0.05, 0.10, 0.25, 0.50, 1.0];
/// Height of the scale bar's block: the bar itself and the labels under it.
const SCALE_BAR_HEIGHT: f32 = 21.0;
/// Gap between the embedded slice preview and the viewport's edges.
const SLICE_PREVIEW_MARGIN: f32 = 10.0;
/// Drop from the top of the viewport to the embedded slice preview when the
/// orientation gizmo it normally hangs below is switched off.
const SLICE_PREVIEW_TOP: f32 = 10.0;
/// Bounds on the embedded slice preview's side. It tracks the viewport's
/// shorter side between them, so a small viewport still gets a usable preview
/// and a large one is not handed most of the scene as a minimap.
const SLICE_PREVIEW_MIN_SIZE: f32 = 160.0;
const SLICE_PREVIEW_MAX_SIZE: f32 = 320.0;

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
fn nudge_away_from_existing(stops: &[UiStop], raw_t: f32) -> f32 {
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
fn insert_stop_sorted(ramp: &mut UiRamp, id: u64, t: f32, min: f64, max: f64) -> usize {
    let index = ramp.stops.partition_point(|stop| stop.t < t);
    // A new boundary splits the band it lands in, so both halves start out the
    // colour that band already had.
    let color = color32_to_straight(ramp.color_at_t(t));
    ramp.stops.insert(
        index,
        UiStop {
            id,
            t,
            value: normalized_to_value(t, min, max),
            inclusive: false,
            color,
        },
    );
    index
}

/// Chequerboard behind the gradient bar, so a band with alpha reads as
/// transparent rather than as the panel background - including the stretch
/// under the first boundary, which is always fully transparent.
fn paint_alpha_checker(painter: &egui::Painter, rect: egui::Rect) {
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

fn color32_from_straight(c: [f32; 4]) -> egui::Color32 {
    let [r, g, b, a] = straight_to_unmultiplied_srgba(c);
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn color32_to_straight(c: egui::Color32) -> [f32; 4] {
    unmultiplied_srgba_to_straight(c.to_srgba_unmultiplied())
}

/// Colour-stop RGB is linear: the scene renders into an sRGB surface view (see
/// `rendering/graphics/init.rs`), so the shader emits linear and the GPU
/// encodes it. egui works in sRGB bytes, so the swatch and the picker have to
/// go through the transfer function rather than scaling by 255 - otherwise a
/// legend swatch is visibly darker than the blocks it describes, and a colour
/// picked in the legend comes back brighter than the one chosen. Alpha is
/// linear in both, so it only scales.
fn straight_to_unmultiplied_srgba(c: [f32; 4]) -> [u8; 4] {
    let [r, g, b] = [c[0], c[1], c[2]].map(crate::rendering::color::linear_to_srgb_byte);
    [r, g, b, (c[3].clamp(0.0, 1.0) * 255.0).round() as u8]
}

fn unmultiplied_srgba_to_straight(c: [u8; 4]) -> [f32; 4] {
    let mut straight = crate::rendering::color::rgb_bytes_to_linear_rgba([c[0], c[1], c[2]]);
    straight[3] = f32::from(c[3]) / 255.0;
    straight
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

/// The interactive colour-scale and slice editor for one block model, drawn
/// inline in the explorer's properties panel.
pub(crate) struct BlockModelProperties<'a> {
    id: egui::Id,
    model: &'a OpenBlockModel,
}

impl<'a> BlockModelProperties<'a> {
    pub(crate) fn new(id_source: impl Hash + Debug, model: &'a OpenBlockModel) -> Self {
        Self {
            id: egui::Id::new(id_source),
            model,
        }
    }

    pub(crate) fn show(self, ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
        let model = self.model;
        let content_width = ui.available_width().max(MIN_CONTENT_WIDTH);
        let range = active_variable_range(editor, model);

        self.draw_slice_controls(ui, content_width, model, commands);
        if !model_has_selectable_variable(model) {
            return;
        }
        ui.add_space(6.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Colour mapping").strong().color(ui.visuals().weak_text_color()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if reset_section_button(ui, "Rebuild this variable's colours from its data") {
                    commands.push(UiCommand::ResetBlockModelColorTransfer { id: model.id });
                }
            });
        });
        ui.add_space(2.0);
        self.draw_variable_dropdown(ui, content_width, model, editor, commands);
        ui.add_space(4.0);
        if model.active_variable_is_categorical() {
            self.draw_category_legend(ui, content_width, model, commands);
        } else if let Some((min, max)) = range {
            self.draw_bar(ui, content_width, min, max, model, editor, commands);
        } else {
            self.draw_no_data(ui, content_width);
        }
    }

    /// One row per axis, so the six bounds fit a side panel's width.
    fn draw_slice_controls(&self, ui: &mut egui::Ui, content_width: f32, model: &OpenBlockModel, commands: &mut Vec<UiCommand>) {
        let (lower, upper) = model.local_bounds();
        let full = BlockModelSlice { min: lower, max: upper };
        let mut slice = model.slice.unwrap_or(full).clamped_to(lower, upper);
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Slice").strong().color(ui.visuals().weak_text_color()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if reset_section_button(ui, "Restore the full model range") {
                    commands.push(UiCommand::SetBlockModelSlice { id: model.id, slice: None });
                }
            });
        });

        let axis_label_width = 14.0;
        let gap = ui.spacing().item_spacing.x;
        let value_width = ((content_width - axis_label_width - gap * 2.0) * 0.5).max(48.0);
        for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
            let extent = (upper[axis] - lower[axis]).abs();
            let speed = (extent / 500.0).max(0.001);
            ui.horizontal(|ui| {
                ui.add_sized(egui::vec2(axis_label_width, 20.0), egui::Label::new(egui::RichText::new(label).strong()));
                changed |= ui
                    .add_sized(
                        egui::vec2(value_width, 20.0),
                        egui::DragValue::new(&mut slice.min[axis]).range(lower[axis]..=slice.max[axis]).speed(speed).max_decimals(4),
                    )
                    .on_hover_text(format!("{label} minimum"))
                    .changed();
                changed |= ui
                    .add_sized(
                        egui::vec2(value_width, 20.0),
                        egui::DragValue::new(&mut slice.max[axis]).range(slice.min[axis]..=upper[axis]).speed(speed).max_decimals(4),
                    )
                    .on_hover_text(format!("{label} maximum"))
                    .changed();
            });
        }

        if changed {
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
        // A category attribute's colours are a plain per-name list in OMF, so
        // the legend edits that list directly - there are no boundaries to
        // place and no cutoff band, and every category the file names can be
        // coloured.
        let ColorTransferFunction::Category { gradient } = model.color_transfer() else {
            return;
        };
        let default = color_variable_default(variable);
        let mut gradient = gradient.clone();
        let mut changed = false;
        ui.allocate_ui_with_layout(egui::vec2(content_width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            for (&code, label) in &variable.strings {
                let is_default = default == Some(code as f64);
                if model.active_category_code_present(code) == Some(false) {
                    continue;
                }
                let color = gradient.get(&code).copied().unwrap_or([0.72, 0.72, 0.75, 1.0]);
                let percent_text = model.active_category_code_fraction(code).map(format_category_percent).unwrap_or_default();
                ui.push_id(("category_color", code), |ui| {
                    ui.horizontal(|ui| {
                        // Share of the renderable blocks in this category, in a
                        // fixed column so the names still line up beneath it.
                        ui.allocate_ui_with_layout(
                            egui::vec2(LEGEND_CATEGORY_PERCENT_WIDTH, ui.spacing().interact_size.y),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(egui::RichText::new(&percent_text).color(ui.visuals().weak_text_color()));
                            },
                        );
                        if !is_default || !model.hide_empty_color_values {
                            let mut srgba = straight_to_unmultiplied_srgba(color);
                            if color_edit_button_srgba_unmultiplied(ui, &mut srgba)
                                .on_hover_text(if is_default {
                                    "Edit the colour used for empty values"
                                } else {
                                    "Edit this category colour"
                                })
                                .changed()
                            {
                                gradient.insert(code, unmultiplied_srgba_to_straight(srgba));
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
        if variable.strings.len() >= MAX_GRADIENT_ENTRIES {
            ui.label(
                egui::RichText::new(format!(
                    "All {} categories keep their colour; only the first {} are drawn distinctly",
                    variable.strings.len(),
                    MAX_GRADIENT_ENTRIES - 1
                ))
                .color(ui.visuals().weak_text_color()),
            );
        }
        if changed {
            commands.push(UiCommand::SetBlockModelColorTransfer {
                id: model.id,
                transfer: ColorTransferFunction::Category { gradient },
            });
        }
    }

    fn draw_no_data(&self, ui: &mut egui::Ui, content_width: f32) {
        // Must match `draw_bar`'s allocation so switching to or from a
        // variable with no usable range doesn't resize the legend.
        let (rect, _response) = ui.allocate_exact_size(egui::vec2(content_width, LEGEND_BAR_HEIGHT), egui::Sense::hover());
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No data for this variable",
            egui::FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
    }

    /// The ramp runs bottom to top - `t = 0`, the variable's minimum, sits at
    /// the bar's bottom edge - so the boundary handles, the value labels and
    /// the colour picker sit side by side in the narrow properties panel
    /// rather than competing for its width.
    #[allow(clippy::too_many_arguments)]
    fn draw_bar(&self, ui: &mut egui::Ui, content_width: f32, min: f64, max: f64, model: &OpenBlockModel, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
        let text_color = ui.visuals().text_color();
        // The value labels are laid out (but not painted) up front: their
        // widest line decides how wide the legend is as a whole, and so both
        // where it starts and where the colour picker column sits.
        let label_font = egui::FontId::proportional(11.0);
        let labels: Vec<_> = LEGEND_LABEL_FRACTIONS
            .iter()
            .map(|&t| {
                (
                    t,
                    ui.painter().layout_no_wrap(format_grade(normalized_to_value(t, min, max)), label_font.clone(), text_color),
                )
            })
            .collect();
        let label_width = labels.iter().map(|(_, galley)| galley.size().x).fold(0.0_f32, f32::max);

        let (rect, _response) = ui.allocate_exact_size(egui::vec2(content_width, LEGEND_BAR_HEIGHT), egui::Sense::hover());
        // Handle column | bar | gap | labels | gap | colour picker, centred as
        // a group in the panel, and pinned left when it doesn't fit.
        let group_width = LEGEND_HANDLE_COLUMN_WIDTH + LEGEND_BAR_THICKNESS + LEGEND_COLUMN_GAP + label_width + LEGEND_COLUMN_GAP + COLOR_PICKER_BUTTON_WIDTH;
        let group_left = rect.left() + ((rect.width() - group_width) * 0.5).max(0.0);
        let handle_column_rect = egui::Rect::from_min_size(egui::pos2(group_left, rect.top()), egui::vec2(LEGEND_HANDLE_COLUMN_WIDTH, LEGEND_BAR_HEIGHT));
        let bar_rect = egui::Rect::from_min_size(egui::pos2(handle_column_rect.right(), rect.top()), egui::vec2(LEGEND_BAR_THICKNESS, LEGEND_BAR_HEIGHT));
        let handle_center_x = handle_column_rect.right() - COLOR_STOP_HANDLE_SIZE * 0.5;
        let y_at = |t: f32| bar_rect.bottom() - bar_rect.height() * t;
        let t_at = |y: f32| (bar_rect.bottom() - y) / bar_rect.height().max(1.0);

        let label_left = bar_rect.right() + LEGEND_COLUMN_GAP;
        let picker_left = (label_left + label_width + LEGEND_COLUMN_GAP).min(rect.right() - COLOR_PICKER_BUTTON_WIDTH).max(label_left);

        let mut ramp = UiRamp::from_transfer(model.color_transfer(), min, max);
        let mut changed = false;
        let mut remove_index = None;
        let selected_id = self.id.with(("selected_color_stop", model.id));
        let mut selected = ui
            .data_mut(|data| data.get_persisted::<usize>(selected_id))
            .filter(|index| *index < ramp.stops.len())
            .unwrap_or(0);
        let value_popup_id = self.id.with(("stop_value_popup_open", model.id));
        let mut value_popup_stop = ui
            .data_mut(|data| data.get_persisted::<Option<usize>>(value_popup_id))
            .flatten()
            .filter(|index| *index < ramp.stops.len());
        // Set whenever a stop interaction this frame opens/keeps the value
        // popup, so the click-outside close below doesn't immediately undo it
        // (clicking a handle is "outside" the popup's own rect).
        let mut popup_kept_open = false;

        {
            let painter = ui.painter();
            paint_alpha_checker(painter, bar_rect);
            const STRIPS: usize = 96;
            let strip_height = bar_rect.height() / STRIPS as f32;
            for i in 0..STRIPS {
                let t = i as f32 / (STRIPS - 1) as f32;
                let strip_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_rect.left(), bar_rect.bottom() - (i as f32 + 1.0) * strip_height),
                    egui::vec2(bar_rect.width(), strip_height + 0.5),
                );
                painter.rect_filled(strip_rect, 0.0, ramp.color_at_t(t));
            }
            painter.rect_stroke(bar_rect, 0.0, egui::Stroke::new(1.0, egui::Color32::from_gray(40)), egui::StrokeKind::Outside);
        }

        let bar_response = ui
            .interact(bar_rect, self.id.with("color_stop_bar"), egui::Sense::click())
            .on_hover_text("Double-click to add a boundary here");
        // A double-click on either the bar or a handle requests an insert.
        // We record the target `t` and apply it *after* the handle loop so the
        // insertion never shifts indices mid-iteration, and so it lands in
        // sorted position (see `insert_stop_sorted`).
        let mut pending_insert: Option<f32> = None;
        if bar_response.double_clicked()
            && ramp.stops.len() < MAX_GRADIENT_ENTRIES - 1
            && let Some(pos) = bar_response.interact_pointer_pos()
        {
            pending_insert = Some(nudge_away_from_existing(&ramp.stops, t_at(pos.y)));
        }

        for i in 0..ramp.stops.len() {
            let y = y_at(ramp.stops[i].t);
            let handle_rect = egui::Rect::from_center_size(egui::pos2(handle_center_x, y), egui::vec2(COLOR_STOP_HANDLE_SIZE, COLOR_STOP_HANDLE_SIZE));
            let handle_id = self.id.with(("color_stop_handle", ramp.stops[i].id));
            let response = ui.interact(handle_rect, handle_id, egui::Sense::click_and_drag()).on_hover_text(if ramp.stops.len() > 1 {
                "Drag to move · Right-click to remove · Middle-click toggles ≤"
            } else {
                "Drag to move · Middle-click toggles ≤"
            });
            // Handles sit directly beside the gradient strip, so a
            // double-click aimed at the bar near an existing stop (most
            // often the last one, at the visually prominent top edge)
            // lands on the handle instead. Without this, that click is
            // swallowed as an ordinary single click that just reselects the
            // handle, and no new stop is ever added.
            if response.double_clicked()
                && ramp.stops.len() < MAX_GRADIENT_ENTRIES - 1
                && let Some(pos) = response.interact_pointer_pos()
            {
                pending_insert = Some(nudge_away_from_existing(&ramp.stops, t_at(pos.y)));
            } else {
                if response.secondary_clicked() && ramp.stops.len() > 1 {
                    remove_index = Some(i);
                }
                // Inclusiveness is an OMF boundary property with no natural
                // drag gesture; middle-click flips it in place.
                if response.middle_clicked() && !ramp.interpolate {
                    ramp.stops[i].inclusive = !ramp.stops[i].inclusive;
                    changed = true;
                }
                if response.clicked() {
                    selected = i;
                    value_popup_stop = Some(i);
                    popup_kept_open = true;
                }
                if response.dragged() {
                    let lower = if i == 0 { 0.0 } else { ramp.stops[i - 1].t + STOP_EPSILON };
                    let upper = if i + 1 == ramp.stops.len() { 1.0 } else { ramp.stops[i + 1].t - STOP_EPSILON };
                    let (lo, hi) = (lower.min(upper), lower.max(upper));
                    if let Some(pos) = response.interact_pointer_pos() {
                        let t = t_at(pos.y).clamp(lo, hi);
                        if (ramp.stops[i].t - t).abs() > f32::EPSILON {
                            ramp.stops[i].set_t(t, min, max);
                            changed = true;
                        }
                    }
                    selected = i;
                    if value_popup_stop.is_some() {
                        value_popup_stop = Some(i);
                        popup_kept_open = true;
                    }
                }
            }
            // The value label sits in its own column left of the handle. It
            // reads as plain text until clicked, then becomes an in-place number
            // field clamped between the neighbouring boundaries' values - no
            // detached popup.
            let value_rect = egui::Rect::from_center_size(
                egui::pos2(handle_column_rect.left() + LEGEND_STOP_VALUE_WIDTH * 0.5, y),
                egui::vec2(LEGEND_STOP_VALUE_WIDTH, ui.spacing().interact_size.y.min(18.0)),
            );
            let editing_value = value_popup_stop == Some(i);
            let value_field_hovered;
            if editing_value {
                let gap_t = 1.0e-4_f32;
                let lower_t = if i == 0 { 0.0 } else { ramp.stops[i - 1].t + gap_t };
                let upper_t = if i + 1 == ramp.stops.len() { 1.0 } else { ramp.stops[i + 1].t - gap_t };
                let (lo_t, hi_t) = (lower_t.min(upper_t), lower_t.max(upper_t));
                let (lo_v, hi_v) = (normalized_to_value(lo_t, min, max), normalized_to_value(hi_t, min, max));
                let mut value = ramp.stops[i].value;
                let field = ui
                    .scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(value_rect)
                            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight)),
                        |ui| {
                            // The column is narrow; trim the number box's padding so
                            // a few significant digits fit without clipping.
                            ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
                            ui.add(
                                egui::DragValue::new(&mut value)
                                    .range(lo_v.min(hi_v)..=lo_v.max(hi_v))
                                    .speed(((max - min).abs() / 250.0).max(0.0001))
                                    .max_decimals(6),
                            )
                        },
                    )
                    .inner;
                if field.changed() {
                    ramp.stops[i].set_t(value_to_normalized(value, min, max).clamp(lo_t, hi_t), min, max);
                    changed = true;
                }
                value_field_hovered = field.hovered();
                if !popup_kept_open && (field.lost_focus() || field.clicked_elsewhere()) {
                    value_popup_stop = None;
                } else {
                    selected = i;
                }
            } else {
                let label = ui
                    .interact(value_rect, self.id.with(("color_stop_value_label", ramp.stops[i].id)), egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::Text)
                    .on_hover_text("Click to type this boundary's value");
                value_field_hovered = label.hovered();
                if label.clicked() {
                    selected = i;
                    value_popup_stop = Some(i);
                    popup_kept_open = true;
                }
            }

            let painter = ui.painter();
            let marker_color = color32_from_straight(ramp.stops[i].color);
            let center = egui::pos2(handle_center_x, y);
            let active = selected == i || response.dragged() || response.hovered() || value_field_hovered;
            let radius = if active { 5.5 } else { 4.5 };
            painter.line_segment(
                [egui::pos2(center.x + radius, y), egui::pos2(bar_rect.left(), y)],
                egui::Stroke::new(if active { 1.5 } else { 1.0 }, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
            painter.circle_filled(center, radius, marker_color);
            painter.circle_stroke(
                center,
                radius,
                egui::Stroke::new(if active { 1.5 } else { 1.0 }, if active { egui::Color32::BLACK } else { egui::Color32::from_gray(40) }),
            );
            // Each boundary's value in the variable's own units, so every stop
            // shows what it is without having to be clicked open. Skipped while
            // the in-place field for this stop is showing.
            if !editing_value {
                painter.text(
                    egui::pos2(handle_column_rect.left(), y),
                    egui::Align2::LEFT_CENTER,
                    format_grade(ramp.stops[i].value),
                    egui::FontId::proportional(10.0),
                    if active { text_color } else { ui.visuals().weak_text_color() },
                );
            }
            // An inclusive boundary owns its own value, which changes which
            // band a block exactly on it lands in - worth showing.
            if ramp.stops[i].inclusive {
                painter.text(
                    egui::pos2(handle_rect.left() - 3.0, y),
                    egui::Align2::RIGHT_CENTER,
                    "≤",
                    egui::FontId::proportional(9.0),
                    text_color,
                );
            }
        }

        if let Some(t) = pending_insert
            && ramp.stops.len() < MAX_GRADIENT_ENTRIES - 1
        {
            let new_index = insert_stop_sorted(&mut ramp, editor.allocate_color_stop_id(), t, min, max);
            selected = new_index;
            value_popup_stop = Some(new_index);
            changed = true;
        }

        if let Some(index) = remove_index {
            ramp.stops.remove(index);
            value_popup_stop = match value_popup_stop {
                Some(open_index) if open_index == index => None,
                Some(open_index) if open_index > index => Some(open_index - 1),
                other => other,
            };
            selected = selected.min(ramp.stops.len().saturating_sub(1));
            changed = true;
        }

        if let Some(color) = ramp.stops.get(selected).map(|stop| stop.color) {
            let swatch_y = y_at(ramp.stops[selected].t).clamp(rect.top() + COLOR_PICKER_BUTTON_HEIGHT * 0.5, rect.bottom() - COLOR_PICKER_BUTTON_HEIGHT * 0.5);
            let swatch_rect = egui::Rect::from_center_size(
                egui::pos2(picker_left + COLOR_PICKER_BUTTON_WIDTH * 0.5, swatch_y),
                egui::vec2(COLOR_PICKER_BUTTON_WIDTH, COLOR_PICKER_BUTTON_HEIGHT),
            );
            let mut srgba = straight_to_unmultiplied_srgba(color);
            let response = ui
                .scope_builder(egui::UiBuilder::new().id_salt("color_stop_color_picker").max_rect(swatch_rect), |ui| {
                    ui.spacing_mut().interact_size = egui::vec2(COLOR_PICKER_BUTTON_WIDTH, COLOR_PICKER_BUTTON_HEIGHT);
                    color_edit_button_srgba_unmultiplied(ui, &mut srgba)
                })
                .inner
                .on_hover_text("Click to edit color; right-click to remove");
            let picker_remove_clicked =
                (response.secondary_clicked() || (ui.rect_contains_pointer(swatch_rect) && ui.input(|input| input.pointer.secondary_clicked()))) && ramp.stops.len() > 1;
            if picker_remove_clicked {
                value_popup_stop = None;
                ramp.stops.remove(selected);
                selected = selected.min(ramp.stops.len().saturating_sub(1));
                changed = true;
            }
            if response.changed() {
                if let Some(stop) = ramp.stops.get_mut(selected) {
                    stop.color = unmultiplied_srgba_to_straight(srgba);
                }
                changed = true;
            }
        }

        if changed {
            let selected_value = ramp.stops.get(selected).map(|stop| stop.value);
            let popup_value = value_popup_stop.and_then(|index| ramp.stops.get(index).map(|stop| stop.value));
            let transfer = ramp.to_transfer();
            // The command applies `sanitise`, which may sort and merge; track
            // the selection by value so it follows its stop through that.
            let sanitised = {
                let mut sanitised = transfer.clone();
                sanitised.sanitise(Some((min, max)));
                UiRamp::from_transfer(&sanitised, min, max)
            };
            let nearest = |value: f64| {
                sanitised
                    .stops
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| (a.value - value).abs().total_cmp(&(b.value - value).abs()))
                    .map(|(index, _)| index)
            };
            selected = selected_value.and_then(nearest).unwrap_or(selected);
            value_popup_stop = popup_value.and_then(nearest);
            commands.push(UiCommand::SetBlockModelColorTransfer { id: model.id, transfer });
        }
        ui.data_mut(|data| data.insert_persisted(selected_id, selected));
        ui.data_mut(|data| data.insert_persisted(value_popup_id, value_popup_stop));

        let painter = ui.painter();
        for (t, galley) in labels {
            let y = y_at(t);
            let height = galley.size().y;
            painter.line_segment(
                [egui::pos2(bar_rect.right(), y), egui::pos2(bar_rect.right() + LEGEND_COLUMN_GAP * 0.5, y)],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
            painter.galley(
                egui::pos2(label_left, (y - height * 0.5).clamp(bar_rect.top(), bar_rect.bottom() - height)),
                galley,
                text_color,
            );
        }
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

    pub(crate) fn show(self, ctx: &egui::Context, world_per_point: Option<f64>, viewport_background: [f32; 4]) {
        let Some(world_per_point) = world_per_point.filter(|value| value.is_finite() && *value > 0.0) else {
            return;
        };
        let distance = nice_scale_distance(world_per_point * SCALE_BAR_TARGET_WIDTH);
        let bar_width = (distance / world_per_point) as f32;
        let bar_size = egui::vec2(bar_width + SCALE_BAR_LABEL_OVERHANG * 2.0, SCALE_BAR_HEIGHT);
        // Nothing floats over the viewport's right edge any more, so the bar
        // hangs off its bottom-right corner with nothing to dodge.
        let anchor = egui::pos2(
            self.viewport_rect.right() - SCALE_BAR_VIEWPORT_MARGIN,
            self.viewport_rect.bottom() - SCALE_BAR_VIEWPORT_MARGIN,
        );
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
                let (rect, _) = ui.allocate_exact_size(bar_size, egui::Sense::hover());
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

/// Compact share label for a legend category: `0%`, `<1%`, or a whole percent.
fn format_category_percent(fraction: f32) -> String {
    let percent = fraction * 100.0;
    if fraction <= 0.0 {
        "0%".to_owned()
    } else if percent < 1.0 {
        "<1%".to_owned()
    } else {
        format!("{percent:.0}%")
    }
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
    gizmo_block: egui::Rect,
}

impl ViewportMiniMap {
    pub(crate) fn new(id_source: impl Hash + Debug, viewport_rect: egui::Rect) -> Self {
        Self {
            id: egui::Id::new(id_source),
            viewport_rect,
            gizmo_block: egui::Rect::NOTHING,
        }
    }

    /// Hang the preview below the orientation gizmo, which shares the
    /// viewport's top-right corner with it.
    ///
    /// The preview is painted in the foreground order, above the gizmo, so it
    /// has to keep clear of it itself rather than let paint order hide it.
    /// Pass [`egui::Rect::NOTHING`] while the gizmo is switched off, and the
    /// preview takes the corner.
    pub(crate) fn below_gizmo(mut self, block: egui::Rect) -> Self {
        self.gizmo_block = block;
        self
    }

    pub(crate) fn show(self, ctx: &egui::Context, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
        #[cfg(target_arch = "wasm32")]
        let _ = commands;

        if editor.slice_preview_detached {
            return;
        }

        // The preview is square - a slice is looked at from straight above, so
        // neither axis deserves more room than the other - and it tracks the
        // viewport's shorter side within fixed bounds. It is deliberately not
        // user-resizable: resizing an egui window captures pointer interaction
        // and makes the following middle-drag feel as if the 3D canvas needs
        // to be focused again.
        let top = if self.gizmo_block.is_positive() {
            self.gizmo_block.bottom() + SLICE_PREVIEW_MARGIN
        } else {
            self.viewport_rect.top() + SLICE_PREVIEW_TOP
        };
        let side = (self.viewport_rect.width().min(self.viewport_rect.height()) * 0.24)
            .clamp(SLICE_PREVIEW_MIN_SIZE, SLICE_PREVIEW_MAX_SIZE)
            // Never hang off the bottom of the viewport, however short it is.
            .min(self.viewport_rect.bottom() - SLICE_PREVIEW_MARGIN - top)
            .max(1.0);
        let preview_size = egui::vec2(side, side);
        let preview_pos = egui::pos2(self.viewport_rect.right() - preview_size.x - SLICE_PREVIEW_MARGIN, top);
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
