//! CPU scene tessellation primitives. Domain geometry remains double precision;
//! vertices are rebased to a scene origin only at the GPU boundary.

use std::sync::LazyLock;

use glam::DVec3;

use crate::{
    model::{PolyVertex, geometry::tessellate_bulge_segment},
    rendering::{StrokeVertex, Vertex},
};

pub(crate) struct DrawContext<'a> {
    pub(crate) stroke_vertex_buf: &'a mut Vec<StrokeVertex>,
    pub(crate) stroke_index_buf: &'a mut Vec<u32>,
    pub(crate) fill_vertex_buf: &'a mut Vec<Vertex>,
    pub(crate) fill_index_buf: &'a mut Vec<u32>,
    pub(crate) scene_origin: DVec3,
    pub(crate) scale_factor: f32,
}

/// Cosine of the turn angle below which a round join is visually redundant:
/// the wedge gap between adjacent stroke quads is `half_width * tan(angle/2)`,
/// sub-pixel at document line widths for turns under ~11 degrees. Densely
/// sampled polylines (contours, imported strings) are almost entirely such
/// turns, and a join costs 18 vertices.
const JOIN_COLLINEAR_COS: f64 = 0.98;

/// Whether the turn from direction `incoming` to `outgoing` is sharp enough
/// that the joint between their stroke quads needs a round join to cover it.
pub(crate) fn needs_round_join(incoming: DVec3, outgoing: DVec3) -> bool {
    let scale = incoming.length_squared() * outgoing.length_squared();
    if scale <= f64::EPSILON {
        return false;
    }
    incoming.dot(outgoing) < JOIN_COLLINEAR_COS * scale.sqrt()
}

fn local(point: DVec3, origin: DVec3) -> [f32; 3] {
    let p = point - origin;
    [p.x as f32, p.y as f32, p.z as f32]
}

pub(crate) fn draw_line(ctx: &mut DrawContext, start: DVec3, end: DVec3, line_width: f32, color: [f32; 4]) {
    let delta = end - start;
    if delta.length_squared() <= f64::EPSILON {
        return;
    }
    let line_width = line_width * ctx.scale_factor;
    let i = ctx.stroke_vertex_buf.len() as u32;
    let start = local(start, ctx.scene_origin);
    let end = local(end, ctx.scene_origin);
    let half = line_width.max(1.0) * 0.5;

    ctx.stroke_vertex_buf.extend_from_slice(&[
        StrokeVertex {
            pos: start,
            color,
            other_pos: end,
            offset_px: [-half, 0.0],
            screen_space: 0.0,
        },
        StrokeVertex {
            pos: start,
            color,
            other_pos: end,
            offset_px: [half, 0.0],
            screen_space: 0.0,
        },
        StrokeVertex {
            pos: end,
            color,
            other_pos: start,
            offset_px: [half, 0.0],
            screen_space: 0.0,
        },
        StrokeVertex {
            pos: end,
            color,
            other_pos: start,
            offset_px: [-half, 0.0],
            screen_space: 0.0,
        },
    ]);
    ctx.stroke_index_buf.extend_from_slice(&[i + 1, i, i + 3, i, i + 2, i + 3]);
}

/// Draw a filled circle (sphere indicator) in screen space at the given world position.
/// `radius_px` is in device pixels.
pub(crate) fn draw_screen_sphere(ctx: &mut DrawContext, center: DVec3, radius_px: f32, color: [f32; 4]) {
    const SEGMENTS: u32 = 16;
    let base = ctx.stroke_vertex_buf.len() as u32;
    let position = local(center, ctx.scene_origin);
    ctx.stroke_vertex_buf.push(StrokeVertex {
        pos: position,
        color,
        other_pos: position,
        offset_px: [0.0, 0.0],
        screen_space: 1.0,
    });
    for i in 0..SEGMENTS {
        let angle = std::f32::consts::TAU * i as f32 / SEGMENTS as f32;
        ctx.stroke_vertex_buf.push(StrokeVertex {
            pos: position,
            color,
            other_pos: position,
            offset_px: [angle.cos() * radius_px, angle.sin() * radius_px],
            screen_space: 1.0,
        });
    }
    for i in 0..SEGMENTS {
        ctx.stroke_index_buf.extend_from_slice(&[base, base + i + 1, base + (i + 1) % SEGMENTS + 1]);
    }
}

/// Snap marker matching the design-vertex markers drawn by `design_point.wgsl`:
/// same pixel footprint and dark outline, round instead of square and filled
/// with `color` so a snapped vertex reads as highlighted rather than covered.
pub(crate) fn draw_screen_point_marker(ctx: &mut DrawContext, center: DVec3, color: [f32; 4]) {
    draw_screen_point_marker_sized(ctx, center, 10.0, color);
}

/// The same outlined marker at an explicit outer diameter in logical pixels, so
/// overlays that need a size hierarchy (an active endpoint against its
/// candidates, a selected vertex against its siblings) still read as one family.
pub(crate) fn draw_screen_point_marker_sized(ctx: &mut DrawContext, center: DVec3, outer_px: f32, color: [f32; 4]) {
    let inner_px = (outer_px - 3.0).max(1.0);
    draw_screen_sphere(ctx, center, outer_px * 0.5 * ctx.scale_factor, crate::rendering::graphics::POINT_MARKER_COLOR);
    draw_screen_sphere(ctx, center, inner_px * 0.5 * ctx.scale_factor, color);
}

