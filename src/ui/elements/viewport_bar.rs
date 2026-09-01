//! The viewport bar: the row under the menu bar that carries everything about
//! the open workspace.
//!
//! Three clusters across one strip, the way Blender heads an editor:
//!
//! - **Left** - the project actions that are true in every workspace (save,
//!   import, export, undo, redo), followed by the menus belonging to the
//!   workspace itself.
//! - **Centre** - what the drawing tools will use next: the active layer, the
//!   working elevation, the line colour and the fill.
//! - **Right** - the view controls, which used to float on a tile hung off the
//!   viewport's right edge.
//!
//! The centre is centred on the *window*, not on the space the other two
//! clusters leave, so it stays put as they change width - but it is clamped to
//! that space, so a narrow window slides it across rather than letting the
//! three overlap. Narrower still and the bar stops narrowing altogether and
//! scrolls under the wheel, the way a Blender header does - see
//! [`crate::ui::elements::bar_strip`].

use crate::ui::{
    EditorState, UiProjectView, color32_to_rgba,
    elements::main_menu,
    rgba_to_color32,
    state::{ActiveTool, UiCommand, UiProjectEntry},
    themed_icon, unthemed_icon,
    widgets::{
        menu::MenuFieldF64,
        toolbar::{ColorSquarePicker, HatchPicker, TOOL_CELL_SIZE, ToolbarButton},
    },
};

/// Gap between buttons in the same cluster.
const BUTTON_GAP: f32 = 0.0;
/// How much shorter than a button a menu label's hover fill is drawn, so the
/// dropdowns read as labels in the bar rather than as more buttons.
const MENU_ROW_INSET: f32 = 6.0;
/// Gap between two clusters of buttons. The floating tiles' gap reads as more
/// space than it is, because the tiles pad themselves; this is the docked
/// equivalent of that separation.
const CLUSTER_GAP: f32 = 12.0;
/// Clear space kept between the centre cluster and the two beside it.
const CENTRE_CLEARANCE: f32 = 16.0;
/// Gap between one drawing setting in the centre run and the next. Wider than
/// the gap between buttons: these are labelled fields rather than a run of
/// icons, and they read as separate settings.
const CENTRE_ITEM_GAP: f32 = 14.0;
/// Gap *inside* one of those settings: between a label and its own control.
const CENTRE_LABEL_GAP: f32 = 4.0;
/// Width the centre cluster is placed from on the very first frame, before it
/// has been laid out once and can report its own.
const CENTRE_WIDTH_GUESS: f32 = 400.0;
/// Width of the active-layer combo box.
const LAYER_COMBO_WIDTH: f32 = 220.0;
/// Longest layer name shown in that combo before it is elided.
const MAX_LAYER_DISPLAY: usize = 22;
/// Label for the primary shortcut modifier in tooltips. Spelled out rather
/// than drawn as a glyph, so it can't land as tofu in the bundled fonts.
const PRIMARY_MODIFIER: &str = if cfg!(target_os = "macos") { "Cmd+" } else { "Ctrl+" };
/// Label for the shift modifier in tooltips.
const SHIFT_MODIFIER: &str = "Shift+";

