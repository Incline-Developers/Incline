//! Two toolbar panels: left (drawing tools) and bottom (cursor mode,
//! measuring, task progress).
//!
//! The bottom strip is three clusters like the viewport bar above it: the
//! measuring run at the left, task progress at the right, and the cursor modes
//! centred on the window between them.
//!
//! The project actions, the layer/Z/line/fill settings and the view controls
//! that used to be a strip over the explorer, a strip over the scene and a
//! floating tile at the scene's right edge are one row now - see
//! [`crate::ui::elements::viewport_bar`].

use crate::{
    i18n::{tr, tr_format},
    ui::{
        EditorState, UiProjectView,
        state::{ActiveTool, CursorMode, UiCommand, Workspace},
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

/// Clear space the centred cursor run keeps from the clusters either side of it.
const CENTRE_CLEARANCE: f32 = 16.0;

/// What one of the drawing toolbar's buttons does when clicked.
enum LeftToolAction {
    /// Open or close the new layer dialog.
    NewLayer,
    /// Make this the active tool.
    Tool(ActiveTool),
    /// Open or close the Drill & Blast pattern builder.
    DrillPattern,
}

/// One button in the drawing toolbar's run.
struct LeftTool {
    icon: egui::Image<'static>,
    tooltip: String,
    action: LeftToolAction,
    /// Whether the tool can be used at all this frame.
    enabled: bool,
    /// Said on hover in place of `tooltip` while the view, not the project, greys the cell.
    hint: Option<String>,
}

/// The drawing tools in the order they are drawn: the project action, the
/// creation tools, the transform tools, the polyline edits, and the
/// destructive one.
///
/// One flat list rather than clusters: the column is a single run of cells, so
/// what a tool belongs to is its neighbours' business, not a tile's.
fn left_tools(ui: &egui::Ui, editor: &EditorState, editing_enabled: bool, project_active: bool) -> Vec<LeftTool> {
    let tool = |icon: egui::ImageSource<'static>, tooltip: String, tool: ActiveTool| {
        let layer_ok = !tool.requires_active_layer() || editor.active_layer.is_some();
        let blocked_by_section = editor.slice_mode_enabled && tool.section_refuses();
        LeftTool {
            icon: egui::Image::new(icon),
            hint: (blocked_by_section && editing_enabled && layer_ok).then(|| tr_format!(literal = "%tool% - not available in the section view", tool = tooltip.as_str())),
            tooltip,
            action: LeftToolAction::Tool(tool),
            enabled: editing_enabled && layer_ok && !blocked_by_section,
        }
    };
    vec![
        LeftTool {
            icon: egui::Image::new(unthemed_icon!("layer.svg")),
            tooltip: tr!(literal = "New Layer"),
            action: LeftToolAction::NewLayer,
            enabled: project_active,
            hint: None,
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
/// nudge its holes, re-aim them, tie them together, then say where it starts.
fn blast_tools(ui: &egui::Ui, project: &UiProjectView, editor: &EditorState, editing_enabled: bool, project_active: bool) -> Vec<LeftTool> {
    // Setting the initiation point acts on the pattern the viewport bar's
    // centre run names, and reads it the same way that run does: a dataset
    // that is no longer loaded shows as "None" there and is nothing to act on
    // here. Laying a new pattern out is what fills that combo, so it asks only
    // for somewhere to put one.
    let has_active_dataset = editor
        .active_drill_hole
        .is_some_and(|id| project.drill_holes.iter().any(|dataset| dataset.id == id && dataset.is_loaded));
    let editing_enabled = editing_enabled && !editor.slice_mode_enabled;
    vec![
        LeftTool {
            icon: egui::Image::new(themed_icon!(ui, "create_drill_pattern.svg")),
            tooltip: tr!(literal = "Create Drill Pattern"),
            action: LeftToolAction::DrillPattern,
            enabled: project_active,
            hint: None,
        },
        LeftTool {
            // The same mark production's Move Design carries: one translate
            // gesture, drawn the same way whichever discipline is running it.
            icon: egui::Image::new(themed_icon!(ui, "move_element.svg")),
            tooltip: tr!(literal = "Move Collar"),
            action: LeftToolAction::Tool(ActiveTool::MoveCollar),
            enabled: editing_enabled,
            hint: None,
        },
        LeftTool {
            // Move Collar's counterpart: the same holes, turned instead of
            // shifted, so it sits directly beside it in the run.
            icon: egui::Image::new(themed_icon!(ui, "rotate_element.svg")),
            tooltip: tr!(literal = "Rotate Collar"),
            action: LeftToolAction::Tool(ActiveTool::RotateCollar),
            enabled: editing_enabled,
            hint: None,
        },
        LeftTool {
            icon: egui::Image::new(unthemed_icon!("tie_holes.svg")),
            tooltip: tr!(literal = "Tie Holes"),
            action: LeftToolAction::Tool(ActiveTool::TieHoles),
            enabled: editing_enabled && has_active_dataset,
            hint: None,
        },
        LeftTool {
            icon: egui::Image::new(unthemed_icon!("initiation_point.svg")),
            tooltip: tr!(literal = "Set Initiation Point"),
            action: LeftToolAction::Tool(ActiveTool::SetInitiationPoint),
            enabled: editing_enabled && has_active_dataset,
            hint: None,
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
        LeftToolAction::DrillPattern => editor.drill_pattern_open,
    };
    let button = ToolbarButton::new(tool.icon.clone(), tool.tooltip.as_str())
        .id_salt(("left_tool", tool.tooltip.as_str()))
        .selected(selected);
    let mut response = ui.add_enabled_ui(tool.enabled, |ui| ui.add(button)).inner;
    if let Some(hint) = tool.hint.as_deref() {
        response = response.on_disabled_hover_text(hint);
    }
    if !response.clicked() {
        return;
    }
    match tool.action {
        LeftToolAction::NewLayer => {
            editor.new_layer_dialog_open = !editor.new_layer_dialog_open;
            if editor.new_layer_dialog_open {
                editor.new_layer_name = tr!(literal = "Design");
                commands.push(UiCommand::SetActiveTool(ActiveTool::None));
            }
        }
        LeftToolAction::Tool(active) => commands.push(UiCommand::SetActiveTool(active)),
        LeftToolAction::DrillPattern => commands.push(UiCommand::ToggleCreateDrillPattern),
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
        workspace if workspace.has_production_tools() => left_tools(ui, editor, editing_enabled, project_active),
        Workspace::DrillAndBlast => blast_tools(ui, project, editor, editing_enabled, project_active),
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

/// Draw the bottom toolbar (cursor mode, measuring, task progress).
///
/// Visibility and locking are per-item concerns now, so they live on the
/// explorer's rows rather than as whole-scene toolbar actions: see
/// `ExplorerEntry::toggles`.
///
/// Every workspace shows and edits the same shared cursor mode. The two
/// measurements are of a pit being designed, so only that run belongs to the
/// production workspace - see
/// [`crate::ui::state::Workspace::has_production_tools`].
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
            // Where the middle of the *window* falls on this bar. The panel
            // starts where the explorer leaves off, so its own middle is not
            // the window's, and the cursor run is meant to sit under the middle
            // of the screen. Taken as an offset rather than an absolute, so the
            // run travels with the strip when the strip is scrolled.
            let centre_offset = ui.ctx().content_rect().center().x - ui.max_rect().left();
            // Stop narrowing and scroll under the wheel once the window is too
            // narrow for what is on the bar, rather than letting the clusters
            // run into each other - the same strip the two bars across the top
            // of the window use. See `elements::bar_strip`.
            crate::ui::elements::bar_strip(ui, "bottom_toolbar_strip", ui.available_height(), |ui, strip| {
                let side = strip.height();
                let contents_id = ui.make_persistent_id("bottom_toolbar_buttons");
                ui.scope_builder(egui::UiBuilder::new().id(contents_id).max_rect(strip), |ui| {
                    // Three clusters placed against the same strip, the way the
                    // viewport bar lays its own out - see [`cluster`] - so the
                    // centred run is not pushed along by what is beside it.
                    let left = cluster(ui, strip, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        draw_measure_tools(ui, editor, commands, side);
                    });
                    // Task progress hugs the right end of the strip, out of the
                    // way of the tools and with room to say what is running -
                    // the status bar had neither.
                    let right = cluster(ui, strip, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        crate::ui::widgets::progress::draw_task_progress(ui, editor);
                    });

                    // Held clear of the left cluster first and slid back from
                    // the right only where there is room, so the window centre
                    // landing behind a cluster crowds the run rather than
                    // hiding it - these are how the canvas is clicked at all.
                    // Opened to the right of where the run starts rather than
                    // sized to it, so that crowding never clips a button.
                    let width = cursor_modes_width(side);
                    let band_left = left.right() + CENTRE_CLEARANCE;
                    let band_right = right.left() - CENTRE_CLEARANCE;
                    let x = (strip.left() + centre_offset - width / 2.0).clamp(band_left, (band_right - width).max(band_left));
                    let run = egui::Rect::from_min_max(egui::pos2(x, strip.top()), egui::pos2(strip.right(), strip.bottom()));
                    cluster(ui, run, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        draw_cursor_modes(ui, editor, commands, side);
                    });

                    // The width the strip has to keep: the three clusters, the
                    // centre one with its clearance either side.
                    left.width() + CENTRE_CLEARANCE + width + CENTRE_CLEARANCE + right.width()
                })
                .inner
            });
        })
        .response
        .rect
}

/// Lay one of the bottom toolbar's clusters out over `rect`, and report what it
/// drew into.
///
/// The three are placed against the same strip rather than in sequence, so each
/// is given the rect it should align itself in and none of them consumes space
/// the next one wanted.
fn cluster(ui: &mut egui::Ui, rect: egui::Rect, layout: egui::Layout, add_contents: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(layout), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        add_contents(ui);
    })
    .response
    .rect
}

