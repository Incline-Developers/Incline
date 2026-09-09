//! The "Edit Object" dialog: spreadsheet-style editing of one design object.
//!
//! The dialog owns a working copy and never reads or writes the document.
//! Apply/OK hand the copy back as
//! [`crate::ui::state::UiCommand::ApplyObjectEdit`], one `Command::Replace`
//! per Apply. Layer name and colour are seeded from the active project at
//! open; `App::refresh_object_edit_dialog` notices the object going away.

use crate::{
    i18n::{tr, tr_format},
    model::{
        FillStyle, Object, ObjectColor, ObjectId,
        object_edit::{
            CircleSpec, ObjectEditIssue, bulge_for_radius, bulge_radius, bulge_to_sweep_degrees, chord_length_xy, compact_circle, delete_vertex, insert_vertex_after, move_vertex,
            polyline_area_xy, polyline_length, reverse_vertices, set_compact_circle, sweep_degrees_to_bulge, validate_object,
        },
    },
    rendering::color::{color32_to_rgba, rgba_to_color32},
    ui::{
        elements::properties::{fill_style_label, read_only_row},
        state::{EditorState, UiCommand},
        widgets::{
            menu::{self, DragableMenu, MenuButton, MenuFieldBool, MenuFieldColor32, MenuFieldCombo, MenuFieldF32, MenuFieldF64, MenuFieldText},
            toolbar::GROUP_CORNER_RADIUS,
        },
    },
};

/// Height of one sheet row, and the height given to the sheet itself.
const ROW_HEIGHT: f32 = 20.0;
const SHEET_HEIGHT: f32 = 210.0;
/// Width of the leading row-number column.
const INDEX_COLUMN: f32 = 34.0;
/// Horizontal gap between sheet cells.
const CELL_SPACING: f32 = 4.0;
/// Narrowest a value cell is allowed to become on a cramped dialog.
const MIN_CELL_WIDTH: f32 = 40.0;

/// egui id of the one cell in text-entry state.
///
/// A single stable id keeps the caret alive across frames and lets a freshly
/// opened cell request focus before the widget that will carry it exists.
fn active_cell_id() -> egui::Id {
    egui::Id::new("object_edit_active_cell")
}

/// Which sheet of the dialog is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ObjectEditTab {
    #[default]
    Properties,
    Vertices,
    Arcs,
}

/// A numeric column of the vertex sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VertexColumn {
    X,
    Y,
    Z,
    Bulge,
}

/// A row button press, applied once the button row has been laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    InsertAfter,
    Delete,
    MoveUp,
    MoveDown,
    Reverse,
}

/// Working state of the "Edit Object" dialog.
///
/// `baseline` records the document state Apply drift-checks against, to
/// catch an edit that landed underneath the dialog while it was open.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObjectEditDialog {
    pub(crate) id: ObjectId,
    /// Working copy. Only written to the document by Apply/OK.
    pub(crate) object: Object,
    /// Document state the working copy was taken from, for drift detection.
    pub(crate) baseline: Object,
    /// Owning layer's name, display only; the dialog cannot change an object's layer.
    pub(crate) layer_name: String,
    /// Owning layer's colour; what "Colour by layer" hands back when the
    /// user turns the toggle off.
    pub(crate) layer_rgba: [f32; 4],
    pub(crate) tab: ObjectEditTab,
    /// Row highlighted in the vertex sheet, and the target of the row buttons.
    pub(crate) selected_row: Option<usize>,
    /// Cell currently being typed into, with its full-precision text buffer.
    pub(crate) editing_cell: Option<(usize, VertexColumn, String)>,
    /// Bumped by every edit; the derived-value and arc-segment caches rebuild
    /// only when it changes, so a 200k-vertex contour isn't rescanned per frame.
    pub(crate) revision: u64,
    /// Inline validation or status message shown above the buttons.
    pub(crate) message: Option<String>,
    /// Cached derived values for `revision`: (length, area).
    pub(crate) derived: Option<(u64, f64, Option<f64>)>,
    /// Cached indices of bulged segments for `revision`.
    pub(crate) arc_rows: Option<(u64, Vec<usize>)>,
    /// Segment the Arc tab is driving, kept listed even once its sweep passes
    /// zero and stops being an arc, so a drag to zero can't delete its own row
    /// (or the whole tab) from under the pointer. Cleared by leaving the tab.
    pub(crate) sticky_arc: Option<usize>,
    /// Cached verdict of [`validate_object`] for `revision`; disables
    /// Apply/OK and drives the inline message while `Some`.
    pub(crate) issue: Option<ObjectEditIssue>,
}

