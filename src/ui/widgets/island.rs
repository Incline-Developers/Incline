//! Side islands: the horizontally-resizable panels down either edge of a
//! workspace, and the one gesture that opens and closes them.
//!
//! Every island behaves the same way. Drag its seam past the width it stops
//! shrinking at and it closes to a narrow spine that still names it; pull the
//! spine back out, or double-click the seam, and it opens at the width it
//! had. The seam is the only handle - a closed spine is not a button, so the
//! gesture reads the same whichever state the island is in, and a stray click
//! on a strip of panel never rearranges the workspace.
//!
//! A column of several panes keeps its shape shut: the spine carries one strip
//! per pane, in the order they stand and splitting the height between them, so
//! what closed is still legible as the two or three things it was.
//!
//! Whether an island is open is remembered on the context under its own id
//! rather than in `EditorState`: an island's content routinely borrows the
//! editor for the whole of the closure that draws it, which leaves no way to
//! hand a flag out of the same struct in beside it.

use crate::ui::chrome;

/// Width of a closed island's spine.
///
/// Wide enough to read as a strip of panel with a label down it, and to be an
/// easy target for the drag that opens it again. The region margin comes off
/// either side, so the surface itself is narrower than this by two of them.
const SPINE_WIDTH: f32 = 34.0;

/// Type size of the title on a closed spine. Set bold, like the headings the
/// panes it stands in for carry.
const SPINE_TEXT_SIZE: f32 = 11.0;

/// Gap between the top of the spine and the start of its title.
const SPINE_TEXT_INSET: f32 = 12.0;

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

/// One side panel of a workspace, open or dragged shut.
pub(crate) struct Island {
    id: &'static str,
    side: Side,
    titles: Vec<String>,
    default_width: f32,
    min_width: f32,
    max_width: Option<f32>,
    fill: Option<egui::Color32>,
    flush: bool,
    bare: bool,
    open_by_default: bool,
}

/// What an island claimed, and what its content returned.
pub(crate) struct IslandResponse<R> {
    /// What the island claimed, margins included, clipped to what it was
    /// allowed to occupy.
    pub(crate) rect: egui::Rect,
    /// The regions it drew itself, for [`chrome::paint_regions`]: the panel
    /// while it is open, one strip per section once it is shut - and, for a
    /// [`Island::bare`] column that is open, nothing at all, since the panes
    /// inside it are the caller's own regions.
    pub(crate) regions: Vec<egui::Rect>,
    /// The seam that resizes it, ready for [`chrome::paint_grips`].
    pub(crate) grip: chrome::Grip,
    /// What the content returned, or `None` while the island is shut.
    pub(crate) inner: Option<R>,
}

impl Island {
    /// An island on `side`, titled `title` when closed. `id` is the panel's
    /// own id: it keys the persisted width, the open flag and the grip alike,
    /// so it has to stay stable across frames. A column of several panes names
    /// the rest with [`Self::section`].
    pub(crate) fn new(id: &'static str, side: Side, title: impl Into<String>) -> Self {
        Self {
            id,
            side,
            titles: vec![title.into()],
            default_width: 260.0,
            min_width: 140.0,
            max_width: None,
            fill: None,
            flush: false,
            bare: false,
            open_by_default: true,
        }
    }

    /// The width the island opens at before the user has dragged it.
    pub(crate) fn default_width(mut self, width: f32) -> Self {
        self.default_width = width;
        self
    }

    /// The width the island stops shrinking at - and so the width dragging
    /// past closes it.
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

    /// Name another pane, below the ones named already: shut, the spine is one
    /// strip per section, top to bottom in the order they were given.
    pub(crate) fn section(mut self, title: impl Into<String>) -> Self {
        self.titles.push(title.into());
        self
    }

    /// Hand the content the region edge to edge, for an island filled by a
    /// table that draws its own heading and banding.
    pub(crate) fn flush(mut self) -> Self {
        self.flush = true;
        self
    }

