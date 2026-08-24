//! Four toolbar panels: top (layer/Z/line/fill settings), left (drawing tools),
//! right (viewport controls), bottom (cursor mode, measuring, task progress).

use crate::ui::{
    EditorState, UiProjectView, color32_to_rgba, rgba_to_color32,
    state::{ActiveTool, CursorMode, UiCommand, UiProjectEntry},
    themed_icon, unthemed_icon,
    widgets::{
        menu::MenuFieldF64,
        toolbar::{ColorSquarePicker, HatchPicker, TOOL_CELL_SIZE, ToolCellButton, ToolbarButton, ToolbarGroup, tool_cell_run},
    },
};

pub(crate) const BOTTOM_TOOLBAR_HEIGHT: f32 = 32.0;

/// Height of a docked toolbar strip. The explorer's header and the viewport's
/// top toolbar share it, so their bottom rules meet as one line.
const TOOLBAR_STRIP_HEIGHT: f32 = 34.0;
/// Space between a docked strip's edge and its buttons.
const STRIP_MARGIN: i8 = 6;
/// Space between the bottom strip's edge and its buttons. Tighter than
/// [`STRIP_MARGIN`], and the same on every side, so the strip reads as a
/// sleeve around the run of buttons rather than a frame with a border.
const BOTTOM_STRIP_MARGIN: i8 = 2;
/// Gap between buttons in the same cluster of a docked strip.
const STRIP_BUTTON_GAP: f32 = 2.0;
/// Gap between two clusters of buttons in a docked strip. The floating tiles'
/// gap reads as more space than it is, because the tiles pad themselves; this
/// is the docked equivalent of that separation.
const STRIP_CLUSTER_GAP: f32 = 12.0;
/// Label for the primary shortcut modifier in tooltips. Spelled out rather
/// than drawn as a glyph, so it can't land as tofu in the bundled fonts.
const PRIMARY_MODIFIER: &str = if cfg!(target_os = "macos") { "Cmd+" } else { "Ctrl+" };
/// Label for the shift modifier in tooltips.
const SHIFT_MODIFIER: &str = "Shift+";

/// Gap above the view tools when the gizmo that usually sits over them is
/// hidden and they start at the top of the viewport.
pub(crate) const TOOLS_TOP_MARGIN: f32 = 10.0;

/// A viewport smaller than this in either direction has no room to float the
/// view tools over at all.
const MIN_TOOLS_VIEWPORT: f32 = 120.0;

/// Gap between the orientation gizmo and the view tools stacked under it. The
/// gizmo's own artwork stops short of its rect, so this is wider than it looks
/// on paper.
const VIEW_TOOLS_GIZMO_GAP: f32 = 26.0;

/// Cells in the view tools' tile: the camera moves, the display toggles and
/// the viewing modes, all on one surface.
const VIEW_TOOL_CELLS: usize = 8;

