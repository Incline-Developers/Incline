//! Compact widgets for persistent right-click menus.
//!
//! These intentionally do not reuse the draggable dialog widgets in
//! [`super::menu`]. Context menus have denser rows, flat actions, a subdued
//! header, and remain anchored to the click position.

use std::{fmt::Debug, hash::Hash};

const MENU_WIDTH: f32 = 220.0;
const HEADER_HEIGHT: f32 = 20.0;
const ROW_HEIGHT: f32 = 20.0;
const CONTENT_HORIZONTAL_MARGIN: i8 = 6;
const CONTENT_VERTICAL_MARGIN: i8 = 4;
const CORNER_RADIUS: u8 = 3;
const FIELD_WIDTH: f32 = 108.0;

// Keep the legacy draggable-menu positioning API compiled even though the one
// context-menu caller moved here. Other draggable-menu behavior remains
// completely independent from this module.
const _: fn(super::menu::DragableMenu<'static>, egui::Pos2) -> super::menu::DragableMenu<'static> = super::menu::DragableMenu::default_pos;

/// A non-draggable, click-position-anchored context panel.
pub(crate) struct ContextMenu {
    id: egui::Id,
    title: egui::WidgetText,
    position: egui::Pos2,
    width: f32,
}

impl ContextMenu {
    pub(crate) fn new(id_source: impl Hash + Debug, title: impl Into<egui::WidgetText>) -> Self {
        Self {
            id: egui::Id::new(("context_menu", id_source)),
            title: title.into(),
            position: egui::Pos2::ZERO,
            width: MENU_WIDTH,
        }
    }

    pub(crate) fn position(mut self, position: egui::Pos2) -> Self {
        self.position = position;
        self
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = width.max(120.0);
        self
    }

    pub(crate) fn show<R>(self, ctx: &egui::Context, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
        let Self { id, title, position, width } = self;
        egui::Area::new(id)
            .order(egui::Order::Foreground)
            .movable(false)
            .constrain(true)
            .fade_in(false)
            .pivot(egui::Align2::LEFT_TOP)
            .current_pos(position)
            .show(ctx, |ui| {
                let visuals = ui.visuals().clone();
                egui::Frame::new()
                    .fill(visuals.window_fill())
                    .stroke(visuals.window_stroke())
                    .corner_radius(egui::CornerRadius::same(CORNER_RADIUS))
                    .shadow(visuals.popup_shadow)
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.set_min_width(width);
                        ui.set_max_width(width);
                        apply_compact_style(ui);

                        let (header_rect, _) = ui.allocate_exact_size(egui::vec2(width, HEADER_HEIGHT), egui::Sense::hover());
                        ui.painter().text(
                            header_rect.left_center() + egui::vec2(f32::from(CONTENT_HORIZONTAL_MARGIN), 0.0),
                            egui::Align2::LEFT_CENTER,
                            title.text(),
                            egui::FontId::proportional(11.0),
                            visuals.weak_text_color(),
                        );
                        ui.painter().line_segment(
                            [header_rect.left_bottom(), header_rect.right_bottom()],
                            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
                        );

                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(CONTENT_HORIZONTAL_MARGIN, CONTENT_VERTICAL_MARGIN))
                            .show(ui, add_contents)
                            .inner
                    })
                    .inner
            })
    }
}

fn apply_compact_style(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    style.spacing.item_spacing = egui::vec2(4.0, 1.0);
    style.spacing.button_padding = egui::vec2(4.0, 1.0);
    style.spacing.interact_size.y = ROW_HEIGHT;
    style.spacing.combo_width = FIELD_WIDTH;
    style.spacing.text_edit_width = FIELD_WIDTH;
    style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(12.0));
    style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(12.0));
}

/// A full-width, flat command row with optional shortcut and submenu hint.
pub(crate) struct ContextMenuAction {
    label: egui::WidgetText,
    shortcut: Option<egui::WidgetText>,
    enabled: bool,
    submenu: bool,
}

