//! Integrate a closed, outward-oriented boundary's signed vertical columns over
//! a block. Each column is clipped analytically as a convex polyhedron; no point
//! sampling or centroid classification is involved. Downward faces subtract the
//! space below the floor, including cavities, from the space below the roof.
use glam::DVec3;

use crate::{
    app::jobs::CancelFlag,
    model::{block_model::BlockBounds, formats::block_model_data::BlockModelData},
};

type Polyhedron = Vec<Vec<DVec3>>;

pub(super) struct Block {
    center: DVec3,
    scale: f64,
    faces: Polyhedron,
    volume: f64,
    pub(super) min: DVec3,
    pub(super) max: DVec3,
}

impl Block {
    pub(super) fn new(model: &BlockModelData, bounds: BlockBounds) -> anyhow::Result<Self> {
        let size = bounds.upper - bounds.lower;
        anyhow::ensure!(bounds.lower.is_finite() && size.is_finite() && size.min_element() > 0.0, "Invalid block bounds");
        let center = model.local_to_world(bounds.lower + size * 0.5);
        let offsets = std::array::from_fn::<_, 8, _>(|i| {
            model.rotation()
                * (size
                    * DVec3::new(
                        if i & 1 == 0 { -0.5 } else { 0.5 },
                        if i & 2 == 0 { -0.5 } else { 0.5 },
                        if i & 4 == 0 { -0.5 } else { 0.5 },
                    ))
        });
        let scale = offsets.iter().map(|p| p.abs().max_element()).fold(0.0, f64::max);
        anyhow::ensure!(center.is_finite() && scale.is_finite() && scale > 0.0, "Invalid block transform");
        let points = offsets.map(|p| p / scale);
        let faces = [[0, 1, 3, 2], [4, 5, 7, 6], [0, 1, 5, 4], [2, 3, 7, 6], [0, 2, 6, 4], [1, 3, 7, 5]]
            .map(|face| face.map(|i| points[i]).to_vec())
            .to_vec();
        let volume = polyhedron_volume(&faces);
        anyhow::ensure!(
            volume.is_finite() && volume > 0.0 && model.rotation().determinant().abs() > 0.0,
            "Degenerate block transform"
        );
        Ok(Self {
            center,
            scale,
            faces,
            volume,
            min: center + offsets.iter().copied().fold(DVec3::splat(f64::INFINITY), DVec3::min),
            max: center + offsets.iter().copied().fold(DVec3::splat(f64::NEG_INFINITY), DVec3::max),
        })
    }

    /// This block's own volume in world units, so a measured overlap
    /// fraction can be reported as cubic metres of covered ground.
    pub(super) fn world_volume(&self) -> f64 {
        self.volume * self.scale * self.scale * self.scale
    }

    pub(super) fn fraction(&self, piece: &super::ReservePiece, stack: &mut Vec<usize>, cancel: &CancelFlag) -> anyhow::Result<f64> {
        let bounds = piece.mesh.bounds();
        if self.max.x <= bounds.min.x || self.min.x >= bounds.max.x || self.max.y <= bounds.min.y || self.min.y >= bounds.max.y {
            return Ok(0.0);
        }
        let mut sum = 0.0;
        let mut correction = 0.0;
        piece
            .spatial
            .for_each_xy_bounds_candidate_index_with_stack(self.min.truncate(), self.max.truncate(), stack, |index| {
                if cancel.is_cancelled() {
                    return;
                }
                let triangle = piece.mesh.triangle(index).expect("BVH triangle exists");
                let mut points = triangle.vertices.map(|p| (DVec3::new(p.x, p.y, p.z) - self.center) / self.scale);
                let normal = (points[1] - points[0]).cross(points[2] - points[0]);
                if normal.z == 0.0 {
                    return;
                }
                let sign = normal.z.signum();
                if sign < 0.0 {
                    points.swap(1, 2);
                }
                let mut poly = self.faces.clone();
                for i in 0..3 {
                    let a = points[i];
                    let edge = points[(i + 1) % 3] - a;
                    let outward = DVec3::new(edge.y, -edge.x, 0.0).normalize();
                    clip(&mut poly, outward, outward.dot(a));
                    if poly.is_empty() {
                        return;
                    }
                }
                let roof = (normal * sign).normalize();
                clip(&mut poly, roof, roof.dot(points[0]));
                // Compensated summation limits cancellation between floor and roof.
                let value = sign * polyhedron_volume(&poly) - correction;
                let next = sum + value;
                correction = (next - sum) - value;
                sum = next;
            });
        anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
        let fraction = sum / self.volume;
        anyhow::ensure!(
            fraction.is_finite() && (-1e-7..=1.0 + 1e-7).contains(&fraction),
            "Invalid solid/block overlap fraction {fraction}; check mesh orientation and closure"
        );
        Ok(fraction.clamp(0.0, 1.0))
    }
}

