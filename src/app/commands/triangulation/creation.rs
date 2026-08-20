use super::*;
use crate::{
    model::geometry::{points_coincident, signed_area_xy, triangle_xy_area},
    ui::state::TriCreateDiagnostic,
};

const DEGENERATE_OPEN_BOUNDARY_MESSAGE: &str = "Open strings form a closed boundary, but that boundary is degenerate or collinear";

/// Structured form of the existing user-facing failure. Carrying the exact
/// assembled ring through `anyhow` lets the UI highlight it without parsing
/// coordinates back out of the message or rerunning the CDT.
#[derive(Debug)]
struct DegenerateOpenBoundary {
    ring: Vec<glam::DVec3>,
}

impl std::fmt::Display for DegenerateOpenBoundary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(DEGENERATE_OPEN_BOUNDARY_MESSAGE)
    }
}

impl std::error::Error for DegenerateOpenBoundary {}

/// Exact pair of input edges responsible for a breakline intersection error.
/// `message` remains byte-for-byte the text produced before diagnostics were
/// added; the geometry is metadata for the failure overlay only.
#[derive(Debug)]
struct BreaklineConflictFailure {
    message: String,
    edges: [[glam::DVec3; 2]; 2],
    kind: BreaklineConflictKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BreaklineConflictKind {
    ConflictingElevation,
    Geometric,
}

impl std::fmt::Display for BreaklineConflictFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BreaklineConflictFailure {}

fn push_unique_marker(markers: &mut Vec<glam::DVec3>, marker: glam::DVec3) {
    if marker.is_finite() && !markers.contains(&marker) {
        markers.push(marker);
    }
}

/// Pure conversion of an exact degenerate boundary into overlay geometry.
fn diagnostic_for_degenerate_ring(ring: &[glam::DVec3]) -> TriCreateDiagnostic {
    let mut diagnostic = TriCreateDiagnostic::default();
    for &point in ring {
        push_unique_marker(&mut diagnostic.markers_world, point);
    }

    for pair in ring.windows(2) {
        if pair[0] != pair[1] {
            diagnostic.segments_world.push([pair[0], pair[1]]);
        }
    }
    if ring.len() > 2 && ring[0] != ring[ring.len() - 1] {
        diagnostic.segments_world.push([ring[ring.len() - 1], ring[0]]);
    }
    diagnostic
}

/// Pure conversion of a conflicting edge pair into highlighted input edges
/// and exact conflict locations. At a crossing with different elevations two
/// markers are retained at the same XY so an oblique view shows both heights.
fn diagnostic_for_conflicting_edges(first: [glam::DVec3; 2], second: [glam::DVec3; 2]) -> TriCreateDiagnostic {
    use crate::model::kernel::SegSeg;

    let [a, b] = first;
    let [c, d] = second;
    let mut diagnostic = TriCreateDiagnostic {
        markers_world: Vec::new(),
        segments_world: vec![first, second],
    };
    match crate::model::kernel::segment_segment(a.truncate(), b.truncate(), c.truncate(), d.truncate()) {
        SegSeg::Crossing { t, u, .. } | SegSeg::Touching { t, u, .. } => {
            push_unique_marker(&mut diagnostic.markers_world, a.lerp(b, t));
            push_unique_marker(&mut diagnostic.markers_world, c.lerp(d, u));
        }
        SegSeg::CollinearOverlap { t0, t1 } => {
            for t in [t0, t1] {
                let on_first = a.lerp(b, t);
                let (_, u) = crate::model::kernel::project_onto_segment(on_first.truncate(), c.truncate(), d.truncate());
                push_unique_marker(&mut diagnostic.markers_world, on_first);
                push_unique_marker(&mut diagnostic.markers_world, c.lerp(d, u));
            }
        }
        SegSeg::Disjoint => {
            // The generic CDT conflict path also diagnoses near-coincident
            // endpoints. Mark the closest exact pair instead of an unrelated
            // point on either edge.
            let closest = [(a, c), (a, d), (b, c), (b, d)].into_iter().min_by(|(p, q), (other_p, other_q)| {
                p.truncate()
                    .distance_squared(q.truncate())
                    .total_cmp(&other_p.truncate().distance_squared(other_q.truncate()))
            });
            if let Some((p, q)) = closest {
                push_unique_marker(&mut diagnostic.markers_world, p);
                push_unique_marker(&mut diagnostic.markers_world, q);
            }
        }
    }
    diagnostic
}

fn diagnostic_from_creation_error(error: &anyhow::Error) -> TriCreateDiagnostic {
    if let Some(failure) = error.downcast_ref::<DegenerateOpenBoundary>() {
        diagnostic_for_degenerate_ring(&failure.ring)
    } else if let Some(failure) = error.downcast_ref::<BreaklineConflictFailure>() {
        diagnostic_for_conflicting_edges(failure.edges[0], failure.edges[1])
    } else {
        TriCreateDiagnostic::default()
    }
}

fn upper_surface_retry_available(error: &anyhow::Error, surface_type: TriSurfaceType) -> bool {
    surface_type == TriSurfaceType::Surface
        && error
            .downcast_ref::<BreaklineConflictFailure>()
            .is_some_and(|failure| failure.kind == BreaklineConflictKind::ConflictingElevation)
}

/// Coarse endpoint-weld tolerance (metres, XY and Z) offered by the failure
/// dialog for breaklines digitized independently: gaps up to this size are
/// treated as one intended shared vertex when the user opts in.
pub(crate) const COARSE_WELD_TOL: f64 = 0.05;

#[derive(Clone, Debug)]
struct BreaklinePath {
    points: Vec<glam::DVec3>,
    closed: bool,
}

#[derive(Debug)]
struct TriangulationInput {
    /// Closed strings that define the finite surface domain. Nested rings are
    /// terrain breaklines too; they are not holes.
    boundaries: Vec<Vec<glam::DVec3>>,
    /// Genuine open strings inside the domain. These constrain the CDT but do
    /// not acquire an invented last-to-first edge.
    constraints: Vec<Vec<glam::DVec3>>,
}

impl<'a> App<'a> {
    pub(crate) fn create_triangulation_from_objects(
        &mut self,
        name: String,
        object_ids: Vec<ObjectId>,
        surface_type: TriSurfaceType,
        coarse_weld: bool,
        upper_surface: bool,
    ) -> Result<()> {
        if object_ids.is_empty() {
            anyhow::bail!("No objects selected for triangulation");
        }
        let project = self
            .workspace
            .active_project()
            .ok_or_else(|| anyhow::anyhow!("Open a PIDB project before creating a triangulation"))?;
        let project_key = crate::app::jobs::JobKey::Project {
            runtime_id: project.runtime_id,
            document_revision: project.pidb.document.revision(),
        };

        // Snapshot the referenced geometry into owned paths on the UI thread —
        // the worker must not touch `self.scene_document`.
        let (paths, rejected) = self.collect_triangulation_paths(&object_ids);

        if paths.is_empty() {
            anyhow::bail!("No usable polylines selected — triangulation requires a closed boundary, or open strings whose endpoints form one");
        }
        if rejected > 0 {
            userspace_warn!("Ignored {} non-polyline or degenerate object(s) during triangulation", rejected);
        }

        // Whether a coarse-weld retry could help, computed from the un-welded
        // paths on the UI thread so the async failure dialog can offer it.
        let weld_retry_available = !coarse_weld && {
            let mut probe = paths.clone();
            weld_breakline_vertices(&mut probe, COARSE_WELD_TOL, COARSE_WELD_TOL) > 0
        };
        let name_for_failure = name.clone();
        let object_ids_for_failure = object_ids.clone();

        // A retry supersedes the previous failure; clear it now for immediate
        // feedback (the apply step re-sets it if this attempt also fails).
        self.editor.tri_create_failure = None;
        self.editor.tri_create_diagnostic_markers_screen_px.clear();
        self.editor.tri_create_diagnostic_segments_screen_px.clear();

        let compute = move |cancel: &crate::app::jobs::CancelFlag| -> Result<crate::model::triangulation::GeneratedTriangulation> {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let generated = build_created_triangulation(paths, name, surface_type, coarse_weld, upper_surface)?;
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            Ok(generated)
        };
        let apply = move |app: &mut App, result: Result<crate::model::triangulation::GeneratedTriangulation>| match result {
            Ok(generated) => {
                app.editor.tri_create_failure = None;
                app.editor.tri_create_diagnostic_markers_screen_px.clear();
                app.editor.tri_create_diagnostic_segments_screen_px.clear();
                app.insert_generated_triangulation(generated);
            }
            Err(error) => {
                let upper_surface_retry_available = !upper_surface && upper_surface_retry_available(&error, surface_type);
                app.editor.tri_create_failure = Some(crate::ui::state::TriCreateFailure {
                    message: format!("{error:#}"),
                    name: name_for_failure,
                    object_ids: object_ids_for_failure,
                    surface_type,
                    weld_retry_available,
                    upper_surface_retry_available,
                    coarse_weld_applied: coarse_weld,
                    diagnostic: diagnostic_from_creation_error(&error),
                });
            }
        };
        self.spawn_job("Creating triangulation…", vec![project_key], compute, apply);
        Ok(())
    }

