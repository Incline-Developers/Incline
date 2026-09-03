use super::{
    cuts::{
        SurfaceClipVertex, append_surface_clip_polyline, bary_z, clip_surface_polyline, clip_target_triangle_to_reference_xy, point_in_triangle_bary_z,
        reference_xy_overlap_area_tolerance, triangle_area_3d, triangle_xy_bounds,
    },
    *,
};
use crate::model::geometry::{deduplicate_ring_by, signed_area_xy, triangle_xy_area};

impl<'a> App<'a> {
    /// Create a new topology by replacing the area covered by a pit or stockpile
    /// solid with that solid's mesh.
    ///
    /// The heavy include (clip + BVH build) runs on a background thread so the
    /// UI stays interactive; the result is applied on the next frame.
    pub(crate) fn include_solid_in_topology(&mut self, topology_id: TriangulationId, shape_id: TriangulationId, name: String, save_as_two: bool, hide_old: bool) -> Result<()> {
        if topology_id == shape_id {
            anyhow::bail!("Topology and pit/stockpile solid must be different triangulations");
        }

        // Snapshot the (cheap-to-clone) inputs on the UI thread; the worker
        // must never touch `self`.
        let topology = self
            .triangulations
            .iter()
            .find(|triangulation| triangulation.id == topology_id)
            .ok_or_else(|| anyhow::anyhow!("Topology triangulation not found"))?;
        let shape = self
            .triangulations
            .iter()
            .find(|triangulation| triangulation.id == shape_id)
            .ok_or_else(|| anyhow::anyhow!("Pit/stockpile solid triangulation not found"))?;

        let topology_mesh = topology.mesh.clone();
        let topology_spatial = topology.spatial.clone();
        let shape_mesh = shape.mesh.clone();
        let topology_name = topology.name.clone();
        let shape_name = shape.name.clone();

        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| -> Result<IncludeJobOutput> {
            // The merge itself is the bulk of the work; the mesh and BVH builds
            // after it close out the bar.
            let included = include_shape_mesh_in_topology_with_spatial(&topology_mesh, &topology_spatial, &shape_mesh, &progress.phase(0.0, 0.85))?;
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            if included.faces.is_empty() {
                anyhow::bail!("Combined triangulation produced no faces");
            }
            let IncludedShapeMesh {
                mut vertices,
                mut faces,
                topology_vertex_count,
                topology_face_count,
                retained_topology_faces: retained,
                skipped_cap_faces: skipped,
            } = included;

            // Build the mesh(es), BVH(s), and edge lists in the worker too -
            // otherwise the freeze just moves to the apply step.
            let generated = if save_as_two {
                let shape_vertices = vertices.split_off(topology_vertex_count);
                let mut shape_faces = faces.split_off(topology_face_count);
                let vertex_offset = topology_vertex_count as u32;
                for face in &mut shape_faces {
                    for index in face {
                        *index -= vertex_offset;
                    }
                }

                let topology_edges = triangle_edge_list(&faces);
                let topology = session::build_generated_triangulation(component_name(&name, "Topology"), vertices, faces, TriSurfaceType::Surface, |_| topology_edges)?;
                let shape_edges = triangle_edge_list(&shape_faces);
                let shape = session::build_generated_triangulation(component_name(&name, "Shell"), shape_vertices, shape_faces, TriSurfaceType::Surface, |_| shape_edges)?;
                vec![topology, shape]
            } else {
                let edges = triangle_edge_list(&faces);
                vec![session::build_generated_triangulation(name, vertices, faces, TriSurfaceType::Surface, |_| edges)?]
            };
            Ok(IncludeJobOutput {
                generated,
                topology_name,
                shape_name,
                retained,
                skipped,
            })
        };

        let apply = move |app: &mut App, result: Result<IncludeJobOutput>| match result {
            Ok(output) => {
                userspace_log!(
                    "{}",
                    tr_format!(
                        literal = "Included solid '%shape_name%' in topology '%topology_name%' (retained %retained% topology faces, skipped %skipped% closure-cap faces)",
                        shape_name = output.shape_name,
                        topology_name = output.topology_name,
                        retained = output.retained,
                        skipped = output.skipped
                    )
                );
                for generated in output.generated {
                    app.insert_generated_triangulation(generated);
                }
                // The job has finished, so unloading its source dependencies no
                // longer risks cancelling it.
                if hide_old {
                    app.close_triangulation(topology_id);
                    app.close_triangulation(shape_id);
                }
            }
            Err(err) => {
                let message = format!("{err:#}");
                userspace_warn!("{}", tr_format!(literal = "Include failed: %message%", message = message));
            }
        };

        // Both source triangulations are dependencies: closing or replacing
        // either one must cancel the include, not just the topology.
        self.spawn_job_reporting_progress(
            crate::i18n::tr!(literal = "Including pit/stockpile solid…"),
            vec![crate::app::jobs::JobKey::Triangulation(topology_id), crate::app::jobs::JobKey::Triangulation(shape_id)],
            compute,
            apply,
        );
        Ok(())
    }
}

/// Worker output for a background include job: the built triangulation plus the
/// data needed for the completion log message.
struct IncludeJobOutput {
    generated: Vec<crate::model::triangulation::GeneratedTriangulation>,
    topology_name: String,
    shape_name: String,
    retained: usize,
    skipped: usize,
}

pub(super) struct IncludedShapeMesh {
    pub(super) vertices: Vec<mesh_data::Vertex>,
    pub(super) faces: Vec<[u32; 3]>,
    /// Number of leading vertices/faces belonging to the cut topology. The
    /// remaining mesh is the clipped/extended pit or stockpile surface.
    pub(super) topology_vertex_count: usize,
    pub(super) topology_face_count: usize,
    pub(super) retained_topology_faces: usize,
    pub(super) skipped_cap_faces: usize,
}

fn triangle_edge_list(faces: &[[u32; 3]]) -> Vec<[u32; 2]> {
    let mut edges = Vec::with_capacity(faces.len().saturating_mul(3));
    for [a, b, c] in faces.iter().copied() {
        edges.push(sorted_u32_edge(a, b));
        edges.push(sorted_u32_edge(b, c));
        edges.push(sorted_u32_edge(c, a));
    }
    edges
}

fn sorted_u32_edge(a: u32, b: u32) -> [u32; 2] {
    if a <= b { [a, b] } else { [b, a] }
}

fn component_name(name: &str, component: &str) -> String {
    let stem = std::path::Path::new(name).file_stem().and_then(|value| value.to_str()).unwrap_or(name).trim();
    format!("{stem} ({component})")
}

pub(super) fn include_shape_mesh_in_topology_with_spatial(
    topology: &mesh_data::Triangulation,
    topology_spatial: &crate::model::spatial::TriangleBvh,
    shape: &mesh_data::Triangulation,
    progress: &crate::model::progress::Phase,
) -> Result<IncludedShapeMesh> {
    if topology.face_count() == 0 {
        anyhow::bail!("Topology contains no faces");
    }
    if shape.face_count() == 0 {
        anyhow::bail!("Pit/stockpile solid contains no faces");
    }

    let topology_vertices = topology.vertices();
    let source_shape_vertices = shape.vertices();
    let source_shape_faces: Vec<[usize; 3]> = shape.face_vertex_indices_iter().collect();
    let source_cap = closure_cap_info(source_shape_vertices, &source_shape_faces, topology, topology_spatial)?;
    let xy_area_tolerance = reference_xy_overlap_area_tolerance(topology);

    let diag = std::env::var("PI_INCLUDE_DIAG").is_ok();
    let stage_start = web_time::Instant::now();
    let skipped_cap_faces = source_cap.face_mask.iter().filter(|masked| **masked).count();
    let mut shape_vertices = source_shape_vertices.to_vec();
    let mut shape_faces = source_shape_faces;
    let mut cap_mask = source_cap.face_mask.clone();
    // A shell exported by the dedicated cut tool already carries the exact
    // topology contact as its opening rim. Prefer that verified seam over
    // reconstructing it from triangle/triangle clip fragments: against an
    // already-cut topology the two meshes meet edge-on-edge and have no
    // positive-area overlap, while near that boundary duplicate fragments can
    // also parity-cancel. In either case a reconstructed ring is missing or can
    // be stitched across the real shell, which leaves a gap in the merge.
    let exact_contact_rings = preclipped_contact_rings(
        &shape_vertices,
        &shape_faces,
        &cap_mask,
        topology,
        topology_spatial,
        xy_area_tolerance,
        source_cap.shape_cut_side,
    );
    let clipped_shape = if let Some(rings) = exact_contact_rings.as_deref() {
        retained_vertical_shape_faces(&shape_vertices, &shape_faces, &cap_mask, rings)
    } else {
        // Complete the shell before clipping, even when some source faces
        // already cross the topology. A partial intersection produces open
        // contact arcs; clipping the unextended shell can bridge those arcs
        // while leaving the below-ground portion of the opening rim uncovered.
        // The temporary vertical curtain makes every exposed rim theoretically
        // continue until it meets the topology. Faces already above/below the
        // local surface are trimmed away by the normal clip, so fully crossing
        // shells retain their existing contact while incomplete shells gain the
        // missing seam.
        (shape_vertices, shape_faces, cap_mask) = extend_shape_opening_to_topology(&shape_vertices, &shape_faces, &cap_mask, topology_vertices, source_cap.shape_cut_side);
        clip_shape_to_topology(
            &shape_vertices,
            &shape_faces,
            &cap_mask,
            topology,
            topology_spatial,
            xy_area_tolerance,
            source_cap.shape_cut_side,
        )
        .map_err(|error| anyhow::anyhow!("Pit/stockpile clip failed after vertical rim extension: {error:#}"))?
    };
    if diag {
        log::debug!(target: "incline_design::include", "include diag: shape clip {:?}", stage_start.elapsed());
    }
    // The stages below are each a single pass over one of the meshes, so the
    // bar steps between them; the weights reflect the relative cost of each
    // stage so progress remains smooth across different inputs.
    progress.set_fraction(0.3);
    let stage_start = web_time::Instant::now();
    // A large contour flank is lost where the shape cuts through near-coincident
    // doubled topology sheets (a pushback into a prior excavation): the
    // per-triangle clip parity-cancels the near-coplanar patches there. Bridge
    // such gaps by tracing the real boundary against the topology upper
    // envelope, so the ring closes along the true crest instead of a straight
    // chord that would slice through the cut interior and leave an overhang.
    let cut_rings = if let Some(rings) = exact_contact_rings {
        rings
    } else {
        let shape_spatial = crate::model::spatial::TriangleBvh::build(shape);
        let bridge =
            |start: mesh_data::Vertex, end: mesh_data::Vertex| trace_contact_contour(start, end, shape, &shape_spatial, topology, topology_spatial, source_cap.shape_cut_side);
        rings_from_boundary_segments_bridged(&clipped_shape.topology_segments, Some(&bridge))?
    };
    if diag {
        log::debug!(target: "incline_design::include", "include diag: rings {:?}", stage_start.elapsed());
    }
    progress.set_fraction(0.45);
    if std::env::var("PI_INCLUDE_DIAG").is_ok() {
        let mut sizes: Vec<usize> = cut_rings.iter().map(|ring| ring.len()).collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        sizes.truncate(12);
        let areas: Vec<f64> = cut_rings.iter().map(|ring| signed_area_xy(ring).abs()).collect();
        let max_area = areas.iter().copied().fold(0.0f64, f64::max);
        log::debug!(target: "incline_design::include",
            "include diag: {} segments -> {} rings, largest sizes {:?}, max ring area {max_area:.0}",
            clipped_shape.topology_segments.len(),
            cut_rings.len(),
            sizes
        );
    }
    if cut_rings.is_empty() {
        anyhow::bail!("Pit/stockpile solid did not produce a topology intersection boundary");
    }

    // Partition the plane by the cut rings (a CDT constrained on every ring
    // edge, with far padding cells), classify each cell inside/outside the
    // footprint, then rebuild the topology cell by cell: faces that never
    // touch a covered cell pass through whole (vertex sharing preserved via
    // the remap), faces fully inside are dropped, and only faces straddling
    // the boundary are fragmented - each fragment a convex triangle–cell
    // overlap. This replaces the earlier per-face geo boolean difference,
    // whose cost was faces-in-ring-bbox × ring vertices and which stalled for
    // hours once a real multi-kilometre contact ring closed.
    let stage_start = web_time::Instant::now();
    let coverage = build_ring_coverage(&cut_rings)?;
    if diag {
        log::debug!(target: "incline_design::include", "include diag: coverage {:?}", stage_start.elapsed());
    }
    progress.set_fraction(0.55);

    let stage_start = web_time::Instant::now();
    let candidate_boundary_edges = topology_boundary_edges(topology);
    let candidate_count = candidate_boundary_edges.len();
    let boundary_edges = filter_presence_boundary_edges(candidate_boundary_edges, topology, topology_spatial);
    if diag {
        log::debug!(target: "incline_design::include",
            "include diag: boundary scan {:?} ({} boundary edges, {} interior cracks dropped)",
            stage_start.elapsed(),
            boundary_edges.len(),
            candidate_count - boundary_edges.len()
        );
    }

    progress.set_fraction(0.65);

    let stage_start = web_time::Instant::now();
    let shape_coverage = build_shape_cell_coverage(&cut_rings, topology, topology_spatial, &boundary_edges)?;
    if diag {
        log::debug!(target: "incline_design::include", "include diag: shape coverage {:?}", stage_start.elapsed());
    }
    progress.set_fraction(0.75);

    let stage_start = web_time::Instant::now();
    let shape_context = ShapeClipContext {
        topology,
        topology_spatial,
        xy_area_tolerance,
        side: source_cap.shape_cut_side,
        emit_surface_pieces: true,
    };
    let (shape_surface_vertices, shape_surface_faces) = clip_shape_faces_by_cells(&shape_vertices, &shape_faces, &cap_mask, &shape_coverage, &shape_context);
    if diag {
        log::debug!(target: "incline_design::include", "include diag: shape emit {:?} ({} surface faces)", stage_start.elapsed(), shape_surface_faces.len());
    }

    let stage_start = web_time::Instant::now();
    let affected = fragment_topology_faces_by_coverage(topology, &coverage, &progress.phase(0.8, 0.95));
    if diag {
        log::debug!(target: "incline_design::include", "include diag: fragment {:?}", stage_start.elapsed());
    }
    let stage_start = web_time::Instant::now();
    let mut affected_cursor = 0usize;

    let estimated_output_faces = topology.face_count().saturating_add(clipped_shape.faces.len()).saturating_add(shape_surface_faces.len());
    let mut output_vertices = Vec::with_capacity(
        topology
            .vertex_count()
            .saturating_add(affected.len().saturating_mul(3))
            .saturating_add(clipped_shape.vertices.len())
            .saturating_add(shape_surface_vertices.len()),
    );
    let mut output_faces = Vec::with_capacity(estimated_output_faces);
    let mut topology_vertex_remap = vec![u32::MAX; topology_vertices.len()];
    let mut retained_topology_faces = 0usize;

    for (face_index, face) in topology.face_vertex_indices_iter().enumerate() {
        let is_affected = affected.get(affected_cursor).map(|(index, _)| *index) == Some(face_index);
        if !is_affected {
            output_faces.push([
                remapped_topology_vertex(face[0], topology_vertices, &mut topology_vertex_remap, &mut output_vertices),
                remapped_topology_vertex(face[1], topology_vertices, &mut topology_vertex_remap, &mut output_vertices),
                remapped_topology_vertex(face[2], topology_vertices, &mut topology_vertex_remap, &mut output_vertices),
            ]);
            retained_topology_faces += 1;
            continue;
        }
        let fragments = &affected[affected_cursor].1;
        affected_cursor += 1;
        retained_topology_faces += fragments.len();
        for fragment in fragments {
            let base = output_vertices.len() as u32;
            output_vertices.extend_from_slice(fragment);
            output_faces.push([base, base + 1, base + 2]);
        }
    }

    let topology_vertex_count = output_vertices.len();
    let topology_face_count = output_faces.len();

    append_mesh(clipped_shape.vertices, clipped_shape.faces, &mut output_vertices, &mut output_faces);
    append_mesh(shape_surface_vertices, shape_surface_faces, &mut output_vertices, &mut output_faces);
    if diag {
        log::debug!(target: "incline_design::include", "include diag: assemble {:?}", stage_start.elapsed());
    }

    Ok(IncludedShapeMesh {
        vertices: output_vertices,
        faces: output_faces,
        topology_vertex_count,
        topology_face_count,
        retained_topology_faces,
        skipped_cap_faces,
    })
}

