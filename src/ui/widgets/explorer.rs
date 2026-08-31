use crate::ui::state::{ExplorerSection, Workspace};

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

/// Reserve a paint slot for the panel's alternating background, and record
/// where its bands should start.
///
/// Call this before any row is drawn, so the bands paint behind row content;
/// finish with [`paint_fixed_stripes`] once the tree's final height is known.
pub(crate) fn reserve_fixed_stripes(ui: &egui::Ui) -> (egui::layers::ShapeIdx, f32) {
    (ui.painter().add(egui::Shape::Noop), ui.cursor().top())
}

/// Paint the tree's alternating background as one fixed, tiled pattern
/// anchored to `top`, rather than a colour recomputed per row from its
/// position in the (changing) list.
///
/// Rows draw with a transparent background and simply slide over these bands
/// as sections collapse or expand. Painting per row instead - each row
/// picking odd/even from its own place in the current content order - looked
/// fine at rest, but the moment a section opened or closed, egui lays out
/// every row below it in the same frame the toggle happens (only the reveal
/// animates), so every row's colour snapped to its new value a beat before it
/// finished sliding into position: bands appeared to invert under the moving
/// content instead of staying put. Anchoring to the content's top edge and
/// painting one shape for the whole tree makes the pattern indifferent to
/// which row currently sits where.
pub(crate) fn paint_fixed_stripes(ui: &egui::Ui, slot: egui::layers::ShapeIdx, top: f32, stripe: egui::Color32) {
    let bands = stripe_bands(ui.clip_rect().x_range(), top, ui.available_rect_before_wrap().bottom(), row_height(ui), stripe);
    ui.painter().set(slot, bands);
}

/// The alternating bands themselves: one shape tiling `top..bottom` in
/// `height`-tall rows, every other one filled.
///
/// Split out from [`paint_fixed_stripes`] so any list that wants the same
/// banding - the welcome splash's Recent box, for one - can anchor it to its
/// own box rather than to the explorer's panel, and can tile the full box
/// whether or not the entries reach the bottom of it.
pub(crate) fn stripe_bands(x_range: egui::Rangef, top: f32, bottom: f32, height: f32, stripe: egui::Color32) -> egui::Shape {
    if bottom <= top || height <= 0.0 {
        return egui::Shape::Noop;
    }
    let mut shapes = Vec::new();
    let mut band_top = top;
    let mut band_index = 0usize;
    while band_top < bottom {
        if band_index % 2 == 1 {
            let band = egui::Rect::from_x_y_ranges(x_range, egui::Rangef::new(band_top, (band_top + height).min(bottom)));
            shapes.push(egui::Shape::rect_filled(band, 0.0, stripe));
        }
        band_top += height;
        band_index += 1;
    }
    egui::Shape::Vec(shapes)
}

/// A section's empty-state line ("No design layers"), as a tree row.
pub(crate) fn explorer_note(ui: &mut egui::Ui, text: &str) {
    let height = row_height(ui);
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        ui.add_space(ENTRY_LABEL_GUTTER);
        ui.label(egui::RichText::new(text).weak().italics());
    });
}

/// Width reserved for one trailing toggle in an explorer row.
const TOGGLE_WIDTH: f32 = 20.0;

/// Drawn size of a toggle's glyph inside [`TOGGLE_WIDTH`].
const TOGGLE_ICON: f32 = 14.0;

/// The trailing visibility and edit-lock state of one explorer row.
#[derive(Clone, Copy)]
pub(crate) struct EntryToggles {
    /// Whether the item currently draws in the viewport.
    pub(crate) visible: bool,
    /// Whether the item is locked against selection and editing.
    pub(crate) locked: bool,
    /// Unloaded items keep both icons in place, greyed out and inert, so rows
    /// do not change width as items load and unload.
    pub(crate) enabled: bool,
}

/// An explorer row's response, plus whichever trailing toggle was clicked.
pub(crate) struct ExplorerEntryResponse {
    /// The row's label, carrying selection clicks and the context menu.
    pub(crate) response: egui::Response,
    pub(crate) visibility_clicked: bool,
    pub(crate) lock_clicked: bool,
}