    /// Draw no frame of the island's own while it is open: for a column that
    /// arranges regions of its own rather than being one. Shut, the spine is a
    /// region like any other, so the caller rounds off whichever it drew - see
    /// [`IslandResponse::inner`].
    pub(crate) fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// Whether the island starts open, the first time it is ever drawn.
    pub(crate) fn open_by_default(mut self, open: bool) -> Self {
        self.open_by_default = open;
        self
    }

    /// Draw the island. `content` runs only while it is open, and receives the
    /// rect it has to work in.
    pub(crate) fn show<R>(self, ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui, egui::Rect) -> R) -> IslandResponse<R> {
        let mut frame = chrome::region_frame(ui);
        if let Some(fill) = self.fill {
            frame = frame.fill(fill);
        }
        let separator = chrome::show_separator_line(ui);
        let mut expanded = self
            .side
            .panel(self.id)
            .resizable(true)
            .default_size(self.default_width)
            .min_size(self.min_width)
            .show_separator_line(separator)
            .frame(match (self.bare, self.flush) {
                (true, _) => egui::Frame::NONE,
                (false, true) => frame.inner_margin(egui::Margin::ZERO),
                (false, false) => frame,
            });
        if let Some(max) = self.max_width {
            expanded = expanded.max_size(max);
        }
        // The spine is a panel of its own, capped tight so a short pull
        // outward is enough to swap the island back in for it. It carries no
        // frame: the strips inside it are the regions.
        let collapsed = self
            .side
            .panel(egui::Id::new((self.id, "island_spine")))
            .resizable(true)
            .exact_size(SPINE_WIDTH)
            .show_separator_line(separator)
            .frame(egui::Frame::NONE);

        // What the island may occupy. Mid-slide the panel is translated out
        // past its own fixed edge and draws clipped to the part still in here,
        // so this is what its region amounts to - see the response below.
        let available = ui.available_rect_before_wrap();
        let open_id = egui::Id::new(("island_open", self.id));
        let mut open = ui.data(|data| data.get_temp::<bool>(open_id)).unwrap_or(self.open_by_default);
        let titles = self.titles;
        let bare = self.bare;
        // Shut, a strip stands in for a pane, so it takes the surface the
        // panes themselves paint - see `tree_row_colors`. Left on the panel
        // fill it would read as a lighter bar between its neighbours rather
        // than as the same material closed.
        let strip = frame
            .inner_margin(egui::Margin::ZERO)
            .fill(self.fill.unwrap_or_else(|| crate::ui::widgets::tree_row_colors(ui).1));
        let mut inner = None;
        let mut spine = Vec::new();
        let response = egui::Panel::show_switched(ui, &mut open, collapsed, expanded, |ui, expanded| {
            let rect = ui.available_rect_before_wrap();
            ui.set_clip_rect(ui.clip_rect().intersect(rect));
            if expanded {
                // Content that measures itself must not push the panel wider
                // than the user dragged it.
                ui.set_max_width(ui.available_width());
                if bare {
                    // A pane of a column is a panel of its own, and a nested
                    // panel widens the clip rect back to its own edges - which
                    // mid-slide are the island's open width, out over the
                    // neighbour it is sliding behind. So a column is laid out
                    // in what is actually on screen: its panes compress over
                    // the last frames of the slide instead of sliding out.
                    let visible = visible_part(ui.max_rect(), ui.clip_rect());
                    ui.scope_builder(egui::UiBuilder::new().max_rect(visible), |ui| {
                        inner = Some(content(ui, visible));
                    });
                } else {
                    inner = Some(content(ui, rect));
                }
            } else {
                spine = draw_spine(ui, rect, &titles, strip);
            }
        });
        ui.data_mut(|data| data.insert_temp(open_id, open));
        // The rect the panel reports is the one it was translated to, which
        // during a slide reaches over the neighbour it is sliding behind. The
        // chrome paints into the background layer, where nothing clips it back
        // - so it is the visible part, and only ever that, that is the region.
        let rect = visible_part(response.response.rect, available);
        // The tail of a slide carries the panel's rect past the edge it was
        // claimed against, and egui walks the parent's cursor back with it -
        // so whatever is drawn next starts painting from inside the island's
        // neighbour, at the width the island had open. A spacer claims that
        // back. Always shown, at nothing when there is nothing to claim: a
        // panel that is not shown is one child fewer on the parent, and every
        // panel after it would take a different auto id.
        let overshoot = match self.side {
            Side::Left => rect.right() - ui.cursor().left(),
            Side::Right => ui.cursor().right() - rect.left(),
        };
        self.side
            .panel(egui::Id::new((self.id, "island_overshoot")))
            .exact_size(overshoot.max(0.0))
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |_| {});
        let regions = match (inner.is_some(), self.bare) {
            // Shut: the strips of the spine, whatever it was split into.
            (false, _) => spine.into_iter().map(|strip| strip.intersect(rect)).collect(),
            // Open: the panel itself, unless its panes are the caller's.
            (true, false) => vec![rect],
            (true, true) => Vec::new(),
        };
        IslandResponse {
            rect,
            regions,
            grip: chrome::Grip::new(rect, self.side.seam(), self.id),
            inner,
        }
    }
}

