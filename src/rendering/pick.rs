//! Geometry picking against the world-space vertex buffers the renderer
//! already produces.
//!
//! Each top-level entity records the range of stroke/fill vertices it emitted
//! (`PickRecord`). To pick, we project those world-space vertices to the screen
//! and find the geometry nearest the cursor, returning its true world position
//! (including Z) and owning entity handle.

use std::{collections::HashSet, ops::Range};

use glam::{DMat4, DVec2, DVec3};

use crate::{
    Size,
    model::{
        Document, Object, ObjectId, ObjectPoint, SceneEntityId,
        geometry::compact_circle_center,
        spatial::{ObjectSnapIndex, projected_box_overlaps},
    },
    rendering::{StrokeVertex, Vertex},
};

#[derive(Clone, Debug)]
pub(crate) struct PickRecord {
    pub(crate) entity: SceneEntityId,
    /// Cached world-space bounds of all rendered vertices owned by this
    /// record. Picking projects this small box before touching potentially
    /// very large CPU vertex/index ranges.
    pub(crate) world_bounds: (DVec3, DVec3),
    /// Half-open range into the stroke vertex buffer.
    pub(crate) stroke_range: (u32, u32),
    /// Half-open range into the stroke index buffer, used for picking.
    pub(crate) stroke_index_range: (u32, u32),
    /// Half-open range into the fill vertex buffer.
    pub(crate) fill_range: (u32, u32),
    /// Half-open range into the fill index buffer.
    pub(crate) fill_index_range: (u32, u32),
    /// Whether this record's solid fill fully occludes geometry behind it.
    pub(crate) fill_opaque: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PickHit {
    pub(crate) entity: SceneEntityId,
    pub(crate) world: DVec3,
}

/// One set of pick records with the CPU vertex/index buffers their ranges
/// index into. The per-rebuild stream is one group; each static stroke chunk
/// is another (with no fill geometry).
#[derive(Clone, Copy)]
pub(crate) struct PickGeometry<'a> {
    /// Optional cached bounds for the whole stream chunk. A cursor outside it
    /// skips every member record without touching the CPU geometry.
    pub(crate) world_bounds: Option<(DVec3, DVec3)>,
    pub(crate) records: &'a [PickRecord],
    pub(crate) stroke_verts: &'a [StrokeVertex],
    pub(crate) stroke_indices: &'a [u32],
    pub(crate) fill_verts: &'a [Vertex],
    pub(crate) fill_indices: &'a [u32],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TextPickRecord {
    pub(crate) entity: SceneEntityId,
    pub(crate) corners: [DVec3; 4],
}

