use anyhow::{Context, Result};

use crate::{
    app::App,
    i18n::{tr, tr_format},
    model::{
        Command, ItemRef, ItemStyle, OpenItem, SceneEntityId,
        drill_hole::{
            DrillColorPreset, DrillColorState, DrillColorStop, DrillFieldKind, DrillHole, DrillHoleDataset, DrillHoleId, DrillHoleSource, LoadedDrillHoleDataset,
            OpenDrillHoleDataset, TraceStation, default_category_colors,
        },
        formats::csv_drill_hole,
    },
    userspace_log, userspace_warn,
};

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
        DrillHoleSource::Omf { .. } => anyhow::bail!("OMF drillhole data is loaded through the project importer"),
    }
}

impl<'a> App<'a> {
    /// Turn the pattern menu's exact preview into a normal project-owned
    /// drillhole dataset. Generated patterns need no reload source: project
    /// saves already embed every collar and trace in OMF.
    pub(crate) fn create_drill_pattern(&mut self, name: String, collars: Vec<glam::DVec3>, depth: f64, diameter: f64) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("{}", tr!(literal = "Enter a name for the drill pattern"));
        }
        if !depth.is_finite() || depth <= 0.0 {
            anyhow::bail!("{}", tr!(literal = "Hole depth must be greater than zero"));
        }
        if !diameter.is_finite() || diameter <= 0.0 {
            anyhow::bail!("{}", tr!(literal = "Hole diameter must be greater than zero"));
        }
        if collars.is_empty() {
            anyhow::bail!("{}", tr!(literal = "The pattern contains no holes"));
        }
        if collars.len() > crate::model::drill_hole::MAX_PATTERN_HOLES || collars.iter().any(|collar| !collar.is_finite()) {
            anyhow::bail!("{}", tr!(literal = "The drill pattern is too large or contains invalid collar coordinates"));
        }

        let width = collars.len().to_string().len().max(3);
        let holes = collars
            .into_iter()
            .enumerate()
            .map(|(index, collar)| DrillHole {
                dhid: format!("H{:0width$}", index + 1, width = width),
                collar,
                diameter: Some(diameter),
                trace: vec![
                    TraceStation { depth: 0.0, position: collar },
                    TraceStation {
                        depth,
                        position: collar - glam::DVec3::Z * depth,
                    },
                ],
                render_ranges: Vec::new(),
                intervals: Vec::new(),
            })
            .collect();
        let dataset = std::sync::Arc::new(DrillHoleDataset::new(holes));
        let id = DrillHoleId(self.next_drill_hole_id);
        self.next_drill_hole_id += 1;
        let name = crate::model::project::unique_item_name(name.to_owned(), self.drill_holes.iter().map(|item| item.name.as_str()));
        let item = OpenDrillHoleDataset {
            id,
            state: crate::model::project::ProjectItemState::dirty(None),
            name,
            dataset,
            color: DrillColorState::default(),
        };
        self.execute_edit(Command::AddItem {
            item: ItemRef::DrillHole(id),
            index: self.drill_holes.len(),
            added: Some(OpenItem::DrillHole(Box::new(item))),
        });
        self.editor.active_drill_hole = Some(id);
        self.editor.close_drill_pattern();
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_web_drill_hole_source(&mut self, mut source: DrillHoleSource) -> Result<()> {
        let (_, inputs) = self.web_import_files.take().context("Choose the drillhole source files again")?;
        let display_path = crate::app::browser_source_filename(&source.display_name());
        remap_browser_source_path(&mut source, display_path.clone());
        let dataset = parse_browser_bundle(&source, inputs.iter().map(|input| input.bytes.as_slice()))?;
        let loaded = LoadedDrillHoleDataset {
            name: source.display_name(),
            source,
            dataset: std::sync::Arc::new(dataset),
        };
        self.add_loaded_drill_holes(loaded);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_drill_hole_source(&mut self, source: DrillHoleSource) -> Result<()> {
        match &source {
            DrillHoleSource::LegacyDhd { .. } => anyhow::bail!("DHD drillhole sources are no longer supported"),
            DrillHoleSource::Csv { files, .. } if files.iter().any(|file| !file.path.is_file()) => anyhow::bail!("One or more drillhole CSV sources no longer exist"),
            _ => {}
        }
        self.open_drill_hole_source(source)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_drill_hole_source(&mut self, source: DrillHoleSource) -> Result<()> {
        if self.pending_drill_hole_loads.iter().any(|(_, pending, _, _)| *pending == source) {
            return Ok(());
        }
        let name = source.display_name();
        let (ticket, progress) = self.begin_reported_task(crate::i18n::tr_format!(literal = "Loading %name%", name = &name));
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
                        DrillHoleSource::Omf { .. } => anyhow::bail!("OMF drillhole data is loaded through the project importer"),
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
                    userspace_warn!("{}", tr_format!(literal = "Failed to load drillholes: %error%", error = format!("{error:#}")));
                    self.finish_background_task(ticket, false);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    userspace_warn!("{}", tr_format!(literal = "Drillhole loader disconnected for %name%", name = source.display_name()));
                    self.finish_background_task(ticket, false);
                }
            }
            drop(report);
        }
    }

    pub(super) fn add_loaded_drill_holes(&mut self, loaded: LoadedDrillHoleDataset) {
        let id = DrillHoleId(self.next_drill_hole_id);
        self.next_drill_hole_id += 1;
        let name = crate::model::project::unique_item_name(loaded.name, self.drill_holes.iter().map(|item| item.name.as_str()));
        userspace_log!(
            "{}",
            tr_format!(
                literal = "Loaded drillhole dataset '%name%': %holes% holes, %fields% colour fields",
                name = name.clone(),
                holes = loaded.dataset.holes.len(),
                fields = loaded.dataset.fields.len()
            )
        );
        self.drill_holes.push(OpenDrillHoleDataset {
            id,
            state: crate::model::project::ProjectItemState::dirty(Some(loaded.source.display_name())),
            name,
            dataset: loaded.dataset,
            color: DrillColorState::default(),
        });
        self.touch_active_project_content();
        self.persist_session();
        self.invalidate_topology_bounds_and_redraw();
    }

    /// Record a change to one dataset's colour state as an undo step.
    fn set_drill_hole_color(&mut self, id: DrillHoleId, change: impl FnOnce(&OpenDrillHoleDataset, &mut DrillColorState)) {
        let Some(dataset) = self.drill_holes.iter().find(|item| item.id == id) else {
            return;
        };
        let mut color = dataset.color.clone();
        change(dataset, &mut color);
        self.set_item_style(
            ItemRef::DrillHole(id),
            ItemStyle::DrillHole {
                loaded: dataset.state.loaded,
                color,
            },
        );
        self.request_topology_redraw();
    }

    pub(crate) fn set_drill_hole_color_field(&mut self, id: DrillHoleId, field: Option<String>) {
        if self
            .drill_holes
            .iter()
            .find(|item| item.id == id)
            .is_none_or(|dataset| field.as_deref().is_some_and(|key| dataset.dataset.field(key).is_none()))
        {
            return;
        }
        self.set_drill_hole_color(id, |dataset, color| {
            color.active_field = field.clone();
            color.preset = DrillColorPreset::Rainbow;
            color.smooth = true;
            color.stops = DrillColorPreset::Rainbow.stops();
            color.categories = field
                .as_deref()
                .and_then(|key| dataset.dataset.field(key))
                .and_then(|field| match &field.kind {
                    DrillFieldKind::Categorical { categories } => Some(default_category_colors(categories)),
                    DrillFieldKind::Numeric { .. } => None,
                })
                .unwrap_or_default();
        });
    }

    pub(crate) fn set_drill_hole_color_preset(&mut self, id: DrillHoleId, preset: DrillColorPreset) {
        self.set_drill_hole_color(id, |_, color| {
            color.preset = preset;
            color.smooth = preset.smooth();
            color.stops = preset.stops();
        });
    }

    pub(crate) fn set_drill_hole_color_stops(&mut self, id: DrillHoleId, mut stops: Vec<DrillColorStop>) {
        stops.retain(|stop| stop.t.is_finite() && stop.color.iter().all(|value| value.is_finite()));
        stops.sort_by(|a, b| a.t.total_cmp(&b.t));
        for stop in &mut stops {
            stop.t = stop.t.clamp(0.0, 1.0);
        }
        if (2..=12).contains(&stops.len()) {
            self.set_drill_hole_color(id, |_, color| color.stops = stops);
        }
    }

    pub(crate) fn set_drill_hole_category_colors(&mut self, id: DrillHoleId, categories: Vec<crate::model::drill_hole::DrillCategoryColor>) {
        self.set_drill_hole_color(id, |_, color| color.categories = categories.into_iter().take(12).collect());
    }

    pub(crate) fn close_drill_hole(&mut self, id: DrillHoleId) {
        self.set_item_loaded(crate::model::ItemRef::DrillHole(id), false);
    }

    pub(crate) fn release_drillhole_runtime(&mut self, id: DrillHoleId) {
        self.cancel_collar_move_touching(id);
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::DrillHole(id));
        let entity = SceneEntityId::DrillHole(id);
        self.editor.selected_handles.remove(&entity);
        self.editor.hidden_handles.remove(&entity);
        self.editor.translucent_handles.remove(&entity);
        if self.editor.drill_hole_color_dialog == Some(id) {
            self.editor.drill_hole_color_dialog = None;
        }
        if self.editor.active_drill_hole == Some(id) {
            self.editor.active_drill_hole = None;
        }
        if self.editor.tie_anchor.is_some_and(|anchor| anchor.dataset == id) {
            self.editor.end_tie_chain();
        }
        self.editor.selected_drill_holes.retain(|hole| hole.dataset != id);
        self.editor.selected_tie_ins.retain(|tie| tie.dataset != id);
        if self.editor.initiation_dialog.as_ref().is_some_and(|dialog| dialog.target.dataset == id) {
            self.editor.initiation_dialog = None;
        }
        self.invalidate_topology_bounds_and_redraw();
    }

    pub(crate) fn remove_drill_hole(&mut self, id: DrillHoleId) {
        self.cancel_collar_move_touching(id);
        self.cancel_jobs(|key| *key == crate::app::jobs::JobKey::DrillHole(id));
        let entity = SceneEntityId::DrillHole(id);
        self.editor.selected_handles.remove(&entity);
        self.editor.hidden_handles.remove(&entity);
        self.editor.explicitly_frozen.remove(&entity);
        self.editor.frozen_handles.remove(&entity);
        self.editor.translucent_handles.remove(&entity);
        if self.editor.active_drill_hole == Some(id) {
            self.editor.active_drill_hole = None;
        }
        if self.editor.tie_anchor.is_some_and(|anchor| anchor.dataset == id) {
            self.editor.end_tie_chain();
        }
        self.editor.selected_drill_holes.retain(|hole| hole.dataset != id);
        self.editor.selected_tie_ins.retain(|tie| tie.dataset != id);
        if self.editor.initiation_dialog.as_ref().is_some_and(|dialog| dialog.target.dataset == id) {
            self.editor.initiation_dialog = None;
        }
        self.delete_project_item(ItemRef::DrillHole(id));
        self.request_topology_redraw();
    }
}
