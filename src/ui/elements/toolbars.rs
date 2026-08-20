//! Four toolbar panels: top (layer/Z/line/fill settings), left (drawing tools),
//! right (viewport controls), bottom (selection actions + cursor mode).

use crate::ui::{
    EditorState, UiProjectView, color32_to_rgba, rgba_to_color32,
    state::{ActiveTool, CursorMode, EditorAction, UiCommand, UiProjectEntry},
    themed_icon, unthemed_icon,
    widgets::{
        menu::MenuFieldF64,
        toolbar::{ColorSquarePicker, HatchPicker, ToolbarButton},
    },
};

pub(crate) const BOTTOM_TOOLBAR_HEIGHT: f32 = 32.0;

/// Draw the top toolbar (save, undo/redo, layer combo, Z level, line/fill colors, weight, hatch).
pub(crate) fn draw_top_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>, can_undo: bool, can_redo: bool) -> egui::Rect {
    egui::Panel::top("top_tools_strip")
        .resizable(false)
        .default_size(34.0)
        .show(ui, |ui| {
            // Keep the automatic ids below this point independent of the
            // parent panel's layout pass. egui may rerun a frame for sizing;
            // an explicit scope prevents an earlier conditional panel from
            // shifting the ids of these persistent controls on that rerun.
            let contents_id = ui.make_persistent_id("top_toolbar_buttons");
            ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                ui.horizontal_centered(|ui| {
                    let preferences = ui.add(ToolbarButton::new(egui::Image::new(themed_icon!(ui, "open_preferences.svg")), "Preferences").id_salt("preferences"));
                    if preferences.clicked() && !editor.preferences_open {
                        commands.push(UiCommand::OpenPreferences);
                    }

                    let has_unsaved = project.projects.iter().any(UiProjectEntry::needs_save);
                    let save = ui.add_enabled(
                        has_unsaved,
                        ToolbarButton::new(egui::Image::new(unthemed_icon!("save_all_pidbs.svg")), "Save All PIDBs").id_salt("save_all_pidbs"),
                    );
                    if save.clicked() {
                        commands.push(UiCommand::SaveAllPidbs);
                    }

                    #[cfg(target_arch = "wasm32")]
                    if ui
                        .add_enabled(!project.projects.is_empty(), egui::Button::new("Download All PIDBs"))
                        .on_hover_text("Download every open project as a .pidb file")
                        .clicked()
                    {
                        commands.push(UiCommand::DownloadAllPidbs);
                    }

                    let undo_btn = ui.add_enabled(can_undo, ToolbarButton::new(egui::Image::new(themed_icon!(ui, "undo.svg")), "Undo").id_salt("undo"));
                    if undo_btn.clicked() {
                        commands.push(UiCommand::Undo);
                    }
                    let redo_btn = ui.add_enabled(can_redo, ToolbarButton::new(egui::Image::new(themed_icon!(ui, "redo.svg")), "Redo").id_salt("redo"));
                    if redo_btn.clicked() {
                        commands.push(UiCommand::Redo);
                    }
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
                    egui::ComboBox::from_id_salt("layer_combo_box").selected_text(layer_display).width(200.).show_ui(ui, |ui| {
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

/// Draw the left toolbar (New layer, MakePoint, MakeLine, MakePoly, MakeCircle, MakeText, Move, OffsetElement, DrapeToTopology,
/// RelimitLine, FuseIntoPolygon, ExplodePolygon, DeleteElement).
pub(crate) fn draw_left_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, editing_enabled: bool, project_active: bool, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::left("left_tools_strip")
        .resizable(false)
        .show_separator_line(false)
        .default_size(32.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    let contents_id = ui.make_persistent_id("left_toolbar_buttons");
                    ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_enabled_ui(project_active, |ui| {
                                let new_layer = ui.add(
                                    ToolbarButton::new(egui::Image::new(unthemed_icon!("layer.svg")), "New layer")
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

                            ui.add_space(12.0);

                            ui.add_enabled_ui(editing_enabled, |ui| {
                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_point.svg")),
                                    "Create Point",
                                    editor,
                                    commands,
                                    ActiveTool::MakePoint,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_line.svg")),
                                    "Create Line",
                                    editor,
                                    commands,
                                    ActiveTool::MakeLine,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_polygon.svg")),
                                    "Create Polygon",
                                    editor,
                                    commands,
                                    ActiveTool::MakePoly,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_circle.svg")),
                                    "Create Circle",
                                    editor,
                                    commands,
                                    ActiveTool::MakeCircle,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(unthemed_icon!("create_text.svg")),
                                    "Create Text",
                                    editor,
                                    commands,
                                    ActiveTool::MakeText,
                                );

                                ui.add_space(12.0);

                                tool_button(ui, egui::Image::new(themed_icon!(ui, "move_element.svg")), "Move", editor, commands, ActiveTool::Move);

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "offset_element.svg")),
                                    "Offset",
                                    editor,
                                    commands,
                                    ActiveTool::OffsetElement,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "drape_element.svg")),
                                    "Drape to Topology",
                                    editor,
                                    commands,
                                    ActiveTool::DrapeToTopology,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(unthemed_icon!("auto_bench.svg")),
                                    "Auto-Bench",
                                    editor,
                                    commands,
                                    ActiveTool::BatterBermOffset,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "relimit_line.svg")),
                                    "Relimit Line",
                                    editor,
                                    commands,
                                    ActiveTool::RelimitLine,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "fuse_lines.svg")),
                                    "Fuse Lines Into Polygon",
                                    editor,
                                    commands,
                                    ActiveTool::FuseIntoPolygon,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "chamfer_corners.svg")),
                                    "Chamfer Polygon Corners",
                                    editor,
                                    commands,
                                    ActiveTool::Chamfer,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "create_bezier.svg")),
                                    "Bezier Curve",
                                    editor,
                                    commands,
                                    ActiveTool::Bezier,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(themed_icon!(ui, "split_at_points.svg")),
                                    "Split Polyline At Points",
                                    editor,
                                    commands,
                                    ActiveTool::SplitAtPoints,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(unthemed_icon!("explode_polygon.svg")),
                                    "Explode Polygon to Lines",
                                    editor,
                                    commands,
                                    ActiveTool::ExplodePolygon,
                                );

                                tool_button(
                                    ui,
                                    egui::Image::new(unthemed_icon!("delete_element.svg")),
                                    "Delete",
                                    editor,
                                    commands,
                                    ActiveTool::DeleteElement,
                                );
                            });
                        });
                    });
                });
        })
        .response
        .rect
}

