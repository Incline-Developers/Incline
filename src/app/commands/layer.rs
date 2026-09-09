use anyhow::{Context, Result};

use crate::{
    app::App,
    i18n::{tr, tr_format},
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
            loaded: true,
            elevation: 0.0,
        };
        self.execute_edit(Command::AddLayerSnapshot { layer, objects: Vec::new() });

        self.editor.selected_handles.clear();
        self.editor.active_layer = Some(layer_id);
        userspace_log!("{}", tr_format!(literal = "Created layer '%name%'", name = name));
        self.invalidate_geometry();
        Ok(())
    }

    pub(crate) fn delete_layer(&mut self, layer_id: LayerId) -> Result<()> {
        if self.restore_layers_for(vec![layer_id], move |app| {
            if let Err(error) = app.delete_layer(layer_id) {
                crate::userspace_error!("{error:#}");
            }
        }) {
            return Ok(());
        }
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
        self.execute_edit(Command::DeleteLayerSnapshot {
            layer,
            layer_index,
            objects: on_layer,
        });

        if self.editor.active_layer == Some(layer_id) {
            self.editor.active_layer = None;
        }
        self.editor.selected_handles.clear();
        userspace_log!(
            "{}",
            tr_format!(literal = "Deleted layer %layer_id% (and all objects on it)", layer_id = format!("{layer_id:?}"))
        );
        self.invalidate_geometry();
        Ok(())
    }

    pub(crate) fn duplicate_layer(&mut self, layer_id: LayerId) {
        if self.restore_layers_for(vec![layer_id], move |app| app.duplicate_layer(layer_id)) {
            return;
        }
        let Some(project) = self.workspace.active_project_mut() else {
            return;
        };
        let Some(source_layer) = project.project.document.layer(layer_id).cloned() else {
            return;
        };
        let source_objects = objects_on_layer(&project.project.document, layer_id);
        let duplicate_name = unique_layer_name(&project.project.document, &tr_format!(literal = "%name% copy", name = &source_layer.name));

        let doc = &mut project.project.document;
        let new_layer_id = doc.allocate_layer_id();
        let duplicate_layer = Layer {
            id: new_layer_id,
            name: duplicate_name.clone(),
            color_index: source_layer.color_index,
            color: source_layer.color,
            loaded: source_layer.loaded,
            elevation: source_layer.elevation,
        };
        let duplicate_objects: Vec<Object> = source_objects
            .into_iter()
            .map(|object| {
                let object_id = doc.allocate_object_id();
                object.with_id_and_layer(object_id, new_layer_id)
            })
            .collect();

        self.execute_edit(Command::AddLayerSnapshot {
            layer: duplicate_layer,
            objects: duplicate_objects,
        });

        self.editor.selected_handles.clear();
        userspace_log!("{}", tr_format!(literal = "Duplicated layer '%duplicate_name%'", duplicate_name = duplicate_name));
        self.invalidate_geometry();
    }

    pub(crate) fn load_layer(&mut self, layer_id: LayerId) {
        self.set_layer_loaded(layer_id, true);
    }

    fn set_layer_loaded(&mut self, layer_id: LayerId, loaded: bool) {
        self.activate_project_for_layer(layer_id);
        if !loaded {
            self.cancel_jobs(|key| matches!(key, crate::app::jobs::JobKey::LayerResidency { layer: pending, restoring: true, .. } if *pending == layer_id));
        } else if self.layer_load_pending(layer_id) {
            return;
        }
        let Some(layer) = self.workspace.active_document().and_then(|document| document.layer(layer_id)) else {
            return;
        };
        if layer.loaded == loaded {
            return;
        }
        self.execute_edit(Command::SetLayerLoaded {
            id: layer_id,
            before: layer.loaded,
            after: loaded,
        });
    }

    pub(crate) fn select_all_objects_in_layer(&mut self, layer_id: LayerId) {
        let Some(project) = self.workspace.active_project() else {
            return;
        };
        if !project.project.document.layer(layer_id).is_some_and(|layer| layer.loaded) {
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
        userspace_log!(
            "{}",
            tr_format!(
                literal = "Selected %count% object(s) in layer %layer_id%",
                count = count,
                layer_id = format!("{layer_id:?}")
            )
        );
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    /// Unload a layer from the scene while retaining its project membership.
    pub(crate) fn unload_layer(&mut self, layer_id: LayerId) {
        self.set_layer_loaded(layer_id, false);
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
        let state = if locked { tr!(literal = "Locked") } else { tr!(literal = "Unlocked") };
        userspace_log!("{}", tr_format!(literal = "%state% layer '%name%'", state = state, name = name));
        self.invalidate_geometry();
    }
}
