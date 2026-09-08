//! Bounded, spreadsheet-style surfaces used by section layouts.
//!
//! [`DataGrid`] is a titled pane holding a scrollable column of rows with a
//! selector gutter; [`PropertyTable`] is the two-column key/value editor that
//! sits beside it. Both paint their own fill, border and row rules so callers
//! only supply content.

use super::explorer::row_height;
use crate::ui::{fonts::bold, unthemed_icon};

/// Height of the bold section-title strip above a grid's rows.
const TITLE_STRIP: f32 = 38.0;
/// Width of the left selector gutter in a [`DataGrid`] row.
const GUTTER: f32 = 24.0;
/// Fraction of a [`PropertyTable`] row given to the key column.
const KEY_FRACTION: f32 = 0.54;

fn grid_row_height(ui: &egui::Ui) -> f32 {
    row_height(ui) + 3.0
}

/// Paint the recessed fill, title strip and border shared by both surfaces,
/// then run `content` clipped to `rect` with zero vertical row spacing.
fn framed_pane<R>(ui: &mut egui::Ui, id: &str, rect: egui::Rect, title: &str, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let mut out = None;
    ui.scope_builder(egui::UiBuilder::new().id_salt(id).max_rect(rect), |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(rect));
        ui.set_min_size(rect.size());
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), TITLE_STRIP), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            ui.label(bold(title));
        });
        out = Some(content(ui));
        ui.painter().rect_stroke(rect, 0.0, ui.visuals().widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
    });
    out.expect("scope_builder always runs its body")
}

/// One row in a [`DataGrid`].
pub(crate) struct GridRow<'a> {
    label: &'a str,
    selected: bool,
    header: bool,
    error: Option<&'a str>,
}

impl<'a> GridRow<'a> {
    /// A selectable body row.
    pub(crate) fn new(label: &'a str) -> Self {
        Self {
            label,
            selected: false,
            header: false,
            error: None,
        }
    }

    /// The non-interactive column-header row.
    pub(crate) fn header(label: &'a str) -> Self {
        Self {
            label,
            selected: false,
            header: true,
            error: None,
        }
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Show a red error badge at the row's right edge; the message is its tooltip.
    #[allow(dead_code)] // Feature-facing: item rows will carry validation state.
    pub(crate) fn error(mut self, message: Option<&'a str>) -> Self {
        self.error = message;
        self
    }
}

/// Draw a single spreadsheet-style row into the current grid body.
pub(crate) fn grid_row(ui: &mut egui::Ui, row: GridRow<'_>) -> egui::Response {
    let GridRow { label, selected, header, error } = row;
    let height = grid_row_height(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), if header { egui::Sense::hover() } else { egui::Sense::click() });
    let visuals = ui.visuals();
    let fill = if header {
        visuals.widgets.noninteractive.bg_fill
    } else if selected {
        visuals.selection.bg_fill
    } else if response.hovered() {
        visuals.widgets.hovered.bg_fill
    } else {
        visuals.extreme_bg_color
    };
    let stroke = visuals.widgets.noninteractive.bg_stroke;
    let text_color = if selected { visuals.selection.stroke.color } else { visuals.text_color() };
    let gutter = rect.left() + GUTTER;
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().line_segment([egui::pos2(gutter, rect.top()), egui::pos2(gutter, rect.bottom())], stroke);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], stroke);
    if selected && !header {
        let center = egui::pos2(rect.left() + 12.0, rect.center().y);
        ui.painter().add(egui::Shape::convex_polygon(
            vec![center + egui::vec2(-3.0, -4.0), center + egui::vec2(3.0, 0.0), center + egui::vec2(-3.0, 4.0)],
            text_color,
            egui::Stroke::NONE,
        ));
    }
    if let Some(message) = error {
        let icon_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 12.0, rect.center().y), egui::vec2(16.0, 16.0));
        egui::Image::new(unthemed_icon!("step_error.svg")).paint_at(ui, icon_rect);
        ui.interact(icon_rect, response.id.with("error_hint"), egui::Sense::hover()).on_hover_text(message);
    }
    let text = if header { bold(label) } else { egui::RichText::new(label) }.color(text_color);
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (rect.right() - gutter - 16.0 - if error.is_some() { 20.0 } else { 0.0 }).max(0.0),
        egui::TextStyle::Body,
    );
    ui.painter().galley(egui::pos2(gutter + 8.0, rect.center().y - galley.size().y * 0.5), galley, text_color);
    response
}

/// A titled, bordered pane holding a scrollable column of [`grid_row`]s.
pub(crate) struct DataGrid<'a> {
    id: &'a str,
    rect: egui::Rect,
    title: &'a str,
    column_header: Option<&'a str>,
}

impl<'a> DataGrid<'a> {
    pub(crate) fn new(id: &'a str, rect: egui::Rect, title: &'a str) -> Self {
        Self {
            id,
            rect,
            title,
            column_header: None,
        }
    }

