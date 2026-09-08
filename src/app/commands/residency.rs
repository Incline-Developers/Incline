//! Load/unload transitions and their worker-owned backing I/O.

use crate::{
    app::{App, jobs::JobKey},
    i18n::tr,
    model::{ItemRef, OpenItem, project::ProjectItemState},
    userspace_error,
};

impl<'a> App<'a> {
    pub(crate) fn project_item_state(&self, item: ItemRef) -> Option<&ProjectItemState> {
        match item {
            ItemRef::Triangulation(id) => self.triangulations.iter().find(|item| item.id == id).map(|item| &item.state),
            ItemRef::BlockModel(id) => self.block_models.iter().find(|item| item.id == id).map(|item| &item.state),
            ItemRef::DrillHole(id) => self.drill_holes.iter().find(|item| item.id == id).map(|item| &item.state),
            ItemRef::PointCloud(id) => self.point_clouds.iter().find(|item| item.id == id).map(|item| &item.state),
            ItemRef::Raster(id) => self.raster_textures.iter().find(|item| item.id == id).map(|item| &item.state),
        }
    }

    fn clone_project_item(&self, item: ItemRef) -> Option<OpenItem> {
        match item {
            ItemRef::Triangulation(id) => self
                .triangulations
                .iter()
                .find(|item| item.id == id)
                .map(|item| OpenItem::Triangulation(Box::new(item.clone()))),
            ItemRef::BlockModel(id) => self.block_models.iter().find(|item| item.id == id).map(|item| OpenItem::BlockModel(Box::new(item.clone()))),
            ItemRef::DrillHole(id) => self.drill_holes.iter().find(|item| item.id == id).map(|item| OpenItem::DrillHole(Box::new(item.clone()))),
            ItemRef::PointCloud(id) => self.point_clouds.iter().find(|item| item.id == id).map(|item| OpenItem::PointCloud(Box::new(item.clone()))),
            ItemRef::Raster(id) => self.raster_textures.iter().find(|item| item.id == id).map(|item| OpenItem::Raster(Box::new(item.clone()))),
        }
    }

