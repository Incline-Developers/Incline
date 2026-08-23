//! Window chrome: the rounded regions the panels read as, and the gap of
//! window background between them.
//!
//! Panels stay rectangular as far as layout is concerned. The rounding is
//! painted over the finished frame, the way Blender masks its editor corners
//! (`screen_draw_edges` in `screen_draw.cc`): the region's own fill is drawn
//! rounded, then a ring of window background is stroked around the outside of
//! that shape afterwards. Anything a child painted into the corners - a nested
//! panel's fill, the tree's row banding, the 3D scene itself - is cut back to
//! the same radius without any of it having to know the shape.
//!
//! The menu bar and the status bar are deliberately not regions: they span the
//! window and stay square, as Blender's global top bar and status bar do.
//!
//! A new top-level panel joins the look in two steps: take [`region_frame`]
//! and hand its rect to the [`paint_regions`] call at the end of `draw_ui`.
//! Panels nested inside one do neither - the region they sit in is what gets
//! rounded. Separator lines are worth turning off on a region: the gap already
//! separates it from its neighbour, and the mask covers where the line lands.

use egui::{Color32, Rect, Shape, Stroke, StrokeKind};

/// Corner radius of a region: the toolbar tiles' rounding, so the panels and
/// the tools floating over the scene read as one family. Blender rounds its
/// editors harder (`EDITORRADIUS` is 6), but it has nothing else on screen to
/// agree with.
const REGION_RADIUS: u8 = crate::ui::widgets::toolbar::GROUP_CORNER_RADIUS;

/// Gap each region leaves around its own fill.
///
/// Two neighbours therefore sit `2 * REGION_MARGIN` apart, and [`region_area`]
/// insets the window edge by the same amount, so every gap in the window is
/// one width.
const REGION_MARGIN: i8 = 3;

/// Width of the ring painted around a region to cut its corners back to
/// [`REGION_RADIUS`].
///
/// Wide enough to reach the seam: a square corner only overshoots the arc by
/// `radius * (sqrt(2) - 1)`, but the ring also has to paint out the highlight
/// line egui draws along a panel's edge while it is hovered or being dragged,
/// which lands just past the region's own margin. The grips say that instead.
/// The far side of the seam is the neighbour's margin, so a hair over the
/// margin covers the line without ever reaching the neighbour's fill.
const MASK_WIDTH: f32 = REGION_MARGIN as f32 + 0.5;

/// Width of the band painted along a region's claimed edge.
///
/// egui draws a highlight line down a panel's edge while it is hovered or
/// dragged, and the mask alone leaves its ends showing past the region's
/// rounded corners. This covers the line over its whole length; centred on the
/// seam, it still stops short of the neighbouring region's fill.
const SEAM_BAND_WIDTH: f32 = 5.0;

/// Radius of one dot in a resize grip.
const GRIP_DOT_RADIUS: f32 = 1.25;
/// Distance between the centres of a grip's dots.
const GRIP_DOT_SPACING: f32 = 5.0;

/// Colour of the gap between regions: the window showing through behind them.
///
/// A step below the panel surface in either theme, the way the tree's banding
/// and the properties tab column step away from it - see
/// `widgets::tree_row_colors`. Deeper than either, because this is the
/// backdrop the whole layout sits on rather than one surface beside another.
fn window_fill(visuals: &egui::Visuals) -> Color32 {
    crate::ui::widgets::shifted(visuals.panel_fill, if visuals.dark_mode { -13 } else { -22 })
}

/// Hairline around a region, separating its fill from the gap.
///
/// The console fills itself with the extreme background, which in the dark
/// theme is nearly the gap's own colour; without this the region would have no
/// visible edge at all.
fn region_stroke(visuals: &egui::Visuals) -> Stroke {
    let color = crate::ui::widgets::shifted(visuals.panel_fill, if visuals.dark_mode { 14 } else { -34 });
    Stroke::new(1.0, color)
}

