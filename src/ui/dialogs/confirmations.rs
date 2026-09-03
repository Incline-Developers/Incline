//! Destructive-action and unsaved-work confirmation dialogs.

use crate::{
    i18n::{tr, tr_format},
    ui::{
        state::{EditorState, UiCommand, UiProjectView},
        widgets::menu::{self, DragableMenu, MenuButton},
    },
};

/// Draw the "Save before quit?" confirmation dialog.
pub(crate) fn draw_exit_confirm_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, _editor: &mut EditorState) {
    let mut open = true;
    let title = tr!(literal = "Exit: Unsaved Changes");
    DragableMenu::new(title).open(&mut open).min_width(360.0).show(ui.ctx(), |ui| {
        ui.set_width(340.);
        #[cfg(not(target_arch = "wasm32"))]
        ui.label(tr!(literal = "Save the modified project before exiting?"));
        #[cfg(target_arch = "wasm32")]
        ui.label(tr!(literal = "Save the modified project to browser storage before exiting?"));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new(tr!(literal = "Save and Exit")).primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::SaveAndExit);
            }
            // Red, and mouse-only: Enter must never be the key that throws
            // away unsaved changes.
            if ui.add(MenuButton::new(tr!(literal = "Exit Without Saving")).danger()).clicked() {
                commands.push(UiCommand::ExitWithoutSaving);
            }
            if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new(tr!(literal = "Replace Project: Unsaved Changes"))
        .open(&mut open)
        .min_width(340.0)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(340.0);
            ui.label(tr!(literal = "Save changes to the current project before replacing it?"));
            menu::menu_actions(ui, |ui| {
                if ui.add(MenuButton::new(tr!(literal = "Save")).primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                    commands.push(UiCommand::SaveAndReplaceProject);
                }
                if ui.add(MenuButton::new(tr!(literal = "Discard")).danger()).clicked() {
                    commands.push(UiCommand::DiscardAndReplaceProject);
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new(tr!(literal = "Confirm OMF Rewrite"))
        .open(&mut open)
        .min_width(420.0)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(520.0);
            ui.label(tr!(
                literal = "Incline Design cannot reproduce all content from the original OMF. Saving will omit the following content:"
            ));
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for warning in warnings {
                    ui.label(format!("• {warning}"));
                }
            });
            menu::menu_actions(ui, |ui| {
                if ui.add(MenuButton::new(tr!(literal = "Save Anyway")).primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                    commands.push(UiCommand::ConfirmLossyProjectSave);
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new(tr!(literal = "Delete Objects")).open(&mut open).min_width(240.0).show(ui.ctx(), |ui| {
        ui.set_max_width(240.);
        ui.label(tr!("confirm-delete-count", count = count));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new(tr!(literal = "Delete")).danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::ConfirmDeleteSelection);
                editor.delete_confirm_open = false;
            }
            if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new(tr!(literal = "Delete Layer")).open(&mut open).min_width(280.0).show(ui.ctx(), |ui| {
        ui.label(tr!("confirm-delete-layer", name = name.clone()));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new(tr!(literal = "Delete Layer")).danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::DeleteLayer(layer_id));
            }
            if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    // One Fluent message carries the whole "Delete <kind>" phrase so each
    // language owns the word order; the confirm button repeats that title.
    let title = tr!("dialog-delete-title", kind = target.kind_label());
    let mut open = true;
    DragableMenu::new(title.clone()).open(&mut open).min_width(280.0).show(ui.ctx(), |ui| {
        ui.label(tr!("dialog-delete-confirm", name = name.clone()));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new(title.clone()).danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(target.remove_command());
                editor.pending_delete_item = None;
            }
            if ui.add(MenuButton::new(tr!("common-cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
        .map(|entry| entry.name.clone())
        .unwrap_or_else(|| tr!(literal = "this project"));
    let mut open = true;
    let removing = editor.remove_project_after_close;
    let title = if removing {
        tr!(literal = "Remove Project: Unsaved Changes")
    } else {
        tr!(literal = "Close Project: Unsaved Changes")
    };
    DragableMenu::new(title).open(&mut open).min_width(320.0).show(ui.ctx(), |ui| {
        ui.set_max_width(320.);
        #[cfg(not(target_arch = "wasm32"))]
        {
            ui.label(if removing {
                tr_format!(literal = "Save changes to '%name%' before removing it from Incline Design?", name = name)
            } else {
                tr_format!(literal = "Save changes to '%name%' before closing it?", name = name)
            });
            menu::menu_actions(ui, |ui| {
                if ui
                    .add(MenuButton::new(if removing { tr!(literal = "Save and Remove") } else { tr!(literal = "Save and Close") }).primary())
                    .clicked()
                    || menu::dialog_confirm_pressed(ui.ctx())
                {
                    commands.push(UiCommand::SaveAndCloseProject(runtime_id));
                }
                if ui
                    .add(
                        MenuButton::new(if removing {
                            tr!(literal = "Remove Without Saving")
                        } else {
                            tr!(literal = "Close Without Saving")
                        })
                        .danger(),
                    )
                    .clicked()
                {
                    commands.push(UiCommand::CloseProjectForce(runtime_id));
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    commands.push(UiCommand::CancelCloseProject);
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            ui.label(if removing {
                tr_format!(literal = "Remove '%name%' and delete its browser-stored copy? Unsaved changes will be lost.", name = name)
            } else {
                tr_format!(literal = "Save changes to '%name%' before closing it?", name = name)
            });
            menu::menu_actions(ui, |ui| {
                if removing {
                    // Removing always discards, so it is deliberately not bound to Enter.
                    if ui.add(MenuButton::new(tr!(literal = "Remove Project")).danger()).clicked() {
                        commands.push(UiCommand::CloseProjectForce(runtime_id));
                    }
                } else {
                    if ui.add(MenuButton::new(tr!(literal = "Save and Close")).primary()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                        commands.push(UiCommand::SaveAndCloseProject(runtime_id));
                    }
                    if ui.add(MenuButton::new(tr!(literal = "Close Without Saving")).danger()).clicked() {
                        commands.push(UiCommand::CloseProjectForce(runtime_id));
                    }
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
        .map(|entry| entry.name.clone())
        .unwrap_or_else(|| tr!(literal = "this project"));
    let mut open = true;
    DragableMenu::new(tr!(literal = "Discard Changes")).open(&mut open).min_width(320.0).show(ui.ctx(), |ui| {
        ui.set_max_width(320.);
        ui.label(tr_format!(
            literal = "Discard all unsaved changes to '%name%'?\n\
                 The last saved version is reloaded from disk. This cannot be undone.",
            name = name
        ));
        menu::menu_actions(ui, |ui| {
            if ui.add(MenuButton::new(tr!(literal = "Discard Changes")).danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                commands.push(UiCommand::DiscardProjectChanges(runtime_id));
            }
            if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
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
    DragableMenu::new(tr!(literal = "Discard Layer Changes"))
        .open(&mut open)
        .min_width(320.0)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(320.0);
            ui.label(tr_format!(
                literal = "Discard all unsaved changes to layer '%name%'?\n\
                 The saved layer is reloaded from disk while changes to other layers are kept. \
                 This cannot be undone.",
                name = name
            ));
            menu::menu_actions(ui, |ui| {
                if ui.add(MenuButton::new(tr!(literal = "Discard Changes")).danger()).clicked() || menu::dialog_confirm_pressed(ui.ctx()) {
                    commands.push(UiCommand::DiscardLayerChanges(layer_id));
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    editor.pending_discard_layer = None;
                }
            });
        });
    if !open {
        editor.pending_discard_layer = None;
    }
}
