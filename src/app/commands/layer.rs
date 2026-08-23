use anyhow::{Context, Result};

use crate::{
    app::App,
    model::{Command, Document, Layer, LayerId, Object, SceneEntityId},
    userspace_log,
};

fn unique_layer_name(document: &Document, preferred: &str) -> String {
    if document.layer_id_by_name(preferred).is_none() {
        return preferred.to_string();
    }

    for index in 2.. {
        let candidate = format!("{preferred} {index}");
        if document.layer_id_by_name(&candidate).is_none() {
            return candidate;
        }
    }

    unreachable!("unbounded iterator should always find a unique layer name")
}

fn objects_on_layer(document: &Document, layer_id: LayerId) -> Vec<Object> {
    document.objects().iter().filter(|object| object.layer() == layer_id).cloned().collect()
}

fn positioned_objects_on_layer(document: &Document, layer_id: LayerId) -> Vec<(usize, Object)> {
    document
        .objects()
        .iter()
        .enumerate()
        .filter(|(_, object)| object.layer() == layer_id)
        .map(|(index, object)| (index, object.clone()))
        .collect()
}

impl<'a> App<'a> {
    pub(crate) fn create_layer(&mut self, name: String) -> Result<()> {
        let Some(project) = self.workspace.active_project_mut() else {
            return Ok(());
        };
        let layer_id = project.project.document.allocate_layer_id();
        let layer = Layer {
            id: layer_id,
            name: name.clone(),
            color_index: None,
            color: [1.0, 1.0, 1.0, 1.0],
            visible: true,
            elevation: 0.0,
        };
        self.history
            .execute(&mut project.project.document, Command::AddLayerSnapshot { layer, objects: Vec::new() });
        project.loaded_layers.insert(layer_id);
        self.editor.selected_handles.clear();
        self.editor.active_layer = Some(layer_id);
        userspace_log!("Created layer '{name}'");
        self.invalidate_geometry();
        Ok(())
    }

    pub(crate) fn delete_layer(&mut self, layer_id: LayerId) -> Result<()> {
        self.editor.pending_delete_layer = None;
        let Some(project) = self.workspace.active_project_mut() else {
            return Ok(());
        };
        let Some(layer) = project.project.document.layer(layer_id).cloned() else {
            return Ok(());
        };
        let layer_index = project
            .project
            .document
            .layers()
            .iter()
            .position(|candidate| candidate.id == layer_id)
            .context("Layer position disappeared")?;
        let on_layer = positioned_objects_on_layer(&project.project.document, layer_id);
        self.history.execute(
            &mut project.project.document,
            Command::DeleteLayerSnapshot {
                layer,
                layer_index,
                objects: on_layer,
            },
        );
        project.loaded_layers.remove(&layer_id);
        if self.editor.active_layer == Some(layer_id) {
            self.editor.active_layer = None;
        }
        self.editor.selected_handles.clear();
        userspace_log!("Deleted layer {:?} (and all objects on it)", layer_id);
        self.invalidate_geometry();
        Ok(())
    }

    pub(crate) fn duplicate_layer(&mut self, layer_id: LayerId) {
        let Some(project) = self.workspace.active_project_mut() else {
            return;
        };
        let Some(source_layer) = project.project.document.layer(layer_id).cloned() else {
            return;
        };
        let source_objects = objects_on_layer(&project.project.document, layer_id);
        let duplicate_name = unique_layer_name(&project.project.document, &format!("{} copy", source_layer.name));

        let doc = &mut project.project.document;
        let new_layer_id = doc.allocate_layer_id();
        let duplicate_layer = Layer {
            id: new_layer_id,
            name: duplicate_name.clone(),
            color_index: source_layer.color_index,
            color: source_layer.color,
            visible: source_layer.visible,
            elevation: source_layer.elevation,
        };
        let duplicate_objects: Vec<Object> = source_objects
            .into_iter()
            .map(|object| {
                let object_id = doc.allocate_object_id();
                object.with_id_and_layer(object_id, new_layer_id)
            })
            .collect();

        self.history.execute(
            doc,
            Command::AddLayerSnapshot {
                layer: duplicate_layer,
                objects: duplicate_objects,
            },
        );
        project.loaded_layers.insert(new_layer_id);
        self.editor.selected_handles.clear();
        userspace_log!("Duplicated layer '{duplicate_name}'");
        self.invalidate_geometry();
    }