/// Project a world point to physical screen pixels. Returns `None` when the
/// point is on/behind the camera plane (`w <= 0`).
pub(crate) fn world_to_screen(view_proj: &DMat4, world: DVec3, screen: Size) -> Option<DVec2> {
    let clip = *view_proj * world.extend(1.0);
    if clip.w.abs() <= f64::EPSILON {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if !(0.0..=1.0).contains(&ndc.z) {
        return None;
    }
    Some(DVec2::new((ndc.x * 0.5 + 0.5) * screen.0 as f64, (0.5 - ndc.y * 0.5) * screen.1 as f64))
}

/// Project a world point to physical screen pixels without rejecting points
/// outside the camera's fitted depth range. This is for foreground overlays
/// whose screen position remains meaningful even when scene clipping excludes
/// the world point itself.
pub(crate) fn world_to_screen_unclipped_depth(view_proj: &DMat4, world: DVec3, screen: Size) -> Option<DVec2> {
    let clip = *view_proj * world.extend(1.0);
    if clip.w.abs() <= f64::EPSILON {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    ndc.is_finite()
        .then(|| DVec2::new((ndc.x * 0.5 + 0.5) * screen.0 as f64, (0.5 - ndc.y * 0.5) * screen.1 as f64))
}

/// Parameter `t in [0, 1]` of the closest point on segment `a-b` to `p`.
pub(crate) fn closest_t_on_segment(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= f64::EPSILON {
        return 0.0;
    }
    ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
}

/// Clamp a recorded half-open buffer range to the data that survived GPU
/// stream truncation. A record may begin beyond the retained prefix; that is
/// an empty range, not a slice with `start > end`.
pub(crate) fn clamped_range(range: (u32, u32), len: usize) -> Range<usize> {
    let start = (range.0 as usize).min(len);
    let end = (range.1 as usize).min(len).max(start);
    start..end
}

/// Compute world-space bounds from renderer-local f32 positions. Scene builds
/// call this once when creating a pick record; cursor polls reuse the result.
pub(crate) fn world_bounds_from_local_positions(positions: impl Iterator<Item = [f32; 3]>, scene_origin: DVec3) -> Option<(DVec3, DVec3)> {
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    let mut any = false;
    for position in positions {
        let world = DVec3::from_array(position.map(f64::from)) + scene_origin;
        if !world.is_finite() {
            continue;
        }
        min = min.min(world);
        max = max.max(world);
        any = true;
    }
    any.then_some((min, max))
}

/// Recover the world-space point corresponding to a screen-space parameter
/// along a projected segment. Perspective projection is affine only after
/// dividing by clip W, so interpolating the endpoints directly with
/// `screen_t` returns the wrong point whenever their depths differ.
pub(crate) fn perspective_correct_segment_point(view_proj: &DMat4, a: DVec3, b: DVec3, screen_t: f64) -> DVec3 {
    let t = screen_t.clamp(0.0, 1.0);
    let wa = (*view_proj * a.extend(1.0)).w;
    let wb = (*view_proj * b.extend(1.0)).w;
    if wa.abs() <= f64::EPSILON || wb.abs() <= f64::EPSILON {
        return a.lerp(b, t);
    }
    let a_weight = (1.0 - t) / wa;
    let b_weight = t / wb;
    let denominator = a_weight + b_weight;
    if denominator.abs() <= f64::EPSILON || !denominator.is_finite() {
        return a.lerp(b, t);
    }
    (a * a_weight + b * b_weight) / denominator
}

fn perspective_correct_triangle_point(view_proj: &DMat4, points: [DVec3; 3], screen_weights: DVec3) -> DVec3 {
    let clip_w = points.map(|point| (*view_proj * point.extend(1.0)).w);
    if clip_w.iter().any(|w| w.abs() <= f64::EPSILON) {
        return points[0] * screen_weights.x + points[1] * screen_weights.y + points[2] * screen_weights.z;
    }
    let weights = DVec3::new(screen_weights.x / clip_w[0], screen_weights.y / clip_w[1], screen_weights.z / clip_w[2]);
    let denominator = weights.element_sum();
    if denominator.abs() <= f64::EPSILON || !denominator.is_finite() {
        return points[0] * screen_weights.x + points[1] * screen_weights.y + points[2] * screen_weights.z;
    }
    (points[0] * weights.x + points[1] * weights.y + points[2] * weights.z) / denominator
}

fn projected_depth(view_proj: &DMat4, world: DVec3) -> f64 {
    let clip = *view_proj * world.extend(1.0);
    if clip.w.abs() <= f64::EPSILON {
        f64::INFINITY
    } else {
        // Reversed-Z stores nearer geometry at larger NDC depth. Negate it so
        // the existing CPU pick ordering can continue treating smaller as nearer.
        -(clip.z / clip.w)
    }
}

fn screen_hit_is_better(distance: f64, depth: f64, best_distance: f64, best_depth: f64) -> bool {
    const DISTANCE_TIE_EPSILON: f64 = 1.0e-6;
    distance < best_distance - DISTANCE_TIE_EPSILON || ((distance - best_distance).abs() <= DISTANCE_TIE_EPSILON && depth < best_depth)
}

fn record_projects_near_cursor(record: &PickRecord, view_proj: &DMat4, screen: Size, cursor: DVec2, threshold: f64) -> bool {
    projected_box_overlaps(record.world_bounds.0, record.world_bounds.1, view_proj, screen, cursor, threshold)
}

/// Find the entity geometry nearest the cursor within `threshold_px`.
///
/// Rendered stroke quads emit a fixed two-triangle index pattern. We recognize
/// that pattern and test the centerline once, rather than testing every
/// tessellated triangle edge. Screen-space markers and round joins use other
/// index patterns, so they fall back to the triangle-edge path below.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pick_nearest(
    groups: &[PickGeometry<'_>],
    scene_origin: DVec3,
    view_proj: &DMat4,
    screen: Size,
    cursor_px: (f32, f32),
    threshold_px: f32,
    frozen: &HashSet<SceneEntityId>,
) -> Option<PickHit> {
    let cursor = DVec2::new(cursor_px.0 as f64, cursor_px.1 as f64);
    let mut best_dist = threshold_px as f64;
    let mut best_stroke_depth = f64::INFINITY;
    let mut best_stroke_hit: Option<PickHit> = None;
    let mut best_fill_vertex_dist = threshold_px as f64;
    let mut best_fill_vertex_depth = f64::INFINITY;
    let mut best_fill_vertex_hit: Option<PickHit> = None;
    let mut best_fill_hit: Option<PickHit> = None;
    let mut best_fill_depth = f64::INFINITY;
    let mut best_opaque_fill_hit: Option<PickHit> = None;
    let mut best_opaque_fill_depth = f64::INFINITY;

    for group in groups {
        let PickGeometry {
            world_bounds,
            records,
            stroke_verts,
            stroke_indices,
            fill_verts,
            fill_indices,
        } = *group;
        if world_bounds.is_some_and(|bounds| !projected_box_overlaps(bounds.0, bounds.1, view_proj, screen, cursor, threshold_px as f64)) {
            continue;
        }
        for rec in records {
            // Frozen entities are visible but not selectable.
            if frozen.contains(&rec.entity) || !record_projects_near_cursor(rec, view_proj, screen, cursor, threshold_px as f64) {
                continue;
            }
            let stroke_indices = &stroke_indices[clamped_range(rec.stroke_index_range, stroke_indices.len())];
            let mut tested_stroke_centerlines = false;
            for quad in stroke_indices.as_chunks::<6>().0 {
                let Some(base) = stroke_line_quad_base(quad) else {
                    continue;
                };
                let (Some(a), Some(b)) = (stroke_verts.get(base), stroke_verts.get(base + 2)) else {
                    continue;
                };
                let wa = DVec3::from_array(a.pos.map(f64::from)) + scene_origin;
                let wb = DVec3::from_array(b.pos.map(f64::from)) + scene_origin;
                if (wb - wa).length_squared() <= f64::EPSILON {
                    continue;
                }
                tested_stroke_centerlines = true;
                if let (Some(sa), Some(sb)) = (world_to_screen(view_proj, wa, screen), world_to_screen(view_proj, wb, screen)) {
                    let t = closest_t_on_segment(cursor, sa, sb);
                    let dist = (sa + (sb - sa) * t).distance(cursor);
                    let world = perspective_correct_segment_point(view_proj, wa, wb, t);
                    let depth = projected_depth(view_proj, world);
                    if screen_hit_is_better(dist, depth, best_dist, best_stroke_depth) {
                        best_dist = dist;
                        best_stroke_depth = depth;
                        best_stroke_hit = Some(PickHit { entity: rec.entity, world });
                    }
                }
            }

            if !tested_stroke_centerlines {
                for triangle in stroke_indices.as_chunks::<3>().0 {
                    for (a, b) in [(triangle[0], triangle[1]), (triangle[1], triangle[2]), (triangle[2], triangle[0])] {
                        let (Some(a), Some(b)) = (stroke_verts.get(a as usize), stroke_verts.get(b as usize)) else {
                            continue;
                        };
                        let wa = DVec3::from_array(a.pos.map(f64::from)) + scene_origin;
                        let wb = DVec3::from_array(b.pos.map(f64::from)) + scene_origin;
                        if let (Some(sa), Some(sb)) = (world_to_screen(view_proj, wa, screen), world_to_screen(view_proj, wb, screen)) {
                            let t = closest_t_on_segment(cursor, sa, sb);
                            let dist = (sa + (sb - sa) * t).distance(cursor);
                            let world = perspective_correct_segment_point(view_proj, wa, wb, t);
                            let depth = projected_depth(view_proj, world);
                            if screen_hit_is_better(dist, depth, best_dist, best_stroke_depth) {
                                best_dist = dist;
                                best_stroke_depth = depth;
                                best_stroke_hit = Some(PickHit { entity: rec.entity, world });
                            }
                        }
                    }
                }
            }

            for vert in &fill_verts[clamped_range(rec.fill_range, fill_verts.len())] {
                let world = DVec3::from_array(vert.pos.map(f64::from)) + scene_origin;
                if let Some(sp) = world_to_screen(view_proj, world, screen) {
                    let dist = sp.distance(cursor);
                    let depth = projected_depth(view_proj, world);
                    if screen_hit_is_better(dist, depth, best_fill_vertex_dist, best_fill_vertex_depth) {
                        best_fill_vertex_dist = dist;
                        best_fill_vertex_depth = depth;
                        best_fill_vertex_hit = Some(PickHit { entity: rec.entity, world });
                    }
                }
            }

            let record_fill_indices = &fill_indices[clamped_range(rec.fill_index_range, fill_indices.len())];
            for triangle in record_fill_indices.as_chunks::<3>().0 {
                let [Some(a), Some(b), Some(c)] = [
                    fill_verts.get(triangle[0] as usize),
                    fill_verts.get(triangle[1] as usize),
                    fill_verts.get(triangle[2] as usize),
                ] else {
                    continue;
                };
                let wa = DVec3::from_array(a.pos.map(f64::from)) + scene_origin;
                let wb = DVec3::from_array(b.pos.map(f64::from)) + scene_origin;
                let wc = DVec3::from_array(c.pos.map(f64::from)) + scene_origin;
                let (Some(sa), Some(sb), Some(sc)) = (
                    world_to_screen(view_proj, wa, screen),
                    world_to_screen(view_proj, wb, screen),
                    world_to_screen(view_proj, wc, screen),
                ) else {
                    continue;
                };
                if let Some(weights) = triangle_weights(cursor, sa, sb, sc) {
                    let world = perspective_correct_triangle_point(view_proj, [wa, wb, wc], weights);
                    let depth = projected_depth(view_proj, world);
                    if depth < best_fill_depth {
                        best_fill_depth = depth;
                        best_fill_hit = Some(PickHit { entity: rec.entity, world });
                    }
                    if rec.fill_opaque && depth < best_opaque_fill_depth {
                        best_opaque_fill_depth = depth;
                        best_opaque_fill_hit = Some(PickHit { entity: rec.entity, world });
                    }
                }
            }
        }
    }

    let foreground = best_stroke_hit
        .map(|hit| (hit, best_stroke_depth))
        .or_else(|| best_fill_vertex_hit.map(|hit| (hit, best_fill_vertex_depth)));
    if let Some((hit, depth)) = foreground {
        if best_opaque_fill_depth + 1.0e-9 < depth {
            return best_opaque_fill_hit;
        }
        return Some(hit);
    }
    best_fill_hit
}

fn stroke_line_quad_base(indices: &[u32]) -> Option<usize> {
    let [a, b, c, d, e, f]: [u32; 6] = indices.try_into().ok()?;
    (d == b && c == f && a == b.wrapping_add(1) && e == b.wrapping_add(2) && c == b.wrapping_add(3)).then_some(b as usize)
}

pub(crate) fn pick_text(records: &[TextPickRecord], view_proj: &DMat4, screen: Size, cursor_px: (f32, f32), frozen: &HashSet<SceneEntityId>) -> Option<PickHit> {
    let cursor = DVec2::new(f64::from(cursor_px.0), f64::from(cursor_px.1));
    let mut best: Option<(f64, f64, PickHit)> = None;
    for record in records {
        if frozen.contains(&record.entity) {
            continue;
        }
        let [Some(a), Some(b), Some(c), Some(d)] = record.corners.map(|corner| world_to_screen(view_proj, corner, screen)) else {
            continue;
        };
        let world = if let Some(weights) = triangle_weights(cursor, a, b, c) {
            perspective_correct_triangle_point(view_proj, [record.corners[0], record.corners[1], record.corners[2]], weights)
        } else if let Some(weights) = triangle_weights(cursor, a, c, d) {
            perspective_correct_triangle_point(view_proj, [record.corners[0], record.corners[2], record.corners[3]], weights)
        } else {
            continue;
        };
        let center_distance = ((a + b + c + d) * 0.25).distance_squared(cursor);
        let depth = projected_depth(view_proj, world);
        if best
            .as_ref()
            .is_none_or(|(distance, best_depth, _)| screen_hit_is_better(center_distance, depth, *distance, *best_depth))
        {
            best = Some((center_distance, depth, PickHit { entity: record.entity, world }));
        }
    }
    best.map(|(_, _, hit)| hit)
}

/// Find the polyline vertex nearest the cursor. Returns `(object_id, vertex_index, world_pos)`.
/// Only considers unfrozen, visible objects whose layer is visible in `doc`.
pub(crate) fn pick_nearest_vertex(
    doc: &Document,
    hidden: &HashSet<SceneEntityId>,
    frozen: &HashSet<SceneEntityId>,
    view_proj: &DMat4,
    screen: Size,
    cursor_px: (f32, f32),
    threshold_px: f32,
) -> Option<(ObjectId, ObjectPoint, DVec3)> {
    pick_nearest_vertex_from_indices(
        doc,
        0..doc.objects().len(),
        hidden,
        frozen,
        view_proj,
        screen,
        cursor_px,
        threshold_px,
        VertexPickFilter::AnyEditable,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VertexPickFilter {
    /// Point objects plus stored polyline vertices.
    AnyEditable,
    /// Vertices that can be removed without invalidating a polyline.
    DeletablePolyline,
}

/// Spatially accelerated design-vertex pick. The index must have been built
/// from `doc`; callers keep it revision-aligned with the composite scene.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pick_nearest_vertex_indexed(
    doc: &Document,
    index: &ObjectSnapIndex,
    hidden: &HashSet<SceneEntityId>,
    frozen: &HashSet<SceneEntityId>,
    view_proj: &DMat4,
    screen: Size,
    cursor_px: (f32, f32),
    threshold_px: f32,
    filter: VertexPickFilter,
) -> Option<(ObjectId, ObjectPoint, DVec3)> {
    let cursor = DVec2::new(f64::from(cursor_px.0), f64::from(cursor_px.1));
    let candidates = index.candidates(view_proj, screen, cursor, f64::from(threshold_px));
    pick_nearest_vertex_from_indices(doc, candidates, hidden, frozen, view_proj, screen, cursor_px, threshold_px, filter)
}

#[allow(clippy::too_many_arguments)]
fn pick_nearest_vertex_from_indices(
    doc: &Document,
    object_indices: impl IntoIterator<Item = usize>,
    hidden: &HashSet<SceneEntityId>,
    frozen: &HashSet<SceneEntityId>,
    view_proj: &DMat4,
    screen: Size,
    cursor_px: (f32, f32),
    threshold_px: f32,
    filter: VertexPickFilter,
) -> Option<(ObjectId, ObjectPoint, DVec3)> {
    let cursor = DVec2::new(f64::from(cursor_px.0), f64::from(cursor_px.1));
    let mut best_dist = threshold_px as f64;
    let mut best: Option<(ObjectId, ObjectPoint, DVec3)> = None;

    for object_index in object_indices {
        let Some(object) = doc.objects().get(object_index) else {
            continue;
        };
        let entity = SceneEntityId::Object(object.id());
        if hidden.contains(&entity) || frozen.contains(&entity) {
            continue;
        }
        if !doc.layer(object.layer()).map(|l| l.visible).unwrap_or(true) {
            continue;
        }
        match object {
            Object::Polyline { verts, closed, .. } if filter == VertexPickFilter::AnyEditable && compact_circle_center(verts, *closed).is_some() => {
                let center = compact_circle_center(verts, *closed).expect("circle checked above");
                if let Some(sp) = world_to_screen(view_proj, center, screen) {
                    let d = sp.distance(cursor);
                    if d < best_dist {
                        best_dist = d;
                        best = Some((object.id(), ObjectPoint::Center, center));
                    }
                }
            }
            Object::Polyline { verts, closed, .. } if filter == VertexPickFilter::AnyEditable || verts.len() > if *closed { 3 } else { 2 } => {
                for (i, vert) in verts.iter().enumerate() {
                    if let Some(sp) = world_to_screen(view_proj, vert.pos, screen) {
                        let d = sp.distance(cursor);
                        if d < best_dist {
                            best_dist = d;
                            best = Some((object.id(), ObjectPoint::Vertex(i), vert.pos));
                        }
                    }
                }
            }
            Object::Point { pos, .. } if filter == VertexPickFilter::AnyEditable => {
                if let Some(sp) = world_to_screen(view_proj, *pos, screen) {
                    let d = sp.distance(cursor);
                    if d < best_dist {
                        best_dist = d;
                        best = Some((object.id(), ObjectPoint::Vertex(0), *pos));
                    }
                }
            }
            _ => {}
        }
    }

    best
}

pub(crate) fn triangle_weights(point: DVec2, a: DVec2, b: DVec2, c: DVec2) -> Option<DVec3> {
    let denominator = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
    if denominator.abs() <= f64::EPSILON {
        return None;
    }
    let u = ((b.y - c.y) * (point.x - c.x) + (c.x - b.x) * (point.y - c.y)) / denominator;
    let v = ((c.y - a.y) * (point.x - c.x) + (a.x - c.x) * (point.y - c.y)) / denominator;
    let w = 1.0 - u - v;
    (u >= 0.0 && v >= 0.0 && w >= 0.0).then_some(DVec3::new(u, v, w))
}
