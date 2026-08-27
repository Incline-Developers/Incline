//! Destructive-action and unsaved-work confirmation dialogs.

use crate::ui::{
    state::{EditorState, UiCommand, UiProjectView},
    widgets::menu::{self, DragableMenu, MenuButton},
};

/// Draw the "Save before quit?" confirmation dialog.
pub(crate) fn draw_exit_confirm_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, _editor: &mut EditorState) {
    let mut open = true;
    let title = "Exit: Unsaved Changes";
    DragableMenu::new(title).open(&mut open).min_width(360.0).show(ui.ctx(), |ui| {
        ui.set_width(340.);
        #[cfg(not(target_arch = "wasm32"))]
        ui.label("Save the modified project before exiting?");
        #[cfg(target_arch = "wasm32")]
        ui.label("Save the modified project to browser storage before exiting?");
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new("Save and Exit").primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::SaveAndExit);
            }
            // Red, and mouse-only: Enter must never be the key that throws
            // away unsaved changes.
            if ui.add(MenuButton::new("Exit Without Saving").danger()).clicked() {
                commands.push(UiCommand::ExitWithoutSaving);
            }
            if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                commands.push(UiCommand::CancelExit);
            }
        });
    });
    if !open {
        commands.push(UiCommand::CancelExit);
    }
}

/// Draw the confirmation required before New/Open replaces the one active
/// project. The pending action itself stays in the application core.
pub(crate) fn draw_replace_project_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    if !editor.replace_project_confirm_open {
        return;
    }
    let mut open = true;
    DragableMenu::new("Replace Project: Unsaved Changes").open(&mut open).min_width(340.0).show(ui.ctx(), |ui| {
        ui.set_max_width(340.0);
        ui.label("Save changes to the current project before replacing it?");
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new("Save").primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::SaveAndReplaceProject);
            }
            if ui.add(MenuButton::new("Discard").danger()).clicked() {
                commands.push(UiCommand::DiscardAndReplaceProject);
            }
            if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                commands.push(UiCommand::CancelProjectReplacement);
            }
        });
    });
    if !open {
        commands.push(UiCommand::CancelProjectReplacement);
    }
}

pub(crate) fn draw_lossy_save_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, project: &UiProjectView) {
    if !editor.lossy_save_confirm_open {
        return;
    }
    let warnings = project.projects.first().map(|entry| entry.lossy_save_warnings.as_slice()).unwrap_or_default();
    let mut open = true;
    DragableMenu::new("Confirm OMF Rewrite").open(&mut open).min_width(420.0).show(ui.ctx(), |ui| {
        ui.set_max_width(520.0);
        ui.label("Incline cannot reproduce all content from the original OMF. Saving will omit the following content:");
        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
            for warning in warnings {
                ui.label(format!("• {warning}"));
            }
        });
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new("Save Anyway").primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::ConfirmLossyProjectSave);
            }
            if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                commands.push(UiCommand::CancelLossyProjectSave);
            }
        });
    });
    if !open {
        commands.push(UiCommand::CancelLossyProjectSave);
    }
}

/// Draw the delete-selection confirmation dialog shown when Delete/Backspace is pressed.
pub(crate) fn draw_delete_confirm_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    if !editor.delete_confirm_open {
        return;
    }
    let count = editor.selected_handles.iter().filter(|h| matches!(h, crate::model::SceneEntityId::Object(_))).count();
    let mut open = true;
    DragableMenu::new("Delete Objects").open(&mut open).min_width(240.0).show(ui.ctx(), |ui| {
        ui.set_max_width(240.);
        ui.label(format!("Are you sure you want to delete {count} selected item{}?", if count == 1 { "" } else { "s" }));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new("Delete").danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::ConfirmDeleteSelection);
                editor.delete_confirm_open = false;
            }
            if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                editor.delete_confirm_open = false;
            }
        });
    });
    if !open {
        editor.delete_confirm_open = false;
    }
}

/// Draw the confirmation dialog shown before deleting a layer and its objects.
pub(crate) fn draw_delete_layer_confirm_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    let Some((layer_id, name)) = editor.pending_delete_layer.clone() else {
        return;
    };
    let mut open = true;
    DragableMenu::new("Delete Layer").open(&mut open).min_width(280.0).show(ui.ctx(), |ui| {
        ui.label(format!("Delete layer '{name}' and all objects on it?\nThis cannot be undone."));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new("Delete Layer").danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::DeleteLayer(layer_id));
            }
            if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                editor.pending_delete_layer = None;
            }
        });
    });
    if !open {
        editor.pending_delete_layer = None;
    }
}

/// Draw the confirmation dialog shown before deleting a non-layer explorer
/// item (triangulation, raster, point cloud, block model, or drill hole dataset).
pub(crate) fn draw_delete_item_confirm_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    let Some((target, name)) = editor.pending_delete_item.clone() else {
        return;
    };
    let kind = target.kind_label();
    let mut open = true;
    DragableMenu::new(format!("Delete {kind}")).open(&mut open).min_width(280.0).show(ui.ctx(), |ui| {
        ui.label(format!("Delete '{name}' from the project?\nThis cannot be undone."));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new(format!("Delete {kind}")).danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(target.remove_command());
                editor.pending_delete_item = None;
            }
            if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                editor.pending_delete_item = None;
            }
        });
    });
    if !open {
        editor.pending_delete_item = None;
    }
}

