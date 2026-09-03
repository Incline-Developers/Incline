//! Spatial acceleration structures for immutable mine geometry.

use glam::{DMat4, DVec2, DVec3};

use crate::model::{
    Document, FillStyle, Object, PolyVertex,
    formats::mesh_data,
    geometry::{polyline_bulge_bounds, text_bounds_corners, triangulate_polyline_fill},
};

/// BVH over document objects, built once when the scene document changes.
/// Allows `snap_cursor` to find nearby objects in O(log N) instead of O(N).
#[derive(Clone, Debug, Default)]
pub(crate) struct ObjectSnapIndex {
    bboxes: Vec<(DVec3, DVec3)>,
    /// Ray-test meshes for closed solid fills, aligned with document object
    /// indices. Boundary tessellation and triangulation are paid once per
    /// document revision rather than on every snap poll.
    filled_polylines: Vec<Option<FilledPolyline>>,
    order: Vec<u32>,
    nodes: Vec<Node>,
}

impl ObjectSnapIndex {
    pub(crate) fn build(document: &Document) -> Self {
        let filled_polylines: Vec<Option<FilledPolyline>> = document.objects().iter().map(FilledPolyline::from_object).collect();
        let bboxes: Vec<(DVec3, DVec3)> = document
            .objects()
            .iter()
            .enumerate()
            .map(|(index, object)| {
                let bounds = object_bbox(object);
                filled_polylines[index].as_ref().map_or(bounds, |polyline| union_bounds(bounds, polyline.bounds))
            })
            .collect();
        let order: Vec<u32> = bboxes
            .iter()
            .enumerate()
            .filter_map(|(index, (min, max))| bbox_valid(*min, *max).then_some(index as u32))
            .collect();
        let n = order.len();
        let mut index = Self {
            bboxes,
            filled_polylines,
            order,
            nodes: Vec::new(),
        };
        if n > 0 {
            index.build_node(0, n);
        }
        index
    }

    /// Return object indices (into the original `document.objects()` slice) whose
    /// projected AABB overlaps the cursor region in screen space.
    pub(crate) fn candidates(&self, view_projection: &DMat4, screen: (f32, f32), cursor: DVec2, threshold: f64) -> Vec<usize> {
        if self.nodes.is_empty() {
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut stack = vec![0usize];
        while let Some(idx) = stack.pop() {
            let node = self.nodes[idx];
            if !projected_box_overlaps(node.min, node.max, view_projection, screen, cursor, threshold) {
                continue;
            }
            if node.count > 0 {
                let range = node.start as usize..(node.start + node.count) as usize;
                result.extend(self.order[range].iter().map(|&i| i as usize));
            } else {
                stack.push(node.left as usize);
                stack.push(node.right as usize);
            }
        }
        result
    }

    /// Find the nearest cached solid-polyline hit along a ray. The BVH rejects
    /// distant objects before `eligible` is called, so visibility/opacity
    /// checks and polyline tests scale with ray-local candidates rather than
    /// every object in the document.
    pub(crate) fn nearest_filled_polyline_hit(&self, origin: DVec3, direction: DVec3, mut eligible: impl FnMut(usize) -> bool) -> Option<DVec3> {
        if self.nodes.is_empty() || !origin.is_finite() || !direction.is_finite() || direction.length_squared() <= f64::EPSILON {
            return None;
        }

        let mut stack = vec![0usize];
        let mut nearest_distance = f64::INFINITY;
        while let Some(node_index) = stack.pop() {
            let node = self.nodes[node_index];
            if !ray_box(origin, direction, node.min, node.max, nearest_distance) {
                continue;
            }
            if node.count == 0 {
                stack.push(node.left as usize);
                stack.push(node.right as usize);
                continue;
            }

            let range = node.start as usize..(node.start + node.count) as usize;
            for &object_index in &self.order[range] {
                let object_index = object_index as usize;
                let Some(Some(polyline)) = self.filled_polylines.get(object_index) else {
                    continue;
                };
                let Some(&(min, max)) = self.bboxes.get(object_index) else {
                    continue;
                };
                if !ray_box(origin, direction, min, max, nearest_distance) || !eligible(object_index) {
                    continue;
                }
                let Some(distance) = polyline.ray_distance(origin, direction) else {
                    continue;
                };
                if distance < nearest_distance {
                    nearest_distance = distance;
                }
            }
        }

        nearest_distance.is_finite().then(|| origin + direction * nearest_distance)
    }

    fn build_node(&mut self, start: usize, end: usize) -> u32 {
        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = DVec3::splat(f64::NEG_INFINITY);
        let mut center_min = DVec3::splat(f64::INFINITY);
        let mut center_max = DVec3::splat(f64::NEG_INFINITY);
        for &i in &self.order[start..end] {
            let (bmin, bmax) = self.bboxes[i as usize];
            min = min.min(bmin);
            max = max.max(bmax);
            let center = (bmin + bmax) * 0.5;
            center_min = center_min.min(center);
            center_max = center_max.max(center);
        }
        let node_index = self.nodes.len() as u32;
        self.nodes.push(Node {
            min,
            max,
            left: 0,
            right: 0,
            start: start as u32,
            count: (end - start) as u32,
        });
        if end - start <= 8 {
            return node_index;
        }
        let extent = center_max - center_min;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };
        let middle = start + (end - start) / 2;
        let bboxes = &self.bboxes;
        self.order[start..end].select_nth_unstable_by(middle - start, |&a, &b| {
            let ca = (bboxes[a as usize].0 + bboxes[a as usize].1)[axis];
            let cb = (bboxes[b as usize].0 + bboxes[b as usize].1)[axis];
            ca.total_cmp(&cb)
        });
        let left = self.build_node(start, middle);
        let right = self.build_node(middle, end);
        self.nodes[node_index as usize].left = left;
        self.nodes[node_index as usize].right = right;
        self.nodes[node_index as usize].count = 0;
        node_index
    }
}