    fn replace_residency(&mut self, item: OpenItem) {
        // A save may finish while backing I/O is running without changing
        // the content revision. Preserve its saved epoch and current metadata.
        fn preserve_state(current: &ProjectItemState, replacement: &mut ProjectItemState) {
            let mut state = current.clone();
            state.deferred = replacement.deferred.take();
            state.summary = replacement.summary.take();
            *replacement = state;
        }
        match item {
            OpenItem::Triangulation(mut item) => {
                if let Some(target) = self.triangulations.iter_mut().find(|target| target.id == item.id) {
                    preserve_state(&target.state, &mut item.state);
                    *target = *item;
                }
            }
            OpenItem::BlockModel(mut item) => {
                if let Some(target) = self.block_models.iter_mut().find(|target| target.id == item.id) {
                    preserve_state(&target.state, &mut item.state);
                    *target = *item;
                }
            }
            OpenItem::DrillHole(mut item) => {
                if let Some(target) = self.drill_holes.iter_mut().find(|target| target.id == item.id) {
                    preserve_state(&target.state, &mut item.state);
                    *target = *item;
                }
            }
            OpenItem::PointCloud(mut item) => {
                if let Some(target) = self.point_clouds.iter_mut().find(|target| target.id == item.id) {
                    preserve_state(&target.state, &mut item.state);
                    *target = *item;
                }
            }
            OpenItem::Raster(mut item) => {
                if let Some(target) = self.raster_textures.iter_mut().find(|target| target.id == item.id) {
                    preserve_state(&target.state, &mut item.state);
                    *target = *item;
                }
            }
        }
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn item_load_pending(&self, item: ItemRef) -> bool {
        self.pending_jobs.iter().any(|job| {
            job.keys
                .iter()
                .any(|key| matches!(key, JobKey::Residency { item: pending, restoring: true, .. } if *pending == item))
        })
    }

    pub(crate) fn layer_load_pending(&self, layer: crate::model::LayerId) -> bool {
        self.pending_jobs.iter().any(|job| {
            job.keys
                .iter()
                .any(|key| matches!(key, JobKey::LayerResidency { layer: pending, restoring: true, .. } if *pending == layer))
        })
    }

    fn restore_pending(&self) -> bool {
        self.pending_jobs.iter().any(|job| {
            job.keys.iter().any(|key| {
                matches!(
                    key,
                    JobKey::Residency { restoring: true, .. } | JobKey::LayerResidency { restoring: true, .. } | JobKey::HistoryResidency { restoring: true, .. }
                )
            })
        })
    }

    pub(crate) fn set_item_loaded(&mut self, item: ItemRef, loaded: bool) {
        if !loaded {
            self.cancel_jobs(|key| matches!(key, JobKey::Residency { item: pending, restoring: true, .. } if *pending == item));
        } else if self.item_load_pending(item) {
            return;
        }
        if let Some(style) = self.item_style(item) {
            self.set_item_style(item, style.with_loaded(loaded));
        }
        if !loaded {
            self.evict_unloaded_items();
            self.evict_unloaded_layers();
        }
    }

    /// Returns true when the continuation was deferred. An error keeps the
    /// unloaded entry and its backing intact; it never applies a partial edit.
    pub(crate) fn restore_items_for(&mut self, needed: Vec<ItemRef>, continuation: impl FnOnce(&mut App<'a>) + 'a) -> bool {
        let mut seen = std::collections::HashSet::new();
        let items: Vec<_> = needed
            .into_iter()
            .filter(|item| seen.insert(*item))
            .filter(|item| self.project_item_state(*item).is_some_and(|state| state.deferred.is_some()))
            .filter_map(|item| self.clone_project_item(item))
            .collect();
        if items.is_empty() {
            return false;
        }
        let Some(runtime_id) = self.workspace.active_project().map(|project| project.runtime_id) else {
            return false;
        };
        let keys = items
            .iter()
            .map(|item| JobKey::Residency {
                item: item.item_ref(),
                runtime_id,
                revision: item.state().revision(),
                restoring: true,
            })
            .collect();
        self.spawn_job(
            tr!("asset-loading"),
            keys,
            move |cancel| {
                let mut restored = Vec::new();
                for item in items {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    restored.push(item.materialize()?);
                }
                Ok(restored)
            },
            move |app, result| match result {
                Ok(items) => {
                    for item in items {
                        app.replace_residency(item);
                    }
                    continuation(app);
                }
                Err(error) => userspace_error!("{}: {error:#}", tr!("asset-load-failed")),
            },
        );
        true
    }

    pub(crate) fn evict_unloaded_items(&mut self) {
        if self.restore_pending() {
            return;
        }
        let Some(runtime_id) = self.workspace.active_project().map(|project| project.runtime_id) else {
            return;
        };
        let items: Vec<_> = self
            .triangulations
            .iter()
            .map(|item| ItemRef::Triangulation(item.id))
            .chain(self.block_models.iter().map(|item| ItemRef::BlockModel(item.id)))
            .chain(self.drill_holes.iter().map(|item| ItemRef::DrillHole(item.id)))
            .chain(self.point_clouds.iter().map(|item| ItemRef::PointCloud(item.id)))
            .chain(self.raster_textures.iter().map(|item| ItemRef::Raster(item.id)))
            .filter(|item| self.project_item_state(*item).is_some_and(|state| !state.loaded && state.deferred.is_none()))
            .collect();
        for item in items {
            let Some(snapshot) = self.clone_project_item(item) else {
                continue;
            };
            let key = JobKey::Residency {
                item,
                runtime_id,
                revision: snapshot.state().revision(),
                restoring: false,
            };
            if self.pending_jobs.iter().any(|job| job.keys.contains(&key)) {
                continue;
            }
            self.spawn_job_reporting_progress(
                tr!("asset-unloading"),
                vec![key],
                move |cancel, progress| {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    snapshot.evict(&progress.phase(0.0, 1.0))
                },
                move |app, result| match result {
                    Ok(item) => {
                        app.replace_residency(item);
                        app.archive_unloaded_history();
                    }
                    Err(error) => {
                        // Data is still resident if writing failed; restore the
                        // loaded state so the row never claims memory was freed.
                        app.set_item_loaded(item, true);
                        userspace_error!("{}: {error:#}", tr!("asset-unload-failed"));
                    }
                },
            );
        }
    }
}

impl<'a> App<'a> {
    pub(crate) fn restore_layers_for(&mut self, needed: Vec<crate::model::LayerId>, continuation: impl FnOnce(&mut App<'a>) + 'a) -> bool {
        let Some(project) = self.workspace.active_project() else {
            return false;
        };
        let mut seen = std::collections::HashSet::new();
        let layers: Vec<_> = needed
            .into_iter()
            .filter(|id| seen.insert(*id))
            .filter_map(|id| project.project.document.deferred_layers.get(&id).map(|stored| (id, stored.clone())))
            .collect();
        if layers.is_empty() {
            return false;
        }
        let hashes = project.current_layer_hashes();
        let keys = layers
            .iter()
            .map(|(id, _)| JobKey::LayerResidency {
                layer: *id,
                runtime_id: project.runtime_id,
                content_hash: hashes[&(id.0 & u64::from(u32::MAX))],
                restoring: true,
            })
            .collect();
        self.spawn_job(
            tr!("asset-loading"),
            keys,
            move |cancel| {
                let mut restored = Vec::new();
                for (id, stored) in layers {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    restored.push((id, stored.read(id)?));
                }
                Ok(restored)
            },
            move |app, result| match result {
                Ok(layers) => {
                    if let Some(project) = app.workspace.active_project_mut() {
                        for (id, payload) in layers {
                            if project.project.document.deferred_layers.contains_key(&id) {
                                project.project.document.restore_layer_payload(id, payload);
                            }
                        }
                    }
                    continuation(app);
                }
                Err(error) => userspace_error!("{}: {error:#}", tr!("asset-load-failed")),
            },
        );
        true
    }

    pub(crate) fn evict_unloaded_layers(&mut self) {
        if self.restore_pending() {
            return;
        }
        let Some(project) = self.workspace.active_project() else {
            return;
        };
        let document = &project.project.document;
        let layers: Vec<_> = document
            .layers()
            .iter()
            .filter(|layer| !layer.loaded && !document.deferred_layers.contains_key(&layer.id))
            .map(|layer| layer.id)
            .collect();
        if layers.is_empty() {
            return;
        }
        let hashes = project.current_layer_hashes();
        let keys = layers
            .iter()
            .map(|id| JobKey::LayerResidency {
                layer: *id,
                runtime_id: project.runtime_id,
                content_hash: hashes[&(id.0 & u64::from(u32::MAX))],
                restoring: false,
            })
            .collect();
        if self.pending_jobs.iter().any(|job| job.keys == keys) {
            return;
        }
        // Replace resident objects only after every backing write succeeds.
        let payloads: Vec<_> = layers.iter().map(|&id| (id, document.layer_payload(id))).collect();
        self.snap_index = Default::default();
        self.snap_index_dirty = true;
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.release_design_memory(document, &layers);
        }
        self.spawn_job(
            tr!("asset-unloading"),
            keys,
            move |cancel| {
                let mut stored = Vec::new();
                for (id, payload) in payloads {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    stored.push((id, payload.store()?));
                }
                Ok(stored)
            },
            move |app, result| match result {
                Ok(layers) => {
                    if let Some(project) = app.workspace.active_project_mut() {
                        for (id, stored) in layers {
                            project.project.document.install_deferred_layer(id, stored);
                        }
                    }
                    app.invalidate_geometry();
                    app.archive_unloaded_history();
                }
                Err(error) => {
                    for id in layers {
                        app.load_layer(id);
                    }
                    userspace_error!("{}: {error:#}", tr!("asset-unload-failed"));
                }
            },
        );
    }
}

impl<'a> App<'a> {
    pub(crate) fn archive_unloaded_history(&mut self) {
        let Some(project) = self.workspace.active_project() else {
            return;
        };
        let runtime_id = project.runtime_id;
        let layers: Vec<_> = project.project.document.layers().iter().filter(|layer| !layer.loaded).map(|layer| layer.id).collect();
        let items: Vec<_> = self.drill_holes.iter().filter(|item| !item.state.loaded).map(|item| ItemRef::DrillHole(item.id)).collect();
        let revision = self.history.archive_revision();
        let key = JobKey::HistoryResidency {
            runtime_id,
            revision,
            restoring: false,
        };
        if self.pending_jobs.iter().any(|job| job.keys.contains(&key)) {
            return;
        }
        let requests = self.history.archive_requests(&layers, &items);
        if requests.is_empty() {
            return;
        }
        self.spawn_job(
            tr!("asset-unloading"),
            vec![key],
            move |cancel| {
                let mut archived = Vec::new();
                for request in requests {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    archived.push(request.write()?);
                }
                Ok(archived)
            },
            move |app, result| {
                if app.workspace.active_project().is_none_or(|project| project.runtime_id != runtime_id) {
                    return;
                }
                if app.history.archive_revision() != revision {
                    app.archive_unloaded_history();
                    return;
                }
                match result {
                    Ok(commands) => app.history.install_archived_commands(commands),
                    Err(error) => {
                        for layer in layers {
                            app.load_layer(layer);
                        }
                        for item in items {
                            app.set_item_loaded(item, true);
                        }
                        userspace_error!("{}: {error:#}", tr!("asset-unload-failed"));
                    }
                }
            },
        );
    }

    pub(crate) fn restore_history_step(&mut self, undo: bool) -> bool {
        let requests = self.history.archived_step(undo);
        if requests.is_empty() {
            return false;
        }
        let Some(runtime_id) = self.workspace.active_project().map(|project| project.runtime_id) else {
            return false;
        };
        let revision = self.history.archive_revision();
        let key = JobKey::HistoryResidency {
            runtime_id,
            revision,
            restoring: true,
        };
        if self.pending_jobs.iter().any(|job| job.keys.contains(&key)) {
            return true;
        }
        self.spawn_job(
            tr!("asset-loading"),
            vec![key],
            move |cancel| {
                let mut restored = Vec::new();
                for (sequence, path, backing) in requests {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    restored.push(crate::model::history_storage::ArchivedCommand {
                        sequence,
                        path,
                        command: serde_json::from_slice(&backing.read()?)?,
                    });
                }
                Ok(restored)
            },
            move |app, result| {
                if app.workspace.active_project().is_none_or(|project| project.runtime_id != runtime_id) || app.history.archive_revision() != revision {
                    return;
                }
                match result {
                    Ok(commands) => {
                        app.history.install_archived_commands(commands);
                        app.apply_history_step(undo);
                    }
                    Err(error) => userspace_error!("{}: {error:#}", tr!("asset-load-failed")),
                }
            },
        );
        true
    }
}
