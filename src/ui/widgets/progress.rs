//! The background-task progress bar that sits at the right end of the bottom
//! toolbar.
//!
//! Painted rather than assembled from `egui::ProgressBar`, for three reasons:
//! an indeterminate task has to animate without reading as "done" (egui only
//! shimmers the *filled* part, so a marquee chunk is the only honest shape for
//! it), the fill carries a static diagonal hatch rather than a flat shade,
//! and the task label rides inside the bar, switching colour where it
//! crosses the fill edge instead of being drawn twice over two backgrounds.

use thousands::Separable;

use crate::ui::{EditorState, widgets::shifted};

/// Share of the room left on the strip that the bar takes, so it grows and
/// shrinks with the window rather than with whatever a task happens to be
/// called.
const BAR_WIDTH_FRACTION: f32 = 0.3;
/// Width the bar never drops below, in points. Wide enough that the fill has
/// somewhere to travel and the counts have room.
const BAR_MIN_WIDTH: f32 = 240.0;
/// Width the bar never grows past: past this it stops reading as a readout at
/// the end of the strip and starts reading as half the toolbar.
const BAR_MAX_WIDTH: f32 = 520.0;
/// Below this there is no room for the bar at all, so a cramped toolbar drops
/// it rather than overlapping the tools beside it.
const BAR_HIDE_WIDTH: f32 = 96.0;
/// Space left above and below the bar. Everything else the strip has is the
/// bar's: it is a readout, and the room it leaves is only what keeps it off
/// the edges of its own panel.
const BAR_VERTICAL_MARGIN: f32 = 1.0;
/// Range the bar's height is held to, whatever the strip gives it.
const BAR_HEIGHT_RANGE: std::ops::RangeInclusive<f32> = 16.0..=32.0;
/// Corner rounding of the bar: the one radius everything in the window is
/// rounded to - the panels' regions, the toolbar tiles, the buttons.
const BAR_CORNER_RADIUS: f32 = crate::ui::widgets::toolbar::GROUP_CORNER_RADIUS as f32;
/// Width of the ring of toolbar surface painted around the bar to cut its
/// corners back, the way [`crate::ui::chrome`] finishes off a region: the fill
/// and the stripes are clipped to a rectangle, which a square corner overshoots
/// by `radius * (sqrt(2) - 1)`.
const BAR_MASK_WIDTH: f32 = 2.0;
/// Inset of the text from each end of the bar, and the least space kept
/// between the task text and the percentage.
const BAR_TEXT_INSET: f32 = 6.0;

/// Fraction of the bar covered by the indeterminate marquee chunk.
const MARQUEE_FRACTION: f32 = 0.3;
/// Seconds for one full there-and-back sweep of the marquee chunk.
const MARQUEE_PERIOD: f64 = 2.0;

/// Spacing between the diagonal stripes over the fill, in points.
const STRIPE_PERIOD: f32 = 14.0;
/// Width of one stripe, as a fraction of the spacing. A half means a stripe
/// and the gap after it are the same width.
const STRIPE_DUTY: f32 = 0.5;
/// Opacity of a stripe over the fill. Barely there: the stripes are meant to
/// give the fill some texture, not to read as a pattern in their own right.
const STRIPE_ALPHA: u8 = 14;

/// Every shade in the bar is the toolbar's own grey, moved this many levels
/// off it: the empty track, the line around it, the fill, and the leading edge
/// of a partial fill. The bar is a readout in the corner of the toolbar rather
/// than something to look at, so the whole widget stays within a narrow band
/// of the surface it sits on - it should be findable, not conspicuous.
const TRACK_SHIFT_DARK: i16 = -8;
const TRACK_SHIFT_LIGHT: i16 = -12;
const TRACK_STROKE_SHIFT_DARK: i16 = -16;
const TRACK_STROKE_SHIFT_LIGHT: i16 = -24;
/// The fill, top and bottom of its gradient. A handful of levels the other
/// side of the surface from the track, so it separates from the track without
/// either of them separating from the toolbar.
const FILL_SHIFT_DARK: (i16, i16) = (4, -2);
const FILL_SHIFT_LIGHT: (i16, i16) = (-24, -30);
/// The leading edge of a partial fill, so the boundary the bar is advancing
/// reads as an edge rather than as where the shade stops. One step past the
/// fill, not a highlight.
const EDGE_SHIFT_DARK: i16 = 14;
const EDGE_SHIFT_LIGHT: i16 = -42;

