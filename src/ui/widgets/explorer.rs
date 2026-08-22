/// Width of the gutter an entry's label is indented by, matching the space the
/// section heading's icon occupies so labels line up under the heading text.
const ENTRY_LABEL_GUTTER: f32 = 20.0;

/// Height of one tree row.
///
/// Every row in the panel - heading, entry, and empty-state note alike - is
/// exactly this tall and carries no spacing between it and the next, so the
/// alternating background tiles the list as continuous bands.
pub(crate) fn row_height(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    (font.size + 8.0).max(22.0)
}

/// Paint one row's alternating background across the panel's full width.
///
/// `shape` is a slot reserved with `Painter::add` before the row's contents
/// were laid out: the rect is only known afterwards, and painting then would
/// cover the row's own text.
fn paint_row_stripe(ui: &egui::Ui, shape: egui::layers::ShapeIdx, y_range: egui::Rangef, row_index: usize, stripe: egui::Color32) {
    if row_index % 2 != 1 {
        return;
    }
    let row = egui::Rect::from_x_y_ranges(ui.clip_rect().x_range(), y_range);
    ui.painter().set(shape, egui::Shape::rect_filled(row, 0.0, stripe));
}

/// Continue the banding into the empty space below the last row.
///
/// Without this the tree reads as a list that stops partway down the panel;
/// with it, it reads as a table that runs to the properties panel's edge.
/// `next_row_index` is the position the next real row would have taken.
pub(crate) fn fill_trailing_stripes(ui: &egui::Ui, next_row_index: usize, stripe: egui::Color32) {
    let height = row_height(ui);
    let remaining = ui.available_rect_before_wrap();
    if remaining.height() <= 0.0 {
        return;
    }
    let x_range = ui.clip_rect().x_range();
    let mut top = remaining.top();
    let mut row_index = next_row_index;
    while top < remaining.bottom() {
        if row_index % 2 == 1 {
            let band = egui::Rect::from_x_y_ranges(x_range, egui::Rangef::new(top, (top + height).min(remaining.bottom())));
            ui.painter().rect_filled(band, 0.0, stripe);
        }
        top += height;
        row_index += 1;
    }
}

/// A section's empty-state line ("No design layers"), as a striped tree row.
pub(crate) fn explorer_note(ui: &mut egui::Ui, text: &str, row_index: usize, stripe: egui::Color32) {
    let height = row_height(ui);
    let slot = ui.painter().add(egui::Shape::Noop);
    let response = ui
        .allocate_ui_with_layout(egui::vec2(ui.available_width(), height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.add_space(ENTRY_LABEL_GUTTER);
            ui.label(egui::RichText::new(text).weak().italics());
        })
        .response;
    paint_row_stripe(ui, slot, response.rect.y_range(), row_index, stripe);
}

/// A label row used for entries in the explorer tree.
///
/// Entries carry no icon of their own: the kind is named once by the section
/// heading above them (see [`ExplorerHeader::icon`]).
pub(crate) struct ExplorerEntry {
    id: egui::Id,
    title: egui::WidgetText,
    selected: bool,
    reserve_toggle_gutter: bool,
    /// Row position within the panel, for the alternating background.
    row_index: usize,
    /// Fill for the lit rows.
    stripe: egui::Color32,
}

impl ExplorerEntry {
    pub(crate) fn new(id: egui::Id, title: impl Into<egui::WidgetText>) -> Self {
        Self {
            id,
            title: title.into(),
            selected: false,
            reserve_toggle_gutter: false,
            row_index: 0,
            stripe: egui::Color32::TRANSPARENT,
        }
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Give this row the alternating tint for `row_index`, filled with `stripe`.
    pub(crate) fn stripe(mut self, row_index: usize, stripe: egui::Color32) -> Self {
        self.row_index = row_index;
        self.stripe = stripe;
        self
    }
}

impl egui::Widget for ExplorerEntry {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            id,
            title,
            selected,
            reserve_toggle_gutter,
            row_index,
            stripe,
        } = self;
        let height = row_height(ui);
        ui.scope_builder(egui::UiBuilder::new().id(id.with("explorer_entry_scope")), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.spacing_mut().item_spacing.x = 0.0;
            let slot = ui.painter().add(egui::Shape::Noop);
            let row = ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if reserve_toggle_gutter {
                    // CollapsingState reserves exactly `spacing.indent` for its toggle.
                    // Match its gutter so this leaf starts at the same x.
                    ui.add_space(ui.spacing().indent);
                }
                ui.add_space(ENTRY_LABEL_GUTTER);
                ui.add(
                    egui::Button::new("")
                        .left_text(title)
                        .frame(false)
                        .selected(selected)
                        .min_size(egui::vec2(ui.available_width(), height)),
                )
            });
            paint_row_stripe(ui, slot, row.response.rect.y_range(), row_index, stripe);
            row.inner
        })
        .inner
    }
}

/// Opens `id`'s collapsing state the first time `default_open` is true.
///
/// `CollapsingState::load_with_default_open` only honours the default while no
/// state has been stored yet, and the Projects section wants to start open.
fn apply_default_open_once(ctx: &egui::Context, id: egui::Id, default_open: bool) {
    if !default_open {
        return;
    }
    let applied_id = id.with("explorer_header_default_open_applied");
    if ctx.data_mut(|d| d.get_temp::<bool>(applied_id).unwrap_or(false)) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(applied_id, true));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, true);
    state.set_open(true);
    state.store(ctx);
}

/// What [`apply_auto_open`] saw for a section on the previous frame.
#[derive(Clone, Copy)]
struct AutoOpenState {
    epoch: u64,
    entries: usize,
}

