//! Top application menu bar: the mark, the application-wide menus (File,
//! Project) and the workspace tabs.
//!
//! Everything that acts on the scene rather than on the application has moved
//! down to the viewport bar - see [`crate::ui::elements::viewport_bar`] - so
//! this bar carries only what is true whichever workspace is open, the way
//! Blender's topbar does.

use crate::ui::{
    EditorState, UiCommand, UiProjectView,
    state::{ActiveTool, Workspace},
    themed_icon,
    widgets::toolbar::GROUP_CORNER_RADIUS,
};
// The dropdowns below are all in the system menu bar on macOS (`mac.rs`), and
// nothing this module still draws there needs any of this.
#[cfg(not(target_os = "macos"))]
use crate::{
    model::{Axis, SceneEntityId},
    ui::{
        state::{UiProjectEntry, ViewToggle},
        widgets::context_menu::{ContextMenuAction, MenuBarMenu, context_menu_separator, context_submenu},
    },
};

/// Extra space above the bar's contents, on top of the panel frame's own
/// margin, so the menus and tabs are not pinned to the window's top edge.
const TOP_PADDING: i8 = 3;
/// Side of the application mark at the left of the bar.
const LOGO_SIZE: f32 = 18.0;
/// Space either side of the mark, so it reads as a mark rather than as the
/// first menu.
const LOGO_MARGIN: f32 = 6.0;
/// Height of the rule parting the menus from the workspace tabs.
#[cfg(not(target_os = "macos"))]
const SEPARATOR_HEIGHT: f32 = 16.0;
/// Space either side of that rule.
#[cfg(not(target_os = "macos"))]
const SEPARATOR_MARGIN: f32 = 8.0;
/// Space either side of a workspace tab's label.
const TAB_PADDING: f32 = 10.0;
/// Height of a workspace tab's fill.
const TAB_HEIGHT: f32 = 20.0;
/// Space either side of a dropdown's label in the viewport bar.
///
/// `egui`'s `menu_style` packs bar labels to 2 points, which is right for a
/// menu bar where the labels are the whole row. In the viewport bar they share
/// the row with three clusters of icons, and at that padding the six of them
/// ran together into one band of text.
///
/// Unused on macOS, where the production menus live in the system menu bar.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const MENU_LABEL_PADDING: f32 = 7.0;
/// Gap between two dropdowns in that run, on top of their padding.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const MENU_LABEL_GAP: f32 = 2.0;
/// What the platform calls showing a file in its file manager. macOS says
/// "Reveal in Finder" and has the row in the system menu instead - see
/// `mac.rs`.
#[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
const SHOW_PROJECT_LABEL: &str = "Show in Explorer";
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "windows"), not(target_os = "macos")))]
const SHOW_PROJECT_LABEL: &str = "Open Containing Folder";

/// Draw the top menu bar panel.
///
/// Dropdowns are the same widget as the right-click menus - see
/// [`crate::ui::widgets::context_menu`] - so rows, submenus and separators all
/// come from there rather than from egui's default menu style.
///
/// On macOS the File and Project menus live in the system menu bar (`mac.rs`)
/// instead, but the bar itself stays: the mark and the workspace tabs are not
/// things an `NSMenu` can carry.
///
/// Returns the panel's bounding rect.
pub(crate) fn draw_main_menu(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) -> egui::Rect {
    // Not a region: the bar spans the window and carries the gap's own colour,
    // so it reads as the backdrop the workspace is laid on rather than as a
    // panel of its own. See `chrome::window_bar_frame`.
    let bar_fill = crate::ui::chrome::window_bar_fill(ui);
    let frame = crate::ui::chrome::window_bar_frame(ui);
    let frame = frame.inner_margin(egui::Margin {
        top: frame.inner_margin.top + TOP_PADDING,
        ..frame.inner_margin
    });
    egui::Panel::top("main_menu")
        .show_separator_line(crate::ui::chrome::show_separator_line(ui))
        .frame(frame)
        .show(ui, |ui| {
            // Too narrow a window scrolls the row rather than clipping the
            // workspace tabs off the end of it. See `elements::bar_strip`.
            // The row is as tall as `MenuBar` makes it, which is this unless
            // something on it is taller; the strip grows with the contents.
            crate::ui::elements::bar_strip(ui, "main_menu_strip", ui.spacing().interact_size.y, |ui, strip| {
                let mut used = 0.0;
                ui.scope_builder(egui::UiBuilder::new().max_rect(strip), |ui| {
                    egui::MenuBar::new().ui(ui, |ui| {
                        draw_logo(ui);

                        #[cfg(not(target_os = "macos"))]
                        {
                            draw_file_menu(ui, editor, project, commands);
                            draw_view_menu(ui, editor, commands);
                        }
                        // The parameter is only read by the menus above.
                        #[cfg(target_os = "macos")]
                        let _ = project;

                        #[cfg(not(target_os = "macos"))]
                        draw_separator(ui);

                        draw_workspace_tabs(ui, editor, commands, bar_fill);

                        // `MenuBar` sizes itself to the whole width it is given,
                        // so its rect says nothing about how much of the row was
                        // used; where the next item would go does.
                        used = ui.cursor().left() - strip.left();
                    });
                });
                used
            });
        })
        .response
        .rect
}

