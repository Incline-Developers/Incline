use anyhow::{Context, Result};

use crate::{
    app::App,
    model::{
        drill_hole::{
            DrillColorPreset, DrillColorState, DrillColorStop, DrillFieldKind, DrillHoleId, DrillHoleSource, LoadedDrillHoleDataset, OpenDrillHoleDataset, default_category_colors,
        },
        formats::csv_drill_hole,
    },
    userspace_log, userspace_warn,
};

#[cfg(target_arch = "wasm32")]
#[derive(serde::Serialize, serde::Deserialize)]
struct BrowserDrillHoleManifest {
    source: DrillHoleSource,
    original_filenames: Vec<String>,
}

#[cfg(target_arch = "wasm32")]
fn remap_browser_source_path(source: &mut DrillHoleSource, path: std::path::PathBuf) {
    match source {
        DrillHoleSource::LegacyDhd { path: source_path } => *source_path = path,
        DrillHoleSource::Csv { browser_path, .. } => *browser_path = Some(path),
        DrillHoleSource::Omf { path: source_path, .. } => *source_path = path,
    }
}

#[cfg(target_arch = "wasm32")]
fn parse_browser_bundle<'bytes>(source: &DrillHoleSource, bytes: impl IntoIterator<Item = &'bytes [u8]>) -> Result<crate::model::drill_hole::DrillHoleDataset> {
    let bytes = bytes.into_iter().collect::<Vec<_>>();
    match source {
        DrillHoleSource::LegacyDhd { .. } => anyhow::bail!("DHD drillhole sources are no longer supported"),
        DrillHoleSource::Csv { files, .. } => {
            if files.len() != bytes.len() {
                anyhow::bail!("Stored CSV manifest contains {} mappings but {} files", files.len(), bytes.len());
            }
            csv_drill_hole::parse_bundle(files.iter().zip(bytes).map(|(mapping, bytes)| (mapping, bytes))).map_err(anyhow::Error::new)
        }
        DrillHoleSource::Omf { .. } => anyhow::bail!("OMF drillhole data is loaded through the OMF project importer"),
    }
}

#[cfg(target_arch = "wasm32")]
fn encode_browser_bundle<'bytes>(files: impl IntoIterator<Item = &'bytes [u8]>) -> Result<Vec<u8>> {
    let files = files.into_iter().collect::<Vec<_>>();
    let mut output = Vec::new();
    output.extend_from_slice(&u32::try_from(files.len()).context("Too many CSV files")?.to_le_bytes());
    for bytes in files {
        output.extend_from_slice(&u64::try_from(bytes.len()).context("Drillhole file is too large")?.to_le_bytes());
        output.extend_from_slice(bytes);
    }
    Ok(output)
}

#[cfg(target_arch = "wasm32")]
fn decode_browser_bundle(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    let mut cursor = 0usize;
    let read_u32 = |cursor: &mut usize| -> Result<u32> {
        let end = cursor.checked_add(4).context("Stored drillhole manifest overflow")?;
        let value = u32::from_le_bytes(
            bytes
                .get(*cursor..end)
                .context("Stored drillhole manifest is truncated")?
                .try_into()
                .expect("four-byte slice"),
        );
        *cursor = end;
        Ok(value)
    };
    let count = read_u32(&mut cursor)? as usize;
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        let length_end = cursor.checked_add(8).context("Stored drillhole manifest overflow")?;
        let length = u64::from_le_bytes(
            bytes
                .get(cursor..length_end)
                .context("Stored drillhole manifest is truncated")?
                .try_into()
                .expect("eight-byte slice"),
        );
        cursor = length_end;
        let length = usize::try_from(length).context("Stored drillhole file is too large")?;
        let end = cursor.checked_add(length).context("Stored drillhole manifest overflow")?;
        files.push(bytes.get(cursor..end).context("Stored drillhole file is truncated")?);
        cursor = end;
    }
    if cursor != bytes.len() {
        anyhow::bail!("Stored drillhole manifest has trailing data");
    }
    Ok(files)
}

