//! Two toolbar panels: left (drawing tools) and bottom (cursor mode,
//! measuring, task progress).
//!
//! The project actions, the layer/Z/line/fill settings and the view controls
//! that used to be a strip over the explorer, a strip over the scene and a
//! floating tile at the scene's right edge are one row now - see
//! [`crate::ui::elements::viewport_bar`].

use crate::{
    i18n::tr,
    ui::{
        EditorState, UiProjectView,
        state::{ActiveTool, BlastCursor, CursorMode, UiCommand, Workspace},
        themed_icon, unthemed_icon,
        widgets::toolbar::{TOOL_CELL_SIZE, ToolbarButton},
    },
};

/// Height claimed by the bottom toolbar, including its chrome margins.
pub(crate) fn bottom_toolbar_height(ctx: &egui::Context) -> f32 {
    TOOL_CELL_SIZE + 2.0 * crate::ui::chrome::margin(ctx)
}

/// Id of the drawing toolbar's column panel.
pub(crate) const LEFT_TOOLBAR_PANEL_ID: &str = "left_toolbar_panel";

/// What one of the drawing toolbar's buttons does when clicked.
enum LeftToolAction {
    /// Open or close the new layer dialog.
    NewLayer,
    /// Make this the active tool.
    Tool(ActiveTool),
    /// A tool whose button is drawn but has nothing behind it yet.
    Placeholder,
}

/// One button in the drawing toolbar's run.
struct LeftTool {
    icon: egui::Image<'static>,
    tooltip: String,
    action: LeftToolAction,
    /// Whether the tool can be used at all this frame.
    enabled: bool,
}

/// The drawing tools in the order they are drawn: the project action, the
/// creation tools, the transform tools, the polyline edits, and the
/// destructive one.
///
/// One flat list rather than clusters: the column is a single run of cells, so
/// what a tool belongs to is its neighbours' business, not a tile's.
fn left_tools(ui: &egui::Ui, editing_enabled: bool, project_active: bool) -> Vec<LeftTool> {
    let tool = |icon: egui::ImageSource<'static>, tooltip: String, tool: ActiveTool| LeftTool {
        icon: egui::Image::new(icon),
        tooltip,
        action: LeftToolAction::Tool(tool),
        enabled: editing_enabled,
    };
    vec![
        LeftTool {
            icon: egui::Image::new(unthemed_icon!("layer.svg")),
            tooltip: tr!(literal = "New Layer"),
            action: LeftToolAction::NewLayer,
            enabled: project_active,
        },
        tool(themed_icon!(ui, "create_point.svg"), tr!(literal = "Create Point"), ActiveTool::MakePoint),
        tool(themed_icon!(ui, "create_line.svg"), tr!(literal = "Create Line"), ActiveTool::MakeLine),
        tool(themed_icon!(ui, "create_polyline.svg"), tr!(literal = "Create Polyline"), ActiveTool::MakePoly),
        tool(themed_icon!(ui, "create_circle.svg"), tr!(literal = "Create Circle"), ActiveTool::MakeCircle),
        tool(unthemed_icon!("create_text.svg"), tr!(literal = "Create Text"), ActiveTool::MakeText),
        tool(themed_icon!(ui, "move_element.svg"), tr!(literal = "Move Design"), ActiveTool::Move),
        tool(themed_icon!(ui, "offset_element.svg"), tr!(literal = "Offset"), ActiveTool::OffsetElement),
        tool(themed_icon!(ui, "drape_element.svg"), tr!(literal = "Drape to Topology"), ActiveTool::DrapeToTopology),
        tool(unthemed_icon!("auto_bench.svg"), tr!(literal = "Auto-Bench"), ActiveTool::BatterBermOffset),
        tool(themed_icon!(ui, "relimit_line.svg"), tr!(literal = "Relimit Line"), ActiveTool::RelimitLine),
        tool(themed_icon!(ui, "create_bezier.svg"), tr!(literal = "Bezier Polyline"), ActiveTool::Bezier),
        tool(themed_icon!(ui, "chamfer_corners.svg"), tr!(literal = "Chamfer Polyline Corners"), ActiveTool::Chamfer),
        tool(themed_icon!(ui, "fuse_lines.svg"), tr!(literal = "Fuse Polylines"), ActiveTool::FuseIntoPolyline),
        tool(
            themed_icon!(ui, "split_at_points.svg"),
            tr!(literal = "Split Polyline At Points"),
            ActiveTool::SplitAtPoints,
        ),
        tool(
            unthemed_icon!("explode_polyline.svg"),
            tr!(literal = "Explode Polyline to Lines"),
            ActiveTool::ExplodePolyline,
        ),
        tool(unthemed_icon!("delete_element.svg"), tr!(literal = "Delete Points"), ActiveTool::DeletePoints),
    ]
}