impl ObjectEditDialog {
    pub(crate) fn new(id: ObjectId, object: Object, layer_name: String, layer_rgba: [f32; 4]) -> Self {
        Self {
            id,
            baseline: object.clone(),
            object,
            layer_name,
            layer_rgba,
            tab: ObjectEditTab::default(),
            selected_row: None,
            editing_cell: None,
            revision: 0,
            message: None,
            derived: None,
            arc_rows: None,
            sticky_arc: None,
            issue: None,
        }
    }

    /// Note an edit to the working copy so the cached derived values rebuild.
    pub(crate) fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

/// Wording for a validation failure, shared with the app-side writeback so the
/// inline message and console warning cannot drift apart.
pub(crate) fn issue_message(issue: ObjectEditIssue) -> String {
    match issue {
        ObjectEditIssue::NonFiniteVertex(row) => tr_format!(literal = "Row %row%: position or bulge is not a valid number", row = row + 1),
        ObjectEditIssue::NonFiniteValue => tr!(literal = "One or more properties is not a valid number"),
        ObjectEditIssue::TooFewVertices { required } => tr_format!(literal = "This object needs at least %required% vertices", required = required),
    }
}

/// Draw the "Edit Object" dialog when one is open.
pub(crate) fn draw_object_edit_dialog(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let Some(id) = editor.object_edit_dialog.as_ref().map(|dialog| dialog.id) else {
        return;
    };

    // Escape with a cell open belongs to the cell - `TextEdit` consumes it and
    // the buffer is abandoned. With no cell open it closes the dialog, which
    // is why `EditorState::dialog_owns_cancel_key` names this dialog.
    if editor.object_edit_dialog.as_ref().is_some_and(|dialog| dialog.editing_cell.is_none()) && menu::dialog_cancel_pressed(ui.ctx()) {
        editor.object_edit_dialog = None;
        return;
    }

    let Some(dialog) = editor.object_edit_dialog.as_mut() else {
        return;
    };
    refresh_caches(dialog);

    let mut open = true;
    let mut apply = false;
    let mut confirm = false;
    let mut cancel = false;
    let mut working_copy = None;

    DragableMenu::new("object_edit_dialog", tr!(literal = "Edit Object"))
        .open(&mut open)
        .min_width(440.0)
        .max_width(480.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui.ctx(), |ui| {
            // The Arc tab only makes sense for geometry with actual curvature.
            let arcs_available = dialog.arc_rows.as_ref().is_some_and(|(_, rows)| !rows.is_empty()) || circle_spec(&dialog.object).is_some();
            if !arcs_available && dialog.tab == ObjectEditTab::Arcs {
                dialog.tab = ObjectEditTab::Properties;
            }
            draw_tab_bar(ui, dialog, arcs_available);
            ui.add_space(4.0);

            match dialog.tab {
                ObjectEditTab::Properties => draw_properties_tab(ui, dialog),
                ObjectEditTab::Vertices => draw_vertices_tab(ui, dialog),
                ObjectEditTab::Arcs => draw_arcs_tab(ui, dialog),
            }

            ui.add_space(4.0);
            if let Some(text) = dialog.issue.map(issue_message).or_else(|| dialog.message.clone()) {
                ui.colored_label(ui.visuals().error_fg_color, text);
            }

            // Enter is deliberately not a confirm shortcut - it belongs to the
            // cell being typed into, which commits on it. Escape is handled above.
            let can_apply = dialog.issue.is_none();
            menu::menu_actions(ui, |ui| {
                confirm = ui.add(MenuButton::new(tr!(literal = "OK")).primary().enabled(can_apply)).clicked();
                cancel = ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked();
                apply = ui.add(MenuButton::new(tr!(literal = "Apply")).enabled(can_apply)).clicked();
            });

            if apply || confirm {
                // Clicking a button takes focus off the cell being typed into;
                // commit that value first, then re-check the verdict cached before it.
                if let Some((row, column, buffer)) = dialog.editing_cell.take() {
                    if !commit_cell(dialog, row, column, &buffer) {
                        // Keep the bad text in its cell with the complaint showing,
                        // rather than write the old value and close over it.
                        dialog.editing_cell = Some((row, column, buffer));
                        apply = false;
                        confirm = false;
                    }
                    dialog.issue = validate_object(&dialog.object);
                }
                if (apply || confirm) && dialog.issue.is_none() {
                    working_copy = Some(dialog.object.clone());
                } else {
                    apply = false;
                    confirm = false;
                }
            }
        });

    // Cancel and the title bar's close button both discard the working copy;
    // anything an earlier Apply already wrote stays written as its own undo step.
    if cancel || !open {
        editor.object_edit_dialog = None;
        return;
    }
    // OK closes through the command, after its drift check has run.
    if let Some(object) = working_copy {
        commands.push(UiCommand::ApplyObjectEdit {
            id,
            object: Box::new(object),
            close: confirm,
        });
    }
}

/// Rebuild everything keyed on the working copy's revision, once per edit
/// rather than once per frame: a long contour is scanned only when it changes.
fn refresh_caches(dialog: &mut ObjectEditDialog) {
    if dialog.derived.is_some_and(|(revision, _, _)| revision == dialog.revision) {
        return;
    }
    let (length, area, mut arc_rows) = match &dialog.object {
        Object::Polyline { verts, closed, .. } => {
            let edge_count = if *closed { verts.len() } else { verts.len().saturating_sub(1) };
            let rows = (0..edge_count)
                .filter(|&edge| verts[edge].bulge.is_finite() && verts[edge].bulge.abs() > f64::EPSILON)
                .collect();
            (polyline_length(verts, *closed), polyline_area_xy(verts, *closed), rows)
        }
        Object::Point { .. } | Object::Text { .. } => (0.0, None, Vec::new()),
    };
    // A pinned segment (see `sticky_arc`) is dropped here once row edits put it out of range.
    if let Some(segment) = dialog.sticky_arc {
        let segment_count = match &dialog.object {
            Object::Polyline { verts, closed, .. } => {
                if *closed {
                    verts.len()
                } else {
                    verts.len().saturating_sub(1)
                }
            }
            Object::Point { .. } | Object::Text { .. } => 0,
        };
        if segment < segment_count {
            // `arc_rows` is ascending, so the pinned segment slots into its
            // own place, not the end.
            if let Err(slot) = arc_rows.binary_search(&segment) {
                arc_rows.insert(slot, segment);
            }
        } else {
            dialog.sticky_arc = None;
        }
    }

    dialog.derived = Some((dialog.revision, length, area));
    dialog.arc_rows = Some((dialog.revision, arc_rows));
    dialog.issue = validate_object(&dialog.object);
    // Bumping the revision answers whatever the last transient message was complaining about.
    dialog.message = None;

    // Rows can disappear under the selection and under a half-typed cell.
    let row_count = vertex_row_count(&dialog.object);
    if dialog.selected_row.is_some_and(|row| row >= row_count) {
        dialog.selected_row = row_count.checked_sub(1);
    }
    if dialog.editing_cell.as_ref().is_some_and(|(row, _, _)| *row >= row_count) {
        dialog.editing_cell = None;
    }
}

fn draw_tab_bar(ui: &mut egui::Ui, dialog: &mut ObjectEditDialog, arcs_available: bool) {
    ui.horizontal(|ui| {
        for (tab, label) in [
            (ObjectEditTab::Properties, tr!(literal = "Properties")),
            (ObjectEditTab::Vertices, tr!(literal = "Vertices")),
            (ObjectEditTab::Arcs, tr!(literal = "Arc & Circle")),
        ] {
            if tab == ObjectEditTab::Arcs && !arcs_available {
                continue;
            }
            if ui.add(MenuButton::new(label).selected(dialog.tab == tab).min_width(104.0)).clicked() {
                dialog.tab = tab;
                dialog.editing_cell = None;
                // Leaving the tab releases the segment pinned to the Arc sheet.
                dialog.sticky_arc = None;
            }
        }
    });
}

// ── Properties ──

fn draw_properties_tab(ui: &mut egui::Ui, dialog: &mut ObjectEditDialog) {
    let layer_rgba = dialog.layer_rgba;
    menu::menu_section(ui, tr!(literal = "Identity"));
    read_only_row(ui, &tr!(literal = "Type"), &dialog.object.kind_name());
    read_only_row(ui, &tr!(literal = "ID"), &dialog.id.0.to_string());
    read_only_row(ui, &tr!(literal = "Layer"), &dialog.layer_name);
    read_only_row(ui, &tr!(literal = "Vertices"), &vertex_row_count(&dialog.object).to_string());

    menu::menu_section(ui, tr!(literal = "Appearance"));
    // Touching the swatch pins a fixed colour on the object; the toggle
    // above returns to the layer's.
    let mut by_layer = matches!(dialog.object.color(), ObjectColor::ByLayer);
    if MenuFieldBool::new(tr!(literal = "Colour by layer"), &mut by_layer)
        .help_text(tr!(literal = "Follow the owning layer's colour instead of a colour pinned to this object."))
        .show(ui)
        .changed()
    {
        set_object_color(&mut dialog.object, if by_layer { ObjectColor::ByLayer } else { ObjectColor::Fixed(layer_rgba) });
    }
    if !by_layer {
        let rgba = match dialog.object.color() {
            ObjectColor::Fixed(rgba) => rgba,
            ObjectColor::ByLayer => layer_rgba,
        };
        let mut color32 = rgba_to_color32(rgba);
        if MenuFieldColor32::new(tr!(literal = "Colour"), &mut color32).show(ui).changed() {
            set_object_color(&mut dialog.object, ObjectColor::Fixed(color32_to_rgba(color32)));
        }
    }

    // Colour, fill and line weight leave the revision alone - they don't
    // affect derived lengths or validation, so a 200k-vertex contour isn't
    // re-tessellated for an unrelated drag. Line weight is also range-clamped
    // by its own field, so it can't fail that check either.
    let mut rescan = false;
    match &mut dialog.object {
        Object::Polyline { closed, fill, line_weight, .. } => {
            rescan |= MenuFieldBool::new(tr!(literal = "Closed"), closed)
                .help_text(tr!(literal = "Join the last vertex back to the first."))
                .show(ui)
                .changed();
            let current_fill = *fill;
            MenuFieldCombo::new(
                "object_edit_fill",
                tr!(literal = "Fill"),
                fill,
                fill_style_label(current_fill),
                [FillStyle::Clear, FillStyle::Crosses, FillStyle::Slashes, FillStyle::Solid].map(|style| (style, fill_style_label(style).into())),
            )
            .show(ui);
            MenuFieldF32::new(tr!(literal = "Line weight"), line_weight, 0.1..=20.0).speed(0.1).max_decimals(2).show(ui);
        }
        Object::Text { content, height, rotation, .. } => {
            rescan |= MenuFieldText::new(tr!(literal = "Text"), content).show(ui).changed();
            rescan |= MenuFieldF64::new(tr!(literal = "Height"), height, 0.001..=1.0e9)
                .speed(0.25)
                .max_decimals(3)
                .suffix(tr!(literal = "m"))
                .show(ui)
                .changed();
            // The model keeps rotation in radians; the field, like the
            // in-viewport text editor, uses degrees.
            let mut degrees = rotation.to_degrees();
            if MenuFieldF64::new(tr!(literal = "Rotation"), &mut degrees, -360.0..=360.0)
                .speed(1.0)
                .max_decimals(3)
                .suffix(tr!(literal = "°"))
                .show(ui)
                .changed()
            {
                *rotation = degrees.to_radians();
                rescan = true;
            }
        }
        Object::Point { .. } => {}
    }
    if rescan {
        dialog.touch();
    }
    ui.add_space(2.0);
}

fn set_object_color(object: &mut Object, color: ObjectColor) {
    match object {
        Object::Point { color: slot, .. } | Object::Polyline { color: slot, .. } | Object::Text { color: slot, .. } => *slot = color,
    }
}

// ── Vertices ──

fn draw_vertices_tab(ui: &mut egui::Ui, dialog: &mut ObjectEditDialog) {
    let is_polyline = matches!(dialog.object, Object::Polyline { .. });
    let closed = matches!(dialog.object, Object::Polyline { closed: true, .. });
    let row_count = vertex_row_count(&dialog.object);

    if is_polyline {
        let (length, area) = derived_values(dialog);
        let summary = match area {
            Some(area) => tr_format!(
                literal = "Perimeter %length% m, area %area% m²",
                length = format!("{length:.3}"),
                area = format!("{area:.3}")
            ),
            None => tr_format!(literal = "Length %length% m", length = format!("{length:.3}")),
        };
        ui.label(egui::RichText::new(summary).color(ui.visuals().weak_text_color()));
    } else {
        // A point or text object has exactly one position, shown as a single
        // row so every kind gets the same sheet shape.
        ui.label(egui::RichText::new(tr!(literal = "This object has a single position.")).color(ui.visuals().weak_text_color()));
    }
    ui.add_space(2.0);

    let columns: &[VertexColumn] = if is_polyline {
        &[VertexColumn::X, VertexColumn::Y, VertexColumn::Z, VertexColumn::Bulge]
    } else {
        &[VertexColumn::X, VertexColumn::Y, VertexColumn::Z]
    };
    let headings: Vec<String> = if is_polyline {
        vec![tr!(literal = "X"), tr!(literal = "Y"), tr!(literal = "Z"), tr!(literal = "Bulge")]
    } else {
        vec![tr!(literal = "X"), tr!(literal = "Y"), tr!(literal = "Z")]
    };
    draw_sheet_header(ui, &headings);

    egui::ScrollArea::vertical()
        .id_salt("object_edit_vertex_sheet")
        .auto_shrink([false; 2])
        .min_scrolled_height(0.0)
        .max_height(SHEET_HEIGHT)
        .show_rows(ui, ROW_HEIGHT, row_count, |ui, visible| {
            for row in visible {
                draw_vertex_row(ui, dialog, row, columns);
            }
        });

    if !is_polyline {
        return;
    }

    ui.add_space(4.0);
    let has_row = dialog.selected_row.is_some();
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        if ui.add(MenuButton::new(tr!(literal = "Insert after")).enabled(has_row)).clicked() {
            action = Some(RowAction::InsertAfter);
        }
        if ui.add(MenuButton::new(tr!(literal = "Delete")).enabled(has_row)).clicked() {
            action = Some(RowAction::Delete);
        }
        if ui.add(MenuButton::new(tr!(literal = "Move up")).enabled(has_row)).clicked() {
            action = Some(RowAction::MoveUp);
        }
        if ui.add(MenuButton::new(tr!(literal = "Move down")).enabled(has_row)).clicked() {
            action = Some(RowAction::MoveDown);
        }
        if ui.add(MenuButton::new(tr!(literal = "Reverse")).enabled(row_count >= 2)).clicked() {
            action = Some(RowAction::Reverse);
        }
    });
    if let Some(action) = action {
        apply_row_action(dialog, action, closed);
    }
}