/// Draw the explorer's header: the project-wide actions, sitting above the
/// data tree rather than out over the viewport, and return what it claimed.
///
/// The floating view tools group their buttons onto tiles, because out there a
/// tile is all that separates a tool from the scene behind it. A docked strip
/// already has a surface, so it groups by spacing alone: related buttons close
/// together, a wider gap between clusters. The button metrics, rounding and
/// hover fills are shared, so the two toolbars read as one family. The strip
/// is a region of its own at the top of the explorer column, as tall as the
/// viewport's top toolbar beside it, so the two read as one row across the
/// split with the same gap under them.
pub(crate) fn draw_explorer_toolbar(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    project: &UiProjectView,
    commands: &mut Vec<UiCommand>,
    can_undo: bool,
    can_redo: bool,
) -> egui::Rect {
    egui::Panel::top("explorer_tools_strip")
        .resizable(false)
        .show_separator_line(false)
        .default_size(TOOLBAR_STRIP_HEIGHT)
        // Only the sides are padded: the strip is exactly as tall as its
        // buttons need, and the gap around the region is its spacing.
        .frame(crate::ui::chrome::region_frame(ui.style()).inner_margin(egui::Margin::symmetric(STRIP_MARGIN, 0)))
        .show(ui, |ui| {
            let contents_id = ui.make_persistent_id("explorer_toolbar_buttons");
            ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = STRIP_BUTTON_GAP;

                    let has_unsaved = project.projects.iter().any(UiProjectEntry::needs_save);
                    let save = ui.add_enabled(
                        has_unsaved,
                        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "save_project.svg")), format!("Save Project ({PRIMARY_MODIFIER}S)")).id_salt("save_project"),
                    );
                    if save.clicked() {
                        commands.push(UiCommand::SaveProject);
                    }

                    // In the browser, saving only reaches IndexedDB, so taking
                    // a copy away with you is a project action of its own.
                    #[cfg(target_arch = "wasm32")]
                    {
                        let download = ui.add_enabled(
                            !project.projects.is_empty(),
                            ToolbarButton::new(egui::Image::new(unthemed_icon!("open_mining_format.svg")), "Download the current project as an .omf file")
                                .id_salt("download_project"),
                        );
                        if download.clicked() {
                            commands.push(UiCommand::DownloadProject);
                        }
                    }

                    ui.add_space(STRIP_CLUSTER_GAP - STRIP_BUTTON_GAP);

                    // The same two dialogs the File menu opens; only one of
                    // the pair is ever up, so opening one closes the other.
                    let has_project = project.projects.iter().any(|entry| entry.is_active);
                    let import = ui.add_enabled(
                        has_project,
                        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "import_data.svg")), "Import...").id_salt("import"),
                    );
                    if import.clicked() {
                        editor.show_import = true;
                        editor.show_export = false;
                    }
                    let export = ui.add_enabled(
                        has_project,
                        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "export_data.svg")), "Export...").id_salt("export"),
                    );
                    if export.clicked() {
                        editor.show_import = false;
                        editor.show_export = true;
                    }

                    ui.add_space(STRIP_CLUSTER_GAP - STRIP_BUTTON_GAP);

                    let undo_btn = ui.add_enabled(
                        can_undo,
                        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "undo.svg")), format!("Undo ({PRIMARY_MODIFIER}Z)")).id_salt("undo"),
                    );
                    if undo_btn.clicked() {
                        commands.push(UiCommand::Undo);
                    }
                    let redo_btn = ui.add_enabled(
                        can_redo,
                        ToolbarButton::new(egui::Image::new(themed_icon!(ui, "redo.svg")), format!("Redo ({PRIMARY_MODIFIER}{SHIFT_MODIFIER}Z)")).id_salt("redo"),
                    );
                    if redo_btn.clicked() {
                        commands.push(UiCommand::Redo);
                    }
                });
            });
        })
        .response
        .rect
}

