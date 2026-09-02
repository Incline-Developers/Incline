//! Object editing and viewport tool dialogs.

use crate::{
    i18n::{tr, tr_format},
    model::{Axis, Document, ObjectId},
    ui::{
        state::{ActiveTool, BatterBermMode, DrapePhase, EditorState, HeightMode, MoveToLayerDialog, OffsetMeasure, RelimitMode, TrimEnd, UiCommand, UiProjectView},
        themed_icon, unthemed_icon,
        widgets::{
            context_menu::{ContextMenu, ContextMenuAction, context_menu_popup, context_menu_separator},
            menu::{self, DragableMenu, MenuButton, MenuField, MenuFieldBool, MenuFieldCombo, MenuFieldF64, MenuFieldRgba, MenuFieldText, MenuFieldU32},
            viewport::ViewportDockPanel,
        },
    },
};

/// Draw the two-step selection prompt for Drape to Topology at the top of the viewport.
pub(crate) fn draw_drape_selection_panel(ui: &mut egui::Ui, editor: &EditorState, commands: &mut Vec<UiCommand>, viewport_rect: egui::Rect) {
    if editor.active_tool != ActiveTool::DrapeToTopology {
        return;
    }

    let selected_count = match editor.drape_phase {
        DrapePhase::Designs => editor
            .selected_handles
            .iter()
            .filter(|handle| matches!(handle, crate::model::SceneEntityId::Object(_)))
            .count(),
        DrapePhase::Topologies => editor
            .selected_handles
            .iter()
            .filter(|handle| matches!(handle, crate::model::SceneEntityId::Triangulation(_)))
            .count(),
    };

    ViewportDockPanel::new("drape_to_topology_panel", tr!(literal = "Drape to Topology"), viewport_rect)
        .min_width(280.0)
        .show(ui.ctx(), |ui| {
            ui.label(tr!("ui-selected-count", count = selected_count));
            ui.add_space(6.0);
            if ui.add(MenuButton::new(tr!(literal = "Confirm Selection")).primary().enabled(selected_count > 0)).clicked() {
                commands.push(UiCommand::ConfirmDrapeSelection);
            }
        });
}

/// Title for the canvas context menu, named after the kind of entity that is
/// selected (e.g. "Triangulation Properties"). Selections spanning several
/// kinds fall back to a generic label.
fn object_kind_label(object: &crate::model::Object) -> String {
    match object {
        crate::model::Object::Point { .. } => tr!(literal = "Point"),
        crate::model::Object::Polyline { verts, .. } if verts.len() == 2 => tr!(literal = "Line"),
        crate::model::Object::Polyline { .. } => tr!(literal = "Polyline"),
        crate::model::Object::Text { .. } => tr!(literal = "Text"),
    }
}

fn canvas_context_menu_title(editor: &EditorState, document: &Document) -> String {
    use crate::model::SceneEntityId;

    // (label, whether it came from a document object)
    let mut kind: Option<(String, bool)> = None;
    for &handle in &editor.selected_handles {
        let (label, is_object) = match handle {
            SceneEntityId::Object(id) => (document.get_object(id).map_or_else(|| tr!(literal = "Object"), object_kind_label), true),
            SceneEntityId::Triangulation(_) => (tr!(literal = "Triangulation"), false),
            SceneEntityId::BlockModel(_) => (tr!(literal = "Block Model"), false),
            SceneEntityId::DrillHole(_) => (tr!(literal = "Drill Hole"), false),
            SceneEntityId::PointCloud(_) => (tr!(literal = "Point Cloud"), false),
        };
        match kind.as_ref() {
            None => kind = Some((label, is_object)),
            Some((existing, _)) if existing == &label => {}
            // Mixed object kinds (a line and a text object, say) still share a menu.
            Some((_, true)) if is_object => kind = Some((tr!(literal = "Object"), true)),
            Some(_) => return tr!(literal = "Properties"),
        }
    }

    kind.map_or_else(
        || tr!(literal = "Properties"),
        |(label, _)| tr_format!(literal = "%kind% %properties%", kind = label, properties = tr!(literal = "Properties")),
    )
}

/// Draw the canvas right-click context menu for selected objects and triangulations.
///
/// Actions only: an object's own values (colour, shape, fill, line weight) are
/// edited in the explorer's Design properties tab. Updates `geometry_dirty`
/// when changes are made.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_right_click_context(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    project: &UiProjectView,
    commands: &mut Vec<UiCommand>,
    geometry_dirty: &mut bool,
    document: &Document,
    px: f32,
    py: f32,
) {
    let ppp = ui.ctx().pixels_per_point();
    let pos = egui::pos2(px / ppp + 4.0, py / ppp + 4.0);
    let title = canvas_context_menu_title(editor, document);
    ContextMenu::new("canvas_properties", title).position(pos).width(220.0).show(ui.ctx(), |ui| {
        // Gather selected document objects
        let selected_obj_ids: Vec<ObjectId> = editor
            .selected_handles
            .iter()
            .filter_map(|&h| match h {
                crate::model::SceneEntityId::Object(id) => Some(id),
                _ => None,
            })
            .collect();

        let selected_drill_hole = editor.selected_handles.iter().find_map(|&h| match h {
            crate::model::SceneEntityId::DrillHole(id) => Some(id),
            _ => None,
        });

        let has_doc_objects = !selected_obj_ids.is_empty();

        // --- Drill hole colouring ---
        if let Some(drill_hole_id) = selected_drill_hole {
            if ContextMenuAction::new(tr!(literal = "Colour by...")).show(ui).clicked() {
                commands.push(UiCommand::OpenDrillHoleColorDialog(drill_hole_id));
                commands.push(UiCommand::CloseCanvasContextMenu);
            }

            context_menu_separator(ui);
        }

        if has_doc_objects {
            if ContextMenuAction::new(tr!(literal = "Move to Layer...")).show(ui).clicked() {
                let target_layer = project
                    .projects
                    .iter()
                    .find(|entry| entry.is_active)
                    .and_then(|entry| entry.layers.first())
                    .map(|layer| layer.id);
                editor.move_to_layer_dialog = Some(MoveToLayerDialog {
                    object_ids: selected_obj_ids.clone(),
                    target_layer,
                    copy: false,
                });
                commands.push(UiCommand::CloseCanvasContextMenu);
            }

            context_menu_separator(ui);
        }

        if !editor.selected_handles.is_empty() {
            if ContextMenuAction::new(tr!(literal = "Hide Selection")).show(ui).clicked() {
                commands.push(UiCommand::HideSelection);
                commands.push(UiCommand::CloseCanvasContextMenu);
            }
            if ContextMenuAction::new(tr!(literal = "Lock Selection")).show(ui).clicked() {
                *geometry_dirty |= editor.apply_action(crate::ui::state::EditorAction::FreezeSelection);
                commands.push(UiCommand::CloseCanvasContextMenu);
            }
            context_menu_separator(ui);
        }

        if ContextMenuAction::new(tr!(literal = "Close")).show(ui).clicked() {
            commands.push(UiCommand::CloseCanvasContextMenu);
        }
    });
}