/// Paint the sheet's column headings above the scrolling rows.
fn draw_sheet_header(ui: &mut egui::Ui, headings: &[String]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), egui::Sense::hover());
    let color = ui.visuals().weak_text_color();
    let font = egui::FontId::proportional(11.0);
    let cell_width = value_cell_width(rect.width(), headings.len());
    ui.painter()
        .text(egui::pos2(rect.left() + 2.0, rect.center().y), egui::Align2::LEFT_CENTER, "#", font.clone(), color);
    for (index, heading) in headings.iter().enumerate() {
        let left = rect.left() + INDEX_COLUMN + CELL_SPACING + (cell_width + CELL_SPACING) * index as f32;
        ui.painter()
            .text(egui::pos2(left + 2.0, rect.center().y), egui::Align2::LEFT_CENTER, heading, font.clone(), color);
    }
}

/// Width one value cell gets when `columns` of them share the row.
fn value_cell_width(row_width: f32, columns: usize) -> f32 {
    let columns = columns.max(1) as f32;
    ((row_width - INDEX_COLUMN - CELL_SPACING * (columns + 1.0)) / columns).max(MIN_CELL_WIDTH)
}

fn draw_vertex_row(ui: &mut egui::Ui, dialog: &mut ObjectEditDialog, row: usize, columns: &[VertexColumn]) {
    let row_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), ROW_HEIGHT));
    // Sensed before the cells are added, so a click on a cell goes to the cell, not the row.
    let row_response = ui.interact(row_rect, ui.id().with(("object_edit_row", row)), egui::Sense::click());
    if dialog.selected_row == Some(row) {
        ui.painter().rect_filled(row_rect, GROUP_CORNER_RADIUS, ui.visuals().selection.bg_fill.gamma_multiply(0.35));
    }

    let cell_width = value_cell_width(row_rect.width(), columns.len());
    let mut clicked_cell = None;
    let mut commit = None;
    ui.allocate_ui_with_layout(row_rect.size(), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = CELL_SPACING;
        ui.add_sized(
            [INDEX_COLUMN, ROW_HEIGHT],
            egui::Label::new(egui::RichText::new((row + 1).to_string()).monospace().color(ui.visuals().weak_text_color())),
        );
        for &column in columns {
            let Some(value) = vertex_component(&dialog.object, row, column) else {
                // No such value here: the last vertex of an open polyline has
                // no outgoing segment, so no bulge. A dash marks it, reading
                // as "not applicable" rather than a zero the user could type over.
                ui.add_sized(
                    [cell_width, ROW_HEIGHT],
                    egui::Label::new(egui::RichText::new("-").monospace().color(ui.visuals().weak_text_color())),
                );
                continue;
            };
            let editing = dialog
                .editing_cell
                .as_ref()
                .is_some_and(|(cell_row, cell_column, _)| *cell_row == row && *cell_column == column);
            if editing {
                let Some((_, _, buffer)) = dialog.editing_cell.as_mut() else {
                    continue;
                };
                let response = ui.add_sized(
                    [cell_width, ROW_HEIGHT],
                    egui::TextEdit::singleline(buffer)
                        .id(active_cell_id())
                        .font(egui::TextStyle::Monospace)
                        .margin(egui::Margin::symmetric(2, 0)),
                );
                if response.lost_focus() {
                    // Enter and blur both commit; Escape abandons the cell,
                    // leaving the working copy as it was.
                    let escaped = ui.input(|input| input.key_pressed(egui::Key::Escape));
                    commit = dialog.editing_cell.take().map(|(row, column, buffer)| (row, column, buffer, escaped));
                }
            } else {
                // Three decimals is a readable sheet; the stored value keeps
                // every digit, shown when the cell opens.
                let response = ui.add_sized(
                    [cell_width, ROW_HEIGHT],
                    egui::Label::new(egui::RichText::new(format!("{value:.3}")).monospace()).sense(egui::Sense::click()),
                );
                if response.clicked() {
                    clicked_cell = Some((row, column, value));
                }
            }
        }
    });

    if let Some((row, column, buffer, escaped)) = commit
        && !escaped
    {
        commit_cell(dialog, row, column, &buffer);
    }
    if let Some((row, column, value)) = clicked_cell {
        // Adopting a new cell must commit the one being left first: a row
        // scrolled out of `show_rows`' visible range is never drawn, so its
        // `TextEdit` never reports `lost_focus` and would silently drop the edit.
        if let Some((previous_row, previous_column, buffer)) = dialog.editing_cell.take() {
            commit_cell(dialog, previous_row, previous_column, &buffer);
        }
        dialog.selected_row = Some(row);
        dialog.editing_cell = Some((row, column, format!("{value}")));
        ui.ctx().memory_mut(|memory| memory.request_focus(active_cell_id()));
    } else if row_response.clicked() {
        dialog.selected_row = Some(row);
    }
}

