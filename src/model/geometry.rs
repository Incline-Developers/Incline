//! Double-precision geometry shared by import, editing and scene preparation.

use glam::{DMat4, DQuat, DVec2, DVec3};

use crate::model::PolyVertex;

/// Build the compact bulged-polyline representation of a horizontal circle.
///
/// The two vertices sit at opposite ends of a diameter and each outgoing
/// segment is a 180-degree arc (`bulge = tan(PI / 4) = 1`). `bearing` only
/// controls where those otherwise-invisible edit vertices land; a missing or
/// degenerate bearing falls back to world +X.
pub(crate) fn circle_polyline_vertices(center: DVec3, radius: f64, bearing: DVec2) -> Option<[PolyVertex; 2]> {
    if !center.is_finite() || !radius.is_finite() || radius <= f64::EPSILON {
        return None;
    }
    let direction = if bearing.is_finite() && bearing.length_squared() > f64::EPSILON {
        bearing.normalize()
    } else {
        DVec2::X
    };
    let offset = DVec3::new(direction.x * radius, direction.y * radius, 0.0);
    Some([PolyVertex { pos: center + offset, bulge: 1.0 }, PolyVertex { pos: center - offset, bulge: 1.0 }])
}

/// Return the centre of the compact two-semicircle representation of a circle.
///
/// The persisted geometry remains a standard bulged polyline for format
/// compatibility, while editor affordances can treat it as one semantic point.
pub(crate) fn compact_circle_center(verts: &[PolyVertex], closed: bool) -> Option<DVec3> {
    const BULGE_TOLERANCE: f64 = 1.0e-9;
    let [first, second] = verts else {
        return None;
    };
    if !closed
        || !first.pos.is_finite()
        || !second.pos.is_finite()
        || (first.pos - second.pos).length_squared() <= f64::EPSILON
        || (first.bulge.abs() - 1.0).abs() > BULGE_TOLERANCE
        || (second.bulge.abs() - 1.0).abs() > BULGE_TOLERANCE
        || first.bulge.signum() != second.bulge.signum()
    {
        return None;
    }
    Some((first.pos + second.pos) * 0.5)
}

pub(crate) fn point_to_dvec3(point: &dxf::Point) -> DVec3 {
    DVec3::new(point.x, point.y, point.z)
}

pub(crate) fn vector_to_dvec3(vector: &dxf::Vector) -> DVec3 {
    DVec3::new(vector.x, vector.y, vector.z)
}

pub(crate) fn normalize_or_z(vector: DVec3) -> DVec3 {
    if vector.length_squared() <= f64::EPSILON { DVec3::Z } else { vector.normalize() }
}

/// Convert a bottom-left-anchored CAD text position (such as DXF `TEXT`) to
/// [`crate::model::Object::Text`]'s top-left anchor by shifting one line
/// height along the text's local up direction. Both conventions anchor on the
/// first line (further lines flow downward), so the shift is one line height
/// regardless of line count. `rotation` is in radians, counter-clockwise in
/// the drafting plane; Z is untouched.
pub(crate) fn text_anchor_bottom_to_top(pos: DVec3, height: f64, rotation: f64) -> DVec3 {
    pos + text_up_direction(rotation) * height
}

/// Inverse of [`text_anchor_bottom_to_top`], for exporting to bottom-left
/// anchored formats.
pub(crate) fn text_anchor_top_to_bottom(pos: DVec3, height: f64, rotation: f64) -> DVec3 {
    pos - text_up_direction(rotation) * height
}

/// Unit vector perpendicular to a text baseline of the given rotation,
/// pointing from the baseline towards the top of the line.
fn text_up_direction(rotation: f64) -> DVec3 {
    DVec3::new(-rotation.sin(), rotation.cos(), 0.0)
}

#[derive(Clone)]
pub(crate) struct Transform {
    stack: Vec<DMat4>,
}

impl Transform {
    pub(crate) fn identity() -> Self {
        Self { stack: vec![DMat4::IDENTITY] }
    }

    pub(crate) fn apply(&self, point: DVec3) -> DVec3 {
        self.matrix().transform_point3(point)
    }

    pub(crate) fn matrix(&self) -> DMat4 {
        self.stack.last().copied().unwrap_or(DMat4::IDENTITY)
    }

    pub(crate) fn push(&mut self, base: DVec3, translation: DVec3, rotation: f64, scale: DVec3) {
        self.push_with_affine(base, translation, rotation, scale, DMat4::IDENTITY);
    }

