//! Build a closed solid from two surfaces - the staple "volume between a
//! design and the ground" construction.
//!
//! Feeds both the Triangulation menu's own tool and the Solids Setup page's
//! inspection preview, so the two can never disagree about what a solid is.

use anyhow::Result;

use super::{
    cuts::clip_mesh_by_surface,
    session::{self},
    *,
};
use crate::ui::state::SolidRegion;

/// The two sheets a solid is bounded by, before they are welded into one mesh.
///
/// A solid is the space between the design surface and the topography over the
/// area where the design is on the wanted side of the ground - below it for a
/// cut, above it for a fill. Its floor and roof are one piece of each surface:
/// for a cut, the design is the floor and the topography the roof, and for a
/// fill the two swap. Both are clipped at the line where the surfaces cross,
/// where the two clips agree exactly, so the welded mesh closes along it with
/// no stitched side wall.
///
/// The *other* region the two surfaces bound - the fill either side of a pit's
/// crest, say - belongs to a different solid and is deliberately not part of
/// this one.
struct SolidEnvelopes {
    lower: (Vec<mesh_data::Vertex>, Vec<[u32; 3]>),
    upper: (Vec<mesh_data::Vertex>, Vec<[u32; 3]>),
}

/// Face-winding convention for one envelope: outward from the enclosed volume.
#[derive(Clone, Copy, PartialEq)]
enum Facing {
    /// The floor, whose outward normal points down.
    Down,
    /// The roof, whose outward normal points up.
    Up,
}

/// Append `(vertices, faces)` onto `output`, rewinding each face so its normal
/// points the way `facing` asks.
///
/// The inputs are 2.5D fragments of clipped surfaces - `clip_mesh_by_surface`
/// drops the vertical and degenerate faces that have no XY area - so the sign
/// of a face's normal Z is exactly its up/down orientation, and flipping two
/// indices is all a face ever needs. The source meshes' own winding is
/// therefore irrelevant, which matters because an imported design surface and
/// an imported topography rarely agree about it.
fn append_oriented(source: (Vec<mesh_data::Vertex>, Vec<[u32; 3]>), facing: Facing, output: &mut (Vec<mesh_data::Vertex>, Vec<[u32; 3]>)) {
    let (vertices, faces) = source;
    let base = output.0.len() as u32;
    for face in faces {
        let corners = face.map(|index| vertices[index as usize]);
        let edge_a = glam::DVec2::new(corners[1].x - corners[0].x, corners[1].y - corners[0].y);
        let edge_b = glam::DVec2::new(corners[2].x - corners[0].x, corners[2].y - corners[0].y);
        let normal_z = edge_a.x * edge_b.y - edge_a.y * edge_b.x;
        let points_up = normal_z > 0.0;
        let wanted_up = facing == Facing::Up;
        let face = [base + face[0], base + face[1], base + face[2]];
        output.1.push(if points_up == wanted_up { face } else { [face[0], face[2], face[1]] });
    }
    output.0.extend(vertices);
}

/// Clip each surface to the wanted region and hand back the two sheets that
/// bound it.
fn build_envelopes(
    design: &mesh_data::Triangulation,
    topography: &mesh_data::Triangulation,
    region: SolidRegion,
    progress: &crate::model::progress::Phase,
) -> Result<SolidEnvelopes> {
    // `CutTop` keeps what lies below the reference, `CutBottom` what lies
    // above it. For a cut the floor is the design where it runs below the
    // ground and the roof is the ground above it; for a fill the roles - and
    // so the sides - are the other way round.
    let (design_side, topography_side) = match region {
        SolidRegion::Cut => (TriSurfaceCutSide::CutTop, TriSurfaceCutSide::CutBottom),
        SolidRegion::Fill => (TriSurfaceCutSide::CutBottom, TriSurfaceCutSide::CutTop),
    };
    let design_sheet = clip_mesh_by_surface(design, topography, design_side, &progress.phase(0.0, 0.5))?;
    let topography_sheet = clip_mesh_by_surface(topography, design, topography_side, &progress.phase(0.5, 1.0))?;

    let (floor, roof) = match region {
        SolidRegion::Cut => (design_sheet, topography_sheet),
        SolidRegion::Fill => (topography_sheet, design_sheet),
    };

    let mut lower = (Vec::new(), Vec::new());
    append_oriented(floor, Facing::Down, &mut lower);
    let mut upper = (Vec::new(), Vec::new());
    append_oriented(roof, Facing::Up, &mut upper);

    Ok(SolidEnvelopes { lower, upper })
}

/// The volume enclosed between two surfaces on one side of their crossing, as
/// one mesh.
///
/// Returns the solid's vertices and faces plus the volume it encloses, in
/// cubic model units. Errors when there is nothing to enclose: surfaces whose
/// XY footprints miss each other, or a design that never runs on the wanted
/// side of the ground.
pub(crate) fn build_solid_between_surfaces(
    design: &mesh_data::Triangulation,
    topography: &mesh_data::Triangulation,
    region: SolidRegion,
    progress: &crate::model::progress::Phase,
) -> Result<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>, f64)> {
    // Surface intersections must be computed near the origin: line and
    // barycentric arithmetic on mine eastings/northings loses enough precision
    // to tear matching rims apart. Restore domain coordinates only afterwards.
    let origin = design.bounds().min;
    let local = |mesh: &mesh_data::Triangulation| -> Result<mesh_data::Triangulation> {
        mesh_data::Triangulation::from_vertices_and_faces(
            mesh.vertices()
                .iter()
                .map(|v| mesh_data::Vertex::new(v.x - origin.x, v.y - origin.y, v.z - origin.z))
                .collect(),
            mesh.face_vertex_indices_iter().map(|f| f.map(|i| i as u32)).collect(),
        )
        .map_err(|error| anyhow::anyhow!("{error}"))
    };
    let (mut vertices, faces, volume) = build_solid_local(&local(design)?, &local(topography)?, region, progress)?;
    for v in &mut vertices {
        v.x += origin.x;
        v.y += origin.y;
        v.z += origin.z;
    }
    Ok((vertices, faces, volume))
}