#[inline(always)]
fn remapped_topology_vertex(vertex_index: usize, topology_vertices: &[mesh_data::Vertex], topology_vertex_remap: &mut [u32], output_vertices: &mut Vec<mesh_data::Vertex>) -> u32 {
    if topology_vertex_remap[vertex_index] != u32::MAX {
        return topology_vertex_remap[vertex_index];
    }
    let mapped = output_vertices.len() as u32;
    output_vertices.push(topology_vertices[vertex_index]);
    topology_vertex_remap[vertex_index] = mapped;
    mapped
}

/// The plane partitioned by the cut rings: a constrained triangulation whose
/// cells never cross a ring edge, so each cell is entirely inside or outside
/// the replacement footprint.
pub(super) struct RingCoverage {
    triangles: Vec<[mesh_data::Vertex; 3]>,
    covered: Vec<bool>,
    spatial: crate::model::spatial::TriangleBvh,
}

pub(super) fn build_ring_coverage(rings: &[Vec<mesh_data::Vertex>]) -> Result<RingCoverage> {
    use rayon::prelude::*;
    use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation as _};

    let mut min = glam::DVec2::splat(f64::INFINITY);
    let mut max = glam::DVec2::splat(f64::NEG_INFINITY);
    for ring in rings {
        for vertex in ring {
            if !vertex.x.is_finite() || !vertex.y.is_finite() {
                anyhow::bail!("Cut ring contains non-finite coordinates");
            }
            min = min.min(glam::DVec2::new(vertex.x, vertex.y));
            max = max.max(glam::DVec2::new(vertex.x, vertex.y));
        }
    }
    if !min.x.is_finite() {
        anyhow::bail!("Cut rings are empty");
    }
    let origin = min;
    let extent = max - min;

    // Same bulk-load scheme as the pit-shell envelope: exact-bit point dedup
    // (-0.0 normalised) keeps our indices aligned with spade's, and only
    // edges the bulk loader rejects as crossing go through the splitting
    // insert. Rings from doubled survey sheets sit nearly on top of each
    // other in XY, so crossings are expected and handled.
    let normalized = |value: f64| if value == 0.0 { 0.0 } else { value };
    let padding = cuts::ENVELOPE_PADDING;
    let mut points: Vec<Point2<f64>> = Vec::new();
    for (corner_x, corner_y) in [
        (-padding, -padding),
        (extent.x + padding, -padding),
        (extent.x + padding, extent.y + padding),
        (-padding, extent.y + padding),
    ] {
        points.push(Point2::new(corner_x, corner_y));
    }
    let mut point_indices: HashMap<(u64, u64), usize> = HashMap::new();
    let mut edges: HashSet<(usize, usize)> = HashSet::new();
    for ring in rings {
        let mut indices = Vec::with_capacity(ring.len());
        for vertex in ring {
            let local_x = normalized(vertex.x - origin.x);
            let local_y = normalized(vertex.y - origin.y);
            indices.push(*point_indices.entry((local_x.to_bits(), local_y.to_bits())).or_insert_with(|| {
                points.push(Point2::new(local_x, local_y));
                points.len() - 1
            }));
        }
        for i in 0..indices.len() {
            let a = indices[i];
            let b = indices[(i + 1) % indices.len()];
            if a != b {
                edges.insert(sorted_usize_pair(a, b));
            }
        }
    }
    let mut edge_list: Vec<[usize; 2]> = edges.into_iter().map(|(a, b)| [a, b]).collect();
    edge_list.sort_unstable();

    let point_count = points.len();
    let mut conflicting_edges: Vec<[usize; 2]> = Vec::new();
    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::try_bulk_load_cdt(points, edge_list, |edge| conflicting_edges.push(edge))
        .map_err(|error| anyhow::anyhow!("Cut ring CDT bulk load failed: {error:?}"))?;
    if cdt.num_vertices() != point_count {
        anyhow::bail!("Cut ring CDT dropped vertices unexpectedly during bulk load");
    }
    cuts::add_split_constraints(&mut cdt, conflicting_edges, "Cut ring coverage", origin);

    let pips: Vec<RingPip> = rings.iter().map(|ring| RingPip::build(ring)).collect();
    let cells: Vec<[glam::DVec2; 3]> = cdt
        .inner_faces()
        .map(|face| {
            face.vertices().map(|vertex| {
                let position = vertex.position();
                glam::DVec2::new(position.x + origin.x, position.y + origin.y)
            })
        })
        .collect();
    let classified: Vec<([mesh_data::Vertex; 3], bool)> = cells
        .par_iter()
        .filter_map(|corners| {
            let cell = corners.map(|corner| mesh_data::Vertex::new(corner.x, corner.y, 0.0));
            if triangle_xy_area(cell).abs() <= 1e-12 {
                return None;
            }
            let centroid = (corners[0] + corners[1] + corners[2]) / 3.0;
            // Inside ANY ring counts as covered: doubled sheets yield two
            // nearly coincident rings over the same footprint, and even-odd
            // across all rings together would cancel them out.
            let covered = pips.iter().any(|pip| pip.contains(centroid));
            Some((cell, covered))
        })
        .collect();

    let mut vertices = Vec::with_capacity(classified.len() * 3);
    let mut faces = Vec::with_capacity(classified.len());
    let mut triangles = Vec::with_capacity(classified.len());
    let mut covered = Vec::with_capacity(classified.len());
    for (cell, is_covered) in classified {
        let base = vertices.len() as u32;
        vertices.extend_from_slice(&cell);
        faces.push([base, base + 1, base + 2]);
        triangles.push(cell);
        covered.push(is_covered);
    }
    let mesh = mesh_data::Triangulation::from_vertices_and_faces(vertices, faces)?;
    let spatial = crate::model::spatial::TriangleBvh::build(&mesh);
    Ok(RingCoverage { triangles, covered, spatial })
}

/// Even-odd point-in-polyline over one ring, with edges bucketed into
/// horizontal bands so a query touches only the few edges crossing its Y.
struct RingPip {
    min: glam::DVec2,
    max: glam::DVec2,
    band_height: f64,
    bands: Vec<Vec<(glam::DVec2, glam::DVec2)>>,
}

impl RingPip {
    fn build(ring: &[mesh_data::Vertex]) -> Self {
        let mut min = glam::DVec2::splat(f64::INFINITY);
        let mut max = glam::DVec2::splat(f64::NEG_INFINITY);
        for vertex in ring {
            min = min.min(glam::DVec2::new(vertex.x, vertex.y));
            max = max.max(glam::DVec2::new(vertex.x, vertex.y));
        }
        let band_count = ring.len().clamp(1, 1024);
        let span = (max.y - min.y).max(1e-12);
        let band_height = span / band_count as f64;
        let mut bands = vec![Vec::new(); band_count];
        for index in 0..ring.len() {
            let a = ring[index];
            let b = ring[(index + 1) % ring.len()];
            let a = glam::DVec2::new(a.x, a.y);
            let b = glam::DVec2::new(b.x, b.y);
            let (y0, y1) = if a.y <= b.y { (a.y, b.y) } else { (b.y, a.y) };
            let first = (((y0 - min.y) / band_height) as usize).min(band_count - 1);
            let last = (((y1 - min.y) / band_height) as usize).min(band_count - 1);
            for band in &mut bands[first..=last] {
                band.push((a, b));
            }
        }
        Self { min, max, band_height, bands }
    }

    fn contains(&self, point: glam::DVec2) -> bool {
        if point.x < self.min.x || point.x > self.max.x || point.y < self.min.y || point.y > self.max.y {
            return false;
        }
        let band = (((point.y - self.min.y) / self.band_height) as usize).min(self.bands.len() - 1);
        let mut inside = false;
        for (a, b) in &self.bands[band] {
            if (a.y > point.y) != (b.y > point.y) {
                let t = (point.y - a.y) / (b.y - a.y);
                if a.x + t * (b.x - a.x) > point.x {
                    inside = !inside;
                }
            }
        }
        inside
    }
}

/// For every topology face that overlaps a covered (inside-footprint) cell,
/// the fragments of it lying over uncovered cells - i.e. the part of the face
/// that survives the cut. Faces absent from the result never touch the
/// footprint and pass through whole; present with no fragments means fully
/// excavated. Returned sorted by face index.
fn fragment_topology_faces_by_coverage(
    topology: &mesh_data::Triangulation,
    coverage: &RingCoverage,
    progress: &crate::model::progress::Phase,
) -> Vec<(usize, Vec<[mesh_data::Vertex; 3]>)> {
    use rayon::prelude::*;

    let vertices = topology.vertices();
    let faces: Vec<[usize; 3]> = topology.face_vertex_indices_iter().collect();
    let task_count = rayon::current_num_threads().saturating_mul(4).max(1);
    let chunk_size = faces.len().div_ceil(task_count).max(1);
    let fragmented = progress.counter(faces.len());
    let partials: Vec<Vec<(usize, Vec<[mesh_data::Vertex; 3]>)>> = faces
        .par_chunks(chunk_size)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let mut affected = Vec::new();
            let mut candidate_stack = Vec::new();
            let mut candidates: Vec<usize> = Vec::new();
            let face_index_base = chunk_index * chunk_size;

            for (offset, face) in chunk.iter().enumerate() {
                let target = [vertices[face[0]], vertices[face[1]], vertices[face[2]]];
                let bounds = triangle_xy_bounds(target);
                candidates.clear();
                coverage
                    .spatial
                    .for_each_xy_bounds_candidate_index_with_stack(bounds.0, bounds.1, &mut candidate_stack, |index| candidates.push(index));

                // Most faces sit far from the footprint: test the covered
                // candidates first and skip all overlap work if none touch.
                let touched = candidates
                    .iter()
                    .any(|&index| coverage.covered[index] && clip_target_triangle_to_reference_xy(target, coverage.triangles[index]).len() >= 3);
                if !touched {
                    continue;
                }
                let mut fragments = Vec::new();
                for &index in &candidates {
                    if coverage.covered[index] {
                        continue;
                    }
                    let overlap = clip_target_triangle_to_reference_xy(target, coverage.triangles[index]);
                    append_xy_polyline_fan(&overlap, &mut fragments);
                }
                affected.push((face_index_base + offset, fragments));
            }
            fragmented.advance_by(chunk.len());
            affected
        })
        .collect();

    let mut affected = Vec::with_capacity(partials.iter().map(Vec::len).sum());
    for partial in partials {
        affected.extend(partial);
    }
    affected
}

fn append_xy_polyline_fan(polyline: &[glam::DVec3], output: &mut Vec<[mesh_data::Vertex; 3]>) {
    if polyline.len() < 3 {
        return;
    }
    let a = polyline[0];
    for i in 1..polyline.len() - 1 {
        let b = polyline[i];
        let c = polyline[i + 1];
        if (b - a).cross(c - a).length_squared() > 1e-20 {
            output.push([
                mesh_data::Vertex::new(a.x, a.y, a.z),
                mesh_data::Vertex::new(b.x, b.y, b.z),
                mesh_data::Vertex::new(c.x, c.y, c.z),
            ]);
        }
    }
}