    pub(crate) fn push_with_affine(&mut self, base: DVec3, translation: DVec3, rotation: f64, scale: DVec3, affine: DMat4) {
        let local = affine * DMat4::from_translation(translation) * DMat4::from_quat(DQuat::from_rotation_z(rotation)) * DMat4::from_scale(scale) * DMat4::from_translation(-base);
        self.stack.push(self.matrix() * local);
    }

    pub(crate) fn pop(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }
}

// -------------------------------------------------------------------------
// Geometric polyline offset
// -------------------------------------------------------------------------

const BULGE_MIN_SEGMENTS: usize = 16;
const BULGE_MAX_SEGMENTS: usize = 4096;
const BULGE_TARGET_SEGMENT_LENGTH: f64 = 0.15;

/// Expand bulged XY segments to straight points for geometry algorithms that
/// cannot retain the original circular-arc representation. The returned
/// closed ring does not repeat its first point. Elevation is interpolated
/// linearly along each arc, matching the design-string convention.
pub(crate) fn tessellate_polyline_bulges(verts: &[PolyVertex], closed: bool) -> Vec<DVec3> {
    match verts.len() {
        0 => return Vec::new(),
        1 => return vec![verts[0].pos],
        _ => {}
    }

    let edge_count = if closed { verts.len() } else { verts.len() - 1 };
    let mut points = Vec::with_capacity(verts.len());
    points.push(verts[0].pos);

    for i in 0..edge_count {
        let start = verts[i];
        let end = verts[(i + 1) % verts.len()].pos;
        let include_end = !closed || i + 1 < edge_count;
        append_bulge_segment_points(&mut points, start.pos, end, start.bulge, include_end);
    }

    points
}

/// Indexed triangle surface spanning a closed polyline. The projected 2D ring
/// determines topology only; every output vertex retains its original 3D
/// position so a non-planar boundary is never flattened for display.
pub(crate) struct PolylineFillMesh {
    pub(crate) vertices: Vec<DVec3>,
    pub(crate) indices: Vec<u32>,
}

/// Return a stable local frame for projecting a 3D polyline to 2D.
///
/// Newell's normal handles vertical and tilted rings. Computing it relative to
/// the centroid keeps large mine-grid coordinates out of the products.
pub(crate) fn polyline_plane_frame_points(points: &[DVec3]) -> (DVec3, DVec3, DVec3) {
    let n = points.len();
    if n == 0 {
        return (DVec3::ZERO, DVec3::X, DVec3::Y);
    }
    let centroid = points.iter().copied().sum::<DVec3>() / n as f64;

    let mut normal = DVec3::ZERO;
    for index in 0..n {
        let current = points[index] - centroid;
        let next = points[(index + 1) % n] - centroid;
        normal.x += (current.y - next.y) * (current.z + next.z);
        normal.y += (current.z - next.z) * (current.x + next.x);
        normal.z += (current.x - next.x) * (current.y + next.y);
    }
    let normal = normal.try_normalize().unwrap_or(DVec3::Z);
    let up_hint = if normal.z.abs() < 0.9 { DVec3::Z } else { DVec3::Y };
    let axis_u = up_hint.cross(normal).normalize_or(DVec3::X);
    let axis_v = normal.cross(axis_u).normalize_or(DVec3::Y);
    (centroid, axis_u, axis_v)
}