pub(crate) fn draw_move_to_layer_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    let Some(active_project) = project.projects.iter().find(|entry| entry.is_active) else {
        editor.move_to_layer_dialog = None;
        return;
    };
    let Some(dialog) = editor.move_to_layer_dialog.as_mut() else {
        return;
    };

    if dialog.target_layer.is_none_or(|id| !active_project.layers.iter().any(|layer| layer.id == id)) {
        dialog.target_layer = active_project.layers.first().map(|layer| layer.id);
    }

    let selected_label = dialog
        .target_layer
        .and_then(|id| active_project.layers.iter().find(|layer| layer.id == id))
        .map(|layer| layer.name.clone())
        .unwrap_or_else(|| tr!(literal = "Choose a layer"));
    let layer_options = active_project.layers.iter().map(|layer| (Some(layer.id), layer.name.clone().into()));
    let can_apply = dialog.target_layer.is_some() && !dialog.object_ids.is_empty();
    let object_count = dialog.object_ids.len();
    let mut close = false;
    let mut apply = false;
    let mut open = true;

    DragableMenu::new("move_to_layer_dialog", tr!(literal = "Move to Layer"))
        .open(&mut open)
        .min_width(260.0)
        .max_width(280.0)
        .show(ui.ctx(), |ui| {
            // Added in reverse: a field row lays its control out from the right.
            MenuField::new(tr!(literal = "Action")).show(ui, |ui, _, _| {
                ui.horizontal(|ui| {
                    if ui.add(MenuButton::new(tr!(literal = "Copy")).selected(dialog.copy).min_width(64.0)).clicked() {
                        dialog.copy = true;
                    }
                    if ui.add(MenuButton::new(tr!(literal = "Move")).selected(!dialog.copy).min_width(64.0)).clicked() {
                        dialog.copy = false;
                    }
                })
                .response
            });
            MenuFieldCombo::new("move_to_layer_target", tr!(literal = "Layer"), &mut dialog.target_layer, selected_label, layer_options)
                .width(180.0)
                .show(ui);
            menu::menu_actions(ui, |ui| {
                let action_label = if dialog.copy { tr!(literal = "Copy") } else { tr!(literal = "Move") };
                let confirm = menu::dialog_confirm_pressed(ui.ctx());
                if ui.add(MenuButton::new(action_label).primary().enabled(can_apply)).clicked() || (confirm && can_apply) {
                    apply = true;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    close = true;
                }
            });
            ui.label(tr!("ui-selected-objects", count = object_count));
        });

    if apply {
        if let Some(target_layer) = dialog.target_layer {
            commands.push(UiCommand::MoveObjectsToLayer {
                object_ids: dialog.object_ids.clone(),
                target_layer,
                copy: dialog.copy,
            });
        }
        editor.move_to_layer_dialog = None;
    } else if close || !open {
        editor.move_to_layer_dialog = None;
    }
}

