//! Structured activity console.

use std::{borrow::Cow, collections::HashSet, ops::Range, sync::Arc};

use crate::{
    logging::{ConsoleEntry, ConsoleEntryId, ConsoleEntryState, ConsoleSeverity},
    ui::{themed_icon, unthemed_icon},
};

#[derive(Clone, Default)]
struct ConsoleUiState {
    expanded: HashSet<ConsoleEntryId>,
    pending_toggle: Option<(ConsoleEntryId, u64)>,
    presentation_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentationRow {
    Header {
        entry_index: usize,
    },
    Detail {
        entry_index: usize,
        detail_index: usize,
        byte_range: (usize, usize),
    },
}

#[derive(Clone, Default)]
struct ConsolePresentationCache {
    log_revision: u64,
    presentation_revision: u64,
    columns: usize,
    rows: Arc<[PresentationRow]>,
}

/// Draw the console content, filling the remaining UI area.
///
/// Returns the panel's bounding rect.
pub(crate) fn draw_console(ui: &mut egui::Ui, max_height: f32, snapshot: &crate::logging::ConsoleSnapshot) -> egui::Rect {
    let (log_revision, entries) = snapshot;
    let surface_fill = ui.visuals().extreme_bg_color;
    egui::Panel::bottom("Console")
        .resizable(true)
        .max_size(max_height)
        .frame(egui::Frame::new().fill(surface_fill))
        .show(ui, |ui| {
            let contents_id = ui.make_persistent_id("console_contents");
            ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                let state_id = ui.make_persistent_id("console_ui_state");
                let mut state = ui.ctx().data_mut(|data| data.get_temp::<ConsoleUiState>(state_id).unwrap_or_default());
                let expanded_this_frame = apply_pending_toggle(&mut state, ui.ctx().cumulative_frame_nr()) == Some(true);

                let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                let row_height = (font_id.size + 8.0).max(22.0);
                let detail_indent = 114.0;
                let detail_width = (ui.available_width() - detail_indent).max(1.0);
                let char_width = ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&font_id, '0')).max(1.0);
                let columns = ((detail_width / char_width).floor() as usize).saturating_sub(1).max(1);

                let cache_id = ui.make_persistent_id("console_presentation_rows");
                let rows = ui.ctx().data_mut(|data| {
                    let cache = data.get_temp_mut_or_default::<ConsolePresentationCache>(cache_id);
                    if cache.log_revision != *log_revision || cache.presentation_revision != state.presentation_revision || cache.columns != columns {
                        cache.log_revision = *log_revision;
                        cache.presentation_revision = state.presentation_revision;
                        cache.columns = columns;
                        cache.rows = build_presentation_rows(entries, &state, columns).into();
                    }
                    Arc::clone(&cache.rows)
                });

                if rows.is_empty() {
                    ui.add_sized(
                        [ui.available_width(), row_height],
                        egui::Label::new(egui::RichText::new("No console activity yet").weak()).halign(egui::Align::Center),
                    );
                } else {
                    let visual_rows_id = ui.make_persistent_id("console_visual_rows");
                    egui::ScrollArea::vertical()
                        .id_salt("console_scroll_area")
                        .auto_shrink([false; 2])
                        .stick_to_bottom(!expanded_this_frame)
                        .show_rows(ui, row_height, rows.len(), |ui, visible| {
                            for row in &rows[visible] {
                                let (rect, row_id) = allocate_visual_row(ui, visual_rows_id, row_height);
                                match *row {
                                    PresentationRow::Header { entry_index } => {
                                        draw_header_row(ui, rect, row_id, entry_index, &entries[entry_index], entries, &mut state);
                                    }
                                    PresentationRow::Detail {
                                        entry_index,
                                        detail_index,
                                        byte_range,
                                    } => {
                                        let entry = &entries[entry_index];
                                        let detail = &entry.details[detail_index];
                                        draw_detail_row(ui, rect, row_id, entry_index, &detail[byte_range.0..byte_range.1], entry, entries, detail_indent, &font_id);
                                    }
                                }
                            }
                        });
                }

                ui.ctx().data_mut(|data| data.insert_temp(state_id, state));
            });
        })
        .response
        .rect
}

/// Apply a queued disclosure change after the click frame has completed.
///
/// `Some(true)` means an entry opened, `Some(false)` means one closed, and
/// `None` means no change was ready for this frame.
fn apply_pending_toggle(state: &mut ConsoleUiState, frame_nr: u64) -> Option<bool> {
    let (entry_id, queued_frame) = state.pending_toggle?;
    if queued_frame >= frame_nr {
        return None;
    }
    state.pending_toggle = None;
    let expanded = if state.expanded.remove(&entry_id) {
        false
    } else {
        state.expanded.insert(entry_id);
        true
    };
    state.presentation_revision = state.presentation_revision.wrapping_add(1);
    Some(expanded)
}

fn visual_row_id(base: egui::Id, row_y: f32) -> egui::Id {
    base.with(row_y.to_bits())
}

