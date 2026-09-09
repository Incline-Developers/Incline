//! Whole-project Open Mining Format import/export commands.

use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Context;
use anyhow::Result;

use crate::{
    app::App,
    i18n::{tr, tr_format},
    model::{
        formats::omf::{self, ImportBundle, ProjectSnapshot},
        project,
        triangulation::{LoadedTriangulation, OpenTriangulation, TriangulationId},
    },
    userspace_log, userspace_warn,
};

/// Whether installing a decoded OMF re-frames the camera.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewOnOpen {
    /// Opening or switching projects: reset to a plan view fitted to the new
    /// project's content.
    Fit,
    /// Reloading the project already on screen (a revert): keep the camera.
    /// Reverting is native-only, so the browser build never constructs this.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Keep,
}

impl<'a> App<'a> {
    pub(super) fn omf_export_snapshot(&mut self) -> Result<ProjectSnapshot> {
        if self.has_pending_move_delta() {
            self.commit_pending_move();
        }
        for project_index in 0..self.workspace.projects.len() {
            self.ensure_project_has_no_pending_text_edit(project_index)?;
        }
        let name = self
            .workspace
            .active_project()
            .map(|project| project.project.metadata.name.trim_end_matches(".omf").to_owned())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| tr!(literal = "Incline Design project"));
        let snapshot = ProjectSnapshot {
            name,
            designs: self.workspace.active_project().map(|project| project.project.clone()),
            triangulations: self.triangulations.clone(),
            block_models: self.block_models.clone(),
            drill_holes: self.drill_holes.clone(),
            point_clouds: self.point_clouds.clone(),
            rasters: self.raster_textures.clone(),
        };
        if snapshot.is_empty() {
            anyhow::bail!(tr!(literal = "There is no open Incline Design data to export"));
        }
        Ok(snapshot)
    }

    /// Install a decoded OMF as the single native project. Unlike OMF merge,
    /// this establishes the file as the clean persistence baseline and
    /// replaces every item owned by the previous project.
    pub(crate) fn apply_opened_omf_bundle(&mut self, path: Option<PathBuf>, source_name: String, bundle: ImportBundle, view: ViewOnOpen) {
        // A view left pointing into the outgoing project frames nothing in the
        // incoming one, which shares no coordinates with it. Reverting is the
        // exception: it reloads the project already on screen, so the camera
        // stays where the user put it - unless there was nothing to look at.
        let should_fit = view == ViewOnOpen::Fit || !self.scene_has_renderables();
        let lossy_save_warnings = bundle.warnings.clone();
        for warning in &lossy_save_warnings {
            userspace_warn!(
                "{}",
                tr_format!(literal = "%source_name%: %warning%", source_name = source_name.clone(), warning = warning.clone())
            );
        }
        let ImportBundle {
            project_name,
            coordinate_reference_system,
            units,
            origin: _,
            designs,
            triangulations,
            block_models,
            drill_holes,
            point_clouds,
            rasters,
            warnings: _,
        } = bundle;

        let mut design = project::new_empty(path.clone());
        if !project_name.trim().is_empty() {
            design.metadata.name = project_name.clone();
        }
        design.metadata.coordinate_reference_system = coordinate_reference_system;
        design.metadata.units = units;
        for imported in designs {
            project::merge_document_preserve_ids(&mut design.document, &imported.document);
        }
        let opened = match path.clone() {
            Some(path) => project::open_project(Some(path), design),
            #[cfg(target_arch = "wasm32")]
            None => project::open_imported_project(source_name.clone(), design),
            #[cfg(not(target_arch = "wasm32"))]
            None => project::open_project(None, design),
        };
        let opened = match opened {
            Ok(opened) => opened,
            Err(error) => {
                userspace_warn!(
                    "{}",
                    tr_format!(
                        literal = "Could not open project %source_name%: %error%",
                        source_name = source_name.clone(),
                        error = format!("{error:#}")
                    )
                );
                return;
            }
        };
        self.set_active_project(opened);
        if let Some(project) = self.workspace.active_project_mut() {
            project.lossy_save_warnings = lossy_save_warnings;
            project.lossy_save_confirmed = false;
        }

        let mut raster_id_map = std::collections::HashMap::new();
        for imported in rasters {
            let target_id = allocate_item_id(imported.preferred_id, &mut self.next_raster_texture_id, self.raster_textures.iter().map(|item| item.id.0));
            let preferred_id = imported.preferred_id;
            let source_name = imported.source_name;
            let source_format = imported.source_format;
            self.add_loaded_raster(imported.loaded);
            if let Some(open) = self.raster_textures.last_mut() {
                open.id = crate::model::raster::RasterTextureId(target_id);
                open.state.set_provenance(source_name, source_format);
                open.state = open.state.clone().with_loaded(imported.is_loaded).with_deferred(imported.deferred);
            }
            if let Some(preferred_id) = preferred_id {
                raster_id_map.insert(preferred_id, crate::model::raster::RasterTextureId(target_id));
            }
        }

        for imported in triangulations {
            let preferred_id = imported.preferred_id;
            let source_name = imported.source_name;
            let source_format = imported.source_format;
            let raster_texture = imported.raster_texture_id.and_then(|id| raster_id_map.get(&id).copied());
            let LoadedTriangulation {
                mut name,
                path: _,
                mesh,
                spatial,
                edges,
                surface_face_order,
            } = imported.loaded;
            name = project::unique_item_name(name, self.triangulations.iter().map(|item| item.name.as_str()));
            let id = TriangulationId(allocate_item_id(
                preferred_id,
                &mut self.next_triangulation_id,
                self.triangulations.iter().map(|item| item.id.0),
            ));
            self.triangulations.push(OpenTriangulation {
                id,
                state: crate::model::project::ProjectItemState::dirty_with_format(source_name, source_format)
                    .with_loaded(imported.is_loaded)
                    .with_deferred(imported.deferred),
                name,
                mesh,
                spatial,
                edges,
                surface_face_order,
                color: imported.color,
                line_color: imported.line_color,
                line_weight: imported.line_weight,
                raster_texture,
                raster_opacity: imported.raster_opacity,
            });
            self.touch_active_project_content();
            if imported.is_loaded {
                self.active_triangulation.get_or_insert(id);
            }
        }
        for imported in block_models {
            let target_id = allocate_item_id(imported.preferred_id, &mut self.next_block_model_id, self.block_models.iter().map(|item| item.id.0));
            self.add_loaded_block_model(imported.loaded);
            if let Some(open) = self.block_models.last_mut() {
                open.id = crate::model::block_model::BlockModelId(target_id);
                open.state.set_provenance(imported.source_name, imported.source_format);
                open.state = open.state.clone().with_loaded(imported.is_loaded).with_deferred(imported.deferred);
                open.color = imported.color;
                open.slice = imported.slice;
                if !open.state.loaded {
                    open.color_transfers.clear();
                }
                open.color_transfers.extend(imported.color_transfers);
                open.hide_empty_color_values = imported.hide_empty_color_values;
            }
        }
        for imported in drill_holes {
            let target_id = allocate_item_id(imported.preferred_id, &mut self.next_drill_hole_id, self.drill_holes.iter().map(|item| item.id.0));
            self.add_loaded_drill_holes(imported.loaded);
            if let Some(open) = self.drill_holes.last_mut() {
                open.id = crate::model::drill_hole::DrillHoleId(target_id);
                open.state.set_provenance(imported.source_name, imported.source_format);
                open.state = open.state.clone().with_loaded(imported.is_loaded).with_deferred(imported.deferred);
                open.color = imported.color;
            }
        }
        for imported in point_clouds {
            let target_id = allocate_item_id(imported.preferred_id, &mut self.next_point_cloud_id, self.point_clouds.iter().map(|item| item.id.0));
            self.add_loaded_point_cloud(imported.loaded, imported.is_loaded, imported.color, imported.point_size);
            if let Some(open) = self.point_clouds.last_mut() {
                open.id = crate::model::point_cloud::PointCloudId(target_id);
                open.state = open.state.clone().with_deferred(imported.deferred);
                open.state.set_provenance(imported.source_name, imported.source_format);
            }
        }

        self.mark_all_project_content_saved();

        userspace_log!(
            "{}",
            tr_format!(
                literal = "Opened project '%project_name%' from %source_name%",
                project_name = project_name,
                source_name = source_name
            )
        );
        self.invalidate_geometry();
        if should_fit {
            self.fit_view_to_extents();
        }
        self.persist_session();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_omf_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.spawn_job_reporting_progress(
            tr!(literal = "Importing project…"),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                let total = paths.len().max(1) as f32;
                let mut decoded = Vec::with_capacity(paths.len());
                for (index, path) in paths.into_iter().enumerate() {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| format!("{}.omf", tr!(literal = "Untitled")));
                    let phase = progress.phase(index as f32 / total, (index + 1) as f32 / total);
                    decoded.push((name.clone(), omf::from_bytes(&name, bytes, &phase)?));
                }
                Ok(decoded)
            },
            |app, result| match result {
                Ok(decoded) => app.apply_omf_bundles(decoded),
                Err(error) => userspace_warn!("{}", tr_format!(literal = "OMF import failed: %error%", error = format!("{error:#}"))),
            },
        );
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_omf_sources(&mut self) -> Result<()> {
        let files = self.take_web_import_files(crate::ui::state::DataMenu::Omf)?;
        self.spawn_job_reporting_progress(
            tr!(literal = "Importing project…"),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                let total = files.len().max(1) as f32;
                let mut decoded = Vec::with_capacity(files.len());
                for (index, file) in files.into_iter().enumerate() {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    let phase = progress.phase(index as f32 / total, (index + 1) as f32 / total);
                    let name = file.source.name;
                    decoded.push((name.clone(), omf::from_bytes(&name, file.bytes, &phase)?));
                }
                Ok(decoded)
            },
            |app, result| match result {
                Ok(decoded) => app.apply_omf_bundles(decoded),
                Err(error) => userspace_warn!("{}", tr_format!(literal = "OMF import failed: %error%", error = format!("{error:#}"))),
            },
        );
        Ok(())
    }

    fn apply_omf_bundles(&mut self, bundles: Vec<(String, ImportBundle)>) {
        if self.workspace.active_project().is_none() {
            userspace_warn!("{}", tr!(literal = "Create or open a project before merging data"));
            return;
        }
        let should_fit = !self.scene_has_renderables();
        let mut imported_items = 0usize;
        for (source_name, bundle) in bundles {
            let target_is_empty = self
                .workspace
                .active_project()
                .is_some_and(|project| project.project.document.layers().is_empty() && project.project.document.objects().is_empty())
                && self.triangulations.is_empty()
                && self.block_models.is_empty()
                && self.drill_holes.is_empty()
                && self.point_clouds.is_empty()
                && self.raster_textures.is_empty();
            let count = bundle.item_count();
            if count == 0 {
                userspace_warn!(
                    "{}",
                    tr_format!(literal = "Project '%source_name%' contains no supported data elements", source_name = source_name.clone())
                );
            }
            for warning in &bundle.warnings {
                userspace_warn!(
                    "{}",
                    tr_format!(literal = "%source_name%: %warning%", source_name = source_name.clone(), warning = warning.clone())
                );
            }
            let ImportBundle {
                project_name,
                coordinate_reference_system,
                units,
                origin,
                designs,
                triangulations,
                block_models,
                drill_holes,
                point_clouds,
                rasters,
                warnings: _,
            } = bundle;

            if let Some(project) = self.workspace.active_project_mut() {
                let target_crs = project.project.metadata.coordinate_reference_system.trim();
                let source_crs = coordinate_reference_system.trim();
                if target_is_empty && target_crs.is_empty() {
                    project.project.metadata.coordinate_reference_system = coordinate_reference_system.clone();
                } else if !target_crs.is_empty() && !source_crs.is_empty() && target_crs != source_crs {
                    userspace_warn!(
                        "{}",
                        tr_format!(
                            literal =
                                "%source_name%: coordinate reference system '%source_crs%' differs from project CRS '%target_crs%'; coordinates were merged without reprojection",
                            source_name = source_name.clone(),
                            source_crs = source_crs,
                            target_crs = target_crs
                        )
                    );
                }
                let target_units = project.project.metadata.units.trim();
                let source_units = units.trim();
                if target_is_empty && target_units.is_empty() {
                    project.project.metadata.units = units.clone();
                } else if !target_units.is_empty() && !source_units.is_empty() && !target_units.eq_ignore_ascii_case(source_units) {
                    userspace_warn!(
                        "{}",
                        tr_format!(
                            literal = "%source_name%: units '%source_units%' differ from project units '%target_units%'; coordinates were merged without conversion",
                            source_name = source_name.clone(),
                            source_units = source_units,
                            target_units = target_units
                        )
                    );
                }
            }
            if origin.iter().any(|value| *value != 0.0) {
                userspace_log!(
                    "{}",
                    tr_format!(
                        literal = "%source_name%: applied project origin %origin% before merge",
                        source_name = source_name.clone(),
                        origin = format!("{origin:?}")
                    )
                );
            }

            for design in designs {
                if let Some(project) = self.workspace.active_project_mut() {
                    project::merge_document_unique_layers(&mut project.project.document, &design.document);
                }
            }

            // Install rasters first so triangulation drape relationships can
            // be remapped from source IDs to newly allocated destination IDs.
            let mut raster_id_map = std::collections::HashMap::new();
            for mut raster in rasters {
                let preferred_id = raster.preferred_id;
                let visible = raster.is_loaded;
                let deferred = raster.deferred;
                raster.loaded.name = project::unique_item_name(raster.loaded.name, self.raster_textures.iter().map(|item| item.name.as_str()));
                self.add_loaded_raster(raster.loaded);
                if let Some(open) = self.raster_textures.last_mut() {
                    open.state.set_provenance(raster.source_name, raster.source_format);
                    open.state = open.state.clone().with_loaded(visible).with_deferred(deferred);
                    if let Some(preferred_id) = preferred_id {
                        raster_id_map.insert(preferred_id, open.id);
                    }
                }
            }

            for imported in triangulations {
                let raster_texture = imported.raster_texture_id.and_then(|id| raster_id_map.get(&id).copied());
                let LoadedTriangulation {
                    mut name,
                    path: _,
                    mesh,
                    spatial,
                    edges,
                    surface_face_order,
                } = imported.loaded;
                name = project::unique_item_name(name, self.triangulations.iter().map(|item| item.name.as_str()));
                let id = TriangulationId(self.next_triangulation_id);
                self.next_triangulation_id += 1;
                self.triangulations.push(OpenTriangulation {
                    id,
                    state: crate::model::project::ProjectItemState::dirty_with_format(imported.source_name, imported.source_format)
                        .with_loaded(imported.is_loaded)
                        .with_deferred(imported.deferred),
                    name,
                    mesh,
                    spatial,
                    edges,
                    surface_face_order,
                    color: imported.color,
                    line_color: imported.line_color,
                    line_weight: imported.line_weight,
                    raster_texture,
                    raster_opacity: imported.raster_opacity,
                });
                self.touch_active_project_content();
                if self.active_triangulation.is_none() {
                    self.active_triangulation = Some(id);
                }
            }

            for imported in block_models {
                let mut loaded = imported.loaded;
                loaded.name = project::unique_item_name(loaded.name, self.block_models.iter().map(|item| item.name.as_str()));
                self.add_loaded_block_model(loaded);
                if let Some(open) = self.block_models.last_mut() {
                    open.state.set_provenance(imported.source_name, imported.source_format);
                    open.state = open.state.clone().with_loaded(imported.is_loaded).with_deferred(imported.deferred);
                    open.color = imported.color;
                    open.slice = imported.slice;
                    if !open.state.loaded {
                        open.color_transfers.clear();
                    }
                    open.color_transfers.extend(imported.color_transfers);
                    open.hide_empty_color_values = imported.hide_empty_color_values;
                }
            }
            for imported in drill_holes {
                let mut loaded = imported.loaded;
                loaded.name = project::unique_item_name(loaded.name, self.drill_holes.iter().map(|item| item.name.as_str()));
                self.add_loaded_drill_holes(loaded);
                if let Some(open) = self.drill_holes.last_mut() {
                    open.state.set_provenance(imported.source_name, imported.source_format);
                    open.state = open.state.clone().with_loaded(imported.is_loaded).with_deferred(imported.deferred);
                    open.color = imported.color;
                }
            }
            for imported in point_clouds {
                let mut loaded = imported.loaded;
                loaded.name = project::unique_item_name(loaded.name, self.point_clouds.iter().map(|item| item.name.as_str()));
                self.add_loaded_point_cloud(loaded, imported.is_loaded, imported.color, imported.point_size);
                if let Some(open) = self.point_clouds.last_mut() {
                    open.state = open.state.clone().with_deferred(imported.deferred);
                }
                if let Some(open) = self.point_clouds.last_mut() {
                    open.state.set_provenance(imported.source_name, imported.source_format);
                }
            }
            imported_items += count;
            userspace_log!(
                "{}",
                tr_format!(
                    literal = "Imported project '%project_name%' from %source_name%: %count% top-level dataset(s)",
                    project_name = project_name,
                    source_name = source_name,
                    count = count
                )
            );
        }

        if imported_items > 0 {
            self.invalidate_geometry();
            if should_fit {
                self.fit_view_to_extents();
            }
            self.persist_session();
        }
    }

    pub(crate) fn choose_export_omf(&mut self) -> Result<()> {
        let snapshot = self.omf_export_snapshot()?;
        let default_name = format!("{}.omf", safe_stem(&snapshot.name));
        #[cfg(target_arch = "wasm32")]
        {
            self.spawn_job_reporting_progress(
                tr!(literal = "Encoding project…"),
                vec![crate::app::jobs::JobKey::Anonymous],
                move |cancel, progress| {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    omf::to_bytes(snapshot, &progress.phase(0.0, 1.0))
                },
                move |_app, result| match result {
                    Ok(bytes) => Self::trigger_browser_download(default_name, bytes, "application/octet-stream", "project"),
                    Err(error) => userspace_warn!("{}", tr_format!(literal = "OMF export failed: %error%", error = format!("{error:#}"))),
                },
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.spawn_file_dialog(async move {
            let path = rfd::AsyncFileDialog::new()
                .add_filter("Open Mining Format", &["omf"])
                .set_file_name(default_name)
                .save_file()
                .await?
                .path()
                .to_owned();
            Some(super::file::FileDialogAction::ExportOmf {
                snapshot: Box::new(snapshot),
                path,
            })
        });
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn export_omf_snapshot(&mut self, snapshot: ProjectSnapshot, mut path: PathBuf) {
        if path.extension().is_none() {
            path.set_extension("omf");
        }
        let display_path = path.clone();
        self.spawn_job_reporting_progress(
            tr_format!(
                literal = "Exporting %name%…",
                name = display_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| format!("{}.omf", tr!(literal = "Untitled")))
            ),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                omf::write_path(snapshot, &path, &progress.phase(0.0, 1.0))
            },
            move |_app, result| match result {
                Ok(()) => userspace_log!("{}", tr_format!(literal = "Exported project to %path%", path = display_path.display().to_string())),
                Err(error) => userspace_warn!("{}", tr_format!(literal = "OMF export failed: %error%", error = format!("{error:#}"))),
            },
        );
    }
}

fn allocate_item_id(preferred: Option<u64>, next: &mut u64, used: impl Iterator<Item = u64>) -> u64 {
    let used = used.collect::<std::collections::HashSet<_>>();
    if let Some(preferred) = preferred.filter(|id| !used.contains(id)) {
        *next = (*next).max(preferred.saturating_add(1));
        return preferred;
    }
    while used.contains(next) {
        *next = next.saturating_add(1);
    }
    let id = *next;
    *next = next.saturating_add(1);
    id
}

fn safe_stem(name: &str) -> String {
    let value = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let value = value.trim_matches('_');
    if value.is_empty() { "incline_design_project".to_owned() } else { value.to_owned() }
}