pub(crate) fn draw_move_to_axis_dialog(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let Some(dialog) = editor.move_to_axis_dialog.as_mut() else {
        return;
    };

    let axis = dialog.axis;
    let axis_label = axis.label();
    let object_count = dialog.object_ids.len();
    let can_apply = dialog.value.is_finite() && object_count > 0;
    let mut close = false;
    let mut apply = false;
    let mut open = true;

    DragableMenu::new("move_to_axis_dialog", tr_format!(literal = "Set %axis%", axis = axis_label))
        .open(&mut open)
        .min_width(260.0)
        .max_width(280.0)
        .show(ui.ctx(), |ui| {
            MenuFieldF64::new(tr_format!(literal = "%axis% value", axis = axis_label), &mut dialog.value, f64::MIN..=f64::MAX)
                .width(120.0)
                .show(ui);
            if !dialog.value.is_finite() {
                ui.colored_label(egui::Color32::from_rgb(200, 70, 70), tr!("ui-invalid-axis-value", axis = axis_label));
            }
            ui.add_space(4.0);
            let submitted = menu::dialog_confirm_pressed(ui.ctx());
            let cancelled = menu::dialog_cancel_pressed(ui.ctx());
            menu::menu_actions(ui, |ui| {
                if (submitted || ui.add(MenuButton::new(tr!(literal = "Apply")).primary().enabled(can_apply)).clicked()) && can_apply {
                    apply = true;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || cancelled {
                    close = true;
                }
            });
            ui.label(tr!("ui-selected-objects", count = object_count));
        });

    if apply {
        let value = dialog.value;
        commands.push(UiCommand::BatchSetAxisValue(dialog.object_ids.clone(), axis, value));
        if axis == Axis::Z {
            editor.z_input = value;
            editor.z_level = value;
        }
        editor.move_to_axis_dialog = None;
    } else if close || !open {
        editor.move_to_axis_dialog = None;
    }
}

pub(crate) fn draw_insert_point_at_elevation_dialog(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let Some(dialog) = editor.insert_point_at_elevation_dialog.as_mut() else {
        return;
    };

    let object_count = dialog.object_ids.len();
    let can_apply = dialog.elevation.is_finite() && object_count > 0;
    let mut close = false;
    let mut apply = false;
    let mut open = true;

    DragableMenu::new("insert_point_at_elevation_dialog", tr!(literal = "Insert Point at Elevation"))
        .open(&mut open)
        .min_width(280.0)
        .max_width(300.0)
        .show(ui.ctx(), |ui| {
            MenuFieldF64::new(tr!(literal = "Elevation"), &mut dialog.elevation, dialog.min_elevation..=dialog.max_elevation)
                .width(120.0)
                .show(ui);
            if !dialog.elevation.is_finite() {
                ui.colored_label(egui::Color32::from_rgb(200, 70, 70), tr!(literal = "Enter a valid elevation."));
            }
            if dialog.min_elevation > f64::MIN {
                ui.label(tr!(
                    "ui-selection-spans",
                    min = format!("{:.2}", dialog.min_elevation),
                    max = format!("{:.2}", dialog.max_elevation)
                ));
            }
            ui.label(tr!(literal = "Segments lying at this elevation are ignored."));
            ui.add_space(4.0);
            let submitted = menu::dialog_confirm_pressed(ui.ctx());
            let cancelled = menu::dialog_cancel_pressed(ui.ctx());
            menu::menu_actions(ui, |ui| {
                if (submitted || ui.add(MenuButton::new(tr!(literal = "Apply")).primary().enabled(can_apply)).clicked()) && can_apply {
                    apply = true;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || cancelled {
                    close = true;
                }
            });
            ui.label(tr!("ui-selected-polylines", count = object_count));
        });

    if apply {
        commands.push(UiCommand::InsertPointsAtElevation {
            object_ids: dialog.object_ids.clone(),
            elevation: dialog.elevation,
        });
        editor.z_input = dialog.elevation;
        editor.insert_point_at_elevation_dialog = None;
    } else if close || !open {
        editor.insert_point_at_elevation_dialog = None;
    }
}

/// Draw the welcome splash the application starts on.
///
/// A project is already open behind it - startup lands on an empty, never-saved
/// one - so the splash is an offer, not a gate: Escape, a click on the backdrop,
/// or `New project` all simply dismiss it and leave that project in place.
pub(crate) fn draw_select_project_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    const PANEL_SIZE: f32 = 500.0;
    const COLUMN_WIDTH: f32 = 190.0;
    const ROW_HEIGHT: f32 = 22.0;
    const RECENT_HEIGHT: f32 = 100.0;
    /// Grow the splash by exactly as much as the Recent box has grown from the
    /// original two-row grid, keeping the footer and surrounding spacing put.
    const PANEL_HEIGHT: f32 = PANEL_SIZE * 0.7 + (RECENT_HEIGHT - 48.0);

    // The splash is the only place a remembered project can be picked up now
    // that the explorer shows the open one alone. The active project is never
    // among these: the splash is only up when nothing but the startup project
    // is open.
    let recent: Vec<&crate::ui::state::UiTrackedProjectEntry> = project.recent_projects().collect();

    // Only the update notice below reads the editor, and that is native-only.
    #[cfg(target_arch = "wasm32")]
    let _ = editor;

    // The splash floats over a live window, so something has to catch the
    // clicks that dismiss it, or they would fall through to the viewport and
    // orbit the camera instead. It paints nothing: the window behind the
    // splash reads as it always does.
    let screen = ui.ctx().viewport_rect();
    let backdrop = egui::Area::new(egui::Id::new("select_project_backdrop"))
        .fixed_pos(screen.min)
        .order(egui::Order::Middle)
        .show(ui.ctx(), |ui| ui.allocate_exact_size(screen.size(), egui::Sense::click()).1);
    if backdrop.inner.clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
        commands.push(UiCommand::CloseStartupDialog);
    }

    egui::Area::new(egui::Id::new("select_project_dialog"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(ui.visuals().window_fill())
                .stroke(ui.visuals().window_stroke())
                .corner_radius(egui::CornerRadius::ZERO)
                .inner_margin(egui::Margin::ZERO)
                .show(ui, |ui| {
                    ui.set_width(PANEL_SIZE);
                    ui.set_height(PANEL_HEIGHT);
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 16.0);

                    // Claimed before the content above it, so that content -
                    // and the Recent list in particular - lays out against
                    // what is genuinely left rather than pushing this out of
                    // the frame. It still draws along the bottom edge.
                    egui::Panel::bottom("meta_splash").show_separator_line(false).show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.label(tr_format!(literal = "%app%: %release%", app = crate::APP_NAME, release = crate::APP_RELEASE));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label("GNU General Public License v3.0"));
                        });
                    });

                    ui.add(egui::Image::new(unthemed_icon!("splash.svg")).shrink_to_fit());

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                        ui.add_space(30.0);
                        select_project_action_column(ui, tr!(literal = "Project"), COLUMN_WIDTH, |ui| {
                            // Startup is already sitting on an empty project,
                            // so this only has to get the splash out of the way.
                            if select_project_action_row(
                                ui,
                                egui::Image::new(themed_icon!(ui, "create_project.svg")),
                                tr!(literal = "New Project"),
                                COLUMN_WIDTH,
                                ROW_HEIGHT,
                            )
                            .clicked()
                            {
                                commands.push(UiCommand::CloseStartupDialog);
                            }
                            if select_project_action_row(
                                ui,
                                egui::Image::new(themed_icon!(ui, "open_project.svg")),
                                tr!(literal = "Load Project"),
                                COLUMN_WIDTH,
                                ROW_HEIGHT,
                            )
                            .clicked()
                            {
                                commands.push(UiCommand::OpenProject);
                            }
                        });

                        ui.add_space(PANEL_SIZE - (COLUMN_WIDTH * 2.0) - 60.0);

                        select_project_action_column(ui, tr!(literal = "Application"), COLUMN_WIDTH, |ui| {
                            if select_project_action_row(
                                ui,
                                egui::Image::new(themed_icon!(ui, "open_website.svg")),
                                tr!(literal = "Website"),
                                COLUMN_WIDTH,
                                ROW_HEIGHT,
                            )
                            .clicked()
                            {
                                ui.ctx().open_url(egui::OpenUrl::new_tab("https://inclinedesign.net"));
                            }
                            // Escape dismisses the splash rather than firing
                            // this row: leaving is a deliberate click only.
                            if select_project_action_row(
                                ui,
                                egui::Image::new(themed_icon!(ui, "close_project.svg")),
                                tr!(literal = "Exit Application"),
                                COLUMN_WIDTH,
                                ROW_HEIGHT,
                            )
                            .clicked()
                            {
                                commands.push(UiCommand::RequestExit);
                            }
                        });
                    });

                    // The splash is a fixed shape, so the Recent list takes
                    // the room left below the two columns rather than making
                    // the frame taller. 16pt of that is the gap above its
                    // heading. The box presents its entries as one full-width
                    // scrolling list and keeps two rows visible at once.
                    let list_width = PANEL_SIZE - 60.0;
                    if !recent.is_empty() {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                            ui.add_space(30.0);
                            select_project_action_column(ui, tr!(literal = "Recent"), list_width, |ui| {
                                draw_recent_projects(ui, &recent, list_width, RECENT_HEIGHT, ROW_HEIGHT, commands);
                            });
                        });
                    }
                });

            #[cfg(target_arch = "wasm32")]
            {
                let warning_color = if ui.visuals().dark_mode {
                    egui::Color32::from_rgb(255, 190, 80)
                } else {
                    egui::Color32::from_rgb(145, 85, 0)
                };

                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(ui.visuals().window_fill())
                    .stroke(egui::Stroke::new(1.0, warning_color))
                    .corner_radius(3.0)
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(PANEL_SIZE - 24.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(tr_format!(
                                literal = "%app% Web is not recommended for production use. Only use it as a demo.",
                                app = crate::APP_NAME
                            )));
                            ui.hyperlink_to(tr!(literal = "Download the free native version at our website ↗"), "https://inclinedesign.net");
                        });
                    });
            }

            #[cfg(not(target_arch = "wasm32"))]
            if let Some(newest_release) = editor.newer_release.as_deref() {
                let notice_color = if ui.visuals().dark_mode {
                    egui::Color32::from_rgb(125, 190, 255)
                } else {
                    egui::Color32::from_rgb(25, 95, 165)
                };

                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(ui.visuals().window_fill())
                    .stroke(egui::Stroke::new(1.0, notice_color))
                    .corner_radius(3.0)
                    .inner_margin(egui::Margin::symmetric(12, 9))
                    .show(ui, |ui| {
                        ui.set_width(PANEL_SIZE - 24.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(egui::RichText::new(tr!("update-available", version = newest_release)).strong());
                                ui.label(egui::RichText::new(tr!("update-current", version = crate::APP_RELEASE)).color(ui.visuals().weak_text_color()));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(MenuButton::new(tr!(literal = "Download")).primary().min_width(92.0)).clicked() {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab("https://inclinedesign.net/downloads/"));
                                }
                            });
                        });
                    });
            }
        });
}