/// Paint the application mark at the head of the bar.
///
/// Flat and one-coloured: beside the menu labels it is a glyph, not artwork.
fn draw_logo(ui: &mut egui::Ui) {
    ui.add_space(LOGO_MARGIN);
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(LOGO_SIZE), egui::Sense::hover());
    egui::Image::new(themed_icon!(ui, "logo.svg")).fit_to_exact_size(rect.size()).paint_at(ui, rect);
    ui.add_space(LOGO_MARGIN);
}

/// Part the menus from the workspace tabs with a short vertical rule.
#[cfg(not(target_os = "macos"))]
fn draw_separator(ui: &mut egui::Ui) {
    ui.add_space(SEPARATOR_MARGIN);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, SEPARATOR_HEIGHT), egui::Sense::hover());
    let color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    ui.painter().line_segment([rect.center_top(), rect.center_bottom()], egui::Stroke::new(1.0, color));
    ui.add_space(SEPARATOR_MARGIN);
}

/// Draw the run of workspace tabs.
fn draw_workspace_tabs(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, bar_fill: egui::Color32) {
    for workspace in Workspace::ALL {
        draw_workspace_tab(ui, editor, commands, workspace, bar_fill);
    }
}

/// Open `workspace`, putting down anything the tools it does not carry had
/// picked up.
///
/// The drawing tools, the slice view and flying mode leave the window with
/// production - see [`Workspace::has_production_tools`] - so a mode still
/// running after the switch would have nothing left to turn it off. Entering a
/// workspace without them hands the scene back to plain orbiting first. The
/// cursor mode is left as the user set it: it does nothing without a drawing
/// tool to use it, and it is theirs again the moment production is back.
fn select_workspace(editor: &mut EditorState, commands: &mut Vec<UiCommand>, workspace: Workspace) {
    editor.active_workspace = workspace;
    if workspace.has_production_tools() {
        return;
    }
    if editor.fly_mode_enabled {
        commands.push(UiCommand::SetFlyModeEnabled(false));
    }
    if editor.slice_mode_enabled {
        commands.push(UiCommand::SetSliceModeEnabled(false));
    }
    if editor.active_tool != ActiveTool::None {
        commands.push(UiCommand::SetActiveTool(ActiveTool::None));
    }
}

