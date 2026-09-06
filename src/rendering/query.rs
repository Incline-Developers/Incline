//! Unified scene interaction queries, independent of GPU buffer ownership.

use std::collections::HashSet;

use glam::{DMat4, DVec2, DVec3};

use crate::{
    Size,
    model::{
        Document, SceneEntityId,
        drill_hole::{COLLAR_MARKER_MIN_PIXEL_DIAMETER, COLLAR_MARKER_RADIUS_SCALE, DrillHoleRef, MIN_RENDER_PIXEL_DIAMETER, OpenDrillHoleDataset},
        spatial::ObjectSnapIndex,
        triangulation::OpenTriangulation,
    },
    rendering::snap,
    ui::state::CursorMode,
};

pub(crate) struct SceneQuery;

impl SceneQuery {
    pub(crate) fn nearest_surface(
        triangulations: &[OpenTriangulation],
        hidden: &HashSet<SceneEntityId>,
        frozen: Option<&HashSet<SceneEntityId>>,
        ray_origin: DVec3,
        ray_direction: DVec3,
    ) -> Option<(SceneEntityId, DVec3)> {
        triangulations
            .iter()
            .filter(|triangulation| {
                let entity = triangulation.entity_id();
                triangulation.state.loaded && !hidden.contains(&entity) && frozen.is_none_or(|set| !set.contains(&entity))
            })
            .filter_map(|triangulation| {
                triangulation
                    .spatial
                    .ray_hit(&triangulation.mesh, ray_origin, ray_direction)
                    .map(|point| (triangulation.entity_id(), point))
            })
            .min_by(|(_, a), (_, b)| (*a - ray_origin).dot(ray_direction).total_cmp(&(*b - ray_origin).dot(ray_direction)))
    }

    /// Nearest selectable drill hole under a ray, named down to the hole
    /// itself - which dataset it belongs to is [`DrillHoleRef::dataset`].
    /// The hit geometry includes both the camera-facing collar marker and the
    /// down-hole trace. The trace uses the same two-pixel visual floor as the
    /// shader, with an additional pixel tolerance to keep it practical to
    /// click.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn nearest_drill_hole(
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        ray_origin: DVec3,
        ray_direction: DVec3,
        view_direction: DVec3,
        view_projection: &DMat4,
        screen: Size,
        threshold_px: f32,
    ) -> Option<(DrillHoleRef, DVec3)> {
        let mut nearest = f64::INFINITY;
        let mut nearest_hole = None;
        for dataset in drill_holes.iter().filter(|dataset| dataset.state.loaded) {
            let entity = dataset.entity_id();
            if hidden.contains(&entity) || frozen.contains(&entity) {
                continue;
            }
            for (index, hole) in dataset.dataset.holes.iter().enumerate() {
                // The collar is a camera-facing disc substantially wider than
                // the trace. Test that visible marker explicitly; otherwise
                // only the narrow cylinder beneath it can ever be picked.
                // Keep the same small lift toward the camera as the shader so
                // depth ordering agrees with what is on screen.
                let hole_radius = hole.render_radius();
                let collar = hole.collar_position();
                let rendered_hole_radius = screen_floored_radius(collar, hole_radius, view_direction, view_projection, screen, f64::from(MIN_RENDER_PIXEL_DIAMETER), 0.0);
                let collar_radius = screen_floored_radius(
                    collar,
                    hole_radius * COLLAR_MARKER_RADIUS_SCALE,
                    view_direction,
                    view_projection,
                    screen,
                    f64::from(COLLAR_MARKER_MIN_PIXEL_DIAMETER),
                    threshold_px,
                );
                let lifted_collar = collar - view_direction * rendered_hole_radius * 1.5;
                if let Some(distance) = ray_disc_distance(ray_origin, ray_direction, lifted_collar, view_direction, collar_radius)
                    && distance < nearest
                {
                    nearest = distance;
                    nearest_hole = Some(DrillHoleRef { dataset: dataset.id, hole: index });
                }

                for pair in hole.trace.windows(2) {
                    let [start, end] = [pair[0], pair[1]];
                    if end.depth <= start.depth + 1.0e-9 || start.position.distance_squared(end.position) <= 1.0e-18 {
                        continue;
                    }
                    let midpoint_depth = (start.depth + end.depth) * 0.5;
                    if !hole.render_ranges.is_empty() && !hole.render_ranges.iter().any(|(from, to)| *from <= midpoint_depth && midpoint_depth < *to) {
                        continue;
                    }
                    let radius = drill_segment_radius(
                        start.position,
                        end.position,
                        hole.diameter.map(|diameter| diameter * 0.5).unwrap_or(0.0),
                        view_projection,
                        screen,
                        threshold_px,
                    );
                    if let Some(distance) = ray_capped_cylinder_distance(ray_origin, ray_direction, start.position, end.position, radius)
                        && distance < nearest
                    {
                        nearest = distance;
                        nearest_hole = Some(DrillHoleRef { dataset: dataset.id, hole: index });
                    }
                }
            }
        }
        nearest_hole.map(|hole| (hole, ray_origin + ray_direction * nearest))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn snap(
        document: &Document,
        snap_index: &ObjectSnapIndex,
        triangulations: &[OpenTriangulation],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        mode: &CursorMode,
        view_projection: &DMat4,
        screen: Size,
        cursor: (f32, f32),
        threshold: f32,
        xray_enabled: bool,
    ) -> Option<DVec3> {
        let candidate = snap::snap_cursor(document, snap_index, triangulations, hidden, frozen, mode, view_projection, screen, cursor, threshold)?;
        if xray_enabled {
            return Some(candidate.world);
        }
        // Test visibility along the candidate's own screen-space ray. Using the
        // cursor ray here skews the accepted snap region on sloped surfaces:
        // the candidate may be several pixels from the cursor, so the cursor ray
        // can hit the same surface in front of an otherwise visible vertex.
        let (ray_origin, ray_direction) = ray_through_world_point(view_projection, candidate.world)?;
        let candidate_depth = (candidate.world - ray_origin).dot(ray_direction);
        // Surface snapping already found the nearest triangulation along this
        // ray. Other snap modes still need the triangulation visibility test.
        let surface_depth = (!matches!(mode, CursorMode::SnapToSurface))
            .then(|| Self::nearest_surface(triangulations, hidden, None, ray_origin, ray_direction))
            .flatten()
            .map(|(_, point)| (point - ray_origin).dot(ray_direction));
        let document_fill_depth = nearest_opaque_document_fill(document, snap_index, hidden, ray_origin, ray_direction).map(|point| (point - ray_origin).dot(ray_direction));
        let occluder_depth = surface_depth.into_iter().chain(document_fill_depth).min_by(f64::total_cmp);
        // A triangulation must occlude its own back-side vertices too. The small
        // relative tolerance only absorbs ray/triangle floating-point noise at
        // the visible surface; it does not open a path through the mesh.
        if occluder_depth.is_some_and(|depth| {
            let tolerance = 1.0e-5_f64.max(depth.abs() * 1.0e-9);
            candidate_depth > depth + tolerance
        }) {
            None
        } else {
            Some(candidate.world)
        }
    }
}