/// Triangulate a closed polyline without moving its boundary onto the
/// projection plane. A constrained Delaunay triangulation is used instead of
/// an unconstrained polyline tessellator so projected-collinear boundary points
/// remain part of the mesh even when their elevations are not collinear.
pub(crate) fn triangulate_polyline_fill(verts: &[PolyVertex]) -> Option<PolylineFillMesh> {
    use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation as _};

    let mut vertices = tessellate_polyline_bulges(verts, true);
    vertices.dedup();
    if vertices.len() > 1 && vertices.first() == vertices.last() {
        vertices.pop();
    }
    if vertices.len() < 3 || vertices.len() > u32::MAX as usize {
        return None;
    }

    let (centroid, axis_u, axis_v) = polyline_plane_frame_points(&vertices);
    let projected: Vec<DVec2> = vertices
        .iter()
        .map(|point| {
            let delta = *point - centroid;
            DVec2::new(delta.dot(axis_u), delta.dot(axis_v))
        })
        .collect();
    if projected.iter().any(|point| !point.is_finite()) {
        return None;
    }

    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::new();
    let mut handles = Vec::with_capacity(projected.len());
    let mut input_index_by_handle: Vec<Option<usize>> = Vec::new();
    for (input_index, point) in projected.iter().enumerate() {
        let handle = cdt.insert(Point2::new(point.x, point.y)).ok()?;
        if handle.index() >= input_index_by_handle.len() {
            input_index_by_handle.resize(handle.index() + 1, None);
        }
        // Two distinct 3D boundary positions at the same projected position
        // cannot bound a single-valued surface in this projection.
        if input_index_by_handle[handle.index()].replace(input_index).is_some() {
            return None;
        }
        handles.push(handle);
    }

    for index in 0..handles.len() {
        let from = handles[index];
        let to = handles[(index + 1) % handles.len()];
        if !cdt.can_add_constraint(from, to) {
            return None;
        }
        cdt.add_constraint(from, to);
    }

    let mut indices = Vec::with_capacity(vertices.len().saturating_sub(2) * 3);
    for face in cdt.inner_faces() {
        let corners = face.vertices();
        let center = corners
            .iter()
            .map(|corner| {
                let point = corner.position();
                DVec2::new(point.x, point.y)
            })
            .sum::<DVec2>()
            / 3.0;
        if matches!(
            crate::model::kernel::point_in_polyline(center, projected.iter().copied()),
            crate::model::kernel::PolyContainment::Outside
        ) {
            continue;
        }
        for corner in corners {
            let input_index = input_index_by_handle.get(corner.fix().index()).copied().flatten()?;
            indices.push(input_index as u32);
        }
    }
    if indices.is_empty() || !indices.len().is_multiple_of(3) {
        return None;
    }

    // Every sampled boundary point must participate in the surface. This is
    // what prevents a 2D triangulator from silently cutting across a raised or
    // lowered point that happens to be collinear in projection.
    let mut referenced = vec![false; vertices.len()];
    for &index in &indices {
        referenced[index as usize] = true;
    }
    if referenced.iter().any(|used| !used) {
        return None;
    }

    Some(PolylineFillMesh { vertices, indices })
}

/// Expand one bulged XY segment, including both endpoints. Elevation follows
/// the segment parameter linearly rather than being rotated about world Z.
pub(crate) fn tessellate_bulge_segment(start: DVec3, end: DVec3, bulge: f64) -> Vec<DVec3> {
    let mut points = vec![start];
    append_bulge_segment_points(&mut points, start, end, bulge, true);
    points
}

/// Exact axis-aligned bounds of a polyline including circular-arc extrema.
pub(crate) fn polyline_bulge_bounds(verts: &[PolyVertex], closed: bool) -> Option<(DVec3, DVec3)> {
    let first = verts.first()?;
    let mut min = first.pos;
    let mut max = first.pos;
    if verts.len() == 1 {
        return Some((min, max));
    }
    let edge_count = if closed { verts.len() } else { verts.len() - 1 };
    for i in 0..edge_count {
        let start = verts[i];
        let end = verts[(i + 1) % verts.len()].pos;
        let (edge_min, edge_max) = bulge_segment_bounds(start.pos, end, start.bulge);
        min = min.min(edge_min);
        max = max.max(edge_max);
    }
    Some((min, max))
}

fn bulge_segment_bounds(start: DVec3, end: DVec3, bulge: f64) -> (DVec3, DVec3) {
    let mut min = start.min(end);
    let mut max = start.max(end);
    let Some((center, start_vec, sweep, _)) = bulge_arc_parameters(start, end, bulge) else {
        return (min, max);
    };
    let start_angle = start_vec.y.atan2(start_vec.x);
    for angle in [0.0, std::f64::consts::FRAC_PI_2, std::f64::consts::PI, std::f64::consts::PI * 1.5] {
        let directed_delta = if sweep >= 0.0 {
            (angle - start_angle).rem_euclid(std::f64::consts::TAU)
        } else {
            (start_angle - angle).rem_euclid(std::f64::consts::TAU)
        };
        if directed_delta <= sweep.abs() + 1.0e-12 {
            let radius = start_vec.length();
            let point = DVec3::new(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
                start.z + (end.z - start.z) * directed_delta / sweep.abs(),
            );
            min = min.min(point);
            max = max.max(point);
        }
    }
    (min, max)
}

