use std::{fmt::Debug, hash::Hash, path::PathBuf};

const TITLE_BAR_HEIGHT: f32 = 28.0;
const TITLE_BAR_HORIZONTAL_PADDING: f32 = 8.0;
const CLOSE_BUTTON_SIZE: f32 = 18.0;
const MENU_CORNER_RADIUS: u8 = 3;

/// A non-resizable, non-collapsible floating menu with a draggable title bar.
///
/// This intentionally uses an [`egui::Area`] instead of [`egui::Window`] so
/// Incline controls the frame, title bar, close button, and shadow.
pub(crate) struct DragableMenu<'open> {
    id: egui::Id,
    title: egui::WidgetText,
    title_bar: bool,
    open: Option<&'open mut bool>,
    min_width: f32,
    max_width: f32,
    fixed_size: Option<egui::Vec2>,
    inner_margin: egui::Margin,
    default_pos: Option<egui::Pos2>,
    current_pos: Option<egui::Pos2>,
}

impl<'open> DragableMenu<'open> {
    pub(crate) fn new(title: impl Into<egui::WidgetText>) -> Self {
        let title = title.into().fallback_text_style(egui::TextStyle::Button);
        Self {
            id: egui::Id::new(("dragable_menu", title.text())),
            title,
            title_bar: true,
            open: None,
            min_width: 0.0,
            max_width: 400.0,
            fixed_size: None,
            inner_margin: egui::Margin::symmetric(8, 6),
            default_pos: None,
            current_pos: None,
        }
    }

    /// Show or hide the title bar. A hidden title bar also hides its close button.
    #[allow(dead_code)]
    pub(crate) fn title_bar(mut self, title_bar: bool) -> Self {
        self.title_bar = title_bar;
        self
    }

    pub(crate) fn open(mut self, open: &'open mut bool) -> Self {
        self.open = Some(open);
        self
    }

    pub(crate) fn min_width(mut self, min_width: f32) -> Self {
        self.min_width = min_width;
        self
    }

    pub(crate) fn max_width(mut self, max_width: f32) -> Self {
        self.max_width = max_width;
        self
    }

    /// Lock the menu to a fixed size.
    pub(crate) fn fixed_size(mut self, fixed_size: egui::Vec2) -> Self {
        self.fixed_size = Some(fixed_size);
        self
    }

    /// Override the content padding for dialogs that need more visual
    /// separation than the compact tool-window default.
    pub(crate) fn inner_margin(mut self, inner_margin: egui::Margin) -> Self {
        self.inner_margin = inner_margin;
        self
    }

    pub(crate) fn default_pos(mut self, default_pos: egui::Pos2) -> Self {
        self.default_pos = Some(default_pos);
        self
    }