/// Text style inside the bar: the same as the status bar's readouts, which is
/// what the bar reads as now that it is as tall as the strip it sits in.
const BAR_TEXT_STYLE: egui::TextStyle = egui::TextStyle::Body;

/// How the bar's fill is laid out this frame.
enum BarFill {
    /// Fill from the left to `0.0..=1.0` of the width.
    Fraction(f32),
    /// A chunk sweeping back and forth: a running task with no percentage.
    Marquee,
}

/// Draw the task progress bar for whatever the editor is reporting.
///
/// Once the first task of the session has run the bar stays put, sitting at
/// 100% with the last task's "…: Finished" text; before then there is nothing
/// to report and nothing is drawn. Unlike the old status-bar slot the toolbar
/// has a fixed height, so the empty case needn't reserve any space.
pub(crate) fn draw_task_progress(ui: &mut egui::Ui, editor: &EditorState) {
    match &editor.status_message {
        Some(message) => match message.progress {
            Some(progress) => {
                let progress = progress.clamp(0.0, 1.0);
                draw_bar(ui, &bar_status_text(progress, message.units), &message.text, BarFill::Fraction(progress));
            }
            None => draw_bar(ui, "", &message.text, BarFill::Marquee),
        },
        // Idle: hold the last task at 100%. Nothing about the parked bar
        // moves, so it costs no repaints.
        None => {
            if let Some(finished) = &editor.last_finished_task {
                let status = bar_status_text(1.0, finished.total_units.map(|total| (total, total)));
                draw_bar(ui, &status, &format!("{}: Finished", finished.text), BarFill::Fraction(1.0));
            }
        }
    }
}

/// Right-hand end of the bar: percentage, plus item counts when the task
/// reports them.
fn bar_status_text(fraction: f32, units: Option<(u64, u64)>) -> String {
    let percent = format!("{:.0}%", fraction * 100.0);
    match units {
        Some((done, total)) => format!("{percent} ({} of {})", done.separate_with_commas(), total.separate_with_commas()),
        None => percent,
    }
}

/// The room the bar asks for: a share of what the strip has left, held between
/// [`BAR_MIN_WIDTH`] and [`BAR_MAX_WIDTH`].
///
/// Sized from the window rather than from the label, so the bar keeps one
/// width across a session's tasks and only moves when the window or the panels
/// beside it do. What the strip has left is already the toolbar minus the
/// tools, so the bar cannot crowd them however wide the window gets.
///
/// It is one row of the strip tall, less the margin that keeps it off the
/// panel's edges.
fn bar_size(ui: &egui::Ui) -> egui::Vec2 {
    let available = ui.available_width();
    let width = (available * BAR_WIDTH_FRACTION).clamp(BAR_MIN_WIDTH, BAR_MAX_WIDTH).min(available);
    let height = (ui.available_height() - 2.0 * BAR_VERTICAL_MARGIN).clamp(*BAR_HEIGHT_RANGE.start(), *BAR_HEIGHT_RANGE.end());
    egui::vec2(width, height)
}

