//! Bottom status bar: app identity, selected count, cursor coords, FPS counter,
//! and the right-aligned task progress bar.

use thousands::Separable;

use crate::ui::EditorState;

/// Width of the status-bar progress bar, in points.
const BAR_WIDTH: f32 = 270.0;
/// Fraction of the bar covered by the indeterminate marquee chunk.
const MARQUEE_FRACTION: f32 = 0.3;
/// Seconds for one full there-and-back sweep of the marquee chunk.
const MARQUEE_PERIOD: f64 = 2.0;
/// Inset of the text from each end of the bar, and the least space kept
/// between the task text and the percentage.
const BAR_TEXT_INSET: f32 = 4.0;
/// Extra padding around a coordinate readout's worst-case value.
const COORD_FIELD_PADDING: f32 = 10.0;
/// Shrink factor on the coordinate fields: the worst-case template is wider
/// than the values normally seen, so the fields don't need its full width.
const COORD_FIELD_SCALE: f32 = 0.9;

/// Filled portion of the bar, for both running and finished tasks.
const BAR_FILL: egui::Color32 = egui::Color32::from_rgb(0x3F, 0xB9, 0x50);
/// Unfilled track in dark mode.
const BAR_TRACK_DARK: egui::Color32 = egui::Color32::from_gray(0xC8);
/// Unfilled track in light mode.
const BAR_TRACK_LIGHT: egui::Color32 = egui::Color32::WHITE;
/// Corner rounding of the bar and its fill.
const BAR_CORNER_RADIUS: u8 = 2;
/// Text colour inside the bar; both the fill and the track are light, so the
/// text is dark in either theme.
const BAR_TEXT: egui::Color32 = egui::Color32::from_gray(0x14);

/// Text style inside the bar: smaller than the readouts, so the bar can be
/// exactly as tall as one of them and no taller.
const BAR_TEXT_STYLE: egui::TextStyle = egui::TextStyle::Small;

/// Footprint of the bar. It is one readout tall, so the panel is the height of
/// its text row whether or not a task is running. Reserved even while the bar
/// is hidden, so the panel keeps one height for the whole session.
fn progress_bar_size(ui: &egui::Ui) -> egui::Vec2 {
    egui::vec2(BAR_WIDTH, ui.text_style_height(&egui::TextStyle::Body))
}

/// How the bar's fill is laid out this frame.
enum BarFill {
    /// Fill from the left to `0.0..=1.0` of the width.
    Fraction(f32),
    /// A chunk sweeping back and forth: a running task with no percentage.
    Marquee,
}

/// Fixed width for each coordinate readout, measured from a worst-case value so
/// the fields (and everything after them) don't shift as the cursor moves.
fn coord_field_width(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap("RL: -8,888,888.88".to_owned(), font, egui::Color32::PLACEHOLDER);
    (galley.size().x + COORD_FIELD_PADDING) * COORD_FIELD_SCALE
}

/// One left-aligned readout occupying a fixed width regardless of its value.
fn coord_field(ui: &mut egui::Ui, width: f32, text: String) {
    let height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let color = ui.visuals().text_color();
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.painter().with_clip_rect(rect).text(rect.left_center(), egui::Align2::LEFT_CENTER, text, font, color);
}

