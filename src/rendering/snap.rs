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
    rendering::{
        camera::SectionSlab,
        pick::{closest_t_on_segment, perspective_correct_segment_point, slab_clipped_segment, world_to_screen, world_to_screen_unclipped_depth},
    },
    ui::state::CursorMode,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct SnapHit {
    pub(crate) world: DVec3,
}

pub(crate) const SNAP_THRESHOLD_PX: f32 = 15.0;

/// Where the cursor is and how world points reach it. `slab` is the section's
/// two walls when one is up: it snaps to what the section draws, so anything
/// the slab clips away is no target, and a segment snaps only along the run
/// of it that survives the clip.
struct SnapView {
    view_proj: DMat4,
    screen: Size,
    cursor: DVec2,
    slab: Option<SectionSlab>,
}

impl SnapView {
    /// Project a world point to screen pixels. Inside a section the slab is
    /// the depth gate, so the camera's fitted depth range must not also
    /// reject a point the section shows.
    fn project(&self, world: DVec3) -> Option<DVec2> {
        if self.slab.is_some() {
            world_to_screen_unclipped_depth(&self.view_proj, world, self.screen)
        } else {
            world_to_screen(&self.view_proj, world, self.screen)
        }
    }

    fn shows(&self, world: DVec3) -> bool {
        self.slab.is_none_or(|slab| slab.contains(world))
    }
}

/// Running nearest target, in squared screen pixels from the cursor.
struct Nearest {
    hit: Option<SnapHit>,
    distance_sq: f64,
}

impl Nearest {
    fn consider_point(&mut self, view: &SnapView, world: DVec3) {
        if !view.shows(world) {
            return;
        }
        if let Some(screen_point) = view.project(world) {
            let distance = screen_point.distance_squared(view.cursor);
            if distance < self.distance_sq {
                self.distance_sq = distance;
                self.hit = Some(SnapHit { world });
            }
        }
    }

    fn consider_segment(&mut self, view: &SnapView, a: DVec3, b: DVec3) {
        let Some((a, b)) = slab_clipped_segment(view.slab, a, b) else {
            return;
        };
        let (Some(screen_a), Some(screen_b)) = (view.project(a), view.project(b)) else {
            return;
        };
        let t = closest_t_on_segment(view.cursor, screen_a, screen_b);
        let distance = (screen_a + (screen_b - screen_a) * t).distance_squared(view.cursor);
        if distance < self.distance_sq {
            self.distance_sq = distance;
            self.hit = Some(SnapHit {
                world: perspective_correct_segment_point(&view.view_proj, a, b, t),
            });
        }
    }
}

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
    slab: Option<SectionSlab>,
) -> Option<SnapHit> {
    let view = SnapView {
        view_proj: *view_proj,
        screen,
        cursor: DVec2::new(cursor_px.0 as f64, cursor_px.1 as f64),
        slab,
    };
    let threshold = threshold_px as f64;
    let mut nearest = Nearest {
        hit: None,
        distance_sq: threshold * threshold,
    };
    let mut best_surface_depth = f64::INFINITY;

    // BVH narrows candidates to objects whose projected AABB overlaps the cursor region.
    let candidates = snap_index.candidates(view_proj, screen, view.cursor, threshold);

    for obj_idx in candidates {
        let object = &document.objects()[obj_idx];
        let entity = SceneEntityId::Object(object.id());
        if frozen.contains(&entity) || hidden.contains(&entity) {
            continue;
        }
        if !document.layer(object.layer()).map(|l| l.loaded).unwrap_or(true) {
            continue;
        }

        match mode {
            CursorMode::SnapToSurface => {}
            CursorMode::SnapToPoint => match object {
                Object::Point { pos, .. } => nearest.consider_point(&view, *pos),
                Object::Polyline { verts, closed, .. } => {
                    let circle_center = compact_circle_center(verts, *closed);
                    let points = circle_center
                        .iter()
                        .copied()
                        .chain(circle_center.is_none().then_some(()).into_iter().flat_map(|()| verts.iter().map(|v| v.pos)));
                    for point in points {
                        nearest.consider_point(&view, point);
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
                        nearest.consider_segment(&view, a, b);
                    } else {
                        for pair in tessellate_bulge_segment(a, b, bulge).windows(2) {
                            nearest.consider_segment(&view, pair[0], pair[1]);
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
    let surface_ray = matches!(mode, CursorMode::SnapToSurface).then(|| screen_ray(view_proj, screen, view.cursor)).flatten();

    for tri in triangulations {
        let entity = tri.entity_id();
        if !tri.state.loaded || hidden.contains(&entity) || frozen.contains(&entity) {
            continue;
        }
        match mode {
            CursorMode::SnapToSurface => {
                let Some((origin, direction)) = surface_ray else {
                    continue;
                };
                if let Some(hit) = tri.spatial.ray_hit_details_where(&tri.mesh, origin, direction, |point| view.shows(point)) {
                    let depth = (hit.point - origin).dot(direction);
                    if depth < best_surface_depth {
                        best_surface_depth = depth;
                        nearest.hit = Some(SnapHit { world: hit.point });
                    }
                }
            }
            CursorMode::SnapToPoint => {
                tri.spatial.for_each_screen_candidate(&tri.mesh, view_proj, screen, cursor_px, threshold_px, |triangle| {
                    for pos in triangle {
                        nearest.consider_point(&view, pos);
                    }
                });
            }
            CursorMode::SnapToLine => {
                tri.spatial.for_each_screen_candidate(&tri.mesh, view_proj, screen, cursor_px, threshold_px, |triangle| {
                    for (a, b) in [(triangle[0], triangle[1]), (triangle[1], triangle[2]), (triangle[2], triangle[0])] {
                        nearest.consider_segment(&view, a, b);
                    }
                });
            }
            CursorMode::Select => {}
        }
    }

    nearest.hit
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