/// Frame for a top-level region: the default panel frame, rounded and inset by
/// the gap.
///
/// Nested panels inside a region take plain frames; only the outermost frame
/// is rounded, and [`paint_regions`] cuts back whatever the children paint
/// over its corners.
pub(crate) fn region_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::side_top_panel(style).corner_radius(REGION_RADIUS).outer_margin(REGION_MARGIN)
}

/// Claim half a gap of window background around whatever `ui` lays out next.
///
/// Called once before the regions, so the window edge reads the same as the
/// seam between two of them, and once after, for the scene's own margin.
///
/// Panels rather than a margin, because egui works out whether the pointer is
/// over the interface from the rect the panels leave the *root* ui
/// (`Context::is_pointer_over_egui`). Space the chrome takes has to be claimed
/// the way a panel claims it, or the scene keeps taking scrolls and clicks
/// meant for the gap - and `salt` keeps the two calls' panel ids apart.
pub(crate) fn claim_gap(ui: &mut egui::Ui, salt: &str) {
    // Not resizable, and no separator: a spacer is chrome, not something the
    // user can grab.
    let spacer = |panel: egui::Panel| {
        panel
            .exact_size(f32::from(REGION_MARGIN))
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
    };
    spacer(egui::Panel::top(egui::Id::new((salt, "top")))).show(ui, |_| {});
    spacer(egui::Panel::bottom(egui::Id::new((salt, "bottom")))).show(ui, |_| {});
    spacer(egui::Panel::left(egui::Id::new((salt, "left")))).show(ui, |_| {});
    spacer(egui::Panel::right(egui::Id::new((salt, "right")))).show(ui, |_| {});
}

/// The rect a region's fill occupies, given the rect its panel claimed.
///
/// A panel's rect covers the outer margin its frame reserved, so the visible
/// surface is that much smaller on every side, and the claimed edge itself is
/// the seam its neighbour's margin ends at.
pub(crate) fn region_rect(claimed: Rect) -> Rect {
    claimed.shrink(f32::from(REGION_MARGIN))
}

/// Window background for everything outside the scene.
///
/// The 3D scene is drawn under egui across the whole window, so the gaps
/// between regions would show it through. This covers the window with the gap
/// colour and leaves only `scene_rect` open; the regions are painted over it.
///
/// The shape belongs at the very back of the frame, before any panel is drawn,
/// but `scene_rect` is only known once they all have - reserve an index with
/// `painter.add(Shape::Noop)` on the background layer first and hand it back
/// here.
pub(crate) fn paint_window_background(ctx: &egui::Context, index: egui::layers::ShapeIdx, scene_rect: Rect) {
    let window = ctx.viewport_rect();
    let fill = window_fill(&ctx.global_style().visuals);
    let bands = [
        Rect::from_min_max(window.min, egui::pos2(window.right(), scene_rect.top())),
        Rect::from_min_max(egui::pos2(window.left(), scene_rect.bottom()), window.max),
        Rect::from_min_max(egui::pos2(window.left(), scene_rect.top()), egui::pos2(scene_rect.left(), scene_rect.bottom())),
        Rect::from_min_max(egui::pos2(scene_rect.right(), scene_rect.top()), egui::pos2(window.right(), scene_rect.bottom())),
    ];
    let shapes = bands.into_iter().filter(|band| band.is_positive()).map(|band| Shape::rect_filled(band, 0, fill)).collect();
    ctx.layer_painter(egui::LayerId::background()).set(index, Shape::Vec(shapes));
}

/// The edge of a panel the user drags to resize it.
#[derive(Clone, Copy)]
pub(crate) enum Edge {
    Right,
    Top,
}

/// A draggable seam between two regions, marked with three dots.
///
/// `claimed` is the rect the drag resizes, as its panel claimed it - so its
/// named edge *is* the seam. That is not always one region: the explorer's
/// column is dragged as a whole, so its grip is centred on the tree and the
/// properties panel together.
#[derive(Clone, Copy)]
pub(crate) struct Grip {
    claimed: Rect,
    edge: Edge,
    /// The panel that owns the drag, so the grip can light up from egui's own
    /// interaction state rather than guessing at the pointer.
    panel: egui::Id,
}