fn bulge_arc_parameters(start: DVec3, end: DVec3, bulge: f64) -> Option<(DVec2, DVec2, f64, f64)> {
    let chord = end.truncate() - start.truncate();
    let chord_len = chord.length();
    if !bulge.is_finite() || bulge.abs() <= f64::EPSILON || chord_len <= f64::EPSILON {
        return None;
    }
    let sweep = 4.0 * bulge.atan();
    let radius = chord_len * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
    let midpoint = (start.truncate() + end.truncate()) * 0.5;
    let left = DVec2::new(-chord.y, chord.x) / chord_len;
    let center = midpoint + left * chord_len * (1.0 - bulge * bulge) / (4.0 * bulge);
    Some((center, start.truncate() - center, sweep, radius))
}

fn append_bulge_segment_points(points: &mut Vec<DVec3>, start: DVec3, end: DVec3, bulge: f64, include_end: bool) {
    let Some((center, start_vec, theta, radius)) = bulge_arc_parameters(start, end, bulge) else {
        if include_end {
            points.push(end);
        }
        return;
    };
    let requested = (radius * theta.abs() / BULGE_TARGET_SEGMENT_LENGTH).ceil();
    let segments = if requested.is_finite() {
        (requested as usize).clamp(BULGE_MIN_SEGMENTS, BULGE_MAX_SEGMENTS)
    } else {
        BULGE_MAX_SEGMENTS
    };

    for step in 1..=segments {
        if step == segments && !include_end {
            break;
        }
        if step == segments {
            points.push(end);
            continue;
        }
        let t = step as f64 / segments as f64;
        let angle = theta * t;
        let (sin, cos) = angle.sin_cos();
        let rotated = DVec2::new(start_vec.x * cos - start_vec.y * sin, start_vec.x * sin + start_vec.y * cos);
        let xy = center + rotated;
        points.push(DVec3::new(xy.x, xy.y, start.z + (end.z - start.z) * t));
    }
}

/// Approximate the same text box used by document rendering and picking.
pub(crate) fn text_bounds_corners(pos: DVec3, content: &str, height: f64, rotation: f64) -> [DVec3; 4] {
    let height = height.abs().max(0.001);
    let longest_line = content.lines().map(|line| line.chars().count()).max().unwrap_or(0).max(1);
    let width = height * 0.58 * longest_line as f64;
    text_bounds_corners_with_layout_width(pos, content, height, rotation, width)
}

/// Text bounds using a width reported by the renderer's shaping engine.
/// Model-only callers use [`text_bounds_corners`]'s font-independent estimate;
/// rendered selection and picking bounds can use the exact shaped advance.
pub(crate) fn text_bounds_corners_with_layout_width(pos: DVec3, content: &str, height: f64, rotation: f64, layout_width: f64) -> [DVec3; 4] {
    const LINE_HEIGHT_FACTOR: f64 = 1.1;
    let height = height.abs().max(0.001);
    let width = layout_width.max(height * 0.58);
    let line_count = content.lines().count().max(1);
    let text_height = height * (1.0 + LINE_HEIGHT_FACTOR * (line_count - 1) as f64);
    let (sin, cos) = rotation.sin_cos();
    let right = DVec3::new(cos, sin, 0.0);
    let down = DVec3::new(sin, -cos, 0.0);
    let padding = height * 0.15;
    let top_left = pos - right * padding - down * padding;
    let top_right = pos + right * (width + padding) - down * padding;
    let bottom_right = top_right + down * (text_height + padding * 2.0);
    let bottom_left = top_left + down * (text_height + padding * 2.0);
    [top_left, top_right, bottom_right, bottom_left]
}

/// Intersect adjacent offset edge lines; `None` when (near-)parallel.
fn intersect_offset_edges(a: DVec2, r: DVec2, b: DVec2, s: DVec2) -> Option<DVec2> {
    crate::model::kernel::line_line(a, r, b, s)
}

/// When the mitre extension at a corner is larger than this multiple of the offset
/// distance, the corner is bevelled instead to prevent self-intersecting output.
const MITER_LIMIT: f64 = 4.0;