/// Opening edges of a shape after its removable cap is ignored. Closed solids
/// use the cap perimeter; already-open shells use their free mesh boundary.
/// Coordinate keys (rather than vertex indices) also handle unwelded triangle
/// soup without treating every internal triangle edge as an opening.
fn shape_opening_edges(source_vertices: &[mesh_data::Vertex], source_faces: &[[usize; 3]], cap_mask: &[bool]) -> Vec<(usize, usize)> {
    type VertexKey = [u64; 3];
    type EdgeKey = (VertexKey, VertexKey);

    let vertex_key = |vertex: mesh_data::Vertex| {
        let bits = |value: f64| {
            if value == 0.0 { 0.0f64.to_bits() } else { value.to_bits() }
        };
        [bits(vertex.x), bits(vertex.y), bits(vertex.z)]
    };
    let mut edge_counts: HashMap<EdgeKey, ((usize, usize), usize)> = HashMap::new();
    let has_removed_cap = cap_mask.iter().any(|masked| *masked);
    for (face_index, face) in source_faces.iter().enumerate() {
        if has_removed_cap && !cap_mask.get(face_index).copied().unwrap_or(false) {
            continue;
        }
        for edge_index in 0..3 {
            let a = face[edge_index];
            let b = face[(edge_index + 1) % 3];
            let a_key = vertex_key(source_vertices[a]);
            let b_key = vertex_key(source_vertices[b]);
            let key = if a_key <= b_key { (a_key, b_key) } else { (b_key, a_key) };
            edge_counts.entry(key).and_modify(|(_, count)| *count += 1).or_insert(((a, b), 1));
        }
    }

    let mut opening_edges: Vec<(usize, usize)> = edge_counts.into_iter().filter_map(|(_, (edge, count))| (count == 1).then_some(edge)).collect();
    opening_edges.sort_unstable();
    opening_edges
}

/// Parameter interval of `a -> b` whose XY projection lies in `triangle`.
/// A triangle is convex, so clipping the segment against its three oriented
/// half-planes produces at most one interval. The caller rejects degenerate XY
/// triangles before reaching this helper.
fn segment_triangle_xy_interval(a: mesh_data::Vertex, b: mesh_data::Vertex, triangle: [mesh_data::Vertex; 3]) -> Option<(f64, f64)> {
    let triangle_ccw = triangle_xy_area(triangle) > 0.0;
    let a_xy = glam::DVec2::new(a.x, a.y);
    let b_xy = glam::DVec2::new(b.x, b.y);
    let mut t_min: f64 = 0.0;
    let mut t_max: f64 = 1.0;

    for edge_index in 0..3 {
        let edge_a = triangle[edge_index];
        let edge_b = triangle[(edge_index + 1) % 3];
        let edge_a = glam::DVec2::new(edge_a.x, edge_a.y);
        let edge_b = glam::DVec2::new(edge_b.x, edge_b.y);
        let orient = |point| {
            let distance = crate::model::kernel::signed_distance_to_line(point, edge_a, edge_b);
            if triangle_ccw { distance } else { -distance }
        };
        let distance_a = orient(a_xy);
        let distance_b = orient(b_xy);

        if distance_a < 0.0 && distance_b < 0.0 {
            return None;
        }
        if distance_a >= 0.0 && distance_b >= 0.0 {
            continue;
        }

        let denominator = distance_a - distance_b;
        if denominator == 0.0 {
            return None;
        }
        let crossing = (distance_a / denominator).clamp(0.0, 1.0);
        if distance_a < 0.0 {
            t_min = t_min.max(crossing);
        } else {
            t_max = t_max.min(crossing);
        }
        if t_min > t_max {
            return None;
        }
    }

    Some((t_min, t_max))
}

/// Prove that every part of a straight opening-rim edge lies on at least one
/// topology triangle at the same elevation. Matching only a few samples is not
/// sufficient: a long rim edge can cross several TIN faces and agree at the
/// samples while departing from the terrain between them.
fn rim_segment_is_continuously_on_topology(
    a: mesh_data::Vertex,
    b: mesh_data::Vertex,
    topology: &mesh_data::Triangulation,
    topology_spatial: &crate::model::spatial::TriangleBvh,
    xy_area_tolerance: f64,
    z_tolerance: f64,
    candidate_stack: &mut Vec<usize>,
) -> bool {
    let a_xy = glam::DVec2::new(a.x, a.y);
    let b_xy = glam::DVec2::new(b.x, b.y);
    let segment_length = a_xy.distance(b_xy);
    if !segment_length.is_finite() || segment_length <= f64::EPSILON {
        return false;
    }

    let min = a_xy.min(b_xy);
    let max = a_xy.max(b_xy);
    let topology_vertices = topology.vertices();
    let mut matching_intervals = Vec::new();
    topology_spatial.for_each_xy_bounds_candidate_index_with_stack(min, max, candidate_stack, |face_index| {
        let Some(face) = topology.face_vertex_indices(face_index) else {
            return;
        };
        let triangle = [topology_vertices[face[0]], topology_vertices[face[1]], topology_vertices[face[2]]];
        if triangle_xy_area(triangle).abs() <= xy_area_tolerance {
            return;
        }
        let Some((t_start, t_end)) = segment_triangle_xy_interval(a, b, triangle) else {
            return;
        };
        if t_end <= t_start {
            return;
        }

        let shape_z = |t: f64| a.z + (b.z - a.z) * t;
        let topology_z = |t: f64| {
            let x = a.x + (b.x - a.x) * t;
            let y = a.y + (b.y - a.y) * t;
            bary_z(x, y, triangle)
        };
        if (shape_z(t_start) - topology_z(t_start)).abs() <= z_tolerance && (shape_z(t_end) - topology_z(t_end)).abs() <= z_tolerance {
            // Both height functions are linear over this topology face, so
            // equality at the interval ends proves equality throughout it.
            matching_intervals.push((t_start, t_end));
        }
    });

    matching_intervals.sort_unstable_by(|left, right| left.0.total_cmp(&right.0));
    // Shared TIN edges should produce identical interval endpoints. Allow only
    // relative floating-point drift here: a metric XY tolerance would permit
    // most of a very short rim edge to remain unverified.
    let parameter_tolerance = 1.0e-9;
    let mut covered_until: f64 = 0.0;
    for (start, end) in matching_intervals {
        if start > covered_until + parameter_tolerance {
            return false;
        }
        covered_until = covered_until.max(end);
        if covered_until >= 1.0 - parameter_tolerance {
            return true;
        }
    }
    false
}

/// Use an already-clipped opening rim as the cut seam only when it is a closed
/// contour, every complete edge lies on the topology, and all sampled shape
/// geometry is on the side that the include operation retains. These checks
/// keep the shortcut specific to pre-clipped inputs; ordinary crossing solids
/// continue through the full triangle clipping path.
fn preclipped_contact_rings(
    shape_vertices: &[mesh_data::Vertex],
    shape_faces: &[[usize; 3]],
    cap_mask: &[bool],
    topology: &mesh_data::Triangulation,
    topology_spatial: &crate::model::spatial::TriangleBvh,
    xy_area_tolerance: f64,
    side: TriSurfaceCutSide,
) -> Option<Vec<Vec<mesh_data::Vertex>>> {
    let opening_segments: Vec<[mesh_data::Vertex; 2]> = shape_opening_edges(shape_vertices, shape_faces, cap_mask)
        .into_iter()
        .map(|(a, b)| [shape_vertices[a], shape_vertices[b]])
        .collect();
    if !boundary_segments_are_closed(&opening_segments) {
        return None;
    }
    let rings = rings_from_boundary_segments_bridged(&opening_segments, None).ok()?;

    let bounds = topology.bounds();
    let z_tolerance = ((bounds.max.z - bounds.min.z).abs() * 1.0e-9).max(1.0e-7);
    let mut candidate_stack = Vec::new();
    for ring in &rings {
        for index in 0..ring.len() {
            let a = ring[index];
            let b = ring[(index + 1) % ring.len()];
            if !rim_segment_is_continuously_on_topology(a, b, topology, topology_spatial, xy_area_tolerance, z_tolerance, &mut candidate_stack) {
                return None;
            }
        }
    }

    let context = ShapeClipContext {
        topology,
        topology_spatial,
        xy_area_tolerance,
        side,
        emit_surface_pieces: false,
    };
    for (face_index, face) in shape_faces.iter().enumerate() {
        if cap_mask.get(face_index).copied().unwrap_or(false) {
            continue;
        }
        let triangle = [shape_vertices[face[0]], shape_vertices[face[1]], shape_vertices[face[2]]];
        let centroid = glam::DVec3::new(
            (triangle[0].x + triangle[1].x + triangle[2].x) / 3.0,
            (triangle[0].y + triangle[1].y + triangle[2].y) / 3.0,
            (triangle[0].z + triangle[1].z + triangle[2].z) / 3.0,
        );
        for point in triangle
            .iter()
            .map(|vertex| glam::DVec3::new(vertex.x, vertex.y, vertex.z))
            .chain(std::iter::once(centroid))
        {
            let Some(delta) = worst_height_delta(point, &context, &mut candidate_stack) else {
                continue;
            };
            let retained = match side {
                TriSurfaceCutSide::CutTop => delta <= z_tolerance,
                TriSurfaceCutSide::CutBottom => delta >= -z_tolerance,
            };
            if !retained {
                return None;
            }
        }
    }
    Some(rings)
}

fn boundary_segments_are_closed(segments: &[[mesh_data::Vertex; 2]]) -> bool {
    let mut grid = BoundaryNodeGrid::new(1.0e-8);
    let mut edge_counts: HashMap<(usize, usize), usize> = HashMap::new();
    for [a, b] in segments {
        let a = grid.index_for(*a);
        let b = grid.index_for(*b);
        if a != b {
            *edge_counts.entry(sorted_usize_pair(a, b)).or_insert(0) += 1;
        }
    }
    let edges: Vec<(usize, usize)> = edge_counts.into_iter().filter_map(|(edge, count)| (count % 2 == 1).then_some(edge)).collect();
    if edges.len() < 3 {
        return false;
    }
    let mut degree = vec![0usize; grid.nodes.len()];
    for (a, b) in edges {
        degree[a] += 1;
        degree[b] += 1;
    }
    degree.into_iter().all(|degree| degree == 0 || degree == 2)
}

fn retained_vertical_shape_faces(shape_vertices: &[mesh_data::Vertex], shape_faces: &[[usize; 3]], cap_mask: &[bool], rings: &[Vec<mesh_data::Vertex>]) -> ClippedShapeMesh {
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    for (face_index, face) in shape_faces.iter().copied().enumerate() {
        if cap_mask.get(face_index).copied().unwrap_or(false) {
            continue;
        }
        let triangle = [shape_vertices[face[0]], shape_vertices[face[1]], shape_vertices[face[2]]];
        if triangle_xy_area(triangle).abs() > 1.0e-10 || triangle_area_3d(triangle) <= 1.0e-12 {
            continue;
        }
        let base = vertices.len() as u32;
        vertices.extend_from_slice(&triangle);
        faces.push([base, base + 1, base + 2]);
    }
    let topology_segments = rings
        .iter()
        .flat_map(|ring| (0..ring.len()).map(|index| [ring[index], ring[(index + 1) % ring.len()]]))
        .collect();
    ClippedShapeMesh {
        vertices,
        faces,
        topology_segments,
    }
}

/// Clone a shape and add vertical faces from its opening to beyond the target
/// topology. A closed solid opens where its removable closure cap meets the
/// shell; an already-open shell uses its free boundary.
///
/// Extending to the topology's global Z range (rather than sampling it at rim
/// vertices) also handles sparse TINs and overlapping survey sheets. The
/// normal clip immediately trims the temporary faces back to the exact local
/// topology elevation.
fn extend_shape_opening_to_topology(
    source_vertices: &[mesh_data::Vertex],
    source_faces: &[[usize; 3]],
    cap_mask: &[bool],
    topology_vertices: &[mesh_data::Vertex],
    side: TriSurfaceCutSide,
) -> (Vec<mesh_data::Vertex>, Vec<[usize; 3]>, Vec<bool>) {
    let opening_edges = shape_opening_edges(source_vertices, source_faces, cap_mask);

    let (topology_z_min, topology_z_max) = topology_vertices
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), vertex| (min.min(vertex.z), max.max(vertex.z)));
    if opening_edges.is_empty() || !topology_z_min.is_finite() || !topology_z_max.is_finite() {
        return (source_vertices.to_vec(), source_faces.to_vec(), cap_mask.to_vec());
    }

    let z_span = (topology_z_max - topology_z_min).abs();
    let z_margin = z_span.max(1.0) * 1.0e-6;
    let extension_z = match side {
        TriSurfaceCutSide::CutTop => topology_z_max + z_margin,
        TriSurfaceCutSide::CutBottom => topology_z_min - z_margin,
    };

    let mut vertices = source_vertices.to_vec();
    let mut faces = source_faces.to_vec();
    let mut extended_mask = cap_mask.to_vec();
    faces.reserve(opening_edges.len() * 2);
    extended_mask.reserve(opening_edges.len() * 2);

    for (a, b) in opening_edges {
        let va = source_vertices[a];
        let vb = source_vertices[b];
        let extended_a_z = match side {
            TriSurfaceCutSide::CutTop => va.z.max(extension_z),
            TriSurfaceCutSide::CutBottom => va.z.min(extension_z),
        };
        let extended_b_z = match side {
            TriSurfaceCutSide::CutTop => vb.z.max(extension_z),
            TriSurfaceCutSide::CutBottom => vb.z.min(extension_z),
        };
        if extended_a_z == va.z && extended_b_z == vb.z {
            continue;
        }

        let extended_a = vertices.len();
        vertices.push(mesh_data::Vertex::new(va.x, va.y, extended_a_z));
        let extended_b = vertices.len();
        vertices.push(mesh_data::Vertex::new(vb.x, vb.y, extended_b_z));
        faces.push([a, b, extended_b]);
        faces.push([a, extended_b, extended_a]);
        extended_mask.push(false);
        extended_mask.push(false);
    }

    (vertices, faces, extended_mask)
}

pub(super) struct ClosureCapInfo {
    face_mask: Vec<bool>,
    shape_cut_side: TriSurfaceCutSide,
}