fn allocate_visual_row(ui: &mut egui::Ui, base_id: egui::Id, row_height: f32) -> (egui::Rect, egui::Id) {
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), row_height));
    let row_id = visual_row_id(base_id, rect.min.y);
    ui.advance_cursor_after_rect(rect);
    (rect, row_id)
}

fn stripe_fill(ui: &egui::Ui, entry_index: usize) -> egui::Color32 {
    if entry_index % 2 != 1 {
        return egui::Color32::TRANSPARENT;
    }
    let visuals = ui.visuals();
    if visuals.dark_mode {
        visuals.faint_bg_color.gamma_multiply(1.4)
    } else {
        // The light theme's `faint_bg_color` is ~2% black, which vanishes against the
        // console's white surface, so darken the surface itself instead.
        visuals.extreme_bg_color.lerp_to_gamma(egui::Color32::BLACK, 0.045)
    }
}

fn paint_stripe(ui: &egui::Ui, rect: egui::Rect, entry_index: usize) {
    ui.painter().rect_filled(rect, 0.0, stripe_fill(ui, entry_index));
}

fn draw_header_row(ui: &mut egui::Ui, rect: egui::Rect, row_id: egui::Id, entry_index: usize, entry: &ConsoleEntry, entries: &[ConsoleEntry], state: &mut ConsoleUiState) {
    let has_details = !entry.details.is_empty();
    let response = ui.interact(rect, row_id, egui::Sense::click());
    paint_stripe(ui, rect, entry_index);

    let expanded = state.expanded.contains(&entry.id);
    if has_details && response.clicked() {
        state.pending_toggle = Some((entry.id, ui.ctx().cumulative_frame_nr()));
        ui.ctx().request_repaint();
    }

    let weak_color = ui.visuals().weak_text_color();
    let text_color = ui.visuals().text_color();
    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let timestamp_font = egui::FontId::new((body_font.size - 1.0).max(8.0), egui::FontFamily::Monospace);
    let title_font = egui::FontId::new(body_font.size, egui::FontFamily::Name("open_sans_bold".into()));
    let center_y = rect.center().y;

    let mut x = rect.left() + 6.0;
    let disclosure_rect = egui::Rect::from_center_size(egui::pos2(x + 6.0, center_y), egui::vec2(10.0, 10.0));
    if has_details {
        let mut chevron = egui::Image::new(themed_icon!(ui, "console_chevron.svg")).fit_to_exact_size(disclosure_rect.size());
        if expanded {
            chevron = chevron.rotate(std::f32::consts::FRAC_PI_2, egui::Vec2::splat(0.5));
        }
        chevron.paint_at(ui, disclosure_rect);
    }
    x += 18.0;

    let timestamp_rect = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x + 58.0, rect.bottom()));
    ui.painter()
        .with_clip_rect(timestamp_rect)
        .text(egui::pos2(x, center_y), egui::Align2::LEFT_CENTER, &entry.timestamp, timestamp_font, weak_color);
    x += 68.0;

    let severity_rect = egui::Rect::from_center_size(egui::pos2(x + 7.0, center_y), egui::vec2(13.0, 13.0));
    egui::Image::new(severity_icon(entry.severity))
        .fit_to_exact_size(severity_rect.size())
        .paint_at(ui, severity_rect);
    x += 22.0;

    let remaining = (rect.right() - x - 6.0).max(1.0);
    let title_width = ui
        .painter()
        .layout_no_wrap(entry.title.clone(), title_font.clone(), text_color)
        .size()
        .x
        .min(210.0)
        .min(remaining);
    let title_rect = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x + title_width, rect.bottom()));
    ui.painter()
        .with_clip_rect(title_rect)
        .text(egui::pos2(x, center_y), egui::Align2::LEFT_CENTER, &entry.title, title_font, text_color);
    x += title_width + 8.0;

    let summary = if entry.state == ConsoleEntryState::Pending {
        format!("In progress · {}", entry.summary)
    } else {
        entry.summary.clone()
    };
    let summary_rect = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(rect.right() - 6.0, rect.bottom()));
    ui.painter()
        .with_clip_rect(summary_rect)
        .text(egui::pos2(x, center_y), egui::Align2::LEFT_CENTER, &summary, body_font, weak_color);

    copy_context_menu(&response, entry, entries);
}