/// One trailing icon toggle. Returns whether it was clicked.
///
/// `emphasis` draws the icon at full text strength rather than muted, and the
/// disabled states fainter still. Each toggle emphasises the state that means
/// "this row is doing something": an open eye for a drawn item, a shut
/// padlock for a locked one.
///
/// No hover tooltip: these sit on every row of the tree, so one would pop up
/// on any pass of the mouse over the panel. The icon brightening under the
/// cursor is the affordance, and the row's context menu names both actions in
/// words.
fn entry_toggle(ui: &mut egui::Ui, icon: egui::ImageSource<'static>, emphasis: bool, enabled: bool, height: f32) -> bool {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(TOGGLE_WIDTH, height), sense);
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let tint = match (enabled, response.hovered(), emphasis) {
            (false, ..) => visuals.weak_text_color().gamma_multiply(0.4),
            (true, true, _) => visuals.strong_text_color(),
            (true, false, true) => visuals.text_color(),
            (true, false, false) => visuals.weak_text_color().gamma_multiply(0.7),
        };
        if enabled && response.hovered() {
            let hover_fill = visuals.widgets.hovered.bg_fill;
            ui.painter().rect_filled(rect.shrink2(egui::vec2(1.0, 3.0)), 3.0, hover_fill);
        }
        egui::Image::new(icon)
            .tint(tint)
            .paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(TOGGLE_ICON)));
    }
    enabled && response.clicked()
}

/// A label row used for entries in the explorer tree.
///
/// Entries carry no leading icon of their own: the kind is named once by the
/// section heading above them (see [`ExplorerHeader::icon`]). Data rows do
/// carry trailing visibility and lock toggles: see [`ExplorerEntry::toggles`].
pub(crate) struct ExplorerEntry {
    id: egui::Id,
    title: egui::WidgetText,
    selected: bool,
    reserve_toggle_gutter: bool,
    toggles: Option<EntryToggles>,
    leading_icon: Option<(egui::ImageSource<'static>, egui::Color32)>,
}

impl ExplorerEntry {
    pub(crate) fn new(id: egui::Id, title: impl Into<egui::WidgetText>) -> Self {
        Self {
            id,
            title: title.into(),
            selected: false,
            reserve_toggle_gutter: false,
            toggles: None,
            leading_icon: None,
        }
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Show trailing eye and padlock toggles at the row's right edge.
    pub(crate) fn toggles(mut self, toggles: EntryToggles) -> Self {
        self.toggles = Some(toggles);
        self
    }

    /// Lay the row out, returning its label response alongside the toggles.
    ///
    /// [`egui::Widget`] can only hand back the label response, so rows that
    /// act on their toggles call this instead of `ui.add`.
    pub(crate) fn show(self, ui: &mut egui::Ui) -> ExplorerEntryResponse {
        let Self {
            id,
            title,
            selected,
            reserve_toggle_gutter,
            toggles,
            leading_icon,
        } = self;
        let height = row_height(ui);
        ui.scope_builder(egui::UiBuilder::new().id(id.with("explorer_entry_scope")), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if reserve_toggle_gutter {
                    // CollapsingState reserves exactly `spacing.indent` for its toggle.
                    // Match its gutter so this leaf starts at the same x.
                    ui.add_space(ui.spacing().indent);
                }
                match leading_icon {
                    Some((icon, color)) => {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(ENTRY_LABEL_GUTTER, height), egui::Sense::hover());
                        if ui.is_rect_visible(rect) {
                            egui::Image::new(icon)
                                .tint(color)
                                .paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(TOGGLE_ICON)));
                        }
                    }
                    None => ui.add_space(ENTRY_LABEL_GUTTER),
                }
                // The label claims everything the toggles do not, so a long
                // name truncates against them rather than pushing them off.
                //
                // The width has to come from a nested allocation rather than
                // `Button::min_size`: a button sizes its text against the
                // whole of `ui.available_width()` and treats `min_size` only
                // as a floor, so the name would truncate at the row's edge and
                // shove the toggles past it - which is what stopped the panel
                // from being dragged narrower than its longest entry.
                let label_width = (ui.available_width() - if toggles.is_some() { 2.0 * TOGGLE_WIDTH } else { 0.0 }).max(0.0);
                let response = ui
                    .allocate_ui_with_layout(egui::vec2(label_width, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Button::new("")
                                .left_text(title)
                                .frame(false)
                                .selected(selected)
                                .min_size(egui::vec2(label_width, height)),
                        )
                    })
                    .inner;
                let (visibility_clicked, lock_clicked) = match toggles {
                    Some(EntryToggles { visible, locked, enabled }) => {
                        let visibility_clicked = entry_toggle(
                            ui,
                            if visible {
                                crate::ui::unthemed_icon!("entry_visible.svg")
                            } else {
                                crate::ui::unthemed_icon!("entry_hidden.svg")
                            },
                            visible,
                            enabled,
                            height,
                        );
                        let lock_clicked = entry_toggle(
                            ui,
                            if locked {
                                crate::ui::unthemed_icon!("entry_locked.svg")
                            } else {
                                crate::ui::unthemed_icon!("entry_unlocked.svg")
                            },
                            locked,
                            enabled,
                            height,
                        );
                        (visibility_clicked, lock_clicked)
                    }
                    None => (false, false),
                };
                ExplorerEntryResponse {
                    response,
                    visibility_clicked,
                    lock_clicked,
                }
            })
            .inner
        })
        .inner
    }
}