/// Classify the solid as a pit (keep its lower surface, CutTop) or a stockpile
/// (keep its upper surface, CutBottom), and mask the flat closure cap on the
/// removed side so it never contributes geometry.
///
/// Classification is primarily **topology-invariant** - it reads the solid's own
/// structure, not its position relative to the target topology. For open shell
/// surfaces the free crest/toe rim decides (rim above the surface mean ⇒ pit).
/// For closed solids the decisive signal is flat-cap asymmetry, split by closure
/// type. A *frustum* (a real horizontal flat cap at both z extremes) is decided
/// by batter geometry: the walls widen toward the ground opening, so the *larger*
/// cap is the closure and the *smaller* is the design surface. A pit's opening
/// (`z_max`) is wider than its floor (`z_min`) - remove the top, keep the floor
/// (`CutTop`); a stockpile's base (`z_min`) is wider than its crest (`z_max`) -
/// remove the base, keep the crest (`CutBottom`). A solid with a flat cap at only
/// *one* extreme (its closure crest follows terrain, or tents to an apex - ~0
/// flat area) keeps that sole cap as the design surface. Both branches are
/// topology-invariant, so they stay correct when the target topology has already
/// been excavated by a prior include - the previous position-vs-topology score
/// read the lowered post-excavation reference and flipped a real pit into a
/// stockpile. When the two caps are nearly equal (a vertical-walled box,
/// ambiguous from caps alone) the cap signal abstains and falls through.
///
/// The position-vs-topology score survives only as the last-resort tiebreaker
/// (open shell surfaces with no flat caps, or symmetric caps), where it is still
/// correct against original, un-excavated ground.
pub(super) fn closure_cap_info(
    vertices: &[mesh_data::Vertex],
    faces: &[[usize; 3]],
    topology: &mesh_data::Triangulation,
    topology_spatial: &crate::model::spatial::TriangleBvh,
) -> Result<ClosureCapInfo> {
    let mut mask = vec![false; faces.len()];
    if faces.is_empty() || vertices.is_empty() {
        anyhow::bail!("Pit/stockpile solid contains no cap faces");
    }

    let mut z_min = f64::INFINITY;
    let mut z_max = f64::NEG_INFINITY;
    for vertex in vertices {
        z_min = z_min.min(vertex.z);
        z_max = z_max.max(vertex.z);
    }
    let z_tolerance = ((z_max - z_min).abs() * 1e-8).max(1e-6);

    let mut min_area = 0.0;
    let mut max_area = 0.0;
    let mut candidates = Vec::new();
    let mut is_cap_candidate = vec![false; faces.len()];
    for (face_index, face) in faces.iter().copied().enumerate() {
        let triangle = [vertices[face[0]], vertices[face[1]], vertices[face[2]]];
        if !triangle_is_flat_z(triangle, z_tolerance) {
            continue;
        }

        let area = triangle_xy_area(triangle).abs();
        if area <= 1e-10 {
            continue;
        }

        let z = (triangle[0].z + triangle[1].z + triangle[2].z) / 3.0;
        if (z - z_min).abs() <= z_tolerance {
            min_area += area;
            candidates.push((face_index, false));
            is_cap_candidate[face_index] = true;
        } else if (z - z_max).abs() <= z_tolerance {
            max_area += area;
            candidates.push((face_index, true));
            is_cap_candidate[face_index] = true;
        }
    }

    // Decision precedence - topology-invariant signals first, so the result is
    // unaffected by a target topology already excavated by a prior include.
    //
    // 1. Open-rim elevation (open shell surfaces): a free crest/toe rim's
    //    elevation vs the area-weighted surface mean. `open_rim_above_surface`
    //    returns `None` for closed solids, so this only fires for genuinely open
    //    surfaces whose flat extremes are design features (berm skirts, floors),
    //    not closure caps.
    //
    // 2. Flat-cap asymmetry (closed solids), topology-invariant:
    //    - Frustum (a flat cap at both extremes): battered walls widen toward the
    //      ground opening, so the *larger* cap is the closure to remove and the
    //      *smaller* is the design surface. A pit's opening (z_max) > its floor
    //      (z_min) - remove the top, keep the floor (CutTop). A stockpile's base
    //      (z_min) > its crest (z_max) - remove the base, keep the crest.
    //    - Sole flat cap (the other extreme is tilted/tented, ~0 flat area): that
    //      one cap is the design surface - keep it, cut the other side.
    //    Either way the result is independent of the target topology, so a second
    //    pit included into already-excavated ground still reads as a pit.
    //    Symmetric cap pairs (vertical-walled box helpers) abstain and fall
    //    through to the position score.
    //
    // 3. Position relative to topology (last resort): correct only against
    //    original, un-excavated ground.
    let cap_total = min_area + max_area;
    let cap_asymmetric = cap_total > 1e-10 && (min_area - max_area).abs() > 0.05 * cap_total;
    let larger_cap = min_area.max(max_area);
    let smaller_cap = min_area.min(max_area);
    // A genuine frustum carries a real horizontal flat cap at *both* extremes; a
    // solid whose closure is tilted (crest follows terrain) or tented contributes
    // exactly zero flat area at that extreme, so its sole flat cap is the design
    // surface. `1e-6` of the larger cap separates a real (however small) opening
    // cap from a tilt/tent's zero contribution.
    let both_caps_flat = smaller_cap > 1e-6 * larger_cap;
    let remove_max_cap = if let Some(rim_above_surface) = open_rim_above_surface(vertices, faces) {
        rim_above_surface
    } else if cap_asymmetric && both_caps_flat {
        // Frustum: battered walls widen toward the ground opening, so the larger
        // cap is the closure to remove (a pit's opening > its floor; a
        // stockpile's base > its crest).
        max_area > min_area
    } else if cap_asymmetric {
        // Only one extreme carries a flat cap (the other is tilted/tented); that
        // sole flat cap is the design surface - keep it, cut the other side.
        min_area > max_area
    } else {
        let topology_vertices = topology.vertices();
        let height_score = topology_height_score(vertices, faces, &is_cap_candidate, topology, topology_spatial, topology_vertices);
        if height_score < -1e-9 {
            true
        } else if height_score > 1e-9 {
            false
        } else if min_area <= 1e-10 && max_area <= 1e-10 {
            anyhow::bail!("Pit/stockpile solid has no flat closure cap and does not overlap the topology");
        } else {
            min_area > max_area
        }
    };
    let shape_cut_side = if remove_max_cap { TriSurfaceCutSide::CutTop } else { TriSurfaceCutSide::CutBottom };
    for (face_index, is_max_cap) in candidates {
        if is_max_cap == remove_max_cap {
            mask[face_index] = true;
        }
    }
    Ok(ClosureCapInfo { face_mask: mask, shape_cut_side })
}

/// Per-non-cap-face 3D area × (centroid z − topology z), summed in parallel.
/// Negative ⇒ the solid's bulk is below the topology (pit). Last-resort
/// classifier only - it reads the current topology reference, so it flips a
/// real pit to stockpile when the topology has already been excavated by a
/// prior include. The topology-invariant flat-cap signal in `closure_cap_info`
/// handles that case; this is reached only when the cap and rim signals are
/// inconclusive.
fn topology_height_score(
    vertices: &[mesh_data::Vertex],
    faces: &[[usize; 3]],
    is_cap_candidate: &[bool],
    topology: &mesh_data::Triangulation,
    topology_spatial: &crate::model::spatial::TriangleBvh,
    topology_vertices: &[mesh_data::Vertex],
) -> f64 {
    use rayon::prelude::*;
    faces
        .par_iter()
        .enumerate()
        .fold(
            || (Vec::new(), 0.0f64),
            |(mut candidate_stack, mut score), (face_index, face)| {
                if is_cap_candidate[face_index] {
                    return (candidate_stack, score);
                }
                let triangle = [vertices[face[0]], vertices[face[1]], vertices[face[2]]];
                let centroid_x = (triangle[0].x + triangle[1].x + triangle[2].x) / 3.0;
                let centroid_y = (triangle[0].y + triangle[1].y + triangle[2].y) / 3.0;
                let centroid_z = (triangle[0].z + triangle[1].z + triangle[2].z) / 3.0;
                // Uppermost topology sheet covering the centroid (overlapping
                // resurvey sheets can stack; the ground is the top one).
                let mut topology_z = f64::NEG_INFINITY;
                let centroid_xy = glam::DVec2::new(centroid_x, centroid_y);
                topology_spatial.for_each_xy_bounds_candidate_index_with_stack(centroid_xy, centroid_xy, &mut candidate_stack, |topology_index| {
                    if let Some(reference_face) = topology.face_vertex_indices(topology_index) {
                        let reference = [
                            topology_vertices[reference_face[0]],
                            topology_vertices[reference_face[1]],
                            topology_vertices[reference_face[2]],
                        ];
                        if let Some(z) = point_in_triangle_bary_z(centroid_x, centroid_y, reference) {
                            topology_z = topology_z.max(z);
                        }
                    }
                });
                if topology_z.is_finite() {
                    score += triangle_area_3d(triangle) * (centroid_z - topology_z);
                }
                (candidate_stack, score)
            },
        )
        .map(|(_, score)| score)
        .sum()
}

/// Whether the mesh's open rim (edges used by exactly one face) sits above
/// its area-weighted mean surface elevation. `None` when the mesh is closed,
/// the rim is too short to be a real crest/toe (shorter than the square root
/// of the surface area - stray cracks in an otherwise closed solid), or the
/// elevation difference is within noise.
fn open_rim_above_surface(vertices: &[mesh_data::Vertex], faces: &[[usize; 3]]) -> Option<bool> {
    let mut edge_counts: HashMap<(usize, usize), usize> = HashMap::new();
    for face in faces {
        for i in 0..3 {
            *edge_counts.entry(sorted_usize_pair(face[i], face[(i + 1) % 3])).or_insert(0) += 1;
        }
    }
    let mut rim_length = 0.0;
    let mut rim_z_weighted = 0.0;
    for ((a, b), count) in &edge_counts {
        if *count != 1 {
            continue;
        }
        let va = vertices[*a];
        let vb = vertices[*b];
        let dx = va.x - vb.x;
        let dy = va.y - vb.y;
        let dz = va.z - vb.z;
        let length = (dx * dx + dy * dy + dz * dz).sqrt();
        rim_length += length;
        rim_z_weighted += length * (va.z + vb.z) * 0.5;
    }
    if rim_length <= 1e-9 {
        return None;
    }

    let mut total_area = 0.0;
    let mut surface_z_weighted = 0.0;
    let mut z_min = f64::INFINITY;
    let mut z_max = f64::NEG_INFINITY;
    for face in faces {
        let triangle = [vertices[face[0]], vertices[face[1]], vertices[face[2]]];
        let area = triangle_area_3d(triangle);
        total_area += area;
        surface_z_weighted += area * (triangle[0].z + triangle[1].z + triangle[2].z) / 3.0;
        for vertex in triangle {
            z_min = z_min.min(vertex.z);
            z_max = z_max.max(vertex.z);
        }
    }
    if total_area <= 1e-9 || rim_length * rim_length <= total_area {
        return None;
    }
    let rim_z = rim_z_weighted / rim_length;
    let surface_z = surface_z_weighted / total_area;
    if (rim_z - surface_z).abs() <= (z_max - z_min).abs() * 0.01 {
        return None;
    }
    Some(rim_z > surface_z)
}

pub(super) fn triangle_is_flat_z(triangle: [mesh_data::Vertex; 3], tolerance: f64) -> bool {
    let z_min = triangle.iter().map(|vertex| vertex.z).fold(f64::INFINITY, f64::min);
    let z_max = triangle.iter().map(|vertex| vertex.z).fold(f64::NEG_INFINITY, f64::max);
    z_max - z_min <= tolerance
}

/// Given the two dead-ends of an open contour flank, return a polyline (its
/// own end points snapping back onto the dead-ends) that traces the true cut
/// boundary across the gap. `None` if it cannot, in which case the caller
/// falls back to a straight chord.
pub(super) type GapBridge<'a> = &'a dyn Fn(mesh_data::Vertex, mesh_data::Vertex) -> Option<Vec<mesh_data::Vertex>>;

pub(super) fn rings_from_boundary_segments_bridged(segments: &[[mesh_data::Vertex; 2]], gap_bridge: Option<GapBridge<'_>>) -> Result<Vec<Vec<mesh_data::Vertex>>> {
    let tolerance_sq = 1e-8;
    let mut grid = BoundaryNodeGrid::new(tolerance_sq);
    // Segments shared by adjacent clip polylines appear an even number of
    // times and cancel; only odd-count edges lie on the cut boundary.
    let mut edge_counts: HashMap<(usize, usize), usize> = HashMap::new();
    for segment in segments {
        let a = grid.index_for(segment[0]);
        let b = grid.index_for(segment[1]);
        if a != b {
            *edge_counts.entry(sorted_usize_pair(a, b)).or_insert(0) += 1;
        }
    }
    let mut edges: Vec<(usize, usize)> = edge_counts.into_iter().filter(|(_, count)| count % 2 == 1).map(|(edge, _)| edge).collect();
    edges.sort_unstable();
    stitch_contour_gaps(&grid.nodes, &mut edges, segments);
    // Close any large open flank left by the distance-capped stitch - tracing
    // the real boundary across the gap when a bridger is supplied, otherwise a
    // straight chord. Adds nodes to `grid`, so it must precede the read-only
    // ring walk below.
    bridge_or_close_open_contours(&mut grid, &mut edges, gap_bridge);
    let nodes = grid.nodes;

    let mut adjacency = vec![Vec::<usize>::new(); nodes.len()];
    for (a, b) in &edges {
        if !adjacency[*a].contains(b) {
            adjacency[*a].push(*b);
        }
        if !adjacency[*b].contains(a) {
            adjacency[*b].push(*a);
        }
    }

    let mut visited_edges = HashSet::new();
    let mut rings = Vec::new();
    for (start, next) in edges {
        let edge_key = sorted_usize_pair(start, next);
        if visited_edges.contains(&edge_key) {
            continue;
        }

        let mut ring_nodes = vec![start, next];
        visited_edges.insert(edge_key);
        let mut previous = start;
        let mut current = next;
        let mut closed = false;
        loop {
            if current == start {
                closed = true;
                break;
            }

            let Some(candidate) = adjacency[current]
                .iter()
                .copied()
                .find(|neighbor| *neighbor != previous && !visited_edges.contains(&sorted_usize_pair(current, *neighbor)))
            else {
                break;
            };
            visited_edges.insert(sorted_usize_pair(current, candidate));
            previous = current;
            current = candidate;
            ring_nodes.push(current);
        }

        if ring_nodes.last() == Some(&start) {
            ring_nodes.pop();
        }
        let ring = deduplicate_ring_by(ring_nodes.into_iter().map(|node_index| nodes[node_index]).collect::<Vec<_>>(), |a, b| {
            vertices_close_xy(a, b, tolerance_sq)
        });
        if closed && ring.len() >= 3 && signed_area_xy(&ring).abs() > 1e-8 {
            rings.push(ring);
        }
    }

    if rings.is_empty() {
        anyhow::bail!("Could not form a closed topology cut boundary from the clipped pit/stockpile");
    }
    Ok(rings)
}