/// Parse a typed cell and write it to the working copy. `str::parse` accepts
/// "nan"/"inf", which would poison the geometry, so non-finite input is
/// refused here with the same complaint as a bad parse. False when refused.
fn commit_cell(dialog: &mut ObjectEditDialog, row: usize, column: VertexColumn, text: &str) -> bool {
    let trimmed = text.trim();
    match trimmed.parse::<f64>() {
        Ok(value) if value.is_finite() => {
            if set_vertex_component(&mut dialog.object, row, column, value) {
                dialog.touch();
            }
            true
        }
        _ => {
            dialog.message = Some(if trimmed.is_empty() {
                tr!(literal = "Enter a number")
            } else {
                tr_format!(literal = "\"%text%\" is not a number", text = trimmed)
            });
            false
        }
    }
}

/// How many rows the vertex sheet shows.
fn vertex_row_count(object: &Object) -> usize {
    match object {
        Object::Polyline { verts, .. } => verts.len(),
        Object::Point { .. } | Object::Text { .. } => 1,
    }
}

/// Position and bulge of one sheet row. A point or text object has a
/// position but no outgoing segment, hence no bulge - neither does the last
/// vertex of an open polyline, matching how `arc_segment` counts segments.
fn vertex_row(object: &Object, row: usize) -> Option<(glam::DVec3, Option<f64>)> {
    match object {
        Object::Polyline { verts, closed, .. } => verts.get(row).map(|vertex| {
            let starts_a_segment = *closed || row + 1 < verts.len();
            (vertex.pos, starts_a_segment.then_some(vertex.bulge))
        }),
        Object::Point { pos, .. } | Object::Text { pos, .. } => (row == 0).then_some((*pos, None)),
    }
}