/// The Drill & Blast tools, in the order they are drawn: lay a pattern out,
/// nudge its holes, then say where it starts.
///
/// Move Collar is the one with something behind it; the other two are
/// placeholders - the cells, their icons and their enablement are here, but
/// nothing is wired to them yet.
fn blast_tools(ui: &egui::Ui, project: &UiProjectView, editor: &EditorState, project_active: bool) -> Vec<LeftTool> {
    // Setting the initiation point acts on the pattern the viewport bar's
    // centre run names, and reads it the same way that run does: a dataset
    // that is no longer loaded shows as "None" there and is nothing to act on
    // here. Laying a new pattern out is what fills that combo, so it asks only
    // for somewhere to put one.
    let has_active_dataset = editor
        .active_drill_hole
        .is_some_and(|id| project.drill_holes.iter().any(|dataset| dataset.id == id && dataset.is_loaded));
    vec![
        LeftTool {
            icon: egui::Image::new(themed_icon!(ui, "create_drill_pattern.svg")),
            tooltip: tr!(literal = "Create Drill Pattern [PLACEHOLDER]"),
            action: LeftToolAction::Placeholder,
            enabled: project_active,
        },
        LeftTool {
            // The same mark production's Move Design carries: one translate
            // gesture, drawn the same way whichever discipline is running it.
            icon: egui::Image::new(themed_icon!(ui, "move_element.svg")),
            tooltip: tr!(literal = "Move Collar"),
            action: LeftToolAction::Tool(ActiveTool::MoveCollar),
            enabled: has_active_dataset,
        },
        LeftTool {
            icon: egui::Image::new(unthemed_icon!("initiation_point.svg")),
            tooltip: tr!(literal = "Set Initiation Point [PLACEHOLDER]"),
            action: LeftToolAction::Placeholder,
            enabled: has_active_dataset,
        },
    ]
}

/// Draw one cell of the drawing toolbar's run.
///
/// A tool greys out on its own rather than the run being wrapped in a single
/// `add_enabled_ui`: the run is one block now, and whether a cell is usable is
/// a question about that tool rather than about the toolbar.
fn draw_left_tool(ui: &mut egui::Ui, tool: &LeftTool, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let selected = match tool.action {
        LeftToolAction::NewLayer => editor.new_layer_dialog_open,
        LeftToolAction::Tool(active) => editor.active_tool == active,
        LeftToolAction::Placeholder => false,
    };
    let button = ToolbarButton::new(tool.icon.clone(), tool.tooltip.as_str())
        .id_salt(("left_tool", tool.tooltip.as_str()))
        .selected(selected);
    if !ui.add_enabled_ui(tool.enabled, |ui| ui.add(button)).inner.clicked() {
        return;
    }
    match tool.action {
        LeftToolAction::NewLayer => {
            editor.new_layer_dialog_open = !editor.new_layer_dialog_open;
            if editor.new_layer_dialog_open {
                editor.new_layer_name = tr!(literal = "design");
                commands.push(UiCommand::SetActiveTool(ActiveTool::None));
            }
        }
        LeftToolAction::Tool(active) => commands.push(UiCommand::SetActiveTool(active)),
        LeftToolAction::Placeholder => {}
    }
}

