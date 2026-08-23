use std::{fmt::Debug, hash::Hash, path::PathBuf};

use super::shifted;

/// Height of a floating menu's drag bar, and of a docked panel's heading.
pub(crate) const TITLE_BAR_HEIGHT: f32 = 30.0;
const TITLE_BAR_HORIZONTAL_PADDING: f32 = 10.0;
/// Side of the square the close cross is drawn inside.
const CLOSE_BUTTON_SIZE: f32 = 20.0;
/// Rounding of a floating menu's card, and of every control inside it -
/// buttons, entry boxes, combo boxes.
///
/// The toolbar tiles' radius, which the panels in [`crate::ui::chrome`] also
/// take, so every rounded surface in the window is one family.
pub(crate) const MENU_CORNER_RADIUS: u8 = super::toolbar::GROUP_CORNER_RADIUS;
const CONTROL_CORNER_RADIUS: u8 = MENU_CORNER_RADIUS;
/// Track of the sliding toggle a boolean field shows. Deliberately smaller
/// than the row it sits in: it is a switch, not a button.
const TOGGLE_WIDTH: f32 = 32.0;
const TOGGLE_HEIGHT: f32 = 16.0;
/// Height of a field row, and so of the control sitting in it.
const ROW_HEIGHT: f32 = 24.0;
/// Height of a [`MenuButton`]. The same as a field row, and the same as the
/// height [`apply_menu_style`] gives a stock [`egui::Button`], so a dialog
/// that has not been ported to [`MenuButton`] still lines up with one that
/// has.
const BUTTON_HEIGHT: f32 = ROW_HEIGHT;
/// Least width of a [`MenuButton`], so a row of short labels reads as a set.
const BUTTON_MIN_WIDTH: f32 = 72.0;
/// Space either side of a button's label.
const BUTTON_HORIZONTAL_PADDING: f32 = 12.0;

/// Surface a floating menu paints itself with: one step brighter than the
/// panels it floats over, so the card lifts off them without a shadow (the
/// interface is deliberately flat - see `theme_visuals`).
pub(crate) fn menu_surface(visuals: &egui::Visuals) -> egui::Color32 {
    shifted(visuals.panel_fill, if visuals.dark_mode { 8 } else { 7 })
}

/// Hairline around a floating menu's card.
///
/// The card's only elevation cue, so it is a firmer line than the one
/// [`crate::ui::chrome`] draws around a region.
pub(crate) fn menu_border(visuals: &egui::Visuals) -> egui::Stroke {
    egui::Stroke::new(1.0, shifted(visuals.panel_fill, if visuals.dark_mode { 34 } else { -40 }))
}

/// Tint of the drag bar: a hint of separation from the body, no divider line.
fn title_bar_fill(surface: egui::Color32, dark_mode: bool) -> egui::Color32 {
    shifted(surface, if dark_mode { 7 } else { -6 })
}

