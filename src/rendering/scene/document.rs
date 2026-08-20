//! Document entity scene assembly.

use glam::DVec3;
use lyon::{
    math::point as lyon_point,
    path::Path as LyonPath,
    tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers},
};

use crate::{
    model::{PolyVertex, geometry::tessellate_polyline_bulges},
    rendering::{
        Vertex,
        geometry::{DrawContext, draw_line},
    },
};

fn polygon_plane_frame_points(points: &[DVec3]) -> (DVec3, DVec3, DVec3) {
    let n = points.len();
    if n == 0 {
        return (DVec3::ZERO, DVec3::X, DVec3::Y);
    }
    let centroid = points.iter().copied().sum::<DVec3>() / n as f64;

    let mut normal = DVec3::ZERO;
    for i in 0..n {
        let c = points[i];
        let d = points[(i + 1) % n];
        normal.x += (c.y - d.y) * (c.z + d.z);
        normal.y += (c.z - d.z) * (c.x + d.x);
        normal.z += (c.x - d.x) * (c.y + d.y);
    }
    let normal = if normal.length_squared() > f64::EPSILON { normal.normalize() } else { DVec3::Z };

    let up_hint = if normal.z.abs() < 0.9 { DVec3::Z } else { DVec3::Y };
    let axis_u = up_hint.cross(normal).normalize_or(DVec3::X);
    let axis_v = normal.cross(axis_u).normalize_or(DVec3::Y);
    (centroid, axis_u, axis_v)
}

pub(crate) fn polygon_hatch_spacing(verts: &[PolyVertex], divisions: f64) -> f64 {
    let points = tessellate_polyline_bulges(verts, true);
    let (centroid, axis_u, axis_v) = polygon_plane_frame_points(&points);
    let mut min = glam::DVec2::splat(f64::INFINITY);
    let mut max = glam::DVec2::splat(f64::NEG_INFINITY);
    for point in points {
        let delta = point - centroid;
        let projected = glam::DVec2::new(delta.dot(axis_u), delta.dot(axis_v));
        min = min.min(projected);
        max = max.max(projected);
    }
    ((max - min).max_element() / divisions.max(1.0)).max(f64::EPSILON)
}

/// Draw hatch lines at `angle_deg` clipped to the interior of a closed polygon.
/// Works in the polygon's own plane, so the fill is correct regardless of
/// camera orientation or polygon tilt.
pub(crate) fn fill_polygon_hatch(draw_ctx: &mut DrawContext<'_>, verts: &[PolyVertex], color: [f32; 4], angle_deg: f32, spacing: f64, line_weight: f32) {
    let points = tessellate_polyline_bulges(verts, true);
    if points.len() < 3 {
        return;
    }
    let (centroid, axis_u, axis_v) = polygon_plane_frame_points(&points);

    let pts_2d: Vec<[f64; 2]> = points
        .iter()
        .map(|point| {
            let d = *point - centroid;
            [d.dot(axis_u), d.dot(axis_v)]
        })
        .collect();

    let angle = (angle_deg as f64).to_radians();
    let cos_a = angle.cos();
    let sin_a = angle.sin();
    let rotated: Vec<[f64; 2]> = pts_2d.iter().map(|p| [p[0] * cos_a + p[1] * sin_a, -p[0] * sin_a + p[1] * cos_a]).collect();

    let min_v = rotated.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let max_v = rotated.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
    let n = rotated.len();

    let mut scan_v = min_v + spacing * 0.5;
    while scan_v < max_v {
        let mut xs: Vec<f64> = Vec::new();
        for i in 0..n {
            let j = (i + 1) % n;
            let v0 = rotated[i][1];
            let u0 = rotated[i][0];
            let v1 = rotated[j][1];
            let u1 = rotated[j][0];
            if (v0 <= scan_v && scan_v < v1) || (v1 <= scan_v && scan_v < v0) {
                let t = (scan_v - v0) / (v1 - v0);
                xs.push(u0 + t * (u1 - u0));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let mut i = 0;
        while i + 1 < xs.len() {
            let (su0, su1) = (xs[i], xs[i + 1]);
            let pu0 = su0 * cos_a - scan_v * sin_a;
            let pv0 = su0 * sin_a + scan_v * cos_a;
            let pu1 = su1 * cos_a - scan_v * sin_a;
            let pv1 = su1 * sin_a + scan_v * cos_a;
            let a = centroid + pu0 * axis_u + pv0 * axis_v;
            let b = centroid + pu1 * axis_u + pv1 * axis_v;
            draw_line(draw_ctx, a, b, line_weight, color);
            i += 2;
        }
        scan_v += spacing;
    }
}

/// Tessellate a closed polygon using lyon and push the triangles into the fill
/// buffers. Works in the polygon's own plane so the solid fill is correct
/// regardless of camera orientation or polygon tilt.
pub(crate) fn fill_polygon_solid(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, verts: &[PolyVertex], color: [f32; 4], scene_origin: DVec3) {
    let points = tessellate_polyline_bulges(verts, true);
    if points.len() < 3 {
        return;
    }
    let (centroid, axis_u, axis_v) = polygon_plane_frame_points(&points);

    let pts_2d: Vec<[f32; 2]> = points
        .iter()
        .map(|point| {
            let d = *point - centroid;
            [d.dot(axis_u) as f32, d.dot(axis_v) as f32]
        })
        .collect();

    let mut builder = LyonPath::builder();
    builder.begin(lyon_point(pts_2d[0][0], pts_2d[0][1]));
    for pt in &pts_2d[1..] {
        builder.line_to(lyon_point(pt[0], pt[1]));
    }
    builder.close();
    let path = builder.build();

    let base = vertices.len();
    if base > u32::MAX as usize {
        return;
    }
    let mut tessellator = FillTessellator::new();
    let mut output: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let result = tessellator.tessellate_path(
        &path,
        &FillOptions::default(),
        &mut BuffersBuilder::new(&mut output, |vertex: FillVertex| [vertex.position().x, vertex.position().y]),
    );
    if result.is_err() {
        return;
    }
    for [u, v] in &output.vertices {
        let pos = centroid + *u as f64 * axis_u + *v as f64 * axis_v - scene_origin;
        vertices.push(Vertex {
            pos: [pos.x as f32, pos.y as f32, pos.z as f32],
            color,
        });
    }
    let base = base as u32;
    for idx in &output.indices {
        indices.push(base + idx);
    }
}