/// Draw the drawing tools down a docked column between the explorer and the
/// scene, and return what it claimed.
///
/// A panel rather than tiles floating over the viewport, so the tools sit
/// flush against the scene's edge and carry the same chrome as every other
/// panel: the column is one region, running the full height the panels around
/// it leave, with its run of cells at the top.
///
/// Each workspace fills the column with its own run - production's drawing
/// tools, Drill & Blast's pattern tools - and a workspace with none leaves it
/// standing and empty, one cell wide, rather than taking it off the window:
/// it is where that discipline's own tools will go, and the workspace tabs are
/// not a reason for the window to change shape under the pointer.
pub(crate) fn draw_left_toolbar(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    project: &UiProjectView,
    editing_enabled: bool,
    project_active: bool,
    commands: &mut Vec<UiCommand>,
) -> egui::Rect {
    let tools = match editor.active_workspace {
        workspace if workspace.has_production_tools() => left_tools(ui, editing_enabled, project_active),
        Workspace::DrillAndBlast => blast_tools(ui, project, editor, project_active),
        _ => Vec::new(),
    };
    // The run wraps into further columns rather than off the bottom of a short
    // window, and a panel claims its width before anything is drawn in it - so
    // the packing is arithmetic, every cell being one square.
    let margins = 2.0 * crate::ui::chrome::margin(ui.ctx());
    // A column is filled before the next is started - ten tools in room for
    // six wrap 6-4, not 5-5 - so the toolbar only reaches as far across the
    // window as it has to. An empty run still claims the one column it would
    // have started, so the column has a width to be a column at.
    let cells = tools.len().max(1);
    let rows = (((ui.available_height() - margins) / TOOL_CELL_SIZE) as usize).clamp(1, cells);
    let columns = cells.div_ceil(rows);
    let width = columns as f32 * TOOL_CELL_SIZE + margins;

    egui::Panel::left(LEFT_TOOLBAR_PANEL_ID)
        .resizable(false)
        .show_separator_line(crate::ui::chrome::show_separator_line(ui))
        .exact_size(width)
        // No padding on the region: the square cell fills run edge to edge,
        // and the region chrome masks whichever ones reach its corners.
        .frame(crate::ui::chrome::region_frame(ui).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                // Nothing between the columns: a wrap carries on down the next
                // one rather than starting a block of its own.
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                for column in tools.chunks(rows) {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for tool in column {
                            draw_left_tool(ui, tool, editor, commands);
                        }
                    });
                }
            });
        })
        .response
        .rect
}

/// Draw the bottom toolbar (cursor mode, measure distance).
///
/// Visibility and locking are per-item concerns now, so they live on the
/// explorer's rows rather than as whole-scene toolbar actions: see
/// `ExplorerEntry::toggles`.
///
/// The snapping cursor modes go with the drawing tools they snap for, and the
/// two measurements are of a pit being designed, so that run belongs to the
/// production workspace - see
/// [`crate::ui::state::Workspace::has_production_tools`]. Drill & Blast has a
/// cursor run of its own in its place; a workspace with neither is left with
/// the strip and what is running on it.
pub(crate) fn draw_bottom_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) -> egui::Rect {
    let claimed = bottom_toolbar_height(ui.ctx());
    egui::Panel::bottom("bottom_tools_strip")
        .resizable(false)
        .show_separator_line(crate::ui::chrome::show_separator_line(ui))
        .exact_size(claimed)
        // The cells meet the region on every side; the chrome painted after
        // them is what rounds whichever fill reaches an outer corner.
        .frame(crate::ui::chrome::region_frame(ui).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| {
            let side = ui.available_height();
            let contents_id = ui.make_persistent_id("bottom_toolbar_buttons");
            ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if editor.active_workspace == Workspace::DrillAndBlast {
                        draw_blast_cursors(ui, editor, side);
                    }

                    if editor.active_workspace.has_production_tools() {
                        draw_cursor_modes(ui, editor, side);
                        ui.add_space(12.);

                        ui.add_enabled_ui(!editor.fly_mode_enabled, |ui| {
                            tool_button(
                                ui,
                                egui::Image::new(themed_icon!(ui, "measure_distance.svg")),
                                tr!(literal = "Measure Distance").as_str(),
                                editor,
                                commands,
                                ActiveTool::MeasureDistance,
                                side,
                            );

                            tool_button(
                                ui,
                                egui::Image::new(themed_icon!(ui, "measure_batter_angle.svg")),
                                tr!(literal = "Strike and Dip").as_str(),
                                editor,
                                commands,
                                ActiveTool::MeasureBatterAngle,
                                side,
                            );
                        });
                    }

                    // Task progress hugs the right end of the strip, out of the
                    // way of the tools and with room to say what is running -
                    // the status bar had neither.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        crate::ui::widgets::progress::draw_task_progress(ui, editor);
                    });
                });
            });
        })
        .response
        .rect
}