fn draw_recent_projects(ui: &mut egui::Ui, recent: &[&crate::ui::state::UiTrackedProjectEntry], width: f32, height: f32, row_height: f32, commands: &mut Vec<UiCommand>) {
    const INSET: i8 = 3;

    let (surface, stripe) = crate::ui::widgets::tree_row_colors(ui);
    let frame = egui::Frame::new()
        .fill(surface)
        .stroke(ui.visuals().window_stroke())
        .corner_radius(egui::CornerRadius::same(crate::ui::widgets::toolbar::GROUP_CORNER_RADIUS))
        .inner_margin(egui::Margin::same(INSET));

    frame.show(ui, |ui| {
        let inset = f32::from(INSET);
        ui.set_width(width - inset * 2.0);
        // The banding belongs to the box, not to the entries: it tiles the
        // whole list the way the explorer's does, so a couple of entries read
        // as a striped list rather than as one stray dark bar. Reserved here,
        // outside the scroll area, so the bands sit behind the rows and can be
        // measured against the box itself once its rect and scroll offset are
        // known - inside, the only rect wide enough to band is the dialog's
        // clip rect, which is the whole window.
        let stripes_slot = ui.painter().add(egui::Shape::Noop);
        let scroll = egui::ScrollArea::vertical()
            .id_salt("recent_projects_scroll")
            .max_height(height - inset * 2.0)
            .min_scrolled_height(0.0)
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for entry in recent.iter() {
                    let label = if entry.dirty { format!("{} *", entry.name) } else { entry.name.clone() };
                    let row = select_project_action_row(
                        ui,
                        egui::Image::new(themed_icon!(ui, "recent_project.svg")),
                        label.clone(),
                        ui.available_width(),
                        row_height,
                    );
                    #[cfg(not(target_arch = "wasm32"))]
                    let row = row.on_hover_text(format!("{}\n{}", entry.name, entry.path.display()));
                    #[cfg(target_arch = "wasm32")]
                    let row = row.on_hover_text(format!(
                        "{}\n{}",
                        entry.name,
                        if entry.stored_in_browser {
                            tr!(literal = "Saved in browser storage")
                        } else {
                            tr!(literal = "Not saved in browser storage")
                        }
                    ));
                    if row.clicked() {
                        #[cfg(not(target_arch = "wasm32"))]
                        commands.push(UiCommand::ActivateTrackedProject(entry.path.clone()));
                        #[cfg(target_arch = "wasm32")]
                        commands.push(UiCommand::ActivateTrackedProject(entry.id));
                    }
                    context_menu_popup(&row, entry.name.as_str(), |ui| {
                        if ContextMenuAction::new(tr!(literal = "Remove from List")).show(ui).clicked() {
                            #[cfg(not(target_arch = "wasm32"))]
                            commands.push(UiCommand::RemoveTrackedProject(entry.path.clone()));
                            #[cfg(target_arch = "wasm32")]
                            commands.push(UiCommand::RemoveTrackedProject(entry.id));
                            ui.close();
                        }
                    });
                }
            });
        // Anchored to the content's top edge - the box top less however far the
        // list is scrolled - so the bands travel with the rows rather than
        // staying put under them.
        let box_rect = scroll.inner_rect;
        let bands = crate::ui::widgets::explorer::stripe_bands(box_rect.x_range(), box_rect.top() - scroll.state.offset.y, box_rect.bottom(), row_height, stripe);
        // Clipped to the box: when the list is scrolled the first band starts
        // above the box's top edge.
        ui.painter().with_clip_rect(box_rect).set(stripes_slot, bands);
    });
}

fn select_project_action_column(ui: &mut egui::Ui, heading: impl Into<String>, width: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.label(egui::RichText::new(heading.into()).size(12.0).color(ui.visuals().weak_text_color()));
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 4.0);
        add_contents(ui);
    });
}

fn select_project_action_row(ui: &mut egui::Ui, icon: egui::Image<'static>, label: impl Into<String>, width: f32, height: f32) -> egui::Response {
    select_project_action_row_with_fill(ui, icon, label, width, height, egui::Color32::TRANSPARENT)
}

fn select_project_action_row_with_fill(ui: &mut egui::Ui, icon: egui::Image<'static>, label: impl Into<String>, width: f32, height: f32, fill: egui::Color32) -> egui::Response {
    let label = label.into();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let hovered = response.hovered();
    if fill != egui::Color32::TRANSPARENT || hovered {
        ui.painter().rect_filled(
            rect,
            if hovered { egui::CornerRadius::same(2) } else { egui::CornerRadius::ZERO },
            if hovered { ui.visuals().widgets.hovered.bg_fill } else { fill },
        );
    }

    let icon_size = egui::vec2(22.0, 22.0);
    let icon_rect = egui::Rect::from_min_size(egui::pos2(rect.left() + 2.0, rect.center().y - icon_size.y / 2.0), icon_size);
    icon.fit_to_exact_size(icon_size).paint_at(ui, icon_rect);

    // Rows are laid out to a fixed width, so long labels truncate rather than
    // running beyond their action area. The full name stays on the hover text.
    let text_left = rect.left() + 30.0;
    let text_color = ui.visuals().text_color();
    let mut job = egui::text::LayoutJob::single_section(label.to_owned(), egui::TextFormat::simple(egui::FontId::proportional(13.0), text_color));
    job.wrap = egui::text::TextWrapping::truncate_at_width((rect.right() - 4.0 - text_left).max(0.0));
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(egui::pos2(text_left, rect.center().y - galley.size().y / 2.0), galley, text_color);

    response
}

/// Draw the browser-only prompt used to name a new project before it is created.
#[cfg(target_arch = "wasm32")]
pub(crate) fn draw_create_project_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, viewport_rect: egui::Rect) {
    ViewportDockPanel::new("create_project_panel", tr!(literal = "Create a new project"), viewport_rect)
        .min_width(240.0)
        .show(ui.ctx(), |ui| {
            let can_create = !editor.new_project_name.trim().is_empty();
            MenuFieldText::new(tr!(literal = "Project name"), &mut editor.new_project_name)
                .hint_text(tr!(literal = "Required"))
                .show(ui);
            let submitted = menu::dialog_confirm_pressed(ui.ctx());
            let cancelled = menu::dialog_cancel_pressed(ui.ctx());
            menu::menu_actions(ui, |ui| {
                if (submitted || ui.add(MenuButton::new(tr!(literal = "Create project")).primary().enabled(can_create)).clicked()) && can_create {
                    commands.push(UiCommand::CreateBrowserProject {
                        name: editor.new_project_name.trim().to_owned(),
                    });
                    editor.new_project_dialog_open = false;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || cancelled {
                    editor.new_project_dialog_open = false;
                }
            });
        });
}

/// Draw the "Create a new layer" dialog (opened when NewLayer tool is active).
pub(crate) fn draw_create_layer_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, project: &UiProjectView, viewport_rect: egui::Rect) {
    if !project.has_active_project {
        editor.new_layer_dialog_open = false;
        return;
    }

    ViewportDockPanel::new("create_layer_panel", tr!(literal = "Create a new layer"), viewport_rect)
        .min_width(220.0)
        .show(ui.ctx(), |ui| {
            let can_save = !editor.new_layer_name.trim().is_empty();
            MenuFieldText::new(tr!(literal = "Layer name"), &mut editor.new_layer_name)
                .hint_text(tr!(literal = "Required"))
                .show(ui);
            let submitted = menu::dialog_confirm_pressed(ui.ctx());
            let cancelled = menu::dialog_cancel_pressed(ui.ctx());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let create_clicked = ui.add(MenuButton::new(tr!(literal = "Create Layer")).primary().enabled(can_save)).clicked();
                if (submitted || create_clicked) && can_save {
                    commands.push(UiCommand::CreateLayer {
                        name: editor.new_layer_name.trim().to_string(),
                    });
                    editor.new_layer_dialog_open = false;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || cancelled {
                    editor.new_layer_dialog_open = false;
                }
            });
        });
}