impl ContextMenuAction {
    pub(crate) fn new(label: impl Into<egui::WidgetText>) -> Self {
        Self {
            label: label.into(),
            shortcut: None,
            enabled: true,
            submenu: false,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn shortcut(mut self, shortcut: impl Into<egui::WidgetText>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    #[allow(dead_code)]
    pub(crate) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn submenu(mut self, submenu: bool) -> Self {
        self.submenu = submenu;
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            label,
            shortcut,
            enabled,
            submenu,
        } = self;
        ui.add_enabled_ui(enabled, |ui| {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), egui::Sense::click());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label.text()));
            if ui.is_rect_visible(rect) {
                let visuals = ui.style().interact(&response);
                if response.hovered() || response.has_focus() {
                    ui.painter().rect_filled(rect, 1.0, visuals.bg_fill);
                }
                let text_color = visuals.fg_stroke.color;
                ui.painter().text(
                    rect.left_center() + egui::vec2(4.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    label.text(),
                    egui::TextStyle::Button.resolve(ui.style()),
                    text_color,
                );
                let right_padding = if submenu { 14.0 } else { 4.0 };
                if let Some(shortcut) = shortcut {
                    ui.painter().text(
                        rect.right_center() - egui::vec2(right_padding, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        shortcut.text(),
                        egui::TextStyle::Button.resolve(ui.style()),
                        ui.visuals().weak_text_color(),
                    );
                }
                if submenu {
                    let center = rect.right_center() - egui::vec2(5.0, 0.0);
                    ui.painter().add(egui::Shape::convex_polygon(
                        vec![center + egui::vec2(-2.0, -3.0), center + egui::vec2(2.0, 0.0), center + egui::vec2(-2.0, 3.0)],
                        ui.visuals().weak_text_color(),
                        egui::Stroke::NONE,
                    ));
                }
            }
            response
        })
        .inner
    }
}

/// Add a narrow Blender-style divider between groups of context rows.
pub(crate) fn context_menu_separator(ui: &mut egui::Ui) {
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
    ui.add_space(2.0);
}

/// A compact labelled row for a custom right-aligned control.
pub(crate) struct ContextMenuField {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
}

impl ContextMenuField {
    pub(crate) fn new(label: impl Into<egui::WidgetText>) -> Self {
        Self {
            label: label.into(),
            help_text: None,
        }
    }

    pub(crate) fn help_text(mut self, help_text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(help_text.into());
        self
    }

    pub(crate) fn show<R>(self, ui: &mut egui::Ui, add_field: impl FnOnce(&mut egui::Ui, f32) -> R) -> R {
        context_menu_field_row(ui, self.label, self.help_text, add_field)
    }
}

fn context_menu_field_row<R>(ui: &mut egui::Ui, label: egui::WidgetText, help_text: Option<egui::WidgetText>, add_field: impl FnOnce(&mut egui::Ui, f32) -> R) -> R {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), ROW_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.label(label);
        if let Some(help_text) = help_text {
            draw_help_marker(ui, help_text);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| add_field(ui, ROW_HEIGHT)).inner
    })
    .inner
}

fn draw_help_marker(ui: &mut egui::Ui, help_text: egui::WidgetText) {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(12.0), egui::Sense::hover());
    let color = ui.visuals().weak_text_color();
    ui.painter().circle_stroke(rect.center(), 4.5, egui::Stroke::new(1.0, color));
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "i", egui::FontId::proportional(8.5), color);
    response.on_hover_text(help_text);
}

/// Compact wide color swatch with the standard egui color-picker popup.
pub(crate) struct ContextMenuFieldColor32<'value> {
    label: egui::WidgetText,
    value: &'value mut egui::Color32,
    width: f32,
}

impl<'value> ContextMenuFieldColor32<'value> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut egui::Color32) -> Self {
        Self {
            label: label.into(),
            value,
            width: 44.0,
        }
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self { label, value, width } = self;
        ContextMenuField::new(label).show(ui, |ui, row_height| {
            ui.scope(|ui| {
                ui.spacing_mut().interact_size = egui::vec2(width, row_height - 3.0);
                ui.color_edit_button_srgba(value)
            })
            .inner
        })
    }
}

/// Compact floating-point drag field.
pub(crate) struct ContextMenuFieldF32<'value> {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    value: &'value mut f32,
    range: std::ops::RangeInclusive<f32>,
    width: f32,
    speed: f64,
}