    pub(crate) fn show<R>(self, ctx: &egui::Context, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> Option<egui::InnerResponse<R>> {
        let Self {
            id,
            title,
            title_bar,
            open,
            min_width,
            max_width,
            fixed_size,
            inner_margin,
            default_pos,
            current_pos,
        } = self;

        if open.as_deref().is_some_and(|open| !open) {
            return None;
        }

        let mut area = egui::Area::new(id).order(egui::Order::Foreground).movable(true).constrain(true).fade_in(false);
        area = if let Some(current_pos) = current_pos {
            area.pivot(egui::Align2::LEFT_TOP).current_pos(current_pos)
        } else if let Some(default_pos) = default_pos {
            area.pivot(egui::Align2::LEFT_TOP).default_pos(default_pos)
        } else {
            area.pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center())
        };
        if let Some(fixed_size) = fixed_size {
            area = area.default_size(fixed_size);
        }
        let max_width = max_width.max(min_width);
        let show_close_button = open.is_some();
        let mut close_clicked = false;

        let response = area.show(ctx, |ui| {
            let visuals = ui.visuals().clone();
            egui::Frame::new()
                .fill(visuals.window_fill())
                .stroke(visuals.window_stroke())
                .corner_radius(egui::CornerRadius::same(MENU_CORNER_RADIUS))
                .shadow(egui::epaint::Shadow::NONE)
                .show(ui, |ui| {
                    if let Some(fixed_size) = fixed_size {
                        ui.set_min_size(fixed_size);
                        ui.set_max_size(fixed_size);
                    } else {
                        ui.set_min_width(min_width);
                        ui.set_max_width(max_width);
                    }

                    let title_rect = title_bar.then(|| {
                        let font_id = egui::TextStyle::Button.resolve(ui.style());
                        let title_width = ui.painter().layout_no_wrap(title.text().to_owned(), font_id, ui.visuals().text_color()).size().x;
                        let close_slot_width = if show_close_button { TITLE_BAR_HEIGHT } else { TITLE_BAR_HORIZONTAL_PADDING };
                        let minimum_width = title_width + TITLE_BAR_HORIZONTAL_PADDING + close_slot_width;
                        let width = fixed_size.map_or(min_width.max(minimum_width), |size| size.x);
                        ui.allocate_exact_size(egui::vec2(width, TITLE_BAR_HEIGHT), egui::Sense::hover()).0
                    });

                    let inner = egui::Frame::NONE
                        .inner_margin(inner_margin)
                        .show(ui, |ui| {
                            if let Some(fixed_size) = fixed_size {
                                let body_height = fixed_size.y - if title_bar { TITLE_BAR_HEIGHT } else { 0.0 } - inner_margin.sum().y;
                                ui.set_min_size(egui::vec2(fixed_size.x - inner_margin.sum().x, body_height.max(0.0)));
                                ui.set_max_size(egui::vec2(fixed_size.x - inner_margin.sum().x, body_height.max(0.0)));
                            }
                            add_contents(ui)
                        })
                        .inner;

                    if let Some(mut title_rect) = title_rect {
                        title_rect.max.x = ui.min_rect().right();
                        draw_menu_title_bar(ui, title, title_rect, show_close_button, &mut close_clicked);
                    }

                    inner
                })
                .inner
        });

        if close_clicked && let Some(open) = open {
            *open = false;
        }

        Some(response)
    }
}

fn draw_menu_title_bar(ui: &mut egui::Ui, title: egui::WidgetText, rect: egui::Rect, show_close_button: bool, close_clicked: &mut bool) {
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius {
            nw: MENU_CORNER_RADIUS,
            ne: MENU_CORNER_RADIUS,
            sw: 0,
            se: 0,
        },
        ui.visuals().widgets.inactive.bg_fill,
    );
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );

    let close_slot_width = if show_close_button { TITLE_BAR_HEIGHT } else { TITLE_BAR_HORIZONTAL_PADDING };
    let title_rect = egui::Rect::from_min_max(
        rect.min + egui::vec2(TITLE_BAR_HORIZONTAL_PADDING, 0.0),
        egui::pos2(rect.right() - close_slot_width, rect.bottom()),
    );
    ui.painter().text(
        title_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        title.text(),
        egui::TextStyle::Button.resolve(ui.style()),
        ui.visuals().text_color(),
    );

    if show_close_button {
        let close_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - TITLE_BAR_HEIGHT / 2.0, rect.center().y), egui::Vec2::splat(CLOSE_BUTTON_SIZE));
        let response = ui.interact(close_rect, ui.id().with("close"), egui::Sense::click());
        let stroke = ui.style().interact(&response).fg_stroke;
        let icon_rect = close_rect.shrink(4.0);
        ui.painter().line_segment([icon_rect.left_top(), icon_rect.right_bottom()], stroke);
        ui.painter().line_segment([icon_rect.right_top(), icon_rect.left_bottom()], stroke);
        if response.clicked() {
            *close_clicked = true;
        }
    }
}

const MENU_FIELD_WIDTH: f32 = 120.0;

/// A labelled menu row for controls that do not fit one of the standard field types.
pub(crate) struct MenuField {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
}

impl MenuField {
    pub(crate) fn new(label: impl Into<egui::WidgetText>) -> Self {
        Self {
            label: label.into(),
            help_text: None,
        }
    }

    /// Add a small discoverable info marker beside the field label.
    pub(crate) fn help_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(text.into());
        self
    }

    pub(crate) fn show<R>(self, ui: &mut egui::Ui, add_field: impl FnOnce(&mut egui::Ui, f32) -> R) -> R {
        menu_field_row(ui, self.label, self.help_text, add_field)
    }
}