/// Value shown in one cell, or `None` when this kind has no such cell.
fn vertex_component(object: &Object, row: usize, column: VertexColumn) -> Option<f64> {
    let (pos, bulge) = vertex_row(object, row)?;
    Some(match column {
        VertexColumn::X => pos.x,
        VertexColumn::Y => pos.y,
        VertexColumn::Z => pos.z,
        VertexColumn::Bulge => bulge?,
    })
}

/// Write one cell back. Returns whether the working copy actually changed.
fn set_vertex_component(object: &mut Object, row: usize, column: VertexColumn, value: f64) -> bool {
    let (pos, bulge) = match object {
        Object::Polyline { verts, closed, .. } => {
            // Mirrors `vertex_row`: no outgoing segment, no bulge to write.
            let starts_a_segment = *closed || row + 1 < verts.len();
            match verts.get_mut(row) {
                Some(vertex) => (&mut vertex.pos, starts_a_segment.then_some(&mut vertex.bulge)),
                None => return false,
            }
        }
        Object::Point { pos, .. } | Object::Text { pos, .. } => {
            if row != 0 {
                return false;
            }
            (pos, None)
        }
    };
    let slot = match column {
        VertexColumn::X => &mut pos.x,
        VertexColumn::Y => &mut pos.y,
        VertexColumn::Z => &mut pos.z,
        VertexColumn::Bulge => match bulge {
            Some(bulge) => bulge,
            None => return false,
        },
    };
    if *slot == value {
        return false;
    }
    *slot = value;
    true
}