pub(crate) fn draw_screen_cross(ctx: &mut DrawContext, center: DVec3, half_size_px: f32, line_width: f32, color: [f32; 4]) {
    draw_screen_segment(ctx, center, DVec3::X, half_size_px, line_width, color);
    draw_screen_segment(ctx, center, DVec3::Y, half_size_px, line_width, color);
}

fn draw_screen_segment(ctx: &mut DrawContext, center: DVec3, direction: DVec3, half_size_px: f32, line_width: f32, color: [f32; 4]) {
    let index = ctx.stroke_vertex_buf.len() as u32;
    let position = local(center, ctx.scene_origin);
    let other = local(center + direction, ctx.scene_origin);
    let half_width = (line_width * ctx.scale_factor).max(1.0) * 0.5;
    ctx.stroke_vertex_buf.extend_from_slice(&[
        StrokeVertex {
            pos: position,
            color,
            other_pos: other,
            offset_px: [-half_width, -half_size_px],
            screen_space: 0.0,
        },
        StrokeVertex {
            pos: position,
            color,
            other_pos: other,
            offset_px: [half_width, -half_size_px],
            screen_space: 0.0,
        },
        StrokeVertex {
            pos: position,
            color,
            other_pos: other,
            offset_px: [-half_width, half_size_px],
            screen_space: 0.0,
        },
        StrokeVertex {
            pos: position,
            color,
            other_pos: other,
            offset_px: [half_width, half_size_px],
            screen_space: 0.0,
        },
    ]);
    ctx.stroke_index_buf.extend_from_slice(&[index + 1, index, index + 3, index, index + 2, index + 3]);
}

/// Add a camera-independent round join in pixel space at a world position.
pub(crate) fn draw_round_join(ctx: &mut DrawContext, center: DVec3, line_width: f32, color: [f32; 4]) {
    const MAX_SEGMENTS: u32 = 16;
    static DIRECTIONS: LazyLock<[[f32; 2]; (MAX_SEGMENTS + 1) as usize]> = LazyLock::new(|| {
        std::array::from_fn(|index| {
            let angle = std::f32::consts::TAU * index as f32 / MAX_SEGMENTS as f32;
            [angle.cos(), angle.sin()]
        })
    });
    let radius = (line_width * ctx.scale_factor).max(1.0) * 0.5;
    let segments = if radius <= 2.0 { 8 } else { MAX_SEGMENTS };
    let direction_step = (MAX_SEGMENTS / segments) as usize;
    let base = ctx.stroke_vertex_buf.len() as u32;
    let position = local(center, ctx.scene_origin);
    ctx.stroke_vertex_buf.push(StrokeVertex {
        pos: position,
        color,
        other_pos: position,
        offset_px: [0.0, 0.0],
        screen_space: 1.0,
    });
    for index in (0..=MAX_SEGMENTS as usize).step_by(direction_step) {
        let direction = DIRECTIONS[index];
        ctx.stroke_vertex_buf.push(StrokeVertex {
            pos: position,
            color,
            other_pos: position,
            offset_px: [direction[0] * radius, direction[1] * radius],
            screen_space: 1.0,
        });
    }
    for index in 0..segments {
        ctx.stroke_index_buf.extend_from_slice(&[base, base + index + 1, base + index + 2]);
    }
}

/// Tessellate a polyline's stroke (segments, arcs and the round joins between
/// them) into the context's stroke buffers. Shared by the per-rebuild scene
/// builder and the static stroke chunk cache so both produce identical
/// geometry.
pub(crate) fn tessellate_polyline_stroke(ctx: &mut DrawContext, verts: &[PolyVertex], closed: bool, line_weight: f32, line_rgba: [f32; 4]) {
    for segment in verts.windows(2) {
        draw_bulge_segment(ctx, segment[0].pos, segment[1].pos, segment[0].bulge, line_weight, line_rgba);
    }
    if closed
        && verts.len() >= 2
        && let (Some(first), Some(last)) = (verts.first(), verts.last())
    {
        draw_bulge_segment(ctx, last.pos, first.pos, last.bulge, line_weight, line_rgba);
    }
    let n = verts.len();
    let joint_indices = if closed && n >= 2 {
        0..n
    } else if n > 2 {
        1..n - 1
    } else {
        0..0
    };
    for i in joint_indices {
        let prev = &verts[(i + n - 1) % n];
        let cur = &verts[i];
        let next = &verts[(i + 1) % n];
        // Bulged segments meet the joint at the arc tangent,
        // not the chord direction, so always keep their joins.
        let keep = prev.bulge.abs() > f64::EPSILON || cur.bulge.abs() > f64::EPSILON || needs_round_join(cur.pos - prev.pos, next.pos - cur.pos);
        if keep {
            draw_round_join(ctx, cur.pos, line_weight, line_rgba);
        }
    }
}

pub(crate) fn draw_bulge_segment(ctx: &mut DrawContext, start: DVec3, end: DVec3, bulge: f64, line_width: f32, color: [f32; 4]) {
    let points = tessellate_bulge_segment(start, end, bulge);
    let segments = points.len().saturating_sub(1);
    if segments == 0 {
        return;
    }
    // The turn between consecutive arc segments is theta/segments; when it is
    // near-collinear the interior joins are invisible and can be skipped.
    let theta = 4.0 * bulge.atan();
    let interior_joins = bulge.is_finite() && bulge.abs() > f64::EPSILON && (theta / segments as f64).cos() < JOIN_COLLINEAR_COS;
    for (index, pair) in points.windows(2).enumerate() {
        draw_line(ctx, pair[0], pair[1], line_width, color);
        if index + 1 < segments && interior_joins {
            draw_round_join(ctx, pair[1], line_width, color);
        }
    }
}
