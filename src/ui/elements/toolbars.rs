//! Two toolbar panels: left (drawing tools) and bottom (cursor mode,
//! measuring, task progress).
//!
//! The project actions, the layer/Z/line/fill settings and the view controls
//! that used to be a strip over the explorer, a strip over the scene and a
//! floating tile at the scene's right edge are one row now - see
//! [`crate::ui::elements::viewport_bar`].

use crate::ui::{
    EditorState,
    state::{ActiveTool, CursorMode, UiCommand},
    themed_icon, unthemed_icon,
    widgets::toolbar::{TOOL_CELL_SIZE, ToolbarButton},
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
}

/// One button in the drawing toolbar's run.
struct LeftTool {
    icon: egui::Image<'static>,
    tooltip: &'static str,
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
    let tool = |icon: egui::ImageSource<'static>, tooltip: &'static str, tool: ActiveTool| LeftTool {
        icon: egui::Image::new(icon),
        tooltip,
        action: LeftToolAction::Tool(tool),
        enabled: editing_enabled,
    };
    vec![
        LeftTool {
            icon: egui::Image::new(unthemed_icon!("layer.svg")),
            tooltip: "New layer",
            action: LeftToolAction::NewLayer,
            enabled: project_active,
        },
        tool(themed_icon!(ui, "create_point.svg"), "Create Point", ActiveTool::MakePoint),
        tool(themed_icon!(ui, "create_line.svg"), "Create Line", ActiveTool::MakeLine),
        tool(themed_icon!(ui, "create_polyline.svg"), "Create Polyline", ActiveTool::MakePoly),
        tool(themed_icon!(ui, "create_circle.svg"), "Create Circle", ActiveTool::MakeCircle),
        tool(unthemed_icon!("create_text.svg"), "Create Text", ActiveTool::MakeText),
        tool(themed_icon!(ui, "move_element.svg"), "Move", ActiveTool::Move),
        tool(themed_icon!(ui, "offset_element.svg"), "Offset", ActiveTool::OffsetElement),
        tool(themed_icon!(ui, "drape_element.svg"), "Drape to Topology", ActiveTool::DrapeToTopology),
        tool(unthemed_icon!("auto_bench.svg"), "Auto-Bench", ActiveTool::BatterBermOffset),
        tool(themed_icon!(ui, "relimit_line.svg"), "Relimit Line", ActiveTool::RelimitLine),
        tool(themed_icon!(ui, "fuse_lines.svg"), "Fuse Polylines", ActiveTool::FuseIntoPolyline),
        tool(themed_icon!(ui, "chamfer_corners.svg"), "Chamfer Polyline Corners", ActiveTool::Chamfer),
        tool(themed_icon!(ui, "create_bezier.svg"), "Bezier Polyline", ActiveTool::Bezier),
        tool(themed_icon!(ui, "split_at_points.svg"), "Split Polyline At Points", ActiveTool::SplitAtPoints),
        tool(unthemed_icon!("explode_polyline.svg"), "Explode Polyline to Lines", ActiveTool::ExplodePolyline),
        tool(unthemed_icon!("delete_element.svg"), "Delete Points", ActiveTool::DeletePoints),
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
    };
    let button = ToolbarButton::new(tool.icon.clone(), tool.tooltip).id_salt(("left_tool", tool.tooltip)).selected(selected);
    if !ui.add_enabled_ui(tool.enabled, |ui| ui.add(button)).inner.clicked() {
        return;
    }
    match tool.action {
        LeftToolAction::NewLayer => {
            editor.new_layer_dialog_open = !editor.new_layer_dialog_open;
            if editor.new_layer_dialog_open {
                editor.new_layer_name = "design".to_owned();
                commands.push(UiCommand::SetActiveTool(ActiveTool::None));
            }
        }
        LeftToolAction::Tool(active) => commands.push(UiCommand::SetActiveTool(active)),
    }
}

/// Draw the drawing tools down a docked column between the explorer and the
/// scene, and return what it claimed.
///
/// A panel rather than tiles floating over the viewport, so the tools sit
/// flush against the scene's edge and carry the same chrome as every other
/// panel: the column is one region, running the full height the panels around
/// it leave, with its run of cells at the top.
pub(crate) fn draw_left_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, editing_enabled: bool, project_active: bool, commands: &mut Vec<UiCommand>) -> egui::Rect {
    let tools = left_tools(ui, editing_enabled, project_active);
    // The run wraps into further columns rather than off the bottom of a short
    // window, and a panel claims its width before anything is drawn in it - so
    // the packing is arithmetic, every cell being one square.
    let margins = 2.0 * crate::ui::chrome::margin(ui.ctx());
    // A column is filled before the next is started - ten tools in room for
    // six wrap 6-4, not 5-5 - so the toolbar only reaches as far across the
    // window as it has to.
    let rows = (((ui.available_height() - margins) / TOOL_CELL_SIZE) as usize).clamp(1, tools.len());
    let columns = tools.len().div_ceil(rows);
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
                    cursor_mode_button(ui, egui::Image::new(themed_icon!(ui, "cursor_select.svg")), "Cursor: Regular", editor, CursorMode::Select, side);

                    cursor_mode_button(
                        ui,
                        egui::Image::new(themed_icon!(ui, "snap_to_surface.svg")),
                        "Cursor: Snap to surface",
                        editor,
                        CursorMode::SnapToSurface,
                        side,
                    );

                    cursor_mode_button(
                        ui,
                        egui::Image::new(themed_icon!(ui, "snap_to_line.svg")),
                        "Cursor: Snap to line",
                        editor,
                        CursorMode::SnapToLine,
                        side,
                    );

                    cursor_mode_button(
                        ui,
                        egui::Image::new(themed_icon!(ui, "snap_to_point.svg")),
                        "Cursor: Snap to point",
                        editor,
                        CursorMode::SnapToPoint,
                        side,
                    );

                    ui.add_space(12.);

                    ui.add_enabled_ui(!editor.fly_mode_enabled, |ui| {
                        tool_button(
                            ui,
                            egui::Image::new(themed_icon!(ui, "measure_distance.svg")),
                            "Measure distance",
                            editor,
                            commands,
                            ActiveTool::MeasureDistance,
                            side,
                        );

                        tool_button(
                            ui,
                            egui::Image::new(themed_icon!(ui, "measure_batter_angle.svg")),
                            "Measure batter angle",
                            editor,
                            commands,
                            ActiveTool::MeasureBatterAngle,
                            side,
                        );
                    });

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

/// Draw a cursor mode button; sets `editor.cursor_mode` on click.
pub(crate) fn cursor_mode_button(ui: &mut egui::Ui, icon: egui::Image<'static>, tooltip: &str, editor: &mut EditorState, mode: CursorMode, side: f32) -> egui::Response {
    let selected = editor.cursor_mode == mode;
    let response = ui.add(ToolbarButton::new(icon, tooltip).id_salt(("cursor_mode", tooltip)).button_side(side).selected(selected));

    if response.clicked() {
        editor.cursor_mode = mode;
    }

    response
}
