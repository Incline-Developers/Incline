//! Scene bounds helpers.

use glam::DVec3;

use crate::model::{
    Document, Object, SceneEntityId,
    block_model::OpenBlockModel,
    drill_hole::OpenDrillHoleDataset,
    geometry::{polyline_bulge_bounds, text_bounds_corners},
    point_cloud::OpenPointCloud,
    triangulation::OpenTriangulation,
};

pub(crate) fn scene_bounds(
    document: &Document,
    triangulations: &[OpenTriangulation],
    block_models: &[OpenBlockModel],
    drill_holes: &[OpenDrillHoleDataset],
    point_clouds: &[OpenPointCloud],
    hidden: &std::collections::HashSet<SceneEntityId>,
) -> Option<(DVec3, DVec3)> {
    let mut min = DVec3::splat(f64::MAX);
    let mut max = DVec3::splat(f64::MIN);
    let mut any = false;
    for_each_visible_object_aabb(document, triangulations, block_models, drill_holes, point_clouds, hidden, &mut |object_min, object_max| {
        min = min.min(object_min);
        max = max.max(object_max);
        any = true;
    });
    any.then_some((min, max))
}

/// World-space AABB of every visible object, one entry per object. Depth-range
/// fitting needs these individually (not the merged scene AABB) so a distant
/// model that is outside the current view sideways can be rejected before it
/// blows up the near/far clip range.
pub(crate) fn visible_object_aabbs(
    document: &Document,
    triangulations: &[OpenTriangulation],
    block_models: &[OpenBlockModel],
    drill_holes: &[OpenDrillHoleDataset],
    point_clouds: &[OpenPointCloud],
    hidden: &std::collections::HashSet<SceneEntityId>,
) -> Vec<(DVec3, DVec3)> {
    let mut aabbs = Vec::new();
    for_each_visible_object_aabb(document, triangulations, block_models, drill_holes, point_clouds, hidden, &mut |object_min, object_max| {
        aabbs.push((object_min, object_max))
    });
    aabbs
}

/// Shared visible-object iteration. Each object contributes one AABB; text,
/// polylines and roads collapse their many points into a single min/max so
/// callers see one box per object.
fn for_each_visible_object_aabb(
    document: &Document,
    triangulations: &[OpenTriangulation],
    block_models: &[OpenBlockModel],
    drill_holes: &[OpenDrillHoleDataset],
    point_clouds: &[OpenPointCloud],
    hidden: &std::collections::HashSet<SceneEntityId>,
    emit: &mut impl FnMut(DVec3, DVec3),
) {
    let hidden_layers: std::collections::HashSet<_> = document.layers().iter().filter(|layer| !layer.visible).map(|layer| layer.id).collect();
    for object in document.objects() {
        if hidden_layers.contains(&object.layer()) || hidden.contains(&SceneEntityId::Object(object.id())) {
            continue;
        }

        let mut object_min = DVec3::splat(f64::MAX);
        let mut object_max = DVec3::splat(f64::MIN);
        let mut object_any = false;
        let mut include = |point: DVec3| {
            object_min = object_min.min(point);
            object_max = object_max.max(point);
            object_any = true;
        };
        match object {
            Object::Point { pos, .. } => include(*pos),
            Object::Text {
                pos, content, height, rotation, ..
            } => {
                for corner in text_bounds_corners(*pos, content, *height, *rotation) {
                    include(corner);
                }
            }
            Object::Polyline { verts, closed, .. } => {
                if let Some((verts_min, verts_max)) = polyline_bulge_bounds(verts, *closed) {
                    include(verts_min);
                    include(verts_max);
                }
            }
        }
        if object_any {
            emit(object_min, object_max);
        }
    }

    for triangulation in triangulations
        .iter()
        .filter(|triangulation| triangulation.state.loaded && triangulation.visible && !hidden.contains(&triangulation.entity_id()))
    {
        let bounds = triangulation.mesh.bounds();
        emit(DVec3::new(bounds.min.x, bounds.min.y, bounds.min.z), DVec3::new(bounds.max.x, bounds.max.y, bounds.max.z));
    }

    for block_model in block_models
        .iter()
        .filter(|block_model| block_model.state.loaded && block_model.visible && !hidden.contains(&block_model.entity_id()))
    {
        if let Some((block_min, block_max)) = block_model.visible_world_bounds() {
            emit(block_min, block_max);
        }
    }

    for dataset in drill_holes
        .iter()
        .filter(|dataset| dataset.state.loaded && dataset.visible && !hidden.contains(&dataset.entity_id()))
    {
        if let Some((min, max)) = dataset.dataset.bounds {
            emit(min, max);
        }
    }

    for point_cloud in point_clouds
        .iter()
        .filter(|point_cloud| point_cloud.state.loaded && point_cloud.visible && !hidden.contains(&point_cloud.entity_id()))
    {
        let (cloud_min, cloud_max) = point_cloud.bounds;
        emit(cloud_min, cloud_max);
    }
}
