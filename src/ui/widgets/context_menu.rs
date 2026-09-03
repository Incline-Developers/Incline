//! Compact widgets for persistent right-click menus.
//!
//! These intentionally do not reuse the draggable dialog widgets in
//! [`super::menu`]. Context menus have denser rows, flat actions, a subdued
//! header, and remain anchored to the click position.
//!
//! Every menu in the app is built from these: the viewport's own right-click
//! menu drives [`ContextMenu`] from editor state, widget menus - the explorer
//! tree, the console - go through [`context_menu_popup`], and the menu bar goes
//! through [`MenuBarMenu`]. The latter two are egui's own popups wearing this
//! module's frame, header and rows instead of the default menu style.

use std::{fmt::Debug, hash::Hash};

const MENU_WIDTH: f32 = 220.0;
const HEADER_HEIGHT: f32 = 20.0;
const ROW_HEIGHT: f32 = 20.0;
/// Width of the tick column at the head of a checkable row, which its label is
/// indented past.
const CHECK_COLUMN: f32 = 16.0;
const CONTENT_HORIZONTAL_MARGIN: i8 = 6;
const CONTENT_VERTICAL_MARGIN: i8 = 4;
const CORNER_RADIUS: u8 = 3;
const FIELD_WIDTH: f32 = 108.0;

// Keep the legacy draggable-menu positioning API compiled even though the
// context-menu callers moved here. Other draggable-menu behavior remains
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
            .show(ctx, |ui| menu_frame(ui.style()).show(ui, |ui| draw_body(ui, &title, width, add_contents)).inner)
    }
}

/// Show the same menu as [`ContextMenu`] as a widget's right-click popup.
///
/// Rows anchor to the pointer and close through `ui.close()`, exactly as
/// egui's own [`egui::Response::context_menu`] does - only the frame, header
/// and row metrics come from this module instead of the default menu style.
pub(crate) fn context_menu_popup<R>(
    response: &egui::Response,
    title: impl Into<egui::WidgetText>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let title = title.into();
    egui::Popup::context_menu(response)
        .frame(menu_frame(&response.ctx.style_of(response.ctx.theme())))
        .width(MENU_WIDTH)
        .show(|ui| draw_body(ui, &title, MENU_WIDTH, add_contents))
}

/// Paint the header and run `add_contents` inside the menu's content margins.
fn draw_body<R>(ui: &mut egui::Ui, title: &egui::WidgetText, width: f32, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.set_width(width);
    ui.set_min_width(width);
    ui.set_max_width(width);
    apply_compact_style(ui);
    paint_header(ui, title.text(), width);
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(CONTENT_HORIZONTAL_MARGIN, CONTENT_VERTICAL_MARGIN))
        .show(ui, add_contents)
        .inner
}