/// Draw the viewport bar and return what it claimed.
///
/// Its visible surface uses [`TOOL_CELL_SIZE`], the same thickness as the left
/// and bottom toolbars; the panel claims its chrome margins around that.
pub(crate) fn draw_viewport_bar(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) -> egui::Rect {
    let claimed = TOOL_CELL_SIZE + 2.0 * crate::ui::chrome::margin(ui.ctx());
    egui::Panel::top("viewport_bar")
        .resizable(false)
        .show_separator_line(crate::ui::chrome::show_separator_line(ui))
        .exact_size(claimed)
        // Buttons meet the region edges so its later chrome pass can mask their
        // outer corners; explicit cluster gaps provide all the spacing needed.
        .frame(crate::ui::chrome::region_frame(ui).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| {
            // Once the window is too narrow to hold all three clusters the bar
            // stops narrowing and scrolls instead, rather than letting them
            // run into each other. See `elements::bar_strip`.
            crate::ui::elements::bar_strip(ui, "viewport_bar_strip", ui.available_height(), |ui, strip| {
                // Keep the automatic ids below this point independent of the
                // parent panel's layout pass. egui may rerun a frame for sizing;
                // an explicit scope prevents an earlier conditional panel from
                // shifting the ids of these persistent controls on that rerun.
                let contents_id = ui.make_persistent_id("viewport_bar_contents");
                ui.scope_builder(egui::UiBuilder::new().id(contents_id).max_rect(strip), |ui| {
                    let side = strip.height();
                    // Deliberately *not* raising `interact_size.y` to match: the
                    // combo box and the number field in the centre take their
                    // height from it, and a taller bar is not a reason to grow
                    // them. The layout centres everything on the row, so the three
                    // clusters can be three different heights and still line up.

                    let left = cluster(ui, strip, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        draw_project_actions(ui, editor, project, commands, side);
                        main_menu::draw_workspace_menus(ui, editor, project, commands, (side - MENU_ROW_INSET).max(1.0));
                    });
                    let right = cluster(ui, strip, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        draw_view_tools(ui, editor, commands, side);
                    });

                    if !editor.active_workspace.has_production_tools() {
                        // Nothing between the two clusters but the parting they
                        // would take if they met.
                        return left.width() + CLUSTER_GAP + right.width();
                    }

                    // egui centres a block it is told the size of, and the run
                    // below is only measured once it has been laid out - so it
                    // is placed from the width it came out at last frame and
                    // reports its own width back for the next one. Its content
                    // is fixed-width, so that settles on the first frame and
                    // stays there.
                    let width_id = ui.make_persistent_id("viewport_bar_centre_width");
                    let width: f32 = ui.data(|data| data.get_temp(width_id)).unwrap_or(CENTRE_WIDTH_GUESS);
                    if let Some(band) = centre_band(strip, left, right) {
                        let left_edge = (strip.center().x - width / 2.0).clamp(band.left(), (band.right() - width).max(band.left()));
                        // Open to the right of where the run starts rather than
                        // sized to it, so a stale width slides the run along
                        // instead of squeezing what is in it.
                        let run = egui::Rect::from_min_max(egui::pos2(left_edge, band.top()), band.max);
                        let drawn = cluster(ui, run, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            draw_drawing_settings(ui, editor, project);
                        });
                        ui.data_mut(|data| data.insert_temp(width_id, drawn.width()));
                    }
                    // The width the strip has to keep, worked out from the same
                    // centre width the run was just placed from: the three
                    // clusters, each with its clearance.
                    left.width() + CENTRE_CLEARANCE + width + CENTRE_CLEARANCE + right.width()
                })
                .inner
            });
        })
        .response
        .rect
}

/// Lay one cluster out over `rect`, and report what it drew into.
///
/// The three clusters are placed against the same strip rather than in
/// sequence, so each is given the rect it should align itself in and none of
/// them consumes space the next one wanted.
fn cluster(ui: &mut egui::Ui, rect: egui::Rect, layout: egui::Layout, add_contents: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(layout), |ui| {
        ui.spacing_mut().item_spacing.x = BUTTON_GAP;
        add_contents(ui);
    })
    .response
    .rect
}

/// The clear space between the two clusters the centre run has to stay inside.
///
/// `None` once they meet, which is a window too narrow to show the centre at
/// all.
fn centre_band(strip: egui::Rect, left: egui::Rect, right: egui::Rect) -> Option<egui::Rect> {
    let band = egui::Rect::from_min_max(
        egui::pos2(left.right() + CENTRE_CLEARANCE, strip.top()),
        egui::pos2(right.left() - CENTRE_CLEARANCE, strip.bottom()),
    );
    (band.width() > 0.0).then_some(band)
}