/// Offset each edge of a polyline perpendicular to itself in the XY plane,
/// then intersect adjacent offset edges to find the new vertices.
///
/// `signed_horiz_dist > 0` offsets to the **left** of each directed edge (CCW = inward).
/// `z_delta` is added to every vertex Z (use `0.0` for a horizontal berm offset).
///
/// For open polylines the first/last vertices are the respective endpoints of the
/// first/last offset edges (no intersection needed).
pub(crate) fn geometric_offset(verts: &[DVec3], closed: bool, signed_horiz_dist: f64, z_delta: f64) -> Vec<DVec3> {
    let n = verts.len();
    if n < 2 {
        return verts.to_vec();
    }

    // Compute per-edge normals (left perpendicular, XY only).
    let edge_count = if closed { n } else { n - 1 };
    let mut normals: Vec<DVec2> = Vec::with_capacity(edge_count);
    let mut dirs: Vec<DVec2> = Vec::with_capacity(edge_count);
    for i in 0..edge_count {
        let a = verts[i].truncate();
        let b = verts[(i + 1) % n].truncate();
        let d = b - a;
        let len = d.length();
        let dir = if len > 1e-10 { d / len } else { DVec2::X };
        dirs.push(dir);
        normals.push(DVec2::new(-dir.y, dir.x)); // left perpendicular
    }

    // Build offset edge start points.
    let offset_starts: Vec<DVec2> = (0..edge_count).map(|i| verts[i].truncate() + signed_horiz_dist * normals[i]).collect();

    // Compute new vertex positions.
    let mut result = Vec::with_capacity(n);

    if closed {
        for i in 0..n {
            let prev = (i + edge_count - 1) % edge_count;
            let a = offset_starts[prev];
            let b = offset_starts[i];
            // Apply mitre limit: if the corner extension exceeds MITER_LIMIT × offset
            // the polyline would self-intersect (tight inward corner); bevel instead.
            let max_ext = MITER_LIMIT * signed_horiz_dist.abs();
            let intersection = intersect_offset_edges(a, dirs[prev], b, dirs[i])
                .filter(|&pt| signed_horiz_dist.abs() < 1e-10 || (pt - verts[i].truncate()).length() <= max_ext)
                .unwrap_or_else(|| {
                    // Parallel or overconstrained corner: bevel by averaging edge endpoints.
                    let end_of_prev = offset_starts[prev] + dirs[prev] * (verts[i].truncate() - verts[prev % n].truncate()).length();
                    (end_of_prev + b) * 0.5
                });
            result.push(DVec3::new(intersection.x, intersection.y, verts[i].z + z_delta));
        }
    } else {
        // First vertex: start of first offset edge.
        result.push(DVec3::new(offset_starts[0].x, offset_starts[0].y, verts[0].z + z_delta));
        // Interior vertices: intersection of adjacent offset edges.
        for i in 1..n - 1 {
            let a = offset_starts[i - 1];
            let b = offset_starts[i];
            let max_ext = MITER_LIMIT * signed_horiz_dist.abs();
            let intersection = intersect_offset_edges(a, dirs[i - 1], b, dirs[i])
                .filter(|&pt| signed_horiz_dist.abs() < 1e-10 || (pt - verts[i].truncate()).length() <= max_ext)
                .unwrap_or_else(|| {
                    let end_of_prev = offset_starts[i - 1] + dirs[i - 1] * (verts[i].truncate() - verts[i - 1].truncate()).length();
                    (end_of_prev + b) * 0.5
                });
            result.push(DVec3::new(intersection.x, intersection.y, verts[i].z + z_delta));
        }
        // Last vertex: end of last offset edge.
        let last_edge = edge_count - 1;
        let end = offset_starts[last_edge]
            + dirs[last_edge] * {
                let a = verts[n - 2].truncate();
                let b = verts[n - 1].truncate();
                (b - a).length()
            };
        result.push(DVec3::new(end.x, end.y, verts[n - 1].z + z_delta));
    }

    result
}

/// Project each vertex of a polyline outward (perpendicular, XY) by the horizontal
/// distance implied by *its own* elevation and a fixed batter angle, so every output
/// vertex lands flat at `target_rl` - as if a batter wall of that angle were cut from
/// the string down (or up) to the target level at each point along its length.
///
/// Unlike `geometric_offset` (uniform per-edge XY offset + uniform Z shift), the
/// horizontal offset here varies per vertex with `verts[i].z`, so a string that
/// changes elevation along its length (e.g. a ramp crest) ends up flat rather than
/// retaining its original elevation profile.
///
/// `side` selects which side of the polyline to project toward (see
/// `offset_side_from_cursor`).
pub(crate) fn geometric_offset_project_to_rl(verts: &[DVec3], closed: bool, side: f64, tan_angle: f64, target_rl: f64) -> Vec<DVec3> {
    let n = verts.len();
    if n < 2 {
        return verts.to_vec();
    }

    let edge_count = if closed { n } else { n - 1 };
    let mut normals: Vec<DVec2> = Vec::with_capacity(edge_count);
    for i in 0..edge_count {
        let a = verts[i].truncate();
        let b = verts[(i + 1) % n].truncate();
        let d = b - a;
        let len = d.length();
        let dir = if len > 1e-10 { d / len } else { DVec2::X };
        normals.push(DVec2::new(-dir.y, dir.x));
    }

    let vertex_offset = |i: usize, signed_dist: f64| -> DVec2 {
        let (prev, next) = if closed {
            (normals[(i + edge_count - 1) % edge_count], normals[i % edge_count])
        } else if i == 0 {
            return signed_dist * normals[0];
        } else if i == n - 1 {
            return signed_dist * normals[edge_count - 1];
        } else {
            (normals[i - 1], normals[i])
        };
        let sum = prev + next;
        if sum.length() <= 1e-10 {
            return signed_dist * next;
        }
        let bisector = sum.normalize();
        let normal_component = bisector.dot(next);
        if normal_component.abs() <= 1e-10 {
            signed_dist * next
        } else {
            // A corner must be farther along its bisector than the requested
            // perpendicular edge offset. Dividing by this projection is
            // equivalent to intersecting the two offset edge lines.
            bisector * (signed_dist / normal_component)
        }
    };

    verts
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let dist = if tan_angle.abs() < 1e-9 { 0.0 } else { ((target_rl - v.z) / tan_angle).abs() };
            let xy = v.truncate() + vertex_offset(i, side * dist);
            DVec3::new(xy.x, xy.y, target_rl)
        })
        .collect()
}

