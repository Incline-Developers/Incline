use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use anyhow::Result;
use spade::Triangulation as SpadeTriangulation;

use crate::{
    app::{App, file_name},
    model::{
        Layer, Object, ObjectId, formats,
        formats::mesh_data,
        triangulation::{LoadedTriangulation, OpenTriangulation, TriangulationId},
    },
    ui::state::{ContourOutputLayer, TriPolygonClipMode, TriSurfaceCutSide, TriSurfaceType},
    userspace_log, userspace_warn,
};

// Linear-space value; displays as sRGB 0.8 (204) grey.
const DEFAULT_TRIANGULATION_COLOR: [f32; 4] = [0.6038, 0.6038, 0.6038, 1.0];

#[cfg(target_arch = "wasm32")]
pub(crate) mod browser;
mod contours;
mod creation;
mod cuts;
mod geometry;
mod include;
mod point_cloud_tin;
pub(crate) mod session;

use geometry::*;
pub(crate) use point_cloud_tin::{TerrainBudget, TerrainSampler, TerrainTinParams, terrain_budget_target};