fn apply_row_action(dialog: &mut ObjectEditDialog, action: RowAction, closed: bool) {
    let selected = dialog.selected_row;
    let (changed, selection) = {
        let Object::Polyline { verts, .. } = &mut dialog.object else {
            return;
        };
        match action {
            RowAction::InsertAfter => match selected.and_then(|row| insert_vertex_after(verts, row, closed)) {
                Some(row) => (true, Some(row)),
                None => (false, selected),
            },
            RowAction::Delete => match selected {
                // The row below slid up into the deleted row's place, so the
                // cursor stays where the eye is unless the list ended there.
                Some(row) if delete_vertex(verts, row) => (true, verts.len().checked_sub(1).map(|last| row.min(last))),
                _ => (false, selected),
            },
            RowAction::MoveUp | RowAction::MoveDown => match selected.and_then(|row| move_vertex(verts, row, action == RowAction::MoveUp)) {
                Some(row) => (true, Some(row)),
                None => (false, selected),
            },
            RowAction::Reverse => match verts.len().checked_sub(1) {
                Some(last) if last >= 1 => {
                    reverse_vertices(verts, closed);
                    (true, selected.map(|row| last.saturating_sub(row)))
                }
                _ => (false, selected),
            },
        }
    };
    if changed {
        dialog.selected_row = selection;
        // The cell being typed into may have just moved or gone.
        dialog.editing_cell = None;
        dialog.touch();
    }
}