/// Draw the right toolbar (Reset view, Zoom to extents, Vertical exaggeration, X-Ray).
pub(crate) fn draw_right_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::right("right_tools_strip")
        .resizable(false)
        .show_separator_line(false)
        .default_size(32.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    let contents_id = ui.make_persistent_id("right_toolbar_buttons");
                    ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                        ui.vertical_centered(|ui| {
                            // Reset view button
                            let response = ui.add(ToolbarButton::new(egui::Image::new(unthemed_icon!("reset_view.svg")), "Reset view").id_salt("reset_view"));
                            if response.clicked() {
                                commands.push(UiCommand::ResetView);
                            }

                            // Zoom to bounds button
                            let response = ui.add(ToolbarButton::new(egui::Image::new(unthemed_icon!("zoom_to_extents.svg")), "Zoom to extents").id_salt("zoom_to_extents"));
                            if response.clicked() {
                                commands.push(UiCommand::ZoomToExtents);
                            }

                            // Open Vertical Exaggeration menu button
                            let response = ui.add(
                                ToolbarButton::new(
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

                            // Enable x-ray vision
                            let response = ui.add(
                                ToolbarButton::new(
                                    egui::Image::new(unthemed_icon!("toggle_xray.svg")),
                                    if editor.xray_enabled { "Disable X-Ray Vision" } else { "Enable X-Ray Vision" },
                                )
                                .id_salt("xray")
                                .selected(editor.xray_enabled),
                            );
                            if response.clicked() {
                                editor.xray_enabled = !editor.xray_enabled;
                            }

                            // Show vertices for every visible design object
                            let response = ui.add(
                                ToolbarButton::new(
                                    egui::Image::new(unthemed_icon!("toggle_points.svg")),
                                    if editor.show_points { "Hide Points" } else { "Show Points" },
                                )
                                .id_salt("show_points")
                                .selected(editor.show_points),
                            );
                            if response.clicked() {
                                commands.push(UiCommand::SetShowPoints(!editor.show_points));
                            }

                            // Show wireframes on every topology mesh
                            let response = ui.add(
                                ToolbarButton::new(
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
                                ToolbarButton::new(
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

                            // Toggle fly mode
                            let response = ui.add(
                                ToolbarButton::new(
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
                });
        })
        .response
        .rect
}

/// Draw the bottom toolbar (reveal/hide/freeze selection, cursor mode, measure distance).
///
/// Updates `geometry_dirty` based on which selection actions were triggered.
pub(crate) fn draw_bottom_toolbar(ui: &mut egui::Ui, editor: &mut EditorState, geometry_dirty: &mut bool, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::bottom("bottom_tools_strip")
        .resizable(false)
        .default_size(BOTTOM_TOOLBAR_HEIGHT)
        .show(ui, |ui| {
            let contents_id = ui.make_persistent_id("bottom_toolbar_buttons");
            ui.scope_builder(egui::UiBuilder::new().id(contents_id), |ui| {
                ui.horizontal_centered(|ui| {
                    let reveal_all = editor_action_button(
                        ui,
                        egui::Image::new(unthemed_icon!("reveal_all.svg")),
                        "Reveal all elements",
                        editor,
                        EditorAction::RevealAll,
                    );
                    if reveal_all {
                        commands.push(UiCommand::RevealAllTriangulations);
                    }
                    *geometry_dirty |= reveal_all;

                    ui.add_enabled_ui(!editor.slice_mode_enabled, |ui| {
                        *geometry_dirty |= editor_action_button(
                            ui,
                            egui::Image::new(themed_icon!(ui, "hide_selection.svg")),
                            "Hide selection",
                            editor,
                            EditorAction::HideSelection,
                        );

                        *geometry_dirty |= editor_action_button(
                            ui,
                            egui::Image::new(unthemed_icon!("freeze_selection.svg")),
                            "Freeze selection",
                            editor,
                            EditorAction::FreezeSelection,
                        );
                    });

                    ui.add_space(12.);

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

/// Draw a single tool button; toggles `editor.active_tool` on click.
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

/// Draw a selection action button.  Returns `true` if the action was applied.
pub(crate) fn editor_action_button(ui: &mut egui::Ui, icon: egui::Image<'static>, tooltip: &str, editor: &mut EditorState, action: EditorAction) -> bool {
    let response = ui.add(ToolbarButton::new(icon, tooltip).id_salt(("editor_action", tooltip)));

    response.clicked() && editor.apply_action(action)
}

fn shifted_up(ui: &mut egui::Ui, amount: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    let rect = ui.available_rect_before_wrap();
    let shifted_rect = rect.translate(egui::vec2(0.0, -amount));

    ui.scope_builder(egui::UiBuilder::new().max_rect(shifted_rect), |ui| add_contents(ui));
}