    /// Polyline geometry from the selection, preserving whether each source
    /// string was explicitly closed. Open paths are assembled into boundary
    /// cycles (where possible) by the worker after endpoint welding.
    fn collect_triangulation_paths(&self, object_ids: &[ObjectId]) -> (Vec<BreaklinePath>, usize) {
        let mut paths = Vec::new();
        let mut rejected = 0usize;
        for id in object_ids {
            let Some(obj) = self.scene_document.get_object(*id) else {
                continue;
            };
            match obj {
                Object::Polyline { verts, closed, .. } => {
                    let points = crate::model::geometry::tessellate_polyline_bulges(verts, *closed);
                    let minimum = if *closed { 3 } else { 2 };
                    if points.len() >= minimum {
                        paths.push(BreaklinePath { points, closed: *closed });
                    } else {
                        rejected += 1;
                    }
                }
                _ => {
                    rejected += 1;
                }
            }
        }
        (paths, rejected)
    }

    /// Whether the coarse weld would actually move any vertex of the
    /// selection — if not, offering "weld & retry" would be a no-op lie.
    pub(crate) fn coarse_weld_would_change(&self, object_ids: &[ObjectId]) -> bool {
        let (mut paths, _) = self.collect_triangulation_paths(object_ids);
        weld_breakline_vertices(&mut paths, COARSE_WELD_TOL, COARSE_WELD_TOL) > 0
    }