/// Cached length and plan area of the working copy.
fn derived_values(dialog: &mut ObjectEditDialog) -> (f64, Option<f64>) {
    refresh_caches(dialog);
    let (_, length, area) = dialog.derived.expect("refresh_caches populates the derived cache");
    (length, area)
}

// ── Arc & Circle ──

fn circle_spec(object: &Object) -> Option<CircleSpec> {
    match object {
        Object::Polyline { verts, closed, .. } => compact_circle(verts, *closed),
        Object::Point { .. } | Object::Text { .. } => None,
    }
}

fn draw_arcs_tab(ui: &mut egui::Ui, dialog: &mut ObjectEditDialog) {
    menu::menu_note(
        ui,
        tr!(literal = "Bulge arcs are horizontal by data model: the arc turns in plan and the elevation runs straight from one vertex to the next."),
    );
    ui.add_space(4.0);

    if let Some(spec) = circle_spec(&dialog.object) {
        menu::menu_section(ui, tr!(literal = "Circle"));
        let mut center = spec.center;
        let mut radius = spec.radius;
        let mut changed = false;
        changed |= MenuFieldF64::new(tr!(literal = "Centre X"), &mut center.x, f64::MIN..=f64::MAX)
            .speed(0.5)
            .max_decimals(3)
            .show(ui)
            .changed();
        changed |= MenuFieldF64::new(tr!(literal = "Centre Y"), &mut center.y, f64::MIN..=f64::MAX)
            .speed(0.5)
            .max_decimals(3)
            .show(ui)
            .changed();
        changed |= MenuFieldF64::new(tr!(literal = "Centre Z"), &mut center.z, f64::MIN..=f64::MAX)
            .speed(0.5)
            .max_decimals(3)
            .show(ui)
            .changed();
        changed |= MenuFieldF64::new(tr!(literal = "Radius"), &mut radius, 1.0e-6..=f64::MAX)
            .speed(0.5)
            .max_decimals(3)
            .suffix(tr!(literal = "m"))
            .show(ui)
            .changed();
        if changed {
            let applied = match &mut dialog.object {
                Object::Polyline { verts, .. } => set_compact_circle(verts, center, radius),
                Object::Point { .. } | Object::Text { .. } => false,
            };
            if applied {
                dialog.touch();
            }
        }
        return;
    }

    let arc_count = dialog.arc_rows.as_ref().map_or(0, |(_, rows)| rows.len());
    if arc_count == 0 {
        ui.label(egui::RichText::new(tr!(literal = "This object has no arc segments.")).color(ui.visuals().weak_text_color()));
        return;
    }

    menu::menu_section(ui, tr!(literal = "Arc segments"));
    draw_sheet_header(ui, &[tr!(literal = "Chord"), tr!(literal = "Radius"), tr!(literal = "Sweep")]);
    egui::ScrollArea::vertical()
        .id_salt("object_edit_arc_sheet")
        .auto_shrink([false; 2])
        .min_scrolled_height(0.0)
        .max_height(SHEET_HEIGHT)
        .show_rows(ui, ROW_HEIGHT, arc_count, |ui, visible| {
            // Copy out only the visible indices, so the cache borrow ends
            // before rows write back to the working copy.
            let segments: Vec<usize> = dialog.arc_rows.as_ref().map(|(_, rows)| rows[visible].to_vec()).unwrap_or_default();
            for segment in segments {
                draw_arc_row(ui, dialog, segment);
            }
        });
}

