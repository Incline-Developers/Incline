//! Eviction and restoration of project payloads. Metadata stays in the explorer;
//! the backing handle is the only owner of an evicted payload.

use std::sync::Arc;

use anyhow::{Context, Result};
use glam::DVec3;

use super::{
    OpenItem,
    asset_storage::Backing,
    formats::omf::{self, DeferredAsset, ProjectSnapshot},
    progress::Phase,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct AssetSummary {
    pub(crate) primary_count: usize,
    pub(crate) secondary_count: usize,
    pub(crate) bounds: Option<(DVec3, DVec3)>,
}

impl OpenItem {
    pub(crate) fn state(&self) -> &super::project::ProjectItemState {
        match self {
            Self::Triangulation(item) => &item.state,
            Self::BlockModel(item) => &item.state,
            Self::DrillHole(item) => &item.state,
            Self::PointCloud(item) => &item.state,
            Self::Raster(item) => &item.state,
        }
    }

    pub(crate) fn state_mut(&mut self) -> &mut super::project::ProjectItemState {
        match self {
            Self::Triangulation(item) => &mut item.state,
            Self::BlockModel(item) => &mut item.state,
            Self::DrillHole(item) => &mut item.state,
            Self::PointCloud(item) => &mut item.state,
            Self::Raster(item) => &mut item.state,
        }
    }

    pub(crate) fn evict(mut self, progress: &Phase) -> Result<Self> {
        if self.state().deferred.is_some() {
            return Ok(self);
        }
        let mut snapshot = ProjectSnapshot::default();
        match &self {
            Self::Triangulation(item) => snapshot.triangulations.push((**item).clone()),
            Self::BlockModel(item) => snapshot.block_models.push((**item).clone()),
            Self::DrillHole(item) => snapshot.drill_holes.push((**item).clone()),
            Self::PointCloud(item) => snapshot.point_clouds.push((**item).clone()),
            Self::Raster(item) => snapshot.rasters.push((**item).clone()),
        }
        let bytes = omf::to_bytes(snapshot, progress)?;
        let backing = Backing::write(&bytes)?;
        self.state_mut().deferred = Some(DeferredAsset { backing, element_path: vec![0] });
        self.release_payload();
        Ok(self)
    }

    pub(crate) fn release_payload(&mut self) {
        let summary = match self {
            Self::Triangulation(item) => {
                let bounds = item.mesh.bounds();
                let summary = AssetSummary {
                    primary_count: item.mesh.vertex_count(),
                    secondary_count: item.mesh.face_count(),
                    bounds: Some((DVec3::from_array(bounds.min.as_array()), DVec3::from_array(bounds.max.as_array()))),
                };
                item.mesh = Arc::new(super::formats::mesh_data::Triangulation::empty());
                item.spatial = Arc::new(super::spatial::TriangleBvh::build(&item.mesh));
                item.edges = Vec::new();
                item.surface_face_order = Arc::new(Vec::new());
                summary
            }
            Self::BlockModel(item) => {
                let summary = AssetSummary {
                    primary_count: item.renderable_block_indices.len(),
                    secondary_count: item.model.metadata.variables.len(),
                    bounds: item.world_bounds,
                };
                item.model.release_values();
                item.blocks = Arc::new(super::block_model::BlockBoundsSource::Explicit(Vec::new()));
                item.renderable_block_indices = Arc::new(super::block_model::RenderableBlockIndices::All(0));
                item.active_values_cache = Default::default();
                summary
            }
            Self::DrillHole(item) => {
                let summary = AssetSummary {
                    primary_count: item.dataset.holes.len(),
                    secondary_count: item.dataset.fields.len(),
                    bounds: item.dataset.bounds,
                };
                item.dataset = Arc::new(super::drill_hole::DrillHoleDataset::new(Vec::new()));
                summary
            }
            Self::PointCloud(item) => {
                let summary = AssetSummary {
                    primary_count: item.points.len(),
                    secondary_count: 0,
                    bounds: Some(item.bounds),
                };
                item.points = Arc::new(Vec::new());
                item.colors = None;
                item.prepared = Arc::new(super::point_cloud::prepare_for_render(&[], None, item.bounds));
                summary
            }
            Self::Raster(item) => {
                item.full_rgba = Arc::new(Vec::new());
                item.rgba = Arc::new(Vec::new());
                AssetSummary::default()
            }
        };
        self.state_mut().summary = Some(summary);
    }

    /// Restore only payload fields; edits to the unloaded row's name/style/ID
    /// remain authoritative, even when they differ from the backing archive.
    pub(crate) fn materialize(mut self) -> Result<Self> {
        let Some(deferred) = self.state().deferred.clone() else {
            return Ok(self);
        };
        let mut bundle = deferred.read()?;
        match &mut self {
            Self::Triangulation(item) => {
                let data = bundle.triangulations.pop().context("backing contains no triangulation")?.loaded;
                item.mesh = data.mesh;
                item.spatial = data.spatial;
                item.edges = data.edges;
                item.surface_face_order = data.surface_face_order;
            }
            Self::BlockModel(item) => {
                let imported = bundle.block_models.pop().context("backing contains no block model")?;
                let data = imported.loaded;
                item.model = data.model;
                item.blocks = data.blocks;
                item.renderable_block_indices = data.renderable_block_indices;
                item.uniform_grid = data.uniform_grid;
                item.opaque_surface_blocks = data.opaque_surface_blocks;
                item.world_bounds = data.world_bounds;
                for (name, transfer) in imported.color_transfers {
                    item.color_transfers.entry(name).or_insert(transfer);
                }
                item.active_values_cache =
                    super::block_model::OpenBlockModel::prepare_active_values_cache(&item.model, &item.renderable_block_indices, item.active_color_variable.as_deref());
            }
            Self::DrillHole(item) => {
                item.dataset = bundle.drill_holes.pop().context("backing contains no drillholes")?.loaded.dataset;
            }
            Self::PointCloud(item) => {
                let data = bundle.point_clouds.pop().context("backing contains no point cloud")?.loaded;
                item.points = data.points;
                item.colors = data.colors;
                item.prepared = data.prepared;
                item.bounds = data.bounds;
            }
            Self::Raster(item) => {
                let data = bundle.rasters.pop().context("backing contains no raster")?.loaded;
                item.full_rgba = data.full_rgba;
                item.rgba = data.rgba;
                item.source_size = data.source_size;
                item.preview_size = data.preview_size;
                item.world_to_uv = data.world_to_uv;
                item.projection = data.projection;
            }
        }
        self.state_mut().deferred = None;
        self.state_mut().summary = None;
        Ok(self)
    }
}