/// Draw the rename floating dialog for whichever explorer item is being renamed.
pub(crate) fn draw_rename_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    let Some((target, _)) = editor.renaming_item else {
        return;
    };
    // Work on a local copy of the name buffer to avoid borrow conflicts inside the closure.
    let mut name_buf = editor.renaming_item.as_ref().map(|(_, n)| n.clone()).unwrap_or_default();
    let mut close = false;
    let mut rename_to: Option<String> = None;
    let mut open = true;
    DragableMenu::new("rename_dialog", tr!("dialog-rename-title", kind = target.kind_label()))
        .open(&mut open)
        .max_width(380.0)
        .show(ui.ctx(), |ui| {
            // `menu_field_row` gives the entry its requested width and leaves the
            // rest of the row to the label, so the extra width here is the label's.
            MenuFieldText::new(tr!("dialog-rename-field"), &mut name_buf)
                .width(260.0)
                .hint_text(tr!("dialog-rename-field-hint"))
                .show(ui);
            menu::menu_actions(ui, |ui| {
                let can_rename = !name_buf.trim().is_empty();
                let submitted = menu::dialog_confirm_pressed(ui.ctx());
                if (submitted || ui.add(MenuButton::new(tr!("dialog-rename-submit")).primary().enabled(can_rename)).clicked()) && can_rename {
                    rename_to = Some(name_buf.trim().to_string());
                }
                if ui.add(MenuButton::new(tr!("common-cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    close = true;
                }
            });
        });
    // Write the edited buffer back.
    if let Some((_, ref mut buf)) = editor.renaming_item {
        *buf = name_buf;
    }
    if let Some(new_name) = rename_to {
        commands.push(UiCommand::RenameItem { target, new_name });
    } else if close || !open {
        editor.renaming_item = None;
    }
}

/// Draw the text-editing properties popup (height, rotation, colour, content).
pub(crate) fn draw_text_edit_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, geometry_dirty: &mut bool, viewport_rect: egui::Rect) {
    let Some(object_id) = editor.editing_labels_id else {
        return;
    };
    ViewportDockPanel::new("text_edit_panel", tr!(literal = "Edit Text"), viewport_rect)
        .min_width(260.0)
        .show(ui.ctx(), |ui| {
            let response = MenuFieldText::new(tr!(literal = "Text"), &mut editor.pending_text)
                .width(240.0)
                .hint_text(tr!(literal = "Text"))
                .show(ui);
            if response.changed() {
                *geometry_dirty = true;
            }
            if editor.text_edit_focus_requested {
                response.request_focus();
                editor.text_edit_focus_requested = false;
            }
            *geometry_dirty |= MenuFieldF64::new(tr!(literal = "Height"), &mut editor.pending_text_height, 0.001..=1.0e9)
                .speed(0.25)
                .max_decimals(3)
                .suffix(tr!(literal = "m"))
                .show(ui)
                .changed();
            *geometry_dirty |= MenuFieldF64::new(tr!(literal = "Rotation"), &mut editor.pending_text_rotation_degrees, f64::MIN..=f64::MAX)
                .speed(1.0)
                .suffix(tr!(literal = "°"))
                .show(ui)
                .changed();
            *geometry_dirty |= MenuFieldRgba::new(tr!(literal = "Colour"), &mut editor.pending_text_color)
                .help_text(tr!(literal = "Text colour and opacity."))
                .show(ui)
                .changed();

            let apply_from_enter = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let cancel_from_escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let apply = ui.add(MenuButton::new(tr!(literal = "Apply")).primary()).clicked() || apply_from_enter;
                let cancel = ui
                    .add(MenuButton::new(if editor.text_edit_created { tr!(literal = "Discard") } else { tr!(literal = "Cancel") }))
                    .clicked()
                    || cancel_from_escape;
                if apply {
                    commands.push(UiCommand::CommitTextEdit(
                        object_id,
                        editor.pending_text.clone(),
                        editor.pending_text_height,
                        editor.pending_text_rotation_degrees,
                        editor.pending_text_color,
                    ));
                    editor.text_editing_enabled = false;
                } else if cancel {
                    commands.push(UiCommand::CancelTextEdit);
                    editor.text_editing_enabled = false;
                }
            });
        });
    editor.text_edit_position_frames = editor.text_edit_position_frames.saturating_sub(1);
}

/// Draw the polyline finish dialog (Close / Leave open / Cancel) near the cursor.
pub(crate) fn draw_finish_polyline_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, viewport_rect: egui::Rect) {
    ViewportDockPanel::new("finish_poly_dialog", tr!(literal = "Finish Polyline"), viewport_rect).show(ui, |ui| {
        MenuField::new(tr!(literal = "Shape")).show(ui, |ui, _, _| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Drain the key from egui's queue every frame so it never leaks
                // elsewhere, but honour it only once the Enter that opened this
                // dialog has been released - a key held down to finish the line
                // would otherwise confirm "Open" the instant the dialog appears.
                let confirm_key = menu::dialog_confirm_pressed(ui.ctx());
                if ui.add(MenuButton::new(tr!(literal = "Open")).primary()).clicked() || (confirm_key && editor.poly_finish_dialog_confirm_armed) {
                    commands.push(UiCommand::CommitStrokeOpen);
                    editor.poly_finish_dialog = false;
                }
                if ui.add(MenuButton::new(tr!(literal = "Closed"))).clicked() {
                    commands.push(UiCommand::FinishPolyClose);
                    editor.poly_finish_dialog = false;
                }
            });
        });
    });
}

