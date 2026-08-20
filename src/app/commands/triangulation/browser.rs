//! Browser persistence for triangulations.
//!
//! Browser storage stands in for the desktop build's mesh files. The desktop
//! build can leave a generated mesh unsaved in memory until the user picks a
//! path; the browser has no path to offer, so a generated triangulation is
//! encoded to PLY and written to IndexedDB as soon as it exists. From then
//! on it behaves like a file-backed mesh on desktop: the explorer lists it
//! whether or not it is loaded, unloading frees the memory and leaves the
//! record, loading reads it back, and only an explicit Delete drops the
//! record. Getting a copy onto the user's disk is the separate Download action.

use std::path::Path;

use super::*;
use crate::{
    app::web_storage::{BrowserAssetRecord, BrowserAssetSummary, BrowserStore},
    model::progress::Phase,
};

/// Slack added to the encode-size estimate for headers and the bookkeeping the
/// writer allocates around the vertex/face payload.
const ENCODE_ESTIMATE_SLACK: usize = 1024 * 1024;

/// Share of a browser load spent decoding the stored PLY payload, before
/// the spatial index, edge list and draw order are built from it.
const DECODE_SHARE: f32 = 0.6;

/// One triangulation held in browser storage, loaded or not. The explorer
/// keys these by `path` — a display-only name reserved through
/// `allocate_browser_source_path` — exactly as it keys desktop meshes by their
/// file path.
pub(crate) struct BrowserTriangulationEntry {
    pub(crate) record_id: String,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    /// Set while the mesh is in memory.
    pub(crate) loaded: Option<TriangulationId>,
}

