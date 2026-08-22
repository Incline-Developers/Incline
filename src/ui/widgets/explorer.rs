/// An icon-and-label row used for entries in the explorer tree.
pub(crate) struct ExplorerEntry {
    id: egui::Id,
    icon: egui::ImageSource<'static>,
    title: egui::WidgetText,
    selected: bool,
    icon_size: egui::Vec2,
    reserve_toggle_gutter: bool,
}

impl ExplorerEntry {
    pub(crate) fn new(id: egui::Id, icon: egui::ImageSource<'static>, title: impl Into<egui::WidgetText>) -> Self {
        Self {
            id,
            icon,
            title: title.into(),
            selected: false,
            icon_size: egui::vec2(16.0, 16.0),
            reserve_toggle_gutter: false,
        }
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl egui::Widget for ExplorerEntry {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let Self {
            id,
            icon,
            title,
            selected,
            icon_size,
            reserve_toggle_gutter,
        } = self;
        ui.scope_builder(egui::UiBuilder::new().id(id.with("explorer_entry_scope")), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.horizontal(|ui| {
                if reserve_toggle_gutter {
                    // CollapsingState reserves exactly `spacing.indent` for its toggle.
                    // Match its zero-spacing gutter so this leaf starts at the same x.
                    let item_spacing = ui.spacing().item_spacing;
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(ui.spacing().indent);
                    ui.spacing_mut().item_spacing = item_spacing;
                }
                let icon = ui.add(egui::Image::new(icon).fit_to_exact_size(icon_size).sense(egui::Sense::click()));
                let label = ui.add(
                    egui::Button::new("")
                        .left_text(title)
                        .frame(false)
                        .selected(selected)
                        .min_size((ui.available_width(), ui.available_height()).into()),
                );
                icon.union(label)
            })
            .inner
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
    dirty: bool,
    default_open: bool,
    /// Heading tint, matching the section's entry icons. `None` uses the
    /// theme's normal text colour.
    color: Option<egui::Color32>,
    /// Entry count and active-project epoch, for sections that follow their
    /// contents. `None` leaves the section on plain `default_open` behaviour.
    auto_open: Option<(usize, u64)>,
}

impl ExplorerHeader {
    pub(crate) fn new(id: egui::Id, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            dirty: false,
            default_open: false,
            color: None,
            auto_open: None,
        }
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
            dirty,
            mut default_open,
            color,
            auto_open,
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
        ui.scope_builder(egui::UiBuilder::new().id(id.with("explorer_header_scope")), |ui| {
            let (toggle_response, header_response, body_response) = state
                .show_header(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    let text = match color {
                        Some(color) => crate::ui::fonts::bold(&title).color(color),
                        None => crate::ui::fonts::bold(&title),
                    };
                    ui.add(egui::Button::new(text).frame(false).sense(egui::Sense::click()))
                })
                .body(|ui| {
                    ui.add_space(1.0);
                    add_contents(ui)
                });

            if header_response.inner.clicked() {
                let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
                state.toggle(ui);
                state.store(ui.ctx());
            }

            ui.add_space(1.0);

            (toggle_response, header_response, body_response)
        })
        .inner
    }
}