/// Style every control in a menu shares, applied to `ui` and its children.
///
/// Menu-family widgets are used both in floating menus and in the docked
/// properties panel, so the styling travels with the widgets rather than with
/// the window: pass the surface the controls are sitting on and they step away
/// from it by the same amount in either place, and in either theme.
///
/// Controls are recessed rather than raised - a filled entry box on a flat
/// surface - and none of them grow on hover: at this density a widget that
/// expands nudges its neighbours' text.
pub(crate) fn apply_menu_style(ui: &mut egui::Ui, surface: egui::Color32) {
    let dark = ui.visuals().dark_mode;
    let step = |levels: i16| shifted(surface, if dark { levels } else { -levels });
    let style = ui.style_mut();

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.interact_size.y = ROW_HEIGHT;
    // Matches a `MenuButton`'s padding, so a stock button beside one is the
    // same size for the same label.
    style.spacing.button_padding = egui::vec2(BUTTON_HORIZONTAL_PADDING, 4.0);
    style.spacing.icon_width = 16.0;
    style.spacing.icon_width_inner = 9.0;
    style.spacing.icon_spacing = 6.0;
    style.spacing.slider_rail_height = 4.0;
    // Fill the rail behind the handle with the accent, so a slider says where
    // it sits without needing its number beside it.
    style.visuals.slider_trailing_fill = true;

    let visuals = &mut style.visuals;
    // Entry boxes and other sunken controls. A text box is the one control
    // that has to read as a hole in the card even on a white surface, so it
    // sinks further than the rest.
    visuals.extreme_bg_color = step(-8);
    visuals.text_edit_bg_color = Some(step(-16));
    visuals.faint_bg_color = step(6);

    let widgets = &mut visuals.widgets;
    for widget in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(CONTROL_CORNER_RADIUS);
        widget.expansion = 0.0;
    }

    // Label text, and with it the weak text derived from it: a step crisper
    // than egui's default grey, which reads as disabled on a card this size.
    widgets.noninteractive.fg_stroke.color = if dark { egui::Color32::from_gray(210) } else { egui::Color32::from_gray(40) };
    widgets.inactive.fg_stroke.color = if dark { egui::Color32::from_gray(205) } else { egui::Color32::from_gray(45) };
    widgets.noninteractive.bg_fill = step(10);
    widgets.noninteractive.weak_bg_fill = step(10);
    widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, step(18));

    // Buttons, combo boxes and drag values at rest: filled, unstroked.
    widgets.inactive.bg_fill = step(16);
    widgets.inactive.weak_bg_fill = step(16);
    widgets.inactive.bg_stroke = egui::Stroke::NONE;

    widgets.hovered.bg_fill = step(26);
    widgets.hovered.weak_bg_fill = step(26);
    widgets.hovered.bg_stroke = egui::Stroke::new(1.0, step(38));

    widgets.active.bg_fill = step(34);
    widgets.active.weak_bg_fill = step(34);
    widgets.active.bg_stroke = egui::Stroke::new(1.0, step(46));

    widgets.open.bg_fill = step(26);
    widgets.open.weak_bg_fill = step(26);
    widgets.open.bg_stroke = egui::Stroke::new(1.0, step(38));
}

/// Paint a docked panel's heading: the drag bar's look without its close
/// button, so a panel pinned in the viewport reads as the same family of card.
pub(crate) fn draw_menu_heading(ui: &mut egui::Ui, title: &egui::WidgetText, rect: egui::Rect, surface: egui::Color32) {
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius {
            nw: MENU_CORNER_RADIUS,
            ne: MENU_CORNER_RADIUS,
            sw: 0,
            se: 0,
        },
        title_bar_fill(surface, ui.visuals().dark_mode),
    );
    paint_title_text(ui, title, rect, false);
}