/// Draw the top toolbar (layer combo, Z level, line colour, hatch).
pub(crate) fn draw_top_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView) -> egui::Rect {
    egui::Panel::top("top_tools_strip")
        .resizable(false)
        .show_separator_line(false)
        .default_size(TOOLBAR_STRIP_HEIGHT)
        .frame(crate::ui::chrome::region_frame(ui.style()))
        .show(ui, |ui| {
            // Keep the automatic ids below this point independent of the
            // parent panel's layout pass. egui may rerun a frame for sizing;
            // an explicit scope prevents an earlier conditional panel from
            // shifting the ids of these persistent controls on that rerun.
            let contents_id = ui.make_persistent_id("top_toolbar_buttons");
            ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label("Layer: ");
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
                    const MAX_LAYER_DISPLAY: usize = 22;
                    let layer_display: String = if selected_layer.chars().count() > MAX_LAYER_DISPLAY {
                        format!("{}…", selected_layer.chars().take(MAX_LAYER_DISPLAY - 1).collect::<String>())
                    } else {
                        selected_layer.to_string()
                    };
                    egui::ComboBox::from_id_salt("layer_combo_box").selected_text(layer_display).width(300.).show_ui(ui, |ui| {
                        ui.selectable_value(&mut editor.active_layer, None, "None");
                        for layer in active_layers.iter().filter(|layer| layer.is_loaded) {
                            ui.selectable_value(&mut editor.active_layer, Some(layer.id), &layer.name);
                        }
                    });

                    let z_resp = MenuFieldF64::new("Z:", &mut editor.z_input, f64::MIN..=f64::MAX).width(80.0).suffix("m").show_inline(ui);
                    if z_resp.changed() && editor.z_input.is_finite() {
                        editor.z_level = editor.z_input;
                    }

                    shifted_up(ui, 3.0, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;

                            let mut line_c32 = rgba_to_color32(editor.tool_line_color);

                            if ColorSquarePicker::new(&mut line_c32).show(ui).changed() {
                                editor.tool_line_color = color32_to_rgba(line_c32);
                            }

                            HatchPicker::new(&mut editor.tool_hatch, rgba_to_color32(editor.tool_line_color)).show(ui);
                        });
                    });
                });
            });
        })
        .response
        .rect
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
    let button = ToolCellButton::new(tool.icon.clone(), tool.tooltip).id_salt(("left_tool", tool.tooltip)).selected(selected);
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
    let margins = 2.0 * f32::from(crate::ui::chrome::REGION_MARGIN);
    // A column is filled before the next is started - ten tools in room for
    // six wrap 6-4, not 5-5 - so the toolbar only reaches as far across the
    // window as it has to.
    let rows = (((ui.available_height() - margins) / TOOL_CELL_SIZE) as usize).clamp(1, tools.len());
    let columns = tools.len().div_ceil(rows);
    let width = columns as f32 * TOOL_CELL_SIZE + margins;

    egui::Panel::left(LEFT_TOOLBAR_PANEL_ID)
        .resizable(false)
        .show_separator_line(false)
        .exact_size(width)
        // No padding on the region: the cells run edge to edge, so a selected
        // tool's fill reaches its rounded corners the way it used to reach a
        // tile's.
        .frame(crate::ui::chrome::region_frame(ui.style()).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                // Nothing between the columns: a wrap carries on down the next
                // one rather than starting a block of its own.
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                for column in tools.chunks(rows) {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        tool_cell_run(ui, |ui| {
                            for tool in column {
                                draw_left_tool(ui, tool, editor, commands);
                            }
                        });
                    });
                }
            });
        })
        .response
        .rect
}

/// The tile the view tools occupy, worked out without drawing anything, or
/// [`egui::Rect::NOTHING`] when the viewport is too small to float them over.
///
/// The tools sit in egui's middle order, so the overlays painted above them
/// have to keep clear of the tile themselves rather than rely on paint order.
/// Where it lands is arithmetic - the gizmo's corner, and one cell per tool -
/// so those overlays can ask for this frame's block before either they or the
/// tools are drawn.
pub(crate) fn view_tools_bounds(canvas_rect: egui::Rect, gizmo_visible: bool) -> egui::Rect {
    if canvas_rect.width() < MIN_TOOLS_VIEWPORT || canvas_rect.height() < MIN_TOOLS_VIEWPORT {
        return egui::Rect::NOTHING;
    }
    let gizmo_rect = if gizmo_visible {
        crate::ui::elements::cursors::orientation_gizmo_rect(canvas_rect)
    } else {
        egui::Rect::NOTHING
    };
    let top = if gizmo_rect.is_positive() {
        gizmo_rect.bottom() + VIEW_TOOLS_GIZMO_GAP
    } else {
        canvas_rect.top() + TOOLS_TOP_MARGIN
    };
    let size = egui::vec2(TOOL_CELL_SIZE, VIEW_TOOL_CELLS as f32 * TOOL_CELL_SIZE);
    // Flush against the viewport's right edge rather than floating a margin
    // inside it: `draw_ui` paints the chrome gap around the other three sides,
    // so the tile reads as a piece of chrome let into the edge rather than
    // something parked on the scene.
    egui::Rect::from_min_size(egui::pos2(canvas_rect.right() - TOOL_CELL_SIZE, top), size)
}