/// Extract the true cut boundary between two dead-ends of a broken contour
/// flank by marching squares on the contact against the topology's UPPER
/// envelope (max-z sheet) - the single-valued reference that stays clean where
/// the per-triangle clip drops out over near-coincident doubled survey sheets.
/// The boundary is the zero set of the signed cut depth (topo above design for
/// a pit / design above topo for a stockpile; a surveyed-out hole reads as deep
/// inside, matching the pit-floor-fills-holes rule). Marching over a bbox
/// padded well past the straight chord captures a flank that bulges away from
/// it; the extracted segments are then walked into the single polyline linking
/// the two dead-ends. Returns `None` (caller falls back to a chord) if the
/// field is unusable or no such chain exists.
pub(super) fn trace_contact_contour(
    start: mesh_data::Vertex,
    end: mesh_data::Vertex,
    shape: &mesh_data::Triangulation,
    shape_spatial: &crate::model::spatial::TriangleBvh,
    topology: &mesh_data::Triangulation,
    topology_spatial: &crate::model::spatial::TriangleBvh,
    side: TriSurfaceCutSide,
) -> Option<Vec<mesh_data::Vertex>> {
    use rayon::prelude::*;
    let shape_vertices = shape.vertices();
    let topology_vertices = topology.vertices();
    let extreme = |src: &mesh_data::Triangulation, spatial: &crate::model::spatial::TriangleBvh, verts: &[mesh_data::Vertex], p: glam::DVec2, want_max: bool| -> Option<f64> {
        let mut best = if want_max { f64::NEG_INFINITY } else { f64::INFINITY };
        let mut hit = false;
        let mut stack = Vec::new();
        spatial.for_each_xy_bounds_candidate_index_with_stack(p, p, &mut stack, |index| {
            if let Some(face) = src.face_vertex_indices(index) {
                let triangle = [verts[face[0]], verts[face[1]], verts[face[2]]];
                if let Some(z) = point_in_triangle_bary_z(p.x, p.y, triangle) {
                    hit = true;
                    best = if want_max { best.max(z) } else { best.min(z) };
                }
            }
        });
        hit.then_some(best)
    };
    let shape_max = !matches!(side, TriSurfaceCutSide::CutTop);
    let topo_max = matches!(side, TriSurfaceCutSide::CutTop);
    let contact_z = |p: glam::DVec2| extreme(shape, shape_spatial, shape_vertices, p, shape_max);
    // Signed cut depth: > 0 inside the cut, < 0 outside, 0 on the boundary. No
    // shape under the column => outside (there is no cut without the design);
    // shape present but no topology (surveyed hole) => deep inside.
    let depth = |p: glam::DVec2| -> f64 {
        let Some(shape_z) = extreme(shape, shape_spatial, shape_vertices, p, shape_max) else {
            return -1.0e6;
        };
        match extreme(topology, topology_spatial, topology_vertices, p, topo_max) {
            Some(topo_z) => {
                if topo_max {
                    topo_z - shape_z
                } else {
                    shape_z - topo_z
                }
            }
            None => 1.0e6,
        }
    };

    let start_xy = glam::DVec2::new(start.x, start.y);
    let end_xy = glam::DVec2::new(end.x, end.y);
    let span = start_xy.distance(end_xy);
    if span < 1.0 {
        return None;
    }
    // Pad the dead-end bbox generously: the lost flank can bulge far from the
    // straight chord (it did in the reported pushback), and marching only sees
    // what the window covers.
    let margin = (span * 0.5).max(100.0);
    let min = glam::DVec2::new(start.x.min(end.x), start.y.min(end.y)) - glam::DVec2::splat(margin);
    let max = glam::DVec2::new(start.x.max(end.x), start.y.max(end.y)) + glam::DVec2::splat(margin);
    let extent = max - min;
    // Resolution fine enough to trace a bench-scale crest, capped for cost.
    let mut cell = (span / 300.0).clamp(2.0, 8.0);
    let mut nx = (extent.x / cell).ceil() as usize + 1;
    let mut ny = (extent.y / cell).ceil() as usize + 1;
    while nx.saturating_mul(ny) > 500_000 {
        cell *= 1.5;
        nx = (extent.x / cell).ceil() as usize + 1;
        ny = (extent.y / cell).ceil() as usize + 1;
    }
    if nx < 2 || ny < 2 {
        return None;
    }
    let corner = |ix: usize, iy: usize| glam::DVec2::new(min.x + extent.x * (ix as f64) / ((nx - 1) as f64), min.y + extent.y * (iy as f64) / ((ny - 1) as f64));
    // Sample the depth field at every grid corner (BVH point queries dominate;
    // do them in parallel).
    let depths: Vec<f64> = (0..nx * ny).into_par_iter().map(|k| depth(corner(k % nx, k / nx))).collect();
    let at = |ix: usize, iy: usize| depths[iy * nx + ix];

    // Marching squares: per cell, emit the zero-crossing segment(s).
    let cross = |a: glam::DVec2, da: f64, b: glam::DVec2, db: f64| {
        let t = (da / (da - db)).clamp(0.0, 1.0);
        a + (b - a) * t
    };
    let mut segments: Vec<[glam::DVec2; 2]> = Vec::new();
    for iy in 0..ny - 1 {
        for ix in 0..nx - 1 {
            let p00 = corner(ix, iy);
            let p10 = corner(ix + 1, iy);
            let p11 = corner(ix + 1, iy + 1);
            let p01 = corner(ix, iy + 1);
            let d00 = at(ix, iy);
            let d10 = at(ix + 1, iy);
            let d11 = at(ix + 1, iy + 1);
            let d01 = at(ix, iy + 1);
            // Crossings on the four edges (bottom, right, top, left).
            let mut pts: Vec<glam::DVec2> = Vec::with_capacity(4);
            if (d00 > 0.0) != (d10 > 0.0) {
                pts.push(cross(p00, d00, p10, d10));
            }
            if (d10 > 0.0) != (d11 > 0.0) {
                pts.push(cross(p10, d10, p11, d11));
            }
            if (d11 > 0.0) != (d01 > 0.0) {
                pts.push(cross(p11, d11, p01, d01));
            }
            if (d01 > 0.0) != (d00 > 0.0) {
                pts.push(cross(p01, d01, p00, d00));
            }
            match pts.len() {
                2 => segments.push([pts[0], pts[1]]),
                4 => {
                    // Saddle: connect as two segments (pairing is arbitrary at
                    // this resolution).
                    segments.push([pts[0], pts[1]]);
                    segments.push([pts[2], pts[3]]);
                }
                _ => {}
            }
        }
    }
    if segments.is_empty() {
        return None;
    }

    // Snap crossing endpoints to nodes and walk the chain that links the node
    // nearest `start` to the node nearest `end`.
    let tolerance_sq = (cell * 0.5).powi(2);
    let mut grid = BoundaryNodeGrid::new(tolerance_sq);
    let mut edges: Vec<(usize, usize)> = Vec::with_capacity(segments.len());
    for [a, b] in &segments {
        let ia = grid.index_for(mesh_data::Vertex::new(a.x, a.y, 0.0));
        let ib = grid.index_for(mesh_data::Vertex::new(b.x, b.y, 0.0));
        if ia != ib {
            edges.push(sorted_usize_pair(ia, ib));
        }
    }
    edges.sort_unstable();
    edges.dedup();
    let nodes = &grid.nodes;
    let mut adjacency = vec![Vec::<usize>::new(); nodes.len()];
    for (a, b) in &edges {
        adjacency[*a].push(*b);
        adjacency[*b].push(*a);
    }
    let nearest = |target: glam::DVec2| -> Option<usize> {
        nodes
            .iter()
            .enumerate()
            .filter(|(index, _)| !adjacency[*index].is_empty())
            .min_by(|(_, a), (_, b)| {
                let da = glam::DVec2::new(a.x, a.y).distance_squared(target);
                let db = glam::DVec2::new(b.x, b.y).distance_squared(target);
                da.total_cmp(&db)
            })
            .map(|(index, _)| index)
    };
    let source = nearest(start_xy)?;
    let sink = nearest(end_xy)?;
    if source == sink {
        return None;
    }
    // Greedy walk preferring the neighbour that most continues the current
    // heading; robust for the smooth, mostly degree-2 contact chain.
    let mut path_nodes = vec![source];
    let mut visited = HashSet::new();
    visited.insert(source);
    let mut current = source;
    let mut heading = glam::DVec2::new(nodes[sink].x - nodes[source].x, nodes[sink].y - nodes[source].y).normalize_or_zero();
    while current != sink {
        let here = glam::DVec2::new(nodes[current].x, nodes[current].y);
        let next = adjacency[current].iter().copied().filter(|n| !visited.contains(n)).max_by(|a, b| {
            let da = glam::DVec2::new(nodes[*a].x, nodes[*a].y) - here;
            let db = glam::DVec2::new(nodes[*b].x, nodes[*b].y) - here;
            da.normalize_or_zero().dot(heading).total_cmp(&db.normalize_or_zero().dot(heading))
        });
        let Some(next) = next else {
            return None; // dead-ended before reaching the sink
        };
        let step = glam::DVec2::new(nodes[next].x, nodes[next].y) - here;
        heading = step.normalize_or_zero();
        visited.insert(next);
        path_nodes.push(next);
        current = next;
        if path_nodes.len() > nodes.len() {
            return None;
        }
    }

    let path = path_nodes
        .into_iter()
        .map(|index| {
            let p = glam::DVec2::new(nodes[index].x, nodes[index].y);
            let z = contact_z(p).unwrap_or_else(|| {
                let t = (start_xy.distance(p) / span).clamp(0.0, 1.0);
                start.z + (end.z - start.z) * t
            });
            mesh_data::Vertex::new(p.x, p.y, z)
        })
        .collect();
    Some(path)
}