    pub(crate) fn load_layer(&mut self, layer_id: LayerId) {
        let scene_was_empty = self.scene_document.objects().is_empty() && self.triangulations.is_empty();
        let Some(index) = self.workspace.project_index_for_layer(layer_id) else {
            return;
        };
        let Some(project) = self.workspace.projects.get_mut(index) else {
            return;
        };
        let Some(name) = project.project.document.layer(layer_id).map(|layer| layer.name.clone()) else {
            return;
        };
        project.loaded_layers.insert(layer_id);
        self.editor.selected_handles.clear();
        userspace_log!("Loaded layer '{name}'");
        self.invalidate_geometry();
        if scene_was_empty {
            self.fit_view_to_extents();
        }
    }

    pub(crate) fn select_all_objects_in_layer(&mut self, layer_id: LayerId) {
        let Some(project) = self.workspace.active_project() else {
            return;
        };
        if !project.loaded_layers.contains(&layer_id) {
            return;
        }
        let handles: Vec<SceneEntityId> = project
            .project
            .document
            .objects()
            .iter()
            .filter(|object| object.layer() == layer_id)
            .map(|object| SceneEntityId::Object(object.id()))
            .collect();

        self.editor.active_layer = Some(layer_id);
        self.editor.selected_handles = handles.into_iter().collect();
        self.editor.tri_selected_object_ids.clear();
        self.editor.tri_selected_layer_ids.clear();
        self.editor.canvas_context_menu_open = false;
        let count = self.editor.selected_handles.len();
        userspace_log!("Selected {count} object(s) in layer {:?}", layer_id);
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    /// Unload a layer from the scene. Purely a visibility change: the layer's
    /// in-memory state (including unsaved edits) stays in the document and is
    /// written out by whole-project saves.
    pub(crate) fn unload_layer(&mut self, layer_id: LayerId) {
        let Some(index) = self.workspace.project_index_for_layer(layer_id) else {
            return;
        };
        let Some(project) = self.workspace.projects.get_mut(index) else {
            return;
        };
        let object_handles: Vec<_> = project
            .project
            .document
            .objects()
            .iter()
            .filter(|object| object.layer() == layer_id)
            .map(|object| SceneEntityId::Object(object.id()))
            .collect();
        if !project.loaded_layers.remove(&layer_id) {
            return;
        }
        for handle in object_handles {
            self.editor.selected_handles.remove(&handle);
            self.editor.hidden_handles.remove(&handle);
            self.editor.explicitly_frozen.remove(&handle);
            self.editor.frozen_handles.remove(&handle);
            self.editor.translucent_handles.remove(&handle);
        }
        if self.editor.active_layer == Some(layer_id) {
            self.editor.active_layer = None;
        }
        userspace_log!("Unloaded layer {:?}", layer_id);
        self.invalidate_geometry();
    }

    /// Show or hide a loaded layer without unloading it.
    ///
    /// Unlike load/unload this keeps the layer's objects in the scene
    /// document, so snapping targets and selection sets survive the toggle.
    pub(crate) fn toggle_layer_visible(&mut self, layer_id: LayerId) {
        self.activate_project_for_layer(layer_id);
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        let Some(layer) = document.layer(layer_id) else {
            return;
        };
        let (name, visible) = (layer.name.clone(), !layer.visible);
        document.set_layer_visible(layer_id, visible);
        userspace_log!("{} layer '{}'", if visible { "Shown" } else { "Hidden" }, name);
        self.invalidate_geometry();
    }

    /// Lock or unlock every object on a layer against selection and editing.
    ///
    /// The lock is held per layer; `invalidate_geometry` expands it onto the
    /// individual object handles that picking and snapping test.
    pub(crate) fn toggle_layer_locked(&mut self, layer_id: LayerId) {
        self.activate_project_for_layer(layer_id);
        let name = self
            .workspace
            .active_project()
            .and_then(|project| project.project.document.layer(layer_id))
            .map(|layer| layer.name.clone());
        let Some(name) = name else {
            return;
        };
        let locked = !self.editor.locked_layers.remove(&layer_id);
        if locked {
            self.editor.locked_layers.insert(layer_id);
            // A locked layer cannot be the drawing target, and nothing on it
            // may stay selected.
            if self.editor.active_layer == Some(layer_id) {
                self.editor.active_layer = None;
            }
        }
        userspace_log!("{} layer '{}'", if locked { "Locked" } else { "Unlocked" }, name);
        self.invalidate_geometry();
    }
}