impl egui::Widget for ExplorerEntry {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        self.show(ui).response
    }
}

/// Set a section's open state from the workspace tab that is selected.
///
/// Only the workspace is recorded, so the arrangement is applied on the frame
/// the tab changes and never afterwards: a header the user collapsed by hand
/// stays collapsed until the next tab change.
fn apply_workspace_open(ctx: &egui::Context, id: egui::Id, workspace: Workspace, open: bool) {
    let state_id = id.with("explorer_header_workspace_open");
    if ctx.data_mut(|d| d.get_temp::<Workspace>(state_id)) == Some(workspace) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(state_id, workspace));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, open);
    state.set_open(open);
    state.store(ctx);
}

/// A collapsible explorer section heading.
pub(crate) struct ExplorerHeader {
    id: egui::Id,
    title: String,
    /// Icon naming the kind of entry this section holds.
    icon: Option<egui::ImageSource<'static>>,
    dirty: bool,
    /// Heading tint, matching the section's entry icons. `None` uses the
    /// theme's normal text colour.
    color: Option<egui::Color32>,
    /// The workspace this section belongs to, and whether that workspace
    /// opens it: see [`apply_workspace_open`].
    workspace_open: Option<(Workspace, bool)>,
}

impl ExplorerHeader {
    pub(crate) fn new(id: egui::Id, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            icon: None,
            dirty: false,
            color: None,
            workspace_open: None,
        }
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

    /// Tint the heading text, keying the section to the colour of the icons
    /// its entries use.
    pub(crate) fn color(mut self, color: egui::Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Let `workspace` decide whether this section is open: see
    /// [`apply_workspace_open`]. Switching tabs re-syncs the header against
    /// the incoming workspace's arrangement.
    pub(crate) fn workspace_open(mut self, section: ExplorerSection, workspace: Workspace) -> Self {
        self.workspace_open = Some((workspace, workspace.opens_section(section)));
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
            color,
            workspace_open,
        } = self;
        let default_open = match workspace_open {
            Some((workspace, open)) => {
                apply_workspace_open(ui.ctx(), id, workspace, open);
                open
            }
            None => false,
        };
        let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
        if dirty && !state.is_open() {
            title.push_str(" *");
        }
        let height = row_height(ui);
        ui.scope_builder(egui::UiBuilder::new().id(id.with("explorer_header_scope")), |ui| {
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
                        // The label claims the rest of the row rather than
                        // just its text, so clicking - or right-clicking, for
                        // the section menu - anywhere along the heading hits
                        // a widget that senses it.
                        let label = ui.add(
                            egui::Button::new("")
                                .left_text(text)
                                .frame(false)
                                .sense(egui::Sense::click())
                                .min_size(egui::vec2(ui.available_width(), height)),
                        );
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

            (toggle_response, header_response, body_response)
        })
        .inner
    }
}