/// Bridge small gaps in the contact contour by pairing odd-degree (dead-end)
/// nodes with their nearest odd-degree partner.
///
/// Real topologies break the contour in two ways: numeric near-misses just
/// over the node-snap tolerance, and overlapping survey sheets that end
/// partway around the shape - the contact line jumps from one sheet to the
/// other, leaving a gap the size of the sheet separation (metres). Without
/// stitching, one gap discards the entire multi-kilometre contour and the cut
/// silently keeps every topology face. Pairs are joined closest-first and
/// only within a tolerance scaled from the median segment length, so distinct
/// far-apart contours are never fused.
fn stitch_contour_gaps(nodes: &[mesh_data::Vertex], edges: &mut Vec<(usize, usize)>, segments: &[[mesh_data::Vertex; 2]]) {
    let mut degree = vec![0usize; nodes.len()];
    for (a, b) in edges.iter() {
        degree[*a] += 1;
        degree[*b] += 1;
    }
    let open: Vec<usize> = (0..nodes.len()).filter(|&index| degree[index] % 2 == 1).collect();
    if open.is_empty() {
        return;
    }

    let mut lengths: Vec<f64> = segments
        .iter()
        .map(|[a, b]| {
            let dx = a.x - b.x;
            let dy = a.y - b.y;
            let dz = a.z - b.z;
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .collect();
    lengths.sort_unstable_by(f64::total_cmp);
    let median_length = lengths.get(lengths.len() / 2).copied().unwrap_or(0.0);
    let gap_tolerance = (median_length * 32.0).max(1.0e-3);
    let gap_tolerance_sq = gap_tolerance * gap_tolerance;

    let existing: HashSet<(usize, usize)> = edges.iter().copied().collect();
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for i in 0..open.len() {
        for j in (i + 1)..open.len() {
            let a = nodes[open[i]];
            let b = nodes[open[j]];
            let dx = a.x - b.x;
            let dy = a.y - b.y;
            let dz = a.z - b.z;
            let distance_sq = dx * dx + dy * dy + dz * dz;
            if distance_sq <= gap_tolerance_sq && !existing.contains(&sorted_usize_pair(open[i], open[j])) {
                pairs.push((distance_sq, open[i], open[j]));
            }
        }
    }
    pairs.sort_unstable_by(|left, right| left.0.total_cmp(&right.0));

    let mut used = HashSet::new();
    for (_, a, b) in pairs {
        if used.contains(&a) || used.contains(&b) {
            continue;
        }
        used.insert(a);
        used.insert(b);
        edges.push(sorted_usize_pair(a, b));
    }
    edges.sort_unstable();
}

/// Close every large open contour component left after the distance-capped
/// stitch. The stitch bridges numeric near-misses and small sheet-overlap
/// jumps, but a contact running through a large region of overlapping/doubled
/// topology sheets (a pushback cutting into a prior excavation) loses a whole
/// flank - leaving one near-complete loop broken by a single multi-kilometre
/// gap. Discarding it (only closed cycles survive) silently keeps every
/// topology face and cuts nothing, so we always close it: `gap_bridge` traces
/// the real boundary across the gap when it can, otherwise a straight chord
/// (which tracks the true boundary closely only when the flank is near-linear -
/// the tracer is what fixes a bulging flank). Only components with exactly two
/// dead-ends and a substantial share of the contour are closed, so a stray
/// short arc is never fused shut.
fn bridge_or_close_open_contours(grid: &mut BoundaryNodeGrid, edges: &mut Vec<(usize, usize)>, gap_bridge: Option<GapBridge<'_>>) {
    if edges.is_empty() {
        return;
    }
    let node_count = grid.nodes.len();
    let mut parent: Vec<usize> = (0..node_count).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        let mut cursor = x;
        while parent[cursor] != root {
            let next = parent[cursor];
            parent[cursor] = root;
            cursor = next;
        }
        root
    }
    let mut degree = vec![0usize; node_count];
    for (a, b) in edges.iter() {
        let ra = find(&mut parent, *a);
        let rb = find(&mut parent, *b);
        if ra != rb {
            parent[ra] = rb;
        }
        degree[*a] += 1;
        degree[*b] += 1;
    }
    let mut component_edges: HashMap<usize, usize> = HashMap::new();
    for (a, _) in edges.iter() {
        *component_edges.entry(find(&mut parent, *a)).or_insert(0) += 1;
    }
    // Dead-ends (odd degree) grouped by component.
    let mut component_ends: HashMap<usize, Vec<usize>> = HashMap::new();
    for (node, &node_degree) in degree.iter().enumerate() {
        if node_degree % 2 == 1 {
            let root = find(&mut parent, node);
            component_ends.entry(root).or_default().push(node);
        }
    }
    let threshold = (edges.len() / 20).max(64);
    let mut added = false;
    for (component, ends) in component_ends {
        if ends.len() != 2 || component_edges.get(&component).copied().unwrap_or(0) < threshold {
            continue;
        }
        let (start, end) = (ends[0], ends[1]);
        // Prefer a traced boundary; each interior point snaps to a (new) node,
        // and the path's own end points snap back onto `start`/`end`.
        if let Some(path) = gap_bridge.and_then(|bridge| bridge(grid.nodes[start], grid.nodes[end])) {
            let mut previous = start;
            for point in path {
                let index = grid.index_for(point);
                if index != previous {
                    edges.push(sorted_usize_pair(previous, index));
                    previous = index;
                }
            }
            if previous != end {
                edges.push(sorted_usize_pair(previous, end));
            }
            added = true;
            continue;
        }
        edges.push(sorted_usize_pair(start, end));
        added = true;
    }
    if added {
        edges.sort_unstable();
    }
}

/// Snaps boundary vertices to node indices through a uniform hash grid so
/// lookups stay near-constant regardless of how many segments the cut
/// produced (a linear scan here goes quadratic on dense topologies).
///
/// Matching is 3D: topologies with overlapping sheets (resurveys, tile
/// overlap) yield one contact contour per sheet at nearly the same XY but
/// different z; merging those by XY tangles the contours into one graph and
/// no ring closes. Kept apart, each contour closes or is discarded on its own.
pub(super) struct BoundaryNodeGrid {
    nodes: Vec<mesh_data::Vertex>,
    cells: HashMap<(i64, i64), Vec<usize>>,
    cell_size: f64,
    tolerance_sq: f64,
}

impl BoundaryNodeGrid {
    pub(super) fn new(tolerance_sq: f64) -> Self {
        Self {
            nodes: Vec::new(),
            cells: HashMap::new(),
            cell_size: tolerance_sq.sqrt(),
            tolerance_sq,
        }
    }

    pub(super) fn index_for(&mut self, vertex: mesh_data::Vertex) -> usize {
        let cell_x = (vertex.x / self.cell_size).floor() as i64;
        let cell_y = (vertex.y / self.cell_size).floor() as i64;
        // The cell size equals the snap tolerance, so any node within
        // tolerance of `vertex` lives in the 3x3 cell neighbourhood.
        for dx in -1i64..=1 {
            for dy in -1i64..=1 {
                let Some(bucket) = self.cells.get(&(cell_x + dx, cell_y + dy)) else {
                    continue;
                };
                for &index in bucket {
                    if vertices_close_xyz(self.nodes[index], vertex, self.tolerance_sq) {
                        return index;
                    }
                }
            }
        }
        let index = self.nodes.len();
        self.nodes.push(vertex);
        self.cells.entry((cell_x, cell_y)).or_default().push(index);
        index
    }
}

pub(super) fn sorted_usize_pair(a: usize, b: usize) -> (usize, usize) {
    if a < b { (a, b) } else { (b, a) }
}

pub(super) fn vertices_close_xy(a: mesh_data::Vertex, b: mesh_data::Vertex, tolerance_sq: f64) -> bool {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy <= tolerance_sq
}

pub(super) fn vertices_close_xyz(a: mesh_data::Vertex, b: mesh_data::Vertex, tolerance_sq: f64) -> bool {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz <= tolerance_sq
}

pub(super) struct ClippedShapeMesh {
    /// Pieces of xy-degenerate (vertical) shape faces; surface-face geometry
    /// is produced separately by [`clip_shape_faces_by_cells`].
    vertices: Vec<mesh_data::Vertex>,
    faces: Vec<[u32; 3]>,
    topology_segments: Vec<[mesh_data::Vertex; 2]>,
}

pub(super) struct ShapeClipContext<'a> {
    topology: &'a mesh_data::Triangulation,
    topology_spatial: &'a crate::model::spatial::TriangleBvh,
    xy_area_tolerance: f64,
    side: TriSurfaceCutSide,
    /// Emit clipped surface pieces (one per covering topology triangle).
    /// Off for the whole-shape contact pass, on for the exact sub-clip of
    /// coverage-cell pieces the contact ring actually crosses.
    emit_surface_pieces: bool,
}

#[derive(Default)]
pub(super) struct ShapeClipOutput {
    /// Pieces of xy-degenerate (vertical) shape faces only; surface faces are
    /// re-emitted later against the coarse cell coverage instead.
    vertices: Vec<mesh_data::Vertex>,
    faces: Vec<[u32; 3]>,
    topology_segments: Vec<[mesh_data::Vertex; 2]>,
    /// Whether any surface face had geometry on the retained side.
    surface_retained: bool,
}

pub(super) fn clip_shape_to_topology(
    shape_vertices: &[mesh_data::Vertex],
    shape_faces: &[[usize; 3]],
    cap_mask: &[bool],
    topology: &mesh_data::Triangulation,
    topology_spatial: &crate::model::spatial::TriangleBvh,
    xy_area_tolerance: f64,
    side: TriSurfaceCutSide,
) -> Result<ClippedShapeMesh> {
    use rayon::prelude::*;

    let context = ShapeClipContext {
        topology,
        topology_spatial,
        xy_area_tolerance,
        side,
        emit_surface_pieces: false,
    };
    let task_count = rayon::current_num_threads().saturating_mul(4).max(1);
    let chunk_size = shape_faces.len().div_ceil(task_count).max(1);
    let partials = shape_faces
        .par_chunks(chunk_size)
        .enumerate()
        .map(|(chunk_index, faces)| {
            let mut output = ShapeClipOutput::default();
            let mut candidate_stack = Vec::new();
            let face_index_base = chunk_index * chunk_size;

            for (offset, face) in faces.iter().copied().enumerate() {
                let face_index = face_index_base + offset;
                if cap_mask.get(face_index).copied().unwrap_or(false) {
                    continue;
                }

                let triangle = [shape_vertices[face[0]], shape_vertices[face[1]], shape_vertices[face[2]]];
                if triangle_xy_area(triangle).abs() <= 1.0e-10 {
                    let vertices_start = output.vertices.len();
                    let faces_start = output.faces.len();
                    clip_vertical_shape_triangle_to_topology(triangle, &context, &mut output, &mut candidate_stack);
                    collapse_fully_retained_face(triangle, vertices_start, faces_start, &mut output.vertices, &mut output.faces);
                } else {
                    clip_surface_shape_triangle_to_topology(triangle, &context, &mut output, &mut candidate_stack);
                }
            }

            output
        })
        .collect::<Vec<_>>();

    let mut output = ShapeClipOutput {
        vertices: Vec::with_capacity(partials.iter().map(|partial| partial.vertices.len()).sum()),
        faces: Vec::with_capacity(partials.iter().map(|partial| partial.faces.len()).sum()),
        topology_segments: Vec::with_capacity(partials.iter().map(|partial| partial.topology_segments.len()).sum()),
        surface_retained: false,
    };
    for partial in partials {
        append_shape_clip_output(&mut output, partial);
    }

    if output.faces.is_empty() && !output.surface_retained {
        anyhow::bail!("Pit/stockpile solid has no geometry on the retained side of the topology");
    }
    if output.topology_segments.is_empty() {
        anyhow::bail!("Pit/stockpile solid did not intersect the topology surface");
    }

    Ok(ClippedShapeMesh {
        vertices: output.vertices,
        faces: output.faces,
        topology_segments: output.topology_segments,
    })
}

/// Surface-face geometry of the shape, emitted against the coarse cell
/// coverage instead of one piece per covering topology triangle (which put
/// the whole pit floor at LiDAR resolution - millions of slivers for a
/// few-hundred-face design). Within a cell the footprint membership and
/// topology presence are uniform, and the shape-vs-topology sign can only
/// flip across a contact ring edge (a cell constraint), so height samples at
/// the piece corners decide almost every piece whole; only pieces the contact
/// actually crosses (stitched ring gaps, snap tolerance) fall back to the
/// exact per-topology-triangle clip. Fully retained faces collapse back to
/// their original design tessellation.
fn clip_shape_faces_by_cells(
    shape_vertices: &[mesh_data::Vertex],
    shape_faces: &[[usize; 3]],
    cap_mask: &[bool],
    coverage: &ShapeCellCoverage,
    context: &ShapeClipContext<'_>,
) -> (Vec<mesh_data::Vertex>, Vec<[u32; 3]>) {
    use rayon::prelude::*;

    let retained = |delta: f64| match context.side {
        TriSurfaceCutSide::CutTop => delta <= 1e-9,
        TriSurfaceCutSide::CutBottom => delta >= -1e-9,
    };

    let task_count = rayon::current_num_threads().saturating_mul(4).max(1);
    let chunk_size = shape_faces.len().div_ceil(task_count).max(1);
    let partials: Vec<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)> = shape_faces
        .par_chunks(chunk_size)
        .enumerate()
        .map(|(chunk_index, faces)| {
            let mut vertices = Vec::new();
            let mut output_faces = Vec::new();
            let mut cell_stack = Vec::new();
            let mut topology_stack = Vec::new();
            let mut candidates: Vec<usize> = Vec::new();
            let face_index_base = chunk_index * chunk_size;

            for (offset, face) in faces.iter().enumerate() {
                if cap_mask.get(face_index_base + offset).copied().unwrap_or(false) {
                    continue;
                }
                let triangle = [shape_vertices[face[0]], shape_vertices[face[1]], shape_vertices[face[2]]];
                if triangle_xy_area(triangle).abs() <= 1.0e-10 {
                    // Vertical faces have no usable XY footprint; the contact
                    // pass already emitted their pieces.
                    continue;
                }
                let vertices_start = vertices.len();
                let faces_start = output_faces.len();
                let bounds = triangle_xy_bounds(triangle);
                candidates.clear();
                coverage
                    .spatial
                    .for_each_xy_bounds_candidate_index_with_stack(bounds.0, bounds.1, &mut cell_stack, |index| candidates.push(index));
                for &index in &candidates {
                    let piece = clip_target_triangle_to_reference_xy(triangle, coverage.triangles[index]);
                    if piece.len() < 3 {
                        continue;
                    }
                    if coverage.covered[index] {
                        // Inside the cut footprint the shape *is* the design
                        // surface and replaces the topology unconditionally.
                        // A local topo dip that drops the topology below the
                        // shell must not punch a hole through the continuous
                        // pit/stockpile surface - the shell fills the dip, it
                        // does not expose it. (Surveyed-out holes - covered
                        // cells with no topology - are handled identically.)
                        append_shape_polyline(&piece, &mut vertices, &mut output_faces);
                        continue;
                    }
                    if !coverage.present[index] {
                        // Outside the footprint with no topology: nothing to
                        // keep. Don't let the shape hang past the topology's
                        // rim.
                        continue;
                    }
                    let centroid = piece.iter().fold(glam::DVec3::ZERO, |sum, point| sum + *point) / piece.len() as f64;
                    let mut any_retained = false;
                    let mut any_discarded = false;
                    for point in piece.iter().chain(std::iter::once(&centroid)) {
                        let Some(delta) = worst_height_delta(*point, context, &mut topology_stack) else {
                            continue;
                        };
                        if retained(delta) {
                            any_retained = true;
                        } else {
                            any_discarded = true;
                        }
                    }
                    if !any_retained {
                        continue;
                    }
                    if !any_discarded {
                        append_shape_polyline(&piece, &mut vertices, &mut output_faces);
                        continue;
                    }
                    // The contact crosses this piece (it can wander off the
                    // ring edges by the snap tolerance and across stitched
                    // gaps): resolve it exactly against the topology.
                    let mut scratch = ShapeClipOutput::default();
                    for i in 1..piece.len() - 1 {
                        let sub_triangle = [
                            mesh_data::Vertex::new(piece[0].x, piece[0].y, piece[0].z),
                            mesh_data::Vertex::new(piece[i].x, piece[i].y, piece[i].z),
                            mesh_data::Vertex::new(piece[i + 1].x, piece[i + 1].y, piece[i + 1].z),
                        ];
                        if triangle_xy_area(sub_triangle).abs() <= 1.0e-10 {
                            continue;
                        }
                        clip_surface_shape_triangle_to_topology(sub_triangle, context, &mut scratch, &mut topology_stack);
                    }
                    let base = vertices.len() as u32;
                    vertices.extend(scratch.vertices);
                    output_faces.extend(scratch.faces.into_iter().map(|face| [base + face[0], base + face[1], base + face[2]]));
                }
                collapse_fully_retained_face(triangle, vertices_start, faces_start, &mut vertices, &mut output_faces);
            }
            (vertices, output_faces)
        })
        .collect();

    let mut vertices = Vec::with_capacity(partials.iter().map(|(v, _)| v.len()).sum());
    let mut faces = Vec::with_capacity(partials.iter().map(|(_, f)| f.len()).sum());
    for (partial_vertices, partial_faces) in partials {
        let base = vertices.len() as u32;
        vertices.extend(partial_vertices);
        faces.extend(partial_faces.into_iter().map(|face| [base + face[0], base + face[1], base + face[2]]));
    }
    (vertices, faces)
}

