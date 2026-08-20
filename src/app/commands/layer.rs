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
        let layer_id = project.pidb.document.allocate_layer_id();
        let layer = Layer {
            id: layer_id,
            name: name.clone(),
            color_index: None,
            color: [1.0, 1.0, 1.0, 1.0],
            visible: true,
            elevation: 0.0,
        };
        self.history.execute(&mut project.pidb.document, Command::AddLayerSnapshot { layer, objects: Vec::new() });
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
        let Some(layer) = project.pidb.document.layer(layer_id).cloned() else {
            return Ok(());
        };
        let layer_index = project
            .pidb
            .document
            .layers()
            .iter()
            .position(|candidate| candidate.id == layer_id)
            .context("Layer position disappeared")?;
        let on_layer = positioned_objects_on_layer(&project.pidb.document, layer_id);
        self.history.execute(
            &mut project.pidb.document,
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
        let Some(source_layer) = project.pidb.document.layer(layer_id).cloned() else {
            return;
        };
        let source_objects = objects_on_layer(&project.pidb.document, layer_id);
        let duplicate_name = unique_layer_name(&project.pidb.document, &format!("{} copy", source_layer.name));

        let doc = &mut project.pidb.document;
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

    /// Move a layer between two open PIDBs. Both documents remain dirty until
    /// their next whole-PIDB save.
    pub(crate) fn move_layer_to_pidb(&mut self, layer_id: LayerId, target_runtime_id: u32) -> Result<()> {
        let source_index = self.workspace.project_index_for_layer(layer_id).context("The source PIDB is no longer open")?;
        let target_index = self
            .workspace
            .project_index_for_runtime_id(target_runtime_id)
            .context("The target PIDB is no longer open")?;
        if source_index == target_index {
            return Ok(());
        }
        let source_project = &self.workspace.projects[source_index];
        let source_runtime_id = source_project.runtime_id;
        let source_layer = source_project.pidb.document.layer(layer_id).cloned().context("The selected layer no longer exists")?;
        let source_objects = objects_on_layer(&source_project.pidb.document, layer_id);

        let target_project = &mut self.workspace.projects[target_index];
        let new_layer_id = target_project.pidb.document.allocate_layer_id();
        let target_name = unique_layer_name(&target_project.pidb.document, &source_layer.name);
        let moved_layer = Layer {
            id: new_layer_id,
            name: target_name.clone(),
            color_index: source_layer.color_index,
            color: source_layer.color,
            visible: source_layer.visible,
            elevation: source_layer.elevation,
        };
        let moved_objects: Vec<Object> = source_objects
            .iter()
            .map(|object| {
                let object_id = target_project.pidb.document.allocate_object_id();
                object.with_id_and_layer(object_id, new_layer_id)
            })
            .collect();
        let moved_handles: Vec<SceneEntityId> = moved_objects.iter().map(|object| SceneEntityId::Object(object.id())).collect();
        target_project.pidb.document.append_layer_snapshot(&moved_layer, moved_objects.iter());
        target_project.loaded_layers.insert(new_layer_id);

        // This is an explicit cross-project transaction. Until workspace-level
        // undo exists, invalidate only the two affected histories; unrelated
        // open PIDBs keep their undo stacks.
        self.history.clear_project(source_runtime_id);
        self.history.clear_project(target_runtime_id);
        self.editor.selected_handles.clear();
        self.editor.hidden_handles.clear();
        self.editor.frozen_handles.clear();
        self.editor.translucent_handles.clear();

        if let Some(source_project) = self.workspace.projects.get_mut(source_index) {
            for object in &source_objects {
                source_project.pidb.document.remove_object(object.id());
            }
            source_project.pidb.document.delete_layer(layer_id);
            source_project.loaded_layers.remove(&layer_id);
        }

        if self.editor.active_layer == Some(layer_id) {
            self.editor.active_layer = None;
        }
        self.activate_project_index(target_index);
        self.editor.active_layer = Some(new_layer_id);
        self.editor.selected_handles = moved_handles.into_iter().collect();
        userspace_log!("Moved layer '{target_name}' into PIDB {target_runtime_id}");
        self.invalidate_geometry();
        Ok(())
    }

    pub(crate) fn load_layer(&mut self, layer_id: LayerId) {
        let scene_was_empty = self.scene_document.objects().is_empty() && self.triangulations.is_empty();
        let Some(index) = self.workspace.project_index_for_layer(layer_id) else {
            return;
        };
        let Some(project) = self.workspace.projects.get_mut(index) else {
            return;
        };
        let Some(name) = project.pidb.document.layer(layer_id).map(|layer| layer.name.clone()) else {
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

    pub(crate) fn load_all_layers(&mut self, runtime_id: u32) {
        let scene_was_empty = self.scene_document.objects().is_empty() && self.triangulations.is_empty();
        let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) else {
            return;
        };
        let Some(project) = self.workspace.projects.get_mut(index) else {
            return;
        };
        let layer_ids: Vec<LayerId> = project.pidb.document.layers().iter().map(|l| l.id).collect();
        for id in &layer_ids {
            project.loaded_layers.insert(*id);
        }
        self.editor.selected_handles.clear();
        userspace_log!("Loaded {} layer(s)", layer_ids.len());
        self.invalidate_geometry();
        if scene_was_empty {
            self.fit_view_to_extents();
        }
    }

    pub(crate) fn unload_all_layers(&mut self, runtime_id: u32) {
        let Some(index) = self.workspace.project_index_for_runtime_id(runtime_id) else {
            return;
        };
        let project = &mut self.workspace.projects[index];
        let layer_ids = std::mem::take(&mut project.loaded_layers);
        let count = layer_ids.len();
        if count == 0 {
            return;
        }
        let object_handles: std::collections::HashSet<_> = project
            .pidb
            .document
            .objects()
            .iter()
            .filter(|object| layer_ids.contains(&object.layer()))
            .map(|object| SceneEntityId::Object(object.id()))
            .collect();
        self.editor.selected_handles.retain(|handle| !object_handles.contains(handle));
        self.editor.hidden_handles.retain(|handle| !object_handles.contains(handle));
        self.editor.frozen_handles.retain(|handle| !object_handles.contains(handle));
        self.editor.translucent_handles.retain(|handle| !object_handles.contains(handle));
        if self.editor.active_layer.is_some_and(|id| layer_ids.contains(&id)) {
            self.editor.active_layer = None;
        }
        userspace_log!("Unloaded {count} layer(s)");
        self.invalidate_geometry();
    }

    pub(crate) fn select_all_objects_in_layer(&mut self, layer_id: LayerId) {
        let Some(project) = self.workspace.active_project() else {
            return;
        };
        if !project.loaded_layers.contains(&layer_id) {
            return;
        }
        let handles: Vec<SceneEntityId> = project
            .pidb
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
    /// written out by whole-PIDB saves.
    pub(crate) fn unload_layer(&mut self, layer_id: LayerId) {
        let Some(index) = self.workspace.project_index_for_layer(layer_id) else {
            return;
        };
        let Some(project) = self.workspace.projects.get_mut(index) else {
            return;
        };
        let object_handles: Vec<_> = project
            .pidb
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
            self.editor.frozen_handles.remove(&handle);
            self.editor.translucent_handles.remove(&handle);
        }
        if self.editor.active_layer == Some(layer_id) {
            self.editor.active_layer = None;
        }
        userspace_log!("Unloaded layer {:?}", layer_id);
        self.invalidate_geometry();
    }
}
