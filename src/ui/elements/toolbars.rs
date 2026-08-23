//! Four toolbar panels: top (layer/Z/line/fill settings), left (drawing tools),
//! right (viewport controls), bottom (selection actions + cursor mode).

use crate::ui::{
    EditorState, UiProjectView, color32_to_rgba, rgba_to_color32,
    state::{ActiveTool, CursorMode, UiCommand, UiProjectEntry},
    themed_icon, unthemed_icon,
    widgets::{
        menu::MenuFieldF64,
        toolbar::{ColorSquarePicker, HatchPicker, TOOL_CELL_SIZE, ToolCellButton, ToolbarButton, ToolbarGroup},
    },
};

pub(crate) const BOTTOM_TOOLBAR_HEIGHT: f32 = 32.0;

/// Space between toolbar tiles. The tiles pad themselves, so the visible gap
/// is this less the padding either side of it.
const TOOL_GROUP_GAP: f32 = 16.0;
/// Height of a docked toolbar strip. The explorer's header and the viewport's
/// top toolbar share it, so their bottom rules meet as one line.
const TOOLBAR_STRIP_HEIGHT: f32 = 34.0;
/// Space between a docked strip's edge and its buttons.
const STRIP_MARGIN: i8 = 6;
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

/// Gap between the view tools and the viewport's right edge. Matches the
/// orientation gizmo's margin, so the stack and the gizmo share an edge.
const VIEW_TOOLS_MARGIN: f32 = 16.0;

/// Width of the band along the viewport's right edge that the floating view
/// tools occupy: the tile column plus its margin. The tools sit in egui's
/// middle order, so overlays painted above them have to inset themselves by
/// this much instead of relying on paint order to stay clear of the stack.
pub(crate) const VIEW_TOOLS_BAND_WIDTH: f32 = VIEW_TOOLS_MARGIN + TOOL_CELL_SIZE;

/// Draw the explorer's header: the project-wide actions, sitting above the
/// data tree rather than out over the viewport.
///
/// The floating view tools group their buttons onto tiles, because out there a
/// tile is all that separates a tool from the scene behind it. A docked strip
/// already has a surface, so it groups by spacing alone: related buttons close
/// together, a wider gap between clusters. The button metrics, rounding and
/// hover fills are shared, so the two toolbars read as one family. The strip
/// is as tall as the viewport's top toolbar, and shares its default panel
/// fill and separator line, so the two read as one family across the split.
pub(crate) fn draw_explorer_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>, can_undo: bool, can_redo: bool) {
    egui::Panel::top("explorer_tools_strip")
        .resizable(false)
        .default_size(TOOLBAR_STRIP_HEIGHT)
        .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(STRIP_MARGIN, 0)))
        .show(ui, |ui| {
            let contents_id = ui.make_persistent_id("explorer_toolbar_buttons");
            ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = STRIP_BUTTON_GAP;

                    let has_unsaved = project.projects.iter().any(UiProjectEntry::needs_save);
                    let save = ui.add_enabled(
                        has_unsaved,
                        ToolbarButton::new(egui::Image::new(unthemed_icon!("save_project.svg")), format!("Save Project ({PRIMARY_MODIFIER}S)")).id_salt("save_project"),
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
        });
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

/// Draw the left toolbar: the project action, the creation tools, the
/// transform tools, the polyline edits and the destructive one, each cluster
/// its own rounded tile.
pub(crate) fn draw_left_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, editing_enabled: bool, project_active: bool, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::left("left_tools_strip")
        .resizable(false)
        .show_separator_line(false)
        // No background of its own: the tool tiles float over the scene.
        .frame(egui::Frame::NONE)
        .default_size(44.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    let contents_id = ui.make_persistent_id("left_toolbar_buttons");
                    ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                        ui.spacing_mut().item_spacing.y = TOOL_GROUP_GAP;
                        ui.add_space(8.0);

                        ui.add_enabled_ui(project_active, |ui| {
                            ToolbarGroup::new().show(ui, |ui| {
                                let new_layer = ui.add(
                                    ToolCellButton::new(egui::Image::new(unthemed_icon!("layer.svg")), "New layer")
                                        .id_salt("new_layer")
                                        .selected(editor.new_layer_dialog_open),
                                );
                                if new_layer.clicked() {
                                    editor.new_layer_dialog_open = !editor.new_layer_dialog_open;
                                    if editor.new_layer_dialog_open {
                                        editor.new_layer_name = "design".to_owned();
                                        commands.push(UiCommand::SetActiveTool(ActiveTool::None));
                                    }
                                }
                            });
                        });

                        ui.add_enabled_ui(editing_enabled, |ui| {
                            ToolbarGroup::new().show(ui, |ui| {
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_point.svg")),
                                    "Create Point",
                                    editor,
                                    commands,
                                    ActiveTool::MakePoint,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_line.svg")),
                                    "Create Line",
                                    editor,
                                    commands,
                                    ActiveTool::MakeLine,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_polyline.svg")),
                                    "Create Polyline",
                                    editor,
                                    commands,
                                    ActiveTool::MakePoly,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_circle.svg")),
                                    "Create Circle",
                                    editor,
                                    commands,
                                    ActiveTool::MakeCircle,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(unthemed_icon!("create_text.svg")),
                                    "Create Text",
                                    editor,
                                    commands,
                                    ActiveTool::MakeText,
                                );
                            });

                            ToolbarGroup::new().show(ui, |ui| {
                                tool_cell_button(ui, egui::Image::new(themed_icon!(ui, "move_element.svg")), "Move", editor, commands, ActiveTool::Move);
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "offset_element.svg")),
                                    "Offset",
                                    editor,
                                    commands,
                                    ActiveTool::OffsetElement,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "drape_element.svg")),
                                    "Drape to Topology",
                                    editor,
                                    commands,
                                    ActiveTool::DrapeToTopology,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(unthemed_icon!("auto_bench.svg")),
                                    "Auto-Bench",
                                    editor,
                                    commands,
                                    ActiveTool::BatterBermOffset,
                                );
                            });

                            ToolbarGroup::new().show(ui, |ui| {
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "relimit_line.svg")),
                                    "Relimit Line",
                                    editor,
                                    commands,
                                    ActiveTool::RelimitLine,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "fuse_lines.svg")),
                                    "Fuse Polylines",
                                    editor,
                                    commands,
                                    ActiveTool::FuseIntoPolyline,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "chamfer_corners.svg")),
                                    "Chamfer Polyline Corners",
                                    editor,
                                    commands,
                                    ActiveTool::Chamfer,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_bezier.svg")),
                                    "Bezier Polyline",
                                    editor,
                                    commands,
                                    ActiveTool::Bezier,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "split_at_points.svg")),
                                    "Split Polyline At Points",
                                    editor,
                                    commands,
                                    ActiveTool::SplitAtPoints,
                                );
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(unthemed_icon!("explode_polyline.svg")),
                                    "Explode Polyline to Lines",
                                    editor,
                                    commands,
                                    ActiveTool::ExplodePolyline,
                                );
                            });

                            ToolbarGroup::new().show(ui, |ui| {
                                tool_cell_button(
                                    ui,
                                    egui::Image::new(unthemed_icon!("delete_element.svg")),
                                    "Delete Points",
                                    editor,
                                    commands,
                                    ActiveTool::DeletePoints,
                                );
                            });
                        });
                    });
                });
        })
        .response
        .rect
}

