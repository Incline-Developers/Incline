//! The Solids workspace's Reserves setup: the project-wide Field List and
//! each block model's mapping onto it.
//!
//! Unlike design edits, these are config-like list edits - matching how the
//! Drill & Blast palette's products behave - so they are not undoable
//! through `History`, and go straight through `Document`/`OpenBlockModel`.

use crate::model::{ReserveAggregation, ReserveFieldId, block_model::BlockModelId};

impl crate::app::App<'_> {
    pub(crate) fn add_reserve_field(&mut self, name: String, aggregation: ReserveAggregation) {
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        let name = crate::model::project::unique_item_name(name, document.reserve_fields().iter().map(|field| field.name.as_str()));
        document.add_reserve_field(name, aggregation);
        self.touch_active_project_content();
        self.recompute_all_reserve_totals();
        self.invalidate_geometry();
    }

    pub(crate) fn rename_reserve_field(&mut self, id: ReserveFieldId, new_name: String) {
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        let new_name = crate::model::project::unique_item_name(new_name, document.reserve_fields().iter().filter(|field| field.id != id).map(|field| field.name.as_str()));
        document.rename_reserve_field(id, new_name);
        self.touch_active_project_content();
        self.invalidate_geometry();
    }

    /// Remove a field from the Field List, and every block model's mapping
    /// of it - a mapping entry naming a field that no longer exists would
    /// otherwise linger, unreachable, until the next OMF round-trip pruned it.
    pub(crate) fn delete_reserve_field(&mut self, id: ReserveFieldId) {
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        if !document.remove_reserve_field(id) {
            return;
        }
        for model in &mut self.block_models {
            let before = model.reserve_mapping.len();
            model.reserve_mapping.retain(|mapping| mapping.field != id);
            if model.reserve_mapping.len() != before {
                model.state.touch();
            }
        }
        self.touch_active_project_content();
        self.recompute_all_reserve_totals();
        self.invalidate_geometry();
    }

    /// Map (or unmap, when `source` is `None`) one block model's own column
    /// or a constant onto one Reserves field.
    pub(crate) fn set_reserve_mapping(&mut self, block_model: BlockModelId, field: ReserveFieldId, source: Option<crate::model::block_model::ReserveMappingSource>) {
        let Some(model) = self.block_models.iter_mut().find(|model| model.id == block_model) else {
            return;
        };
        model.reserve_mapping.retain(|mapping| mapping.field != field);
        if let Some(source) = source {
            model.reserve_mapping.push(crate::model::block_model::ReserveFieldMapping { field, source });
        }
        model.state.touch();
        self.touch_active_project_content();
        self.recompute_reserve_totals(block_model);
    }

    /// Opt one block model in or out of the project's Reserves. Its mapping
    /// is kept either way. Setup statistics remain available independently
    /// of whether the model contributes to project reserves.
    pub(crate) fn set_reserve_model_included(&mut self, block_model: BlockModelId, included: bool) {
        let Some(model) = self.block_models.iter_mut().find(|model| model.id == block_model) else {
            return;
        };
        if model.included_in_reserves == included {
            return;
        }
        model.included_in_reserves = included;
        model.state.touch();
        self.touch_active_project_content();
    }

    /// Recompute one block model's reserve totals from its current mapping
    /// and the project's Field List.
    pub(crate) fn recompute_reserve_totals(&mut self, block_model: BlockModelId) {
        if let Some(model) = self.block_models.iter_mut().find(|model| model.id == block_model) {
            clear_reserve_totals(model);
        }
    }

    pub(crate) fn recompute_all_reserve_totals(&mut self) {
        for model in &mut self.block_models {
            clear_reserve_totals(model);
        }
    }

    /// Refresh on page entry, restored input, or mapping/field changes.
    ///
    /// The Block Models step shows the selected model's figures, but the
    /// pipeline's own stage computes every configured model's, so this only
    /// asks for the selected one and the work itself lives in
    /// [`Self::request_reserve_stats`].
    pub(crate) fn sync_reserve_setup_stats(&mut self) {
        use crate::ui::state::{PlanningPage, SolidsStep};
        if !self.editor.is_planning_setup() || self.editor.planning_page != PlanningPage::Solids || self.editor.planning_solids_step != SolidsStep::BlockModels {
            return;
        }
        let Some(id) = self.editor.planning_selected_block_model else {
            return;
        };
        self.request_reserve_stats(id);
    }

    /// Compute one block model's whole-model reserve statistics, if they are
    /// not already current. Scans shared columns on a worker, never in the
    /// egui draw path.
    ///
    /// The request key is only committed once the scan is actually started or
    /// a restore is in flight, so a run that could not begin leaves the key
    /// alone and is retried rather than being suppressed by its own attempt.
    pub(crate) fn request_reserve_stats(&mut self, id: BlockModelId) {
        use std::{
            hash::{DefaultHasher, Hash, Hasher},
            sync::Arc,
        };

        use crate::{
            app::jobs::JobKey,
            model::{ItemRef, block_model::ReserveMappingSource},
        };
        let Some(project) = self.workspace.active_project() else {
            return;
        };
        let runtime = project.runtime_id;
        let fields = self.workspace.active_document().map(|doc| doc.reserve_fields().to_vec()).unwrap_or_default();
        let Some(model) = self.block_models.iter().find(|model| model.id == id) else {
            return;
        };
        let mut hasher = DefaultHasher::new();
        runtime.hash(&mut hasher);
        serde_json::to_vec(&fields).unwrap_or_default().hash(&mut hasher);
        serde_json::to_vec(&model.reserve_mapping).unwrap_or_default().hash(&mut hasher);
        model.model.metadata.n_blocks.hash(&mut hasher);
        let mut missing = false;
        let mut data_hash = DefaultHasher::new();
        for entry in &model.reserve_mapping {
            if let ReserveMappingSource::Column(name) = &entry.source {
                let values = model.model.shared_numeric_values(name);
                missing |= values.is_none();
                values.map(|values| Arc::as_ptr(&values) as usize).hash(&mut data_hash);
            }
        }
        // Unloading drops arrays, not the measured values. Reuse their last
        // identity so a completed scan does not trigger a load/evict loop.
        let data_key = if missing && model.state.deferred.is_some() {
            model.reserve_totals_data_key
        } else {
            data_hash.finish()
        };
        data_key.hash(&mut hasher);
        let key = hasher.finish();
        // A settled failure is terminal for the inputs it belongs to. Without
        // this the Setup page, which asks every frame, would restart the scan
        // every frame; and the Block Models stage would see Working for ever
        // instead of Failed.
        if model.reserve_totals_error.is_some() && model.reserve_totals_error_key == Some(key) {
            return;
        }
        if model.reserve_totals_key == Some(key) {
            // A restore that ended without bringing the values back would
            // otherwise leave this key matching for ever, and the stage
            // waiting behind it.
            if model.reserve_totals_awaiting_restore && missing && !self.item_load_pending(crate::model::ItemRef::BlockModel(id)) {
                let model = self.block_models.iter_mut().find(|model| model.id == id).unwrap();
                model.reserve_totals_key = None;
                model.reserve_totals_awaiting_restore = false;
                model.reserve_totals_error = Some(crate::i18n::tr!("planning-reserve-unavailable"));
                model.reserve_totals_error_key = Some(key);
            } else if !model.reserve_totals_awaiting_restore
                && !fields.is_empty()
                && model.reserve_totals.is_empty()
                && !self.pending_jobs.iter().any(|job| job.keys.contains(&JobKey::ReserveStats(id, key)))
            {
                // A cancelled run drops its job without applying the result,
                // stranding the key it committed; the request would then wait
                // for a scan that is never coming. Retire it, and the next
                // pass asks again - every caller that reaches this point
                // polls per frame.
                self.block_models.iter_mut().find(|model| model.id == id).unwrap().reserve_totals_key = None;
            }
            return;
        }
        let data = model.model.clone();
        let mapping = model.reserve_mapping.clone();
        let deferred = model.state.deferred.is_some();
        if missing && deferred {
            let item = ItemRef::BlockModel(id);
            let restoring = self.item_load_pending(item) || self.restore_items_for(vec![item], |app| app.sync_reserve_setup_stats());
            let model = self.block_models.iter_mut().find(|model| model.id == id).unwrap();
            if restoring {
                // Only now is the key safe to record: a restore is genuinely
                // under way, so the next pass has something to wait for. The
                // flag is what lets the next pass tell "still loading" from
                // "the load finished and the values still are not here".
                model.reserve_totals_key = Some(key);
                model.reserve_totals_data_key = data_key;
                model.reserve_totals.clear();
                model.reserve_totals_awaiting_restore = true;
            } else {
                // Nothing is coming. A terminal failure, not a wait: the key
                // is left clear so Recompute Statistics can try again.
                model.reserve_totals_key = None;
                model.reserve_totals_awaiting_restore = false;
                model.reserve_totals_error = Some(crate::i18n::tr!("planning-reserve-unavailable"));
                model.reserve_totals_error_key = Some(key);
            }
            return;
        }
        self.cancel_jobs(|job| matches!(job, JobKey::ReserveStats(model, _) if *model == id));
        let model = self.block_models.iter_mut().find(|model| model.id == id).unwrap();
        model.reserve_totals_key = Some(key);
        model.reserve_totals_data_key = data_key;
        model.reserve_totals.clear();
        model.reserve_totals_awaiting_restore = false;
        model.reserve_totals_error = None;
        self.spawn_job_reporting_progress(
            crate::i18n::tr!("planning-computing-reserves"),
            vec![JobKey::ReserveStats(id, key)],
            move |cancel, _| crate::model::block_model::compute_reserve_totals(&data, &fields, &mapping, cancel),
            move |app, result| {
                if app.workspace.active_project().is_none_or(|project| project.runtime_id != runtime) {
                    return;
                }
                if let Some(model) = app.block_models.iter_mut().find(|model| model.id == id && model.reserve_totals_key == Some(key)) {
                    match result {
                        Ok(totals) => model.reserve_totals = totals,
                        Err(error) => {
                            // A terminal failure, recorded where the stage and
                            // the page can both see it. The key is left clear
                            // so Recompute Statistics starts a fresh scan
                            // rather than an automatic retry loop.
                            model.reserve_totals_key = None;
                            model.reserve_totals_error = Some(format!("{error:#}"));
                            model.reserve_totals_error_key = Some(key);
                            crate::userspace_error!("{error:#}");
                        }
                    }
                }
            },
        );
    }
}

/// Forget one model's statistics and every terminal state attached to them, so
/// the next request is a fresh scan rather than a retry of a settled failure.
fn clear_reserve_totals(model: &mut crate::model::block_model::OpenBlockModel) {
    model.reserve_totals_key = None;
    model.reserve_totals.clear();
    model.reserve_totals_awaiting_restore = false;
    model.reserve_totals_error = None;
    model.reserve_totals_error_key = None;
}