/// A labelled file-picker row showing the selected file count/name and a choose button.
pub(crate) struct MenuFieldFilePicker<'paths> {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    paths: &'paths [PathBuf],
    empty_text: egui::WidgetText,
    button_text: egui::WidgetText,
    width: f32,
}

impl<'paths> MenuFieldFilePicker<'paths> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, paths: &'paths [PathBuf]) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            paths,
            empty_text: "No file chosen".into(),
            button_text: "Choose...".into(),
            width: MENU_FIELD_WIDTH,
        }
    }

    pub(crate) fn empty_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.empty_text = text.into();
        self
    }

    pub(crate) fn help_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(text.into());
        self
    }

    pub(crate) fn button_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.button_text = text.into();
        self
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            label,
            help_text,
            paths,
            empty_text,
            button_text,
            width,
        } = self;
        let text = selected_file_label(paths, empty_text);
        menu_field_row(ui, label, help_text, |ui, row_height| {
            let inner = ui.horizontal(|ui| {
                let clicked = ui.add_sized([row_height * 3.8, row_height], egui::Button::new(button_text)).clicked();
                ui.add_sized([width, row_height], egui::Label::new(text).truncate());
                clicked
            });
            let mut response = inner.response;
            if inner.inner {
                response.mark_changed();
            }
            response
        })
    }
}

fn selected_file_label(paths: &[PathBuf], empty_text: egui::WidgetText) -> egui::WidgetText {
    match paths {
        [] => empty_text,
        [path] => path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
            .into(),
        paths => format!("{} files selected", paths.len()).into(),
    }
}

fn menu_field_row<R>(ui: &mut egui::Ui, label: egui::WidgetText, help_text: Option<egui::WidgetText>, add_field: impl FnOnce(&mut egui::Ui, f32) -> R) -> R {
    let row_height = ui.spacing().interact_size.y;
    let row_width = ui.available_width();

    ui.allocate_ui_with_layout(egui::vec2(row_width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        menu_field_label(ui, label, help_text);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| add_field(ui, row_height)).inner
    })
    .inner
}

pub(crate) fn menu_field_label(ui: &mut egui::Ui, label: egui::WidgetText, help_text: Option<egui::WidgetText>) {
    ui.label(label);
    if let Some(help_text) = help_text {
        let desired_size = egui::vec2(14.0, 14.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let color = ui.visuals().weak_text_color();
            ui.painter().circle_stroke(rect.center(), 5.5, egui::Stroke::new(1.0, color));
            ui.painter().text(
                rect.center() + egui::vec2(0.0, -0.25),
                egui::Align2::CENTER_CENTER,
                "i",
                egui::FontId::proportional(10.0),
                color,
            );
        }
        response
            //.on_hover_cursor(egui::CursorIcon::Help)
            .on_hover_text(help_text);
    }
}

macro_rules! menu_field_float {
    ($name:ident, $value_type:ty) => {
        pub(crate) struct $name<'value> {
            label: egui::WidgetText,
            help_text: Option<egui::WidgetText>,
            value: &'value mut $value_type,
            range: std::ops::RangeInclusive<$value_type>,
            width: f32,
            speed: f64,
            suffix: String,
            max_decimals: usize,
        }

        #[allow(dead_code)]
        impl<'value> $name<'value> {
            pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut $value_type, range: std::ops::RangeInclusive<$value_type>) -> Self {
                Self {
                    label: label.into(),
                    help_text: None,
                    value,
                    range,
                    width: MENU_FIELD_WIDTH,
                    speed: 0.1,
                    suffix: String::new(),
                    max_decimals: 2,
                }
            }

            pub(crate) fn width(mut self, width: f32) -> Self {
                self.width = width;
                self
            }

            pub(crate) fn help_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
                self.help_text = Some(text.into());
                self
            }

            pub(crate) fn speed(mut self, speed: f64) -> Self {
                self.speed = speed;
                self
            }

            pub(crate) fn suffix(mut self, suffix: impl Into<String>) -> Self {
                self.suffix = suffix.into();
                self
            }

            pub(crate) fn max_decimals(mut self, max_decimals: usize) -> Self {
                self.max_decimals = max_decimals;
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
                    suffix,
                    max_decimals,
                } = self;
                menu_field_row(ui, label, help_text, |ui, row_height| {
                    ui.add_sized(
                        [width, row_height],
                        egui::DragValue::new(value).speed(speed).range(range).suffix(suffix).max_decimals(max_decimals),
                    )
                })
            }

            pub(crate) fn show_inline(self, ui: &mut egui::Ui) -> egui::Response {
                let Self {
                    label,
                    help_text,
                    value,
                    range,
                    width,
                    speed,
                    suffix,
                    max_decimals,
                } = self;
                let row_height = ui.spacing().interact_size.y;
                menu_field_label(ui, label, help_text);
                ui.add_sized(
                    [width, row_height],
                    egui::DragValue::new(value).speed(speed).range(range).suffix(suffix).max_decimals(max_decimals),
                )
            }
        }
    };
}