fn build_solid_local(
    design: &mesh_data::Triangulation,
    topography: &mesh_data::Triangulation,
    region: SolidRegion,
    progress: &crate::model::progress::Phase,
) -> Result<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>, f64)> {
    let SolidEnvelopes { lower, upper } = build_envelopes(design, topography, region, progress)?;
    if lower.1.is_empty() || upper.1.is_empty() {
        anyhow::bail!(
            "The two surfaces enclose no {} volume: the design surface is nowhere {} the topography within the area they share",
            region.label().to_lowercase(),
            match region {
                SolidRegion::Cut => "below",
                SolidRegion::Fill => "above",
            }
        );
    }

    let volume = volume_between(&lower, &upper);
    let walls = close_region_sides(&lower, &upper);

    let mut vertices = lower.0;
    let mut faces = lower.1;
    for part in [upper, walls] {
        let base = vertices.len() as u32;
        vertices.extend(part.0);
        faces.extend(part.1.into_iter().map(|face| [base + face[0], base + face[1], base + face[2]]));
    }

    // The sheets close against each other along the line where the surfaces
    // cross, and against the wall everywhere else. Anything still open is a
    // rim the wall could not trace - a sheet torn into pieces too small to
    // ring, say - and is worth saying rather than passing off as a solid.
    let open_edges = open_edge_count(&vertices, &faces);
    if open_edges > 0 {
        userspace_warn!(
            "{}",
            tr_format!(
                literal = "Solid is open along %count% edge(s): the two surfaces do not meet all the way round, so this is a shell between them rather than a closed solid. Its volume is still exact.",
                count = open_edges.to_string()
            )
        );
    }

    Ok((vertices, faces, volume))
}

/// The volume between the two sheets, by integrating their height difference
/// over the region they share.
///
/// Both sheets cover exactly the same XY region - they are the two surfaces
/// clipped by the same predicate - so the volume is the roof's integral of z
/// minus the floor's, face by face. Unlike a divergence-theorem sum over the
/// whole mesh this needs no closed surface, so it stays exact for a solid
/// whose sides are open (see [`open_edge_count`]).
fn volume_between(lower: &(Vec<mesh_data::Vertex>, Vec<[u32; 3]>), upper: &(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)) -> f64 {
    let origin_z = lower.0.first().map_or(0.0, |vertex| vertex.z);
    fn height_integral((vertices, faces): &(Vec<mesh_data::Vertex>, Vec<[u32; 3]>), origin_z: f64) -> f64 {
        faces
            .iter()
            .map(|face| {
                let corners = face.map(|index| vertices[index as usize]);
                let area = ((corners[1].x - corners[0].x) * (corners[2].y - corners[0].y) - (corners[1].y - corners[0].y) * (corners[2].x - corners[0].x)).abs() / 2.0;
                area * ((corners[0].z - origin_z) + (corners[1].z - origin_z) + (corners[2].z - origin_z)) / 3.0
            })
            .sum()
    }
    height_integral(upper, origin_z) - height_integral(lower, origin_z)
}

/// How many edges of the mesh are not shared by exactly two faces, once
/// coincident vertices are welded.
///
/// A closed solid has none. Ones that do appear are the sides: where the
/// region ends because a surface simply runs out - a design that stops short
/// of the ground rather than meeting it - the floor and the roof end at
/// different heights with nothing between them. The volume is still exact
/// there, but the mesh is a shell rather than a solid, which is worth saying.
pub(crate) fn open_edge_count(vertices: &[mesh_data::Vertex], faces: &[[u32; 3]]) -> usize {
    use std::collections::HashMap;
    let scale = weld_scale(vertices);
    let key = |index: u32| point_key_scaled(vertices[index as usize], scale);
    let mut counts: HashMap<EdgeKey, usize> = HashMap::new();
    for face in faces {
        for (a, b) in [(face[0], face[1]), (face[1], face[2]), (face[2], face[0])] {
            let (a, b) = (key(a), key(b));
            *counts.entry(if a <= b { (a, b) } else { (b, a) }).or_default() += 1;
        }
    }
    counts.values().filter(|count| **count != 2).count()
}

impl App<'_> {
    /// Create a new closed solid from a design surface and the topography it
    /// meets, as the Triangulation menu's own tool. The build runs on a
    /// worker; the result arrives as a normal generated triangulation.
    pub(crate) fn create_solid_from_surfaces(&mut self, design_id: TriangulationId, topography_id: TriangulationId, region: SolidRegion, name: String) -> Result<()> {
        if design_id == topography_id {
            anyhow::bail!("A solid needs two different surfaces");
        }
        let (design_mesh, design_name) = {
            let design = self
                .triangulations
                .iter()
                .find(|triangulation| triangulation.id == design_id)
                .ok_or_else(|| anyhow::anyhow!("Design surface not found"))?;
            (design.mesh.clone(), design.name.clone())
        };
        let (topography_mesh, topography_name) = {
            let topography = self
                .triangulations
                .iter()
                .find(|triangulation| triangulation.id == topography_id)
                .ok_or_else(|| anyhow::anyhow!("Topography surface not found"))?;
            (topography.mesh.clone(), topography.name.clone())
        };

        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| -> Result<crate::model::triangulation::GeneratedTriangulationLog> {
            let (vertices, faces, volume) = build_solid_between_surfaces(&design_mesh, &topography_mesh, region, &progress.phase(0.0, 0.85))?;
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let generated = session::build_generated_triangulation(name, vertices, faces, TriSurfaceType::SolidClosed, crate::model::triangulation::unique_edges)?;
            Ok(crate::model::triangulation::GeneratedTriangulationLog {
                generated,
                message: tr_format!(
                    literal = "Built %region% solid between '%design%' and '%topography%' · %volume% m³",
                    region = region.label().to_lowercase(),
                    design = &design_name,
                    topography = &topography_name,
                    volume = format!("{volume:.1}")
                ),
            })
        };
        let apply = move |app: &mut App, result: Result<crate::model::triangulation::GeneratedTriangulationLog>| {
            app.apply_generated_triangulation_job(result);
        };
        self.spawn_job_reporting_progress(
            tr!(literal = "Building solid from surfaces…"),
            vec![crate::app::jobs::JobKey::Triangulation(design_id), crate::app::jobs::JobKey::Triangulation(topography_id)],
            compute,
            apply,
        );
        Ok(())
    }
}

/// One generated slab: its welded vertices, and the faces indexing them.
pub(crate) type Slab = (Vec<mesh_data::Vertex>, Vec<[u32; 3]>);