/// The project actions, which every workspace carries.
fn draw_project_actions(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>, side: f32) {
    let has_unsaved = project.projects.iter().any(UiProjectEntry::needs_save);
    let save = ui.add_enabled(
        has_unsaved,
        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "save_project.svg")), format!("Save Project ({PRIMARY_MODIFIER}S)"))
            .id_salt("save_project")
            .button_side(side),
    );
    if save.clicked() {
        commands.push(UiCommand::SaveProject);
    }

    // The same two dialogs the File menu opens; only one of the pair is ever
    // up, so opening one closes the other.
    let has_project = project.projects.iter().any(|entry| entry.is_active);
    let import = ui.add_enabled(
        has_project,
        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "import_data.svg")), "Import...")
            .id_salt("import")
            .button_side(side),
    );
    if import.clicked() {
        editor.show_import = true;
        editor.show_export = false;
    }
    let export = ui.add_enabled(
        has_project,
        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "export_data.svg")), "Export...")
            .id_salt("export")
            .button_side(side),
    );
    if export.clicked() {
        editor.show_import = false;
        editor.show_export = true;
    }

    let undo = ui.add_enabled(
        editor.can_undo,
        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "undo.svg")), format!("Undo ({PRIMARY_MODIFIER}Z)"))
            .id_salt("undo")
            .button_side(side),
    );
    if undo.clicked() {
        commands.push(UiCommand::Undo);
    }
    let redo = ui.add_enabled(
        editor.can_redo,
        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "redo.svg")), format!("Redo ({PRIMARY_MODIFIER}{SHIFT_MODIFIER}Z)"))
            .id_salt("redo")
            .button_side(side),
    );
    if redo.clicked() {
        commands.push(UiCommand::Redo);
    }
}

/// What the drawing tools will use next: layer, elevation, line colour, fill.
fn draw_drawing_settings(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView) {
    // The run's own spacing holds a label to its control; the settings are
    // parted from each other by [`CENTRE_ITEM_GAP`] on top of it, so "Z:" stays
    // against its field while the four settings read as four.
    ui.spacing_mut().item_spacing.x = CENTRE_LABEL_GAP;
    let part = |ui: &mut egui::Ui| ui.add_space(CENTRE_ITEM_GAP - CENTRE_LABEL_GAP);

    ui.label("Layer:");
    let active_layers = project
        .projects
        .iter()
        .find(|entry| entry.is_active)
        .map(|entry| entry.layers.as_slice())
        .unwrap_or_default();
    let selected_layer = editor
        .active_layer
        .and_then(|id| active_layers.iter().find(|layer| layer.id == id && layer.is_loaded))
        .map(|layer| layer.name.as_str())
        .unwrap_or("None");
    let layer_display: String = if selected_layer.chars().count() > MAX_LAYER_DISPLAY {
        format!("{}…", selected_layer.chars().take(MAX_LAYER_DISPLAY - 1).collect::<String>())
    } else {
        selected_layer.to_string()
    };
    egui::ComboBox::from_id_salt("layer_combo_box")
        .selected_text(layer_display)
        .width(LAYER_COMBO_WIDTH)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut editor.active_layer, None, "None");
            for layer in active_layers.iter().filter(|layer| layer.is_loaded) {
                ui.selectable_value(&mut editor.active_layer, Some(layer.id), &layer.name);
            }
        });

    part(ui);

    let z_resp = MenuFieldF64::new("Z:", &mut editor.z_input, f64::MIN..=f64::MAX).width(80.0).suffix("m").show_inline(ui);
    if z_resp.changed() && editor.z_input.is_finite() {
        editor.z_level = editor.z_input;
    }

    part(ui);
    ui.label("Color:");
    let mut line_c32 = rgba_to_color32(editor.tool_line_color);
    if ColorSquarePicker::new(&mut line_c32).show(ui).changed() {
        editor.tool_line_color = color32_to_rgba(line_c32);
    }

    part(ui);
    ui.label("Fill:");
    HatchPicker::new(&mut editor.tool_hatch, rgba_to_color32(editor.tool_line_color)).show(ui);
}