/// The run of cursor modes at the head of the bottom toolbar: what a click in
/// the scene snaps to.
fn draw_cursor_modes(ui: &mut egui::Ui, editor: &mut EditorState, side: f32) {
    cursor_mode_button(
        ui,
        egui::Image::new(themed_icon!(ui, "cursor_select.svg")),
        tr!(literal = "Cursor: Regular").as_str(),
        editor,
        CursorMode::Select,
        side,
    );

    cursor_mode_button(
        ui,
        egui::Image::new(themed_icon!(ui, "snap_to_surface.svg")),
        tr!(literal = "Cursor: Snap to Surface").as_str(),
        editor,
        CursorMode::SnapToSurface,
        side,
    );

    cursor_mode_button(
        ui,
        egui::Image::new(themed_icon!(ui, "snap_to_line.svg")),
        tr!(literal = "Cursor: Snap to Line").as_str(),
        editor,
        CursorMode::SnapToLine,
        side,
    );

    cursor_mode_button(
        ui,
        egui::Image::new(themed_icon!(ui, "snap_to_point.svg")),
        tr!(literal = "Cursor: Snap to Point").as_str(),
        editor,
        CursorMode::SnapToPoint,
        side,
    );
}

/// The Drill & Blast workspace's cursor run, standing where production's
/// snapping modes do.
///
/// Two so far: a plain pick, and the mode a round is tied in under. The tie-in
/// mark is the same one a delay product's card is read by - see
/// `products::paint_tie_in_mark` - so the tool and the palette it arms say the
/// same thing.
fn draw_blast_cursors(ui: &mut egui::Ui, editor: &mut EditorState, side: f32) {
    blast_cursor_button(
        ui,
        egui::Image::new(themed_icon!(ui, "cursor_select.svg")),
        tr!(literal = "Select").as_str(),
        editor,
        BlastCursor::Select,
        side,
    );

    blast_cursor_button(
        ui,
        egui::Image::new(themed_icon!(ui, "tie_holes.svg")),
        tr!(literal = "Tie Holes").as_str(),
        editor,
        BlastCursor::TieHoles,
        side,
    );
}

/// Draw a tool button in a horizontal toolbar; sets `editor.active_tool` on click.
pub(crate) fn tool_button(
    ui: &mut egui::Ui,
    icon: egui::Image<'static>,
    tooltip: &str,
    editor: &mut EditorState,
    commands: &mut Vec<UiCommand>,
    tool: ActiveTool,
    side: f32,
) -> egui::Response {
    let selected = editor.active_tool == tool;
    let response = ui.add(ToolbarButton::new(icon, tooltip).id_salt(("tool", tooltip)).button_side(side).selected(selected));

    if response.clicked() {
        commands.push(UiCommand::SetActiveTool(tool));
    }

    response
}

/// Draw a cursor mode button; sets production's cursor on click.
pub(crate) fn cursor_mode_button(ui: &mut egui::Ui, icon: egui::Image<'static>, tooltip: &str, editor: &mut EditorState, mode: CursorMode, side: f32) -> egui::Response {
    let selected = editor.cursors.production == mode;
    let response = ui.add(ToolbarButton::new(icon, tooltip).id_salt(("cursor_mode", tooltip)).button_side(side).selected(selected));

    if response.clicked() {
        editor.cursors.production = mode;
    }

    response
}

/// Draw a Drill & Blast cursor button; sets that workspace's cursor on click.
pub(crate) fn blast_cursor_button(ui: &mut egui::Ui, icon: egui::Image<'static>, tooltip: &str, editor: &mut EditorState, cursor: BlastCursor, side: f32) -> egui::Response {
    let selected = editor.cursors.blast == cursor;
    let response = ui.add(ToolbarButton::new(icon, tooltip).id_salt(("blast_cursor", tooltip)).button_side(side).selected(selected));

    if response.clicked() {
        editor.cursors.blast = cursor;
    }

    response
}