/// Draw the view tools: one tile hung off the viewport's right edge, under the
/// orientation gizmo, and return the block it occupies.
///
/// The tile carries its own chrome - the gap around it, its rounded corners
/// and its outline - because it floats above the layer the panels' regions are
/// finished off in.
///
/// The block matches what [`view_tools_bounds`] promised the overlays drawn
/// before it. A viewport too short for the whole run clips the tile at its
/// bottom edge rather than moving the tools somewhere the overlays are not
/// expecting them.
pub(crate) fn draw_view_tools(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, canvas_rect: egui::Rect, gizmo_visible: bool) -> egui::Rect {
    let bounds = view_tools_bounds(canvas_rect, gizmo_visible);
    if !bounds.is_positive() {
        return egui::Rect::NOTHING;
    }

    egui::Area::new(egui::Id::new("view_tools"))
        // Above egui's Background layer, or right clicks inside the buttons
        // leak through to the viewport's orbit and context handling.
        .order(egui::Order::Middle)
        // `Area` is movable by default, which would give the whole tile a drag
        // response and swallow middle-drag camera moves.
        .movable(false)
        .sense(egui::Sense::hover())
        // Clipped to the viewport, but deliberately not *constrained* to it:
        // the tile has already placed itself, and letting egui slide it as
        // well would move the tools out from under the block the caller was
        // handed.
        .constrain_to(canvas_rect)
        .constrain(false)
        .fixed_pos(bounds.min)
        .show(ui.ctx(), |ui| {
            let drawn = ui.scope_builder(egui::UiBuilder::new().max_rect(bounds).layout(egui::Layout::top_down(egui::Align::Center)), |ui| {
                ToolbarGroup::new().show(ui, |ui| {
                    let response = ui.add(ToolCellButton::new(egui::Image::new(unthemed_icon!("reset_view.svg")), "Reset view").id_salt("reset_view"));
                    if response.clicked() {
                        commands.push(UiCommand::ResetView);
                    }

                    let response = ui.add(ToolCellButton::new(egui::Image::new(unthemed_icon!("zoom_to_extents.svg")), "Zoom to extents").id_salt("zoom_to_extents"));
                    if response.clicked() {
                        commands.push(UiCommand::ZoomToExtents);
                    }

                    let response = ui.add(
                        ToolCellButton::new(
                            egui::Image::new(unthemed_icon!("vertical_exaggeration.svg")),
                            format!("Vertical Exaggeration ({:.2}×)", editor.vertical_exaggeration),
                        )
                        .id_salt("vertical_exaggeration")
                        .selected(editor.vertical_exaggeration != 1.0),
                    );
                    if response.clicked() {
                        editor.vertical_exaggeration_input = editor.vertical_exaggeration;
                        editor.vertical_exaggeration_dialog_open = true;
                    }

                    let response = ui.add(
                        ToolCellButton::new(
                            egui::Image::new(unthemed_icon!("toggle_xray.svg")),
                            if editor.xray_enabled { "Disable X-Ray Vision" } else { "Enable X-Ray Vision" },
                        )
                        .id_salt("xray")
                        .selected(editor.xray_enabled),
                    );
                    if response.clicked() {
                        editor.xray_enabled = !editor.xray_enabled;
                    }

                    let response = ui.add(
                        ToolCellButton::new(
                            egui::Image::new(unthemed_icon!("toggle_points.svg")),
                            if editor.show_points { "Hide Points" } else { "Show Points" },
                        )
                        .id_salt("show_points")
                        .selected(editor.show_points),
                    );
                    if response.clicked() {
                        commands.push(UiCommand::SetShowPoints(!editor.show_points));
                    }

                    let response = ui.add(
                        ToolCellButton::new(
                            egui::Image::new(unthemed_icon!("toggle_wireframes.svg")),
                            if editor.topology_wireframes_enabled { "Hide Wireframes" } else { "Show Wireframes" },
                        )
                        .id_salt("wireframes")
                        .selected(editor.topology_wireframes_enabled),
                    );
                    if response.clicked() {
                        commands.push(UiCommand::SetTopologyWireframes(!editor.topology_wireframes_enabled));
                    }

                    // Vertical slice view: arm the two-click line placement, or
                    // exit the mode if it is already active.
                    let slice_engaged = editor.slice_mode_enabled || editor.active_tool == ActiveTool::VerticalSlice;
                    let response = ui.add(
                        ToolCellButton::new(
                            egui::Image::new(unthemed_icon!("slice_view.svg")),
                            if editor.slice_mode_enabled { "Exit Slice View" } else { "Vertical Slice View" },
                        )
                        .id_salt("vertical_slice")
                        .selected(slice_engaged),
                    );
                    if response.clicked() {
                        if editor.slice_mode_enabled {
                            commands.push(UiCommand::SetSliceModeEnabled(false));
                        } else {
                            commands.push(UiCommand::SetActiveTool(ActiveTool::VerticalSlice));
                        }
                    }

                    let response = ui.add(
                        ToolCellButton::new(
                            egui::Image::new(unthemed_icon!("fly_mode.svg")),
                            if editor.fly_mode_enabled { "Disable Flying Mode" } else { "Enable Flying Mode" },
                        )
                        .id_salt("fly_mode")
                        .selected(editor.fly_mode_enabled),
                    );
                    if response.clicked() {
                        commands.push(UiCommand::SetFlyModeEnabled(!editor.fly_mode_enabled));
                    }
                });
            });
            debug_assert!(
                (drawn.response.rect.height() - bounds.height()).abs() < 1.0,
                "the view tools' tile was declared {} cells tall but drew {} points of buttons",
                VIEW_TOOL_CELLS,
                drawn.response.rect.height(),
            );
            // Over the tile it has just drawn, so its corners are cut back and
            // the gap parts the scene around it, the way a panel's region is
            // finished off from the background layer.
            crate::ui::chrome::paint_floating_region(ui.painter(), ui.visuals(), bounds);
        });

    bounds
}