/// Chord length and bulge of the segment leaving vertex `index`.
fn arc_segment(object: &Object, index: usize) -> Option<(f64, f64)> {
    let Object::Polyline { verts, closed, .. } = object else {
        return None;
    };
    if !*closed && index + 1 >= verts.len() {
        return None;
    }
    let start = verts.get(index)?;
    let end = verts.get((index + 1) % verts.len())?;
    Some((chord_length_xy(start.pos, end.pos), start.bulge))
}

fn draw_arc_row(ui: &mut egui::Ui, dialog: &mut ObjectEditDialog, segment: usize) {
    let Some((chord, bulge)) = arc_segment(&dialog.object, segment) else {
        return;
    };
    let mut radius = bulge_radius(chord, bulge).unwrap_or(0.0);
    let mut sweep = bulge_to_sweep_degrees(bulge);
    let cell_width = value_cell_width(ui.available_width(), 3);
    let mut new_bulge = None;

    // Drag values carry auto-generated ids, so each row needs its own
    // `push_id` namespace or carets get mixed up.
    ui.push_id(segment, |ui| {
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), ROW_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = CELL_SPACING;
            ui.add_sized(
                [INDEX_COLUMN, ROW_HEIGHT],
                egui::Label::new(egui::RichText::new((segment + 1).to_string()).monospace().color(ui.visuals().weak_text_color())),
            );
            ui.add_sized([cell_width, ROW_HEIGHT], egui::Label::new(egui::RichText::new(format!("{chord:.3}")).monospace()));
            // A chord cannot subtend an arc tighter than its own half-length, so
            // the radius range stops there; a stored value outside either range
            // is shown as is, not clamped, or a flat segment would report a change.
            let radius_response = ui.add_sized(
                [cell_width, ROW_HEIGHT],
                egui::DragValue::new(&mut radius)
                    .speed(0.25)
                    .range((chord * 0.5)..=f64::MAX)
                    .clamp_existing_to_range(false)
                    .max_decimals(3),
            );
            // The bulge encoding is a quarter-angle tangent, which asymptotes
            // at a full turn; the range stops just short of it.
            let sweep_response = ui.add_sized(
                [cell_width, ROW_HEIGHT],
                egui::DragValue::new(&mut sweep)
                    .speed(0.5)
                    .range(-359.9..=359.9)
                    .clamp_existing_to_range(false)
                    .max_decimals(3)
                    .suffix(tr!(literal = "°")),
            );
            if radius_response.changed() {
                new_bulge = bulge_for_radius(chord, radius, bulge);
            } else if sweep_response.changed() {
                new_bulge = Some(sweep_degrees_to_bulge(sweep));
            }
        });
    });

    if new_bulge.is_some() {
        // Touching either drag pins this segment (see `sticky_arc`) so a drag
        // to zero can't delete its own row - or the whole tab - out from under it.
        dialog.sticky_arc = Some(segment);
    }
    if let Some(new_bulge) = new_bulge
        && new_bulge.is_finite()
        && set_vertex_component(&mut dialog.object, segment, VertexColumn::Bulge, new_bulge)
    {
        dialog.touch();
    }
}