/// Lay a card's title into its bar, truncated to the room left beside the
/// close button.
///
/// Laid out as bold rather than taking the text's own style: the title is the
/// card's heading, and every caller passes plain text for it.
fn paint_title_text(ui: &mut egui::Ui, title: &egui::WidgetText, rect: egui::Rect, show_close_button: bool) {
    let close_slot_width = if show_close_button { TITLE_BAR_HEIGHT } else { TITLE_BAR_HORIZONTAL_PADDING };
    let title_width = (rect.width() - TITLE_BAR_HORIZONTAL_PADDING - close_slot_width).max(0.0);
    let mut job = egui::text::LayoutJob::single_section(
        title.text().to_owned(),
        egui::TextFormat {
            font_id: egui::FontId::new(13.0, egui::FontFamily::Name("open_sans_bold".into())),
            color: ui.visuals().text_color(),
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(title_width);
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        egui::pos2(rect.left() + TITLE_BAR_HORIZONTAL_PADDING, rect.center().y - galley.size().y / 2.0),
        galley,
        ui.visuals().text_color(),
    );
}

/// The accent a primary action is filled with.
///
/// The selection blue, darkened in the light theme so white text on it stays
/// legible against a bright surface.
fn accent_fill(visuals: &egui::Visuals) -> egui::Color32 {
    let accent = visuals.selection.stroke.color;
    if visuals.dark_mode { accent } else { shifted(accent, -45) }
}

/// The fill a destructive action takes, in place of [`accent_fill`].
fn danger_fill(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_rgb(0xC4, 0x4B, 0x4B)
    } else {
        egui::Color32::from_rgb(0xB4, 0x3A, 0x3A)
    }
}

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
    #[allow(dead_code)]
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
            let surface = menu_surface(ui.visuals());
            egui::Frame::new()
                .fill(surface)
                .stroke(menu_border(ui.visuals()))
                .corner_radius(egui::CornerRadius::same(MENU_CORNER_RADIUS))
                .shadow(egui::epaint::Shadow::NONE)
                .show(ui, |ui| {
                    apply_menu_style(ui, surface);
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
                        draw_menu_title_bar(ui, title, title_rect, surface, show_close_button, &mut close_clicked);
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

/// Paint a menu's drag bar: title, a tint separating it from the body, and
/// the close cross when the menu is closable.
///
/// No divider under it. The whole card is the drag target, so the bar does not
/// have to announce itself as one; the tint alone sets the title apart.
fn draw_menu_title_bar(ui: &mut egui::Ui, title: egui::WidgetText, rect: egui::Rect, surface: egui::Color32, show_close_button: bool, close_clicked: &mut bool) {
    let dark_mode = ui.visuals().dark_mode;
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius {
            nw: MENU_CORNER_RADIUS,
            ne: MENU_CORNER_RADIUS,
            sw: 0,
            se: 0,
        },
        title_bar_fill(surface, dark_mode),
    );

    paint_title_text(ui, &title, rect, show_close_button);

    if show_close_button {
        let close_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - TITLE_BAR_HEIGHT / 2.0, rect.center().y), egui::Vec2::splat(CLOSE_BUTTON_SIZE));
        let response = ui.interact(close_rect, ui.id().with("close"), egui::Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(close_rect, CONTROL_CORNER_RADIUS, shifted(surface, if dark_mode { 26 } else { -26 }));
        }
        let color = if response.hovered() {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        let stroke = egui::Stroke::new(1.3, color);
        let icon_rect = close_rect.shrink(6.0);
        ui.painter().line_segment([icon_rect.left_top(), icon_rect.right_bottom()], stroke);
        ui.painter().line_segment([icon_rect.right_top(), icon_rect.left_bottom()], stroke);
        if response.clicked() {
            *close_clicked = true;
        }
    }
}

/// How much weight a [`MenuButton`] carries in its row.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum MenuButtonKind {
    /// The action the dialog exists to perform. Filled with the accent; one
    /// per row, or the row has no primary at all.
    Primary,
    /// Everything else: cancel, secondary choices, tool actions.
    #[default]
    Secondary,
    /// A primary action that destroys work. Filled red in place of the accent.
    Danger,
}

/// The action button used across floating menus and docked panels.
///
/// Flat, filled, and sized as a set: every button is [`BUTTON_HEIGHT`] tall
/// and at least [`BUTTON_MIN_WIDTH`] wide, so a row of them lines up whatever
/// the labels say. Add them through [`menu_actions`] to get the row itself
/// right.
pub(crate) struct MenuButton {
    text: egui::WidgetText,
    kind: MenuButtonKind,
    enabled: bool,
    min_width: f32,
    selected: bool,
}

impl MenuButton {
    pub(crate) fn new(text: impl Into<egui::WidgetText>) -> Self {
        Self {
            text: text.into(),
            kind: MenuButtonKind::Secondary,
            enabled: true,
            min_width: BUTTON_MIN_WIDTH,
            selected: false,
        }
    }

    /// The dialog's confirming action.
    pub(crate) fn primary(mut self) -> Self {
        self.kind = MenuButtonKind::Primary;
        self
    }

    /// A confirming action that destroys work.
    pub(crate) fn danger(mut self) -> Self {
        self.kind = MenuButtonKind::Danger;
        self
    }

    pub(crate) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Draw the button as a held-down choice, for the button pairs that stand
    /// in for a two-value radio.
    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub(crate) fn min_width(mut self, min_width: f32) -> Self {
        self.min_width = min_width;
        self
    }
}