pub(crate) fn points_coincident(a: DVec3, b: DVec3) -> bool {
    crate::model::kernel::points_coincident_3d(a, b)
}

/// Minimal 2.5D point abstraction for the polyline algorithms shared across
/// the drawing and triangulation tools: they operate in the XY plane but must
/// carry each point's full payload (e.g. Z) through clipping unchanged.
pub(crate) trait XyPoint: Copy {
    fn xy(self) -> DVec2;
    /// Linear interpolation in the point's native space (all components).
    fn lerp_point(self, other: Self, t: f64) -> Self;
    /// Squared distance in the point's native space (2D for `DVec2`,
    /// 3D otherwise), used for coincident-vertex cleanup after clipping.
    fn distance_sq(self, other: Self) -> f64;
}

impl XyPoint for DVec2 {
    fn xy(self) -> DVec2 {
        self
    }
    fn lerp_point(self, other: Self, t: f64) -> Self {
        self.lerp(other, t)
    }
    fn distance_sq(self, other: Self) -> f64 {
        self.distance_squared(other)
    }
}

impl XyPoint for DVec3 {
    fn xy(self) -> DVec2 {
        DVec2::new(self.x, self.y)
    }
    fn lerp_point(self, other: Self, t: f64) -> Self {
        self.lerp(other, t)
    }
    fn distance_sq(self, other: Self) -> f64 {
        self.distance_squared(other)
    }
}

impl XyPoint for crate::model::formats::mesh_data::Vertex {
    fn xy(self) -> DVec2 {
        DVec2::new(self.x, self.y)
    }
    fn lerp_point(self, other: Self, t: f64) -> Self {
        Self {
            x: self.x + (other.x - self.x) * t,
            y: self.y + (other.y - self.y) * t,
            z: self.z + (other.z - self.z) * t,
        }
    }
    fn distance_sq(self, other: Self) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        dx * dx + dy * dy + dz * dz
    }
}

/// Signed area of a polyline (XY plane only). Positive = CCW, negative = CW.
pub(crate) fn signed_area_xy<P: XyPoint>(verts: &[P]) -> f64 {
    let n = verts.len();
    let mut area = 0.0_f64;
    for i in 0..n {
        let a = verts[i].xy();
        let b = verts[(i + 1) % n].xy();
        area += a.x * b.y - b.x * a.y;
    }
    area * 0.5
}

/// Signed XY area of a triangle. Positive = CCW viewed from above.
#[inline(always)]
pub(crate) fn triangle_xy_area<P: XyPoint>(triangle: [P; 3]) -> f64 {
    let [a, b, c] = triangle.map(XyPoint::xy);
    ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)) * 0.5
}

/// Squared-distance tolerance for collapsing coincident ring vertices
/// produced by clipping (1e-10 m).
pub(crate) const RING_DEDUP_EPS_SQ: f64 = 1e-20;

/// Drop consecutive ring vertices considered coincident by `close`, treating
/// the ring as closed (a trailing vertex coincident with the first is popped).
pub(crate) fn deduplicate_ring_by<P: Copy>(mut ring: Vec<P>, close: impl Fn(P, P) -> bool) -> Vec<P> {
    ring.dedup_by(|a, b| close(*a, *b));
    if ring.len() > 1 && close(ring[0], *ring.last().expect("non-empty")) {
        ring.pop();
    }
    ring
}