/// Cut closed slabs out of a solid between successive elevations - the benches
/// of a pit, or the flitches inside one.
///
/// A plain Z clip of a solid gives back only the *surfaces* that fall in the
/// band: for a pit that is a thin ring of wall, which reads as nothing at all
/// next to the lid at the top. A slab is that ring capped at both elevations,
/// so it is a solid in its own right - which is also what a bench or flitch
/// has to be to carry a tonnage later.
///
/// The caps are the solid's own cross-sections: the clip leaves an open rim
/// lying exactly on each plane, and those rims are traced into rings and
/// filled. A solid that is open at the sides (see the warning
/// [`build_solid_between_surfaces`] emits) has rims that do not close, and
/// those are left uncapped rather than guessed at.
///
/// Bands are expected in ascending order and must not overlap. A face only
/// touches the bands its own height range reaches, so cutting every bench of a
/// pit at once costs one walk of the mesh rather than one per bench.
pub(crate) fn slabs_between_elevations(mesh: &mesh_data::Triangulation, bands: &[(f64, f64)], cancel: Option<&crate::app::jobs::CancelFlag>) -> Result<Vec<Slab>> {
    if bands.iter().any(|(base, top)| !base.is_finite() || !top.is_finite() || base >= top) || bands.windows(2).any(|pair| pair[0].1 > pair[1].0 + 1e-6) {
        anyhow::bail!("Bench bands must be finite, ascending and non-overlapping");
    }
    let mut slabs: Vec<Slab> = vec![(Vec::new(), Vec::new()); bands.len()];
    let source = mesh.vertices();
    for (face_index, face) in mesh.face_vertex_indices_iter().enumerate() {
        if face_index % 1024 == 0 && cancel.is_some_and(|cancel| cancel.is_cancelled()) {
            anyhow::bail!("Cancelled");
        }
        let triangle = [source[face[0]], source[face[1]], source[face[2]]];
        let lowest = triangle[0].z.min(triangle[1].z).min(triangle[2].z);
        let highest = triangle[0].z.max(triangle[1].z).max(triangle[2].z);
        let first = bands.partition_point(|(_, top)| *top < lowest);
        for (index, (base, top)) in bands.iter().enumerate().skip(first).take_while(|(_, (base, _))| *base <= highest) {
            if top <= base || highest < *base || lowest > *top {
                continue;
            }
            // A horizontal boundary face belongs only to the slab on its
            // material side. Copying it into both bands creates a zero-thickness
            // sheet which cap triangulation can cover a second time.
            if highest - lowest < 1e-8 {
                let normal_z = (triangle[1].x - triangle[0].x) * (triangle[2].y - triangle[0].y) - (triangle[1].y - triangle[0].y) * (triangle[2].x - triangle[0].x);
                if ((lowest - base).abs() < 1e-8 && normal_z > 0.0) || ((highest - top).abs() < 1e-8 && normal_z < 0.0) {
                    continue;
                }
            }
            let slab = &mut slabs[index];
            for clipped in super::cuts::clip_triangle_z(triangle, *base, *top) {
                let [a, b, c] = clipped.map(|p| glam::DVec3::new(p.x, p.y, p.z));
                if (b - a).cross(c - a).length_squared() <= 1e-18 {
                    continue;
                }
                let vertex = slab.0.len() as u32;
                slab.0.extend_from_slice(&clipped);
                slab.1.push([vertex, vertex + 1, vertex + 2]);
            }
        }
    }
    for (slab, (base, top)) in slabs.iter_mut().zip(bands) {
        if slab.1.is_empty() {
            continue;
        }
        if cancel.is_some_and(|cancel| cancel.is_cancelled()) {
            anyhow::bail!("Cancelled");
        }
        cap_slab(slab, *base, *top);
    }
    Ok(slabs)
}

/// Fill the open rims a slab was cut at, turning the band of surface into a
/// solid.
fn cap_slab(slab: &mut (Vec<mesh_data::Vertex>, Vec<[u32; 3]>), base: f64, top: f64) {
    let weld = Weld::of(&slab.0);
    let rims = boundary_segments(&slab.0, &slab.1);
    for (plane, upwards) in [(base, false), (top, true)] {
        let rim: Vec<[mesh_data::Vertex; 2]> = rims
            .iter()
            .copied()
            .filter(|[a, b]| (a.z - plane).abs() <= weld.tolerance && (b.z - plane).abs() <= weld.tolerance)
            .collect();
        if rim.is_empty() {
            continue;
        }
        let rings = planar_cap_rings(&rim, weld);
        append_caps(&rings, plane, upwards, &mut slab.0, &mut slab.1);
    }
}

/// The edges of a triangle soup that only one face uses, welded by position.
fn boundary_segments(vertices: &[mesh_data::Vertex], faces: &[[u32; 3]]) -> Vec<[mesh_data::Vertex; 2]> {
    use std::collections::HashMap;
    let weld = Weld::of(vertices);
    let key = |vertex: mesh_data::Vertex| weld.key(vertex);
    let mut counts: HashMap<EdgeKey, (usize, [mesh_data::Vertex; 2])> = HashMap::new();
    for face in faces {
        let corners = face.map(|index| vertices[index as usize]);
        for (a, b) in [(corners[0], corners[1]), (corners[1], corners[2]), (corners[2], corners[0])] {
            let (ka, kb) = (key(a), key(b));
            let entry = counts.entry(if ka <= kb { (ka, kb) } else { (kb, ka) }).or_insert((0, [a, b]));
            entry.0 += 1;
        }
    }
    counts.into_values().filter(|(count, _)| *count == 1).map(|(_, segment)| segment).collect()
}

/// Trace the rings bounding a closed slab's plan footprint - every XY point
/// a vertical ray through the slab hits, flattened onto `plane`.
///
/// Only the upward-facing faces are used, and they are exactly the footprint:
/// every point with material below it has one topmost boundary above it, so
/// the roof covers the footprint once and only once. That holds whether the
/// roof is a horizontal cut cap or the topography itself, which is why this
/// works for the top bench of a pit, where there is no cap to trace, and why
/// it beats the cap elsewhere - ground under a dip in the topography never
/// reaches the band top but is still ground.
///
/// Vertical walls project to slivers and are dropped; their edges are already
/// shared with the roof and floor they join.
pub(crate) fn plan_footprint_rings(mesh: &mesh_data::Triangulation, plane: f64) -> Vec<Vec<mesh_data::Vertex>> {
    let source = mesh.vertices();
    if source.is_empty() {
        return Vec::new();
    }
    let sliver = Weld::of(source).tolerance.powi(2);
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    for face in mesh.face_vertex_indices_iter() {
        let [a, b, c] = face.map(|index| source[index]);
        // Counter-clockwise in XY is an outward normal pointing up.
        if (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x) <= sliver {
            continue;
        }
        let base = vertices.len() as u32;
        vertices.extend([a, b, c].map(|vertex| mesh_data::Vertex::new(vertex.x, vertex.y, plane)));
        faces.push([base, base + 1, base + 2]);
    }
    if faces.is_empty() {
        return Vec::new();
    }
    // Flattening first makes the shared 3D weld an XY weld, so the roof's
    // interior edges cancel and only its outline is left.
    let weld = Weld::of(&vertices);
    closed_boundary_rings(&boundary_segments(&vertices, &faces), weld)
}