#[allow(clippy::too_many_arguments)]
fn draw_detail_row(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    row_id: egui::Id,
    entry_index: usize,
    text: &str,
    entry: &ConsoleEntry,
    entries: &[ConsoleEntry],
    indent: f32,
    font_id: &egui::FontId,
) {
    let row_response = ui.interact(rect, row_id, egui::Sense::click());
    paint_stripe(ui, rect, entry_index);
    let detail_rect = egui::Rect::from_min_max(egui::pos2(rect.left() + indent - 8.0, rect.top()), egui::pos2(rect.right(), rect.bottom()));
    ui.painter().rect_filled(detail_rect, 0.0, ui.visuals().faint_bg_color.gamma_multiply(0.22));
    let guide_x = rect.left() + indent - 5.0;
    ui.painter().line_segment(
        [egui::pos2(guide_x, rect.top()), egui::pos2(guide_x, rect.bottom())],
        egui::Stroke::new(1.0, ui.visuals().weak_text_color().gamma_multiply(0.45)),
    );

    let text = if text.contains('\t') { Cow::Owned(expand_tabs(text, 4)) } else { Cow::Borrowed(text) };
    let text_rect = egui::Rect::from_min_max(egui::pos2(rect.left() + indent, rect.top()), egui::pos2(rect.right() - 4.0, rect.bottom()));
    let text_response = ui
        .scope_builder(
            egui::UiBuilder::new()
                .id(row_id.with("detail_text"))
                .max_rect(text_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(text.as_ref()).font(font_id.clone()).color(ui.visuals().weak_text_color()))
                        .selectable(true)
                        .halign(egui::Align::Min)
                        .truncate(),
                )
            },
        )
        .inner;
    let response = row_response.union(text_response);
    copy_context_menu(&response, entry, entries);
}

fn copy_context_menu(response: &egui::Response, entry: &ConsoleEntry, entries: &[ConsoleEntry]) {
    response.context_menu(|ui| {
        if ui.button("Copy message").clicked() {
            ui.ctx().copy_text(console_entry_text(entry));
            ui.close();
        }
        if ui.button("Copy all").clicked() {
            ui.ctx().copy_text(console_text(entries));
            ui.close();
        }
    });
}

fn console_severity_label(entry: &ConsoleEntry) -> &'static str {
    match (entry.state, entry.severity) {
        (ConsoleEntryState::Pending, _) => "PENDING",
        (_, ConsoleSeverity::Info) => "INFO",
        (_, ConsoleSeverity::Success) => "SUCCESS",
        (_, ConsoleSeverity::Warn) => "WARN",
        (_, ConsoleSeverity::Error) => "ERROR",
    }
}

fn append_console_entry(text: &mut String, entry: &ConsoleEntry) {
    use std::fmt::Write as _;

    let _ = writeln!(text, "[{}] [{}] {}: {}", entry.timestamp, console_severity_label(entry), entry.title, entry.summary);
    for detail in &entry.details {
        for line in detail.lines() {
            let _ = writeln!(text, "  {line}");
        }
    }
}

fn console_entry_text(entry: &ConsoleEntry) -> String {
    let mut text = String::new();
    append_console_entry(&mut text, entry);
    text
}

fn console_text(entries: &[ConsoleEntry]) -> String {
    let mut text = String::new();
    for entry in entries {
        append_console_entry(&mut text, entry);
    }
    text
}

fn severity_icon(severity: ConsoleSeverity) -> egui::ImageSource<'static> {
    match severity {
        ConsoleSeverity::Info => unthemed_icon!("console_info.svg"),
        ConsoleSeverity::Success => unthemed_icon!("console_success.svg"),
        ConsoleSeverity::Warn => unthemed_icon!("console_warn.svg"),
        ConsoleSeverity::Error => unthemed_icon!("console_error.svg"),
    }
}

fn build_presentation_rows(entries: &[ConsoleEntry], state: &ConsoleUiState, columns: usize) -> Vec<PresentationRow> {
    let mut rows = Vec::new();
    for (entry_index, entry) in entries.iter().enumerate() {
        rows.push(PresentationRow::Header { entry_index });
        if !state.expanded.contains(&entry.id) {
            continue;
        }
        for (detail_index, detail) in entry.details.iter().enumerate() {
            rows.extend(wrapped_row_ranges(detail, columns).map(|range| PresentationRow::Detail {
                entry_index,
                detail_index,
                byte_range: (range.start, range.end),
            }));
        }
    }
    rows
}

/// Byte ranges for fixed-column rows. Newlines always start a new row, tabs
/// advance to four-column stops, and non-ASCII glyphs conservatively reserve
/// two monospace cells. Every returned boundary is a UTF-8 character boundary.
fn wrapped_row_ranges(text: &str, columns: usize) -> impl Iterator<Item = Range<usize>> + '_ {
    let columns = columns.max(1);
    let mut ranges = Vec::new();
    let mut row_start = 0;
    let mut used = 0usize;

    for (byte_index, character) in text.char_indices() {
        if character == '\n' {
            ranges.push(row_start..byte_index);
            row_start = byte_index + character.len_utf8();
            used = 0;
            continue;
        }

        let mut width = if character == '\t' {
            4 - used % 4
        } else if character.is_ascii() {
            1
        } else {
            2
        };
        if used > 0 && used.saturating_add(width) > columns {
            ranges.push(row_start..byte_index);
            row_start = byte_index;
            used = 0;
            if character == '\t' {
                width = 4;
            }
        }
        used = used.saturating_add(width);
    }
    ranges.push(row_start..text.len());
    ranges.into_iter()
}

fn expand_tabs(text: &str, tab_width: usize) -> String {
    let mut output = String::with_capacity(text.len());
    let mut column = 0;
    for character in text.chars() {
        if character == '\t' {
            let spaces = tab_width - column % tab_width;
            output.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            output.push(character);
            column += if character.is_ascii() { 1 } else { 2 };
        }
    }
    output
}
