//! Destructive-action and unsaved-work confirmation dialogs.

use crate::ui::{
    state::{EditorState, UiCommand, UiProjectView},
    widgets::menu::DragableMenu,
};

/// Draw the "Save before quit?" confirmation dialog.
pub(crate) fn draw_exit_confirm_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, _editor: &mut EditorState) {
    let mut open = true;
    #[cfg(not(target_arch = "wasm32"))]
    let title = "Exit: Unsaved Changes";
    #[cfg(target_arch = "wasm32")]
    let title = "Quit Incline?";
    DragableMenu::new(title).open(&mut open).show(ui.ctx(), |ui| {
        ui.set_width(280.);
        #[cfg(not(target_arch = "wasm32"))]
        {
            ui.label("Save all modified PIDBs, unsaved triangulations, and unsaved block models before exiting?");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Save and Exit").clicked() {
                    commands.push(UiCommand::SaveAndExit);
                }
                if ui.button("Exit Without Saving").clicked() {
                    commands.push(UiCommand::ExitWithoutSaving);
                }
                if ui.button("Cancel").clicked() {
                    commands.push(UiCommand::CancelExit);
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            ui.label(
                "Are you sure you want to quit? Browser PIDBs do not persist as files. \
                     Download them as .pidb files before leaving.",
            );
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Download All PIDBs").clicked() {
                    commands.push(UiCommand::DownloadAllPidbs);
                }
                if ui.button("Quit Without Downloading").clicked() {
                    commands.push(UiCommand::ExitWithoutSaving);
                }
                if ui.button("Cancel").clicked() {
                    commands.push(UiCommand::CancelExit);
                }
            });
        }
    });
    if !open {
        commands.push(UiCommand::CancelExit);
    }
}

/// Draw the delete-selection confirmation dialog shown when Delete/Backspace is pressed.
pub(crate) fn draw_delete_confirm_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    if !editor.delete_confirm_open {
        return;
    }
    let count = editor.selected_handles.iter().filter(|h| matches!(h, crate::model::SceneEntityId::Object(_))).count();
    let mut open = true;
    DragableMenu::new("Delete Objects").open(&mut open).min_width(160.0).show(ui.ctx(), |ui| {
        ui.set_max_width(160.);
        ui.label(format!("Are you sure you want to delete {count} selected item{}?", if count == 1 { "" } else { "s" }));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Delete").clicked() {
                commands.push(UiCommand::ConfirmDeleteSelection);
                editor.delete_confirm_open = false;
            }
            if ui.button("Cancel").clicked() {
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
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Delete Layer").clicked() {
                commands.push(UiCommand::DeleteLayer(layer_id));
            }
            if ui.button("Cancel").clicked() {
                editor.pending_delete_layer = None;
            }
        });
    });
    if !open {
        editor.pending_delete_layer = None;
    }
}

/// Draw the confirmation dialog shown before closing a dirty PIDB.
pub(crate) fn draw_close_pidb_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, project: &UiProjectView) {
    let Some(runtime_id) = editor.pending_close_pidb else {
        return;
    };
    let name = project
        .projects
        .iter()
        .find(|entry| entry.runtime_id == runtime_id)
        .map(|entry| entry.name.as_str())
        .unwrap_or("this PIDB");
    let mut open = true;
    #[cfg(not(target_arch = "wasm32"))]
    let title = "Close PIDB: Unsaved Changes";
    #[cfg(target_arch = "wasm32")]
    let title = "Delete Project";
    DragableMenu::new(title).open(&mut open).min_width(320.0).show(ui.ctx(), |ui| {
        ui.set_max_width(320.);
        #[cfg(not(target_arch = "wasm32"))]
        {
            ui.label(format!("Save changes to '{name}' before closing it?"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save and Close").clicked() {
                    commands.push(UiCommand::SaveAndClosePidb(runtime_id));
                }
                if ui.button("Close Without Saving").clicked() {
                    commands.push(UiCommand::ClosePidbForce(runtime_id));
                }
                if ui.button("Cancel").clicked() {
                    editor.pending_close_pidb = None;
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            ui.label(format!(
                "Are you sure you want to delete '{name}'?\nThis closes the project and removes any saved browser copy. It cannot be undone."
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Delete Project").clicked() {
                    commands.push(UiCommand::ClosePidbForce(runtime_id));
                }
                if ui.button("Cancel").clicked() {
                    editor.pending_close_pidb = None;
                }
            });
        }
    });
    if !open {
        editor.pending_close_pidb = None;
    }
}

/// Draw the confirmation dialog shown before discarding a dirty PIDB's
/// changes (reverting to the last saved version on disk).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn draw_discard_pidb_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState, project: &UiProjectView) {
    let Some(runtime_id) = editor.pending_discard_pidb else {
        return;
    };
    let name = project
        .projects
        .iter()
        .find(|entry| entry.runtime_id == runtime_id)
        .map(|entry| entry.name.as_str())
        .unwrap_or("this PIDB");
    let mut open = true;
    DragableMenu::new("Discard Changes").open(&mut open).min_width(320.0).show(ui.ctx(), |ui| {
        ui.set_max_width(320.);
        ui.label(format!(
            "Discard all unsaved changes to '{name}'?\n\
                 The last saved version is reloaded from disk. This cannot be undone."
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Discard Changes").clicked() {
                commands.push(UiCommand::DiscardPidbChanges(runtime_id));
            }
            if ui.button("Cancel").clicked() {
                editor.pending_discard_pidb = None;
            }
        });
    });
    if !open {
        editor.pending_discard_pidb = None;
    }
}