/// How many other rings each ring sits inside. Even depth is solid ground,
/// odd is a hole through it - the same even-odd rule the caps are filled by.
pub(crate) fn ring_depths(rings: &[Vec<mesh_data::Vertex>]) -> Vec<usize> {
    rings
        .iter()
        .enumerate()
        .map(|(index, ring)| {
            rings
                .iter()
                .enumerate()
                .filter(|(other, candidate)| *other != index && !ring.is_empty() && ring_contains(candidate, ring[0]))
                .count()
        })
        .collect()
}

/// Even-odd crossing test in XY, shared by the cap filler and the blast
/// outlines so both agree on what is inside a ring.
pub(crate) fn ring_contains(ring: &[mesh_data::Vertex], point: mesh_data::Vertex) -> bool {
    let mut inside = false;
    for (a, b) in ring.iter().zip(ring.iter().cycle().skip(1)).take(ring.len()) {
        if (a.y > point.y) != (b.y > point.y) && point.x < a.x + (point.y - a.y) * (b.x - a.x) / (b.y - a.y) {
            inside = !inside;
        }
    }
    inside
}

/// Fill even-depth rings with their immediate holes. Filling every ring
/// independently would seal voids and overstate slab volumes.
fn append_caps(rings: &[Vec<mesh_data::Vertex>], plane: f64, upwards: bool, vertices: &mut Vec<mesh_data::Vertex>, faces: &mut Vec<[u32; 3]>) {
    let contains = ring_contains;
    let depths = ring_depths(rings);
    for (index, outer) in rings.iter().enumerate() {
        if outer.len() < 3 || !depths[index].is_multiple_of(2) {
            continue;
        }
        let mut points = outer.clone();
        let mut holes = Vec::new();
        for (j, hole) in rings.iter().enumerate() {
            if !hole.is_empty() && depths[j] == depths[index] + 1 && contains(outer, hole[0]) {
                holes.push(points.len());
                points.extend(hole);
            }
        }
        let mut indices = Vec::new();
        earcut::Earcut::new().earcut(points.iter().map(|point| [point.x, point.y]), &holes, &mut indices);
        let base = vertices.len() as u32;
        vertices.extend(points.iter().map(|point| mesh_data::Vertex { x: point.x, y: point.y, z: plane }));
        for triangle in indices.as_chunks::<3>().0 {
            let face = [base + triangle[0] as u32, base + triangle[1] as u32, base + triangle[2] as u32];
            let corners = face.map(|index| vertices[index as usize]);
            let points_up = (corners[1].x - corners[0].x) * (corners[2].y - corners[0].y) - (corners[1].y - corners[0].y) * (corners[2].x - corners[0].x) > 0.0;
            faces.push(if points_up == upwards { face } else { [face[0], face[2], face[1]] });
        }
    }
}

/// Close the sides of the region with a vertical wall between the two sheets.
///
/// The floor and roof meet on their own along the line where the surfaces
/// cross - both clips stop there, at the same heights - but nowhere else. Where
/// the region ends because a surface simply runs out inside the other's
/// footprint, the two sheets end one above the other with a gap between them,
/// and that gap is what this fills.
///
/// Both sheets cover the same XY region, so their rims trace the same closed
/// path with different tessellations. The wall is built on the merge of the
/// two: every rim vertex of either sheet becomes a wall vertex, so the wall's
/// edges line up with the sheet edges on both sides and the result is closed
/// rather than merely gapless.
fn close_region_sides(floor: &(Vec<mesh_data::Vertex>, Vec<[u32; 3]>), roof: &(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)) -> (Vec<mesh_data::Vertex>, Vec<[u32; 3]>) {
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    // One weld for both sheets: they are welded against each other, so they
    // cannot each pick their own grid from their own extent.
    let weld = Weld::of(&floor.0);
    let floor_rings = boundary_rings(floor, weld);
    let mut roof_rings = boundary_rings(roof, weld);
    if floor_rings.is_empty() || roof_rings.is_empty() {
        return (vertices, faces);
    }
    let outward = outward_lookup(floor, weld);

    for floor_ring in &floor_rings {
        // The rims trace the same path, so the roof ring that belongs with
        // this one is the one that runs closest to it.
        let Some(index) = nearest_ring(floor_ring, &roof_rings) else {
            continue;
        };
        let roof_ring = roof_rings.remove(index);
        append_wall(floor_ring, &roof_ring, &outward, weld, &mut vertices, &mut faces);
    }
    (vertices, faces)
}

/// The rim of a sheet, as closed rings of points.
fn boundary_rings(sheet: &(Vec<mesh_data::Vertex>, Vec<[u32; 3]>), weld: Weld) -> Vec<Vec<mesh_data::Vertex>> {
    let segments = boundary_segments(&sheet.0, &sheet.1);
    if segments.is_empty() {
        return Vec::new();
    }
    let mut rings = closed_boundary_rings(&segments, weld);
    for ring in &mut rings {
        // A ring that repeats its first point closes itself; the wall walks
        // the loop and would emit a zero-width quad on that repeat.
        if ring.len() > 1 && weld.same_xy(ring[0], ring[ring.len() - 1]) {
            ring.pop();
        }
    }
    rings.retain(|ring| ring.len() >= 3);
    rings
}

/// Six times the signed volume a triangle soup encloses, positive when its
/// faces wind outwards. Rebased, so mine coordinates keep their precision.
fn signed_volume(vertices: &[mesh_data::Vertex], faces: &[[u32; 3]]) -> f64 {
    let Some(origin) = vertices.first() else {
        return 0.0;
    };
    faces
        .iter()
        .map(|face| {
            let [a, b, c] = face.map(|index| {
                let vertex = vertices[index as usize];
                glam::DVec3::new(vertex.x - origin.x, vertex.y - origin.y, vertex.z - origin.z)
            });
            a.dot(b.cross(c))
        })
        .sum()
}