    /// Pin a non-interactive header row above the scroll area.
    pub(crate) fn column_header(mut self, text: &'a str) -> Self {
        self.column_header = Some(text);
        self
    }

    /// `body` draws the scrolling rows by calling [`grid_row`]; its return value
    /// is passed back to the caller.
    pub(crate) fn show<R>(self, ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
        framed_pane(ui, self.id, self.rect, self.title, |ui| {
            if let Some(header) = self.column_header {
                grid_row(ui, GridRow::header(header));
            }
            egui::ScrollArea::vertical()
                .id_salt((self.id, "body"))
                .auto_shrink([false; 2])
                .min_scrolled_height(0.0)
                .show(ui, body)
                .inner
        })
    }
}

/// A titled, bordered two-column key/value editor.
pub(crate) struct PropertyTable<'a> {
    id: &'a str,
    rect: egui::Rect,
    title: &'a str,
}

impl<'a> PropertyTable<'a> {
    pub(crate) fn new(id: &'a str, rect: egui::Rect, title: &'a str) -> Self {
        Self { id, rect, title }
    }

    /// `body` populates the table via [`PropertyRows::header`] and
    /// [`PropertyRows::field`].
    pub(crate) fn show(self, ui: &mut egui::Ui, body: impl FnOnce(&mut PropertyRows<'_>)) {
        framed_pane(ui, self.id, self.rect, self.title, |ui| {
            egui::ScrollArea::vertical()
                .id_salt((self.id, "body"))
                .auto_shrink([false; 2])
                .min_scrolled_height(0.0)
                .show(ui, |ui| body(&mut PropertyRows { ui }));
        });
    }
}

/// Row sink handed to a [`PropertyTable`] body closure.
pub(crate) struct PropertyRows<'u> {
    ui: &'u mut egui::Ui,
}

impl PropertyRows<'_> {
    /// The bold "Property / Value" heading row.
    pub(crate) fn header(&mut self, key: &str, value: &str) {
        let (rect, split) = self.begin_row(true);
        self.ui
            .put(self.key_rect(rect, split, false), egui::Label::new(bold(key)).truncate().halign(egui::Align::Min));
        self.ui.put(self.value_rect(rect, split), egui::Label::new(bold(value)).halign(egui::Align::Min));
    }

    /// An editable key/value pair. `error`, when set, shows a red badge in the
    /// key column with the message as its tooltip. Returns the field's response.
    pub(crate) fn field(&mut self, key: &str, value: &mut String, error: Option<&str>) -> egui::Response {
        let (rect, split) = self.begin_row(false);
        self.ui.put(
            self.key_rect(rect, split, error.is_some()),
            egui::Label::new(egui::RichText::new(key)).truncate().halign(egui::Align::Min),
        );
        if let Some(message) = error {
            let icon_rect = egui::Rect::from_center_size(egui::pos2(split - 12.0, rect.center().y), egui::vec2(16.0, 16.0));
            self.ui
                .put(
                    icon_rect,
                    egui::Image::new(unthemed_icon!("step_error.svg"))
                        .fit_to_exact_size(icon_rect.size())
                        .sense(egui::Sense::hover()),
                )
                .on_hover_text(message);
        }
        let value_rect = self.value_rect(rect, split);
        self.ui
            .scope_builder(egui::UiBuilder::new().max_rect(value_rect), |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(value_rect));
                ui.put(
                    value_rect,
                    egui::TextEdit::singleline(value)
                        .vertical_align(egui::Align::Center)
                        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 1)))
                        // egui 0.35 needs a nonzero text atom to anchor the caret
                        // in an empty field. A blank hint supplies it without
                        // inserting placeholder text into the stored value.
                        .hint_text(" ")
                        .desired_width(value_rect.width()),
                )
            })
            .inner
    }

    /// Allocate one row and paint its column split and bottom rule.
    fn begin_row(&mut self, header: bool) -> (egui::Rect, f32) {
        let height = grid_row_height(self.ui);
        let (rect, _) = self.ui.allocate_exact_size(egui::vec2(self.ui.available_width(), height), egui::Sense::hover());
        let split = rect.left() + rect.width() * KEY_FRACTION;
        let stroke = self.ui.visuals().widgets.noninteractive.bg_stroke;
        if header {
            self.ui.painter().rect_filled(rect, 0.0, self.ui.visuals().widgets.noninteractive.bg_fill);
        }
        self.ui.painter().line_segment([egui::pos2(split, rect.top()), egui::pos2(split, rect.bottom())], stroke);
        self.ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], stroke);
        (rect, split)
    }

    fn key_rect(&self, rect: egui::Rect, split: f32, has_error: bool) -> egui::Rect {
        egui::Rect::from_min_max(rect.min + egui::vec2(8.0, 0.0), egui::pos2(split - if has_error { 24.0 } else { 4.0 }, rect.bottom()))
    }

    fn value_rect(&self, rect: egui::Rect, split: f32) -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(split + 4.0, rect.top() + 2.0), rect.max - egui::vec2(4.0, 2.0))
    }
}
