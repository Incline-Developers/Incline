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
        let source_name = input.source.name.clone();
        self.spawn_job_reporting_progress(
            crate::i18n::tr_format!(literal = "Loading %name%", name = &source_name),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                let crate::model::input::InputFile { source, bytes, reservation } = input;
                let source_name = source.name;
                let name = crate::model::project::imported_item_name(std::path::Path::new(&source_name), &crate::i18n::tr!(literal = "Triangulation"));
                // The bytes are already in memory here, so the read share of
                // the bar covers parsing alone.
                let mesh = formats::read_mesh_bytes(&source_name, &bytes, &progress.phase(0.0, LOAD_READ_SHARE))
                    .map_err(|error| anyhow::anyhow!("Failed to read {source_name}: {error}"))?;
                // Parsing owns the resulting mesh, so release the potentially
                // large browser source before building its derived structures.
                drop(bytes);
                drop(reservation);
                let (spatial, edges, surface_face_order) = build_triangulation_indexes(&mesh, &progress.phase(LOAD_READ_SHARE, 1.0));
                Ok(LoadedTriangulation {
                    path: std::path::PathBuf::from(&source_name),
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
                    let name = crate::model::project::unique_item_name(loaded.name, app.triangulations.iter().map(|item| item.name.as_str()));
                    app.triangulations.push(OpenTriangulation {
                        id,
                        state: crate::model::project::ProjectItemState::dirty(loaded.path.file_name().map(|name| name.to_string_lossy().into_owned())),
                        name,
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
                    app.touch_active_project_content();
                    if should_fit {
                        app.fit_view_to_extents();
                    }
                    app.invalidate_topology_bounds_and_redraw();
                }
                Err(error) => userspace_warn!("{}", tr_format!(literal = "Failed to load triangulation: %error%", error = format!("{error:#}"))),
            },
        );
    }

    fn clear_triangulation_entity_state(&mut self, handle: crate::model::SceneEntityId) {
        self.editor.selected_handles.remove(&handle);
        self.editor.hidden_handles.remove(&handle);
        self.editor.explicitly_frozen.remove(&handle);
        self.editor.frozen_handles.remove(&handle);
        self.editor.translucent_handles.remove(&handle);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_triangulation_path(&mut self, path: &std::path::Path) -> Result<()> {
        if self.pending_triangulation_loads.iter().any(|(_, pending_path, _, _)| pending_path == path) {
            return Ok(());
        }

        let source_name = file_name(path);
        let name = crate::model::project::imported_item_name(path, &crate::i18n::tr!(literal = "Triangulation"));
        let path = path.to_path_buf();
        let (ticket, progress) = self.begin_reported_task(tr_format!(literal = "Loading %name%", name = &source_name));

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
                        "{}",
                        tr_format!(
                            literal = "Loaded triangulation '%name%' (%path%, %vertex_count% vertices, %face_count% faces)",
                            name = name,
                            path = path.display(),
                            vertex_count = mesh.vertex_count(),
                            face_count = mesh.face_count()
                        )
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
                    let name = crate::model::project::unique_item_name(loaded.name, self.triangulations.iter().map(|item| item.name.as_str()));
                    self.triangulations.push(OpenTriangulation {
                        id,
                        state: crate::model::project::ProjectItemState::dirty(loaded.path.file_name().map(|name| name.to_string_lossy().into_owned())),
                        name,
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
                    self.touch_active_project_content();
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
                    userspace_warn!("{}", tr_format!(literal = "Failed to load triangulation: %message%", message = message));
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => unreachable!(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    userspace_warn!("{}", tr_format!(literal = "Triangulation load for %path% ended without a result", path = path.display()));
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

    pub(crate) fn activate_triangulation(&mut self, id: TriangulationId) {
        let Some(tri) = self.triangulations.iter().find(|tri| tri.id == id) else {
            return;
        };
        let handle = tri.entity_id();
        if self.active_triangulation == Some(id) && self.editor.selected_handles.contains(&handle) {
            self.active_triangulation = None;
            self.editor.selected_handles.remove(&handle);
            userspace_log!("{}", tr_format!(literal = "Deselected triangulation '%name%'", name = tri.name));
            self.request_topology_redraw();
            return;
        }
        let cleared_object_selection = self.editor.selected_handles.iter().any(|handle| matches!(handle, crate::model::SceneEntityId::Object(_)));
        self.active_triangulation = Some(id);
        self.editor.selected_handles.clear();
        self.editor.selected_handles.insert(handle);
        userspace_log!("{}", tr_format!(literal = "Activated triangulation '%name%'", name = tri.name));
        if cleared_object_selection {
            self.invalidate_geometry();
        } else {
            self.request_topology_redraw();
        }
    }

    pub(crate) fn toggle_triangulation_visible(&mut self, id: TriangulationId) {
        let Some(tri) = self.triangulations.iter().find(|tri| tri.id == id) else {
            return;
        };
        let handle = tri.entity_id();
        let name = tri.name.clone();
        let visible = !(tri.visible && !self.editor.hidden_handles.contains(&handle));
        if visible {
            // A toolbar hide is stored on the scene entity, while the explorer
            // stores visibility on the triangulation. Showing from either UI
            // must clear both sources of hidden state.
            self.editor.hidden_handles.remove(&handle);
        }
        let message = if visible {
            tr_format!(literal = "Shown triangulation '%name%'", name = name)
        } else {
            tr_format!(literal = "Hidden triangulation '%name%'", name = name)
        };
        let style = ItemStyle::of_triangulation(tri).with_visible(visible);
        userspace_log!("{}", message);
        self.set_item_style(ItemRef::Triangulation(id), style);
    }

    pub(crate) fn close_triangulation(&mut self, id: TriangulationId) {
        let Some(tri) = self.triangulations.iter_mut().find(|tri| tri.id == id) else {
            return;
        };
        tri.state.loaded = false;
        let name = tri.name.clone();
        let handle = tri.entity_id();
        self.clear_triangulation_entity_state(handle);
        self.clear_dialog_refs_to_triangulation(id);
        // In-flight jobs derived from this triangulation would otherwise
        // finish later and apply against a closed source.
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::Triangulation(id));
        if self.active_triangulation == Some(id) {
            self.active_triangulation = None;
        }
        userspace_log!("{}", tr_format!(literal = "Unloaded triangulation '%name%'", name = name));
        self.invalidate_topology_bounds_and_redraw();
        self.persist_session();
    }

    /// Delete an embedded mesh from the active project.
    pub(crate) fn remove_triangulation(&mut self, id: TriangulationId) {
        let Some(name) = self.triangulations.iter().find(|triangulation| triangulation.id == id).map(|tri| tri.name.clone()) else {
            return;
        };
        self.clear_triangulation_entity_state(SceneEntityId::Triangulation(id));
        self.clear_dialog_refs_to_triangulation(id);
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::Triangulation(id));
        if self.active_triangulation == Some(id) {
            self.active_triangulation = None;
        }
        // Rasters draped onto this surface are a property of the surface, so
        // they come back with it; nothing else references it.
        self.delete_project_item(ItemRef::Triangulation(id));
        userspace_log!("{}", tr_format!(literal = "Deleted triangulation '%name%' from project", name = name));
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

    pub(crate) fn set_triangulation_color(&mut self, tri_id: TriangulationId, new_color: [f32; 4]) {
        let item = ItemRef::Triangulation(tri_id);
        if let Some(style) = self.item_style(item) {
            self.set_item_style(item, style.with_color(new_color));
        }
        self.log_when_gesture_ends(tr_format!(
            literal = "Set triangulation %tri_id% color to %color%",
            tri_id = format!("{tri_id:?}"),
            color = format!("{new_color:?}")
        ));
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
                userspace_warn!("{}", tr_format!(literal = "Triangulation operation failed: %message%", message = message));
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
        let name = crate::model::project::unique_item_name(name, self.triangulations.iter().map(|item| item.name.as_str()));

        let cleared_object_selection = self.editor.selected_handles.iter().any(|handle| matches!(handle, crate::model::SceneEntityId::Object(_)));
        self.triangulations.push(OpenTriangulation {
            id,
            state: crate::model::project::ProjectItemState::dirty(None),
            name: name.clone(),
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
        self.touch_active_project_content();
        self.active_triangulation = Some(id);
        self.editor.selected_handles.clear();
        self.editor.selected_handles.insert(crate::model::SceneEntityId::Triangulation(id));
        userspace_log!(
            "{}",
            tr_format!(
                literal = "Created triangulation '%name%' (%vertex_count% vertices, %face_count% faces) from surface type %surface_type%",
                name = name,
                vertex_count = vertex_count,
                face_count = face_count,
                surface_type = format!("{surface_type:?}")
            )
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
