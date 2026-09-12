//! Side islands: the horizontally-resizable panels down either edge of a
//! workspace.
//!
//! One place decides how wide an island may be, what surface it paints, and
//! which region and seam it hands back to the chrome, so every column of every
//! workspace resizes and rounds off the same way. An island is always shown:
//! its seam narrows it to the width its content stops working at, and no
//! further.

use crate::ui::chrome;

/// Which edge of the workspace an island is anchored to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Left,
    Right,
}

impl Side {
    /// The edge the resize seam runs down: the island's inner one.
    fn seam(self) -> chrome::Edge {
        match self {
            Self::Left => chrome::Edge::Right,
            Self::Right => chrome::Edge::Left,
        }
    }

    fn panel(self, id: impl Into<egui::Id>) -> egui::Panel {
        match self {
            Self::Left => egui::Panel::left(id),
            Self::Right => egui::Panel::right(id),
        }
    }
}

/// One side panel of a workspace.
pub(crate) struct Island {
    id: &'static str,
    side: Side,
    default_width: f32,
    min_width: f32,
    max_width: Option<f32>,
    fill: Option<egui::Color32>,
    flush: bool,
    bare: bool,
}

/// What an island claimed, and what its content returned.
pub(crate) struct IslandResponse<R> {
    /// The regions it drew itself, for [`chrome::paint_regions`]: the panel,
    /// or nothing at all for an [`Island::bare`] column, whose panes are the
    /// caller's own regions.
    pub(crate) regions: Vec<egui::Rect>,
    /// The seam that resizes it, ready for [`chrome::paint_grips`].
    pub(crate) grip: chrome::Grip,
    /// What the content returned.
    pub(crate) inner: R,
}

impl Island {
    /// An island on `side`. `id` is the panel's own id: it keys the persisted
    /// width and the grip alike, so it has to stay stable across frames.
    pub(crate) fn new(id: &'static str, side: Side) -> Self {
        Self {
            id,
            side,
            default_width: 260.0,
            min_width: 140.0,
            max_width: None,
            fill: None,
            flush: false,
            bare: false,
        }
    }

    /// The width the island opens at before the user has dragged it.
    pub(crate) fn default_width(mut self, width: f32) -> Self {
        self.default_width = width;
        self
    }

    /// The width the island stops shrinking at.
    pub(crate) fn min_width(mut self, width: f32) -> Self {
        self.min_width = width;
        self
    }

    pub(crate) fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }

    pub(crate) fn fill(mut self, fill: egui::Color32) -> Self {
        self.fill = Some(fill);
        self
    }

    /// Hand the content the region edge to edge, for an island filled by a
    /// table that draws its own heading and banding.
    pub(crate) fn flush(mut self) -> Self {
        self.flush = true;
        self
    }

    /// Draw no frame of the island's own: for a column that arranges regions
    /// of its own rather than being one, and rounds those off itself.
    pub(crate) fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// Draw the island. `content` receives the rect it has to work in.
    pub(crate) fn show<R>(self, ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui, egui::Rect) -> R) -> IslandResponse<R> {
        let mut frame = chrome::region_frame(ui);
        if let Some(fill) = self.fill {
            frame = frame.fill(fill);
        }
        // No island takes more than half of what is left when it is claimed.
        // Each one would otherwise keep the width it was dragged to until the
        // window ran out, leaving the last island and the workspace to absorb
        // the whole of a shrink between them - and a workspace of no width at
        // all. The cap only ever binds on a window too narrow for the islands
        // on it; the minimum gives way with it, since a floor no one can
        // honour just puts the overflow back.
        let cap = (ui.available_rect_before_wrap().width() * 0.5).max(1.0);
        let min_width = self.min_width.min(cap);
        let max_width = self.max_width.map_or(cap, |max| max.min(cap));
        // What the user dragged the island to, kept apart from what it is
        // showing: egui stores the width a panel came out at every frame, so a
        // window too narrow to hold the island would otherwise overwrite the
        // choice, and widening it again would leave the island squeezed.
        let width_id = egui::Id::new(("island_width", self.id));
        let seam = ui.ctx().read_response(egui::Id::new(self.id).with("__resize"));
        let dragging = seam.is_some_and(|seam| seam.dragged());
        if !dragging && let Some(wanted) = ui.data(|data| data.get_temp::<f32>(width_id)) {
            pin_width(ui, self.id, wanted.clamp(min_width, max_width));
        }

        let response = self
            .side
            .panel(self.id)
            .resizable(true)
            .default_size(self.default_width)
            .min_size(min_width)
            .max_size(max_width)
            .show_separator_line(chrome::show_separator_line(ui))
            .frame(match (self.bare, self.flush) {
                (true, _) => egui::Frame::NONE,
                (false, true) => frame.inner_margin(egui::Margin::ZERO),
                (false, false) => frame,
            })
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                ui.set_clip_rect(ui.clip_rect().intersect(rect));
                // Content that measures itself must neither push the panel
                // wider than the user dragged it nor let it fall short: egui
                // takes the panel's size from what the content came out as,
                // and persists it, so content that does not fill the width
                // walks the island back to its minimum a frame at a time.
                ui.set_max_width(ui.available_width());
                ui.set_min_width(ui.available_width());
                content(ui, rect)
            });

        let rect = response.response.rect;
        // Worth remembering only while the island is showing a width of its
        // own: at the cap it is showing what the window allows, which is not a
        // choice to come back to.
        if rect.width() + 0.5 < cap {
            ui.data_mut(|data| data.insert_temp(width_id, rect.width()));
        }
        IslandResponse {
            // A bare column is not a region: the panes inside it are, and the
            // caller rounds those off itself.
            regions: if self.bare { Vec::new() } else { vec![rect] },
            grip: chrome::Grip::new(rect, self.side.seam(), self.id),
            inner: response.inner,
        }
    }
}

/// Hold the panel at `width`, by writing the size egui keeps for it.
///
/// The panel stays resizable, so its seam is live; while no one is holding
/// that seam the width comes from here, which is what carries a dragged width
/// back across a spell in a window too narrow to show it.
fn pin_width(ui: &egui::Ui, id: &'static str, width: f32) {
    let panel_id = egui::Id::new(id);
    let Some(mut state) = egui::containers::panel::PanelState::load(ui.ctx(), panel_id) else {
        return;
    };
    if (state.size().x - width).abs() < 0.1 {
        return;
    }
    state.outer_rect = egui::Rect::from_min_size(state.outer_rect.min, egui::vec2(width, state.outer_rect.height()));
    ui.ctx().data_mut(|data| data.insert_persisted(panel_id, state));
}