fn screen_floored_radius(center: DVec3, source_radius: f64, view_direction: DVec3, view_projection: &DMat4, screen: Size, minimum_pixel_diameter: f64, threshold_px: f32) -> f64 {
    let Some(view_direction) = view_direction.try_normalize() else {
        return source_radius;
    };
    let helper = if view_direction.z.abs() > 0.9 { DVec3::Y } else { DVec3::Z };
    let right = helper.cross(view_direction).normalize();
    let up = view_direction.cross(right);
    let viewport = DVec2::new(f64::from(screen.0), f64::from(screen.1));
    let center_clip = *view_projection * center.extend(1.0);
    let safe_w = center_clip.w.abs().max(1.0e-6);
    let pixels_per_world = [right, up]
        .into_iter()
        .map(|radial| {
            let radial_clip = *view_projection * radial.extend(0.0);
            let ndc_per_world = (radial_clip.truncate().truncate() * center_clip.w - center_clip.truncate().truncate() * radial_clip.w) / (safe_w * safe_w);
            (ndc_per_world * viewport * 0.5).length()
        })
        .fold(0.0_f64, f64::max);
    let minimum_radius = (minimum_pixel_diameter * 0.5 + f64::from(threshold_px)) / pixels_per_world.max(1.0e-6);
    source_radius.max(minimum_radius)
}

