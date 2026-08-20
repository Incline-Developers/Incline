use super::*;
use crate::model::progress::Phase;

/// Share of a triangulation load spent reading and parsing the file, before
/// its spatial index, edge list and draw order are built. Reading reports
/// exactly within its share; the stages after it are single calls, so they
/// step the bar at boundaries weighted by their typical relative cost.
const LOAD_READ_SHARE: f32 = 0.6;

/// Build the derived structures a loaded mesh needs for picking, wireframes
/// and draw order, stepping `progress` as each finishes.
pub(super) fn build_triangulation_indexes(
    mesh: &mesh_data::Triangulation,
    progress: &Phase,
) -> (std::sync::Arc<crate::model::spatial::TriangleBvh>, Vec<[u32; 2]>, std::sync::Arc<Vec<u32>>) {
    let spatial = std::sync::Arc::new(crate::model::spatial::TriangleBvh::build(mesh));
    progress.set_fraction(0.6);
    let edges = crate::model::triangulation::unique_edges(mesh);
    progress.set_fraction(0.8);
    let surface_face_order = std::sync::Arc::new(crate::model::triangulation::morton_surface_face_order(mesh));
    progress.finish();
    (spatial, edges, surface_face_order)
}

impl<'a> App<'a> {
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn open_triangulation_input(&mut self, input: crate::model::input::InputFile) {
        let name = input.source.name.clone();
        let display_path = self.allocate_browser_source_path(&name);
        self.spawn_job_reporting_progress(
            format!("Loading {name}"),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                let crate::model::input::InputFile { source, bytes, reservation } = input;
                let name = source.name;
                // The bytes are already in memory here, so the read share of
                // the bar covers parsing alone.
                let mesh = formats::read_mesh_bytes(&name, &bytes, &progress.phase(0.0, LOAD_READ_SHARE)).map_err(|error| anyhow::anyhow!("Failed to read {name}: {error}"))?;
                // Parsing owns the resulting mesh, so release the potentially
                // large browser source before building its derived structures.
                drop(bytes);
                drop(reservation);
                let (spatial, edges, surface_face_order) = build_triangulation_indexes(&mesh, &progress.phase(LOAD_READ_SHARE, 1.0));
                Ok(LoadedTriangulation {
                    path: std::path::PathBuf::from(&name),
                    name,
                    mesh: std::sync::Arc::new(mesh),
                    spatial,
                    edges,
                    surface_face_order,
                })
            },
            move |app, result| match result {
                Ok(loaded) => {
                    let should_fit = !app.scene_has_renderables();
                    let id = TriangulationId(app.next_triangulation_id);
                    app.next_triangulation_id += 1;
                    app.triangulations.push(OpenTriangulation {
                        id,
                        name: loaded.name,
                        path: display_path,
                        // Cleared by the browser-storage write started below.
                        is_saved: false,
                        mesh: loaded.mesh,
                        spatial: loaded.spatial,
                        edges: loaded.edges,
                        surface_face_order: loaded.surface_face_order,
                        visible: true,
                        color: DEFAULT_TRIANGULATION_COLOR,
                        line_color: [0.05, 0.08, 0.10, 1.0],
                        line_weight: Some(1.0),
                        raster_texture: None,
                        raster_opacity: 1.0,
                    });
                    if should_fit {
                        app.fit_view_to_extents();
                    }
                    // An imported mesh is stored like a generated one: the
                    // file the user picked is not somewhere the browser can go
                    // back to, so the record is the only copy that outlives an
                    // unload or the tab.
                    app.store_triangulation_in_browser(id);
                    app.invalidate_topology_bounds_and_redraw();
                }
                Err(error) => userspace_warn!("Failed to load triangulation: {error:#}"),
            },
        );
    }

    fn clear_triangulation_entity_state(&mut self, handle: crate::model::SceneEntityId) {
        self.editor.selected_handles.remove(&handle);
        self.editor.hidden_handles.remove(&handle);
        self.editor.frozen_handles.remove(&handle);
        self.editor.translucent_handles.remove(&handle);
    }

    pub(crate) fn open_triangulation_path(&mut self, path: &std::path::Path) -> Result<()> {
        if self.triangulation_excluded_paths.remove(path) {
            self.refresh_triangulation_dir_entries();
        }
        if self.triangulations.iter().find(|tri| tri.path.as_path() == path).is_some() {
            self.invalidate_geometry();
            return Ok(());
        }
        if self.pending_triangulation_loads.iter().any(|(_, pending_path, _, _)| pending_path == path) {
            return Ok(());
        }

        let name = file_name(path);
        let path = path.to_path_buf();
        let (ticket, progress) = self.begin_reported_task(format!("Loading {name}"));

        let (tx, rx) = std::sync::mpsc::channel();
        let console_report = crate::logging::retain_current_report();
        let worker_console_report = console_report.as_ref().map(crate::logging::ConsoleReportHandle::child);
        self.pending_triangulation_loads.push((ticket, path.clone(), rx, console_report));

        let window = self.window.clone();
        crate::app::jobs::spawn_pool_task(move || {
            let compute = || {
                crate::app::jobs::run_compute_catching_panic(|| -> Result<LoadedTriangulation> {
                    let mesh = formats::read_mesh_with_progress(&path, &progress.phase(0.0, LOAD_READ_SHARE))
                        .map_err(|err| anyhow::anyhow!("Failed to read {}: {err}", path.display()))?;
                    userspace_log!(
                        "Loaded triangulation '{}' ({}, {} vertices, {} faces)",
                        name,
                        path.display(),
                        mesh.vertex_count(),
                        mesh.face_count()
                    );
                    let (spatial, edges, surface_face_order) = build_triangulation_indexes(&mesh, &progress.phase(LOAD_READ_SHARE, 1.0));
                    Ok(LoadedTriangulation {
                        name,
                        path,
                        mesh: std::sync::Arc::new(mesh),
                        spatial,
                        edges,
                        surface_face_order,
                    })
                })
            };
            let result = if let Some(report) = worker_console_report.as_ref() {
                report.scope(compute)
            } else {
                compute()
            };
            let _ = tx.send(result);
            if let Some(w) = window {
                w.request_redraw();
            }
        });

        Ok(())
    }

    /// Drain any completed background triangulation loads and integrate their results.
    /// Called at the start of each frame so results appear in the same render they arrive.
    pub(crate) fn poll_triangulation_loads(&mut self) {
        let receivers = std::mem::take(&mut self.pending_triangulation_loads);
        let mut still_pending = Vec::new();

        for (ticket, path, rx, console_report) in receivers {
            let result = rx.try_recv();
            if matches!(result, Err(std::sync::mpsc::TryRecvError::Empty)) {
                still_pending.push((ticket, path, rx, console_report));
                continue;
            }
            let complete = || match result {
                Ok(Ok(loaded)) => {
                    let should_fit = !self.scene_has_renderables();
                    let id = TriangulationId(self.next_triangulation_id);
                    self.next_triangulation_id += 1;
                    self.triangulations.push(OpenTriangulation {
                        id,
                        name: loaded.name,
                        path: loaded.path,
                        is_saved: true,
                        mesh: loaded.mesh,
                        spatial: loaded.spatial,
                        edges: loaded.edges,
                        surface_face_order: loaded.surface_face_order,
                        visible: true,
                        color: DEFAULT_TRIANGULATION_COLOR,
                        line_color: [0.05, 0.08, 0.10, 1.0],
                        line_weight: Some(1.0),
                        raster_texture: None,
                        raster_opacity: 1.0,
                    });
                    if should_fit {
                        self.fit_view_to_extents();
                    }
                    self.finish_background_task(ticket, true);
                    self.persist_session();
                    // The mesh renders from triangulation_gpu's per-id cache, not
                    // the document scene. A full invalidate_geometry here would
                    // re-upload every vector object per loaded tri.
                    self.invalidate_topology_bounds_and_redraw();
                }
                Ok(Err(e)) => {
                    let message = format!("{e:#}");
                    userspace_warn!("Failed to load triangulation: {message}");
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => unreachable!(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    userspace_warn!("Triangulation load for {} ended without a result", path.display());
                    self.finish_background_task(ticket, false);
                }
            };
            if let Some(report) = console_report.as_ref() {
                report.scope(complete);
            } else {
                complete();
            }
            drop(console_report);
        }

        self.pending_triangulation_loads = still_pending;
    }

    fn cancel_pending_triangulation_loads(&mut self, mut matches: impl FnMut(&std::path::Path) -> bool) {
        let pending = std::mem::take(&mut self.pending_triangulation_loads);
        for (ticket, path, receiver, console_report) in pending {
            if matches(&path) {
                self.cancel_background_task(ticket);
                if let Some(report) = console_report {
                    report.cancel();
                }
            } else {
                self.pending_triangulation_loads.push((ticket, path, receiver, console_report));
            }
        }
    }

    pub(crate) fn activate_triangulation(&mut self, id: TriangulationId) {
        let Some(tri) = self.triangulations.iter().find(|tri| tri.id == id) else {
            return;
        };
        let handle = tri.entity_id();
        if self.active_triangulation == Some(id) && self.editor.selected_handles.contains(&handle) {
            self.active_triangulation = None;
            self.editor.selected_handles.remove(&handle);
            userspace_log!("Deselected triangulation '{}'", tri.name);
            self.request_topology_redraw();
            return;
        }
        let cleared_object_selection = self.editor.selected_handles.iter().any(|handle| matches!(handle, crate::model::SceneEntityId::Object(_)));
        self.active_triangulation = Some(id);
        self.editor.selected_handles.clear();
        self.editor.selected_handles.insert(handle);
        userspace_log!("Activated triangulation '{}'", tri.name);
        if cleared_object_selection {
            self.invalidate_geometry();
        } else {
            self.request_topology_redraw();
        }
    }

    pub(crate) fn toggle_triangulation_visible(&mut self, id: TriangulationId) {
        let Some(tri) = self.triangulations.iter_mut().find(|tri| tri.id == id) else {
            return;
        };
        let handle = tri.entity_id();
        let was_visible = tri.visible && !self.editor.hidden_handles.contains(&handle);
        tri.visible = !was_visible;
        if tri.visible {
            // A toolbar hide is stored on the scene entity, while the explorer
            // stores visibility on the triangulation. Showing from either UI
            // must clear both sources of hidden state.
            self.editor.hidden_handles.remove(&handle);
        }
        let action = if tri.visible { "Shown" } else { "Hidden" };
        userspace_log!("{} triangulation '{}'", action, tri.name);
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn close_triangulation(&mut self, id: TriangulationId) {
        let Some(index) = self.triangulations.iter().position(|tri| tri.id == id) else {
            return;
        };
        let tri = self.triangulations.remove(index);
        #[cfg(target_arch = "wasm32")]
        // A stored triangulation keeps both its record and its reserved
        // display name across an unload, so the explorer can still list it and
        // load it again; anything else was a one-off browser import.
        if !self.mark_browser_triangulation_unloaded(id) {
            self.browser_source_paths.remove(&tri.path);
        }
        let handle = tri.entity_id();
        self.clear_triangulation_entity_state(handle);
        self.clear_dialog_refs_to_triangulation(id);
        // In-flight jobs derived from this triangulation would otherwise
        // finish later and apply against a closed source.
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::Triangulation(id));
        if self.active_triangulation == Some(id) {
            self.active_triangulation = None;
        }
        userspace_log!("Closed triangulation '{}'", tri.name);
        self.invalidate_topology_bounds_and_redraw();
        self.persist_session();
    }

    /// Unload a loaded mesh (if any) and drop the path from the individual-file
    /// tracker. Desktop-only: the browser's equivalent, `delete_browser_triangulation`,
    /// removes the stored record instead of forgetting a file.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn remove_triangulation(&mut self, path: PathBuf) {
        self.cancel_pending_triangulation_loads(|pending_path| pending_path == path);
        if let Some(idx) = self.triangulations.iter().position(|t| t.path == path) {
            let tri = self.triangulations.remove(idx);
            let handle = tri.entity_id();
            self.clear_triangulation_entity_state(handle);
            self.clear_dialog_refs_to_triangulation(tri.id);
            let removed_id = tri.id;
            self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::Triangulation(removed_id));
            if self.active_triangulation == Some(tri.id) {
                self.active_triangulation = None;
            }
            self.invalidate_topology_bounds_and_redraw();
            userspace_log!("Removed triangulation '{}'", tri.name);
        }
        self.triangulation_files.retain(|f| f != &path);
        if self.triangulation_dirs.iter().any(|dir| path.parent() == Some(dir.as_path())) {
            self.triangulation_excluded_paths.insert(path.clone());
            self.refresh_triangulation_dir_entries();
        }
        userspace_log!(
            "Removed triangulation '{}' from file tracker",
            path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
        );
        self.persist_session();
    }

    fn clear_dialog_refs_to_triangulation(&mut self, id: TriangulationId) {
        // A picker may have been waiting for this (or another) loaded surface;
        // cancel it before changing the available target set.
        self.editor.triangulation_pick_target = None;
        self.editor.viewport_pick_hover_label = None;
        self.editor.tri_hover_handles.clear();
        if self.editor.tri_cut_poly_tri_id == Some(id) {
            self.editor.tri_cut_poly_tri_id = None;
            self.editor.tri_cut_poly_open = false;
            self.editor.tri_cut_poly_awaiting_pick = false;
        }
        if self.editor.tri_cut_z_tri_id == Some(id) {
            self.editor.tri_cut_z_tri_id = None;
            self.editor.tri_cut_z_open = false;
        }
        if self.editor.tri_cut_surface_target_id == Some(id) || self.editor.tri_cut_surface_reference_id == Some(id) {
            self.editor.tri_cut_surface_target_id = None;
            self.editor.tri_cut_surface_reference_id = None;
            self.editor.tri_cut_surface_open = false;
        }
        if self.editor.tri_cut_pitshell_topology_id == Some(id) || self.editor.tri_cut_pitshell_pitshell_id == Some(id) {
            self.editor.tri_cut_pitshell_topology_id = None;
            self.editor.tri_cut_pitshell_pitshell_id = None;
            self.editor.tri_cut_pitshell_open = false;
        }
        if self.editor.tri_include_solid_topology_id == Some(id) || self.editor.tri_include_solid_shape_id == Some(id) {
            self.editor.tri_include_solid_topology_id = None;
            self.editor.tri_include_solid_shape_id = None;
            self.editor.tri_include_solid_open = false;
        }
        if self.editor.tri_contour_tri_id == Some(id) {
            self.editor.tri_contour_tri_id = None;
            self.editor.tri_contour_open = false;
        }
    }

    /// Unload every mesh whose path lives in `dir`, and drop the dir from the tracked list.
    pub(crate) fn remove_triangulation_folder(&mut self, dir: PathBuf) {
        self.cancel_pending_triangulation_loads(|pending_path| pending_path.parent() == Some(dir.as_path()));
        let to_remove: Vec<_> = self.triangulations.iter().filter(|t| t.path.parent() == Some(dir.as_path())).map(|t| t.id).collect();
        for id in to_remove {
            if let Some(idx) = self.triangulations.iter().position(|t| t.id == id) {
                let tri = self.triangulations.remove(idx);
                let handle = tri.entity_id();
                self.clear_triangulation_entity_state(handle);
                if self.active_triangulation == Some(tri.id) {
                    self.active_triangulation = None;
                }
            }
        }
        self.triangulation_dirs.retain(|d| d != &dir);
        self.triangulation_excluded_paths.retain(|path| path.parent() != Some(dir.as_path()));
        self.triangulation_dir_entries.remove(&dir);
        self.invalidate_topology_bounds_and_redraw();
        userspace_log!("Removed triangulation folder '{}'", dir.display());
        self.persist_session();
    }

    pub(crate) fn load_all_triangulations_in_folder(&mut self, dir: PathBuf) -> Result<()> {
        let paths: Vec<PathBuf> = self.triangulation_dir_entries.get(&dir).cloned().unwrap_or_default();
        let count = paths.len();
        for path in paths {
            self.open_triangulation_path(&path)?;
        }
        userspace_log!("Loaded {} triangulation(s) from folder {}", count, dir.display());
        Ok(())
    }

    pub(crate) fn close_all_triangulations_in_folder(&mut self, dir: PathBuf) {
        // Cancel any loads that haven't completed yet so they don't reappear after unload.
        self.cancel_pending_triangulation_loads(|pending_path| pending_path.parent() == Some(dir.as_path()));
        let ids: Vec<TriangulationId> = self.triangulations.iter().filter(|t| t.path.parent() == Some(dir.as_path())).map(|t| t.id).collect();
        let count = ids.len();
        if count == 0 {
            return;
        }
        let ids: HashSet<_> = ids.into_iter().collect();
        self.cancel_jobs(|key| matches!(key, crate::app::jobs::JobKey::Triangulation(id) if ids.contains(id)));
        let loaded = std::mem::take(&mut self.triangulations);
        for triangulation in loaded {
            if ids.contains(&triangulation.id) {
                self.clear_triangulation_entity_state(triangulation.entity_id());
                self.clear_dialog_refs_to_triangulation(triangulation.id);
            } else {
                self.triangulations.push(triangulation);
            }
        }
        if self.active_triangulation.is_some_and(|id| ids.contains(&id)) {
            self.active_triangulation = None;
        }
        self.invalidate_topology_bounds_and_redraw();
        self.persist_session();
        userspace_log!("Closed {} triangulation(s) from folder {}", count, dir.display());
    }

    pub(crate) fn set_triangulation_color(&mut self, tri_id: TriangulationId, new_color: [f32; 4]) {
        if let Some(tri) = self.triangulations.iter_mut().find(|t| t.id == tri_id) {
            tri.color = new_color;
        }
        userspace_log!("Set triangulation {:?} color to {:?}", tri_id, new_color);
        self.request_topology_redraw();
    }
    /// Apply the result of a background job that produces one triangulation
    /// with a single completion log line: log + insert on success, warn on
    /// failure. Shared by the backgrounded cut/create paths.
    pub(crate) fn apply_generated_triangulation_job(&mut self, result: Result<crate::model::triangulation::GeneratedTriangulationLog>) {
        match result {
            Ok(log) => {
                userspace_log!("{}", log.message);
                self.insert_generated_triangulation(log.generated);
            }
            Err(err) => {
                let message = format!("{err:#}");
                userspace_warn!("Triangulation operation failed: {message}");
            }
        }
    }

    /// UI-thread half of a generated-triangulation apply: register a
    /// pre-built mesh/BVH/edge bundle as a new `OpenTriangulation` and select
    /// it. The heavy build (mesh assembly + BVH) is done by
    /// `build_generated_triangulation`, which can run on a worker thread.
    pub(crate) fn insert_generated_triangulation(&mut self, built: crate::model::triangulation::GeneratedTriangulation) {
        let crate::model::triangulation::GeneratedTriangulation {
            name,
            mesh,
            spatial,
            edges,
            surface_face_order,
            surface_type,
        } = built;
        let vertex_count = mesh.vertex_count();
        let face_count = mesh.face_count();

        let id = TriangulationId(self.next_triangulation_id);
        self.next_triangulation_id += 1;

        // The browser lists a generated mesh alongside its stored siblings, so
        // it needs a reserved display name rather than an internal-only one.
        #[cfg(target_arch = "wasm32")]
        let synthetic_path = self.allocate_browser_source_path(&name);
        #[cfg(not(target_arch = "wasm32"))]
        let synthetic_path = PathBuf::from(format!("generated::{}::{name}", id.0));

        let cleared_object_selection = self.editor.selected_handles.iter().any(|handle| matches!(handle, crate::model::SceneEntityId::Object(_)));
        self.triangulations.push(OpenTriangulation {
            id,
            name: name.clone(),
            path: synthetic_path,
            is_saved: false,
            mesh,
            spatial,
            edges,
            surface_face_order,
            visible: true,
            color: DEFAULT_TRIANGULATION_COLOR,
            line_color: [0.05, 0.08, 0.10, 1.0],
            line_weight: Some(1.0),
            raster_texture: None,
            raster_opacity: 1.0,
        });
        self.active_triangulation = Some(id);
        self.editor.selected_handles.clear();
        self.editor.selected_handles.insert(crate::model::SceneEntityId::Triangulation(id));
        // The browser has nowhere to defer an unsaved mesh to, so it goes
        // straight into IndexedDB and is marked saved when the write lands.
        #[cfg(target_arch = "wasm32")]
        self.store_triangulation_in_browser(id);
        userspace_log!(
            "Created triangulation '{name}' ({vertex_count} vertices, {face_count} faces) from surface type {:?}",
            surface_type
        );
        if cleared_object_selection {
            self.invalidate_geometry();
        } else {
            self.invalidate_topology_bounds_and_redraw();
        }
    }
}