    /// Create Triangulation with failure capture. The triangulation runs on a
    /// background thread; async CDT failures populate the failure dialog in the
    /// job's apply step. This wrapper only captures the *synchronous* early
    /// errors (empty selection, no usable polylines) into the same dialog so the
    /// coarse-weld retry stays available.
    pub(crate) fn run_create_triangulation(&mut self, name: String, object_ids: Vec<ObjectId>, surface_type: TriSurfaceType, coarse_weld: bool, upper_surface: bool) -> Result<()> {
        let result = self.create_triangulation_from_objects(name.clone(), object_ids.clone(), surface_type, coarse_weld, upper_surface);
        if let Err(error) = &result {
            // Don't offer the weld again if it already ran and failed.
            let weld_retry_available = !coarse_weld && self.coarse_weld_would_change(&object_ids);
            let upper_surface_retry_available = !upper_surface && upper_surface_retry_available(error, surface_type);
            self.editor.tri_create_failure = Some(crate::ui::state::TriCreateFailure {
                message: format!("{error:#}"),
                name,
                object_ids,
                surface_type,
                weld_retry_available,
                upper_surface_retry_available,
                coarse_weld_applied: coarse_weld,
                diagnostic: TriCreateDiagnostic::default(),
            });
        }
        result
    }
}

/// Worker-thread half of Create: weld, triangulate, and build the mesh/BVH.
/// Pure (no `App`, no scene access) so it can run off the UI thread. Emits the
/// weld/creation log lines (the log sink is thread-safe).
fn build_created_triangulation(
    mut paths: Vec<BreaklinePath>,
    name: String,
    surface_type: TriSurfaceType,
    coarse_weld: bool,
    upper_surface: bool,
) -> Result<crate::model::triangulation::GeneratedTriangulation> {
    let welded = weld_breakline_vertices(&mut paths, crate::model::kernel::XY_TOL, crate::model::kernel::Z_TOL);
    if welded > 0 {
        userspace_log!("Welded {} breakline vertex/vertices that coincided within tolerance", welded);
    }
    if coarse_weld {
        let coarse_welded = weld_breakline_vertices(&mut paths, COARSE_WELD_TOL, COARSE_WELD_TOL);
        userspace_log!(
            "Weld & retry: moved {} vertex/vertices onto shared positions (up to {} m); \
             source objects are unchanged",
            coarse_welded,
            COARSE_WELD_TOL
        );
    }

    let input = assemble_triangulation_input(paths)?;
    let boundary_count = input.boundaries.len();
    let constraint_count = input.constraints.len();

    let (all_verts, all_faces) = if boundary_count == 1 && constraint_count == 0 && surface_type == TriSurfaceType::Surface && !upper_surface {
        // Single ring: triangulate its flat interior (normal pointing up).
        let flip = signed_area_xy(&input.boundaries[0]) <= 0.0; // CW ring needs flip for normal-up
        cdt_fill_ring(&input.boundaries[0], flip)?
    } else if surface_type == TriSurfaceType::Surface && upper_surface {
        cdt_upper_surface_from_breaklines(&input.boundaries, &input.constraints)?
    } else if surface_type == TriSurfaceType::Surface {
        // Nested contours, benches and berms are terrain breaklines, not a
        // Z-sorted loft stack. Triangulate all selected boundaries in one XY
        // CDT so every string edge is preserved and its vertex Z drives the
        // resulting terrain surface.
        cdt_surface_from_breaklines(&input.boundaries, &input.constraints)?
    } else {
        closed_solid_from_breaklines(&input.boundaries, &input.constraints)?
    };

    if all_faces.is_empty() {
        anyhow::bail!("Triangulation produced no faces — polygons may be collinear or degenerate");
    }

    userspace_log!(
        "Created triangulation from {} boundary ring(s) and {} open constraint(s), surface type {:?}",
        boundary_count,
        constraint_count,
        surface_type
    );
    session::build_generated_triangulation(name, all_verts, all_faces, surface_type, crate::model::triangulation::unique_edges)
}

/// Snap breakline vertices that coincide within the given tolerances (XY
/// horizontally, Z vertically, both metres) onto one canonical position,
/// across all paths. At kernel tolerance, vertices this close come from
/// digitization or floating-point noise, never intent; welding them lets the
/// CDT share one vertex instead of threading constraints between two points a
/// fraction of a millimetre apart (sliver triangles) or rejecting the same XY
/// at two noise-level elevations. At the coarse tolerance this deliberately
/// moves user geometry and must be user-initiated. Returns the number of
/// vertices moved.
fn weld_breakline_vertices(paths: &mut [BreaklinePath], xy_tol: f64, z_tol: f64) -> usize {
    let cell = |v: glam::DVec3| -> (i64, i64) { ((v.x / xy_tol).floor() as i64, (v.y / xy_tol).floor() as i64) };
    let coincident = |a: glam::DVec3, b: glam::DVec3| -> bool { a.truncate().distance_squared(b.truncate()) <= xy_tol * xy_tol && (a.z - b.z).abs() <= z_tol };
    let mut grid: HashMap<(i64, i64), Vec<glam::DVec3>> = HashMap::new();
    let mut welded = 0usize;
    for path in paths.iter_mut() {
        for point in path.points.iter_mut() {
            let (cx, cy) = cell(*point);
            let mut canonical = None;
            'search: for gx in cx - 1..=cx + 1 {
                for gy in cy - 1..=cy + 1 {
                    for candidate in grid.get(&(gx, gy)).into_iter().flatten() {
                        if coincident(*candidate, *point) {
                            canonical = Some(*candidate);
                            break 'search;
                        }
                    }
                }
            }
            match canonical {
                Some(canonical) => {
                    if *point != canonical {
                        *point = canonical;
                        welded += 1;
                    }
                }
                None => grid.entry((cx, cy)).or_default().push(*point),
            }
        }
    }
    welded
}

type EndpointKey = (u64, u64, u64);

fn endpoint_key(point: glam::DVec3) -> EndpointKey {
    (point.x.to_bits(), point.y.to_bits(), point.z.to_bits())
}

/// Separate explicit/inferred boundary cycles from genuine open constraints.
///
/// Open source objects are edges in an endpoint graph. Repeatedly peeling
/// degree-one nodes removes dangling constraint branches and leaves the graph's
/// cycle core. A degree-two core has an unambiguous set of closed walks, which
/// we concatenate regardless of source order or direction. More complicated
/// junctions remain constraints because guessing a boundary through them would
/// change the user's geometry semantics.
fn assemble_triangulation_input(paths: Vec<BreaklinePath>) -> Result<TriangulationInput> {
    let mut boundaries = Vec::new();
    let mut open = Vec::new();
    for path in paths {
        if path.closed {
            boundaries.push(path.points);
        } else {
            open.push(path.points);
        }
    }

    if open.is_empty() {
        if boundaries.is_empty() {
            anyhow::bail!("Triangulation has no closed boundary");
        }
        return Ok(TriangulationInput {
            boundaries,
            constraints: Vec::new(),
        });
    }

    let mut node_for_key: HashMap<EndpointKey, usize> = HashMap::new();
    let mut edge_nodes = Vec::with_capacity(open.len());
    for points in &open {
        let endpoints = [points[0], points[points.len() - 1]];
        let mut nodes = [0usize; 2];
        for (slot, point) in nodes.iter_mut().zip(endpoints) {
            let next = node_for_key.len();
            *slot = *node_for_key.entry(endpoint_key(point)).or_insert(next);
        }
        edge_nodes.push(nodes);
    }

    let mut incident = vec![Vec::<usize>::new(); node_for_key.len()];
    let mut degree = vec![0usize; node_for_key.len()];
    for (edge, [a, b]) in edge_nodes.iter().copied().enumerate() {
        incident[a].push(edge);
        incident[b].push(edge);
        degree[a] += 1;
        degree[b] += 1;
    }
    let original_degree = degree.clone();

    // Peel every bridge-like tail. What remains is the 2-core containing all
    // possible endpoint cycles, including a boundary with an attached open
    // breakline.
    let mut active = vec![true; open.len()];
    let mut queue: std::collections::VecDeque<usize> = degree.iter().enumerate().filter_map(|(node, degree)| (*degree <= 1).then_some(node)).collect();
    while let Some(node) = queue.pop_front() {
        if degree[node] > 1 {
            continue;
        }
        for &edge in &incident[node] {
            if !active[edge] {
                continue;
            }
            active[edge] = false;
            let [a, b] = edge_nodes[edge];
            for endpoint in [a, b] {
                degree[endpoint] = degree[endpoint].saturating_sub(1);
                if degree[endpoint] == 1 {
                    queue.push_back(endpoint);
                }
            }
        }
    }

    let core_is_unambiguous = degree.iter().all(|degree| *degree == 0 || *degree == 2);
    let mut used = vec![false; open.len()];
    let mut assembled_count = 0usize;
    if core_is_unambiguous {
        for first_edge in 0..open.len() {
            if !active[first_edge] || used[first_edge] {
                continue;
            }
            let start_node = edge_nodes[first_edge][0];
            let mut node = start_node;
            let mut edge = first_edge;
            let mut ring = Vec::new();

            loop {
                let [a, b] = edge_nodes[edge];
                let forward = a == node;
                let points = &open[edge];
                if forward {
                    ring.extend(points.iter().copied().skip((!ring.is_empty()) as usize));
                    node = b;
                } else {
                    ring.extend(points.iter().rev().copied().skip((!ring.is_empty()) as usize));
                    node = a;
                }
                used[edge] = true;

                if node == start_node {
                    break;
                }
                edge = incident[node]
                    .iter()
                    .copied()
                    .find(|candidate| active[*candidate] && !used[*candidate])
                    .ok_or_else(|| anyhow::anyhow!("Open-string boundary cycle ended unexpectedly"))?;
            }

            // The walk repeats its first endpoint at the end; rings elsewhere
            // in the triangulator store that closure implicitly.
            if ring.last() == ring.first() {
                ring.pop();
            }
            if ring.len() < 3 || signed_area_xy(&ring).abs() <= 1e-12 {
                return Err(anyhow::Error::new(DegenerateOpenBoundary { ring }));
            }
            boundaries.push(ring);
            assembled_count += 1;
        }
    }

    // Peeled paths are genuine open constraints. If a cycle core contains a
    // branching junction, retain the whole core as constraints rather than
    // choosing one of several possible boundary walks.
    let constraints: Vec<Vec<glam::DVec3>> = open.into_iter().enumerate().filter_map(|(edge, points)| (!used[edge]).then_some(points)).collect();

    if boundaries.is_empty() {
        let unmatched = original_degree.iter().filter(|degree| **degree == 1).count();
        let junctions = original_degree.iter().filter(|degree| **degree > 2).count();
        anyhow::bail!("Selected open strings do not define an unambiguous closed boundary ({unmatched} unmatched endpoint(s), {junctions} branching junction(s))");
    }
    if assembled_count > 0 {
        userspace_log!("Assembled {} closed boundary ring(s) from fragmented open strings", assembled_count);
    }

    Ok(TriangulationInput { boundaries, constraints })
}