impl Grip {
    pub(crate) fn new(claimed: Rect, edge: Edge, panel: impl Into<egui::Id>) -> Self {
        Self {
            claimed,
            edge,
            panel: panel.into(),
        }
    }
}

/// Round off every region, over the top of the finished frame.
///
/// `regions` are the rects the panels claimed, margins included: see
/// [`region_rect`]. Painting into the background layer keeps the masks above
/// the panels and the scene overlays, which are drawn there too, but below
/// menus, tooltips and the floating dialogs, which are areas of their own and
/// should be free to sit wherever they open.
pub(crate) fn paint_regions(ctx: &egui::Context, regions: impl IntoIterator<Item = Rect>) {
    let visuals = &ctx.global_style().visuals;
    let fill = window_fill(visuals);
    let mask = Stroke::new(MASK_WIDTH, fill);
    let band = Stroke::new(SEAM_BAND_WIDTH, fill);
    let outline = region_stroke(visuals);
    let painter = ctx.layer_painter(egui::LayerId::background());
    for claimed in regions {
        let region = region_rect(claimed);
        if !region.is_positive() {
            continue;
        }
        // The band runs along the seam itself, the mask hugs the fill: between
        // them the whole margin is covered, corners and line ends included.
        painter.rect_stroke(claimed, 0, band, StrokeKind::Middle);
        painter.rect_stroke(region, REGION_RADIUS, mask, StrokeKind::Outside);
        painter.rect_stroke(region, REGION_RADIUS, outline, StrokeKind::Inside);
    }
}

/// Mark every draggable seam with three dots, the way a drag handle is spelled
/// everywhere else.
///
/// They sit in the gap rather than on either region, centred on the seam the
/// pointer has to hit, and brighten while it is hovered or being dragged - the
/// only feedback the seam gives, since [`paint_regions`] paints out the line
/// egui would draw there.
///
/// Call after [`paint_regions`]: a region's mask and band both reach across
/// the seam, so dots painted any earlier would be cut in half.
pub(crate) fn paint_grips(ctx: &egui::Context, grips: impl IntoIterator<Item = Grip>) {
    let (resting, active) = grip_colors(&ctx.global_style().visuals);
    let painter = ctx.layer_painter(egui::LayerId::background());
    for grip in grips {
        if !grip.claimed.is_positive() {
            continue;
        }
        // egui names a panel's resize widget after the panel itself, and that
        // widget is what the user actually grabs - so its response is the
        // truth about whether this seam is live, clamped drags included.
        let resize = ctx.read_response(grip.panel.with("__resize"));
        let live = resize.is_some_and(|response| response.hovered() || response.dragged());
        let color = if live { active } else { resting };
        let (center, along) = match grip.edge {
            Edge::Right => (egui::pos2(grip.claimed.right(), grip.claimed.center().y), egui::vec2(0.0, GRIP_DOT_SPACING)),
            Edge::Top => (egui::pos2(grip.claimed.center().x, grip.claimed.top()), egui::vec2(GRIP_DOT_SPACING, 0.0)),
        };
        for step in -1..=1 {
            painter.circle_filled(center + along * step as f32, GRIP_DOT_RADIUS, color);
        }
    }
}

/// Resting and live colours for a resize grip's dots.
///
/// The resting dots have to carry against the gap without becoming furniture
/// of their own; the live ones say the seam under the cursor is the one that
/// will move.
fn grip_colors(visuals: &egui::Visuals) -> (Color32, Color32) {
    let step = |levels: i16| crate::ui::widgets::shifted(visuals.panel_fill, levels);
    if visuals.dark_mode { (step(48), step(110)) } else { (step(-80), step(-150)) }
}