/// The view controls, at the far end of the bar.
///
/// One evenly spaced run: these are all questions about how the scene is being
/// looked at, so nothing here is clustered off from anything else.
///
/// Drawn into a right-to-left layout, so the run is written here in the order
/// it reads on screen and placed from the strip's right edge inward.
///
/// What is here is what any workspace asks of the camera; the switches over
/// how the scene itself is drawn belong to production and are added ahead of
/// it only there - see [`draw_production_view_tools`].
fn draw_view_tools(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, side: f32) {
    if editor.active_workspace.has_production_tools() {
        draw_production_view_tools(ui, editor, commands, side);
    }

    let exaggeration = ui.add(
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("vertical_exaggeration.svg")),
            format!("Vertical Exaggeration ({:.2}×)", editor.vertical_exaggeration),
        )
        .id_salt("vertical_exaggeration")
        .button_side(side)
        .selected(editor.vertical_exaggeration != 1.0),
    );
    if exaggeration.clicked() {
        editor.vertical_exaggeration_input = editor.vertical_exaggeration;
        editor.vertical_exaggeration_dialog_open = true;
    }

    let zoom = ui.add(
        ToolbarButton::new(egui::Image::new(unthemed_icon!("zoom_to_extents.svg")), "Zoom to Extents")
            .id_salt("zoom_to_extents")
            .button_side(side),
    );
    if zoom.clicked() {
        commands.push(UiCommand::ZoomToExtents);
    }

    let reset = ui.add(
        ToolbarButton::new(egui::Image::new(unthemed_icon!("reset_view.svg")), "Reset View")
            .id_salt("reset_view")
            .button_side(side),
    );
    if reset.clicked() {
        commands.push(UiCommand::ResetView);
    }
}

/// The head of that run: the switches over how the scene is drawn, which the
/// production workspace carries and the others do not.
///
/// Flying and the slice view are modes the scene is put into rather than
/// settings, so a workspace that does not offer them cannot be left holding
/// one - see `main_menu::select_workspace`.
fn draw_production_view_tools(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, side: f32) {
    // A right-to-left layout adds each button to the left of the last, so the
    // run is added in reverse to read left to right on screen.
    let fly = ui.add(
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("fly_mode.svg")),
            if editor.fly_mode_enabled { "Disable Flying Mode" } else { "Enable Flying Mode" },
        )
        .id_salt("fly_mode")
        .button_side(side)
        .selected(editor.fly_mode_enabled),
    );
    if fly.clicked() {
        commands.push(UiCommand::SetFlyModeEnabled(!editor.fly_mode_enabled));
    }

    // Vertical slice view: arm the two-click line placement, or exit the mode
    // if it is already active.
    let slice_engaged = editor.slice_mode_enabled || editor.active_tool == ActiveTool::VerticalSlice;
    let slice = ui.add(
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("slice_view.svg")),
            if editor.slice_mode_enabled { "Exit Slice View" } else { "Vertical Slice View" },
        )
        .id_salt("vertical_slice")
        .button_side(side)
        .selected(slice_engaged),
    );
    if slice.clicked() {
        if editor.slice_mode_enabled {
            commands.push(UiCommand::SetSliceModeEnabled(false));
        } else {
            commands.push(UiCommand::SetActiveTool(ActiveTool::VerticalSlice));
        }
    }

    let wireframes = ui.add(
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("toggle_wireframes.svg")),
            if editor.topology_wireframes_enabled { "Hide Wireframes" } else { "Show Wireframes" },
        )
        .id_salt("wireframes")
        .button_side(side)
        .selected(editor.topology_wireframes_enabled),
    );
    if wireframes.clicked() {
        commands.push(UiCommand::SetTopologyWireframes(!editor.topology_wireframes_enabled));
    }

    let points = ui.add(
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("toggle_points.svg")),
            if editor.show_points { "Hide Points" } else { "Show Points" },
        )
        .id_salt("show_points")
        .button_side(side)
        .selected(editor.show_points),
    );
    if points.clicked() {
        commands.push(UiCommand::SetShowPoints(!editor.show_points));
    }

    let xray = ui.add(
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("toggle_xray.svg")),
            if editor.xray_enabled { "Disable X-Ray Vision" } else { "Enable X-Ray Vision" },
        )
        .id_salt("xray")
        .button_side(side)
        .selected(editor.xray_enabled),
    );
    if xray.clicked() {
        editor.xray_enabled = !editor.xray_enabled;
    }
}
