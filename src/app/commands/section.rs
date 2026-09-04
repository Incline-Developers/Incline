//! Bulk show/hide/lock actions for one explorer section, as offered by the
//! right-click menu on each section heading.
//!
//! Every action here is exactly what clicking each of that section's enabled
//! row toggles would do, so the two stay consistent: unloaded items own no
//! live eye or padlock and are left alone.

use crate::{
    app::App,
    i18n::tr_format,
    model::{Command, ItemRef, LayerId, SceneEntityId},
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

    /// Loaded project items in `section`, or an empty vec for Designs, whose
    /// content lives in the document rather than in an item collection.
    fn loaded_section_items(&self, section: ExplorerSection) -> Vec<ItemRef> {
        match section {
            ExplorerSection::Triangulations => self
                .triangulations
                .iter()
                .filter(|item| item.state.loaded)
                .map(|item| ItemRef::Triangulation(item.id))
                .collect(),
            ExplorerSection::Rasters => self.raster_textures.iter().filter(|item| item.state.loaded).map(|item| ItemRef::Raster(item.id)).collect(),
            ExplorerSection::PointClouds => self.point_clouds.iter().filter(|item| item.state.loaded).map(|item| ItemRef::PointCloud(item.id)).collect(),
            ExplorerSection::BlockModels => self.block_models.iter().filter(|item| item.state.loaded).map(|item| ItemRef::BlockModel(item.id)).collect(),
            ExplorerSection::DrillHoles => self.drill_holes.iter().filter(|item| item.state.loaded).map(|item| ItemRef::DrillHole(item.id)).collect(),
            ExplorerSection::Designs => Vec::new(),
        }
    }

    /// Show or hide every loaded item in one explorer section, as a single
    /// undo step.
    pub(crate) fn set_section_visible(&mut self, section: ExplorerSection, visible: bool) {
        let mut commands = Vec::new();
        if section == ExplorerSection::Designs {
            if let Some(document) = self.workspace.active_document() {
                commands.extend(self.loaded_layer_ids().into_iter().filter_map(|id| {
                    document.layer(id).filter(|layer| layer.visible != visible).map(|_| Command::SetLayerVisible {
                        id,
                        before: !visible,
                        after: visible,
                    })
                }));
                // Individual objects hidden from the canvas menu own no
                // explorer row, so this is where they come back.
                if visible {
                    commands.extend(document.hidden_object_ids().map(|id| Command::SetObjectHidden { id, before: true, after: false }));
                }
            }
        } else {
            commands.extend(
                self.loaded_section_items(section)
                    .into_iter()
                    .filter_map(|item| self.item_style_command(item, |style| style.with_visible(visible))),
            );
        }

        let changed = commands.len();
        if visible {
            // A row's eye and the toolbar-era per-entity hide are two sources
            // of the same state; revealing has to clear both, the way
            // `toggle_triangulation_visible` does for one item.
            for entity in self.loaded_section_entities(section) {
                self.editor.hidden_handles.remove(&entity);
            }
        }
        if !commands.is_empty() {
            self.execute_edit(Command::Batch(commands));
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