impl<'a> App<'a> {
    /// Encode `id`'s mesh and write it to IndexedDB, registering it in the
    /// explorer's stored list. Marks the triangulation saved once the write
    /// completes (see `AppEvent::BrowserTriangulationStored`).
    pub(crate) fn store_triangulation_in_browser(&mut self, id: TriangulationId) {
        let Some(triangulation) = self.triangulations.iter().find(|triangulation| triangulation.id == id) else {
            return;
        };
        let name = triangulation.name.clone();
        let path = triangulation.path.clone();
        let mesh = std::sync::Arc::clone(&triangulation.mesh);

        // The encoded bytes exist twice at the hand-off: once in the worker's
        // buffer and once in the JS-owned copy IndexedDB clones from. Reserve
        // both up front so an oversized mesh reports a budget error instead of
        // aborting the whole module on an allocation failure.
        let estimate = mesh
            .vertex_count()
            .checked_mul(24)
            .and_then(|vertices| vertices.checked_add(mesh.face_count().checked_mul(12)?))
            .and_then(|payload| payload.checked_add(ENCODE_ESTIMATE_SLACK))
            .and_then(|bytes| bytes.checked_mul(2));
        let reservation = match estimate {
            Some(bytes) => crate::app::memory::reserve(bytes, "Saving a triangulation to browser storage"),
            None => Err("the triangulation is too large to encode".to_owned()),
        };
        let reservation = match reservation {
            Ok(reservation) => reservation,
            Err(error) => {
                userspace_warn!("Could not save triangulation '{name}' to browser storage: {error}");
                return;
            }
        };

        let Some(proxy) = self.web_event_loop_proxy.clone() else {
            userspace_warn!("Browser storage is unavailable; '{name}' exists only until this tab is closed");
            return;
        };

        // Re-storing an already-stored triangulation must overwrite its record
        // rather than leave the previous copy behind under a stale key.
        let record_id = match self.browser_triangulations.iter().find(|entry| entry.loaded == Some(id)) {
            Some(entry) => entry.record_id.clone(),
            None => {
                let record_id = uuid::Uuid::new_v4().to_string();
                self.browser_triangulations.push(BrowserTriangulationEntry {
                    record_id: record_id.clone(),
                    name: name.clone(),
                    path: path.clone(),
                    loaded: Some(id),
                });
                if !self.triangulation_files.contains(&path) {
                    self.triangulation_files.push(path);
                }
                record_id
            }
        };

        self.spawn_job(
            format!("Saving {name} to browser storage"),
            vec![crate::app::jobs::JobKey::Triangulation(id)],
            move |cancel| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                formats::write_mesh_bytes(&mesh, formats::MeshFormat::Ply).map_err(|error| anyhow::anyhow!("Could not encode triangulation: {error}"))
            },
            move |_app, result| match result {
                Ok(bytes) => {
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = crate::app::web_storage::put_asset(BrowserStore::Triangulations, &record_id, &name, &bytes).await;
                        drop(reservation);
                        let _ = proxy.send_event(crate::app::AppEvent::BrowserTriangulationStored { id, record_id, name, result });
                    });
                }
                Err(error) => userspace_warn!("Could not save triangulation to browser storage: {error:#}"),
            },
        );
    }

    /// Register every stored triangulation in the explorer, unloaded. Meshes
    /// are read on demand, so a reload does not decode every stored surface.
    pub(crate) fn catalogue_browser_triangulations(&mut self, summaries: Vec<BrowserAssetSummary>) {
        let restored = summaries.len();
        for summary in summaries {
            if self.browser_triangulations.iter().any(|entry| entry.record_id == summary.id) {
                continue;
            }
            let path = self.allocate_browser_source_path(&summary.name);
            self.browser_triangulations.push(BrowserTriangulationEntry {
                record_id: summary.id,
                name: summary.name,
                path: path.clone(),
                loaded: None,
            });
            if !self.triangulation_files.contains(&path) {
                self.triangulation_files.push(path);
            }
        }
        if restored > 0 {
            userspace_log!("Found {restored} triangulation(s) in browser storage");
            self.redraw_requested = true;
        }
    }

    /// Read a stored triangulation back into memory. The fetch runs on the
    /// browser's main thread; decoding and index building happen in a job.
    pub(crate) fn load_browser_triangulation(&mut self, path: &Path) {
        let Some(entry) = self.browser_triangulations.iter().find(|entry| entry.path == path) else {
            userspace_warn!("That triangulation is no longer in browser storage");
            return;
        };
        if entry.loaded.is_some() {
            return;
        }
        let record_id = entry.record_id.clone();
        if self.browser_triangulation_loads.contains(&record_id) {
            return;
        }
        let Some(proxy) = self.web_event_loop_proxy.clone() else {
            userspace_warn!("Browser storage is unavailable");
            return;
        };
        self.browser_triangulation_loads.insert(record_id.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::app::web_storage::get_asset(BrowserStore::Triangulations, &record_id).await;
            let _ = proxy.send_event(crate::app::AppEvent::BrowserTriangulationFetched { record_id, result });
        });
    }

    /// Apply a fetched record: decode it off the UI thread and register the
    /// mesh against the stored entry it came from.
    pub(crate) fn apply_fetched_browser_triangulation(&mut self, record_id: String, result: Result<Option<BrowserAssetRecord>, String>) {
        self.browser_triangulation_loads.remove(&record_id);
        let record = match result {
            Ok(Some(record)) => record,
            Ok(None) => {
                userspace_warn!("That triangulation is no longer in browser storage");
                self.forget_browser_triangulation(&record_id);
                return;
            }
            Err(error) => {
                userspace_warn!("Could not read triangulation from browser storage: {error}");
                return;
            }
        };
        let BrowserAssetRecord { id: record_id, name, bytes, .. } = record;
        let Some(entry) = self.browser_triangulations.iter().find(|entry| entry.record_id == record_id) else {
            return;
        };
        let display_path = entry.path.clone();
        let loaded_name = name.clone();
        self.spawn_job_reporting_progress(
            format!("Loading {name}"),
            vec![crate::app::jobs::JobKey::Anonymous],
            move |cancel, progress| {
                if cancel.is_cancelled() {
                    anyhow::bail!("Cancelled");
                }
                let mesh =
                    decode_stored_mesh(&bytes, &progress.phase(0.0, DECODE_SHARE)).map_err(|error| anyhow::anyhow!("Failed to read stored triangulation {name}: {error}"))?;
                drop(bytes);
                let (spatial, edges, surface_face_order) = super::session::build_triangulation_indexes(&mesh, &progress.phase(DECODE_SHARE, 1.0));
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
                    // The entry may have been deleted while the mesh decoded.
                    let Some(index) = app.browser_triangulations.iter().position(|entry| entry.record_id == record_id) else {
                        return;
                    };
                    let should_fit = !app.scene_has_renderables();
                    let id = TriangulationId(app.next_triangulation_id);
                    app.next_triangulation_id += 1;
                    app.browser_triangulations[index].loaded = Some(id);
                    app.triangulations.push(OpenTriangulation {
                        id,
                        name: loaded.name,
                        path: display_path,
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
                        app.fit_view_to_extents();
                    }
                    userspace_log!("Loaded triangulation '{loaded_name}' from browser storage");
                    app.invalidate_topology_bounds_and_redraw();
                }
                Err(error) => userspace_warn!("Could not load triangulation: {error:#}"),
            },
        );
    }

    /// Note that `id`'s mesh has left memory. The record stays, so the
    /// explorer keeps listing it as an unloaded triangulation.
    pub(crate) fn mark_browser_triangulation_unloaded(&mut self, id: TriangulationId) -> bool {
        match self.browser_triangulations.iter_mut().find(|entry| entry.loaded == Some(id)) {
            Some(entry) => {
                entry.loaded = None;
                true
            }
            None => false,
        }
    }

    /// Delete a stored triangulation for good: unload it if it is in memory,
    /// drop its IndexedDB record and stop listing it.
    pub(crate) fn delete_browser_triangulation(&mut self, path: &Path) {
        let Some(entry) = self.browser_triangulations.iter().find(|entry| entry.path == path) else {
            return;
        };
        let record_id = entry.record_id.clone();
        let name = entry.name.clone();
        if let Some(id) = entry.loaded {
            // Cancel first: an in-flight store for this mesh would otherwise
            // recreate the record after the delete transaction completes.
            self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::Triangulation(id));
            self.close_triangulation(id);
        }
        self.forget_browser_triangulation(&record_id);
        let deleted_id = record_id;
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = crate::app::web_storage::delete_asset(BrowserStore::Triangulations, &deleted_id).await {
                userspace_warn!("Could not delete triangulation from browser storage: {error}");
            }
        });
        userspace_log!("Deleted triangulation '{name}' from browser storage");
        self.redraw_requested = true;
    }

    /// Apply the outcome of a write. A record the explorer no longer lists was
    /// deleted while its write was in flight, and would otherwise come back on
    /// the next load, so it is deleted again here.
    pub(crate) fn apply_stored_browser_triangulation(&mut self, id: TriangulationId, record_id: String, name: String, result: Result<(), String>) {
        if let Err(error) = result {
            userspace_warn!("Could not save triangulation '{name}' to browser storage: {error}");
            return;
        }
        if !self.browser_triangulations.iter().any(|entry| entry.record_id == record_id) {
            wasm_bindgen_futures::spawn_local(async move {
                let _ = crate::app::web_storage::delete_asset(BrowserStore::Triangulations, &record_id).await;
            });
            return;
        }
        if let Some(triangulation) = self.triangulations.iter_mut().find(|triangulation| triangulation.id == id) {
            triangulation.is_saved = true;
        }
        userspace_log!("Saved triangulation '{name}' to browser storage");
        self.redraw_requested = true;
    }

    /// Download a stored triangulation that is not in memory as OBJ. Browser
    /// storage keeps an internal binary encoding, so decode it before export.
    pub(crate) fn download_stored_browser_triangulation(&mut self, path: &Path) {
        let Some(entry) = self.browser_triangulations.iter().find(|entry| entry.path == path) else {
            userspace_warn!("That triangulation is no longer in browser storage");
            return;
        };
        let record_id = entry.record_id.clone();
        let file_name = format!("{}.obj", Path::new(&entry.name).file_stem().and_then(|stem| stem.to_str()).unwrap_or("triangulation"));
        wasm_bindgen_futures::spawn_local(async move {
            match crate::app::web_storage::get_asset(BrowserStore::Triangulations, &record_id).await {
                Ok(Some(record)) => {
                    let progress = crate::model::progress::Progress::new();
                    let bytes = decode_stored_mesh(&record.bytes, &progress.phase(0.0, 1.0))
                        .and_then(|mesh| formats::write_mesh_bytes(&mesh, formats::MeshFormat::Obj).map_err(anyhow::Error::new));
                    if let Err(error) = bytes.and_then(|bytes| crate::app::web_download::download(&file_name, &bytes, "text/plain").map_err(anyhow::Error::msg)) {
                        userspace_warn!("Triangulation download failed: {error}");
                    } else {
                        userspace_log!("Downloaded triangulation '{}'", record.name);
                    }
                }
                Ok(None) => userspace_warn!("That triangulation is no longer in browser storage"),
                Err(error) => userspace_warn!("Could not read triangulation from browser storage: {error}"),
            }
        });
    }

    /// Stop listing a stored triangulation and release the display name it
    /// reserved.
    fn forget_browser_triangulation(&mut self, record_id: &str) {
        let Some(index) = self.browser_triangulations.iter().position(|entry| entry.record_id == record_id) else {
            return;
        };
        let entry = self.browser_triangulations.remove(index);
        self.triangulation_files.retain(|path| path != &entry.path);
        self.browser_source_paths.remove(&entry.path);
    }
}

/// Records are always written in PLY format, so they are parsed
/// directly rather than sniffed from a (possibly extension-less) name.
fn decode_stored_mesh(bytes: &[u8], progress: &Phase) -> Result<mesh_data::Triangulation> {
    formats::read_mesh_bytes("stored.ply", bytes, progress).map_err(anyhow::Error::new)
}