fn drill_segment_radius(start: DVec3, end: DVec3, source_radius: f64, view_projection: &DMat4, screen: Size, threshold_px: f32) -> f64 {
    let Some(axis) = (end - start).try_normalize() else {
        return source_radius;
    };
    let helper = if axis.z.abs() > 0.9 { DVec3::Y } else { DVec3::Z };
    let right = helper.cross(axis).normalize();
    let up = axis.cross(right);
    let viewport = DVec2::new(f64::from(screen.0), f64::from(screen.1));
    let pixels_per_world = [start, end]
        .into_iter()
        .map(|center| {
            let center_clip = *view_projection * center.extend(1.0);
            let safe_w = center_clip.w.abs().max(1.0e-6);
            [right, up]
                .into_iter()
                .map(|radial| {
                    let radial_clip = *view_projection * radial.extend(0.0);
                    let ndc_per_world = (radial_clip.truncate().truncate() * center_clip.w - center_clip.truncate().truncate() * radial_clip.w) / (safe_w * safe_w);
                    (ndc_per_world * viewport * 0.5).length()
                })
                .fold(0.0_f64, f64::max)
        })
        .fold(f64::INFINITY, f64::min);
    let minimum_radius = (f64::from(MIN_RENDER_PIXEL_DIAMETER) * 0.5 + f64::from(threshold_px)) / pixels_per_world.max(1.0e-6);
    source_radius.max(minimum_radius)
}

/// Distance along a normalized ray to a finite cylinder, including its flat
/// end caps. Returns the first forward intersection.
fn ray_capped_cylinder_distance(origin: DVec3, direction: DVec3, start: DVec3, end: DVec3, radius: f64) -> Option<f64> {
    let axis_vector = end - start;
    let length = axis_vector.length();
    if length <= 1.0e-12 || radius <= 0.0 {
        return None;
    }
    let axis = axis_vector / length;
    let offset = origin - start;
    let direction_axial = direction.dot(axis);
    let offset_axial = offset.dot(axis);
    let direction_radial = direction - axis * direction_axial;
    let offset_radial = offset - axis * offset_axial;
    let a = direction_radial.length_squared();
    let b = 2.0 * direction_radial.dot(offset_radial);
    let c = offset_radial.length_squared() - radius * radius;
    let mut nearest = f64::INFINITY;

    if a > 1.0e-18 {
        let discriminant = b * b - 4.0 * a * c;
        if discriminant >= 0.0 {
            let root = discriminant.sqrt();
            for distance in [(-b - root) / (2.0 * a), (-b + root) / (2.0 * a)] {
                let axial = offset_axial + distance * direction_axial;
                if distance >= 0.0 && axial >= 0.0 && axial <= length {
                    nearest = nearest.min(distance);
                }
            }
        }
    }

    if direction_axial.abs() > 1.0e-18 {
        for cap_axial in [0.0, length] {
            let distance = (cap_axial - offset_axial) / direction_axial;
            let radial = offset_radial + direction_radial * distance;
            if distance >= 0.0 && radial.length_squared() <= radius * radius {
                nearest = nearest.min(distance);
            }
        }
    }

    nearest.is_finite().then_some(nearest)
}

/// Distance along a normalized ray to a camera-facing disc.
fn ray_disc_distance(origin: DVec3, direction: DVec3, center: DVec3, normal: DVec3, radius: f64) -> Option<f64> {
    let normal = normal.try_normalize()?;
    let denominator = direction.dot(normal);
    if denominator.abs() <= 1.0e-18 || radius <= 0.0 {
        return None;
    }
    let distance = (center - origin).dot(normal) / denominator;
    if distance < 0.0 {
        return None;
    }
    let hit = origin + direction * distance;
    (hit.distance_squared(center) <= radius * radius).then_some(distance)
}

fn nearest_opaque_document_fill(document: &Document, snap_index: &ObjectSnapIndex, hidden: &HashSet<SceneEntityId>, ray_origin: DVec3, ray_direction: DVec3) -> Option<DVec3> {
    snap_index.nearest_filled_polyline_hit(ray_origin, ray_direction, |object_index| {
        let Some(object) = document.objects().get(object_index) else {
            return false;
        };
        let entity = SceneEntityId::Object(object.id());
        !hidden.contains(&entity) && document.layer(object.layer()).is_none_or(|layer| layer.loaded) && document.object_fill_rgba(object)[3] >= 1.0 - f32::EPSILON
    })
}

pub(crate) fn ray_through_world_point(view_projection: &DMat4, point: DVec3) -> Option<(DVec3, DVec3)> {
    let clip = *view_projection * point.extend(1.0);
    if clip.w.abs() <= f64::EPSILON {
        return None;
    }

    let ndc = clip.truncate() / clip.w;
    let inverse = view_projection.inverse();
    let near_h = inverse * DVec3::new(ndc.x, ndc.y, 1.0).extend(1.0);
    let far_h = inverse * DVec3::new(ndc.x, ndc.y, 0.0).extend(1.0);
    if near_h.w.abs() <= f64::EPSILON || far_h.w.abs() <= f64::EPSILON {
        return None;
    }

    let near = near_h.truncate() / near_h.w;
    let far = far_h.truncate() / far_h.w;
    let direction = (far - near).try_normalize()?;
    Some((near, direction))
}
