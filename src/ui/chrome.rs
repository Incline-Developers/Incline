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
//! window and stay square, as Blender's global top bar and status bar do - and
//! they are painted in the gap's own colour rather than the panel surface, so
//! they read as the backdrop the regions are laid on rather than as two more
//! panels floating at the window's edges. See [`window_bar_frame`].
//!
//! A new top-level panel joins the look in two steps: take [`region_frame`]
//! and hand its rect to the [`paint_regions`] call at the end of `draw_ui`.
//! Panels nested inside one do neither - the region they sit in is what gets
//! rounded. A region asks [`show_separator_line`] whether to draw egui's line
//! on its claimed edge rather than suppressing it outright: with the chrome on
//! the gap already separates it from its neighbour and the mask covers where
//! the line lands, but with the chrome off that line is all there is.
//!
//! The whole look is one preference - see [`set_enabled`]. Turned off, every
//! function here degrades to the plain panel it dresses up: no gap, no
//! rounding, no mask, no grips.

use egui::{Color32, Id, Rect, Shape, Stroke, StrokeKind};

/// Corner radius of a region: the tool cells' rounding, so the panels and the
/// buttons on them read as one family. Blender rounds its editors harder
/// (`EDITORRADIUS` is 6), but it has nothing else on screen to agree with.
const REGION_RADIUS: u8 = crate::ui::widgets::toolbar::GROUP_CORNER_RADIUS;

/// Gap each region leaves around its own fill.
///
/// Two neighbours therefore sit `2 * REGION_MARGIN` apart, and [`region_rect`]
/// insets the window edge by the same amount, so every gap in the window is
/// one width. Read it through [`margin`], which is nothing while the chrome is
/// off.
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

/// Where the preference lives between being set and being read.
fn enabled_id() -> Id {
    Id::new("chrome_enabled")
}

/// Turn the chrome on or off for the rest of the frame.
///
/// Called once at the top of `draw_ui`, before any panel is claimed. The
/// panels that sit in a region are drawn from all over the interface and most
/// of them never see `EditorState`, so the preference rides on the context
/// rather than being threaded through every one of them.
pub(crate) fn set_enabled(ctx: &egui::Context, enabled: bool) {
    ctx.data_mut(|data| data.insert_temp(enabled_id(), enabled));
}

/// Whether the chrome is being painted, as [`set_enabled`] last left it.
///
/// On until told otherwise, so a context nobody has set it on - a test, an
/// early frame - looks the way the application does.
pub(crate) fn enabled(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(enabled_id())).unwrap_or(true)
}

/// The gap a region leaves around its fill: [`REGION_MARGIN`], or nothing at
/// all while the chrome is off, which is what collapses the whole layout back
/// to flush panels.
pub(crate) fn margin(ctx: &egui::Context) -> f32 {
    if enabled(ctx) { f32::from(REGION_MARGIN) } else { 0.0 }
}

/// Whether a top-level panel should draw egui's separator line along the edge
/// it claimed.
///
/// Never while the chrome is on: the gap parts the regions, and
/// [`paint_regions`] paints the line out anyway. With the chrome off it is the
/// only thing left between one panel and the next, so it comes back. Nested
/// panels are not regions and pass `false` themselves.
pub(crate) fn show_separator_line(ui: &egui::Ui) -> bool {
    !enabled(ui.ctx())
}

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

/// Fill for a bar that spans the window instead of sitting in a region.
///
/// The gap's colour: the menu bar and the status bar are the window's own
/// background rather than surfaces on it, so a bar and the gap beside it are
/// one continuous field with no seam between them. With the chrome off there
/// is no gap and no backdrop to belong to, so they fall back to the panel
/// surface and are told apart by their separator lines as before.
pub(crate) fn window_bar_fill(ui: &egui::Ui) -> Color32 {
    if enabled(ui.ctx()) { window_fill(ui.visuals()) } else { ui.visuals().panel_fill }
}

/// Frame for such a bar: [`window_bar_fill`], with the panel frame's margins.
///
/// Pair it with [`show_separator_line`], which is what drops the line egui
/// would otherwise draw along the bar's inner edge - with the bar and the gap
/// the same colour, that line is the only thing left saying "panel".
pub(crate) fn window_bar_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::side_top_panel(ui.style()).fill(window_bar_fill(ui))
}

