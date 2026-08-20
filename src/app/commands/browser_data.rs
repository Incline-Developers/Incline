//! Browser persistence for imported point clouds, block models and rasters.
//!
//! These are byte-backed imports: the desktop build keeps the chosen file on
//! disk and re-reads it whenever the explorer entry is loaded, but the browser
//! has no path to go back to, so the imported bytes are copied into IndexedDB
//! as they arrive. From then on an entry behaves like a tracked file on
//! desktop — the explorer lists it whether or not it is loaded, unloading
//! frees the decoded data and leaves the record, loading reads the bytes back
//! and decodes them again, and only an explicit Remove drops the record.
//!
//! Generated triangulations follow the same model but keep their own module,
//! `triangulation::browser`, because a mesh is encoded on the way out rather
//! than stored as the file the user picked.

use std::path::{Path, PathBuf};

use crate::{
    app::{
        App,
        web_storage::{BrowserAssetRecord, BrowserAssetSummary, BrowserStore},
    },
    model::{
        block_model::BlockModelSource,
        input::{InputFile, SourceRef},
    },
    userspace_log, userspace_warn,
};

/// The byte-backed import kinds that reuse this module. Triangulations are
/// deliberately absent: they are stored as re-encoded PLY bytes, not as the
/// file the user chose, and carry their own loaded-mesh bookkeeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowserDataKind {
    PointCloud,
    BlockModel,
    Raster,
    DrillHole,
}

impl BrowserDataKind {
    pub(crate) const ALL: [Self; 4] = [Self::PointCloud, Self::BlockModel, Self::Raster, Self::DrillHole];

    pub(crate) fn store(self) -> BrowserStore {
        match self {
            Self::PointCloud => BrowserStore::PointClouds,
            Self::BlockModel => BrowserStore::BlockModels,
            Self::Raster => BrowserStore::Rasters,
            Self::DrillHole => BrowserStore::DrillHoles,
        }
    }

    fn label(self) -> &'static str {
        self.store().label()
    }
}

/// Whether dropping an entry also frees the explorer display name it holds.
/// It must not be freed while the import it names is still in the scene, or a
/// later import of the same file name could be handed the same path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReleaseName {
    Yes,
    No,
}

/// One imported file held in browser storage, decoded into the scene or not.
/// The explorer keys these by `path` — a display-only name reserved through
/// `allocate_browser_source_path` — exactly as it keys desktop imports by
/// their file path.
pub(crate) struct BrowserDataEntry {
    pub(crate) record_id: String,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

impl<'a> App<'a> {
    fn browser_data_mut(&mut self, kind: BrowserDataKind) -> &mut Vec<BrowserDataEntry> {
        match kind {
            BrowserDataKind::PointCloud => &mut self.browser_point_clouds,
            BrowserDataKind::BlockModel => &mut self.browser_block_models,
            BrowserDataKind::Raster => &mut self.browser_rasters,
            BrowserDataKind::DrillHole => &mut self.browser_drill_holes,
        }
    }

    fn browser_data(&self, kind: BrowserDataKind) -> &[BrowserDataEntry] {
        match kind {
            BrowserDataKind::PointCloud => &self.browser_point_clouds,
            BrowserDataKind::BlockModel => &self.browser_block_models,
            BrowserDataKind::Raster => &self.browser_rasters,
            BrowserDataKind::DrillHole => &self.browser_drill_holes,
        }
    }

    /// Write an import's bytes to IndexedDB and list it in the explorer's
    /// source list, so the entry survives a reload and an unload the way a
    /// desktop import survives through its file on disk.
    pub(crate) fn store_browser_data(&mut self, kind: BrowserDataKind, name: &str, path: &Path, bytes: &[u8]) {
        self.store_browser_data_with_metadata(kind, name, path, bytes, None);
    }

