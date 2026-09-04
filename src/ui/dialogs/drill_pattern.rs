//! Interactive Drill & Blast pattern creation.

use crate::{
    i18n::{tr, tr_format},
    model::{Document, Object, geometry::tessellate_polyline_bulges},
    ui::{
        state::{EditorState, UiCommand},
        widgets::menu::{self, DragableMenu, MenuButton, MenuField, MenuFieldCombo, MenuFieldF64, MenuFieldText},
    },
};

const DIALOG_MIN_WIDTH: f32 = 410.0;
const PICK_BUTTON_WIDTH: f32 = 64.0;

fn refresh_preview(editor: &mut EditorState, document: &Document) -> bool {
    if let Some(id) = editor.drill_pattern_boundary_id
        && document.get_object(id).is_none_or(|object| {
            !matches!(
                object,
                Object::Polyline { verts, closed: true, .. }
                    if verts.len() >= 3 || (verts.len() == 2 && verts.iter().any(|vertex| vertex.bulge.abs() > f64::EPSILON))
            )
        })
    {
        editor.drill_pattern_boundary_id = None;
        editor.drill_pattern_boundary_name.clear();
        editor.tool_highlight_id = None;
    }
    let result = editor
        .drill_pattern_boundary_id
        .and_then(|id| document.get_object(id))
        .and_then(|object| match object {
            Object::Polyline { verts, closed: true, .. } if verts.len() >= 2 => Some(tessellate_polyline_bulges(verts, true)),
            _ => None,
        })
        .ok_or_else(|| "Pick a closed polyline to define the blast shape".to_owned())
        .and_then(|boundary| {
            crate::model::drill_hole::generate_pattern_collars(
                &boundary,
                editor.drill_pattern_burden,
                editor.drill_pattern_spacing,
                editor.drill_pattern_rotation_deg,
                glam::DVec2::new(editor.drill_pattern_offset_x, editor.drill_pattern_offset_y),
                editor.drill_pattern_layout,
            )
        });

    let (collars, error) = match result {
        Ok(collars) => (collars, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let diameter = editor.drill_pattern_diameter_mm / 1_000.0;
    let changed = editor.drill_pattern_preview_collars != collars
        || editor.drill_pattern_preview_error != error
        || editor.drill_pattern_preview_depth != editor.drill_pattern_depth
        || editor.drill_pattern_preview_diameter != diameter;
    editor.drill_pattern_preview_collars = collars;
    editor.drill_pattern_preview_depth = editor.drill_pattern_depth;
    editor.drill_pattern_preview_diameter = diameter;
    editor.drill_pattern_preview_error = error;
    changed
}

/// Draw the movable pattern builder and keep its world-space preview current.
/// Returns whether overlay geometry changed or needs to be removed.
pub(crate) fn draw_drill_pattern_dialog(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document, commands: &mut Vec<UiCommand>) -> bool {
    if !editor.drill_pattern_open {
        return false;
    }

    let mut geometry_dirty = refresh_preview(editor, document);
    let mut open = true;
    let mut close = false;
    let boundary_label = if editor.drill_pattern_boundary_id.is_some() {
        editor.drill_pattern_boundary_name.clone()
    } else {
        tr!(literal = "None picked")
    };
    let layout_label = editor.drill_pattern_layout.label();

    DragableMenu::new("drill_pattern_dialog", tr!(literal = "Create Drill Pattern"))
        .open(&mut open)
        .min_width(DIALOG_MIN_WIDTH)
        .max_width(440.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui.ctx(), |ui| {
            menu::menu_note(
                ui,
                tr!(literal = "Choose a closed blast boundary, then tune the grid. The drill holes update live in the viewport."),
            );
            ui.add_space(6.0);

            MenuField::new(tr!(literal = "Blast shape"))
                .help_text(tr!(literal = "The closed design polyline whose XY footprint will be filled with holes."))
                .show(ui, |ui, row_height, column_width| {
                    let button_width = PICK_BUTTON_WIDTH;
                    let label_width = (column_width - ui.spacing().item_spacing.x - button_width).max(80.0);
                    ui.allocate_ui_with_layout(egui::vec2(column_width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let mut display = boundary_label.clone();
                        ui.add_sized([label_width, row_height], egui::TextEdit::singleline(&mut display).interactive(false))
                            .on_hover_text(&boundary_label);
                        let button_text = if editor.drill_pattern_awaiting_shape_pick {
                            tr!(literal = "Cancel")
                        } else {
                            tr!(literal = "Pick")
                        };
                        if ui.add(MenuButton::new(button_text).min_width(button_width)).clicked() {
                            if editor.drill_pattern_awaiting_shape_pick {
                                editor.drill_pattern_awaiting_shape_pick = false;
                                editor.viewport_pick_hover_label = None;
                                editor.tool_highlight_id = editor.drill_pattern_boundary_id;
                                geometry_dirty = true;
                            } else {
                                commands.push(UiCommand::BeginDrillPatternShapePick);
                            }
                        }
                    })
                    .response
                });

            if editor.drill_pattern_awaiting_shape_pick {
                let status = editor
                    .viewport_pick_hover_label
                    .clone()
                    .unwrap_or_else(|| tr!(literal = "Move over a closed polyline, then click it in the viewport. Esc cancels the pick."));
                ui.add_space(4.0);
                menu::menu_note(ui, status);
            }

            ui.add_space(4.0);
            MenuFieldF64::new(tr!(literal = "Burden"), &mut editor.drill_pattern_burden, 0.01..=1_000_000.0)
                .help_text(tr!(literal = "Perpendicular distance between pattern rows."))
                .speed(0.1)
                .suffix(tr!(literal = " m"))
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Spacing"), &mut editor.drill_pattern_spacing, 0.01..=1_000_000.0)
                .help_text(tr!(literal = "Distance between holes along each pattern row."))
                .speed(0.1)
                .suffix(tr!(literal = " m"))
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Rotation"), &mut editor.drill_pattern_rotation_deg, -360.0..=360.0)
                .help_text(tr!(literal = "Counter-clockwise pattern rotation from the global X axis."))
                .speed(1.0)
                .suffix("°")
                .show(ui);
            MenuFieldF64::new(tr!(literal = "X offset"), &mut editor.drill_pattern_offset_x, -1_000_000.0..=1_000_000.0)
                .help_text(tr!(literal = "Shift the pattern grid along the global X axis while keeping it clipped to the blast shape."))
                .speed(0.1)
                .suffix(tr!(literal = " m"))
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Y offset"), &mut editor.drill_pattern_offset_y, -1_000_000.0..=1_000_000.0)
                .help_text(tr!(literal = "Shift the pattern grid along the global Y axis while keeping it clipped to the blast shape."))
                .speed(0.1)
                .suffix(tr!(literal = " m"))
                .show(ui);
            MenuFieldCombo::new(
                "drill_pattern_layout",
                tr!(literal = "Arrangement"),
                &mut editor.drill_pattern_layout,
                layout_label,
                crate::model::drill_hole::DrillPatternLayout::ALL.map(|layout| (layout, layout.label().into())),
            )
            .help_text(tr!(literal = "Staggered offsets every second row by half the spacing."))
            .show(ui);
            MenuFieldF64::new(tr!(literal = "Hole diameter"), &mut editor.drill_pattern_diameter_mm, 25.0..=1_000.0)
                .help_text(tr!(literal = "Finished hole diameter. Entered in millimetres and stored with every generated hole."))
                .speed(1.0)
                .suffix(" mm")
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Hole depth"), &mut editor.drill_pattern_depth, 0.01..=100_000.0)
                .help_text(tr!(literal = "Vertical depth below each collar."))
                .speed(0.5)
                .suffix(tr!(literal = " m"))
                .show(ui);
            MenuFieldText::new(tr!(literal = "Pattern name"), &mut editor.drill_pattern_name)
                .help_text(tr!(literal = "Name of the drillhole dataset created in the project."))
                .hint_text(tr!(literal = "e.g. West Cut 03"))
                .show(ui);

            ui.add_space(6.0);
            if let Some(error) = &editor.drill_pattern_preview_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            } else {
                let count = editor.drill_pattern_preview_collars.len();
                menu::menu_note(
                    ui,
                    tr_format!(
                        literal = "Preview: %count% hole(s) · %diameter% mm diameter · %depth% m deep",
                        count = count,
                        diameter = format!("{:.0}", editor.drill_pattern_diameter_mm),
                        depth = format!("{:.2}", editor.drill_pattern_depth)
                    ),
                );
            }

            let can_create = !editor.drill_pattern_preview_collars.is_empty()
                && editor.drill_pattern_diameter_mm.is_finite()
                && editor.drill_pattern_diameter_mm > 0.0
                && editor.drill_pattern_depth.is_finite()
                && editor.drill_pattern_depth > 0.0
                && !editor.drill_pattern_name.trim().is_empty()
                && !editor.drill_pattern_awaiting_shape_pick;
            menu::menu_actions(ui, |ui| {
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if (ui.add(MenuButton::new(tr!(literal = "Create")).primary().enabled(can_create)).clicked() || (confirm && can_create)) && can_create {
                    commands.push(UiCommand::CreateDrillPattern {
                        name: editor.drill_pattern_name.trim().to_owned(),
                        collars: editor.drill_pattern_preview_collars.clone(),
                        depth: editor.drill_pattern_depth,
                        diameter: editor.drill_pattern_diameter_mm / 1_000.0,
                    });
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    close = true;
                }
            });
        });

    if close || !open {
        editor.close_drill_pattern();
        return true;
    }
    geometry_dirty |= refresh_preview(editor, document);
    geometry_dirty
}
