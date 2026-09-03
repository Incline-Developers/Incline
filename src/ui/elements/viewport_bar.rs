//! The viewport bar: the row under the menu bar that carries everything about
//! the open workspace.
//!
//! Three clusters across one strip, the way Blender heads an editor:
//!
//! - **Left** - the project actions that are true in every workspace (save,
//!   import, export, undo, redo), followed by the menus belonging to the
//!   workspace itself.
//! - **Centre** - the working elevation shared by every workspace, plus what
//!   the workspace's tools will act on next: in Production the active layer,
//!   line colour and fill; in Drill & Blast the drill hole dataset being
//!   worked on.
//! - **Right** - the view controls, which used to float on a tile hung off the
//!   viewport's right edge.
//!
//! The centre is centred on the *window*, not on the space the other two
//! clusters leave, so it stays put as they change width - but it is clamped to
//! that space, so a narrow window slides it across rather than letting the
//! three overlap. Narrower still and the bar stops narrowing altogether and
//! scrolls under the wheel, the way a Blender header does - see
//! [`crate::ui::elements::bar_strip`].

use crate::{
    i18n::tr,
    ui::{
        EditorState, UiProjectView, color32_to_rgba,
        elements::main_menu,
        rgba_to_color32,
        state::{ActiveTool, UiCommand, UiProjectEntry, Workspace},
        themed_icon, unthemed_icon,
        widgets::{
            menu::MenuFieldF64,
            toolbar::{ColorSquarePicker, HatchPicker, TOOL_CELL_SIZE, ToolbarButton},
        },
    },
};

/// Gap between buttons in the same cluster.
const BUTTON_GAP: f32 = 0.0;
/// How much shorter than a button a menu label's hover fill is drawn, so the
/// dropdowns read as labels in the bar rather than as more buttons.
const MENU_ROW_INSET: f32 = 6.0;
/// Clear space kept between the centre cluster and the two beside it.
const CENTRE_CLEARANCE: f32 = 16.0;
/// Clear space either side of the hairline parting the view controls every
/// workspace carries from the ones the open workspace adds - see [`divider`].
const DIVIDER_GAP: f32 = 7.0;
/// How far that hairline is held clear of the strip's top and bottom.
const DIVIDER_INSET: f32 = 7.0;
/// Gap between one drawing setting in the centre run and the next. Wider than
/// the gap between buttons: these are labelled fields rather than a run of
/// icons, and they read as separate settings.
const CENTRE_ITEM_GAP: f32 = 14.0;
/// Gap *inside* one of those settings: between a label and its own control.
const CENTRE_LABEL_GAP: f32 = 4.0;
/// Width the centre cluster is placed from on the very first frame, before it
/// has been laid out once and can report its own.
const CENTRE_WIDTH_GUESS: f32 = 400.0;
/// Width of the centre run's combo boxes - the active layer's, and the active
/// drill hole dataset's - which are one behind the other as the workspace
/// changes and so are the same width.
const SELECTOR_COMBO_WIDTH: f32 = 220.0;
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
                        draw_view_tools(ui, editor, project, commands, side);
                    });

                    // egui centres a block it is told the size of, and the run
                    // below is only measured once it has been laid out - so it
                    // is placed from the width it came out at last frame and
                    // reports its own width back for the next one. Its content
                    // is fixed-width, so that settles on the first frame and
                    // stays there.
                    // Keyed by workspace: the runs are different widths, and
                    // a stale one would place the incoming run off centre for
                    // a frame after every tab switch.
                    let width_id = ui.make_persistent_id(("viewport_bar_centre_width", editor.active_workspace.label()));
                    let width: f32 = ui.data(|data| data.get_temp(width_id)).unwrap_or(CENTRE_WIDTH_GUESS);
                    if let Some(band) = centre_band(strip, left, right) {
                        let left_edge = (strip.center().x - width / 2.0).clamp(band.left(), (band.right() - width).max(band.left()));
                        // Open to the right of where the run starts rather than
                        // sized to it, so a stale width slides the run along
                        // instead of squeezing what is in it.
                        let run = egui::Rect::from_min_max(egui::pos2(left_edge, band.top()), band.max);
                        let drawn = cluster(ui, run, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            draw_centre_settings(ui, editor, project, commands);
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
        ToolbarButton::new(
            egui::Image::new(themed_icon!(ui, "save_project.svg")),
            format!("{} ({PRIMARY_MODIFIER}S)", tr!(literal = "Save Project")),
        )
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
        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "import_data.svg")), tr!(literal = "Import..."))
            .id_salt("import")
            .button_side(side),
    );
    if import.clicked() {
        editor.show_import = true;
        editor.show_export = false;
    }
    let export = ui.add_enabled(
        has_project,
        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "export_data.svg")), tr!(literal = "Export..."))
            .id_salt("export")
            .button_side(side),
    );
    if export.clicked() {
        editor.show_import = false;
        editor.show_export = true;
    }

    let undo = ui.add_enabled(
        editor.can_undo,
        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "undo.svg")), format!("{} ({PRIMARY_MODIFIER}Z)", tr!(literal = "Undo")))
            .id_salt("undo")
            .button_side(side),
    );
    if undo.clicked() {
        commands.push(UiCommand::Undo);
    }
    let redo = ui.add_enabled(
        editor.can_redo,
        ToolbarButton::new(
            egui::Image::new(themed_icon!(ui, "redo.svg")),
            format!("{} ({PRIMARY_MODIFIER}{SHIFT_MODIFIER}Z)", tr!(literal = "Redo")),
        )
        .id_salt("redo")
        .button_side(side),
    );
    if redo.clicked() {
        commands.push(UiCommand::Redo);
    }
}