/// Trace the rings bounding a planar rim, getting past nodes where more than
/// two rim edges meet.
///
/// [`closed_boundary_rings`] abandons a ring at any such node, which is right
/// when building a solid - bridging an ambiguous rim would invent geometry -
/// but wrong for capping a cut. A vertical cut through a bench slab reaches
/// those nodes routinely: the cross-section pinches to a point, two pieces
/// touch at a corner, or independently clipped triangles leave a T-junction.
/// Giving up there left the cell uncapped, so it stayed open and its volume
/// leaked out of the total.
///
/// Turning the sharpest way available at every node walks the faces of the
/// planar subdivision the rim draws. Each rim ring is then walked once in each
/// direction, so keeping the positive-turning walks yields every ring exactly
/// once - outer rings and hole rings alike, which is what [`append_caps`] wants
/// before sorting them by nesting.
fn planar_cap_rings(segments: &[[mesh_data::Vertex; 2]], weld: Weld) -> Vec<Vec<mesh_data::Vertex>> {
    use std::collections::{HashMap, HashSet};

    let mut points: HashMap<PointKey, mesh_data::Vertex> = HashMap::new();
    let mut adjacency: HashMap<PointKey, Vec<PointKey>> = HashMap::new();
    for [a, b] in segments {
        let (ka, kb) = (weld.key(*a), weld.key(*b));
        if ka == kb {
            continue;
        }
        points.insert(ka, *a);
        points.insert(kb, *b);
        for (from, to) in [(ka, kb), (kb, ka)] {
            let neighbours = adjacency.entry(from).or_default();
            // A rim edge repeated by two faces is one edge of the outline.
            if !neighbours.contains(&to) {
                neighbours.push(to);
            }
        }
    }

    let angle = |from: PointKey, to: PointKey| {
        let (origin, target) = (points[&from], points[&to]);
        (target.y - origin.y).atan2(target.x - origin.x)
    };
    for (key, neighbours) in &mut adjacency {
        neighbours.sort_by(|a, b| angle(*key, *a).total_cmp(&angle(*key, *b)));
    }

    let mut directed: Vec<(PointKey, PointKey)> = adjacency.iter().flat_map(|(from, tos)| tos.iter().map(|to| (*from, *to))).collect();
    directed.sort_unstable();

    let mut visited: HashSet<(PointKey, PointKey)> = HashSet::new();
    let mut rings = Vec::new();
    for start in directed {
        if visited.contains(&start) {
            continue;
        }
        let mut ring: Vec<mesh_data::Vertex> = Vec::new();
        let mut edge = start;
        while visited.insert(edge) {
            let (from, to) = edge;
            ring.push(points[&from]);
            let neighbours = &adjacency[&to];
            // Step back the way we came, then take the next neighbour turning
            // one way round. At a plain degree-two node that is simply the
            // other edge, so a clean rim traces exactly as it did before.
            let incoming = neighbours.iter().position(|neighbour| *neighbour == from).expect("edges are added both ways");
            edge = (to, neighbours[(incoming + neighbours.len() - 1) % neighbours.len()]);
        }
        if ring.len() >= 3 && ring_signed_area(&ring) > 0.0 {
            rings.push(ring);
        }
    }
    rings
}

/// Twice the signed area of a ring in the plane it was traced in.
fn ring_signed_area(ring: &[mesh_data::Vertex]) -> f64 {
    (0..ring.len())
        .map(|index| {
            let (a, b) = (ring[index], ring[(index + 1) % ring.len()]);
            a.x * b.y - b.x * a.y
        })
        .sum()
}

/// Trace only closed, unambiguous loops. The general include tool bridges
/// open contours with chords; manufacturing those edges is unsafe for solids.
fn closed_boundary_rings(segments: &[[mesh_data::Vertex; 2]], weld: Weld) -> Vec<Vec<mesh_data::Vertex>> {
    use std::collections::{HashMap, HashSet};
    let mut nodes: HashMap<PointKey, (mesh_data::Vertex, Vec<PointKey>)> = HashMap::new();
    for [a, b] in segments {
        let (ka, kb) = (weld.key(*a), weld.key(*b));
        if ka == kb {
            continue;
        }
        nodes.entry(ka).or_insert((*a, Vec::new())).1.push(kb);
        nodes.entry(kb).or_insert((*b, Vec::new())).1.push(ka);
    }
    let mut starts: Vec<_> = nodes.keys().copied().collect();
    starts.sort_unstable();
    let mut visited = HashSet::new();
    let mut rings = Vec::new();
    for start in starts {
        if visited.contains(&start) {
            continue;
        }
        let mut ring = Vec::new();
        let mut previous = None;
        let mut current = start;
        loop {
            if !visited.insert(current) {
                if current == start && ring.len() >= 3 {
                    rings.push(ring);
                }
                break;
            }
            let (point, neighbors) = &nodes[&current];
            if neighbors.len() != 2 {
                break;
            }
            ring.push(*point);
            let next = if Some(neighbors[0]) == previous { neighbors[1] } else { neighbors[0] };
            previous = Some(current);
            current = next;
        }
    }
    rings
}

/// For each rim edge of a sheet, which horizontal side of it the sheet lies
/// on - keyed by the edge's welded endpoints.
///
/// The face that owns a rim edge has a third corner, and the sheet is on that
/// corner's side. That is what tells the wall which way is out, without having
/// to work out which rings are outer boundaries and which are holes.
fn outward_lookup(sheet: &(Vec<mesh_data::Vertex>, Vec<[u32; 3]>), weld: Weld) -> std::collections::HashMap<EdgeKey, bool> {
    use std::collections::HashMap;
    let point_key = |vertex| weld.key(vertex);
    let mut counts: HashMap<EdgeKey, (usize, bool)> = HashMap::new();
    for face in &sheet.1 {
        let corners = face.map(|index| sheet.0[index as usize]);
        for (a, b, c) in [
            (corners[0], corners[1], corners[2]),
            (corners[1], corners[2], corners[0]),
            (corners[2], corners[0], corners[1]),
        ] {
            let (ka, kb) = (point_key(a), point_key(b));
            // Keyed on the sorted pair, with the side recorded in the
            // direction the key is stored, so a lookup can undo the sort.
            let sorted = ka <= kb;
            let (first, second) = if sorted { (a, b) } else { (b, a) };
            let entry = counts.entry(if sorted { (ka, kb) } else { (kb, ka) }).or_insert((0, false));
            entry.0 += 1;
            entry.1 = left_of(first, second, c);
        }
    }
    counts.into_iter().filter(|(_, (count, _))| *count == 1).map(|(key, (_, left))| (key, left)).collect()
}