/// Worker-thread half of a generated-triangulation apply: assemble the mesh,
/// build its BVH and edge list. Pure (no `App`), so it can run off the UI
/// thread; the result is handed to `insert_generated_triangulation`.
pub(crate) fn build_generated_triangulation(
    name: String,
    tri_vertices: Vec<mesh_data::Vertex>,
    tri_faces: Vec<[u32; 3]>,
    surface_type: TriSurfaceType,
    build_edges: impl FnOnce(&mesh_data::Triangulation) -> Vec<[u32; 2]>,
) -> Result<crate::model::triangulation::GeneratedTriangulation> {
    if tri_faces.is_empty() {
        anyhow::bail!("Triangulation produced no faces");
    }
    let mesh = mesh_data::Triangulation::from_vertices_and_faces(tri_vertices, tri_faces)?;
    let spatial = std::sync::Arc::new(crate::model::spatial::TriangleBvh::build(&mesh));
    let edges = build_edges(&mesh);
    let surface_face_order = std::sync::Arc::new(crate::model::triangulation::morton_surface_face_order(&mesh));
    Ok(crate::model::triangulation::GeneratedTriangulation {
        name,
        mesh: std::sync::Arc::new(mesh),
        spatial,
        edges,
        surface_face_order,
        surface_type,
    })
}