/// One workspace tab: a text cell filled from [`tab_fill`].
///
/// A tab for a workspace that has nothing behind it yet is drawn as bare text
/// on the bar rather than left off it, so the shape of the application is
/// visible before all of it is built.
fn draw_workspace_tab(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, workspace: Workspace, bar_fill: egui::Color32) {
    let enabled = workspace.implemented();
    let selected = editor.active_workspace == workspace;
    let state = if !enabled {
        TabState::Disabled
    } else if selected {
        TabState::Active
    } else {
        TabState::Inactive
    };

    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(workspace.label().to_owned(), font, egui::Color32::PLACEHOLDER);
    // Not `add_enabled_ui`, which fades everything painted inside it toward the
    // background: the weak text colour below is the whole of what a disabled tab
    // says about itself, and a fade would wash it out further. The colours below
    // say what state a tab is in; the sense is what stops it being clicked.
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(galley.size().x + TAB_PADDING * 2.0, TAB_HEIGHT), sense);
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, workspace.label()));

    let visuals = ui.visuals();
    ui.painter().rect_filled(rect, GROUP_CORNER_RADIUS, tab_fill(visuals, bar_fill, state, response.hovered()));
    let text = match state {
        TabState::Active => visuals.strong_text_color(),
        TabState::Inactive => visuals.text_color(),
        TabState::Disabled => visuals.weak_text_color(),
    };
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, text);

    if response.clicked() {
        select_workspace(editor, commands, workspace);
    }
}

/// What a workspace tab is showing about itself.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TabState {
    Active,
    Inactive,
    Disabled,
}

/// Fill for a workspace tab, stepped off `bar_fill` the way Blender steps its
/// workspace tabs.
///
/// The open workspace's tab takes the panel surface itself - it reads as the
/// workspace below coming up through the bar, which is exactly what Blender's
/// active tab does. Every other tab takes the bar's own colour, so no cell
/// outline shows around it and only its label is left on the backdrop; what
/// separates an available workspace from one with nothing behind it yet is the
/// text colour alone. An available tab still lights up under the pointer, and
/// that step is signed by theme so "lighter" means away from the background in
/// both.
fn tab_fill(visuals: &egui::Visuals, bar_fill: egui::Color32, state: TabState, hovered: bool) -> egui::Color32 {
    let away = if visuals.dark_mode { 1 } else { -1 };
    let hover = i16::from(hovered && state != TabState::Disabled) * 6;
    match state {
        TabState::Active => crate::ui::widgets::shifted(visuals.panel_fill, away * hover),
        TabState::Inactive => crate::ui::widgets::shifted(bar_fill, away * hover),
        TabState::Disabled => bar_fill,
    }
}