/// The measuring run, which only a workspace designing a pit has anything to
/// measure in - see [`crate::ui::state::Workspace::has_production_tools`].
fn draw_measure_tools(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, side: f32) {
    if !editor.active_workspace.has_production_tools() {
        return;
    }

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

/// The cursor modes in the order the centre run draws them.
const CURSOR_MODES: [CursorMode; 4] = [CursorMode::Select, CursorMode::SnapToSurface, CursorMode::SnapToLine, CursorMode::SnapToPoint];

/// Width of that run, which the strip needs before it has been laid out in
/// order to centre it. The buttons are square and sit flush against each other,
/// so it is theirs alone.
fn cursor_modes_width(side: f32) -> f32 {
    side * CURSOR_MODES.len() as f32
}

/// The centred run of cursor modes: what a click in the scene snaps to.
fn draw_cursor_modes(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, side: f32) {
    for mode in CURSOR_MODES {
        let (icon, tooltip) = match mode {
            CursorMode::Select => (themed_icon!(ui, "cursor_select.svg"), tr!(literal = "Cursor: Regular")),
            CursorMode::SnapToSurface => (themed_icon!(ui, "snap_to_surface.svg"), tr!(literal = "Cursor: Snap to Surface")),
            CursorMode::SnapToLine => (themed_icon!(ui, "snap_to_line.svg"), tr!(literal = "Cursor: Snap to Line")),
            CursorMode::SnapToPoint => (themed_icon!(ui, "snap_to_point.svg"), tr!(literal = "Cursor: Snap to Point")),
        };
        cursor_mode_button(ui, egui::Image::new(icon), tooltip.as_str(), editor, commands, mode, side);
    }
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

/// Draw a cursor mode button; sets the shared cursor on click. In Drill &
/// Blast, Regular also puts down the active tool so canvas input returns to
/// that workspace's individual-hole click and marquee selection path.
pub(crate) fn cursor_mode_button(
    ui: &mut egui::Ui,
    icon: egui::Image<'static>,
    tooltip: &str,
    editor: &mut EditorState,
    commands: &mut Vec<UiCommand>,
    mode: CursorMode,
    side: f32,
) -> egui::Response {
    let selected = editor.cursor_mode == mode;
    let response = ui.add(ToolbarButton::new(icon, tooltip).id_salt(("cursor_mode", tooltip)).button_side(side).selected(selected));

    if response.clicked() {
        editor.cursor_mode = mode;
        if editor.active_workspace == Workspace::DrillAndBlast && mode == CursorMode::Select {
            commands.push(UiCommand::SetActiveTool(ActiveTool::None));
        }
    }

    response
}