impl<'a> App<'a> {
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_drill_hole_source(&mut self, mut source: DrillHoleSource) -> Result<()> {
        let (_, inputs) = self.web_import_files.take().context("Choose the drillhole source files again")?;
        let original_filenames = inputs.iter().map(|input| input.source.name.clone()).collect::<Vec<_>>();
        let display_path = self.allocate_browser_source_path(&source.display_name());
        remap_browser_source_path(&mut source, display_path.clone());
        let dataset = parse_browser_bundle(&source, inputs.iter().map(|input| input.bytes.as_slice()))?;
        let bytes = encode_browser_bundle(inputs.iter().map(|input| input.bytes.as_slice()))?;
        let manifest = BrowserDrillHoleManifest {
            source: source.clone(),
            original_filenames,
        };
        let metadata = serde_json::to_string(&manifest).context("Could not preserve the drillhole file mappings")?;
        self.store_browser_data_with_metadata(
            crate::app::commands::browser_data::BrowserDataKind::DrillHole,
            &source.display_name(),
            &display_path,
            &bytes,
            Some(metadata),
        );
        // Storage registration adds an unloaded placeholder; replace it with
        // the mapped source used by this runtime dataset.
        self.drill_hole_files.retain(|item| item.primary_path() != display_path);
        let loaded = LoadedDrillHoleDataset {
            name: source.display_name(),
            source,
            dataset: std::sync::Arc::new(dataset),
        };
        self.add_loaded_drill_holes(loaded);
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn open_stored_browser_drill_holes(&mut self, display_path: std::path::PathBuf, name: String, bytes: &[u8], metadata: Option<&str>) -> Result<()> {
        let mut manifest: BrowserDrillHoleManifest =
            serde_json::from_str(metadata.context("Stored drillhole mapping is missing")?).context("Stored drillhole mapping is invalid")?;
        remap_browser_source_path(&mut manifest.source, display_path.clone());
        let files = decode_browser_bundle(bytes)?;
        let dataset = parse_browser_bundle(&manifest.source, files.iter().copied())?;
        self.drill_hole_files.retain(|item| item.primary_path() != display_path);
        self.add_loaded_drill_holes(LoadedDrillHoleDataset {
            name,
            source: manifest.source,
            dataset: std::sync::Arc::new(dataset),
        });
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_drill_hole_source(&mut self, source: DrillHoleSource) -> Result<()> {
        match &source {
            DrillHoleSource::LegacyDhd { .. } => anyhow::bail!("DHD drillhole sources are no longer supported"),
            DrillHoleSource::Csv { files, .. } if files.iter().any(|file| !file.path.is_file()) => anyhow::bail!("One or more drillhole CSV sources no longer exist"),
            _ => {}
        }
        if !self.drill_hole_files.contains(&source) {
            self.drill_hole_files.push(source.clone());
        }
        self.persist_session();
        self.open_drill_hole_source(source)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_drill_hole_source(&mut self, source: DrillHoleSource) -> Result<()> {
        if self.drill_holes.iter().any(|item| item.source == source) || self.pending_drill_hole_loads.iter().any(|(_, pending, _, _)| *pending == source) {
            return Ok(());
        }
        let name = source.display_name();
        let (ticket, progress) = self.begin_reported_task(format!("Loading {name}"));
        let (tx, rx) = std::sync::mpsc::channel();
        let console_report = crate::logging::retain_current_report();
        let worker_report = console_report.as_ref().map(crate::logging::ConsoleReportHandle::child);
        self.pending_drill_hole_loads.push((ticket, source.clone(), rx, console_report));
        let window = self.window.clone();
        crate::app::jobs::spawn_pool_task(move || {
            let run = || {
                crate::app::jobs::run_compute_catching_panic(|| -> Result<_> {
                    progress.set_fraction(0.1);
                    let dataset = match &source {
                        DrillHoleSource::LegacyDhd { .. } => anyhow::bail!("DHD drillhole sources are no longer supported"),
                        DrillHoleSource::Csv { files, .. } => csv_drill_hole::parse_paths(files).with_context(|| "Failed to parse mapped drillhole CSV bundle")?,
                        DrillHoleSource::Omf { .. } => anyhow::bail!("OMF drillhole data is loaded through the OMF project importer"),
                    };
                    progress.set_fraction(1.0);
                    Ok(LoadedDrillHoleDataset {
                        name,
                        source,
                        dataset: std::sync::Arc::new(dataset),
                    })
                })
            };
            let result = if let Some(report) = worker_report.as_ref() { report.scope(run) } else { run() };
            let _ = tx.send(result);
            if let Some(window) = window {
                window.request_redraw();
            }
        });
        Ok(())
    }

    pub(crate) fn poll_drill_hole_loads(&mut self) {
        let pending = std::mem::take(&mut self.pending_drill_hole_loads);
        for (ticket, source, receiver, report) in pending {
            match receiver.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.pending_drill_hole_loads.push((ticket, source, receiver, report));
                    continue;
                }
                Ok(Ok(loaded)) => {
                    self.finish_background_task(ticket, true);
                    self.add_loaded_drill_holes(loaded);
                }
                Ok(Err(error)) => {
                    userspace_warn!("Failed to load drillholes: {error:#}");
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    userspace_warn!("Drillhole loader disconnected for {}", source.display_name());
                    self.finish_background_task(ticket, false);
                }
            }
            drop(report);
        }
    }

    pub(super) fn add_loaded_drill_holes(&mut self, loaded: LoadedDrillHoleDataset) {
        let id = DrillHoleId(self.next_drill_hole_id);
        self.next_drill_hole_id += 1;
        userspace_log!(
            "Loaded drillhole dataset '{}': {} holes, {} colour fields",
            loaded.name,
            loaded.dataset.holes.len(),
            loaded.dataset.fields.len()
        );
        self.drill_holes.push(OpenDrillHoleDataset {
            id,
            name: loaded.name,
            source: loaded.source.clone(),
            dataset: loaded.dataset,
            visible: true,
            color: DrillColorState::default(),
        });
        if !matches!(loaded.source, DrillHoleSource::Omf { .. }) && !self.drill_hole_files.contains(&loaded.source) {
            self.drill_hole_files.push(loaded.source);
        }
        self.persist_session();
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn toggle_drill_hole_visible(&mut self, id: DrillHoleId) {
        if let Some(dataset) = self.drill_holes.iter_mut().find(|item| item.id == id) {
            dataset.visible = !dataset.visible;
            self.invalidate_topology_bounds_and_redraw();
        }
    }

    pub(crate) fn set_drill_hole_color_field(&mut self, id: DrillHoleId, field: Option<String>) {
        let Some(dataset) = self.drill_holes.iter_mut().find(|item| item.id == id) else {
            return;
        };
        if field.as_deref().is_some_and(|key| dataset.dataset.field(key).is_none()) {
            return;
        }
        dataset.color.active_field = field.clone();
        dataset.color.preset = DrillColorPreset::Rainbow;
        dataset.color.smooth = true;
        dataset.color.stops = DrillColorPreset::Rainbow.stops();
        dataset.color.categories = field
            .as_deref()
            .and_then(|key| dataset.dataset.field(key))
            .and_then(|field| match &field.kind {
                DrillFieldKind::Categorical { categories } => Some(default_category_colors(categories)),
                DrillFieldKind::Numeric { .. } => None,
            })
            .unwrap_or_default();
        self.request_topology_redraw();
    }

    pub(crate) fn set_drill_hole_color_preset(&mut self, id: DrillHoleId, preset: DrillColorPreset) {
        if let Some(dataset) = self.drill_holes.iter_mut().find(|item| item.id == id) {
            dataset.color.preset = preset;
            dataset.color.smooth = preset.smooth();
            dataset.color.stops = preset.stops();
            self.request_topology_redraw();
        }
    }

    pub(crate) fn set_drill_hole_color_stops(&mut self, id: DrillHoleId, mut stops: Vec<DrillColorStop>) {
        stops.retain(|stop| stop.t.is_finite() && stop.color.iter().all(|value| value.is_finite()));
        stops.sort_by(|a, b| a.t.total_cmp(&b.t));
        for stop in &mut stops {
            stop.t = stop.t.clamp(0.0, 1.0);
        }
        if (2..=12).contains(&stops.len())
            && let Some(dataset) = self.drill_holes.iter_mut().find(|item| item.id == id)
        {
            dataset.color.stops = stops;
            self.request_topology_redraw();
        }
    }

    pub(crate) fn set_drill_hole_category_colors(&mut self, id: DrillHoleId, categories: Vec<crate::model::drill_hole::DrillCategoryColor>) {
        if let Some(dataset) = self.drill_holes.iter_mut().find(|item| item.id == id) {
            dataset.color.categories = categories.into_iter().take(12).collect();
            self.request_topology_redraw();
        }
    }

    pub(crate) fn close_drill_hole(&mut self, id: DrillHoleId) {
        self.drill_holes.retain(|dataset| dataset.id != id);
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::DrillHole(id));
        if self.editor.drill_hole_color_dialog == Some(id) {
            self.editor.drill_hole_color_dialog = None;
        }
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn remove_drill_hole(&mut self, source: DrillHoleSource) {
        let ids = self
            .drill_holes
            .iter()
            .filter(|dataset| dataset.source == source)
            .map(|dataset| dataset.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.close_drill_hole(id);
        }
        let pending = std::mem::take(&mut self.pending_drill_hole_loads);
        for (ticket, pending_source, receiver, report) in pending {
            if pending_source == source {
                self.cancel_background_task(ticket);
                if let Some(report) = report {
                    report.cancel();
                }
            } else {
                self.pending_drill_hole_loads.push((ticket, pending_source, receiver, report));
            }
        }
        self.drill_hole_files.retain(|item| *item != source);
        self.persist_session();
        self.request_topology_redraw();
    }
}