menu_field_float!(MenuFieldF32, f32);
menu_field_float!(MenuFieldF64, f64);

/// A consistently aligned sliding boolean toggle field.
pub(crate) struct MenuFieldBool<'value> {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    value: &'value mut bool,
}

impl<'value> MenuFieldBool<'value> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut bool) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            value,
        }
    }

    pub(crate) fn help_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(text.into());
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self { label, help_text, value } = self;
        menu_field_row(ui, label, help_text, |ui, row_height| {
            ui.add_sized([row_height * 1.8, row_height], SlidingToggle::new(value))
        })
    }
}

struct SlidingToggle<'value> {
    value: &'value mut bool,
}

impl<'value> SlidingToggle<'value> {
    fn new(value: &'value mut bool) -> Self {
        Self { value }
    }
}

impl egui::Widget for SlidingToggle<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let desired_size = ui.available_size_before_wrap();
        let height = desired_size.y.max(ui.spacing().interact_size.y);
        let width = desired_size.x.max(height * 1.8);
        let (rect, mut response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());

        if response.clicked() {
            *self.value = !*self.value;
            response.mark_changed();
        }

        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *self.value, ""));

        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact_selectable(&response, *self.value);
            let animation = ui.ctx().animate_bool_responsive(response.id.with("sliding_toggle"), *self.value);
            let radius = rect.height() / 2.0;
            let track_rect = rect.shrink2(egui::vec2(1.0, 3.0));
            let off_fill = ui.visuals().widgets.inactive.bg_fill;
            let on_fill = ui.visuals().selection.bg_fill;
            let track_fill = if *self.value { on_fill } else { off_fill };
            let knob_radius = (track_rect.height() / 2.0 - 2.0).max(2.0);
            let knob_x = egui::lerp((track_rect.left() + radius)..=(track_rect.right() - radius), animation);
            let knob_center = egui::pos2(knob_x, track_rect.center().y);

            ui.painter().rect(track_rect, radius, track_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
            ui.painter().circle_filled(knob_center, knob_radius, visuals.fg_stroke.color);
        }

        response
    }
}

pub(crate) struct MenuFieldU32<'value> {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    value: &'value mut u32,
    range: std::ops::RangeInclusive<u32>,
    width: f32,
    speed: f32,
    suffix: String,
}

#[allow(dead_code)]
impl<'value> MenuFieldU32<'value> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut u32, range: std::ops::RangeInclusive<u32>) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            value,
            range,
            width: MENU_FIELD_WIDTH,
            speed: 1.,
            suffix: String::new(),
        }
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub(crate) fn help_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(text.into());
        self
    }

    pub(crate) fn speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }

    pub(crate) fn suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = suffix.into();
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
            suffix,
        } = self;
        menu_field_row(ui, label, help_text, |ui, row_height| {
            ui.add_sized([width, row_height], egui::DragValue::new(value).speed(speed).range(range).suffix(suffix))
        })
    }
}