/// Diagnose why an edge failed to insert as a CDT constraint. Scans every
/// other edge across all closed and open breaklines for a
/// direct geometric conflict (spade doesn't say which edge or why it
/// conflicted) and describes the first one found: a crossing point (with
/// each edge's interpolated Z, to show whether it's even representable by a
/// single-valued terrain), a collinear overlap, or near-but-not-exactly
/// coincident endpoints — the common case when two breaklines were meant to
/// share a boundary but were digitized independently.
type BreaklineRef<'a> = (&'a [glam::DVec3], bool);

fn breakline_edge_count(path: BreaklineRef<'_>) -> usize {
    if path.1 { path.0.len() } else { path.0.len().saturating_sub(1) }
}

fn breakline_edge(path: BreaklineRef<'_>, edge_index: usize) -> (glam::DVec3, glam::DVec3) {
    let points = path.0;
    (points[edge_index], points[(edge_index + 1) % points.len()])
}

fn diagnose_breakline_conflict(paths: &[BreaklineRef<'_>], path_index: usize, edge_index: usize) -> (String, Option<[[glam::DVec3; 2]; 2]>) {
    let (a, b) = breakline_edge(paths[path_index], edge_index);
    for (other_path_index, other_path) in paths.iter().copied().enumerate() {
        for other_edge_index in 0..breakline_edge_count(other_path) {
            if other_path_index == path_index && other_edge_index == edge_index {
                continue;
            }
            let (c, d) = breakline_edge(other_path, other_edge_index);
            // Edges that legitimately share an endpoint (adjacent edges
            // within a ring, or two rings meeting at a shared vertex) are
            // not conflicts — skip them so a real conflict elsewhere isn't
            // shadowed by this expected topology.
            let shares_endpoint = points_coincident(a, c) || points_coincident(a, d) || points_coincident(b, c) || points_coincident(b, d);
            if shares_endpoint {
                continue;
            }
            if let Some(detail) = describe_edge_conflict(a, b, c, d) {
                return (
                    format!("breakline {path_index} edge {edge_index} ({a:.3}->{b:.3}) vs breakline {other_path_index} edge {other_edge_index} ({c:.3}->{d:.3}): {detail}"),
                    Some([[a, b], [c, d]]),
                );
            }
        }
    }
    (
        format!("breakline {path_index} edge {edge_index} ({a:.3}->{b:.3}): no conflicting edge found by direct geometric scan (likely a near-degenerate numerical case)"),
        None,
    )
}

/// Classify how segment `a->b` conflicts with segment `c->d` in the XY plane,
/// if at all. `None` means these two edges specifically don't touch (the
/// real conflict is with some other edge).
fn describe_edge_conflict(a: glam::DVec3, b: glam::DVec3, c: glam::DVec3, d: glam::DVec3) -> Option<String> {
    use crate::model::kernel::{self, SegSeg};
    let (a2, b2, c2, d2) = (a.truncate(), b.truncate(), c.truncate(), d.truncate());
    match kernel::segment_segment(a2, b2, c2, d2) {
        SegSeg::Crossing { t, u, .. } => Some(describe_crossing(a, b, c, d, t, u, "cross")),
        SegSeg::Touching { point, t, u } => {
            // An endpoint of one edge on the *interior* of the other fails
            // `try_add_constraint` (spade can't run a constraint through a
            // vertex without splitting it). An endpoint-to-endpoint near miss
            // is the digitized-independently signature instead.
            let a_endpoint = point.distance(a2) <= kernel::XY_TOL || point.distance(b2) <= kernel::XY_TOL;
            let b_endpoint = point.distance(c2) <= kernel::XY_TOL || point.distance(d2) <= kernel::XY_TOL;
            if a_endpoint != b_endpoint {
                Some(describe_crossing(a, b, c, d, t, u, "touch"))
            } else {
                nearest_endpoint_gap(a, b, c, d)
            }
        }
        SegSeg::CollinearOverlap { t0, t1 } => Some(format!(
            "collinear and overlapping along the same line for parameter range [{t0:.3}, {t1:.3}] of edge A \u{2014} these two breaklines run along the same wall without sharing vertices, so the CDT can't insert both without splitting them"
        )),
        SegSeg::Disjoint => nearest_endpoint_gap(a, b, c, d),
    }
}

fn describe_crossing(a: glam::DVec3, b: glam::DVec3, c: glam::DVec3, d: glam::DVec3, t: f64, u: f64, relation: &str) -> String {
    let point = a.truncate() + t * (b.truncate() - a.truncate());
    let z_a = a.z + t * (b.z - a.z);
    let z_c = c.z + u * (d.z - c.z);
    let representable = if (z_a - z_c).abs() > 1e-3 {
        "different elevations: not representable by a single-valued terrain surface"
    } else {
        "same elevation: could be split at this point"
    };
    format!(
        "{relation} in XY at ({:.3}, {:.3}); edge A's Z there is {z_a:.3}, edge B's Z there is {z_c:.3} ({representable})",
        point.x, point.y
    )
}

/// If the closest pair of endpoints between the two edges is suspiciously
/// close (but not exactly coincident), report the gap — this is the
/// signature of two breaklines that were meant to share a vertex but were
/// digitized independently and differ by floating-point/snap noise.
fn nearest_endpoint_gap(a: glam::DVec3, b: glam::DVec3, c: glam::DVec3, d: glam::DVec3) -> Option<String> {
    const NEAR_MISS: f64 = 0.05; // 5cm: plausible "meant to be the same vertex" gap
    [(a, c, "A.start~B.start"), (a, d, "A.start~B.end"), (b, c, "A.end~B.start"), (b, d, "A.end~B.end")]
        .into_iter()
        .map(|(p, q, label)| ((p.truncate() - q.truncate()).length(), (p.z - q.z).abs(), label))
        .filter(|(gap, ..)| *gap < NEAR_MISS)
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map(|(gap, dz, label)| {
            format!("nearest endpoints ({label}) are {gap:.4} apart in XY (Z differs by {dz:.4}) \u{2014} likely meant to be the same shared vertex but digitized independently")
        })
}

fn conflicting_z_detail(a: glam::DVec3, b: glam::DVec3, c: glam::DVec3, d: glam::DVec3) -> Option<String> {
    use crate::model::kernel::{self, SegSeg};
    // Elevation disagreement below Z_TOL is survey noise, not a conflict: the
    // crossing proceeds and the split vertex takes the first edge's Z.
    const Z_EPS: f64 = kernel::Z_TOL;
    let (a2, b2, c2, d2) = (a.truncate(), b.truncate(), c.truncate(), d.truncate());
    match kernel::segment_segment(a2, b2, c2, d2) {
        SegSeg::Crossing { t, u, .. } | SegSeg::Touching { t, u, .. } => {
            let z_a = a.z + t * (b.z - a.z);
            let z_c = c.z + u * (d.z - c.z);
            if (z_a - z_c).abs() > Z_EPS {
                return Some(describe_crossing(a, b, c, d, t, u, "cross"));
            }
            None
        }
        SegSeg::CollinearOverlap { t0, t1 } => {
            let r = b2 - a2;
            for t in [t0, t1] {
                let point = a2 + t * r;
                let (_, u) = kernel::project_onto_segment(point, c2, d2);
                let z_a = a.z + t * (b.z - a.z);
                let z_c = c.z + u * (d.z - c.z);
                if (z_a - z_c).abs() > Z_EPS {
                    return Some(format!(
                        "collinear overlap has conflicting elevations near ({:.3}, {:.3}); edge A's Z is {z_a:.3}, edge B's Z is {z_c:.3}",
                        point.x, point.y
                    ));
                }
            }
            None
        }
        SegSeg::Disjoint => None,
    }
}

fn validate_breakline_edge_z(paths: &[BreaklineRef<'_>], path_index: usize, edge_index: usize) -> Result<()> {
    let (a, b) = breakline_edge(paths[path_index], edge_index);

    for (other_path_index, other_path) in paths.iter().copied().enumerate() {
        for other_edge_index in 0..breakline_edge_count(other_path) {
            if other_path_index == path_index && other_edge_index == edge_index {
                continue;
            }
            let (c, d) = breakline_edge(other_path, other_edge_index);
            if let Some(detail) = conflicting_z_detail(a, b, c, d) {
                return Err(anyhow::Error::new(BreaklineConflictFailure {
                    message: format!(
                        "Selected breakline edges intersect in XY at conflicting elevations and cannot form a single-valued terrain surface (breakline {path_index} edge {edge_index} ({a:.3}->{b:.3}) vs breakline {other_path_index} edge {other_edge_index} ({c:.3}->{d:.3}): {detail})"
                    ),
                    edges: [[a, b], [c, d]],
                    kind: BreaklineConflictKind::ConflictingElevation,
                }));
            }
        }
    }

    Ok(())
}

fn interpolate_z_on_edge(a: glam::DVec3, b: glam::DVec3, point: glam::DVec2) -> f64 {
    let ab = b.truncate() - a.truncate();
    let len_sq = ab.length_squared();
    if len_sq < 1e-18 {
        a.z
    } else {
        let t = ((point - a.truncate()).dot(ab) / len_sq).clamp(0.0, 1.0);
        a.z + t * (b.z - a.z)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpperSurfaceLoser {
    First,
    Second,
    Both,
}

/// Identify which edge lies below the other at a conflicting XY crossing.
/// Collinear edges whose elevation order changes across their overlap cannot
/// both describe one continuous upper envelope, so both constraints are
/// omitted and their vertices remain as unconstrained surface observations.
fn upper_surface_loser(a: glam::DVec3, b: glam::DVec3, c: glam::DVec3, d: glam::DVec3) -> Option<UpperSurfaceLoser> {
    use crate::model::kernel::{self, SegSeg};

    let elevation_loser = |z_first: f64, z_second: f64| {
        let difference = z_first - z_second;
        if difference > kernel::Z_TOL {
            Some(UpperSurfaceLoser::Second)
        } else if difference < -kernel::Z_TOL {
            Some(UpperSurfaceLoser::First)
        } else {
            None
        }
    };
    match kernel::segment_segment(a.truncate(), b.truncate(), c.truncate(), d.truncate()) {
        SegSeg::Crossing { t, u, .. } | SegSeg::Touching { t, u, .. } => elevation_loser(a.z + t * (b.z - a.z), c.z + u * (d.z - c.z)),
        SegSeg::CollinearOverlap { t0, t1 } => {
            let mut losers = [t0, (t0 + t1) * 0.5, t1].into_iter().filter_map(|t| {
                let point = a.truncate().lerp(b.truncate(), t);
                let (_, u) = kernel::project_onto_segment(point, c.truncate(), d.truncate());
                elevation_loser(a.z + t * (b.z - a.z), c.z + u * (d.z - c.z))
            });
            let first = losers.next()?;
            if losers.all(|loser| loser == first) {
                Some(first)
            } else {
                Some(UpperSurfaceLoser::Both)
            }
        }
        SegSeg::Disjoint => None,
    }
}

fn lower_conflicting_breakline_edges(paths: &[BreaklineRef<'_>]) -> HashSet<(usize, usize)> {
    let edges: Vec<_> = paths
        .iter()
        .copied()
        .enumerate()
        .flat_map(|(path_index, path)| {
            (0..breakline_edge_count(path)).map(move |edge_index| {
                let (start, end) = breakline_edge(path, edge_index);
                (
                    path_index,
                    edge_index,
                    start,
                    end,
                    start.truncate().min(end.truncate()),
                    start.truncate().max(end.truncate()),
                )
            })
        })
        .collect();
    // Sweep edge AABBs in X before running exact segment tests. Mine designs
    // commonly contain tens of thousands of short edges; comparing every pair
    // would turn this opt-in recovery into an O(n²) pause.
    let mut order: Vec<usize> = (0..edges.len()).collect();
    order.sort_unstable_by(|&first, &second| edges[first].4.x.total_cmp(&edges[second].4.x));
    let mut active = Vec::<usize>::new();
    let mut ignored = HashSet::new();
    for edge_index in order {
        let (path, path_edge, c, d, min, max) = edges[edge_index];
        active.retain(|&other_index| edges[other_index].5.x >= min.x);
        for &other_index in &active {
            let (other_path, other_edge, a, b, other_min, other_max) = edges[other_index];
            if other_max.y < min.y || max.y < other_min.y {
                continue;
            }
            match upper_surface_loser(a, b, c, d) {
                Some(UpperSurfaceLoser::First) => {
                    ignored.insert((other_path, other_edge));
                }
                Some(UpperSurfaceLoser::Second) => {
                    ignored.insert((path, path_edge));
                }
                Some(UpperSurfaceLoser::Both) => {
                    ignored.insert((other_path, other_edge));
                    ignored.insert((path, path_edge));
                }
                None => {}
            }
        }
        active.push(edge_index);
    }
    ignored
}

pub(super) fn cdt_surface_from_breaklines(boundaries: &[Vec<glam::DVec3>], constraints: &[Vec<glam::DVec3>]) -> Result<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)> {
    cdt_surface_from_breaklines_impl(boundaries, constraints, false)
}

fn cdt_upper_surface_from_breaklines(boundaries: &[Vec<glam::DVec3>], constraints: &[Vec<glam::DVec3>]) -> Result<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)> {
    cdt_surface_from_breaklines_impl(boundaries, constraints, true)
}

fn cdt_surface_from_breaklines_impl(boundaries: &[Vec<glam::DVec3>], constraints: &[Vec<glam::DVec3>], upper_surface: bool) -> Result<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)> {
    use spade::{ConstrainedDelaunayTriangulation, Point2};

    if boundaries.is_empty() {
        anyhow::bail!("No closed breakline boundary supplied");
    }

    let paths: Vec<BreaklineRef<'_>> = boundaries
        .iter()
        .map(|points| (points.as_slice(), true))
        .chain(constraints.iter().map(|points| (points.as_slice(), false)))
        .collect();
    let ignored_edges = if upper_surface { lower_conflicting_breakline_edges(&paths) } else { HashSet::new() };
    if upper_surface {
        userspace_warn!(
            "Generate upper surface: ignored {} lower conflicting breakline segment(s); source objects are unchanged",
            ignored_edges.len()
        );
    }

    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::new();
    let mut handle_z: HashMap<usize, f64> = HashMap::new();

    for (path_index, path) in paths.iter().copied().enumerate() {
        let (points, closed) = path;
        let minimum = if closed { 3 } else { 2 };
        if points.len() < minimum {
            anyhow::bail!("A selected breakline has too few vertices");
        }

        let mut handles = Vec::with_capacity(points.len());
        for point in points {
            if !point.is_finite() {
                anyhow::bail!("A selected breakline contains non-finite coordinates");
            }
            let handle = cdt.insert(Point2::new(point.x, point.y)).map_err(|error| anyhow::anyhow!("CDT insert failed: {error:?}"))?;
            // Same XY from two breaklines: keep the first elevation when they
            // agree within Z_TOL (survey noise); larger disagreement cannot be
            // represented by a single-valued terrain.
            match handle_z.entry(handle.index()) {
                std::collections::hash_map::Entry::Occupied(mut existing) => {
                    let existing_z = *existing.get();
                    if upper_surface {
                        if point.z > existing_z {
                            existing.insert(point.z);
                        }
                    } else if (existing_z - point.z).abs() > crate::model::kernel::Z_TOL {
                        anyhow::bail!(
                            "Selected breaklines contain the same XY point at conflicting elevations ({existing_z:.3} and {:.3})",
                            point.z
                        );
                    }
                }
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    vacant.insert(point.z);
                }
            }
            handles.push(handle);
        }

        for i in 0..breakline_edge_count(path) {
            if ignored_edges.contains(&(path_index, i)) {
                continue;
            }
            let a = handles[i];
            let b = handles[(i + 1) % points.len()];
            if a == b {
                continue;
            }
            if !upper_surface {
                validate_breakline_edge_z(&paths, path_index, i)?;
            }

            let (edge_start, edge_end) = breakline_edge(path, i);
            // A panic here is spade failing to split a near-degenerate
            // crossing (the intersection point snaps onto blocking geometry);
            // report it as the overlap it is rather than crashing.
            let constraint_edges = crate::logging::catch_panic_quietly(|| cdt.add_constraint_and_split(a, b, |point| point)).unwrap_or_default();
            if constraint_edges.is_empty() {
                let (detail, conflicting_edges) = diagnose_breakline_conflict(&paths, path_index, i);
                let message = format!("Selected breakline edges intersect or overlap in XY and cannot form a terrain surface ({detail})");
                if let Some(edges) = conflicting_edges {
                    return Err(anyhow::Error::new(BreaklineConflictFailure {
                        message,
                        edges,
                        kind: BreaklineConflictKind::Geometric,
                    }));
                }
                anyhow::bail!(message);
            }
            for edge in constraint_edges {
                let edge = cdt.directed_edge(edge);
                for vertex in [edge.from(), edge.to()] {
                    let index = vertex.fix().index();
                    let position = vertex.position();
                    let edge_z = interpolate_z_on_edge(edge_start, edge_end, glam::DVec2::new(position.x, position.y));
                    handle_z
                        .entry(index)
                        .and_modify(|existing_z| {
                            if upper_surface {
                                *existing_z = existing_z.max(edge_z);
                            }
                        })
                        .or_insert(edge_z);
                }
            }
        }
    }

    let mut indexed: Vec<(usize, f64, f64, f64)> = cdt
        .vertices()
        .map(|vertex| {
            let index = vertex.fix().index();
            let position = vertex.position();
            let z = handle_z
                .get(&index)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("CDT introduced an elevation-less vertex while resolving breaklines"))?;
            Ok((index, position.x, position.y, z))
        })
        .collect::<Result<Vec<_>>>()?;
    indexed.sort_unstable_by_key(|(index, ..)| *index);

    let index_map: HashMap<usize, u32> = indexed
        .iter()
        .enumerate()
        .map(|(output_index, (spade_index, ..))| (*spade_index, output_index as u32))
        .collect();
    let vertices = indexed.iter().map(|(_, x, y, z)| mesh_data::Vertex::new(*x, *y, *z)).collect();

    let mut faces = Vec::new();
    for face in cdt.inner_faces() {
        let face_vertices = face.vertices();
        let positions = face_vertices.map(|vertex| vertex.position());
        let centroid = glam::DVec2::new(
            (positions[0].x + positions[1].x + positions[2].x) / 3.0,
            (positions[0].y + positions[1].y + positions[2].y) / 3.0,
        );
        if !boundaries.iter().any(|ring| crate::model::geometry::point_in_polygon_xy(centroid, ring)) {
            continue;
        }

        let twice_area = (positions[1].x - positions[0].x) * (positions[2].y - positions[0].y) - (positions[1].y - positions[0].y) * (positions[2].x - positions[0].x);
        if twice_area.abs() <= 1e-12 {
            continue;
        }

        let mut triangle = face_vertices.map(|vertex| index_map[&vertex.fix().index()]);
        if twice_area < 0.0 {
            triangle.swap(1, 2);
        }
        faces.push(triangle);
    }

    if faces.is_empty() {
        anyhow::bail!("Constrained surface triangulation produced no faces");
    }

    Ok((vertices, faces))
}

