//! Whole-project Open Mining Format import/export commands.

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Context;
use anyhow::Result;

use crate::{
    app::App,
    model::{
        formats::omf::{self, ExportSnapshot, ImportBundle},
        pidb,
        triangulation::{LoadedTriangulation, OpenTriangulation, TriangulationId},
    },
    userspace_log, userspace_warn,
};

impl<'a> App<'a> {
    fn omf_export_snapshot(&mut self) -> Result<ExportSnapshot> {
        if self.has_pending_move_delta() {
            self.commit_pending_move();
        }
        for project_index in 0..self.workspace.projects.len() {
            self.ensure_project_has_no_pending_text_edit(project_index)?;
        }
        let name = self
            .workspace
            .active_project()
            .map(|project| project.pidb.metadata.name.trim_end_matches(".pidb").to_owned())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Incline project".to_owned());
        let snapshot = ExportSnapshot {
            name,
            designs: self.workspace.projects.iter().map(|project| project.pidb.clone()).collect(),
            triangulations: self.triangulations.clone(),
            block_models: self.block_models.clone(),
            drill_holes: self.drill_holes.clone(),
            point_clouds: self.point_clouds.clone(),
            rasters: self.raster_textures.clone(),
        };
        if snapshot.is_empty() {
            anyhow::bail!("There is no open Incline data to export");
        }
        Ok(snapshot)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_omf_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.spawn_job_reporting_progress(
            "Importing OMF project…",
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                let total = paths.len().max(1) as f32;
                let mut decoded = Vec::with_capacity(paths.len());
                for (index, path) in paths.into_iter().enumerate() {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
                    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("project.omf").to_owned();
                    let phase = progress.phase(index as f32 / total, (index + 1) as f32 / total);
                    decoded.push((name.clone(), omf::from_bytes(&name, bytes, &phase)?));
                }
                Ok(decoded)
            },
            |app, result| match result {
                Ok(decoded) => app.apply_omf_bundles(decoded),
                Err(error) => userspace_warn!("OMF import failed: {error:#}"),
            },
        );
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_omf_sources(&mut self) -> Result<()> {
        let files = self.take_web_import_files(crate::ui::state::DataMenu::Omf)?;
        self.spawn_job_reporting_progress(
            "Importing OMF project…",
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
                Err(error) => userspace_warn!("OMF import failed: {error:#}"),
            },
        );
        Ok(())
    }

    fn apply_omf_bundles(&mut self, bundles: Vec<(String, ImportBundle)>) {
        let should_fit = !self.scene_has_renderables();
        let mut imported_items = 0usize;
        let mut first_project_index = None;
        for (source_name, bundle) in bundles {
            let count = bundle.item_count();
            if count == 0 {
                userspace_warn!("OMF project '{source_name}' contains no supported data elements");
            }
            for warning in &bundle.warnings {
                userspace_warn!("{source_name}: {warning}");
            }
            let ImportBundle {
                project_name,
                designs,
                triangulations,
                block_models,
                drill_holes,
                point_clouds,
                rasters,
                warnings: _,
            } = bundle;

            for design in designs {
                match pidb::open_project(None, design) {
                    Ok(project) => {
                        let index = self.workspace.add_inactive(project);
                        let layer_ids = self.workspace.projects[index].pidb.document.layers().iter().map(|layer| layer.id).collect::<Vec<_>>();
                        self.workspace.projects[index].loaded_layers.extend(layer_ids);
                        first_project_index.get_or_insert(index);
                    }
                    Err(error) => userspace_warn!("Could not import an OMF design database from {source_name}: {error:#}"),
                }
            }

            for imported in triangulations {
                let LoadedTriangulation {
                    name,
                    path,
                    mesh,
                    spatial,
                    edges,
                    surface_face_order,
                } = imported.loaded;
                let id = TriangulationId(self.next_triangulation_id);
                self.next_triangulation_id += 1;
                self.triangulations.push(OpenTriangulation {
                    id,
                    name,
                    path,
                    is_saved: false,
                    mesh,
                    spatial,
                    edges,
                    surface_face_order,
                    visible: imported.visible,
                    color: imported.color,
                    line_color: imported.line_color,
                    line_weight: imported.line_weight,
                    raster_texture: None,
                    raster_opacity: imported.raster_opacity,
                });
                if self.active_triangulation.is_none() {
                    self.active_triangulation = Some(id);
                }
            }

            for imported in block_models {
                self.add_loaded_block_model(imported.loaded);
                if let Some(open) = self.block_models.last_mut() {
                    open.visible = imported.visible;
                    open.color = imported.color;
                }
            }
            for imported in drill_holes {
                self.add_loaded_drill_holes(imported.loaded);
                if let Some(open) = self.drill_holes.last_mut() {
                    open.visible = imported.visible;
                }
            }
            for imported in point_clouds {
                self.add_loaded_point_cloud(imported.loaded, imported.visible, imported.color, imported.point_size);
            }
            for raster in rasters {
                self.add_loaded_raster(raster);
            }
            imported_items += count;
            userspace_log!("Imported OMF project '{project_name}' from {source_name}: {count} top-level dataset(s)");
        }

        if self.workspace.active_index.is_none()
            && let Some(index) = first_project_index
        {
            self.activate_project_index(index);
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
                "Encoding OMF project…",
                vec![crate::app::jobs::JobKey::Anonymous],
                move |cancel, progress| {
                    if cancel.is_cancelled() {
                        anyhow::bail!("Cancelled");
                    }
                    omf::to_bytes(snapshot, &progress.phase(0.0, 1.0))
                },
                move |_app, result| match result {
                    Ok(bytes) => Self::trigger_browser_download(default_name, bytes, "application/octet-stream", "OMF project"),
                    Err(error) => userspace_warn!("OMF export failed: {error:#}"),
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
            Some(super::file::FileDialogAction::ExportOmf { snapshot, path })
        });
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn export_omf_snapshot(&mut self, snapshot: ExportSnapshot, mut path: PathBuf) {
        if path.extension().is_none() {
            path.set_extension("omf");
        }
        let display_path = path.clone();
        self.spawn_job_reporting_progress(
            format!("Exporting {}…", display_path.file_name().and_then(|name| name.to_str()).unwrap_or("project.omf")),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                omf::write_path(snapshot, &path, &progress.phase(0.0, 1.0))
            },
            move |_app, result| match result {
                Ok(()) => userspace_log!("Exported OMF project to {}", display_path.display()),
                Err(error) => userspace_warn!("OMF export failed: {error:#}"),
            },
        );
    }
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
    if value.is_empty() { "incline_project".to_owned() } else { value.to_owned() }
}