/// Whether `point` lies to the left of the line `from` -> `to`, in XY.
fn left_of(from: mesh_data::Vertex, to: mesh_data::Vertex, point: mesh_data::Vertex) -> bool {
    (to.x - from.x) * (point.y - from.y) - (to.y - from.y) * (point.x - from.x) > 0.0
}

type PointKey = (i64, i64, i64);

/// An edge, keyed by its two welded endpoints in sorted order.
type EdgeKey = (PointKey, PointKey);

/// Grid a set of points is welded on, in units per metre.
///
/// Account for a small number of f64 rounding steps without scaling tolerance
/// to centimetres at mine coordinates. Intersection construction is rebased
/// before clipping; this weld only absorbs residual roundoff.
fn weld_scale(vertices: &[mesh_data::Vertex]) -> f64 {
    let magnitude = vertices.iter().map(|vertex| vertex.x.abs().max(vertex.y.abs()).max(vertex.z.abs())).fold(0.0_f64, f64::max);
    // At least one micrometre, otherwise 64 machine epsilons at this magnitude.
    (1.0 / (magnitude * f64::EPSILON * 64.0).max(1e-6)).min(1e6)
}

/// The grid one solid's points are welded on, and the distance below which two
/// of them are the same point. Computed once per solid and carried, so every
/// step - the rim trace, the ring walk, the degenerate test - agrees about
/// which points coincide.
#[derive(Clone, Copy)]
struct Weld {
    scale: f64,
    tolerance: f64,
}

impl Weld {
    fn of(vertices: &[mesh_data::Vertex]) -> Self {
        let scale = weld_scale(vertices);
        Self { scale, tolerance: 1.0 / scale }
    }

    fn key(self, vertex: mesh_data::Vertex) -> PointKey {
        point_key_scaled(vertex, self.scale)
    }

    fn same_xy(self, a: mesh_data::Vertex, b: mesh_data::Vertex) -> bool {
        (a.x - b.x).abs() <= self.tolerance && (a.y - b.y).abs() <= self.tolerance
    }

    fn same(self, a: mesh_data::Vertex, b: mesh_data::Vertex) -> bool {
        self.same_xy(a, b) && (a.z - b.z).abs() <= self.tolerance
    }
}

fn point_key_scaled(vertex: mesh_data::Vertex, scale: f64) -> PointKey {
    ((vertex.x * scale).round() as i64, (vertex.y * scale).round() as i64, (vertex.z * scale).round() as i64)
}