impl egui::Widget for MenuButton {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            text,
            kind,
            enabled,
            min_width,
            selected,
        } = self;
        let galley = text.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
        // The row may be narrower than the button would like to be; a button
        // that overflows its row pushes the rest of the set off the card.
        let width = (galley.size().x + BUTTON_HORIZONTAL_PADDING * 2.0).max(min_width).min(ui.available_width().max(1.0));
        // A disabled button senses hover only, so it can never report a click.
        let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, BUTTON_HEIGHT), sense);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, galley.text()));
        let response = if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response };

        if ui.is_rect_visible(rect) {
            let visuals = ui.visuals();
            let interaction = if !enabled {
                None
            } else if response.is_pointer_button_down_on() {
                Some(2)
            } else if response.hovered() {
                Some(1)
            } else {
                Some(0)
            };
            let accented = matches!(kind, MenuButtonKind::Primary | MenuButtonKind::Danger) || selected;
            let (fill, text_color) = if accented {
                let base = match kind {
                    MenuButtonKind::Danger => danger_fill(visuals),
                    _ => accent_fill(visuals),
                };
                let fill = match interaction {
                    Some(1) => shifted(base, 16),
                    Some(2) => shifted(base, -16),
                    _ => base,
                };
                (fill, egui::Color32::WHITE)
            } else {
                let widget = match interaction {
                    Some(1) => &visuals.widgets.hovered,
                    Some(2) => &visuals.widgets.active,
                    _ => &visuals.widgets.inactive,
                };
                (widget.bg_fill, widget.fg_stroke.color)
            };
            let (fill, text_color) = if enabled {
                (fill, text_color)
            } else {
                (fill.gamma_multiply(0.4), text_color.gamma_multiply(0.4))
            };

            ui.painter().rect_filled(rect, CONTROL_CORNER_RADIUS, fill);
            let text_pos = rect.center() - galley.size() / 2.0;
            ui.painter().galley(text_pos, galley, text_color);
        }

        response
    }
}

/// Lay out a menu's action buttons: a right-aligned row below the content.
///
/// Buttons are added right to left, so the primary action goes in first and
/// lands on the right where the eye leaves the dialog.
pub(crate) fn menu_actions<R>(ui: &mut egui::Ui, add_buttons: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(6.0);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), BUTTON_HEIGHT),
        egui::Layout::right_to_left(egui::Align::Center),
        add_buttons,
    )
    .inner
}

/// A heading that groups the rows under it.
///
/// Small, weak, and followed by a hairline across the menu, so a long dialog
/// reads as a few short lists rather than one run of fields.
pub(crate) fn menu_section(ui: &mut egui::Ui, heading: impl Into<String>) {
    ui.add_space(4.0);
    let color = ui.visuals().weak_text_color();
    let galley = ui.painter().layout_no_wrap(heading.into(), egui::FontId::proportional(11.0), color);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), galley.size().y.max(14.0)), egui::Sense::hover());
    let text_end = rect.left() + galley.size().x;
    ui.painter().galley(egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0), galley, color);
    ui.painter().line_segment(
        [egui::pos2(text_end + 8.0, rect.center().y), rect.right_center()],
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
    ui.add_space(2.0);
}

/// A quiet block of explanatory text inside a menu.
///
/// For the sentence that says what the dialog is about to do, or what the
/// viewport is waiting for - not for warnings, which belong in the console.
pub(crate) fn menu_note(ui: &mut egui::Ui, text: impl Into<String>) {
    let width = ui.available_width();
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(CONTROL_CORNER_RADIUS)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width((width - 16.0).max(1.0));
            ui.label(egui::RichText::new(text.into()).color(ui.visuals().weak_text_color()));
        });
}

/// Bounds on the width a field's control takes when the caller has not asked
/// for a particular one.
///
/// The floor keeps an entry box usable in a narrow docked panel; the ceiling
/// stops a wide dialog from stretching a number box across the whole card.
const MENU_FIELD_MIN_WIDTH: f32 = 100.0;
const MENU_FIELD_MAX_WIDTH: f32 = 260.0;