#[derive(Clone, Debug)]
struct FilledPolyline {
    triangles: Vec<[DVec3; 3]>,
    bounds: (DVec3, DVec3),
}

impl FilledPolyline {
    fn from_object(object: &Object) -> Option<Self> {
        let Object::Polyline {
            verts,
            closed: true,
            fill: FillStyle::Solid,
            ..
        } = object
        else {
            return None;
        };
        Self::from_vertices(verts)
    }

    fn from_vertices(verts: &[PolyVertex]) -> Option<Self> {
        let mesh = triangulate_polyline_fill(verts)?;
        let bounds = points_bbox(&mesh.vertices);
        let triangles = mesh
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|triangle| triangle.map(|index| mesh.vertices[index as usize]))
            .collect();
        Some(Self { triangles, bounds })
    }

    fn ray_distance(&self, origin: DVec3, direction: DVec3) -> Option<f64> {
        self.triangles
            .iter()
            .filter_map(|triangle| ray_triangle(origin, direction, *triangle))
            .min_by(f64::total_cmp)
    }
}

fn union_bounds(a: (DVec3, DVec3), b: (DVec3, DVec3)) -> (DVec3, DVec3) {
    (a.0.min(b.0), a.1.max(b.1))
}

fn object_bbox(object: &Object) -> (DVec3, DVec3) {
    match object {
        Object::Point { pos, .. } => (*pos, *pos),
        Object::Text {
            pos, content, height, rotation, ..
        } => points_bbox(&text_bounds_corners(*pos, content, *height, *rotation)),
        Object::Polyline { verts, closed, .. } => polyline_bulge_bounds(verts, *closed).unwrap_or((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY))),
    }
}