/// Draw the bottom toolbar (cursor mode, measure distance).
///
/// Visibility and locking are per-item concerns now, so they live on the
/// explorer's rows rather than as whole-scene toolbar actions: see
/// `ExplorerEntry::toggles`.
pub(crate) fn draw_bottom_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::bottom("bottom_tools_strip")
        .resizable(false)
        .show_separator_line(false)
        .default_size(BOTTOM_TOOLBAR_HEIGHT)
        // An even sleeve on all four sides: the buttons carry their own fill,
        // so the strip is only the surface they sit on, not a frame around
        // them - the space beside the first button matches the space over it.
        .frame(crate::ui::chrome::region_frame(ui.style()).inner_margin(egui::Margin::same(BOTTOM_STRIP_MARGIN)))
        .show(ui, |ui| {
            let contents_id = ui.make_persistent_id("bottom_toolbar_buttons");
            ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                ui.horizontal_centered(|ui| {
                    cursor_mode_button(ui, egui::Image::new(themed_icon!(ui, "cursor_select.svg")), "Cursor: Regular", editor, CursorMode::Select);

                    cursor_mode_button(
                        ui,
                        egui::Image::new(themed_icon!(ui, "snap_to_surface.svg")),
                        "Cursor: Snap to surface",
                        editor,
                        CursorMode::SnapToSurface,
                    );

                    cursor_mode_button(
                        ui,
                        egui::Image::new(themed_icon!(ui, "snap_to_line.svg")),
                        "Cursor: Snap to line",
                        editor,
                        CursorMode::SnapToLine,
                    );

                    cursor_mode_button(
                        ui,
                        egui::Image::new(themed_icon!(ui, "snap_to_point.svg")),
                        "Cursor: Snap to point",
                        editor,
                        CursorMode::SnapToPoint,
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
                        );

                        tool_button(
                            ui,
                            egui::Image::new(themed_icon!(ui, "measure_batter_angle.svg")),
                            "Measure batter angle",
                            editor,
                            commands,
                            ActiveTool::MeasureBatterAngle,
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
) -> egui::Response {
    let selected = editor.active_tool == tool;
    let response = ui.add(ToolbarButton::new(icon, tooltip).id_salt(("tool", tooltip)).selected(selected));

    if response.clicked() {
        commands.push(UiCommand::SetActiveTool(tool));
    }

    response
}

/// Draw a cursor mode button; sets `editor.cursor_mode` on click.
pub(crate) fn cursor_mode_button(ui: &mut egui::Ui, icon: egui::Image<'static>, tooltip: &str, editor: &mut EditorState, mode: CursorMode) -> egui::Response {
    let selected = editor.cursor_mode == mode;
    let response = ui.add(ToolbarButton::new(icon, tooltip).id_salt(("cursor_mode", tooltip)).selected(selected));

    if response.clicked() {
        editor.cursor_mode = mode;
    }

    response
}

fn shifted_up(ui: &mut egui::Ui, amount: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    let rect = ui.available_rect_before_wrap();
    let shifted_rect = rect.translate(egui::vec2(0.0, -amount));

    ui.scope_builder(egui::UiBuilder::new().max_rect(shifted_rect), |ui| add_contents(ui));
}