/// The width every field in one menu gives its control.
///
/// Two columns that fill the row between them: the labels take what the
/// *widest* of them needs, and the controls take the rest. The widest is
/// measured across the rows drawn into this container on the previous frame
/// and remembered against the container's id, so every row lands on the same
/// column however long its own label is - and a menu with short labels does
/// not leave a band of dead space down its middle. A container being drawn for
/// the first time falls back to halving the row.
///
/// `label_needs` is what this row's own label would like, and joins the
/// measurement for the next frame; rows that draw their own label (a read-only
/// value, a vector triple) pass theirs through [`label_needs`] so they line up
/// with the fields around them.
pub(crate) fn field_column(ui: &egui::Ui, label_needs: f32) -> f32 {
    let id = ui.id().with("menu_field_column");
    let pass = ui.ctx().cumulative_pass_nr();
    let widest_label = ui.ctx().memory_mut(|memory| {
        let (last_pass, running, settled) = memory.data.get_temp::<(u64, f32, f32)>(id).unwrap_or((u64::MAX, 0.0, 0.0));
        // A new pass promotes what the last one measured and starts again.
        let (running, settled) = if last_pass == pass { (running, settled) } else { (0.0, running) };
        let running = running.max(label_needs);
        memory.data.insert_temp(id, (pass, running, settled));
        settled
    });
    let row_width = ui.available_width();
    let width = if widest_label > 0.0 {
        row_width - widest_label - ui.spacing().item_spacing.x
    } else {
        row_width * 0.5
    };
    width.clamp(MENU_FIELD_MIN_WIDTH, MENU_FIELD_MAX_WIDTH)
}

/// Width a label needs to be drawn in full, including its help marker's slot.
pub(crate) fn label_needs(ui: &egui::Ui, label: &str, has_help_text: bool) -> f32 {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let text = ui
        .ctx()
        .fonts_mut(|fonts| fonts.layout_no_wrap(label.to_owned(), font_id, egui::Color32::PLACEHOLDER))
        .size()
        .x;
    text + if has_help_text { HELP_MARKER_SIZE + ui.spacing().item_spacing.x } else { 0.0 }
}

/// [`field_column`] for a row that owns its label rather than passing it to
/// [`MenuField`].
pub(crate) fn field_column_for(ui: &egui::Ui, label: &str, has_help_text: bool) -> f32 {
    field_column(ui, label_needs(ui, label, has_help_text))
}

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

    /// `add_field` is given the row's height and the width of the column the
    /// menu's controls share, so a custom control fills the same column the
    /// standard fields do.
    pub(crate) fn show<R>(self, ui: &mut egui::Ui, add_field: impl FnOnce(&mut egui::Ui, f32, f32) -> R) -> R {
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
    width: Option<f32>,
}