/// The part of `rect` that `clip` leaves on screen, never inverted.
///
/// The tail of a slide takes the panel's rect right past the clip, and a plain
/// intersection there comes back with its corners crossed over. Laying content
/// out in that walks the parent's cursor backwards, and the workspace beside
/// the island starts painting from inside its neighbour. A rect of no size, at
/// the edge the island is sliding away from, leaves the cursor where it
/// belongs.
fn visible_part(rect: egui::Rect, clip: egui::Rect) -> egui::Rect {
    let min = rect.min.max(clip.min);
    egui::Rect::from_min_max(min, rect.max.min(clip.max).max(min))
}

/// The closed island: one strip of panel per section, stacked in the order
/// they were named and splitting the height evenly, each with its title
/// reading up it - so a shut island still says what pulling it open would
/// show, and a column of panes still reads as a column.
///
/// Painted rather than laid out in panels of its own. A nested panel widens
/// the clip rect back to its own edges, and mid-slide those edges are the
/// island's open width: the strips would paint out over the neighbour the
/// island is sliding behind. The parent's painter is already clipped to the
/// part of the island still on screen, so painting through it stays inside.
///
/// Returns what each strip claimed, margins included, for the chrome to round
/// off.
fn draw_spine(ui: &egui::Ui, rect: egui::Rect, titles: &[String], frame: egui::Frame) -> Vec<egui::Rect> {
    let height = rect.height() / titles.len() as f32;
    titles
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let top = rect.top() + height * index as f32;
            let claimed = egui::Rect::from_min_max(egui::pos2(rect.left(), top), egui::pos2(rect.right(), top + height));
            // The frame's outer margin is the gap that parts one strip from
            // the next, so the surface itself stops short of it.
            let surface = claimed - frame.outer_margin;
            if surface.is_positive() {
                ui.painter().add(frame.paint(surface));
                draw_spine_title(ui, surface, title);
            }
            claimed
        })
        .collect()
}

/// One section's title, reading up its strip.
fn draw_spine_title(ui: &egui::Ui, rect: egui::Rect, title: &str) {
    let color = ui.visuals().text_color();
    let galley = ui.painter().layout_no_wrap(title.to_owned(), crate::ui::fonts::bold_font(SPINE_TEXT_SIZE), color);
    // Rotated a quarter turn, so the label reads up the strip from its foot.
    let text = egui::epaint::TextShape::new(
        egui::pos2(rect.center().x - galley.size().y * 0.5, rect.top() + SPINE_TEXT_INSET + galley.size().x),
        galley,
        color,
    )
    .with_angle(-std::f32::consts::FRAC_PI_2);
    // Narrowing only: the painter is already clipped to what of the island is
    // on screen, and the title must not reach past that either.
    ui.painter().with_clip_rect(ui.clip_rect().intersect(rect)).add(text);
}