    pub(crate) fn store_browser_data_with_metadata(&mut self, kind: BrowserDataKind, name: &str, path: &Path, bytes: &[u8], metadata_json: Option<String>) {
        let Some(proxy) = self.web_event_loop_proxy.clone() else {
            userspace_warn!("Browser storage is unavailable; '{name}' exists only until this tab is closed");
            return;
        };
        // The bytes exist twice while the write runs: once in the importer's
        // buffer and once in the JS-owned copy IndexedDB clones from. Reserve
        // the copy so an oversized import reports a budget error instead of
        // aborting the whole module on an allocation failure.
        let reservation = match crate::app::memory::reserve(bytes.len(), "Saving an import to browser storage") {
            Ok(reservation) => reservation,
            Err(error) => {
                userspace_warn!("Could not save {} '{name}' to browser storage: {error}", kind.label());
                return;
            }
        };
        let staged = crate::app::web_storage::stage_bytes(bytes);

        let record_id = uuid::Uuid::new_v4().to_string();
        self.register_browser_data(
            kind,
            BrowserDataEntry {
                record_id: record_id.clone(),
                name: name.to_owned(),
                path: path.to_owned(),
            },
        );

        let store = kind.store();
        let name = name.to_owned();
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::app::web_storage::put_staged_asset(store, record_id.clone(), name.clone(), staged, metadata_json).await;
            drop(reservation);
            let _ = proxy.send_event(crate::app::AppEvent::BrowserDataStored { kind, record_id, name, result });
        });
    }

    /// Register every stored record of `kind` in the explorer, unloaded. Bytes
    /// are read on demand, so a reload does not decode every stored import.
    pub(crate) fn catalogue_browser_data(&mut self, kind: BrowserDataKind, summaries: Vec<BrowserAssetSummary>) {
        let mut restored = 0usize;
        for summary in summaries {
            let lower_name = summary.name.to_ascii_lowercase();
            let removed_format = matches!(kind, BrowserDataKind::BlockModel) && lower_name.ends_with(".bmf")
                || matches!(kind, BrowserDataKind::DrillHole) && (lower_name.ends_with(".dhd") || lower_name.ends_with(".dhd.isis") || lower_name.ends_with(".isis"));
            if removed_format {
                continue;
            }
            if self.browser_data(kind).iter().any(|entry| entry.record_id == summary.id) {
                continue;
            }
            let path = self.allocate_browser_source_path(&summary.name);
            self.register_browser_data(
                kind,
                BrowserDataEntry {
                    record_id: summary.id,
                    name: summary.name,
                    path,
                },
            );
            restored += 1;
        }
        if restored > 0 {
            userspace_log!("Found {restored} {}(s) in browser storage", kind.label());
            self.redraw_requested = true;
        }
    }

    /// Read a stored import back out of IndexedDB. The fetch runs on the
    /// browser's main thread; decoding happens in a job, as it does for a
    /// freshly chosen file.
    pub(crate) fn load_browser_data(&mut self, kind: BrowserDataKind, path: &Path) {
        let Some(entry) = self.browser_data(kind).iter().find(|entry| entry.path == path) else {
            userspace_warn!("That {} is no longer in browser storage", kind.label());
            return;
        };
        let record_id = entry.record_id.clone();
        if !self.browser_data_loads.insert(record_id.clone()) {
            return;
        }
        let Some(proxy) = self.web_event_loop_proxy.clone() else {
            userspace_warn!("Browser storage is unavailable");
            return;
        };
        let store = kind.store();
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::app::web_storage::get_asset(store, &record_id).await;
            let _ = proxy.send_event(crate::app::AppEvent::BrowserDataFetched { kind, record_id, result });
        });
    }

    /// Hand a fetched record to the same importer a freshly chosen file goes
    /// through, under the display name the entry already occupies.
    pub(crate) fn apply_fetched_browser_data(&mut self, kind: BrowserDataKind, record_id: String, result: Result<Option<BrowserAssetRecord>, String>) {
        self.browser_data_loads.remove(&record_id);
        let record = match result {
            Ok(Some(record)) => record,
            Ok(None) => {
                userspace_warn!("That {} is no longer in browser storage", kind.label());
                self.drop_browser_data_entry(kind, &record_id, ReleaseName::Yes);
                return;
            }
            Err(error) => {
                userspace_warn!("Could not read {} from browser storage: {error}", kind.label());
                return;
            }
        };
        let Some(entry) = self.browser_data(kind).iter().find(|entry| entry.record_id == record.id) else {
            // Removed while the read was in flight.
            return;
        };
        let display_path = entry.path.clone();
        if kind == BrowserDataKind::DrillHole {
            if let Err(error) = self.open_stored_browser_drill_holes(display_path, record.name, &record.bytes, record.metadata_json.as_deref()) {
                userspace_warn!("Could not load stored drillholes: {error:#}");
            }
            return;
        }
        // The bytes are already out of IndexedDB by the time the size is
        // known, so this reservation records what the import now holds rather
        // than gating the read itself.
        let reservation = match crate::app::memory::reserve(record.bytes.len(), &format!("stored {} '{}'", kind.label(), record.name)) {
            Ok(reservation) => Some(reservation),
            Err(error) => {
                userspace_warn!("Could not load {} '{}': {error}", kind.label(), record.name);
                return;
            }
        };
        let csv_columns = record.metadata_json.as_deref().and_then(|json| match serde_json::from_str(json) {
            Ok(mapping) => Some(mapping),
            Err(error) => {
                userspace_warn!("Stored CSV block-model mapping is invalid; trying exact header names instead: {error}");
                None
            }
        });
        let input = InputFile {
            source: SourceRef {
                name: record.name,
                byte_len: record.bytes.len(),
            },
            bytes: record.bytes,
            reservation,
        };
        match kind {
            BrowserDataKind::PointCloud => self.open_point_cloud_input(input, display_path),
            BrowserDataKind::Raster => self.open_raster_input(input, display_path),
            BrowserDataKind::BlockModel => {
                if let Err(error) = self.open_block_model_input(
                    input,
                    BlockModelSource {
                        path: display_path,
                        csv_columns,
                        generated: false,
                    },
                ) {
                    userspace_warn!("Could not load block model: {error:#}");
                }
            }
            BrowserDataKind::DrillHole => unreachable!("handled before creating a single-file input"),
        }
    }

    /// Apply the outcome of a write. A record the explorer no longer lists was
    /// removed while its write was in flight, and would otherwise come back on
    /// the next load, so it is deleted again here.
    pub(crate) fn apply_stored_browser_data(&mut self, kind: BrowserDataKind, record_id: String, name: String, result: Result<(), String>) {
        if let Err(error) = result {
            userspace_warn!("Could not save {} '{name}' to browser storage: {error}", kind.label());
            // Nothing was stored, so stop offering the entry as reloadable.
            // The import itself is still in the scene under this display name,
            // which therefore stays reserved.
            self.drop_browser_data_entry(kind, &record_id, ReleaseName::No);
            return;
        }
        if !self.browser_data(kind).iter().any(|entry| entry.record_id == record_id) {
            let store = kind.store();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = crate::app::web_storage::delete_asset(store, &record_id).await;
            });
            return;
        }
        userspace_log!("Saved {} '{name}' to browser storage", kind.label());
        self.redraw_requested = true;
    }

    /// Drop a stored import for good. The caller still unloads whatever the
    /// record produced in the scene; this only ends its persistence.
    pub(crate) fn delete_browser_data(&mut self, kind: BrowserDataKind, path: &Path) {
        let Some(entry) = self.browser_data(kind).iter().find(|entry| entry.path == path) else {
            return;
        };
        let record_id = entry.record_id.clone();
        let name = entry.name.clone();
        self.drop_browser_data_entry(kind, &record_id, ReleaseName::Yes);
        let store = kind.store();
        wasm_bindgen_futures::spawn_local(async move {
            match crate::app::web_storage::delete_asset(store, &record_id).await {
                Ok(()) => userspace_log!("Deleted {} '{name}' from browser storage", store.label()),
                Err(error) => userspace_warn!("Could not delete {} '{name}' from browser storage: {error}", store.label()),
            }
        });
    }

    /// List an entry in both the stored-record set and the explorer's source
    /// list, which is what makes an unloaded import visible.
    fn register_browser_data(&mut self, kind: BrowserDataKind, entry: BrowserDataEntry) {
        let path = entry.path.clone();
        let name = entry.name.clone();
        self.browser_data_mut(kind).push(entry);
        match kind {
            BrowserDataKind::PointCloud => {
                if !self.point_cloud_files.contains(&path) {
                    self.point_cloud_files.push(path);
                }
            }
            BrowserDataKind::Raster => {
                if !self.raster_files.contains(&path) {
                    self.raster_files.push(path);
                }
            }
            BrowserDataKind::BlockModel => {
                if !self.block_model_files.iter().any(|source| source.path == path) {
                    self.block_model_files.push(BlockModelSource {
                        path,
                        csv_columns: None,
                        generated: false,
                    });
                }
            }
            BrowserDataKind::DrillHole => {
                if !self.drill_hole_files.iter().any(|source| source.primary_path() == path) {
                    self.drill_hole_files.push(crate::model::drill_hole::DrillHoleSource::Csv {
                        name,
                        files: Vec::new(),
                        browser_path: Some(path),
                    });
                }
            }
        }
        self.redraw_requested = true;
    }

    /// Stop listing a stored import, in the explorer as well as the stored-
    /// record set. The display name it reserved is released unless the import
    /// it named is still in the scene.
    fn drop_browser_data_entry(&mut self, kind: BrowserDataKind, record_id: &str, release_name: ReleaseName) {
        let entries = self.browser_data_mut(kind);
        let Some(index) = entries.iter().position(|entry| entry.record_id == record_id) else {
            return;
        };
        let entry = entries.remove(index);
        match kind {
            BrowserDataKind::PointCloud => self.point_cloud_files.retain(|existing| existing != &entry.path),
            BrowserDataKind::Raster => self.raster_files.retain(|existing| existing != &entry.path),
            BrowserDataKind::BlockModel => self.block_model_files.retain(|source| source.path != entry.path),
            BrowserDataKind::DrillHole => self.drill_hole_files.retain(|source| source.primary_path() != entry.path),
        }
        if release_name == ReleaseName::Yes {
            self.browser_source_paths.remove(&entry.path);
        }
        self.redraw_requested = true;
    }
}
