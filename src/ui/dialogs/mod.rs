//! Floating dialogs and viewport-docked tool panels.
//!
//! Dialogs are grouped by workflow while their draw functions remain
//! re-exported here to keep existing call sites concise.

use crate::model::ObjectId;

pub(crate) mod about;
pub(crate) mod confirmations;
pub(crate) mod drill_hole;
pub(crate) mod editing;
pub(crate) mod files;
pub(crate) mod import_export;
pub(crate) mod plot;
pub(crate) mod triangulation;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetSelectionZDialog {
    pub(crate) object_ids: Vec<ObjectId>,
    pub(crate) z_input: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InsertPointAtElevationDialog {
    pub(crate) object_ids: Vec<ObjectId>,
    pub(crate) elevation: f64,
}