/// Draw the confirmation dialog shown before closing a dirty project.
pub(crate) fn draw_close_project_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, project: &UiProjectView) {
    let Some(runtime_id) = editor.pending_close_project else {
        return;
    };
    let name = project
        .projects
        .iter()
        .find(|entry| entry.runtime_id == runtime_id)
        .map(|entry| entry.name.as_str())
        .unwrap_or("this project");
    let mut open = true;
    let removing = editor.remove_project_after_close;
    #[cfg(not(target_arch = "wasm32"))]
    let title = if removing {
        "Remove Project: Unsaved Changes"
    } else {
        "Close Project: Unsaved Changes"
    };
    #[cfg(target_arch = "wasm32")]
    let title = if removing {
        "Remove Project: Unsaved Changes"
    } else {
        "Close Project: Unsaved Changes"
    };
    DragableMenu::new(title).open(&mut open).min_width(320.0).show(ui.ctx(), |ui| {
        ui.set_max_width(320.);
        #[cfg(not(target_arch = "wasm32"))]
        {
            ui.label(if removing {
                format!("Save changes to '{name}' before removing it from Incline?")
            } else {
                format!("Save changes to '{name}' before closing it?")
            });
            menu::menu_actions(ui, |ui| {
                if ui.add(MenuButton::new(if removing { "Save and Remove" } else { "Save and Close" }).primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                    commands.push(UiCommand::SaveAndCloseProject(runtime_id));
                }
                if ui
                    .add(MenuButton::new(if removing { "Remove Without Saving" } else { "Close Without Saving" }).danger())
                    .clicked()
                {
                    commands.push(UiCommand::CloseProjectForce(runtime_id));
                }
                if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    commands.push(UiCommand::CancelCloseProject);
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            ui.label(if removing {
                format!("Remove '{name}' and delete its browser-stored copy? Unsaved changes will be lost.")
            } else {
                format!("Save changes to '{name}' before closing it?")
            });
            menu::menu_actions(ui, |ui| {
                if removing {
                    // Removing always discards, so it is deliberately not bound to Enter.
                    if ui.add(MenuButton::new("Remove Project").danger()).clicked() {
                        commands.push(UiCommand::CloseProjectForce(runtime_id));
                    }
                } else {
                    if ui.add(MenuButton::new("Save and Close").primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                        commands.push(UiCommand::SaveAndCloseProject(runtime_id));
                    }
                    if ui.add(MenuButton::new("Close Without Saving").danger()).clicked() {
                        commands.push(UiCommand::CloseProjectForce(runtime_id));
                    }
                }
                if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    commands.push(UiCommand::CancelCloseProject);
                }
            });
        }
    });
    if !open {
        commands.push(UiCommand::CancelCloseProject);
    }
}

/// Draw the confirmation dialog shown before discarding a dirty project's
/// changes (reverting to the last saved version on disk).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn draw_discard_project_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, project: &UiProjectView) {
    let Some(runtime_id) = editor.pending_discard_project else {
        return;
    };
    let name = project
        .projects
        .iter()
        .find(|entry| entry.runtime_id == runtime_id)
        .map(|entry| entry.name.as_str())
        .unwrap_or("this project");
    let mut open = true;
    DragableMenu::new("Discard Changes").open(&mut open).min_width(320.0).show(ui.ctx(), |ui| {
        ui.set_max_width(320.);
        ui.label(format!(
            "Discard all unsaved changes to '{name}'?\n\
                 The last saved version is reloaded from disk. This cannot be undone."
        ));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new("Discard Changes").danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::DiscardProjectChanges(runtime_id));
            }
            if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                editor.pending_discard_project = None;
            }
        });
    });
    if !open {
        editor.pending_discard_project = None;
    }
}

/// Confirm restoring just one dirty layer from its saved project while retaining
/// unsaved work on the other layers.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn draw_discard_layer_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    let Some((layer_id, name)) = editor.pending_discard_layer.clone() else {
        return;
    };
    let mut open = true;
    DragableMenu::new("Discard Layer Changes").open(&mut open).min_width(320.0).show(ui.ctx(), |ui| {
        ui.set_max_width(320.0);
        ui.label(format!(
            "Discard all unsaved changes to layer '{name}'?\n\
                 The saved layer is reloaded from disk while changes to other layers are kept. \
                 This cannot be undone."
        ));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new("Discard Changes").danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::DiscardLayerChanges(layer_id));
            }
            if ui.add(MenuButton::new("Cancel")).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                editor.pending_discard_layer = None;
            }
        });
    });
    if !open {
        editor.pending_discard_layer = None;
    }
}
