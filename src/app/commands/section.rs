//! Bulk show/hide/lock actions for one explorer section, as offered by the
//! right-click menu on each section heading.
//!
//! Every action here is exactly what clicking each of that section's enabled
//! row toggles would do, so the two stay consistent: unloaded items own no
//! live eye or padlock and are left alone.

use crate::{
    app::App,
    i18n::tr_format,
    model::{LayerId, SceneEntityId},
    ui::state::ExplorerSection,
    userspace_log,
};

impl<'a> App<'a> {
    /// Ids of the active project's loaded layers, in document order.
    fn loaded_layer_ids(&self) -> Vec<LayerId> {
        let Some(project) = self.workspace.active_project() else {
            return Vec::new();
        };
        project
            .project
            .document
            .layers()
            .iter()
            .map(|layer| layer.id)
            .filter(|id| project.loaded_layers.contains(id))
            .collect()
    }

    /// Loaded scene entities in `section`, or an empty vec for sections whose
    /// items are not scene entities (designs and rasters).
    fn loaded_section_entities(&self, section: ExplorerSection) -> Vec<SceneEntityId> {
        match section {
            ExplorerSection::Triangulations => self.triangulations.iter().filter(|item| item.state.loaded).map(|item| item.entity_id()).collect(),
            ExplorerSection::PointClouds => self
                .point_clouds
                .iter()
                .filter(|item| item.state.loaded)
                .map(|item| SceneEntityId::PointCloud(item.id))
                .collect(),
            ExplorerSection::BlockModels => self
                .block_models
                .iter()
                .filter(|item| item.state.loaded)
                .map(|item| SceneEntityId::BlockModel(item.id))
                .collect(),
            ExplorerSection::DrillHoles => self
                .drill_holes
                .iter()
                .filter(|item| item.state.loaded)
                .map(|item| SceneEntityId::DrillHole(item.id))
                .collect(),
            ExplorerSection::Designs | ExplorerSection::Rasters => Vec::new(),
        }
    }

    /// Show or hide every loaded item in one explorer section.
    pub(crate) fn set_section_visible(&mut self, section: ExplorerSection, visible: bool) {
        let mut changed = 0usize;
        let mut asset_changed = false;
        match section {
            ExplorerSection::Designs => {
                for id in self.loaded_layer_ids() {
                    let Some(document) = self.workspace.active_document_mut() else {
                        break;
                    };
                    if document.layer(id).is_some_and(|layer| layer.visible != visible) {
                        document.set_layer_visible(id, visible);
                        changed += 1;
                    }
                }
                // Individual objects hidden from the canvas menu own no
                // explorer row, so this is where they come back.
                if visible && let Some(document) = self.workspace.active_document_mut() {
                    document.reveal_all_objects();
                }
            }
            ExplorerSection::Triangulations => {
                for tri in &mut self.triangulations {
                    if tri.state.loaded && tri.visible != visible {
                        tri.visible = visible;
                        tri.state.touch();
                        changed += 1;
                        asset_changed = true;
                    }
                }
            }
            ExplorerSection::Rasters => {
                for raster in &mut self.raster_textures {
                    if raster.state.loaded && raster.visible != visible {
                        raster.visible = visible;
                        raster.state.touch();
                        changed += 1;
                        asset_changed = true;
                    }
                }
            }
            ExplorerSection::PointClouds => {
                for cloud in &mut self.point_clouds {
                    if cloud.state.loaded && cloud.visible != visible {
                        cloud.visible = visible;
                        cloud.state.touch();
                        changed += 1;
                        asset_changed = true;
                    }
                }
            }
            ExplorerSection::BlockModels => {
                for model in &mut self.block_models {
                    if model.state.loaded && model.visible != visible {
                        model.visible = visible;
                        model.state.touch();
                        changed += 1;
                        asset_changed = true;
                    }
                }
            }
            ExplorerSection::DrillHoles => {
                for dataset in &mut self.drill_holes {
                    if dataset.state.loaded && dataset.visible != visible {
                        dataset.visible = visible;
                        dataset.state.touch();
                        changed += 1;
                        asset_changed = true;
                    }
                }
            }
        }
        if visible {
            // A row's eye and the toolbar-era per-entity hide are two sources
            // of the same state; revealing has to clear both, the way
            // `toggle_triangulation_visible` does for one item.
            for entity in self.loaded_section_entities(section) {
                self.editor.hidden_handles.remove(&entity);
            }
        }
        if asset_changed {
            self.touch_active_project_content();
        }
        userspace_log!(
            "{}",
            tr_format!(
                literal = "%verb% %count% item(s) in %section%",
                verb = if visible { "Revealed" } else { "Hid" },
                count = changed,
                section = section.label()
            )
        );
        self.invalidate_geometry();
    }

    /// Lock or unlock every loaded item in one explorer section.
    pub(crate) fn set_section_locked(&mut self, section: ExplorerSection, locked: bool) {
        let mut changed = 0usize;
        match section {
            ExplorerSection::Designs => {
                for id in self.loaded_layer_ids() {
                    let already = self.editor.locked_layers.contains(&id);
                    if already == locked {
                        continue;
                    }
                    changed += 1;
                    if locked {
                        self.editor.locked_layers.insert(id);
                        if self.editor.active_layer == Some(id) {
                            self.editor.active_layer = None;
                        }
                    } else {
                        self.editor.locked_layers.remove(&id);
                    }
                }
                // Same for objects locked from the canvas menu: releasing the
                // layers has to release those too, or they stay stuck.
                if !locked {
                    self.editor.explicitly_frozen.retain(|handle| !matches!(handle, SceneEntityId::Object(_)));
                }
            }
            ExplorerSection::Rasters => {
                for raster in &self.raster_textures {
                    if !raster.state.loaded {
                        continue;
                    }
                    let already = self.editor.locked_rasters.contains(&raster.id);
                    if already == locked {
                        continue;
                    }
                    changed += 1;
                    if locked {
                        self.editor.locked_rasters.insert(raster.id);
                    } else {
                        self.editor.locked_rasters.remove(&raster.id);
                    }
                }
            }
            ExplorerSection::Triangulations | ExplorerSection::PointClouds | ExplorerSection::BlockModels | ExplorerSection::DrillHoles => {
                for entity in self.loaded_section_entities(section) {
                    if self.editor.frozen_handles.contains(&entity) != locked {
                        changed += 1;
                    }
                    self.editor.set_entity_locked(entity, locked);
                }
            }
        }
        userspace_log!(
            "{}",
            tr_format!(
                literal = "%verb% %count% item(s) in %section%",
                verb = if locked { "Locked" } else { "Unlocked" },
                count = changed,
                section = section.label()
            )
        );
        self.invalidate_geometry();
    }
}