// I dont like this - we need to clean this up later - too many options and sub menus
/// Draw the Offset Element dialog.
pub(crate) fn draw_offset_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, viewport_rect: egui::Rect) {
    if editor.offset_target_ids.is_empty() {
        return;
    }

    ViewportDockPanel::new("offset_element_panel", tr!(literal = "Offset Element"), viewport_rect)
        .min_width(350.0)
        .show(ui.ctx(), |ui| {
            let response = MenuFieldF64::new(tr!(literal = "Angle"), &mut editor.offset_angle_degrees, -90.0..=90.0)
                .help_text(tr!(
                    literal = "Slope angle of the offset. Positive and negative angles move the copy above or below the source as it moves sideways."
                ))
                .width(70.0)
                .speed(0.5)
                .suffix(tr!(literal = "°"))
                .show(ui);
            if response.changed() {
                editor.offset_angle_degrees = editor.offset_angle_degrees.clamp(-90.0, 90.0);
            }

            ui.add_space(4.0);

            MenuField::new(tr!(literal = "Measure"))
                .help_text(tr!(
                    literal = "Choose whether the entered value is distance along the slope, horizontal width, or vertical height."
                ))
                .show(ui, |ui, _, _| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut editor.offset_measure, OffsetMeasure::Distance, tr!(literal = "Distance"));
                        ui.selectable_value(&mut editor.offset_measure, OffsetMeasure::Width, tr!(literal = "Width"));
                        let height_active = matches!(editor.offset_measure, OffsetMeasure::Height(_));
                        if ui.add(egui::Button::selectable(height_active, tr!(literal = "Height"))).clicked() && !height_active {
                            editor.offset_measure = OffsetMeasure::Height(HeightMode::Relative);
                        }
                    })
                    .response
                });
            if let OffsetMeasure::Height(ref mut mode) = editor.offset_measure {
                MenuField::new(tr!(literal = "Height mode"))
                    .help_text(tr!(
                        literal = "Relative applies a vertical change to every point. Absolute RL projects every point onto one target elevation."
                    ))
                    .show(ui, |ui, _, _| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(mode, HeightMode::Relative, tr!(literal = "Relative (+/-)"));
                            ui.selectable_value(mode, HeightMode::AbsoluteRL, tr!(literal = "Absolute RL"));
                        })
                        .response
                    });
            }

            ui.add_space(4.0);

            // Value label adapts to context
            let value_label = match editor.offset_measure {
                OffsetMeasure::Distance => tr!(literal = "Distance along slope"),
                OffsetMeasure::Width => tr!(literal = "Horizontal distance"),
                OffsetMeasure::Height(HeightMode::Relative) => tr!(literal = "Height change"),
                OffsetMeasure::Height(HeightMode::AbsoluteRL) => tr!(literal = "Target RL"),
            };
            let value_range = if matches!(editor.offset_measure, OffsetMeasure::Height(_)) {
                f64::MIN..=f64::MAX
            } else {
                0.0..=f64::MAX
            };
            MenuFieldF64::new(value_label, &mut editor.offset_value_input, value_range)
                .help_text(tr!(literal = "The value is interpreted using the selected Measure and Height mode."))
                .speed(0.1)
                .suffix(tr!(literal = "m"))
                .show(ui);

            MenuFieldBool::new(tr!(literal = "Collide with Triangulation"), &mut editor.offset_collide_with_triangulation)
                .help_text(tr!(literal = "Stop the generated offset where its path first meets a visible triangulation."))
                .show(ui);

            ui.add_space(8.0);
            let can_pick_side = matches!(editor.offset_measure, OffsetMeasure::Height(HeightMode::AbsoluteRL)) || editor.offset_value_input.abs() > 1e-9;
            let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let pick_side_clicked = ui.add(MenuButton::new(tr!(literal = "Pick Side")).enabled(can_pick_side)).clicked();
                if can_pick_side && (pick_side_clicked || enter_pressed) {
                    queue_begin_offset_pick(commands, editor);
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() {
                    commands.push(UiCommand::CancelOffset);
                }
            });
        });
}

fn queue_begin_offset_pick(commands: &mut Vec<UiCommand>, editor: &EditorState) {
    let rad = editor.offset_angle_degrees.to_radians();
    let tan = rad.tan();
    let (horiz_dist, z_delta, project_to_rl) = match editor.offset_measure {
        OffsetMeasure::Distance => {
            let h = editor.offset_value_input * rad.sin();
            let horiz = editor.offset_value_input * rad.cos();
            (horiz, h, None)
        }
        OffsetMeasure::Width => (editor.offset_value_input, editor.offset_value_input * tan, None),
        OffsetMeasure::Height(HeightMode::Relative) => {
            if tan.abs() < 1e-9 {
                (editor.offset_value_input, 0.0, None)
            } else {
                (editor.offset_value_input / tan, editor.offset_value_input, None)
            }
        }
        OffsetMeasure::Height(HeightMode::AbsoluteRL) => {
            // Project each vertex individually along the batter angle so the
            // whole string lands flat at the target RL.
            (0.0, 0.0, Some((tan, editor.offset_value_input)))
        }
    };

    commands.push(UiCommand::BeginOffsetPick {
        object_ids: editor.offset_target_ids.clone(),
        horiz_dist,
        z_delta,
        project_to_rl,
        collide_with_triangulation: editor.offset_collide_with_triangulation,
    });
}

/// Draw the Generate Batter-Berms dialog.
pub(crate) fn draw_batter_berm_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, viewport_rect: egui::Rect) {
    const CONTROL_WIDTH: f32 = 120.0;

    if editor.batter_berm_target_id.is_none() {
        return;
    }

    ViewportDockPanel::new("batter_berm_panel", tr!(literal = "Generate Batter-Berms"), viewport_rect)
        .min_width(310.0)
        .show(ui.ctx(), |ui| {
            MenuFieldF64::new(tr!(literal = "Berm width"), &mut editor.batter_berm_width, 0.1..=500.0)
                .help_text(tr!(literal = "Horizontal width of each flat berm between successive batters."))
                .width(CONTROL_WIDTH)
                .speed(0.1)
                .max_decimals(2)
                .suffix(tr!(literal = "m"))
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Batter angle (\u{b0})"), &mut editor.batter_berm_angle, 1.0..=89.0)
                .help_text(tr!(literal = "Slope angle of each batter face, measured from horizontal."))
                .width(CONTROL_WIDTH)
                .speed(0.5)
                .max_decimals(1)
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Bench height"), &mut editor.batter_berm_bench_height, 0.1..=500.0)
                .help_text(tr!(literal = "Vertical rise or fall of each bench before the next berm is created."))
                .width(CONTROL_WIDTH)
                .speed(0.1)
                .max_decimals(2)
                .suffix(tr!(literal = "m"))
                .show(ui);
            let max_benches = editor.batter_berm_max_benches;
            if max_benches == 0 {
                editor.batter_berm_benches = 0;
            } else {
                editor.batter_berm_benches = editor.batter_berm_benches.clamp(1, max_benches);
            }
            ui.add_enabled_ui(max_benches > 0, |ui| {
                MenuFieldU32::new(
                    tr!(literal = "Benches"),
                    &mut editor.batter_berm_benches,
                    if max_benches == 0 { 0..=0 } else { 1..=max_benches },
                )
                .help_text(tr!(
                    literal = "Number of complete batter-and-berm levels. The maximum is limited to the deepest level that preserves the specified geometry."
                ))
                .width(CONTROL_WIDTH)
                .speed(0.1)
                .show(ui);
            });

            MenuField::new(tr!(literal = "Type"))
                .help_text(tr!(
                    literal = "Type and Direction together set the offset side. Pit + Up and Stockpile + Down step outward; Pit + Down and Stockpile + Up step inward."
                ))
                .show(ui, |ui, row_height, _| {
                    let gap = ui.spacing().item_spacing.x;
                    let button_width = (CONTROL_WIDTH - gap) * 0.5;
                    ui.allocate_ui_with_layout(egui::vec2(CONTROL_WIDTH, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                MenuButton::new(tr!(literal = "Pit"))
                                    .selected(editor.batter_berm_mode == BatterBermMode::Pit)
                                    .min_width(button_width),
                            )
                            .clicked()
                        {
                            editor.batter_berm_mode = BatterBermMode::Pit;
                        }
                        if ui
                            .add(
                                MenuButton::new(tr!(literal = "Stockpile"))
                                    .selected(editor.batter_berm_mode == BatterBermMode::Stockpile)
                                    .min_width(button_width),
                            )
                            .clicked()
                        {
                            editor.batter_berm_mode = BatterBermMode::Stockpile;
                        }
                    })
                    .response
                });

            MenuField::new(tr!(literal = "Direction"))
                .help_text(tr!(
                    literal = "Up raises each bench by the bench height; Down lowers it. This also flips the offset side - see Type."
                ))
                .show(ui, |ui, row_height, _| {
                    let gap = ui.spacing().item_spacing.x;
                    let button_width = (CONTROL_WIDTH - gap) * 0.5;
                    ui.allocate_ui_with_layout(egui::vec2(CONTROL_WIDTH, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if ui
                            .add(MenuButton::new(tr!(literal = "Up")).selected(editor.batter_berm_direction_up).min_width(button_width))
                            .clicked()
                        {
                            editor.batter_berm_direction_up = true;
                        }
                        if ui
                            .add(MenuButton::new(tr!(literal = "Down")).selected(!editor.batter_berm_direction_up).min_width(button_width))
                            .clicked()
                        {
                            editor.batter_berm_direction_up = false;
                        }
                    })
                    .response
                });

            ui.add_space(8.0);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(MenuButton::new(tr!(literal = "Apply")).primary().enabled(!editor.batter_berm_rings_world.is_empty()))
                    .clicked()
                {
                    commands.push(UiCommand::CommitBatterBerm);
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() {
                    commands.push(UiCommand::CancelBatterBerm);
                }
            });
        });
}