/// Close a constrained terrain surface at each design's outer boundary level.
///
/// For a pit, the terrain is the lower shell and the closure cap lies above it.
/// For a stockpile, the terrain is the upper shell and the closure cap lies
/// below it. Nested rings remain terrain breaklines, not internal walls.
pub(super) fn closed_solid_from_breaklines(boundaries: &[Vec<glam::DVec3>], constraints: &[Vec<glam::DVec3>]) -> Result<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)> {
    let (vertices, mut faces) = cdt_surface_from_breaklines(boundaries, constraints)?;
    let roots = outer_breakline_indices(boundaries);
    let surface_indices: HashMap<(u64, u64, u64), u32> = vertices
        .iter()
        .enumerate()
        .map(|(index, vertex)| ((vertex.x.to_bits(), vertex.y.to_bits(), vertex.z.to_bits()), index as u32))
        .collect();

    for root_index in roots {
        let ring = &boundaries[root_index];
        let group_rings: Vec<&Vec<glam::DVec3>> = boundaries
            .iter()
            .filter(|candidate| std::ptr::eq(*candidate, ring) || crate::model::geometry::point_in_polygon_xy(candidate[0].truncate(), ring))
            .collect();
        let group_constraints: Vec<&Vec<glam::DVec3>> = constraints
            .iter()
            .filter(|candidate| candidate.iter().any(|point| crate::model::geometry::point_in_polygon_xy(point.truncate(), ring)))
            .collect();
        let closure_z = ring.iter().map(|point| point.z).sum::<f64>() / ring.len() as f64;
        let outer_z_span = ring
            .iter()
            .map(|point| point.z)
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), z| (min.min(z), max.max(z)));
        if outer_z_span.1 - outer_z_span.0 > 1e-7 {
            anyhow::bail!("A closed solid requires each outer boundary to have one constant elevation");
        }

        let group_min_z = group_rings
            .iter()
            .flat_map(|group_ring| group_ring.iter())
            .chain(group_constraints.iter().flat_map(|constraint| constraint.iter()))
            .map(|point| point.z)
            .fold(f64::INFINITY, f64::min);
        let group_max_z = group_rings
            .iter()
            .flat_map(|group_ring| group_ring.iter())
            .chain(group_constraints.iter().flat_map(|constraint| constraint.iter()))
            .map(|point| point.z)
            .fold(f64::NEG_INFINITY, f64::max);
        let extends_below = group_min_z < closure_z - 1e-8;
        let extends_above = group_max_z > closure_z + 1e-8;
        if extends_below == extends_above {
            if extends_below {
                anyhow::bail!("A solid design cannot extend both above and below its outer boundary elevation");
            }
            anyhow::bail!("Each closed solid group requires breaklines at more than one elevation");
        }
        let is_pit = extends_below;

        let boundary_indices: Vec<u32> = ring
            .iter()
            .map(|point| {
                surface_indices
                    .get(&(point.x.to_bits(), point.y.to_bits(), point.z.to_bits()))
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!("Outer breakline vertex is missing from the constrained surface"))
            })
            .collect::<Result<Vec<_>>>()?;

        // CDT terrain faces initially point upward. A pit's terrain is the
        // bottom of the solid, so its faces must point downward.
        if is_pit {
            for face in &mut faces {
                let centroid = face
                    .iter()
                    .map(|index| {
                        let vertex = vertices[*index as usize];
                        glam::DVec2::new(vertex.x, vertex.y)
                    })
                    .sum::<glam::DVec2>()
                    / 3.0;
                if crate::model::geometry::point_in_polygon_xy(centroid, ring) {
                    face.swap(1, 2);
                }
            }
        }

        let flat: Vec<[f64; 2]> = ring.iter().map(|point| [point.x, point.y]).collect();
        let mut cap_triangles: Vec<usize> = Vec::new();
        earcut::Earcut::new().earcut(flat.iter().copied(), &[], &mut cap_triangles);
        if cap_triangles.is_empty() && flat.len() >= 3 {
            anyhow::bail!("Failed to triangulate solid closure: degenerate boundary ring");
        }
        for triangle in cap_triangles.as_chunks::<3>().0 {
            let mut face = [boundary_indices[triangle[0]], boundary_indices[triangle[1]], boundary_indices[triangle[2]]];
            let cap_should_point_up = is_pit;
            let corners = face.map(|index| vertices[index as usize]);
            if (triangle_xy_area(corners) > 0.0) != cap_should_point_up {
                face.swap(1, 2);
            }
            faces.push(face);
        }
    }

    Ok((vertices, faces))
}
pub(super) fn outer_breakline_indices(rings: &[Vec<glam::DVec3>]) -> Vec<usize> {
    rings
        .iter()
        .enumerate()
        .filter_map(|(index, ring)| {
            let probe = ring[0].truncate();
            let contained = rings.iter().enumerate().any(|(other_index, other)| {
                other_index != index && signed_area_xy(other).abs() > signed_area_xy(ring).abs() && crate::model::geometry::point_in_polygon_xy(probe, other)
            });
            (!contained).then_some(index)
        })
        .collect()
}