/// Draw the view tools: a floating stack of tiles under the viewport's
/// orientation gizmo, `top` being the y the first tile starts at.
pub(crate) fn draw_view_tools(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, canvas_rect: egui::Rect, top: f32) {
    if canvas_rect.width() < 120.0 || canvas_rect.height() < 120.0 {
        return;
    }

    egui::Area::new(egui::Id::new("view_tools"))
        // Above egui's Background layer, or right clicks inside the buttons
        // leak through to the viewport's orbit and context handling.
        .order(egui::Order::Middle)
        // `Area` is movable by default, which would give the whole stack a
        // drag response and swallow middle-drag camera moves.
        .movable(false)
        .sense(egui::Sense::hover())
        .pivot(egui::Align2::RIGHT_TOP)
        .fixed_pos(egui::pos2(canvas_rect.right() - VIEW_TOOLS_MARGIN, top))
        // Middle order paints over every panel, so a stack taller than the
        // space below the gizmo would otherwise run across the bottom toolbar
        // and the console. Constraining slides it up to sit inside the
        // viewport, and clips anything still too tall, so the panels stay on
        // top the way they do beside the left toolbar. Only the vertical range
        // is tightened: each group's tile is painted a little wider than the
        // column of buttons the area measures, so a rect that hugged the
        // stack's own edge would clip the tiles' right-hand corners off.
        .constrain_to(canvas_rect.shrink2(egui::vec2(0.0, VIEW_TOOLS_MARGIN)))
        .show(ui.ctx(), |ui| {
            ui.set_max_width(TOOL_CELL_SIZE);
            ui.spacing_mut().item_spacing.y = TOOL_GROUP_GAP;

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
            });

            ToolbarGroup::new().show(ui, |ui| {
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
            });

            ToolbarGroup::new().show(ui, |ui| {
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
        .frame(crate::ui::chrome::region_frame(ui.style()))
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

/// Draw a tool button inside a [`ToolbarGroup`] tile; sets `editor.active_tool` on click.
pub(crate) fn tool_cell_button(
    ui: &mut egui::Ui,
    icon: egui::Image<'static>,
    tooltip: &str,
    editor: &mut EditorState,
    commands: &mut Vec<UiCommand>,
    tool: ActiveTool,
) -> egui::Response {
    let selected = editor.active_tool == tool;
    let response = ui.add(ToolCellButton::new(icon, tooltip).id_salt(("tool", tooltip)).selected(selected));

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