/// Match XY footprints rather than centroids: concentric outer and hole
/// rings have the same centroid, but must never be joined to one another.
fn nearest_ring(ring: &[mesh_data::Vertex], candidates: &[Vec<mesh_data::Vertex>]) -> Option<usize> {
    let bounds = |ring: &[mesh_data::Vertex]| {
        ring.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, p| {
            [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)]
        })
    };
    let target = bounds(ring);
    let tolerance = Weld::of(ring).tolerance * 4.0;
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, ring)| {
            let candidate = bounds(ring);
            let error = target.iter().zip(candidate).map(|(a, b)| (a - b).abs()).fold(0.0_f64, f64::max);
            (error <= tolerance).then_some((index, error))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// A closed ring parameterised by its XY arc length, so two tessellations of
/// the same path can be sampled against each other.
struct RingPath {
    points: Vec<mesh_data::Vertex>,
    /// Cumulative XY length at each point, normalised to `0.0..1.0`.
    params: Vec<f64>,
}

impl RingPath {
    fn new(points: &[mesh_data::Vertex]) -> Option<Self> {
        let mut lengths = Vec::with_capacity(points.len() + 1);
        let mut total = 0.0;
        lengths.push(0.0);
        for index in 0..points.len() {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            total += ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
            lengths.push(total);
        }
        (total > 1e-9).then(|| Self {
            points: points.to_vec(),
            params: lengths.iter().map(|length| length / total).collect(),
        })
    }

    /// Signed XY area, whose sign is the direction the ring is traced in.
    fn signed_area(&self) -> f64 {
        let mut sum = 0.0;
        let origin = self.points[0];
        for index in 0..self.points.len() {
            let a = self.points[index];
            let b = self.points[(index + 1) % self.points.len()];
            sum += (a.x - origin.x) * (b.y - origin.y) - (b.x - origin.x) * (a.y - origin.y);
        }
        sum / 2.0
    }

    fn reverse(&mut self) {
        self.points.reverse();
        *self = Self::new(&self.points).unwrap_or_else(|| {
            std::mem::replace(
                self,
                Self {
                    points: Vec::new(),
                    params: Vec::new(),
                },
            )
        });
    }

    /// The parameter at which the ring passes closest to `target` in XY.
    fn nearest_param(&self, target: mesh_data::Vertex) -> f64 {
        let mut best = (f64::MAX, 0.0);
        for index in 0..self.points.len() {
            let a = self.points[index];
            let b = self.points[(index + 1) % self.points.len()];
            let (ax, ay) = (b.x - a.x, b.y - a.y);
            let length_sq = ax * ax + ay * ay;
            let t = if length_sq > 0.0 {
                (((target.x - a.x) * ax + (target.y - a.y) * ay) / length_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let distance = (target.x - (a.x + ax * t)).powi(2) + (target.y - (a.y + ay * t)).powi(2);
            if distance < best.0 {
                let span = self.params[index + 1] - self.params[index];
                best = (distance, self.params[index] + span * t);
            }
        }
        best.1
    }

    /// The point at a parameter, interpolating along the ring.
    fn sample(&self, param: f64) -> mesh_data::Vertex {
        let param = param.rem_euclid(1.0);
        let index = self.params.partition_point(|value| *value <= param).saturating_sub(1).min(self.points.len() - 1);
        let a = self.points[index];
        let b = self.points[(index + 1) % self.points.len()];
        let span = self.params[index + 1] - self.params[index];
        let t = if span > 0.0 { ((param - self.params[index]) / span).clamp(0.0, 1.0) } else { 0.0 };
        mesh_data::Vertex {
            x: a.x + (b.x - a.x) * t,
            y: a.y + (b.y - a.y) * t,
            z: a.z + (b.z - a.z) * t,
        }
    }
}

/// Build the wall between one floor rim and the roof rim that matches it.
fn append_wall(
    floor_ring: &[mesh_data::Vertex],
    roof_ring: &[mesh_data::Vertex],
    outward: &std::collections::HashMap<EdgeKey, bool>,
    weld: Weld,
    vertices: &mut Vec<mesh_data::Vertex>,
    faces: &mut Vec<[u32; 3]>,
) {
    let (Some(floor), Some(mut roof)) = (RingPath::new(floor_ring), RingPath::new(roof_ring)) else {
        return;
    };
    // Walk both rims the same way round, from the same place, so the two
    // parameterisations describe the same journey.
    if floor.signed_area().is_sign_negative() != roof.signed_area().is_sign_negative() {
        roof.reverse();
    }
    let roof_offset = roof.nearest_param(floor.points[0]);

    // Which side of the rim the sheet lies on decides which way the wall
    // faces. It is the same all the way round a ring, so one edge answers it.
    let sheet_on_left = floor
        .points
        .iter()
        .enumerate()
        .find_map(|(index, point)| {
            let next = floor.points[(index + 1) % floor.points.len()];
            let (ka, kb) = (weld.key(*point), weld.key(next));
            let sorted = ka <= kb;
            let left = *outward.get(&if sorted { (ka, kb) } else { (kb, ka) })?;
            // The lookup recorded the side relative to the sorted direction,
            // so an edge walked the other way has the sheet on the other side.
            Some(if sorted { left } else { !left })
        })
        .unwrap_or(true);

    // Every rim vertex of either sheet becomes a wall vertex, so the wall's
    // edges match the sheet edges on both sides.
    let mut params: Vec<f64> = floor.params[..floor.points.len()].to_vec();
    // A roof vertex sits at its own parameter less the offset, since the roof
    // is walked from wherever it passes closest to the floor's own start.
    params.extend(roof.params[..roof.points.len()].iter().map(|param| (param - roof_offset).rem_euclid(1.0)));
    params.sort_by(f64::total_cmp);
    params.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    if params.len() < 2 {
        return;
    }

    let base = vertices.len() as u32;
    for param in &params {
        vertices.push(floor.sample(*param));
        vertices.push(roof.sample(param + roof_offset));
    }
    for index in 0..params.len() {
        let next = (index + 1) % params.len();
        let (floor_a, roof_a) = (base + index as u32 * 2, base + index as u32 * 2 + 1);
        let (floor_b, roof_b) = (base + next as u32 * 2, base + next as u32 * 2 + 1);
        // Where the two sheets already meet - along the line the surfaces
        // cross - there is no wall to build.
        let closed = weld.same(vertices[floor_a as usize], vertices[roof_a as usize]) && weld.same(vertices[floor_b as usize], vertices[roof_b as usize]);
        if closed {
            continue;
        }
        // Wound floor-then-roof along the direction of travel, a quad's
        // normal points to the right of it. That is out of the solid when the
        // sheet is on the left, and into it when the sheet is on the right.
        for face in [[floor_a, floor_b, roof_b], [floor_a, roof_b, roof_a]] {
            // Where the wall runs out to nothing - a corner at which the two
            // sheets meet - the quad degenerates into a sliver with two
            // corners in the same place. It has no area to contribute and its
            // repeated edge would read as a hole, so it is not emitted.
            let corners = face.map(|index| vertices[index as usize]);
            if weld.same(corners[0], corners[1]) || weld.same(corners[1], corners[2]) || weld.same(corners[2], corners[0]) {
                continue;
            }
            faces.push(if sheet_on_left { face } else { [face[0], face[2], face[1]] });
        }
    }
}

/// What clipping a solid to a plan polygon produced.
pub(crate) struct ClippedSolid {
    /// Vertices and faces of the clipped body.
    pub(crate) slab: Slab,
    /// The volume it encloses, summed per cell while cutting.
    pub(crate) volume: f64,
    /// Which faces are wall the clip cut along the polygon's own boundary,
    /// rather than the body's original surfaces or an internal wall between
    /// decomposition cells. Parallel to `slab.1`.
    pub(crate) boundary_wall: Vec<bool>,
}

/// Intersect a slab with a vertical polygon (including holes).
///
/// Earcut partitions its plan into disjoint convex cells. Each cell is clipped
/// and capped independently; shared vertical walls cancel in signed volume
/// integration and have zero contribution to block-overlap columns.
///
/// The result is a soup of those cells, and is deliberately *not* edge-manifold:
/// cells meeting at an internal wall triangulate it independently, so
/// [`open_edge_count`] over the result always reports it open even when every
/// cell is sound. It is no use as a trust signal here - the caller should judge
/// the volume on whether the body going *in* was closed, which is the only
/// thing that decides it, since a closed body clips to an exact one.
///
/// An open body is clipped and returned rather than rejected: bench bodies are
/// allowed to be open (`build_solid_between_surfaces` warns rather than
/// failing, and the View page reports it through `planning-reserve-open-solid`).
/// Failing here would take the whole View render down for a condition the page
/// already knows how to show.
pub(crate) fn clip_solid_to_plan(mesh: &mesh_data::Triangulation, face: &[Vec<glam::DVec2>], cancel: &crate::app::jobs::CancelFlag) -> Result<ClippedSolid> {
    let mut points = Vec::new();
    let mut holes = Vec::new();
    for (i, ring) in face.iter().enumerate() {
        if i > 0 {
            holes.push(points.len());
        }
        points.extend_from_slice(ring);
    }
    let mut triangles = Vec::new();
    earcut::Earcut::new().earcut(points.iter().map(|p| [p.x, p.y]), &holes, &mut triangles);
    let original: Slab = (mesh.vertices().to_vec(), mesh.face_vertex_indices_iter().map(|f| f.map(|i| i as u32)).collect());
    let mut result: Slab = (Vec::new(), Vec::new());
    let mut boundary_wall = Vec::new();
    let mut volume = 0.0;
    for triangle in triangles.as_chunks::<3>().0 {
        anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
        let mut cell = [points[triangle[0]], points[triangle[1]], points[triangle[2]]];
        if (cell[1] - cell[0]).perp_dot(cell[2] - cell[0]) < 0.0 {
            cell.swap(1, 2);
        }
        // Only the cell edges that run along the polygon's own boundary cut a
        // wall that bounds the result. The rest are the decomposition's
        // internal diagonals, whose walls are shared with the cell next door
        // and are inside the body, not on it.
        let cell_on_boundary: [bool; 3] = std::array::from_fn(|i| segment_follows(face, cell[i], cell[(i + 1) % 3]));
        let mut slab = original.clone();
        // The body's own surfaces are not walls the clip made.
        let mut wall = vec![false; slab.1.len()];
        for i in 0..3 {
            if slab.1.is_empty() {
                break;
            }
            let edge = (cell[(i + 1) % 3] - cell[i]).normalize();
            let tangent = glam::DVec3::new(edge.x, edge.y, 0.0);
            let normal = glam::DVec3::new(-edge.y, edge.x, 0.0);
            let origin = glam::DVec3::new(cell[i].x, cell[i].y, mesh.bounds().min.z);
            let local: Vec<_> = slab
                .0
                .iter()
                .map(|p| {
                    let delta = glam::DVec3::new(p.x, p.y, p.z) - origin;
                    let distance = delta.dot(normal);
                    mesh_data::Vertex::new(delta.z, delta.dot(tangent), if distance.abs() < 1e-8 { 0.0 } else { distance })
                })
                .collect();
            let high = local.iter().map(|p| p.z).fold(0.0, f64::max) + 1.0;
            let mut clipped: Slab = (Vec::new(), Vec::new());
            let mut clipped_wall = Vec::new();
            for (index, indices) in slab.1.iter().enumerate() {
                if index.is_multiple_of(1024) {
                    anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
                }
                let triangle = indices.map(|index| local[index as usize]);
                // A coplanar face with material on the discarded side is not
                // a zero-thickness part of the retained volume.
                if triangle.iter().all(|p| p.z == 0.0)
                    && (triangle[1].x - triangle[0].x) * (triangle[2].y - triangle[0].y) - (triangle[1].y - triangle[0].y) * (triangle[2].x - triangle[0].x) > 0.0
                {
                    continue;
                }
                for tri in super::cuts::clip_triangle_z(triangle, 0.0, high) {
                    let v = tri.map(|p| glam::DVec3::new(p.x, p.y, p.z));
                    if (v[1] - v[0]).cross(v[2] - v[0]).length_squared() <= 1e-18 {
                        continue;
                    }
                    let start = clipped.0.len() as u32;
                    clipped.0.extend(tri);
                    clipped.1.push([start, start + 1, start + 2]);
                    clipped_wall.push(wall[index]);
                }
            }
            if !clipped.1.is_empty() {
                cap_slab(&mut clipped, 0.0, high);
            }
            // Everything `cap_slab` just appended closes this cut plane, so it
            // is wall - and bounds the body only where this edge does.
            clipped_wall.resize(clipped.1.len(), cell_on_boundary[i]);
            for p in &mut clipped.0 {
                let world = origin + glam::DVec3::Z * p.x + tangent * p.y + normal * p.z;
                *p = mesh_data::Vertex::new(world.x, world.y, world.z);
            }
            slab = clipped;
            wall = clipped_wall;
        }
        if slab.1.is_empty() {
            continue;
        }
        // Each cell's own volume, summed here rather than measured off the
        // merged soup afterwards. Cells are not individually edge-manifold -
        // clipping leaves T-junctions - so a divergence sum taken over the
        // merge, from one shared origin, does not recover the total. Taken per
        // cell against the cell's own origin it is exact.
        let signed = signed_volume(&slab.0, &slab.1);
        volume += signed.abs() / 6.0;
        // Volume is taken per cell as a magnitude, but the block-model overlap
        // integrates signed prisms over floor and roof from one shared origin,
        // so it needs every cell wound the same way. No cell in the test fixture
        // comes out inverted, and none should - this guards the invariant the
        // overlap relies on rather than fixing an observed fault.
        if signed < 0.0 {
            for face in &mut slab.1 {
                face.swap(1, 2);
            }
        }
        boundary_wall.extend(wall);
        let offset = result.0.len() as u32;
        result.0.extend(slab.0);
        result.1.extend(slab.1.into_iter().map(|face| face.map(|index| index + offset)));
    }
    debug_assert_eq!(boundary_wall.len(), result.1.len());
    Ok(ClippedSolid {
        slab: result,
        volume,
        boundary_wall,
    })
}

/// Whether the segment `a`-`b` runs along one of `rings` rather than cutting
/// across the ground they enclose.
///
/// The midpoint decides it: a cell edge either lies on a ring, having been
/// taken from it, or is a diagonal whose middle is clear of every ring.
fn segment_follows(rings: &[Vec<glam::DVec2>], a: glam::DVec2, b: glam::DVec2) -> bool {
    const TOLERANCE: f64 = 1.0e-3;

    let midpoint = a.midpoint(b);
    rings.iter().any(|ring| {
        ring.len() >= 2
            && (0..ring.len()).any(|index| {
                let (start, end) = (ring[index], ring[(index + 1) % ring.len()]);
                let span = end - start;
                let length_squared = span.length_squared();
                let closest = if length_squared < 1e-18 {
                    start
                } else {
                    start + span * ((midpoint - start).dot(span) / length_squared).clamp(0.0, 1.0)
                };
                closest.distance_squared(midpoint) <= TOLERANCE * TOLERANCE
            })
    })
}

/// The rim of the walls the clip cut: the outline separating this piece from
/// the one beside it.
///
/// A wall's own triangulation, and the joins where two cells' walls meet in the
/// same plane, are used by two wall faces and drop out; the top and bottom rims
/// and the wall's ends are used once and remain. Welded by position, because a
/// clipped body's faces do not share vertex indices.
pub(crate) fn boundary_wall_outline(slab: &Slab, boundary_wall: &[bool]) -> Vec<[u32; 2]> {
    use std::collections::HashMap;

    let (vertices, faces) = slab;
    let weld = Weld::of(vertices);
    let mut counts: HashMap<EdgeKey, (usize, [u32; 2])> = HashMap::new();
    for (face, _) in faces.iter().zip(boundary_wall).filter(|(_, wall)| **wall) {
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let (ia, ib) = (face[a], face[b]);
            let (ka, kb) = (weld.key(vertices[ia as usize]), weld.key(vertices[ib as usize]));
            if ka == kb {
                continue;
            }
            let entry = counts.entry(if ka <= kb { (ka, kb) } else { (kb, ka) }).or_insert((0, [ia, ib]));
            entry.0 += 1;
        }
    }
    counts.into_values().filter(|(count, _)| *count == 1).map(|(_, edge)| edge).collect()
}