/// Faces are convex and may have either winding. Tetrahedra measured from an
/// interior point partition the polyhedron, so each contributes positive volume.
fn polyhedron_volume(faces: &Polyhedron) -> f64 {
    let count = faces.iter().map(Vec::len).sum::<usize>();
    if count == 0 {
        return 0.0;
    }
    let center = faces.iter().flatten().copied().sum::<DVec3>() / count as f64;
    faces
        .iter()
        .map(|face| {
            (1..face.len().saturating_sub(1))
                .map(|i| (face[0] - center).dot((face[i] - center).cross(face[i + 1] - center)).abs() / 6.0)
                .sum::<f64>()
        })
        .sum()
}

/// Keep n.p <= distance. Working near the block origin avoids mine-coordinate
/// cancellation. Snap roundoff-sized plane distances consistently on all faces.
fn clip(faces: &mut Polyhedron, normal: DVec3, distance: f64) {
    const EPS: f64 = 1e-12;
    let signed = |point: DVec3| {
        let d = normal.dot(point) - distance;
        if d.abs() <= EPS { 0.0 } else { d }
    };
    // Most queried triangles cover the entire block in one or more planes.
    // Avoid rebuilding faces unless this plane actually cuts the block.
    if !faces.iter().flatten().any(|&point| signed(point) > 0.0) {
        return;
    }
    if !faces.iter().flatten().any(|&point| signed(point) < 0.0) {
        faces.clear();
        return;
    }
    let mut cap = Vec::<DVec3>::new();
    let mut removed = false;
    for face in faces.iter_mut() {
        let mut output = Vec::with_capacity(face.len() + 1);
        for i in 0..face.len() {
            let a = face[i];
            let b = face[(i + 1) % face.len()];
            let da = signed(a);
            let db = signed(b);
            removed |= da > 0.0;
            if da <= 0.0 {
                output.push(a);
            }
            if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
                let point = a + (b - a) * (da / (da - db));
                output.push(point);
                cap.push(point);
            }
            if da == 0.0 {
                cap.push(a);
            }
        }
        *face = output;
    }
    faces.retain(|face| face.len() >= 3);
    if !removed || faces.is_empty() {
        return;
    }
    let mut unique = Vec::<DVec3>::new();
    for p in cap {
        if !unique.iter().any(|q| p.distance_squared(*q) <= EPS * EPS) {
            unique.push(p);
        }
    }
    if unique.len() < 3 {
        return;
    }
    let center = unique.iter().copied().sum::<DVec3>() / unique.len() as f64;
    let axis = if normal.x.abs() < 0.8 { DVec3::X } else { DVec3::Y };
    let u = normal.cross(axis).normalize();
    let v = normal.cross(u);
    unique.sort_by(|a, b| {
        let a = *a - center;
        let b = *b - center;
        a.dot(v).atan2(a.dot(u)).total_cmp(&b.dot(v).atan2(b.dot(u)))
    });
    faces.push(unique);
}