/// The centre run: every workspace gets the working elevation, alongside any
/// settings belonging specifically to that workspace.
fn draw_centre_settings(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    match editor.active_workspace {
        Workspace::Production => draw_drawing_settings(ui, editor, project),
        Workspace::DrillAndBlast => {
            draw_blast_settings(ui, editor, project, commands);
            centre_part(ui);
            draw_z_setting(ui, editor);
        }
        Workspace::Geology => {
            ui.spacing_mut().item_spacing.x = CENTRE_LABEL_GAP;
            draw_z_setting(ui, editor);
        }
    }
}

fn centre_part(ui: &mut egui::Ui) {
    ui.add_space(CENTRE_ITEM_GAP - CENTRE_LABEL_GAP);
}

fn draw_z_setting(ui: &mut egui::Ui, editor: &mut EditorState) {
    let response = MenuFieldF64::new(tr!(literal = "Z:"), &mut editor.z_input, f64::MIN..=f64::MAX)
        .width(80.0)
        .suffix(tr!(literal = "m"))
        .show_inline(ui);
    if response.changed() && editor.z_input.is_finite() {
        editor.z_level = editor.z_input;
    }
}

/// A name as one of the centre combos shows it, cut to the room the combo
/// leaves its text rather than let to widen the fixed-width run.
///
/// Measured rather than counted in characters: a fixed character budget has to
/// assume the widest name, and so cuts every other one short of the box. egui's
/// own truncation is no use here either - it lays the text out against
/// `available_width`, which in the centre band is the whole gap between the
/// clusters, so the combo would grow past [`SELECTOR_COMBO_WIDTH`] rather than
/// elide.
fn elide(ui: &egui::Ui, name: &str) -> String {
    // What the combo has left once its own padding and the dropdown arrow are
    // out - `egui::ComboBox` lays the selected text out inside exactly this.
    let spacing = ui.spacing();
    let room = SELECTOR_COMBO_WIDTH - 2.0 * spacing.button_padding.x - spacing.icon_spacing - spacing.icon_width;
    let font = egui::TextStyle::Button.resolve(ui.style());

    ui.ctx().fonts_mut(|fonts| {
        let full: f32 = name.chars().map(|c| fonts.glyph_width(&font, c)).sum();
        if full <= room {
            return name.to_owned();
        }
        let room = room - fonts.glyph_width(&font, '…');
        let mut kept = String::new();
        let mut used = 0.0;
        for c in name.chars() {
            used += fonts.glyph_width(&font, c);
            if used > room {
                break;
            }
            kept.push(c);
        }
        kept.push('…');
        kept
    })
}