/// Paint the subdued title row and its underline.
///
/// Titles carry item names, so the text is truncated to the menu width rather
/// than allowed to spill past the frame.
fn paint_header(ui: &mut egui::Ui, title: &str, width: f32) {
    let visuals = ui.visuals().clone();
    let (header_rect, _) = ui.allocate_exact_size(egui::vec2(width, HEADER_HEIGHT), egui::Sense::hover());
    let margin = f32::from(CONTENT_HORIZONTAL_MARGIN);
    let color = visuals.weak_text_color();
    let mut job = egui::text::LayoutJob::single_section(
        title.to_owned(),
        egui::TextFormat {
            font_id: egui::FontId::proportional(11.0),
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((width - margin * 2.0).max(0.0));
    let galley = ui.painter().layout_job(job);
    let text_pos = egui::pos2(header_rect.left() + margin, header_rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, color);
    ui.painter().line_segment(
        [header_rect.left_bottom(), header_rect.right_bottom()],
        egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );
}

/// Dense rows, small text, and the frame metrics egui builds nested submenu
/// frames from (they are read from the style, not passed in like [`menu_frame`]).
fn apply_compact_style(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    style.spacing.menu_margin = egui::Margin::ZERO;
    style.visuals.menu_corner_radius = egui::CornerRadius::same(CORNER_RADIUS);
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
    checked: Option<bool>,
}

impl ContextMenuAction {
    pub(crate) fn new(label: impl Into<egui::WidgetText>) -> Self {
        Self {
            label: label.into(),
            shortcut: None,
            enabled: true,
            submenu: false,
            checked: None,
        }
    }

    /// Mark this row as a switch rather than a command, drawing a tick at its
    /// head while the setting is on. Rows in the same menu that are not
    /// switches keep their own left edge; only checkable ones are indented.
    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    #[allow(dead_code)]
    pub(crate) fn shortcut(mut self, shortcut: impl Into<egui::WidgetText>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub(crate) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
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
            checked,
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
                let text_left = if checked.is_some() { CHECK_COLUMN } else { 4.0 };
                if checked == Some(true) {
                    // A tick drawn rather than set as text: the row's glyphs
                    // come from the button style, and a checkmark character is
                    // not in every font that style can resolve to.
                    let center = rect.left_center() + egui::vec2(CHECK_COLUMN / 2.0, 0.0);
                    ui.painter().add(egui::Shape::line(
                        vec![center + egui::vec2(-3.0, 0.0), center + egui::vec2(-1.0, 2.5), center + egui::vec2(3.0, -3.0)],
                        egui::Stroke::new(1.4, text_color),
                    ));
                }
                ui.painter().text(
                    rect.left_center() + egui::vec2(text_left, 0.0),
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

/// A menu-bar dropdown wearing the context-menu frame, header and rows.
///
/// The bar button itself stays an ordinary egui button so the menu bar keeps
/// its own layout and hover behaviour; only the popup below it is ours.
///
/// Unused on macOS, where these menus live in the system menu bar.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) struct MenuBarMenu<'a> {
    label: &'a str,
    enabled: bool,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl<'a> MenuBarMenu<'a> {
    pub(crate) fn new(label: &'a str) -> Self {
        Self { label, enabled: true }
    }

    /// A disabled menu shows its bar button greyed out and never opens.
    ///
    /// Unused while every menu in the bar has content.
    #[allow(dead_code)]
    pub(crate) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub(crate) fn show<R>(self, ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> Option<egui::InnerResponse<R>> {
        let Self { label, enabled } = self;
        let response = ui.add_enabled(enabled, egui::Button::new(label));
        if !enabled {
            return None;
        }
        dropdown_menu(&response, label, MENU_WIDTH, add_contents)
    }
}

/// Show this module's menu under a widget the caller drew and sensed itself,
/// so a picker that is not a bar label - the status bar's language button -
/// still opens the same frame, header and rows.
///
/// Mirrors `menu::MenuButton::ui` with a default (non-bar) menu config, so rows
/// and nested submenus behave as they do in egui's own menus; only the frame,
/// width and row metrics are this module's. The popup flips above the button
/// when there is no room below it, which is how the status bar's picker opens.
pub(crate) fn dropdown_menu<R>(
    response: &egui::Response,
    title: impl Into<egui::WidgetText>,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    use egui::containers::menu::MenuConfig;

    let config = MenuConfig::new();
    let title = title.into();
    egui::Popup::menu(response)
        .close_behavior(config.close_behavior)
        .style(config.style.clone())
        .frame(menu_frame(&response.ctx.style_of(response.ctx.theme())))
        .width(width)
        .info(egui::UiStackInfo::new(egui::UiKind::Menu).with_tag_value(MenuConfig::MENU_CONFIG_TAG, config))
        .show(|ui| draw_body(ui, &title, width, add_contents))
}

/// A submenu row inside a [`MenuBarMenu`] or [`context_menu_popup`], opening a
/// nested menu that wears the same frame, header and rows.
///
/// The nested popup is egui's own [`egui::containers::menu::SubMenu`], so hover
/// timing and keyboard navigation behave exactly as elsewhere.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn context_submenu<R>(ui: &mut egui::Ui, label: &str, enabled: bool, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> Option<egui::InnerResponse<R>> {
    let response = ContextMenuAction::new(label).enabled(enabled).submenu(true).show(ui);
    if !enabled {
        return None;
    }
    let title: egui::WidgetText = label.into();
    egui::containers::menu::SubMenu::new().show(ui, &response, |ui| draw_body(ui, &title, MENU_WIDTH, add_contents))
}

/// The shared popup frame: flat fill, hairline border, no inner margin so the
/// header underline reaches both edges.
fn menu_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::new()
        .fill(style.visuals.window_fill())
        .stroke(style.visuals.window_stroke())
        .corner_radius(egui::CornerRadius::same(CORNER_RADIUS))
        .shadow(style.visuals.popup_shadow)
}
