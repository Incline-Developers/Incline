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

/// A collapsible explorer section with an icon beside its heading.
pub(crate) struct ExplorerHeader {
    id: egui::Id,
    icon: egui::ImageSource<'static>,
    title: egui::WidgetText,
    default_open: bool,
    icon_size: egui::Vec2,
}

impl ExplorerHeader {
    pub(crate) fn new(id: egui::Id, icon: egui::ImageSource<'static>, title: impl Into<egui::WidgetText>) -> Self {
        Self {
            id,
            icon,
            title: title.into(),
            default_open: false,
            icon_size: egui::vec2(20.0, 20.0),
        }
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
            icon,
            title,
            default_open,
            icon_size,
        } = self;
        apply_default_open_once(ui.ctx(), id, default_open);
        ui.scope_builder(egui::UiBuilder::new().id(id.with("explorer_header_scope")), |ui| {
            let (toggle_response, header_response, body_response) = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open)
                .show_header(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    let icon = ui.add(egui::Image::new(icon).fit_to_exact_size(icon_size).sense(egui::Sense::click()));
                    let label = ui.add(egui::Button::new(title).frame(false).sense(egui::Sense::click()));
                    icon.union(label)
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