/// Paint the bar itself.
///
/// Both texts ride inside it: `task` against its left end, `status`
/// (percentage and item counts) against its right.
fn draw_bar(ui: &mut egui::Ui, status: &str, task: &str, fill: BarFill) {
    let size = bar_size(ui);
    if size.x < BAR_HIDE_WIDTH {
        return;
    }
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    // The label is clipped to whatever the counts leave it, so a long one - a
    // full export path, say - is only readable on hover.
    response.on_hover_text(task);

    let visuals = ui.visuals();
    let surface = visuals.panel_fill;
    let track = shifted(surface, if visuals.dark_mode { TRACK_SHIFT_DARK } else { TRACK_SHIFT_LIGHT });
    let track_stroke = shifted(surface, if visuals.dark_mode { TRACK_STROKE_SHIFT_DARK } else { TRACK_STROKE_SHIFT_LIGHT });
    let text_on_track = visuals.text_color();
    // The fill is the panel's own grey, a few levels off the track. Over
    // something that close the text only needs to firm up, never invert.
    let (top_shift, bottom_shift) = if visuals.dark_mode { FILL_SHIFT_DARK } else { FILL_SHIFT_LIGHT };
    let fill_colors = (shifted(surface, top_shift), shifted(surface, bottom_shift));
    let fill_edge = shifted(surface, if visuals.dark_mode { EDGE_SHIFT_DARK } else { EDGE_SHIFT_LIGHT });
    let text_on_fill = visuals.strong_text_color();

    let filled = match fill {
        BarFill::Fraction(fraction) => {
            let width = rect.width() * fraction.clamp(0.0, 1.0);
            egui::Rect::from_min_size(rect.min, egui::vec2(width, rect.height()))
        }
        BarFill::Marquee => {
            ui.ctx().request_repaint();
            // Cosine ease so the chunk slows at each end rather than snapping around.
            let time = ui.input(|i| i.time);
            let t = (0.5 - 0.5 * (time * std::f64::consts::TAU / MARQUEE_PERIOD).cos()) as f32;
            let width = rect.width() * MARQUEE_FRACTION;
            let left = egui::lerp(rect.left()..=(rect.right() - width), t);
            egui::Rect::from_min_size(egui::pos2(left, rect.top()), egui::vec2(width, rect.height()))
        }
    };
    let marquee = matches!(fill, BarFill::Marquee);

    let painter = ui.painter().clone();
    painter.rect_filled(rect, BAR_CORNER_RADIUS, track);

    if filled.width() > 0.5 {
        paint_fill(&painter, filled, fill_colors);
        // The stripes are texture on the fill, not an animation: they are
        // pinned to the bar, so a determinate fill uncovers them as it grows
        // and the marquee chunk slides over them. Nothing here asks for a
        // repaint, which is what lets the app go idle once a task ends.
        paint_stripes(&painter.with_clip_rect(filled), rect);
        // Only on a determinate fill, and only where it stops short of the end
        // of the bar: at 100% the edge is the bar's own, and a line down it
        // would read as a seam. The marquee chunk has no advancing boundary to
        // mark, so a line down one of its ends reads as an artefact rather
        // than as an edge.
        if !marquee && filled.right() < rect.right() - 0.5 {
            painter.vline(filled.right() - 0.5, filled.y_range(), egui::Stroke::new(1.0, fill_edge));
        }
    }

    // Cut the square corners of the fill and the stripes back to the bar's
    // rounding, then draw the bar's own outline over the seam that leaves.
    painter.rect_stroke(rect, BAR_CORNER_RADIUS, egui::Stroke::new(BAR_MASK_WIDTH, surface), egui::StrokeKind::Outside);
    painter.rect_stroke(rect, BAR_CORNER_RADIUS, egui::Stroke::new(1.0, track_stroke), egui::StrokeKind::Inside);

    // The status text keeps its place at the right end; whatever is left of it
    // is the task text's room, clipped so a long label can't overrun the
    // counts or spill out of the bar.
    let font = BAR_TEXT_STYLE.resolve(ui.style());
    let mut task_right = rect.right() - BAR_TEXT_INSET;
    if !status.is_empty() {
        let galley = painter.layout_no_wrap(status.to_owned(), font.clone(), egui::Color32::PLACEHOLDER);
        let status_left = rect.right() - BAR_TEXT_INSET - galley.size().x;
        let pos = egui::pos2(status_left, rect.center().y - galley.size().y / 2.0);
        paint_two_tone(&painter, rect, filled, galley, pos, text_on_track, text_on_fill);
        task_right = status_left - BAR_TEXT_INSET;
    }
    if !task.is_empty() {
        // Elided rather than clipped: a label cut mid-glyph reads as a drawing
        // bug, an ellipsis reads as "there is more, hover for it".
        let mut job = egui::text::LayoutJob::simple_singleline(task.to_owned(), font, egui::Color32::PLACEHOLDER);
        job.wrap = egui::text::TextWrapping::truncate_at_width(task_right - rect.left() - BAR_TEXT_INSET);
        let galley = painter.layout_job(job);
        let pos = egui::pos2(rect.left() + BAR_TEXT_INSET, rect.center().y - galley.size().y / 2.0);
        paint_two_tone(&painter, rect, filled, galley, pos, text_on_track, text_on_fill);
    }
}