/// Triangulate the interior of a closed ring using CDT.
/// `flip_winding` reverses triangle winding (use to control face normal direction).
pub(super) fn cdt_fill_ring(ring: &[glam::DVec3], flip_winding: bool) -> Result<(Vec<mesh_data::Vertex>, Vec<[u32; 3]>)> {
    use spade::{ConstrainedDelaunayTriangulation, Point2};

    if ring.len() < 3 {
        anyhow::bail!("A selected polygon has fewer than 3 vertices");
    }
    validate_single_ring_crossing_z(ring)?;

    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::new();
    let mut handles = Vec::new();
    let mut handle_z: HashMap<usize, f64> = HashMap::new();

    for v in ring {
        if !v.is_finite() {
            anyhow::bail!("A selected polygon contains non-finite coordinates");
        }
        let h = cdt.insert(Point2::new(v.x, v.y)).map_err(|error| anyhow::anyhow!("CDT insert failed: {error:?}"))?;
        match handle_z.entry(h.index()) {
            std::collections::hash_map::Entry::Occupied(existing) => {
                let existing_z = *existing.get();
                if (existing_z - v.z).abs() > crate::model::kernel::Z_TOL {
                    anyhow::bail!("Selected polygon contains the same XY point at conflicting elevations ({existing_z:.3} and {:.3})", v.z);
                }
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                vacant.insert(v.z);
            }
        }
        handles.push(h);
    }
    for i in 0..ring.len() {
        let j = (i + 1) % ring.len();
        let (ha, hb) = (handles[i], handles[j]);
        if ha == hb {
            continue;
        }
        // Self-intersecting edges are split at their crossing points. Every
        // edge that reaches a split must agree on its interpolated elevation;
        // otherwise the ring cannot represent a single-valued surface.
        let constraint_edges = crate::logging::catch_panic_quietly(|| cdt.add_constraint_and_split(ha, hb, |point| point))
            .ok_or_else(|| anyhow::anyhow!("Selected polygon edges cross or overlap themselves too closely in XY to triangulate"))?;
        for edge in constraint_edges {
            let edge = cdt.directed_edge(edge);
            for vertex in [edge.from(), edge.to()] {
                let index = vertex.fix().index();
                let position = vertex.position();
                let edge_z = interpolate_z_on_edge(ring[i], ring[j], glam::DVec2::new(position.x, position.y));
                match handle_z.entry(index) {
                    std::collections::hash_map::Entry::Occupied(existing) => {
                        if (*existing.get() - edge_z).abs() > crate::model::kernel::Z_TOL {
                            anyhow::bail!("Selected polygon edges cross in XY at conflicting elevations ({:.3} and {edge_z:.3})", existing.get());
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(vacant) => {
                        vacant.insert(edge_z);
                    }
                }
            }
        }
    }

    let mut indexed: Vec<(usize, f64, f64, f64)> = cdt
        .vertices()
        .map(|v| {
            let idx = v.fix().index();
            let p = v.position();
            (idx, p.x, p.y, handle_z.get(&idx).copied().unwrap_or(ring[0].z))
        })
        .collect();
    indexed.sort_unstable_by_key(|(idx, ..)| *idx);
    let verts: Vec<mesh_data::Vertex> = indexed.iter().map(|(_, x, y, z)| mesh_data::Vertex::new(*x, *y, *z)).collect();

    // Filter to faces whose centroid lies inside the input ring so that concave
    // polygons don't include CDT faces outside the boundary.
    let ring_xy: Vec<(f64, f64)> = ring.iter().map(|v| (v.x, v.y)).collect();
    let faces: Vec<[u32; 3]> = cdt
        .inner_faces()
        .filter(|f| {
            let vs = f.vertices();
            let cx = (vs[0].position().x + vs[1].position().x + vs[2].position().x) / 3.0;
            let cy = (vs[0].position().y + vs[1].position().y + vs[2].position().y) / 3.0;
            point_in_polygon_xy(cx, cy, &ring_xy)
        })
        .map(|f| {
            let v = f.vertices();
            let [a, b, c] = [v[0].fix().index() as u32, v[1].fix().index() as u32, v[2].fix().index() as u32];
            if flip_winding { [a, c, b] } else { [a, b, c] }
        })
        .collect();

    if faces.is_empty() {
        anyhow::bail!("Failed to triangulate polygon (may be degenerate or collinear)");
    }
    Ok((verts, faces))
}

fn validate_single_ring_crossing_z(ring: &[glam::DVec3]) -> Result<()> {
    for edge_index in 0..ring.len() {
        let a = ring[edge_index];
        let b = ring[(edge_index + 1) % ring.len()];
        for other_edge_index in edge_index + 1..ring.len() {
            let c = ring[other_edge_index];
            let d = ring[(other_edge_index + 1) % ring.len()];
            if let Some(detail) = conflicting_z_detail(a, b, c, d) {
                return Err(anyhow::Error::new(BreaklineConflictFailure {
                    message: format!("Selected polygon edges cross in XY at conflicting elevations (edge {edge_index} vs edge {other_edge_index}: {detail})"),
                    edges: [[a, b], [c, d]],
                    kind: BreaklineConflictKind::ConflictingElevation,
                }));
            }
        }
    }
    Ok(())
}
