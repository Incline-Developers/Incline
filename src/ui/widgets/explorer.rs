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
/// state has been stored yet, and sections pass a `default_open` derived from
/// whether they have any entries.  On wasm the project is loaded asynchronously,
/// so those first frames render empty sections, store the collapsed state, and
/// the sections stay shut once their entries arrive.  Latching on the first
/// `true` gives the same result on both platforms while still leaving the
/// section under the user's control afterwards.
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

/// A collapsible explorer section heading.
pub(crate) struct ExplorerHeader {
    id: egui::Id,
    title: String,
    dirty: bool,
    default_open: bool,
}

impl ExplorerHeader {
    pub(crate) fn new(id: egui::Id, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            dirty: false,
            default_open: false,
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

    pub(crate) fn show<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> (egui::Response, egui::InnerResponse<egui::Response>, Option<egui::InnerResponse<R>>) {
        let Self {
            id,
            mut title,
            dirty,
            default_open,
        } = self;
        apply_default_open_once(ui.ctx(), id, default_open);
        let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
        if dirty && !state.is_open() {
            title.push_str(" *");
        }
        ui.scope_builder(egui::UiBuilder::new().id(id.with("explorer_header_scope")), |ui| {
            let (toggle_response, header_response, body_response) = state
                .show_header(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    ui.add(egui::Button::new(crate::ui::fonts::bold(&title)).frame(false).sense(egui::Sense::click()))
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