/// Draw one galley twice - once over the fill and once over the bare track -
/// so a word straddling the fill edge stays legible on both sides of it.
fn paint_two_tone(
    painter: &egui::Painter,
    rect: egui::Rect,
    filled: egui::Rect,
    galley: std::sync::Arc<egui::Galley>,
    pos: egui::Pos2,
    on_track: egui::Color32,
    on_fill: egui::Color32,
) {
    painter.with_clip_rect(painter.clip_rect().intersect(rect)).galley(pos, galley.clone(), on_track);
    painter.with_clip_rect(painter.clip_rect().intersect(filled)).galley(pos, galley, on_fill);
}

/// The fill's vertical gradient over `rect`, from `top` to `bottom`.
fn paint_fill(painter: &egui::Painter, rect: egui::Rect, (top, bottom): (egui::Color32, egui::Color32)) {
    let mut mesh = egui::Mesh::default();
    quad(
        &mut mesh,
        [(rect.left_top(), top), (rect.right_top(), top), (rect.right_bottom(), bottom), (rect.left_bottom(), bottom)],
    );
    painter.add(egui::Shape::mesh(mesh));
}

/// Stripes leaning across the bar at 45 degrees.
///
/// Drawn across the whole bar and left to the caller's clip rect: a stripe
/// overhangs the fill by the bar's height at the top and the bottom, so it
/// can't be laid out against the fill's edges directly.
fn paint_stripes(painter: &egui::Painter, rect: egui::Rect) {
    let lean = rect.height();
    let width = STRIPE_PERIOD * STRIPE_DUTY;
    let color = faded(egui::Color32::WHITE, f32::from(STRIPE_ALPHA) / 255.0);

    let mut mesh = egui::Mesh::default();
    // Start a full stripe's lean to the left of the bar, so the first one to
    // cross into it is already whole.
    let mut left = rect.left() - lean - STRIPE_PERIOD;
    while left < rect.right() + STRIPE_PERIOD {
        quad(
            &mut mesh,
            [
                (egui::pos2(left + lean, rect.top()), color),
                (egui::pos2(left + lean + width, rect.top()), color),
                (egui::pos2(left + width, rect.bottom()), color),
                (egui::pos2(left, rect.bottom()), color),
            ],
        );
        left += STRIPE_PERIOD;
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Append one four-cornered face, its corners given in order around it.
fn quad(mesh: &mut egui::Mesh, corners: [(egui::Pos2, egui::Color32); 4]) {
    let base = mesh.vertices.len() as u32;
    for (pos, color) in corners {
        mesh.colored_vertex(pos, color);
    }
    mesh.add_triangle(base, base + 1, base + 2);
    mesh.add_triangle(base, base + 2, base + 3);
}

/// `color` at `alpha` of its opacity, premultiplied the way a mesh vertex
/// wants it.
fn faded(color: egui::Color32, alpha: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}