impl<'value> ContextMenuFieldF32<'value> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut f32, range: std::ops::RangeInclusive<f32>) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            value,
            range,
            width: FIELD_WIDTH,
            speed: 0.1,
        }
    }

    pub(crate) fn help_text(mut self, help_text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(help_text.into());
        self
    }

    pub(crate) fn speed(mut self, speed: f64) -> Self {
        self.speed = speed;
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            label,
            help_text,
            value,
            range,
            width,
            speed,
        } = self;
        let field = ContextMenuField::new(label);
        let field = if let Some(help_text) = help_text { field.help_text(help_text) } else { field };
        field.show(ui, |ui, row_height| {
            ui.add_sized([width, row_height], egui::DragValue::new(value).speed(speed).range(range))
        })
    }
}

/// Compact combo-box row.
pub(crate) struct ContextMenuFieldCombo<'value, T> {
    id: egui::Id,
    label: egui::WidgetText,
    value: &'value mut T,
    selected_text: egui::WidgetText,
    options: Vec<(T, egui::WidgetText)>,
    width: f32,
}

impl<'value, T: PartialEq> ContextMenuFieldCombo<'value, T> {
    pub(crate) fn new(
        id_source: impl Hash + Debug,
        label: impl Into<egui::WidgetText>,
        value: &'value mut T,
        selected_text: impl Into<egui::WidgetText>,
        options: impl IntoIterator<Item = (T, egui::WidgetText)>,
    ) -> Self {
        Self {
            id: egui::Id::new(id_source),
            label: label.into(),
            value,
            selected_text: selected_text.into(),
            options: options.into_iter().collect(),
            width: FIELD_WIDTH,
        }
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            id,
            label,
            value,
            selected_text,
            options,
            width,
        } = self;
        ContextMenuField::new(label).show(ui, |ui, _| {
            let mut changed = false;
            let mut popup_style = ui.style().as_ref().clone();
            egui::containers::menu::menu_style(&mut popup_style);
            let mut response = egui::ComboBox::from_id_salt(id)
                .selected_text(selected_text)
                .width(width)
                .truncate()
                .popup_style(popup_style.into())
                .show_ui(ui, |ui| {
                    for (option, text) in options {
                        changed |= ui.selectable_value(value, option, text).changed();
                    }
                })
                .response;
            if changed {
                response.mark_changed();
            }
            response
        })
    }
}

/// Compact mutually-exclusive button group in a labelled field row.
pub(crate) struct ContextMenuFieldSegmented<'value, T> {
    label: egui::WidgetText,
    value: &'value mut T,
    options: Vec<(T, egui::WidgetText)>,
    width: f32,
}

impl<'value, T: Clone + PartialEq> ContextMenuFieldSegmented<'value, T> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut T, options: impl IntoIterator<Item = (T, egui::WidgetText)>) -> Self {
        Self {
            label: label.into(),
            value,
            options: options.into_iter().collect(),
            width: FIELD_WIDTH,
        }
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self { label, value, options, width } = self;
        ContextMenuField::new(label).show(ui, |ui, row_height| {
            let option_count = options.len();
            let gap_width = option_count.saturating_sub(1) as f32;
            let option_width = (width - gap_width).max(0.0) / option_count.max(1) as f32;
            let mut changed = false;
            let inner = ui.allocate_ui_with_layout(egui::vec2(width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let mut combined_response = None;
                if option_count > 0 {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    for (option, text) in options {
                        let selected = *value == option;
                        let response = ui.add_sized(
                            [option_width, row_height],
                            egui::Button::selectable(selected, text).corner_radius(egui::CornerRadius::same(2)),
                        );
                        if response.clicked() && !selected {
                            *value = option.clone();
                            changed = true;
                        }
                        combined_response = Some(match combined_response {
                            Some(combined) => combined | response,
                            None => response,
                        });
                    }
                }
                combined_response
            });
            let mut response = inner.inner.unwrap_or(inner.response);
            if changed {
                response.mark_changed();
            }
            response
        })
    }
}