fn points_bbox(points: &[DVec3]) -> (DVec3, DVec3) {
    if points.is_empty() {
        return (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    }
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for &point in points {
        min = min.min(point);
        max = max.max(point);
    }
    if min.x > max.x {
        return (DVec3::ZERO, DVec3::ZERO);
    }
    (min, max)
}

fn bbox_valid(min: DVec3, max: DVec3) -> bool {
    min.is_finite() && max.is_finite() && min.x <= max.x && min.y <= max.y && min.z <= max.z
}

#[derive(Clone, Debug)]
pub(crate) struct TriangleBvh {
    /// World-space reference point subtracted before f32 conversion of node bounds.
    origin: DVec3,
    xy_bounds: Vec<TriangleXyBounds>,
    order: Vec<u32>,
    /// Compact 28-byte nodes in depth-first layout (see `TriNode`).
    nodes: Vec<TriNode>,
}

#[derive(Clone, Copy, Debug)]
struct TriangleXyBounds {
    min: [f32; 2],
    max: [f32; 2],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TriangleHit {
    pub(crate) point: DVec3,
}

/// BVH node for document objects - world-space DVec3 bounds, used by ObjectSnapIndex.
#[derive(Clone, Copy, Debug)]
struct Node {
    min: DVec3,
    max: DVec3,
    left: u32,
    right: u32,
    start: u32,
    count: u32,
}

/// Compact 28-byte BVH node for triangulation meshes.
///
/// Bounds are stored as local f32 relative to `TriangleBvh::origin`.
/// Nodes are laid out in depth-first order so the left child is always
/// at `index + 1`, saving one field over the naive 4-pointer layout.
///
/// Internal nodes (count == 0): `right_child_or_start` = right child index.
/// Leaf nodes    (count  > 0): `right_child_or_start` = start offset into `order`.
#[derive(Clone, Copy, Debug)]
struct TriNode {
    min: [f32; 3],
    max: [f32; 3],
    right_child_or_start: u32,
    count: u32,
}

/// Temporary 40-byte node produced by the parallel recursive build before
/// being compacted into depth-first `TriNode` layout.
#[derive(Clone, Copy)]
struct BuildNode {
    min: [f32; 3],
    max: [f32; 3],
    left: u32,
    right: u32,
    start: u32,
    count: u32,
}

#[derive(Clone, Copy)]
struct BuildTriangleInfo {
    min: [f32; 3],
    max: [f32; 3],
    centroid: [f32; 3],
    xy_bounds: TriangleXyBounds,
}

impl TriangleBvh {
    pub(crate) fn build(mesh: &mesh_data::Triangulation) -> Self {
        let b = mesh.bounds();
        let origin = DVec3::new((b.min.x + b.max.x) * 0.5, (b.min.y + b.max.y) * 0.5, (b.min.z + b.max.z) * 0.5);
        // Temporary f32 vertex cache for centroid/bounds computation during build.
        // Freed once the BVH is constructed - not stored in the struct.
        let build_vertices: Vec<[f32; 3]> = mesh
            .vertices()
            .iter()
            .map(|v| [(v.x - origin.x) as f32, (v.y - origin.y) as f32, (v.z - origin.z) as f32])
            .collect();
        let build_triangles: Vec<[u32; 3]> = mesh.face_vertex_indices_iter().map(|face| face.map(|i| i as u32)).collect();
        let build_infos: Vec<BuildTriangleInfo> = build_triangles.iter().map(|triangle| build_triangle_info(*triangle, &build_vertices)).collect();
        let xy_bounds = build_infos.iter().map(|info| info.xy_bounds).collect();
        let mut order: Vec<u32> = (0..build_triangles.len() as u32).collect();
        let nodes = if order.is_empty() {
            Vec::new()
        } else {
            let wide = build_triangle_subtree(&mut order, 0, &build_infos);
            flatten_to_dfs(&wide)
        };
        Self { origin, xy_bounds, order, nodes }
    }

    pub(crate) fn ray_hit(&self, mesh: &mesh_data::Triangulation, origin: DVec3, direction: DVec3) -> Option<DVec3> {
        self.ray_hit_details(mesh, origin, direction).map(|hit| hit.point)
    }

    pub(crate) fn ray_hit_details(&self, mesh: &mesh_data::Triangulation, origin: DVec3, direction: DVec3) -> Option<TriangleHit> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut stack = vec![0usize];
        let mut nearest = f64::INFINITY;
        while let Some(index) = stack.pop() {
            let node = self.nodes[index];
            if !ray_box(origin, direction, self.node_min(&node), self.node_max(&node), nearest) {
                continue;
            }
            if node.count > 0 {
                let start = node.right_child_or_start as usize;
                for &triangle_index in &self.order[start..start + node.count as usize] {
                    if let Some(distance) = ray_triangle(origin, direction, self.triangle(mesh, triangle_index as usize))
                        && distance < nearest
                    {
                        nearest = distance;
                    }
                }
            } else {
                stack.push(node.right_child_or_start as usize);
                stack.push(index + 1);
            }
        }
        nearest.is_finite().then_some(TriangleHit {
            point: origin + direction * nearest,
        })
    }

    /// Visit triangles whose projected BVH bounds overlap a cursor-sized
    /// screen region. This keeps vertex/edge snapping proportional to nearby
    /// geometry rather than the entire mesh, without materialising the
    /// candidate set (dense meshes put thousands of triangles in a 15 px
    /// radius).
    pub(crate) fn for_each_screen_candidate(
        &self,
        mesh: &mesh_data::Triangulation,
        view_projection: &DMat4,
        screen: (f32, f32),
        cursor: (f32, f32),
        threshold: f32,
        mut visit: impl FnMut([DVec3; 3]),
    ) {
        if self.nodes.is_empty() {
            return;
        }
        let cursor = DVec2::new(cursor.0 as f64, cursor.1 as f64);
        let threshold = threshold as f64;
        let mut stack = vec![0usize];
        while let Some(index) = stack.pop() {
            let node = self.nodes[index];
            if !projected_box_overlaps(self.node_min(&node), self.node_max(&node), view_projection, screen, cursor, threshold) {
                continue;
            }
            if node.count > 0 {
                let start = node.right_child_or_start as usize;
                for &i in &self.order[start..start + node.count as usize] {
                    visit(self.triangle(mesh, i as usize));
                }
            } else {
                stack.push(node.right_child_or_start as usize);
                stack.push(index + 1);
            }
        }
    }

    /// Return triangle indices whose XY bounds overlap the supplied rectangle.
    pub(crate) fn xy_bounds_candidate_indices(&self, _mesh: &mesh_data::Triangulation, min: DVec2, max: DVec2) -> Vec<usize> {
        let mut result = Vec::new();
        self.for_each_xy_bounds_candidate_index(min, max, |index| result.push(index));
        result
    }

    /// Visit triangle indices whose XY bounds overlap the supplied rectangle.
    ///
    /// This is the allocation-free version used by clipping paths that run the
    /// query thousands of times. It uses per-triangle XY bounds cached when the
    /// BVH is built, so leaf filtering does not have to refetch vertices.
    pub(crate) fn for_each_xy_bounds_candidate_index(&self, min: DVec2, max: DVec2, visit: impl FnMut(usize)) {
        let mut stack = Vec::new();
        self.for_each_xy_bounds_candidate_index_with_stack(min, max, &mut stack, visit);
    }

    pub(crate) fn for_each_xy_bounds_candidate_index_with_stack(&self, min: DVec2, max: DVec2, stack: &mut Vec<usize>, mut visit: impl FnMut(usize)) {
        if self.nodes.is_empty() {
            return;
        }
        stack.clear();
        stack.push(0);
        let local_min_x = min.x - self.origin.x;
        let local_min_y = min.y - self.origin.y;
        let local_max_x = max.x - self.origin.x;
        let local_max_y = max.y - self.origin.y;
        let tol = crate::model::kernel::XY_TOL;
        while let Some(index) = stack.pop() {
            let node = self.nodes[index];
            if f64::from(node.min[0]) > local_max_x + tol
                || f64::from(node.max[0]) < local_min_x - tol
                || f64::from(node.min[1]) > local_max_y + tol
                || f64::from(node.max[1]) < local_min_y - tol
            {
                continue;
            }
            if node.count > 0 {
                let start = node.right_child_or_start as usize;
                for &triangle_index in &self.order[start..start + node.count as usize] {
                    let bounds = self.xy_bounds[triangle_index as usize];
                    if f64::from(bounds.min[0]) <= local_max_x + tol
                        && f64::from(bounds.max[0]) >= local_min_x - tol
                        && f64::from(bounds.min[1]) <= local_max_y + tol
                        && f64::from(bounds.max[1]) >= local_min_y - tol
                    {
                        visit(triangle_index as usize);
                    }
                }
            } else {
                stack.push(node.right_child_or_start as usize);
                stack.push(index + 1);
            }
        }
    }

    fn triangle(&self, mesh: &mesh_data::Triangulation, index: usize) -> [DVec3; 3] {
        mesh.face_vertex_indices(index)
            .unwrap_or_else(|| {
                // The BVH `order` array is built from this mesh, so every
                // index must resolve; substituting a degenerate triangle keeps
                // ray tests safe but must not pass silently.
                debug_assert!(false, "BVH face index {index} out of range");
                log::error!(
                    "{}",
                    crate::i18n::tr_format!(literal = "BVH face index %index% out of range for mesh; substituting degenerate triangle", index = index)
                );
                [0, 0, 0]
            })
            .map(|v| {
                let p = mesh.vertices()[v];
                DVec3::new(p.x, p.y, p.z)
            })
    }

    fn node_min(&self, node: &TriNode) -> DVec3 {
        self.origin + DVec3::new(node.min[0] as f64, node.min[1] as f64, node.min[2] as f64)
    }

    fn node_max(&self, node: &TriNode) -> DVec3 {
        self.origin + DVec3::new(node.max[0] as f64, node.max[1] as f64, node.max[2] as f64)
    }
}

/// Convert the wide arbitrary-indexed `BuildNode` tree into the compact
/// depth-first `TriNode` layout where left child = parent_index + 1.
fn flatten_to_dfs(wide: &[BuildNode]) -> Vec<TriNode> {
    let mut out = Vec::with_capacity(wide.len());
    if !wide.is_empty() {
        dfs_serialize(wide, 0, &mut out);
    }
    out
}

fn dfs_serialize(wide: &[BuildNode], idx: usize, out: &mut Vec<TriNode>) {
    let node = wide[idx];
    let my_idx = out.len();
    if node.count > 0 {
        out.push(TriNode {
            min: node.min,
            max: node.max,
            right_child_or_start: node.start,
            count: node.count,
        });
    } else {
        out.push(TriNode {
            min: node.min,
            max: node.max,
            right_child_or_start: 0,
            count: 0,
        });
        dfs_serialize(wide, node.left as usize, out);
        let right_idx = out.len() as u32;
        out[my_idx].right_child_or_start = right_idx;
        dfs_serialize(wide, node.right as usize, out);
    }
}

/// Minimum subtree size to spawn a rayon parallel task. Below this threshold
/// the recursion finishes serially to avoid thread-pool overhead on tiny slices.
const PARALLEL_MIN_TRIANGLES: usize = 2048;

/// Build a BVH subtree for `order` (a sub-slice of the global order array)
/// and return a `Vec<BuildNode>` whose indices are self-contained (index 0 = root).
///
/// `order_start` is the offset of `order[0]` in the global order array, used
/// to fill leaf `BuildNode::start` correctly.
fn build_triangle_subtree(order: &mut [u32], order_start: usize, triangle_infos: &[BuildTriangleInfo]) -> Vec<BuildNode> {
    let n = order.len();

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut center_min = [f32::INFINITY; 3];
    let mut center_max = [f32::NEG_INFINITY; 3];
    for &idx in order.iter() {
        let info = triangle_infos[idx as usize];
        for i in 0..3 {
            min[i] = min[i].min(info.min[i]);
            max[i] = max[i].max(info.max[i]);
            center_min[i] = center_min[i].min(info.centroid[i]);
            center_max[i] = center_max[i].max(info.centroid[i]);
        }
    }

    if n <= 8 {
        return vec![BuildNode {
            min,
            max,
            left: 0,
            right: 0,
            start: order_start as u32,
            count: n as u32,
        }];
    }

    let extent = [center_max[0] - center_min[0], center_max[1] - center_min[1], center_max[2] - center_min[2]];
    let axis = if extent[0] >= extent[1] && extent[0] >= extent[2] {
        0
    } else if extent[1] >= extent[2] {
        1
    } else {
        2
    };

    let middle = n / 2;
    order.select_nth_unstable_by(middle, |a, b| {
        triangle_infos[*a as usize].centroid[axis].total_cmp(&triangle_infos[*b as usize].centroid[axis])
    });

    let (left_order, right_order) = order.split_at_mut(middle);

    let (mut left_nodes, mut right_nodes) = if n > PARALLEL_MIN_TRIANGLES * 2 {
        rayon::join(
            || build_triangle_subtree(left_order, order_start, triangle_infos),
            || build_triangle_subtree(right_order, order_start + middle, triangle_infos),
        )
    } else {
        (
            build_triangle_subtree(left_order, order_start, triangle_infos),
            build_triangle_subtree(right_order, order_start + middle, triangle_infos),
        )
    };

    // Merge: parent at [0], left subtree at [1..], right subtree at [1+left.len()..].
    let right_offset = 1 + left_nodes.len() as u32;
    shift_build_node_indices(&mut left_nodes, 1);
    shift_build_node_indices(&mut right_nodes, right_offset);

    let mut result = Vec::with_capacity(1 + left_nodes.len() + right_nodes.len());
    result.push(BuildNode {
        min,
        max,
        left: 1,
        right: right_offset,
        start: order_start as u32,
        count: 0,
    });
    result.extend(left_nodes);
    result.extend(right_nodes);
    result
}

fn shift_build_node_indices(nodes: &mut [BuildNode], offset: u32) {
    for node in nodes.iter_mut() {
        if node.count == 0 {
            node.left += offset;
            node.right += offset;
        }
    }
}

pub(crate) fn projected_box_overlaps(min: DVec3, max: DVec3, view_projection: &DMat4, screen: (f32, f32), cursor: DVec2, threshold: f64) -> bool {
    let mut screen_min = DVec2::splat(f64::INFINITY);
    let mut screen_max = DVec2::splat(f64::NEG_INFINITY);
    let mut any = false;
    let mut behind = 0;
    for x in [min.x, max.x] {
        for y in [min.y, max.y] {
            for z in [min.z, max.z] {
                let clip = *view_projection * DVec3::new(x, y, z).extend(1.0);
                if clip.w <= f64::EPSILON {
                    behind += 1;
                    continue;
                }
                let ndc = clip.truncate() / clip.w;
                let point = DVec2::new((ndc.x * 0.5 + 0.5) * screen.0 as f64, (0.5 - ndc.y * 0.5) * screen.1 as f64);
                screen_min = screen_min.min(point);
                screen_max = screen_max.max(point);
                any = true;
            }
        }
    }
    // A box straddling the near plane cannot be projected reliably; keep it
    // (descend into children, which eventually land wholly on one side).
    // A box wholly behind the eye can never contain the cursor hit.
    if behind > 0 {
        return any;
    }
    any && cursor.x >= screen_min.x - threshold && cursor.x <= screen_max.x + threshold && cursor.y >= screen_min.y - threshold && cursor.y <= screen_max.y + threshold
}

fn build_triangle_info(triangle: [u32; 3], vertices: &[[f32; 3]]) -> BuildTriangleInfo {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut sum = [0.0; 3];
    for vertex_index in triangle {
        let vertex = vertices[vertex_index as usize];
        for i in 0..3 {
            min[i] = min[i].min(vertex[i]);
            max[i] = max[i].max(vertex[i]);
            sum[i] += vertex[i];
        }
    }
    // The f64 mesh coordinates were narrowed into the temporary f32 cache.
    // Pad each stored bound by one representable value so rounding can never
    // move a BVH face inward and reject a triangle lying on its boundary.
    for axis in 0..3 {
        min[axis] = min[axis].next_down();
        max[axis] = max[axis].next_up();
    }
    BuildTriangleInfo {
        min,
        max,
        centroid: [sum[0] / 3.0, sum[1] / 3.0, sum[2] / 3.0],
        xy_bounds: TriangleXyBounds {
            min: [min[0], min[1]],
            max: [max[0], max[1]],
        },
    }
}

fn ray_box(origin: DVec3, direction: DVec3, min: DVec3, max: DVec3, limit: f64) -> bool {
    let mut near: f64 = 0.0;
    let mut far = limit;
    for axis in 0..3 {
        let ray_origin = origin[axis];
        let ray_direction = direction[axis];
        let slab_min = min[axis];
        let slab_max = max[axis];
        if ray_direction.abs() <= f64::EPSILON {
            // Multiplying a zero delta by an infinite reciprocal produces
            // NaN exactly on a slab boundary. Handle parallel axes directly.
            if ray_origin < slab_min || ray_origin > slab_max {
                return false;
            }
            continue;
        }
        let inverse = ray_direction.recip();
        let t0 = (slab_min - ray_origin) * inverse;
        let t1 = (slab_max - ray_origin) * inverse;
        near = near.max(t0.min(t1));
        far = far.min(t0.max(t1));
        if near > far {
            return false;
        }
    }
    true
}

fn ray_triangle(origin: DVec3, direction: DVec3, triangle: [DVec3; 3]) -> Option<f64> {
    let edge1 = triangle[1] - triangle[0];
    let edge2 = triangle[2] - triangle[0];
    let p = direction.cross(edge2);
    let determinant = edge1.dot(p);
    if determinant.abs() < 1.0e-12 {
        return None;
    }
    let inverse = determinant.recip();
    let t = origin - triangle[0];
    let u = t.dot(p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = t.cross(edge1);
    let v = direction.dot(q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = edge2.dot(q) * inverse;
    (distance >= 0.0).then_some(distance)
}
