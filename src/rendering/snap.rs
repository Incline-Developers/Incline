use std::collections::HashSet;

use glam::{DMat4, DVec2, DVec3};

use crate::{
    Size,
    model::{
        Document, Object, SceneEntityId,
        geometry::{compact_circle_center, tessellate_bulge_segment},
        spatial::ObjectSnapIndex,
        triangulation::OpenTriangulation,
    },
    rendering::pick::{closest_t_on_segment, perspective_correct_segment_point, world_to_screen},
    ui::state::CursorMode,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct SnapHit {
    pub(crate) world: DVec3,
}

pub(crate) const SNAP_THRESHOLD_PX: f32 = 15.0;

/// Find the nearest snap target to `cursor_px` given `mode`.
///
/// Returns `Some(world_pos)` if any target is within `threshold_px`,
/// otherwise `None` (caller should fall back to the raw cursor ray).
#[allow(clippy::too_many_arguments)]
pub(crate) fn snap_cursor(
    document: &Document,
    snap_index: &ObjectSnapIndex,
    triangulations: &[OpenTriangulation],
    hidden: &HashSet<SceneEntityId>,
    frozen: &HashSet<SceneEntityId>,
    mode: &CursorMode,
    view_proj: &DMat4,
    screen: Size,
    cursor_px: (f32, f32),
    threshold_px: f32,
) -> Option<SnapHit> {
    let cursor = DVec2::new(cursor_px.0 as f64, cursor_px.1 as f64);
    let threshold = threshold_px as f64;
    let mut best_dist_sq = threshold * threshold;
    let mut best: Option<SnapHit> = None;
    let mut best_surface_depth = f64::INFINITY;

    // BVH narrows candidates to objects whose projected AABB overlaps the cursor region.
    let candidates = snap_index.candidates(view_proj, screen, cursor, threshold);

    for obj_idx in candidates {
        let object = &document.objects()[obj_idx];
        let entity = SceneEntityId::Object(object.id());
        if frozen.contains(&entity) || hidden.contains(&entity) {
            continue;
        }
        if !document.layer(object.layer()).map(|l| l.visible).unwrap_or(true) {
            continue;
        }

        match mode {
            CursorMode::SnapToSurface => {}
            CursorMode::SnapToPoint => match object {
                Object::Point { pos, .. } => {
                    if let Some(sp) = world_to_screen(view_proj, *pos, screen) {
                        let d = sp.distance_squared(cursor);
                        if d < best_dist_sq {
                            best_dist_sq = d;
                            best = Some(SnapHit { world: *pos });
                        }
                    }
                }
                Object::Polyline { verts, closed, .. } => {
                    let circle_center = compact_circle_center(verts, *closed);
                    let points = circle_center
                        .iter()
                        .copied()
                        .chain(circle_center.is_none().then_some(()).into_iter().flat_map(|()| verts.iter().map(|v| v.pos)));
                    for point in points {
                        if let Some(sp) = world_to_screen(view_proj, point, screen) {
                            let d = sp.distance_squared(cursor);
                            if d < best_dist_sq {
                                best_dist_sq = d;
                                best = Some(SnapHit { world: point });
                            }
                        }
                    }
                }
                _ => {}
            },

            CursorMode::SnapToLine => {
                let (verts, closed) = match object {
                    Object::Polyline { verts, closed, .. } => (verts.as_slice(), *closed),
                    _ => continue,
                };
                let n = verts.len();
                if n < 2 {
                    continue;
                }
                let seg_count = if closed { n } else { n - 1 };
                for i in 0..seg_count {
                    let a = verts[i].pos;
                    let b = verts[(i + 1) % n].pos;
                    let bulge = verts[i].bulge;
                    if bulge.abs() <= f64::EPSILON {
                        let (Some(sa), Some(sb)) = (world_to_screen(view_proj, a, screen), world_to_screen(view_proj, b, screen)) else {
                            continue;
                        };
                        let t = closest_t_on_segment(cursor, sa, sb);
                        let d = (sa + (sb - sa) * t).distance_squared(cursor);
                        if d < best_dist_sq {
                            best_dist_sq = d;
                            best = Some(SnapHit {
                                world: perspective_correct_segment_point(view_proj, a, b, t),
                            });
                        }
                    } else {
                        let points = tessellate_bulge_segment(a, b, bulge);
                        for pair in points.windows(2) {
                            let (Some(sa), Some(sb)) = (world_to_screen(view_proj, pair[0], screen), world_to_screen(view_proj, pair[1], screen)) else {
                                continue;
                            };
                            let t = closest_t_on_segment(cursor, sa, sb);
                            let d = (sa + (sb - sa) * t).distance_squared(cursor);
                            if d < best_dist_sq {
                                best_dist_sq = d;
                                best = Some(SnapHit {
                                    world: perspective_correct_segment_point(view_proj, pair[0], pair[1], t),
                                });
                            }
                        }
                    }
                }
            }

            CursorMode::Select => {}
        }
    }

    // Surface snapping is a nearest-hit ray cast: the triangle under the
    // cursor is exactly the first surface along the cursor ray, and the BVH
    // answers that in O(log n) instead of projecting every candidate
    // triangle of a dense mesh to screen space.
    let surface_ray = matches!(mode, CursorMode::SnapToSurface).then(|| screen_ray(view_proj, screen, cursor)).flatten();

    for tri in triangulations {
        let entity = tri.entity_id();
        if !tri.visible || hidden.contains(&entity) || frozen.contains(&entity) {
            continue;
        }
        match mode {
            CursorMode::SnapToSurface => {
                let Some((origin, direction)) = surface_ray else {
                    continue;
                };
                if let Some(world) = tri.spatial.ray_hit(&tri.mesh, origin, direction) {
                    let depth = (world - origin).dot(direction);
                    if depth < best_surface_depth {
                        best_surface_depth = depth;
                        best = Some(SnapHit { world });
                    }
                }
            }
            CursorMode::SnapToPoint => {
                tri.spatial.for_each_screen_candidate(&tri.mesh, view_proj, screen, cursor_px, threshold_px, |triangle| {
                    for pos in triangle {
                        if let Some(sp) = world_to_screen(view_proj, pos, screen) {
                            let d = sp.distance_squared(cursor);
                            if d < best_dist_sq {
                                best_dist_sq = d;
                                best = Some(SnapHit { world: pos });
                            }
                        }
                    }
                });
            }
            CursorMode::SnapToLine => {
                tri.spatial.for_each_screen_candidate(&tri.mesh, view_proj, screen, cursor_px, threshold_px, |triangle| {
                    for (a, b) in [(triangle[0], triangle[1]), (triangle[1], triangle[2]), (triangle[2], triangle[0])] {
                        let (Some(sa), Some(sb)) = (world_to_screen(view_proj, a, screen), world_to_screen(view_proj, b, screen)) else {
                            continue;
                        };
                        let ab = sb - sa;
                        let t = closest_t_on_segment(cursor, sa, sb);
                        let d = (sa + ab * t).distance_squared(cursor);
                        if d < best_dist_sq {
                            best_dist_sq = d;
                            best = Some(SnapHit {
                                world: perspective_correct_segment_point(view_proj, a, b, t),
                            });
                        }
                    }
                });
            }
            CursorMode::Select => {}
        }
    }

    best
}

/// World-space ray through a screen pixel, from the near plane forward.
fn screen_ray(view_proj: &DMat4, screen: Size, cursor: DVec2) -> Option<(DVec3, DVec3)> {
    let ndc_x = cursor.x / screen.0 as f64 * 2.0 - 1.0;
    let ndc_y = 1.0 - cursor.y / screen.1 as f64 * 2.0;
    let inverse = view_proj.inverse();
    let near_h = inverse * glam::DVec4::new(ndc_x, ndc_y, 1.0, 1.0);
    let far_h = inverse * glam::DVec4::new(ndc_x, ndc_y, 0.0, 1.0);
    if near_h.w.abs() <= f64::EPSILON || far_h.w.abs() <= f64::EPSILON {
        return None;
    }
    let near = near_h.truncate() / near_h.w;
    let far = far_h.truncate() / far_h.w;
    let direction = (far - near).try_normalize()?;
    Some((near, direction))
}
