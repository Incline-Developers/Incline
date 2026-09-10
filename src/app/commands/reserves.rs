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
        self.invalidate_geometry();
    }

    pub(crate) fn rename_reserve_field(&mut self, id: ReserveFieldId, new_name: String) {
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        let new_name = crate::model::project::unique_item_name(
            new_name,
            document.reserve_fields().iter().filter(|field| field.id != id).map(|field| field.name.as_str()),
        );
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
    /// is kept either way - unchecking is reversible - but the Block Models
    /// step shows an excluded model's totals as not counted.
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
        let fields = self.workspace.active_document().map(|document| document.reserve_fields().to_vec()).unwrap_or_default();
        let Some(model) = self.block_models.iter_mut().find(|model| model.id == block_model) else {
            return;
        };
        model.reserve_totals = crate::model::block_model::compute_reserve_totals(&model.model, &fields, &model.reserve_mapping);
    }

    /// Recompute every block model's reserve totals. Used after a Field List
    /// structural change (aggregation change, delete) that could invalidate
    /// more than one model's figures.
    pub(crate) fn recompute_all_reserve_totals(&mut self) {
        let fields = self.workspace.active_document().map(|document| document.reserve_fields().to_vec()).unwrap_or_default();
        for model in &mut self.block_models {
            model.reserve_totals = crate::model::block_model::compute_reserve_totals(&model.model, &fields, &model.reserve_mapping);
        }
    }
}