/// The shape-vs-topology height difference at `point`, folded to the worst
/// case across every covering topology sheet for the cut side - a piece
/// counts as retained only when it is on the retained side of ALL sheets, so
/// doubled survey sheets don't duplicate it. `None` when no topology covers
/// the point.
fn worst_height_delta(point: glam::DVec3, context: &ShapeClipContext<'_>, candidate_stack: &mut Vec<usize>) -> Option<f64> {
    let xy = glam::DVec2::new(point.x, point.y);
    let mut worst: Option<f64> = None;
    context
        .topology_spatial
        .for_each_xy_bounds_candidate_index_with_stack(xy, xy, candidate_stack, |topology_index| {
            let Some(reference) = reference_triangle_for_topology_index(context, topology_index) else {
                return;
            };
            let Some(reference_z) = point_in_triangle_bary_z(point.x, point.y, reference) else {
                return;
            };
            let delta = point.z - reference_z;
            worst = Some(match worst {
                None => delta,
                Some(current) => match context.side {
                    TriSurfaceCutSide::CutTop => current.max(delta),
                    TriSurfaceCutSide::CutBottom => current.min(delta),
                },
            });
        });
    worst
}

/// Keep only the candidate boundary edges that actually border missing
/// topology in XY: no covering face a hair to one side of the edge midpoint.
///
/// Meshes produced by earlier includes are index-disconnected along fragment
/// seams, and fragments subdivide shared edges at cell crossings, so the
/// index parity in [`topology_boundary_edges`] reports every hairline
/// interior crack and T-junction of the previous contact band as "boundary".
/// Topology presence does not flip across a zero-width crack, so as CDT
/// constraints those edges are pure noise - tens of thousands of
/// near-coincident segments along the old contact ring that drive the
/// constraint-splitting insert into pathological splitting (the re-include
/// hang) and spade's internal assert. Probing both sides keeps true rims
/// (hole and outer) and drops everything interior.
fn filter_presence_boundary_edges(edges: Vec<[usize; 2]>, topology: &mesh_data::Triangulation, topology_spatial: &crate::model::spatial::TriangleBvh) -> Vec<[usize; 2]> {
    use rayon::prelude::*;

    // Wider than any seam crack (snap tolerances bound those to ~1e-4 m),
    // far narrower than a real surveyed-out hole.
    const PROBE_OFFSET: f64 = 1.0e-3;

    let vertices = topology.vertices();
    edges
        .into_par_iter()
        .map_init(Vec::new, |candidate_stack, edge| {
            let a = vertices[edge[0]];
            let b = vertices[edge[1]];
            let direction = glam::DVec2::new(b.x - a.x, b.y - a.y);
            let length = direction.length();
            if length <= f64::EPSILON {
                return None;
            }
            let normal = glam::DVec2::new(-direction.y, direction.x) * (PROBE_OFFSET / length);
            let midpoint = glam::DVec2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
            let mut covered = |probe: glam::DVec2| {
                let mut hit = false;
                topology_spatial.for_each_xy_bounds_candidate_index_with_stack(probe, probe, candidate_stack, |face_index| {
                    if hit {
                        return;
                    }
                    let Some(face) = topology.face_vertex_indices(face_index) else {
                        return;
                    };
                    let triangle = [vertices[face[0]], vertices[face[1]], vertices[face[2]]];
                    if point_in_triangle_bary_z(probe.x, probe.y, triangle).is_some() {
                        hit = true;
                    }
                });
                hit
            };
            (!(covered(midpoint + normal) && covered(midpoint - normal))).then_some(edge)
        })
        .flatten()
        .collect()
}

/// Edges used by exactly one topology face: the rims of holes and of the
/// surface itself.
fn topology_boundary_edges(topology: &mesh_data::Triangulation) -> Vec<[usize; 2]> {
    use rayon::prelude::*;

    let mut keys: Vec<u64> = Vec::with_capacity(topology.face_count() * 3);
    for face in topology.face_vertex_indices_iter() {
        for (a, b) in [(face[0], face[1]), (face[1], face[2]), (face[2], face[0])] {
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            keys.push(((lo as u64) << 32) | hi as u64);
        }
    }
    keys.par_sort_unstable();

    let mut edges = Vec::new();
    let mut index = 0;
    while index < keys.len() {
        let key = keys[index];
        let mut run = 1;
        while index + run < keys.len() && keys[index + run] == key {
            run += 1;
        }
        if run == 1 {
            edges.push([(key >> 32) as usize, (key & 0xffff_ffff) as usize]);
        }
        index += run;
    }
    edges
}

/// Partition the plane by the topology's boundary edges (same CDT scheme as
/// [`build_ring_coverage`]) and classify each cell by whether the topology
/// covers it - presence only changes across a boundary edge, so a single
/// point query per cell is exact.
/// The plane partitioned by a CDT constrained on BOTH discontinuity sets the
/// shape emission cares about: the contact ring edges (where the shape crosses
/// the topology, so the retained side flips) and the topology boundary edges
/// (hole rims and the outer rim, where topology presence flips). Each cell is
/// therefore uniform in footprint membership and in topology presence, and
/// the shape-vs-topology height sign is constant per shape face within it.
pub(super) struct ShapeCellCoverage {
    triangles: Vec<[mesh_data::Vertex; 3]>,
    /// Inside the cut footprint (inside any contact ring).
    covered: Vec<bool>,
    /// Topology exists under the cell.
    present: Vec<bool>,
    spatial: crate::model::spatial::TriangleBvh,
}

fn build_shape_cell_coverage(
    rings: &[Vec<mesh_data::Vertex>],
    topology: &mesh_data::Triangulation,
    topology_spatial: &crate::model::spatial::TriangleBvh,
    boundary_edges: &[[usize; 2]],
) -> Result<ShapeCellCoverage> {
    use rayon::prelude::*;
    use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation as _};

    let topology_vertices = topology.vertices();
    let mut min = glam::DVec2::splat(f64::INFINITY);
    let mut max = glam::DVec2::splat(f64::NEG_INFINITY);
    for edge in boundary_edges {
        for &vertex_index in edge {
            let vertex = topology_vertices[vertex_index];
            if !vertex.x.is_finite() || !vertex.y.is_finite() {
                anyhow::bail!("Topology boundary contains non-finite coordinates");
            }
            min = min.min(glam::DVec2::new(vertex.x, vertex.y));
            max = max.max(glam::DVec2::new(vertex.x, vertex.y));
        }
    }
    for ring in rings {
        for vertex in ring {
            min = min.min(glam::DVec2::new(vertex.x, vertex.y));
            max = max.max(glam::DVec2::new(vertex.x, vertex.y));
        }
    }
    if !min.x.is_finite() {
        anyhow::bail!("Shape cell coverage has no constraint geometry");
    }
    let origin = min;
    let extent = max - min;

    let normalized = |value: f64| if value == 0.0 { 0.0 } else { value };
    let padding = cuts::ENVELOPE_PADDING;
    let mut points: Vec<Point2<f64>> = Vec::new();
    for (corner_x, corner_y) in [
        (-padding, -padding),
        (extent.x + padding, -padding),
        (extent.x + padding, extent.y + padding),
        (-padding, extent.y + padding),
    ] {
        points.push(Point2::new(corner_x, corner_y));
    }
    let mut point_indices: HashMap<(u64, u64), usize> = HashMap::new();
    let mut point_for = |vertex: mesh_data::Vertex, points: &mut Vec<Point2<f64>>| {
        let local_x = normalized(vertex.x - origin.x);
        let local_y = normalized(vertex.y - origin.y);
        *point_indices.entry((local_x.to_bits(), local_y.to_bits())).or_insert_with(|| {
            points.push(Point2::new(local_x, local_y));
            points.len() - 1
        })
    };
    let mut edges: HashSet<(usize, usize)> = HashSet::new();
    for edge in boundary_edges {
        let a = point_for(topology_vertices[edge[0]], &mut points);
        let b = point_for(topology_vertices[edge[1]], &mut points);
        if a != b {
            edges.insert(sorted_usize_pair(a, b));
        }
    }
    for ring in rings {
        for index in 0..ring.len() {
            let a = point_for(ring[index], &mut points);
            let b = point_for(ring[(index + 1) % ring.len()], &mut points);
            if a != b {
                edges.insert(sorted_usize_pair(a, b));
            }
        }
    }
    let mut edge_list: Vec<[usize; 2]> = edges.into_iter().map(|(a, b)| [a, b]).collect();
    edge_list.sort_unstable();

    let point_count = points.len();
    let mut conflicting_edges: Vec<[usize; 2]> = Vec::new();
    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::try_bulk_load_cdt(points, edge_list, |edge| conflicting_edges.push(edge))
        .map_err(|error| anyhow::anyhow!("Boundary CDT bulk load failed: {error:?}"))?;
    if cdt.num_vertices() != point_count {
        anyhow::bail!("Boundary CDT dropped vertices unexpectedly during bulk load");
    }
    cuts::add_split_constraints(&mut cdt, conflicting_edges, "Shape cell coverage", origin);

    let cells: Vec<[glam::DVec2; 3]> = cdt
        .inner_faces()
        .map(|face| {
            face.vertices().map(|vertex| {
                let position = vertex.position();
                glam::DVec2::new(position.x + origin.x, position.y + origin.y)
            })
        })
        .collect();
    let pips: Vec<RingPip> = rings.iter().map(|ring| RingPip::build(ring)).collect();
    let classified: Vec<([mesh_data::Vertex; 3], bool, bool)> = cells
        .par_iter()
        .map_init(Vec::new, |candidate_stack, corners| {
            let cell = corners.map(|corner| mesh_data::Vertex::new(corner.x, corner.y, 0.0));
            if triangle_xy_area(cell).abs() <= 1e-12 {
                return None;
            }
            let centroid = (corners[0] + corners[1] + corners[2]) / 3.0;
            let covered = pips.iter().any(|pip| pip.contains(centroid));
            let mut present = false;
            topology_spatial.for_each_xy_bounds_candidate_index_with_stack(centroid, centroid, candidate_stack, |face_index| {
                if present {
                    return;
                }
                let Some(face) = topology.face_vertex_indices(face_index) else {
                    return;
                };
                let triangle = [topology_vertices[face[0]], topology_vertices[face[1]], topology_vertices[face[2]]];
                if point_in_triangle_bary_z(centroid.x, centroid.y, triangle).is_some() {
                    present = true;
                }
            });
            Some((cell, covered, present))
        })
        .filter_map(|classified_cell| classified_cell)
        .collect();

    let mut vertices = Vec::with_capacity(classified.len() * 3);
    let mut faces = Vec::with_capacity(classified.len());
    let mut triangles = Vec::with_capacity(classified.len());
    let mut covered = Vec::with_capacity(classified.len());
    let mut present = Vec::with_capacity(classified.len());
    for (cell, is_covered, is_present) in classified {
        let base = vertices.len() as u32;
        vertices.extend_from_slice(&cell);
        faces.push([base, base + 1, base + 2]);
        triangles.push(cell);
        covered.push(is_covered);
        present.push(is_present);
    }
    let mesh = mesh_data::Triangulation::from_vertices_and_faces(vertices, faces)?;
    let spatial = crate::model::spatial::TriangleBvh::build(&mesh);
    Ok(ShapeCellCoverage {
        triangles,
        covered,
        present,
        spatial,
    })
}

/// If the clip kept the shape face in its entirety, replace the per-topology-
/// triangle fragments with the original face. The clip emits one piece per
/// covering topology triangle, so a face wholly on the retained side comes
/// back as thousands of coplanar slivers that tile it exactly; detecting that
/// by area lets interior floor/bench/wall faces keep their design tessellation
/// and only faces crossing the contact ring (or a topology hole rim) stay
/// fragmented.
fn collapse_fully_retained_face(triangle: [mesh_data::Vertex; 3], vertices_start: usize, faces_start: usize, vertices: &mut Vec<mesh_data::Vertex>, faces: &mut Vec<[u32; 3]>) {
    if faces.len() == faces_start {
        return;
    }
    let face_area = triangle_area_3d(triangle);
    if face_area <= 1.0e-12 {
        return;
    }
    let pieces_area: f64 = faces[faces_start..]
        .iter()
        .map(|face| triangle_area_3d([vertices[face[0] as usize], vertices[face[1] as usize], vertices[face[2] as usize]]))
        .sum();
    // Degenerate slivers are dropped at append time, so a fully retained face
    // can come up marginally short; a genuine cut at the contact ring removes
    // macroscopic area. Overlapping-sheet topologies emit doubled pieces and
    // overshoot instead, which also fails the check and stays fragmented.
    if (pieces_area - face_area).abs() > face_area * 1.0e-7 {
        return;
    }
    vertices.truncate(vertices_start);
    faces.truncate(faces_start);
    let base = vertices_start as u32;
    vertices.extend(triangle);
    faces.push([base, base + 1, base + 2]);
}

fn append_shape_clip_output(output: &mut ShapeClipOutput, partial: ShapeClipOutput) {
    let base = output.vertices.len() as u32;
    output.vertices.extend(partial.vertices);
    output.faces.extend(partial.faces.into_iter().map(|face| [base + face[0], base + face[1], base + face[2]]));
    output.topology_segments.extend(partial.topology_segments);
    output.surface_retained |= partial.surface_retained;
}