/// Draw the status progress bar. Once the first task has run the bar stays put
/// for the rest of the session, sitting at 100% with the last task's
/// "…: Finished" text.
///
/// Both texts ride inside the bar: `task` against its left end, `status`
/// (percentage and item counts) against its right.
fn draw_progress_bar(ui: &mut egui::Ui, status: &str, task: &str, fill: BarFill) {
    let font = BAR_TEXT_STYLE.resolve(ui.style());
    let (rect, _response) = ui.allocate_exact_size(progress_bar_size(ui), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let track = if ui.visuals().dark_mode { BAR_TRACK_DARK } else { BAR_TRACK_LIGHT };
    let filled = match fill {
        BarFill::Fraction(fraction) => {
            let width = rect.width() * fraction.clamp(0.0, 1.0);
            egui::Rect::from_min_size(rect.min, egui::vec2(width, rect.height()))
        }
        BarFill::Marquee => {
            // egui's `ProgressBar::animate` only shimmers the *filled* part, so a
            // task with no percentage would have to render as a full (i.e. "done"
            // looking) bar to animate at all - hence the hand-painted marquee.
            ui.ctx().request_repaint();
            // Cosine ease so the chunk slows at each end rather than snapping around.
            let time = ui.input(|i| i.time);
            let t = (0.5 - 0.5 * (time * std::f64::consts::TAU / MARQUEE_PERIOD).cos()) as f32;
            let width = rect.width() * MARQUEE_FRACTION;
            let left = egui::lerp(rect.left()..=(rect.right() - width), t);
            egui::Rect::from_min_size(egui::pos2(left, rect.top()), egui::vec2(width, rect.height()))
        }
    };

    let painter = ui.painter();
    painter.rect_filled(rect, BAR_CORNER_RADIUS, track);
    if filled.width() > 0.0 {
        painter.rect_filled(filled, BAR_CORNER_RADIUS, BAR_FILL);
    }

    // The status text keeps its place at the right end; whatever is left of it
    // is the task text's room, clipped so a long label (e.g. a full export
    // path) can't overrun the counts or spill out of the bar.
    let mut task_right = rect.right() - BAR_TEXT_INSET;
    if !status.is_empty() {
        let galley = painter.layout_no_wrap(status.to_owned(), font.clone(), BAR_TEXT);
        let status_left = rect.right() - BAR_TEXT_INSET - galley.size().x;
        painter
            .with_clip_rect(rect)
            .galley(egui::pos2(status_left, rect.center().y - galley.size().y / 2.0), galley, BAR_TEXT);
        task_right = status_left - BAR_TEXT_INSET;
    }
    if !task.is_empty() {
        let clip = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top()), egui::pos2(task_right, rect.bottom()));
        painter
            .with_clip_rect(clip)
            .text(egui::pos2(rect.left() + BAR_TEXT_INSET, rect.center().y), egui::Align2::LEFT_CENTER, task, font, BAR_TEXT);
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

/// Draw the bottom status bar panel.
pub(crate) fn draw_status_bar(ui: &mut egui::Ui, editor: &EditorState) -> egui::Rect {
    egui::Panel::bottom("status_bar")
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{} {}", crate::APP_NAME, crate::APP_RELEASE));
                ui.separator();
                ui.label(format!("Selected: {}", editor.selected_handles.len()));
                ui.separator();
                if editor.frame_counter_enabled {
                    match editor.measured_fps {
                        Some(fps) => ui.label(format!("FPS: {fps:.1}")),
                        None => ui.label("FPS: --"),
                    };
                    ui.separator();
                }
                if editor.debug_chunk_coloring {
                    match editor.debug_chunk_stats {
                        Some((rendered, total)) => ui.label(format!("Chunks: {rendered}/{total} ({} culled)", total.saturating_sub(rendered))),
                        None => ui.label("Chunks: --"),
                    };
                    ui.separator();
                }
                if editor.debug_clip_planes {
                    match editor.debug_clip_plane_distances {
                        Some((near, far)) => ui.label(format!("Clip near/far: {near:.3} / {far:.3} m")),
                        None => ui.label("Clip near/far: -- / --"),
                    };
                    ui.separator();
                }
                let coord_width = coord_field_width(ui);
                match editor.cursor_world {
                    Some(p) => {
                        for (axis, value) in [("X", p.x), ("Y", p.y), ("Z", p.z)] {
                            coord_field(ui, coord_width, format!("{axis}: {}", format!("{value:.2}").separate_with_commas()));
                        }
                    }
                    None => {
                        for axis in ["X", "Y", "Z"] {
                            coord_field(ui, coord_width, format!("{axis}: --"));
                        }
                    }
                }
                // Progress hugs the right edge; everything above keeps its width
                // fixed so the bar doesn't drift while a task runs.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match &editor.status_message {
                        Some(msg) => match msg.progress {
                            Some(p) => {
                                let p = p.clamp(0.0, 1.0);
                                draw_progress_bar(ui, &bar_status_text(p, msg.units), &msg.text, BarFill::Fraction(p));
                            }
                            None => draw_progress_bar(ui, "", &msg.text, BarFill::Marquee),
                        },
                        // Idle: hold the last task at 100%. Before the first task of
                        // the session there is nothing to report, so the bar is
                        // hidden - but its space is still reserved so the panel
                        // doesn't change height when it appears.
                        None => match &editor.last_finished_task {
                            Some(finished) => draw_progress_bar(
                                ui,
                                &bar_status_text(1.0, finished.total_units.map(|total| (total, total))),
                                &format!("{}: Finished", finished.text),
                                BarFill::Fraction(1.0),
                            ),
                            None => {
                                ui.allocate_exact_size(progress_bar_size(ui), egui::Sense::hover());
                            }
                        },
                    }
                });
            });
        })
        .response
        .rect
}