/// What the Drill & Blast tools act on: the drill hole dataset being edited,
/// tied in and simulated. It is also what the workspace can select - see
/// `App::selectable_drill_holes` - so changing it drops the selection the
/// outgoing dataset was holding.
///
/// Only loaded datasets are offered - a closed one has no holes in the scene
/// to work on - and a selection that stops being loaded reads as "None" here
/// until another is picked, the same way the layer combo above treats a layer
/// that has gone.
fn draw_blast_settings(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    ui.spacing_mut().item_spacing.x = CENTRE_LABEL_GAP;
    let previous = editor.active_drill_hole;

    ui.label(tr!(literal = "Drill Holes:"));
    let none = tr!(literal = "None");
    let selected = editor
        .active_drill_hole
        .and_then(|id| project.drill_holes.iter().find(|dataset| dataset.id == id && dataset.is_loaded))
        .map(|dataset| dataset.name.as_str())
        .unwrap_or_else(|| none.as_str());
    egui::ComboBox::from_id_salt("drill_hole_combo_box")
        .selected_text(elide(ui, selected))
        .width(SELECTOR_COMBO_WIDTH)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut editor.active_drill_hole, None, tr!(literal = "None"));
            for dataset in project.drill_holes.iter().filter(|dataset| dataset.is_loaded) {
                ui.selectable_value(&mut editor.active_drill_hole, Some(dataset.id), &dataset.name);
            }
        });
    // Only holes in the active dataset can be selected, so a selection made
    // in the outgoing one has nothing left to act on.
    if editor.active_drill_hole != previous {
        commands.push(UiCommand::ClearSelection);
        // A tie-in runs between holes of one dataset, so it goes with it.
        editor.end_tie_chain();
        editor.initiation_dialog = None;
    }
}

/// What the drawing tools will use next: layer, elevation, line colour, fill.
fn draw_drawing_settings(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView) {
    // The run's own spacing holds a label to its control; the settings are
    // parted from each other by [`CENTRE_ITEM_GAP`] on top of it, so "Z:" stays
    // against its field while the four settings read as four.
    ui.spacing_mut().item_spacing.x = CENTRE_LABEL_GAP;

    ui.label(tr!(literal = "Layer:"));
    let active_layers = project
        .projects
        .iter()
        .find(|entry| entry.is_active)
        .map(|entry| entry.layers.as_slice())
        .unwrap_or_default();
    let none = tr!(literal = "None");
    let selected_layer = editor
        .active_layer
        .and_then(|id| active_layers.iter().find(|layer| layer.id == id && layer.is_loaded))
        .map(|layer| layer.name.as_str())
        .unwrap_or_else(|| none.as_str());
    egui::ComboBox::from_id_salt("layer_combo_box")
        .selected_text(elide(ui, selected_layer))
        .width(SELECTOR_COMBO_WIDTH)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut editor.active_layer, None, tr!(literal = "None"));
            for layer in active_layers.iter().filter(|layer| layer.is_loaded) {
                ui.selectable_value(&mut editor.active_layer, Some(layer.id), &layer.name);
            }
        });

    centre_part(ui);
    draw_z_setting(ui, editor);

    centre_part(ui);
    ui.label(tr!(literal = "Color:"));
    let mut line_c32 = rgba_to_color32(editor.tool_line_color);
    if ColorSquarePicker::new(&mut line_c32).show(ui).changed() {
        editor.tool_line_color = color32_to_rgba(line_c32);
    }

    centre_part(ui);
    ui.label(tr!(literal = "Fill:"));
    HatchPicker::new(&mut editor.tool_hatch, rgba_to_color32(editor.tool_line_color)).show(ui);
}

/// The view controls, at the far end of the bar.
///
/// Drawn into a right-to-left layout: the first button added takes the strip's
/// right edge and each one after it is placed to the left of the last, so the
/// run is written here in the reverse of the order it reads on screen.
///
/// The controls every workspace carries are added first, so they hold the same
/// place against the window's edge whichever tab is open - how the scene is
/// drawn, then what the camera is asked - and whatever the open workspace adds
/// is placed to the left of them, parted from them by [`divider`]. Drill &
/// Blast's reviews of the fired pattern are the only such run so far - see
/// [`draw_blast_view_tools`]; production adds nothing here, its own tools being
/// the toolbar and the design menus.
fn draw_view_tools(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>, side: f32) {
    draw_scene_modes(ui, editor, commands, side);
    draw_camera_tools(ui, editor, commands, side);

    if editor.active_workspace == Workspace::DrillAndBlast {
        divider(ui, side);
        draw_blast_view_tools(ui, editor, project, side);
    }
}

/// Part the run every workspace carries from the run this one adds.
///
/// A hairline rather than a gap: the bar's buttons meet each other, so a space
/// alone would read as a missing button. Held clear of the strip's top and
/// bottom so it marks a seam between two runs rather than looking like an edge
/// of the bar itself.
fn divider(ui: &mut egui::Ui, side: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0 + 2.0 * DIVIDER_GAP, side), egui::Sense::hover());
    let visuals = ui.visuals();
    let color = crate::ui::widgets::shifted(visuals.panel_fill, if visuals.dark_mode { 26 } else { -46 });
    let painter = ui.painter();
    let x = painter.round_to_pixel_center(rect.center().x);
    painter.vline(x, (rect.top() + DIVIDER_INSET)..=(rect.bottom() - DIVIDER_INSET), egui::Stroke::new(1.0, color));
}