fn reference_triangle_for_topology_index(context: &ShapeClipContext<'_>, topology_index: usize) -> Option<[mesh_data::Vertex; 3]> {
    let face = context.topology.face_vertex_indices(topology_index)?;
    let vertices = context.topology.vertices();
    let triangle = [vertices[face[0]], vertices[face[1]], vertices[face[2]]];
    (triangle_xy_area(triangle).abs() > context.xy_area_tolerance).then_some(triangle)
}

pub(super) fn clip_surface_shape_triangle_to_topology(
    triangle: [mesh_data::Vertex; 3],
    context: &ShapeClipContext<'_>,
    output: &mut ShapeClipOutput,
    candidate_stack: &mut Vec<usize>,
) {
    let bounds = triangle_xy_bounds(triangle);
    context
        .topology_spatial
        .for_each_xy_bounds_candidate_index_with_stack(bounds.0, bounds.1, candidate_stack, |topology_index| {
            let Some(reference) = reference_triangle_for_topology_index(context, topology_index) else {
                return;
            };
            let overlap = clip_target_triangle_to_reference_xy(triangle, reference);
            if overlap.len() < 3 {
                return;
            }

            let polyline = overlap
                .into_iter()
                .map(|point| {
                    let reference_z = bary_z(point.x, point.y, reference);
                    SurfaceClipVertex {
                        point,
                        height_delta: point.z - reference_z,
                    }
                })
                .collect::<Vec<_>>();
            let clipped = clip_surface_polyline(polyline, context.side);
            collect_topology_segments(&clipped, &mut output.topology_segments);
            // In the contact pass geometry is not emitted: one piece per
            // covering topology triangle would put the whole pit floor at
            // LiDAR resolution. Surface faces are re-emitted against the
            // coarse cell coverage once the contact rings are known.
            if context.emit_surface_pieces {
                append_surface_clip_polyline(&clipped, &mut output.vertices, &mut output.faces);
            } else if clipped.len() >= 3 {
                output.surface_retained = true;
            }
        });
}

fn append_shape_polyline(polyline: &[glam::DVec3], vertices: &mut Vec<mesh_data::Vertex>, faces: &mut Vec<[u32; 3]>) {
    if polyline.len() < 3 {
        return;
    }
    let base = vertices.len() as u32;
    vertices.extend(polyline.iter().map(|point| mesh_data::Vertex::new(point.x, point.y, point.z)));
    for i in 1..polyline.len() - 1 {
        let face = [base, base + i as u32, base + i as u32 + 1];
        let a = vertices[face[0] as usize];
        let b = vertices[face[1] as usize];
        let c = vertices[face[2] as usize];
        let ab = glam::DVec3::new(b.x - a.x, b.y - a.y, b.z - a.z);
        let ac = glam::DVec3::new(c.x - a.x, c.y - a.y, c.z - a.z);
        if ab.cross(ac).length_squared() > 1e-20 {
            faces.push(face);
        }
    }
}

pub(super) fn clip_vertical_shape_triangle_to_topology(
    triangle: [mesh_data::Vertex; 3],
    context: &ShapeClipContext<'_>,
    output: &mut ShapeClipOutput,
    candidate_stack: &mut Vec<usize>,
) {
    let Some((origin, axis, axis_len_sq)) = vertical_triangle_axis(triangle) else {
        return;
    };
    let segment_min = origin.min(origin + axis);
    let segment_max = origin.max(origin + axis);

    #[derive(Clone, Copy)]
    struct VerticalReference {
        t_min: f64,
        t_max: f64,
        triangle: [mesh_data::Vertex; 3],
    }

    let mut references = Vec::new();
    context
        .topology_spatial
        .for_each_xy_bounds_candidate_index_with_stack(segment_min, segment_max, candidate_stack, |topology_index| {
            let Some(reference) = reference_triangle_for_topology_index(context, topology_index) else {
                return;
            };
            let Some((t_min, t_max)) = segment_triangle_interval_xy(origin, axis, reference) else {
                return;
            };
            if t_max - t_min > 1.0e-10 {
                references.push(VerticalReference {
                    t_min,
                    t_max,
                    triangle: reference,
                });
            }
        });
    if references.is_empty() {
        return;
    }

    // Sweep the projected wall segment. Within each interval the set of
    // covering topology faces is constant; choose their cut-side envelope
    // instead of emitting one coincident wall fragment per face. Pairwise line
    // crossings split intervals where the governing sheet changes.
    let mut events = Vec::with_capacity(references.len() * 2);
    for (index, reference) in references.iter().enumerate() {
        events.push((reference.t_min, true, index));
        events.push((reference.t_max, false, index));
    }
    events.sort_unstable_by(|left, right| left.0.total_cmp(&right.0));
    let mut active = vec![false; references.len()];
    let vertical_polyline = triangle
        .into_iter()
        .map(|vertex| VerticalClipVertex {
            t: vertical_t(origin, axis, axis_len_sq, vertex),
            z: vertex.z,
        })
        .collect::<Vec<_>>();
    let reference_z = |reference: VerticalReference, t: f64| {
        let xy = origin + axis * t;
        bary_z(xy.x, xy.y, reference.triangle)
    };

    let mut event_cursor = 0usize;
    let mut interval_start = events[0].0;
    while event_cursor < events.len() {
        let event_t = events[event_cursor].0;
        if event_t - interval_start > 1.0e-10 {
            let active_indices: Vec<usize> = active.iter().enumerate().filter_map(|(index, is_active)| is_active.then_some(index)).collect();
            if !active_indices.is_empty() {
                let mut breaks = vec![interval_start, event_t];
                for (position, &left_index) in active_indices.iter().enumerate() {
                    for &right_index in &active_indices[position + 1..] {
                        let left = references[left_index];
                        let right = references[right_index];
                        let delta_start = reference_z(left, interval_start) - reference_z(right, interval_start);
                        let delta_end = reference_z(left, event_t) - reference_z(right, event_t);
                        let denominator = delta_start - delta_end;
                        if denominator.abs() <= 1.0e-20 {
                            continue;
                        }
                        let t = interval_start + (event_t - interval_start) * delta_start / denominator;
                        if t > interval_start + 1.0e-10 && t < event_t - 1.0e-10 {
                            breaks.push(t);
                        }
                    }
                }
                breaks.sort_unstable_by(f64::total_cmp);
                breaks.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-10);

                for limits in breaks.windows(2) {
                    let t_min = limits[0];
                    let t_max = limits[1];
                    if t_max - t_min <= 1.0e-10 {
                        continue;
                    }
                    let midpoint = (t_min + t_max) * 0.5;
                    let governing_index = active_indices
                        .iter()
                        .copied()
                        .min_by(|left, right| {
                            let left_z = reference_z(references[*left], midpoint);
                            let right_z = reference_z(references[*right], midpoint);
                            match context.side {
                                TriSurfaceCutSide::CutTop => left_z.total_cmp(&right_z),
                                TriSurfaceCutSide::CutBottom => right_z.total_cmp(&left_z),
                            }
                        })
                        .expect("active topology interval is non-empty");
                    let governing = references[governing_index];

                    let polyline = clip_vertical_polyline_t_min(&vertical_polyline, t_min);
                    let polyline = clip_vertical_polyline_t_max(&polyline, t_max);
                    if polyline.len() < 3 {
                        continue;
                    }
                    let surface_polyline = polyline
                        .into_iter()
                        .map(|vertex| {
                            let xy = origin + axis * vertex.t;
                            let topology_z = reference_z(governing, vertex.t);
                            SurfaceClipVertex {
                                point: glam::DVec3::new(xy.x, xy.y, vertex.z),
                                height_delta: vertex.z - topology_z,
                            }
                        })
                        .collect::<Vec<_>>();
                    let clipped = clip_surface_polyline(surface_polyline, context.side);
                    collect_topology_segments(&clipped, &mut output.topology_segments);
                    append_surface_clip_polyline(&clipped, &mut output.vertices, &mut output.faces);
                }
            }
        }

        while event_cursor < events.len() && (events[event_cursor].0 - event_t).abs() <= 1.0e-12 {
            let (_, starts, reference_index) = events[event_cursor];
            active[reference_index] = starts;
            event_cursor += 1;
        }
        interval_start = event_t;
    }
}

#[derive(Clone, Copy)]
pub(super) struct VerticalClipVertex {
    t: f64,
    z: f64,
}

pub(super) fn vertical_triangle_axis(triangle: [mesh_data::Vertex; 3]) -> Option<(glam::DVec2, glam::DVec2, f64)> {
    let points = triangle.map(|vertex| glam::DVec2::new(vertex.x, vertex.y));
    let pairs = [(0, 1), (0, 2), (1, 2)];
    let (a, b, axis_len_sq) = pairs
        .into_iter()
        .map(|(a, b)| (a, b, points[a].distance_squared(points[b])))
        .max_by(|left, right| left.2.total_cmp(&right.2))?;
    if axis_len_sq <= 1.0e-16 {
        return None;
    }
    Some((points[a], points[b] - points[a], axis_len_sq))
}

pub(super) fn vertical_t(origin: glam::DVec2, axis: glam::DVec2, axis_len_sq: f64, vertex: mesh_data::Vertex) -> f64 {
    (glam::DVec2::new(vertex.x, vertex.y) - origin).dot(axis) / axis_len_sq
}

pub(super) fn segment_triangle_interval_xy(origin: glam::DVec2, axis: glam::DVec2, triangle: [mesh_data::Vertex; 3]) -> Option<(f64, f64)> {
    let points = triangle.map(|vertex| glam::DVec2::new(vertex.x, vertex.y));
    let clip_ccw = triangle_xy_area(triangle) > 0.0;
    let mut t_min: f64 = 0.0;
    let mut t_max: f64 = 1.0;

    for edge_index in 0..3 {
        let edge_a = points[edge_index];
        let edge_b = points[(edge_index + 1) % 3];
        let edge = edge_b - edge_a;
        let signed_at = |t: f64| {
            let point = origin + axis * t;
            let cross = edge.x * (point.y - edge_a.y) - edge.y * (point.x - edge_a.x);
            if clip_ccw { cross } else { -cross }
        };
        let d0 = signed_at(0.0);
        let d1 = signed_at(1.0);
        let inside0 = d0 >= -1.0e-10;
        let inside1 = d1 >= -1.0e-10;
        match (inside0, inside1) {
            (true, true) => {}
            (false, false) => return None,
            _ => {
                let denom = d0 - d1;
                if denom.abs() <= 1.0e-20 {
                    return None;
                }
                let t = (d0 / denom).clamp(0.0, 1.0);
                if inside0 {
                    t_max = t_max.min(t);
                } else {
                    t_min = t_min.max(t);
                }
                if t_min > t_max {
                    return None;
                }
            }
        }
    }

    Some((t_min, t_max))
}

pub(super) fn clip_vertical_polyline_t_min(polyline: &[VerticalClipVertex], t_min: f64) -> Vec<VerticalClipVertex> {
    clip_vertical_polyline_by_t(polyline, t_min, true)
}

pub(super) fn clip_vertical_polyline_t_max(polyline: &[VerticalClipVertex], t_max: f64) -> Vec<VerticalClipVertex> {
    clip_vertical_polyline_by_t(polyline, t_max, false)
}

pub(super) fn clip_vertical_polyline_by_t(polyline: &[VerticalClipVertex], t_plane: f64, keep_greater: bool) -> Vec<VerticalClipVertex> {
    if polyline.is_empty() {
        return Vec::new();
    }
    let retained = |vertex: VerticalClipVertex| {
        if keep_greater { vertex.t >= t_plane - 1.0e-10 } else { vertex.t <= t_plane + 1.0e-10 }
    };

    let mut output = Vec::new();
    let mut previous = *polyline.last().expect("polyline is non-empty");
    let mut previous_inside = retained(previous);
    for &current in polyline {
        let current_inside = retained(current);
        if current_inside != previous_inside {
            let denom = current.t - previous.t;
            if denom.abs() > 1.0e-20 {
                let u = ((t_plane - previous.t) / denom).clamp(0.0, 1.0);
                output.push(VerticalClipVertex {
                    t: t_plane,
                    z: previous.z + (current.z - previous.z) * u,
                });
            }
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_inside = current_inside;
    }
    output
}

pub(super) fn collect_topology_segments(polyline: &[SurfaceClipVertex], segments: &mut Vec<[mesh_data::Vertex; 2]>) {
    if polyline.len() < 2 {
        return;
    }
    // A polyline lying entirely ON the topology (a coplanar contact patch -
    // e.g. a berm skirt at exactly a previously included floor RL) is a tie:
    // the shape neither cuts nor clears the surface there, so it contributes
    // no contact boundary. Emitting its edges would leave the boundary to
    // parity cancellation between floating-point-noise copies of the same
    // segments, which produces near-degenerate cut rings; the real boundary
    // is emitted by the neighbouring polylines that do cross the surface.
    if polyline.iter().all(|vertex| vertex.height_delta.abs() <= 1e-8) {
        return;
    }
    for index in 0..polyline.len() {
        let a = polyline[index];
        let b = polyline[(index + 1) % polyline.len()];
        if a.height_delta.abs() > 1e-8 || b.height_delta.abs() > 1e-8 {
            continue;
        }
        if a.point.distance_squared(b.point) <= 1e-16 {
            continue;
        }
        segments.push([
            mesh_data::Vertex::new(a.point.x, a.point.y, a.point.z),
            mesh_data::Vertex::new(b.point.x, b.point.y, b.point.z),
        ]);
    }
}

pub(super) fn append_mesh(source_vertices: Vec<mesh_data::Vertex>, source_faces: Vec<[u32; 3]>, output_vertices: &mut Vec<mesh_data::Vertex>, output_faces: &mut Vec<[u32; 3]>) {
    let base = output_vertices.len() as u32;
    output_vertices.extend(source_vertices);
    output_faces.extend(source_faces.into_iter().map(|face| [base + face[0], base + face[1], base + face[2]]));
}