/// A consistently aligned single-line text field for draggable and docked menus.
pub(crate) struct MenuFieldText<'value> {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    value: &'value mut String,
    width: f32,
    hint: egui::WidgetText,
}

impl<'value> MenuFieldText<'value> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut String) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            value,
            width: MENU_FIELD_WIDTH,
            hint: egui::WidgetText::default(),
        }
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub(crate) fn help_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(text.into());
        self
    }

    pub(crate) fn hint_text(mut self, hint: impl Into<egui::WidgetText>) -> Self {
        self.hint = hint.into();
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            label,
            help_text,
            value,
            width,
            hint,
        } = self;
        menu_field_row(ui, label, help_text, |ui, row_height| {
            ui.add_sized([width, row_height], egui::TextEdit::singleline(value).hint_text(hint))
        })
    }
}

/// A consistently aligned sRGBA colour field for draggable and docked menus.
pub(crate) struct MenuFieldColor32<'value> {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    value: &'value mut egui::Color32,
}

impl<'value> MenuFieldColor32<'value> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut egui::Color32) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            value,
        }
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self { label, help_text, value } = self;
        menu_field_row(ui, label, help_text, |ui, _| ui.color_edit_button_srgba(value))
    }
}

/// A consistently aligned premultiplied RGBA colour field.
pub(crate) struct MenuFieldRgba<'value> {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    value: &'value mut [f32; 4],
}

impl<'value> MenuFieldRgba<'value> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut [f32; 4]) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            value,
        }
    }

    pub(crate) fn help_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(text.into());
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self { label, help_text, value } = self;
        menu_field_row(ui, label, help_text, |ui, _| ui.color_edit_button_rgba_premultiplied(value))
    }
}

/// A consistently aligned combo-box field.
pub(crate) struct MenuFieldCombo<'value, T> {
    id: egui::Id,
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    value: &'value mut T,
    selected_text: egui::WidgetText,
    options: Vec<(T, egui::WidgetText)>,
    width: f32,
}

impl<'value, T: PartialEq> MenuFieldCombo<'value, T> {
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
            help_text: None,
            value,
            selected_text: selected_text.into(),
            options: options.into_iter().collect(),
            width: MENU_FIELD_WIDTH,
        }
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub(crate) fn help_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.help_text = Some(text.into());
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            id,
            label,
            help_text,
            value,
            selected_text,
            options,
            width,
        } = self;
        menu_field_row(ui, label, help_text, |ui, _| {
            let selected_tooltip = selected_text.text().to_owned();
            let mut selection_changed = false;
            let mut response = egui::ComboBox::from_id_salt(id)
                .selected_text(selected_text)
                .width(width)
                .truncate()
                .show_ui(ui, |ui| {
                    for (option, text) in options {
                        selection_changed |= ui.selectable_value(value, option, text).changed();
                    }
                })
                .response
                .on_hover_text(selected_tooltip);
            if selection_changed {
                response.mark_changed();
            }
            response
        })
    }
}

/// Used in preferences to select a category
pub(crate) struct PreferenceCategory {
    label: String,
    active: bool,
}

impl PreferenceCategory {
    pub(crate) fn new(label: String) -> Self {
        Self { label, active: false }
    }

    pub(crate) fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let Self { label, active, .. } = self;
        let visuals = ui.visuals();
        let fill = if active { crate::ui::SELECTION_COLOR } else { visuals.code_bg_color };
        ui.add(
            egui::Button::new(crate::ui::fonts::bold(&label).color(visuals.strong_text_color()))
                .fill(fill)
                .min_size((170., 30.).into()),
        )
    }
}

/// Standard dialog keyboard shortcuts.
///
/// Enter confirms the dialog's primary action and Escape cancels it. Both keys
/// are *consumed* from egui's input, so only the first dialog that asks for
/// them in a frame reacts, and the viewport tools never see the same press
/// (`App::handle_key_code` skips them while a modal dialog is open).
pub(crate) fn dialog_confirm_pressed(ctx: &egui::Context) -> bool {
    ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
}

/// Escape half of [`dialog_confirm_pressed`].
pub(crate) fn dialog_cancel_pressed(ctx: &egui::Context) -> bool {
    ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
}