/// What every workspace asks of the camera: how far the scene is stretched
/// upward, and the two ways back to a view of all of it.
fn draw_camera_tools(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, side: f32) {
    let exaggeration = ui.add(
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("vertical_exaggeration.svg")),
            format!("{} ({:.2}×)", tr!(literal = "Vertical Exaggeration"), editor.vertical_exaggeration),
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
        ToolbarButton::new(egui::Image::new(unthemed_icon!("zoom_to_extents.svg")), tr!(literal = "Zoom to Extents"))
            .id_salt("zoom_to_extents")
            .button_side(side),
    );
    if zoom.clicked() {
        commands.push(UiCommand::ZoomToExtents);
    }

    let reset = ui.add(
        ToolbarButton::new(egui::Image::new(unthemed_icon!("reset_view.svg")), tr!(literal = "Reset View"))
            .id_salt("reset_view")
            .button_side(side),
    );
    if reset.clicked() {
        commands.push(UiCommand::ResetView);
    }
}

/// How the scene is drawn and got at, which every workspace carries.
///
/// Flying, the slice view, x-ray, the wireframes and the points are all ways of
/// reading what is already in the scene rather than tools for drawing it, so
/// they are as useful over a blast pattern or a geological model as over a pit
/// design and they stay on the bar across the tabs - which also means a mode is
/// never left running with the button that turns it off gone from the window.
fn draw_scene_modes(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, side: f32) {
    // A right-to-left layout adds each button to the left of the last, so the
    // run is added in reverse to read left to right on screen.
    let fly = ui.add(
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("fly_mode.svg")),
            if editor.fly_mode_enabled {
                tr!(literal = "Disable Flying Mode")
            } else {
                tr!(literal = "Enable Flying Mode")
            },
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
            if editor.slice_mode_enabled {
                tr!(literal = "Exit Slice View")
            } else {
                tr!(literal = "Vertical Slice View")
            },
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
            if editor.topology_wireframes_enabled {
                tr!(literal = "Hide Wireframes")
            } else {
                tr!(literal = "Show Wireframes")
            },
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
            if editor.show_points {
                tr!(literal = "Hide Points")
            } else {
                tr!(literal = "Show Points")
            },
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
            if editor.xray_enabled {
                tr!(literal = "Disable X-Ray Vision")
            } else {
                tr!(literal = "Enable X-Ray Vision")
            },
        )
        .id_salt("xray")
        .button_side(side)
        .selected(editor.xray_enabled),
    );
    if xray.clicked() {
        editor.xray_enabled = !editor.xray_enabled;
    }
}

/// The blast reviews, the one run a single workspace adds to the view controls:
/// Drill & Blast's own, left of the divider.
///
/// Each of these reads the fired pattern back - how much burden each hole is
/// left to move, when the ground around it lifts, the shot played through -
/// so all three act on the dataset the centre run names, and none of them has
/// anything to work on until one is picked there.
///
/// Placeholders: the buttons, their icons and their enablement are here, but
/// nothing is wired behind them yet.
fn draw_blast_view_tools(ui: &mut egui::Ui, editor: &EditorState, project: &UiProjectView, side: f32) {
    // The centre run shows "None" for a dataset that is no longer loaded, and
    // these follow it: a stale id is not something to review.
    let has_active_dataset = editor
        .active_drill_hole
        .is_some_and(|id| project.drill_holes.iter().any(|dataset| dataset.id == id && dataset.is_loaded));

    // A right-to-left layout adds each button to the left of the last, so the
    // run is added in reverse to read left to right on screen.
    ui.add_enabled(
        has_active_dataset,
        ToolbarButton::new(egui::Image::new(unthemed_icon!("blast_timeline.svg")), tr!(literal = "Blast Timeline [PLACEHOLDER]"))
            .id_salt("blast_timeline")
            .button_side(side),
    );

    ui.add_enabled(
        has_active_dataset,
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("contours_of_equal_time.svg")),
            tr!(literal = "Contours of Equal Time [PLACEHOLDER]"),
        )
        .id_salt("contours_of_equal_time")
        .button_side(side),
    );

    ui.add_enabled(
        has_active_dataset,
        ToolbarButton::new(
            egui::Image::new(unthemed_icon!("burden_relief_heatmap.svg")),
            tr!(literal = "Burden Relief Heatmap [PLACEHOLDER]"),
        )
        .id_salt("burden_relief_heatmap")
        .button_side(side),
    );
}