/// Keep a project-scoped section's open state in step with its contents.
///
/// A section opens as soon as it gains an entry, and collapses as soon as it
/// has none, so a newly created layer or imported model is never hidden behind
/// a collapsed header and an empty header never sits open over a "nothing
/// here" line. `epoch` identifies the active project: when it changes, the
/// section mirrors the new project's contents outright, which also reopens a
/// populated section the user had collapsed under the previous project.
/// Between those points a populated section is left exactly as the user set
/// it.
///
/// Every rule is a transition rather than a per-frame assertion, because the
/// two inputs do not always change on the same frame: opening a project can
/// publish the new active project a frame or more before its items are loaded
/// or before the outgoing project's are dropped. Watching for the change
/// rather than the state gets the same result whichever arrives first, and
/// still lets the user expand an empty section by hand afterwards.
fn apply_auto_open(ctx: &egui::Context, id: egui::Id, entries: usize, epoch: u64) {
    let state_id = id.with("explorer_header_auto_open");
    let previous = ctx.data_mut(|d| d.get_temp::<AutoOpenState>(state_id));
    ctx.data_mut(|d| d.insert_temp(state_id, AutoOpenState { epoch, entries }));
    let open = match previous {
        Some(previous) if entries > previous.entries => Some(true),
        Some(previous) if entries == 0 && (previous.entries > 0 || previous.epoch != epoch) => Some(false),
        Some(previous) if previous.epoch != epoch => Some(true),
        Some(_) => None,
        None => Some(entries > 0),
    };
    if let Some(open) = open {
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, open);
        state.set_open(open);
        state.store(ctx);
    }
}

/// A collapsible explorer section heading.
pub(crate) struct ExplorerHeader {
    id: egui::Id,
    title: String,
    /// Icon naming the kind of entry this section holds.
    icon: Option<egui::ImageSource<'static>>,
    dirty: bool,
    default_open: bool,
    /// Heading tint, matching the section's entry icons. `None` uses the
    /// theme's normal text colour.
    color: Option<egui::Color32>,
    /// Entry count and active-project epoch, for sections that follow their
    /// contents. `None` leaves the section on plain `default_open` behaviour.
    auto_open: Option<(usize, u64)>,
    /// Row position within the panel, for the alternating background.
    row_index: usize,
    /// Fill for the lit rows.
    stripe: egui::Color32,
}

impl ExplorerHeader {
    pub(crate) fn new(id: egui::Id, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            icon: None,
            dirty: false,
            default_open: false,
            color: None,
            auto_open: None,
            row_index: 0,
            stripe: egui::Color32::TRANSPARENT,
        }
    }

    /// Give this heading row the alternating tint for `row_index`, filled with
    /// `stripe`. Headings take part in the striping like any other row.
    pub(crate) fn stripe(mut self, row_index: usize, stripe: egui::Color32) -> Self {
        self.row_index = row_index;
        self.stripe = stripe;
        self
    }

    /// Show `icon` ahead of the heading text. The section's entries are plain
    /// labels indented to line up beneath it.
    pub(crate) fn icon(mut self, icon: egui::ImageSource<'static>) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Mark this section as containing unsaved changes.
    ///
    /// The marker is only added to a collapsed header. While the section is
    /// open, its dirty entry labels provide the more specific indication.
    pub(crate) fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    pub(crate) fn default_open(mut self, default_open: bool) -> Self {
        self.default_open = default_open;
        self
    }

    /// Tint the heading text, keying the section to the colour of the icons
    /// its entries use.
    pub(crate) fn color(mut self, color: egui::Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Follow the section's contents: see [`apply_auto_open`]. `epoch`
    /// identifies the active project, so switching projects re-syncs the
    /// section against the new project's entries.
    pub(crate) fn auto_open(mut self, entries: usize, epoch: u64) -> Self {
        self.auto_open = Some((entries, epoch));
        self
    }

    pub(crate) fn show<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> (egui::Response, egui::InnerResponse<egui::Response>, Option<egui::InnerResponse<R>>) {
        let Self {
            id,
            mut title,
            icon,
            dirty,
            mut default_open,
            color,
            auto_open,
            row_index,
            stripe,
        } = self;
        if let Some((entries, epoch)) = auto_open {
            apply_auto_open(ui.ctx(), id, entries, epoch);
            default_open = entries > 0;
        } else {
            apply_default_open_once(ui.ctx(), id, default_open);
        }
        let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
        if dirty && !state.is_open() {
            title.push_str(" *");
        }
        let height = row_height(ui);
        ui.scope_builder(egui::UiBuilder::new().id(id.with("explorer_header_scope")), |ui| {
            let slot = ui.painter().add(egui::Shape::Noop);
            let (toggle_response, header_response, body_response) = state
                .show_header(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let text = match color {
                        Some(color) => crate::ui::fonts::bold(&title).color(color),
                        None => crate::ui::fonts::bold(&title),
                    };
                    // The whole row is one row tall, so the heading claims the
                    // full height rather than letting the icon set it.
                    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let icon_response = icon.map(|icon| ui.add(egui::Image::new(icon).fit_to_exact_size(egui::vec2(16.0, 16.0)).sense(egui::Sense::click())));
                        let label = ui.add(egui::Button::new(text).frame(false).sense(egui::Sense::click()));
                        match icon_response {
                            Some(icon_response) => icon_response.union(label),
                            None => label,
                        }
                    })
                    .inner
                })
                .body(add_contents);

            if header_response.inner.clicked() {
                let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
                state.toggle(ui);
                state.store(ui.ctx());
            }

            paint_row_stripe(ui, slot, toggle_response.rect.union(header_response.response.rect).y_range(), row_index, stripe);

            (toggle_response, header_response, body_response)
        })
        .inner
    }
}