impl<'paths> MenuFieldFilePicker<'paths> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, paths: &'paths [PathBuf]) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            paths,
            empty_text: "No file chosen".into(),
            button_text: "Choose...".into(),
            width: None,
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
        self.width = Some(width);
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
        menu_field_row(ui, label, help_text, |ui, row_height, column_width| {
            let width = width.unwrap_or(column_width);
            let inner = ui.horizontal(|ui| {
                let clicked = ui.add(MenuButton::new(button_text).min_width(row_height * 3.8)).clicked();
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

/// Size of the round help marker drawn beside a label.
const HELP_MARKER_SIZE: f32 = 14.0;

fn menu_field_row<R>(ui: &mut egui::Ui, label: egui::WidgetText, help_text: Option<egui::WidgetText>, add_field: impl FnOnce(&mut egui::Ui, f32, f32) -> R) -> R {
    let row_height = ui.spacing().interact_size.y;
    let row_width = ui.available_width();
    let column_width = field_column(ui, label_needs(ui, label.text(), help_text.is_some()));

    // The control claims its width first and the label takes what is left, so
    // a label too long for the row ends in an ellipsis instead of running on
    // underneath the control.
    ui.allocate_ui_with_layout(egui::vec2(row_width, row_height), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let inner = add_field(ui, row_height, column_width);
        let label_width = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(label_width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            menu_field_label(ui, label, help_text);
        });
        inner
    })
    .inner
}

/// Draw a field's label, and its help marker if it has one, within the width
/// the caller has left for them.
///
/// The text is laid out against that width less the marker's slot, so it
/// truncates rather than pushing the marker over the field beside it. A
/// truncated label names itself in full on hover.
pub(crate) fn menu_field_label(ui: &mut egui::Ui, label: egui::WidgetText, help_text: Option<egui::WidgetText>) {
    let marker_slot = if help_text.is_some() { HELP_MARKER_SIZE + ui.spacing().item_spacing.x } else { 0.0 };
    let text_width = (ui.available_width() - marker_slot).max(0.0);
    let full_text = label.text().to_owned();
    let galley = label.into_galley(ui, Some(egui::TextWrapMode::Truncate), text_width, egui::TextStyle::Body);
    let elided = galley.elided;
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().galley(rect.min, galley, ui.visuals().text_color());
    }
    if elided {
        response.on_hover_text(full_text);
    }

    if let Some(help_text) = help_text {
        let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(HELP_MARKER_SIZE), egui::Sense::hover());
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
            width: Option<f32>,
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
                    width: None,
                    speed: 0.1,
                    suffix: String::new(),
                    max_decimals: 2,
                }
            }

            pub(crate) fn width(mut self, width: f32) -> Self {
                self.width = Some(width);
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
                menu_field_row(ui, label, help_text, |ui, row_height, column_width| {
                    ui.add_sized(
                        [width.unwrap_or(column_width), row_height],
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
                let width = width.unwrap_or_else(|| field_column(ui, label_needs(ui, label.text(), help_text.is_some())));
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
        menu_field_row(ui, label, help_text, |ui, _, _| ui.add(SlidingToggle::new(value)))
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
        // The track is a fixed size, but the row's full height stays clickable
        // so the switch is no harder to hit than the fields above it.
        let height = ui.spacing().interact_size.y.max(TOGGLE_HEIGHT);
        let (rect, mut response) = ui.allocate_exact_size(egui::vec2(TOGGLE_WIDTH, height), egui::Sense::click());

        if response.clicked() {
            *self.value = !*self.value;
            response.mark_changed();
        }

        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *self.value, ""));

        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact_selectable(&response, *self.value);
            // Keyed by the bool's own address rather than `response.id`: that id is
            // assigned by allocation order within the enclosing panel, so switching
            // properties tabs lines a toggle up with whatever occupied that same slot
            // in the previous tab and replays a slide from its stale cached value.
            let value_id = egui::Id::new(self.value as *const bool as usize);
            let animation = ui.ctx().animate_bool_responsive(value_id, *self.value);
            let track_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(TOGGLE_WIDTH, TOGGLE_HEIGHT));
            let radius = track_rect.height() / 2.0;
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
    width: Option<f32>,
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
            width: None,
            speed: 1.,
            suffix: String::new(),
        }
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
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
        menu_field_row(ui, label, help_text, |ui, row_height, column_width| {
            ui.add_sized(
                [width.unwrap_or(column_width), row_height],
                egui::DragValue::new(value).speed(speed).range(range).suffix(suffix),
            )
        })
    }
}

/// A consistently aligned single-line text field for draggable and docked menus.
pub(crate) struct MenuFieldText<'value> {
    label: egui::WidgetText,
    help_text: Option<egui::WidgetText>,
    value: &'value mut String,
    width: Option<f32>,
    hint: egui::WidgetText,
}

impl<'value> MenuFieldText<'value> {
    pub(crate) fn new(label: impl Into<egui::WidgetText>, value: &'value mut String) -> Self {
        Self {
            label: label.into(),
            help_text: None,
            value,
            width: None,
            hint: egui::WidgetText::default(),
        }
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
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
        menu_field_row(ui, label, help_text, |ui, row_height, column_width| {
            ui.add_sized([width.unwrap_or(column_width), row_height], egui::TextEdit::singleline(value).hint_text(hint))
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
        menu_field_row(ui, label, help_text, |ui, _, _| ui.color_edit_button_srgba(value))
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
        menu_field_row(ui, label, help_text, |ui, _, _| ui.color_edit_button_rgba_premultiplied(value))
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
    width: Option<f32>,
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
            width: None,
        }
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
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
        menu_field_row(ui, label, help_text, |ui, _, column_width| {
            let width = width.unwrap_or(column_width);
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