/// Confirm restoring just one dirty layer from its saved PIDB while retaining
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
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Discard Changes").clicked() {
                commands.push(UiCommand::DiscardLayerChanges(layer_id));
            }
            if ui.button("Cancel").clicked() {
                editor.pending_discard_layer = None;
            }
        });
    });
    if !open {
        editor.pending_discard_layer = None;
    }
}

pub(crate) fn draw_confirm_load_all_folder_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    let folder_path = match &editor.confirm_load_all_folder {
        Some(p) => p.clone(),
        None => return,
    };
    let folder_name = folder_path.file_name().and_then(|n| n.to_str()).unwrap_or("folder").to_owned();
    let mut open = true;
    DragableMenu::new("Load All Triangulations").open(&mut open).min_width(270.0).show(ui.ctx(), |ui| {
        ui.label(format!("Load all triangulations in \"{folder_name}\"?"));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Load All").clicked() {
                commands.push(UiCommand::LoadAllTriangulationsInFolder(folder_path.clone()));
                editor.confirm_load_all_folder = None;
            }
            if ui.button("Cancel").clicked() {
                editor.confirm_load_all_folder = None;
            }
        });
    });
    if !open {
        editor.confirm_load_all_folder = None;
    }
}

pub(crate) fn draw_close_unsaved_tri_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    let Some(tri_id) = editor.tri_close_unsaved else {
        return;
    };
    let mut open = true;
    DragableMenu::new("Unsaved Triangulation").open(&mut open).min_width(270.0).show(ui.ctx(), |ui| {
        ui.label("This triangulation is not saved to disk.\nSave now?");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Save As...").clicked() {
                commands.push(UiCommand::SaveAndCloseTriangulationAs(tri_id));
            }
            if ui.button("Close Without Saving").clicked() {
                commands.push(UiCommand::CloseTriangulationForce(tri_id));
            }
            if ui.button("Cancel").clicked() {
                editor.tri_close_unsaved = None;
            }
        });
    });
    if !open {
        editor.tri_close_unsaved = None;
    }
}

pub(crate) fn draw_close_unsaved_block_model_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    let Some(id) = editor.block_model_close_unsaved else {
        return;
    };
    let mut open = true;
    DragableMenu::new("Unsaved Block Model").open(&mut open).min_width(270.0).show(ui.ctx(), |ui| {
        ui.label("This block model is not saved to disk.\nSave now?");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Save As...").clicked() {
                commands.push(UiCommand::SaveAndCloseBlockModelAs(id));
            }
            if ui.button("Close Without Saving").clicked() {
                commands.push(UiCommand::CloseBlockModelForce(id));
            }
            if ui.button("Cancel").clicked() {
                editor.block_model_close_unsaved = None;
            }
        });
    });
    if !open {
        editor.block_model_close_unsaved = None;
    }
}

pub(crate) fn draw_close_unsaved_point_cloud_dialog(ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, editor: &mut EditorState) {
    let Some(id) = editor.point_cloud_close_unsaved else {
        return;
    };
    let mut open = true;
    DragableMenu::new("Unsaved Point Cloud").open(&mut open).min_width(270.0).show(ui.ctx(), |ui| {
        ui.label("This point cloud exists only in memory.\nClose without saving?");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Close Without Saving").clicked() {
                commands.push(UiCommand::ClosePointCloudForce(id));
            }
            if ui.button("Cancel").clicked() {
                editor.point_cloud_close_unsaved = None;
            }
        });
    });
    if !open {
        editor.point_cloud_close_unsaved = None;
    }
}