/// Draw the Relimit Line dialog (intersect, absolute length, or relative length modes).
pub(crate) fn draw_relimit_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, viewport_rect: egui::Rect) {
    ViewportDockPanel::new("relimit_line_panel", tr!(literal = "Relimit Line"), viewport_rect)
        // The three mode labels need enough room to stay visually separate
        // from the field's info marker.
        .min_width(335.0)
        .max_width(335.0)
        .show(ui.ctx(), |ui| {
            // Mode tabs
            let previous_mode = editor.relimit_mode;
            MenuField::new(tr!(literal = "Mode"))
                .help_text(tr!(
                    literal = "Intersect moves one endpoint to another line. Absolute sets the final line length. Relative adds or subtracts length."
                ))
                .show(ui, |ui, _, _| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(
                            &mut editor.relimit_mode,
                            RelimitMode::Intersect,
                            tr!(literal = "Intersect"),
                        );
                        ui.selectable_value(
                            &mut editor.relimit_mode,
                            RelimitMode::AbsoluteLength,
                            tr!(literal = "Absolute length"),
                        );
                        ui.selectable_value(
                            &mut editor.relimit_mode,
                            RelimitMode::RelativeLength,
                            tr!(literal = "Relative (+/-)"),
                        );
                    })
                    .response
                });
            if editor.relimit_mode == RelimitMode::Intersect
                && previous_mode != RelimitMode::Intersect
            {
                editor.relimit_waiting_for_pick = true;
                editor.relimit_confirming_end = false;
            }

            ui.add_space(4.0);

            match editor.relimit_mode {
                RelimitMode::Intersect => {
                    if editor.relimit_waiting_for_pick {
                        ui.label(tr!(literal = "Click the line to intersect with…"));
                    } else if editor.relimit_confirming_end {
                        ui.label(tr!(literal = "Hover to choose which end to move, then click to confirm."));
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 180, 0),
                            match editor.relimit_hover_end {
                                TrimEnd::Start => tr!(literal = "Moving: Start endpoint"),
                                TrimEnd::End => tr!(literal = "Moving: End endpoint"),
                            },
                        );
                    }
                }
                RelimitMode::AbsoluteLength | RelimitMode::RelativeLength => {
                    let label = if matches!(editor.relimit_mode, RelimitMode::AbsoluteLength) {
                        tr!(literal = "New length (m)")
                    } else {
                        tr!(literal = "Delta length (m, use + or -)")
                    };
                    MenuFieldF64::new(label, &mut editor.relimit_value_input, f64::MIN..=f64::MAX)
                        .help_text(tr!(
                            literal = "The selected start or end point moves along the line direction; the opposite endpoint stays fixed."
                        ))
                        .speed(0.1)
                        .suffix(tr!(literal = "m"))
                        .show(ui);
                    MenuField::new(tr!(literal = "Move which end"))
                        .help_text(tr!(literal = "Select the endpoint that changes; the other endpoint remains fixed."))
                        .show(ui, |ui, _, _| {
                            ui.horizontal(|ui| {
                                ui.selectable_value(
                                    &mut editor.relimit_resize_end,
                                    TrimEnd::Start,
                                    tr!(literal = "Start"),
                                );
                                ui.selectable_value(
                                    &mut editor.relimit_resize_end,
                                    TrimEnd::End,
                                    tr!(literal = "End"),
                                );
                            })
                            .response
                        });
                }
            }

            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match editor.relimit_mode {
                    RelimitMode::Intersect => {
                        if ui.add(MenuButton::new(tr!(literal = "Apply and Pick Target")).primary()).clicked() {
                            editor.relimit_dialog_open = false;
                            editor.relimit_waiting_for_pick = true;
                        }
                    }
                    RelimitMode::AbsoluteLength | RelimitMode::RelativeLength => {
                        if ui.add(MenuButton::new(tr!(literal = "Apply")).primary()).clicked()
                            && editor.relimit_value_input.is_finite()
                            && let Some(source_id) = editor.relimit_source_id
                        {
                            commands.push(UiCommand::RelimitLineResize {
                                source_id,
                                mode: editor.relimit_mode,
                                value: editor.relimit_value_input,
                            });
                            editor.relimit_dialog_open = false;
                        }
                    }
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() {
                    commands.push(UiCommand::CancelRelimit);
                }
            });
        });
}

/// Draw the floating Move tool panel with dX/dY/dZ inputs and an Apply button.
pub(crate) fn draw_move_panel(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, viewport_rect: egui::Rect) {
    // One panel for both translate tools, titled by the one running it.
    let title = if editor.active_tool == ActiveTool::MoveCollar {
        tr!(literal = "Move Collar")
    } else {
        tr!(literal = "Move Design")
    };
    ViewportDockPanel::new("move_panel", title, viewport_rect)
        .min_width(210.0)
        .show(ui.ctx(), |ui| {
            let dx_resp = MenuFieldF64::new(tr!(literal = "dX"), &mut editor.move_panel_delta[0], f64::MIN..=f64::MAX)
                .help_text(tr!(literal = "Translation distance along the world X axis."))
                .speed(0.1)
                .show(ui);
            let dy_resp = MenuFieldF64::new(tr!(literal = "dY"), &mut editor.move_panel_delta[1], f64::MIN..=f64::MAX)
                .help_text(tr!(literal = "Translation distance along the world Y axis."))
                .speed(0.1)
                .show(ui);
            let dz_resp = MenuFieldF64::new(tr!(literal = "dZ"), &mut editor.move_panel_delta[2], f64::MIN..=f64::MAX)
                .help_text(tr!(literal = "Translation distance along the world Z axis."))
                .speed(0.1)
                .show(ui);
            if dx_resp.changed() || dy_resp.changed() || dz_resp.changed() {
                commands.push(UiCommand::PreviewMoveDelta(glam::DVec3::new(
                    editor.move_panel_delta[0],
                    editor.move_panel_delta[1],
                    editor.move_panel_delta[2],
                )));
            }

            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(MenuButton::new(tr!(literal = "Apply")).primary()).clicked() {
                    commands.push(UiCommand::ApplyMoveDelta(glam::DVec3::new(
                        editor.move_panel_delta[0],
                        editor.move_panel_delta[1],
                        editor.move_panel_delta[2],
                    )));
                    editor.active_tool = ActiveTool::None;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() {
                    commands.push(UiCommand::CancelMoveDelta);
                    editor.active_tool = ActiveTool::None;
                }
            });
        });
}