/// One Sutherland–Hodgman pass: clip `polyline` against the half-plane on the
/// interior side of the directed edge `edge_a -> edge_b` (`clip_ccw` gives the
/// winding of the clip polyline the edge belongs to). Non-XY components of the
/// points are carried through interpolation.
pub(crate) fn clip_polyline_by_xy_edge<P: XyPoint>(polyline: &[P], edge_a: DVec2, edge_b: DVec2, clip_ccw: bool) -> Vec<P> {
    if polyline.is_empty() {
        return Vec::new();
    }
    // Signed distance in metres (scale-independent of edge length), so the
    // on-boundary tolerance means the same thing for every clip edge.
    let signed_distance = |point: P| {
        let d = crate::model::kernel::signed_distance_to_line(point.xy(), edge_a, edge_b);
        if clip_ccw { d } else { -d }
    };
    const INSIDE_TOL: f64 = crate::model::kernel::XY_TOL;

    let mut output = Vec::new();
    let mut previous = *polyline.last().expect("polyline is non-empty");
    let mut previous_distance = signed_distance(previous);
    let mut previous_inside = previous_distance >= -INSIDE_TOL;
    for &current in polyline {
        let current_distance = signed_distance(current);
        let current_inside = current_distance >= -INSIDE_TOL;
        if current_inside != previous_inside {
            let denominator = previous_distance - current_distance;
            if denominator.abs() > 1e-20 {
                let t = (previous_distance / denominator).clamp(0.0, 1.0);
                output.push(previous.lerp_point(current, t));
            }
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_distance = current_distance;
        previous_inside = current_inside;
    }
    deduplicate_ring_by(output, |a, b| a.distance_sq(b) <= RING_DEDUP_EPS_SQ)
}

/// Return which sign of `horiz_dist` places the offset on the cursor's side.
/// Returns `1.0` or `-1.0`. For closed polylines uses a point-in-polyline test
/// so the result correctly tracks inside vs outside regardless of polyline size.
pub(crate) fn offset_side_from_cursor(verts: &[DVec3], closed: bool, cursor_xy: DVec2, abs_dist: f64) -> f64 {
    if verts.len() < 2 || abs_dist < 1e-10 {
        return 1.0;
    }

    if closed && verts.len() >= 3 {
        // geometric_offset positive d = left of each edge = inward for CCW, outward for CW.
        let area = signed_area_xy(verts);
        let positive_is_outward = area < 0.0; // CW polyline: left normal points outward
        let cursor_inside = point_in_polyline_xy(cursor_xy, verts);
        // cursor inside → want inward; cursor outside → want outward.
        let want_outward = !cursor_inside;
        if want_outward == positive_is_outward { 1.0 } else { -1.0 }
    } else {
        // Open polyline: compare which offset centroid is closer to the cursor.
        let pos = compute_offset_centroid(verts, closed, abs_dist);
        let neg = compute_offset_centroid(verts, closed, -abs_dist);
        if (pos - cursor_xy).length_squared() <= (neg - cursor_xy).length_squared() {
            1.0
        } else {
            -1.0
        }
    }
}

/// Point-in-polyline test on the XY plane. Points on the boundary (within
/// `kernel::XY_TOL`) count as inside; use [`crate::model::kernel::point_in_polyline`]
/// directly when the boundary case needs explicit handling.
pub(crate) fn point_in_polyline_xy(point: DVec2, verts: &[DVec3]) -> bool {
    !matches!(
        crate::model::kernel::point_in_polyline(point, verts.iter().map(|v| v.truncate())),
        crate::model::kernel::PolyContainment::Outside
    )
}

/// Check if segment AB strictly intersects segment CD (not at shared endpoints).
/// Returns the intersection point and its parameter along AB if found.
fn seg_seg_intersection_2d(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> Option<(DVec2, f64)> {
    match crate::model::kernel::segment_segment(a, b, c, d) {
        crate::model::kernel::SegSeg::Crossing { point, t, .. } => Some((point, t)),
        _ => None,
    }
}

/// Remove self-intersections from a closed polyline offset result (XY plane).
///
/// When an inward offset is too large for a sharp corner the adjacent edges
/// fold back and cross, creating a small loop. Keeps the loop with the
/// greatest XY area (the polyline body); see `split_self_intersection_loops`
/// for callers that want the discarded lobes too.
pub(crate) fn remove_self_intersections(verts: Vec<DVec3>) -> Vec<DVec3> {
    if verts.len() < 4 {
        return verts;
    }
    split_self_intersection_loops(verts).into_iter().next().unwrap_or_default()
}

/// Split a closed ring at its self-crossings into simple loops, largest
/// XY area first, in a single sweep.
///
/// All pairwise edge crossings are collected once (`kernel::segment_segment`
/// `Crossing`s only - endpoint touches don't split), inserted along their
/// edges, then loops are peeled with a stack: when the ring returns to a
/// crossing it has already passed, the vertices in between form one loop.
/// Interleaved (non-nested) crossing pairs - which offset fold-backs don't
/// produce - degrade gracefully: the orphaned occurrence stays a pass-through
/// vertex. Each crossing keeps the Z interpolated on its own edge.
pub(crate) fn split_self_intersection_loops(verts: Vec<DVec3>) -> Vec<Vec<DVec3>> {
    let n = verts.len();
    if n < 4 {
        return vec![verts];
    }

    // Crossing occurrences per edge: (t along edge, pair id, position).
    let mut edge_hits: Vec<Vec<(f64, usize, DVec3)>> = vec![Vec::new(); n];
    let mut pair_count = 0usize;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        // Adjacent edges share an endpoint and cannot properly cross.
        let j_end = if i == 0 { n - 1 } else { n };
        for j in (i + 2)..j_end {
            let c = verts[j];
            let d = verts[(j + 1) % n];
            if let Some((point, t)) = seg_seg_intersection_2d(a.truncate(), b.truncate(), c.truncate(), d.truncate()) {
                let u = {
                    let cd = (d - c).truncate();
                    let len_sq = cd.length_squared();
                    if len_sq > 0.0 { (point - c.truncate()).dot(cd) / len_sq } else { 0.0 }
                };
                let z_i = a.z + t * (b.z - a.z);
                let z_j = c.z + u * (d.z - c.z);
                edge_hits[i].push((t, pair_count, DVec3::new(point.x, point.y, z_i)));
                edge_hits[j].push((u, pair_count, DVec3::new(point.x, point.y, z_j)));
                pair_count += 1;
            }
        }
    }
    if pair_count == 0 {
        return vec![verts];
    }

    // The ring with crossings inserted in edge order.
    struct AugVertex {
        pos: DVec3,
        pair: Option<usize>,
    }
    let mut ring: Vec<AugVertex> = Vec::with_capacity(n + 2 * pair_count);
    for (i, hits) in edge_hits.iter_mut().enumerate() {
        ring.push(AugVertex { pos: verts[i], pair: None });
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        for &(_, pair, pos) in hits.iter() {
            ring.push(AugVertex { pos, pair: Some(pair) });
        }
    }

    // Peel loops: `open[pair]` is the stack index of the crossing's first
    // occurrence; the second occurrence closes the loop between them.
    let mut open: Vec<Option<usize>> = vec![None; pair_count];
    let mut stack: Vec<AugVertex> = Vec::with_capacity(ring.len());
    let mut loops: Vec<Vec<DVec3>> = Vec::new();
    for vertex in ring {
        let Some(pair) = vertex.pair else {
            stack.push(vertex);
            continue;
        };
        match open[pair] {
            None => {
                open[pair] = Some(stack.len());
                stack.push(vertex);
            }
            Some(start) => {
                let peeled: Vec<DVec3> = stack.drain(start..).map(|v| v.pos).collect();
                // Crossings whose partner left with the peeled loop become
                // plain vertices when the partner shows up later.
                for slot in open.iter_mut() {
                    if slot.is_some_and(|index| index >= start) {
                        *slot = None;
                    }
                }
                if peeled.len() >= 3 {
                    loops.push(peeled);
                }
                // The ring passes through the crossing once more.
                stack.push(AugVertex { pos: vertex.pos, pair: None });
            }
        }
    }
    let remainder: Vec<DVec3> = stack.into_iter().map(|v| v.pos).collect();
    if remainder.len() >= 3 {
        loops.push(remainder);
    }

    loops.sort_by(|a, b| signed_area_xy(b).abs().total_cmp(&signed_area_xy(a).abs()));
    loops
}

fn compute_offset_centroid(verts: &[DVec3], closed: bool, signed_dist: f64) -> DVec2 {
    let offset = geometric_offset(verts, closed, signed_dist, 0.0);
    if offset.is_empty() {
        return DVec2::ZERO;
    }
    let sum: DVec2 = offset.iter().map(|v| v.truncate()).sum();
    sum / offset.len() as f64
}