/// Frame for a top-level region: the default panel frame, rounded and inset by
/// the gap.
///
/// Nested panels inside a region take plain frames; only the outermost frame
/// is rounded, and [`paint_regions`] cuts back whatever the children paint
/// over its corners.
pub(crate) fn region_frame(ui: &egui::Ui) -> egui::Frame {
    let frame = egui::Frame::side_top_panel(ui.style());
    if enabled(ui.ctx()) {
        frame.corner_radius(REGION_RADIUS).outer_margin(REGION_MARGIN)
    } else {
        frame
    }
}

/// Which of a gap's four sides actually take space.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gap {
    /// All four, for something parted from a neighbour on every side.
    All,
    /// Left and right only.
    ///
    /// What lies above and below the workspace is the menu bar and the status
    /// bar, and those are painted in the gap's own colour already - see
    /// [`window_bar_frame`]. A spacer against one of them adds nothing to look
    /// at and doubles the distance to the first region, which reads as a band
    /// of dead space rather than a seam.
    Sides,
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
pub(crate) fn claim_gap(ui: &mut egui::Ui, salt: &str, sides: Gap) {
    // Not resizable, and no separator: a spacer is chrome, not something the
    // user can grab. With the chrome off they are shown at zero width rather
    // than skipped, because a panel that is not shown is one child fewer on
    // the root ui - and every panel after it would take a different auto id,
    // losing the sizes the user had dragged.
    let width = margin(ui.ctx());
    // A side that is switched off is still shown, at nothing, for the same
    // reason a gap is with the chrome off.
    let vertical = if sides == Gap::All { width } else { 0.0 };
    let spacer = |size: f32| move |panel: egui::Panel| panel.exact_size(size).resizable(false).show_separator_line(false).frame(egui::Frame::NONE);
    spacer(vertical)(egui::Panel::top(egui::Id::new((salt, "top")))).show(ui, |_| {});
    spacer(vertical)(egui::Panel::bottom(egui::Id::new((salt, "bottom")))).show(ui, |_| {});
    spacer(width)(egui::Panel::left(egui::Id::new((salt, "left")))).show(ui, |_| {});
    spacer(width)(egui::Panel::right(egui::Id::new((salt, "right")))).show(ui, |_| {});
}

/// The rect a region's fill occupies, given the rect its panel claimed.
///
/// A panel's rect covers the outer margin its frame reserved, so the visible
/// surface is that much smaller on every side, and the claimed edge itself is
/// the seam its neighbour's margin ends at.
pub(crate) fn region_rect(ctx: &egui::Context, claimed: Rect) -> Rect {
    claimed.shrink(margin(ctx))
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
    // Nothing shows between flush panels, so the reserved shape stays a no-op.
    if !enabled(ctx) {
        return;
    }
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
    if !enabled(ctx) {
        return;
    }
    let visuals = &ctx.global_style().visuals;
    let painter = ctx.layer_painter(egui::LayerId::background());
    for claimed in regions {
        let region = region_rect(ctx, claimed);
        if !region.is_positive() {
            continue;
        }
        paint_region_chrome(&painter, visuals, region);
    }
}

/// The gap around a region's fill, its corners cut back, and its outline.
fn paint_region_chrome(painter: &egui::Painter, visuals: &egui::Visuals, region: Rect) {
    let fill = window_fill(visuals);
    // The band runs along the seam itself, the mask hugs the fill: between
    // them the whole margin is covered, corners and line ends included.
    // The band's outer edge is square: a panel's neighbour is another panel,
    // so the far side of every seam is covered by a band of its own.
    painter.rect_stroke(region.expand(f32::from(REGION_MARGIN)), 0, Stroke::new(SEAM_BAND_WIDTH, fill), StrokeKind::Middle);
    painter.rect_stroke(region, REGION_RADIUS, Stroke::new(MASK_WIDTH, fill), StrokeKind::Outside);
    painter.rect_stroke(region, REGION_RADIUS, region_stroke(visuals), StrokeKind::Inside);
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
    // With the chrome off there is no gap to sit them in, and the separator
    // lines mark the seams instead.
    if !enabled(ctx) {
        return;
    }
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