/// Chamfer tool viewport dock: segments input + Apply / Cancel.
pub(crate) fn draw_chamfer_panel(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, viewport_rect: egui::Rect) {
    let corner_picked = editor.chamfer_poly_id.is_some() && editor.chamfer_corner_index.is_some();

    ViewportDockPanel::new("chamfer_panel", tr!(literal = "Chamfer"), viewport_rect)
        .min_width(200.0)
        .show(ui.ctx(), |ui| {
            if !corner_picked {
                ui.label(tr!(literal = "Click a corner on a closed polyline."));
            } else {
                MenuFieldU32::new(tr!(literal = "Segments"), &mut editor.chamfer_segments, 1..=64)
                    .help_text(tr!(
                        literal = "Number of straight segments used to approximate the rounded corner. Use 1 for a straight chamfer."
                    ))
                    .speed(0.1)
                    .show(ui);

                let mut r = editor.chamfer_radius;
                let max_r = if editor.chamfer_max_radius.is_finite() { editor.chamfer_max_radius } else { f64::MAX };
                MenuFieldF64::new(tr!(literal = "Radius"), &mut r, 0.0..=max_r)
                    .help_text(tr!(literal = "Corner radius, limited so the replacement cannot pass adjacent vertices."))
                    .speed(0.05)
                    .show(ui);
                editor.chamfer_radius = r.clamp(0.0, max_r);
            }

            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Grey out when the displayed value is "0.00" (2 dp) - matches user perception.
                let can_apply = corner_picked && (editor.chamfer_radius * 100.0).round() > 0.0;
                if ui.add(MenuButton::new(tr!(literal = "Apply")).primary().enabled(can_apply)).clicked() && can_apply {
                    commands.push(UiCommand::ApplyChamfer);
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() {
                    commands.push(UiCommand::CancelChamfer);
                }
            });
        });
}

fn draw_bezier_xyz(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>, help_text: impl Into<egui::WidgetText>, point: &mut [f64; 3]) {
    const TOTAL_WIDTH: f32 = 270.0;
    MenuField::new(label).help_text(help_text).show(ui, |ui, row_height, _| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let value_width = (TOTAL_WIDTH - ui.spacing().item_spacing.x * 2.0) / 3.0;
        ui.horizontal(|ui| {
            for (axis, value) in ["X ", "Y ", "Z "].into_iter().zip(point.iter_mut()) {
                ui.add_sized([value_width, row_height], egui::DragValue::new(value).speed(0.1).max_decimals(3).prefix(axis));
            }
        })
        .response
    });
}

/// Bezier curve editor: vertex selection status, control point inputs, and Apply/Cancel.
pub(crate) fn draw_bezier_panel(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, viewport_rect: egui::Rect) {
    let both_selected = editor.bezier_selected_verts[0].is_some() && editor.bezier_selected_verts[1].is_some();

    ViewportDockPanel::new("bezier_panel", tr!(literal = "Bezier Curve"), viewport_rect)
        .min_width(400.0)
        .show(ui.ctx(), |ui| {
            if editor.bezier_poly_id.is_none() {
                ui.label(tr!(literal = "Click an open or closed polyline to begin."));
            } else if !both_selected {
                match editor.bezier_selected_verts[0] {
                    None => {
                        ui.label(tr!(literal = "Click a vertex to start the replacement span."));
                    }
                    Some(_) => {
                        ui.label(tr!(literal = "Click the second vertex of the replacement span."));
                    }
                }
            } else {
                MenuFieldU32::new(tr!(literal = "Segments"), &mut editor.bezier_segments, 2..=64)
                    .help_text(tr!(literal = "Number of line segments used to approximate the curve between the two selected vertices."))
                    .speed(0.1)
                    .show(ui);

                if editor.bezier_poly_closed {
                    let previous = editor.bezier_replace_longer;
                    MenuField::new(tr!(literal = "Replace path"))
                        .help_text(tr!(
                            literal = "Choose which of the two polyline paths between the selected vertices will be replaced. Length includes elevation and curved edges."
                        ))
                        .show(ui, |ui, row_height, _| {
                            ui.horizontal(|ui| {
                                if ui
                                    .add_sized([82.0, row_height], egui::Button::selectable(!editor.bezier_replace_longer, tr!(literal = "Shortest")))
                                    .clicked()
                                {
                                    editor.bezier_replace_longer = false;
                                }
                                if ui
                                    .add_sized([82.0, row_height], egui::Button::selectable(editor.bezier_replace_longer, tr!(literal = "Longest")))
                                    .clicked()
                                {
                                    editor.bezier_replace_longer = true;
                                }
                            })
                            .response
                        });
                    if editor.bezier_replace_longer != previous {
                        editor.bezier_selected_verts.swap(0, 1);
                        std::mem::swap(&mut editor.bezier_cp1, &mut editor.bezier_cp2);
                    }
                }

                draw_bezier_xyz(
                    ui,
                    tr!(literal = "Control point 1"),
                    tr!(literal = "World X, Y and Z coordinates of the first Bezier control point."),
                    &mut editor.bezier_cp1,
                );
                draw_bezier_xyz(
                    ui,
                    tr!(literal = "Control point 2"),
                    tr!(literal = "World X, Y and Z coordinates of the second Bezier control point."),
                    &mut editor.bezier_cp2,
                );
            }

            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_apply = both_selected;
                let apply_clicked = ui.add(MenuButton::new(tr!(literal = "Apply")).primary().enabled(can_apply)).clicked();
                let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));
                if can_apply && (apply_clicked || enter_pressed) {
                    commands.push(UiCommand::ApplyBezier);
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() {
                    commands.push(UiCommand::CancelBezier);
                }
            });
        });
}

/// Slice view dock: slab width, movement speed, Q/E rotate rate, and Exit.
pub(crate) fn draw_slice_panel(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, viewport_rect: egui::Rect) {
    ViewportDockPanel::new("slice_panel", tr!(literal = "Slice View"), viewport_rect)
        .min_width(210.0)
        .show(ui.ctx(), |ui| {
            MenuFieldF64::new(tr!(literal = "Width"), &mut editor.slice_width_input, 0.1..=1.0e6)
                .help_text(tr!(literal = "Thickness of the visible slice slab centred on the overview indicator."))
                .speed(0.5)
                .suffix(tr!(literal = "m"))
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Speed"), &mut editor.slice_speed_input, 0.0..=1.0e6)
                .help_text(tr!(literal = "Movement speed of the slice when using the navigation keys."))
                .speed(1.0)
                .suffix(tr!(literal = "m/s"))
                .show(ui);
            MenuFieldF64::new(tr!(literal = "Rotate"), &mut editor.slice_rotate_input, 1.0..=720.0)
                .help_text(tr!(literal = "Rotation speed of the slice when using Q and E."))
                .speed(1.0)
                .suffix(tr!(literal = "°/s"))
                .show(ui);
            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(MenuButton::new(tr!(literal = "Exit slice"))).clicked() {
                    commands.push(UiCommand::SetSliceModeEnabled(false));
                }
            });
        });
}