/// The File menu: everything about the project as a file on disk.
#[cfg(not(target_os = "macos"))]
fn draw_file_menu(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    MenuBarMenu::new("File").show(ui, |ui| {
        let has_unsaved = project.projects.iter().any(UiProjectEntry::needs_save);
        let active_project = project.projects.iter().find(|entry| entry.is_active);
        if ContextMenuAction::new("Save Project").enabled(has_unsaved).show(ui).clicked() {
            commands.push(UiCommand::SaveProject);
            ui.close();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if ContextMenuAction::new("Save Project As...").enabled(active_project.is_some()).show(ui).clicked() {
            if let Some(project) = active_project {
                commands.push(UiCommand::SaveProjectAs(project.runtime_id));
            }
            ui.close();
        }
        context_menu_separator(ui);
        if ContextMenuAction::new("New Project...").show(ui).clicked() {
            commands.push(UiCommand::NewProject);
            ui.close();
        }
        if ContextMenuAction::new("Open Project...").show(ui).clicked() {
            commands.push(UiCommand::OpenProject);
            ui.close();
        }
        draw_open_recent(ui, project, commands);
        // Disabled until the project is a file: a never-saved one is nowhere
        // to be shown.
        #[cfg(not(target_arch = "wasm32"))]
        if ContextMenuAction::new(SHOW_PROJECT_LABEL).enabled(project.active_path.is_some()).show(ui).clicked() {
            commands.push(UiCommand::ShowProjectInFileManager);
            ui.close();
        }
        context_menu_separator(ui);
        if ContextMenuAction::new("Import...").enabled(active_project.is_some()).show(ui).clicked() {
            editor.show_import = true;
            editor.show_export = false;
            ui.close();
        }
        if ContextMenuAction::new("Export...").enabled(active_project.is_some()).show(ui).clicked() {
            editor.show_import = false;
            editor.show_export = true;
            ui.close();
        }
        if ContextMenuAction::new("Export Viewport Image...").show(ui).clicked() {
            commands.push(UiCommand::ExportViewportImage);
            ui.close();
        }
        if ContextMenuAction::new("Export Engineering Drawing...").show(ui).clicked() {
            commands.push(UiCommand::OpenPlotDialog);
            ui.close();
        }
        context_menu_separator(ui);
        if ContextMenuAction::new(format!("About {}...", crate::APP_NAME)).show(ui).clicked() {
            editor.show_about = true;
            ui.close();
        }
        if ContextMenuAction::new("Exit Application").show(ui).clicked() {
            commands.push(UiCommand::RequestExit);
            ui.close();
        }
    });
}

/// The View menu, beside File: the view preferences reached often enough to
/// want a row of their own.
///
/// Each row is a switch onto the same setting the Interface preferences tab
/// holds - see [`crate::ui::elements::properties`] - rather than a second
/// piece of state, so a change here shows in that tab and is saved with it.
#[cfg(not(target_os = "macos"))]
fn draw_view_menu(ui: &mut egui::Ui, editor: &EditorState, commands: &mut Vec<UiCommand>) {
    let preferences = editor.current_preferences();
    MenuBarMenu::new("View").show(ui, |ui| {
        for toggle in [ViewToggle::Console, ViewToggle::DarkMode, ViewToggle::XyGrid] {
            if ContextMenuAction::new(toggle.label()).checked(toggle.get(&preferences)).show(ui).clicked() {
                commands.push(UiCommand::ToggleViewOption(toggle));
                ui.close();
            }
        }
    });
}

/// The File menu's Open Recent submenu: the same remembered projects the
/// welcome splash lists, so a project can be reopened without the splash or
/// the file chooser.
///
/// Greyed out with nothing to offer rather than hidden, so the menu keeps its
/// shape. The open project is not among the rows - see
/// [`UiProjectView::recent_projects`] - so none of them is ever the dirty one
/// and no `*` marker is needed.
#[cfg(not(target_os = "macos"))]
fn draw_open_recent(ui: &mut egui::Ui, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    let recent: Vec<_> = project.recent_projects().collect();
    context_submenu(ui, "Open Recent", !recent.is_empty(), |ui| {
        for entry in recent {
            let row = ContextMenuAction::new(entry.name.as_str()).show(ui);
            // A row is a file stem, so two remembered projects can read alike;
            // the full path tells them apart.
            #[cfg(not(target_arch = "wasm32"))]
            let row = row.on_hover_text(entry.path.display().to_string());
            if row.clicked() {
                #[cfg(not(target_arch = "wasm32"))]
                commands.push(UiCommand::ActivateTrackedProject(entry.path.clone()));
                #[cfg(target_arch = "wasm32")]
                commands.push(UiCommand::ActivateTrackedProject(entry.id));
                ui.close();
            }
        }
    });
}

/// Draw the menus belonging to the Production workspace, for the viewport bar
/// to place at the head of its own row.
///
/// These act on what is in the scene rather than on the application, so they
/// follow the workspace rather than sitting in the menu bar above it. On macOS
/// they are in the system menu bar instead and this draws nothing.
///
/// `row_height` is how tall a label's hover fill is drawn: the bar is taller
/// than the labels need, so that fill sits inside it rather than reaching its
/// edges.
///
/// Deliberately not wrapped in an `egui::MenuBar`, which claims the whole
/// width it is given: these menus are one cluster in a row with two more, so
/// they take only what they use. Every dropdown here is a `MenuBarMenu`, which
/// carries its own menu config rather than reading the bar's, so the only
/// thing the wrapper would have added is the flat button style below.
pub(crate) fn draw_production_menus(ui: &mut egui::Ui, editor: &EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>, row_height: f32) {
    #[cfg(target_os = "macos")]
    {
        let _ = (ui, editor, project, commands, row_height);
    }

    #[cfg(not(target_os = "macos"))]
    ui.scope(|ui| {
        egui::containers::menu::menu_style(ui.style_mut());
        // After `menu_style`, which sets the padding this overrides.
        let spacing = &mut ui.style_mut().spacing;
        spacing.interact_size.y = row_height;
        spacing.button_padding = egui::vec2(MENU_LABEL_PADDING, 0.0);
        spacing.item_spacing.x = MENU_LABEL_GAP;
        MenuBarMenu::new("Design").show(ui, |ui| {
            // Every entry here acts on the current design selection.
            let has_selection = editor.selected_handles.iter().any(|handle| matches!(handle, SceneEntityId::Object(_)));
            context_submenu(ui, "Insert Point", has_selection, |ui| {
                // Needs two or more crossing polylines to insert anything.
                if ContextMenuAction::new("At intersection").enabled(editor.selection_has_intersections).show(ui).clicked() {
                    commands.push(UiCommand::InsertPointsAtIntersections);
                    ui.close();
                }
                if ContextMenuAction::new("At elevation...").show(ui).clicked() {
                    commands.push(UiCommand::OpenInsertPointAtElevationDialog);
                    ui.close();
                }
            });
            context_menu_separator(ui);
            context_submenu(ui, "Move to", has_selection, |ui| {
                for axis in [Axis::X, Axis::Y, Axis::Z] {
                    if ContextMenuAction::new(format!("Set {}...", axis.label())).show(ui).clicked() {
                        commands.push(UiCommand::OpenMoveToAxisDialog(axis));
                        ui.close();
                    }
                }
            });
            context_menu_separator(ui);
            // Unlike the entries above this one runs with nothing
            // selected: the dialog seeds from the selection when there
            // is one, and otherwise you pick in the viewport with it open.
            if ContextMenuAction::new("Create Triangulation...").show(ui).clicked() {
                commands.push(UiCommand::OpenCreateTriangulation);
                ui.close();
            }
        });

        MenuBarMenu::new("Triangulation").show(ui, |ui| {
            if ContextMenuAction::new("Clip Surface by Polyline...").show(ui).clicked() {
                commands.push(UiCommand::OpenCutTriangulationByPolyline);
                ui.close();
            }
            if ContextMenuAction::new("Slice Triangulation by Z Range...").show(ui).clicked() {
                commands.push(UiCommand::OpenCutTriangulationByZ);
                ui.close();
            }
            if ContextMenuAction::new("Trim to Topology...").show(ui).clicked() {
                commands.push(UiCommand::OpenCutTriangulationBySurface);
                ui.close();
            }
            context_menu_separator(ui);
            if ContextMenuAction::new("Cut Topology with Pit Shell...").show(ui).clicked() {
                commands.push(UiCommand::OpenCutTopologyByPitShell);
                ui.close();
            }
            if ContextMenuAction::new("Merge Shell into Topology...").show(ui).clicked() {
                commands.push(UiCommand::OpenIncludeSolidInTopology);
                ui.close();
            }
            context_menu_separator(ui);
            if ContextMenuAction::new("Generate Contour Lines...").show(ui).clicked() {
                commands.push(UiCommand::OpenContourTriangulation);
                ui.close();
            }
        });

        MenuBarMenu::new("Raster").show(ui, |ui| {
            let any_draped = project.raster_textures.iter().any(|raster| raster.is_draped);
            if ContextMenuAction::new("Undrape All").enabled(any_draped).show(ui).clicked() {
                commands.push(UiCommand::UndrapeAllRasters);
                ui.close();
            }
        });

        MenuBarMenu::new("Point Cloud").show(ui, |ui| {
            let has_loaded_cloud = project.point_clouds.iter().any(|cloud| cloud.is_loaded);
            if ContextMenuAction::new("Create Triangulation...").enabled(has_loaded_cloud).show(ui).clicked() {
                commands.push(UiCommand::OpenPointCloudTin);
                ui.close();
            }
        });

        MenuBarMenu::new("Block Model").show(ui, |ui| {
            if ContextMenuAction::new("Create Ore Triangulation...")
                .enabled(!project.block_models.is_empty())
                .show(ui)
                .clicked()
            {
                commands.push(UiCommand::OpenCreateOreTriangulation);
                ui.close();
            }
        });

        MenuBarMenu::new("Drill Holes").show(ui, |ui| {
            let has_loaded_holes = project.drill_holes.iter().any(|dataset| dataset.is_loaded);
            if ContextMenuAction::new("Create Block Model...").enabled(has_loaded_holes).show(ui).clicked() {
                commands.push(UiCommand::OpenCreateBlockModel(None));
                ui.close();
            }
        });
    });
}
