//! Floating dialogs and viewport-docked tool panels.
//!
//! Dialogs are grouped by workflow while their draw functions remain
//! re-exported here to keep existing call sites concise.

use crate::model::{Axis, ObjectId};

pub(crate) mod about;
pub(crate) mod confirmations;
pub(crate) mod drill_hole;
pub(crate) mod editing;
pub(crate) mod files;
pub(crate) mod import_export;
pub(crate) mod plot;
pub(crate) mod triangulation;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MoveToAxisDialog {
    pub(crate) object_ids: Vec<ObjectId>,
    pub(crate) axis: Axis,
    pub(crate) value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InsertPointAtElevationDialog {
    pub(crate) object_ids: Vec<ObjectId>,
    pub(crate) elevation: f64,
    /// Lowest and highest vertex Z across `object_ids`; nothing can be
    /// inserted outside that band, so the entry box is bounded to it.
    pub(crate) min_elevation: f64,
    pub(crate) max_elevation: f64,
}
