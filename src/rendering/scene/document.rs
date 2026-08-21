//! Document entity scene assembly.

use glam::{DVec2, DVec3};

use crate::{
    model::geometry::{PolylineFillMesh, polyline_plane_frame_points},
    rendering::{
        Vertex,
        geometry::{DrawContext, draw_line, draw_round_join, needs_round_join},
    },
};

pub(crate) fn polyline_hatch_spacing(mesh: &PolylineFillMesh, divisions: f64) -> f64 {
    let (centroid, axis_u, axis_v) = polyline_plane_frame_points(&mesh.vertices);
    let mut min = DVec2::splat(f64::INFINITY);
    let mut max = DVec2::splat(f64::NEG_INFINITY);
    for point in &mesh.vertices {
        let delta = *point - centroid;
        let projected = DVec2::new(delta.dot(axis_u), delta.dot(axis_v));
        min = min.min(projected);
        max = max.max(projected);
    }
    ((max - min).max_element() / divisions.max(1.0)).max(f64::EPSILON)
}

#[derive(Clone, Copy)]
struct HatchSegment {
    start_u: f64,
    end_u: f64,
    start: DVec3,
    end: DVec3,
}

fn triangle_hatch_segment(projected: [DVec2; 3], world: [DVec3; 3], scan_v: f64) -> Option<HatchSegment> {
    let mut intersections = Vec::with_capacity(2);
    for index in 0..3 {
        let next = (index + 1) % 3;
        let (start, end) = (projected[index], projected[next]);
        if !((start.y <= scan_v && scan_v < end.y) || (end.y <= scan_v && scan_v < start.y)) {
            continue;
        }
        let t = (scan_v - start.y) / (end.y - start.y);
        intersections.push((start.x + t * (end.x - start.x), world[index].lerp(world[next], t)));
    }
    if intersections.len() != 2 {
        return None;
    }
    intersections.sort_by(|left, right| left.0.total_cmp(&right.0));
    let [(start_u, start), (end_u, end)]: [(f64, DVec3); 2] = intersections.try_into().ok()?;
    (end_u > start_u).then_some(HatchSegment { start_u, end_u, start, end })
}

fn simplify_hatch_line(points: Vec<DVec3>) -> Vec<DVec3> {
    let mut simplified: Vec<DVec3> = Vec::with_capacity(points.len());
    for point in points {
        while simplified.len() >= 2 {
            let incoming = simplified[simplified.len() - 1] - simplified[simplified.len() - 2];
            let outgoing = point - simplified[simplified.len() - 1];
            let length_product = incoming.length_squared() * outgoing.length_squared();
            if length_product <= f64::EPSILON || incoming.dot(outgoing) < 0.0 || incoming.cross(outgoing).length_squared() > length_product * 1.0e-20 {
                break;
            }
            simplified.pop();
        }
        simplified.push(point);
    }
    simplified
}

/// Generate hatch lines in the polyline's projection, but lift every
/// triangle-clipped segment back onto the corresponding 3D face. Adjacent
/// pieces are chained and planar runs are collapsed to keep geometry compact.
fn polyline_hatch_lines(mesh: &PolylineFillMesh, angle_deg: f32, spacing: f64) -> Vec<Vec<DVec3>> {
    if mesh.vertices.len() < 3 || !spacing.is_finite() || spacing <= 0.0 {
        return Vec::new();
    }
    let (centroid, axis_u, axis_v) = polyline_plane_frame_points(&mesh.vertices);
    let angle = f64::from(angle_deg).to_radians();
    let cos_a = angle.cos();
    let sin_a = angle.sin();
    let rotated: Vec<DVec2> = mesh
        .vertices
        .iter()
        .map(|point| {
            let delta = *point - centroid;
            let u = delta.dot(axis_u);
            let v = delta.dot(axis_v);
            DVec2::new(u * cos_a + v * sin_a, -u * sin_a + v * cos_a)
        })
        .collect();
    let min_v = rotated.iter().map(|point| point.y).fold(f64::INFINITY, f64::min);
    let max_v = rotated.iter().map(|point| point.y).fold(f64::NEG_INFINITY, f64::max);
    let coordinate_scale = rotated.iter().fold(spacing.max(1.0), |scale, point| scale.max(point.abs().max_element()));
    let join_tolerance = coordinate_scale * 1.0e-10;
    let triangles = mesh.indices.as_chunks::<3>().0;
    let mut lines = Vec::new();

    let mut scan_v = min_v + spacing * 0.5;
    while scan_v < max_v {
        let mut segments = Vec::new();
        for triangle in triangles {
            let projected = triangle.map(|index| rotated[index as usize]);
            let world = triangle.map(|index| mesh.vertices[index as usize]);
            if let Some(segment) = triangle_hatch_segment(projected, world, scan_v) {
                segments.push(segment);
            }
        }
        segments.sort_by(|left, right| left.start_u.total_cmp(&right.start_u).then_with(|| left.end_u.total_cmp(&right.end_u)));

        let mut current = Vec::new();
        let mut current_end_u = f64::NEG_INFINITY;
        for segment in segments {
            if current.is_empty() || (segment.start_u - current_end_u).abs() > join_tolerance {
                if current.len() >= 2 {
                    lines.push(simplify_hatch_line(current));
                }
                current = vec![segment.start, segment.end];
                current_end_u = segment.end_u;
            } else if segment.end_u > current_end_u + join_tolerance {
                current.push(segment.end);
                current_end_u = segment.end_u;
            }
        }
        if current.len() >= 2 {
            lines.push(simplify_hatch_line(current));
        }
        scan_v += spacing;
    }
    lines
}

/// Draw hatch lines clipped to and draped across a closed polyline's triangle
/// surface rather than flattened onto its projection plane.
pub(crate) fn fill_polyline_hatch(draw_ctx: &mut DrawContext<'_>, mesh: &PolylineFillMesh, color: [f32; 4], angle_deg: f32, spacing: f64, line_weight: f32) {
    for line in polyline_hatch_lines(mesh, angle_deg, spacing) {
        for segment in line.windows(2) {
            draw_line(draw_ctx, segment[0], segment[1], line_weight, color);
        }
        for points in line.windows(3) {
            if needs_round_join(points[1] - points[0], points[2] - points[1]) {
                draw_round_join(draw_ctx, points[1], line_weight, color);
            }
        }
    }
}

/// Tessellate a closed polyline and push its 3D triangle surface into the fill
/// buffers. Projection determines triangle connectivity but never replaces the
/// original boundary elevations.
pub(crate) fn fill_polyline_solid(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, mesh: &PolylineFillMesh, color: [f32; 4], scene_origin: DVec3) {
    let Ok(base) = u32::try_from(vertices.len()) else {
        return;
    };
    if mesh.indices.iter().any(|index| base.checked_add(*index).is_none()) {
        return;
    }
    for point in &mesh.vertices {
        let pos = *point - scene_origin;
        vertices.push(Vertex {
            pos: [pos.x as f32, pos.y as f32, pos.z as f32],
            color,
        });
    }
    indices.extend(mesh.indices.iter().map(|index| base + *index));
}
