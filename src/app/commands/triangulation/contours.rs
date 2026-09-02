use std::collections::{HashMap, VecDeque};

use super::{cuts::lerp_at_z, *};

impl<'a> App<'a> {
    /// Generate contour polylines from a triangulation and store them in a new
    /// or existing layer in the active project. Major contours (multiples
    /// of `major_interval`) use `major_color`; all others use `minor_color`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn generate_contour_triangulation(
        &mut self,
        tri_id: TriangulationId,
        major_interval: f64,
        minor_interval: f64,
        major_color: [f32; 4],
        minor_color: [f32; 4],
        z_range: Option<(f64, f64)>,
        output_layer: ContourOutputLayer,
    ) -> Result<()> {
        if !minor_interval.is_finite() || !major_interval.is_finite() || minor_interval <= 0.0 || major_interval <= 0.0 {
            anyhow::bail!("Intervals must be positive finite numbers");
        }
        if minor_interval < 1e-6 {
            anyhow::bail!("Minor contour interval is too small (minimum 0.000001); this would generate an unbounded number of contour levels");
        }
        if major_interval < minor_interval {
            anyhow::bail!("Major interval must be >= minor interval");
        }

        let (mesh, tri_name) = {
            let tri = self
                .triangulations
                .iter()
                .find(|t| t.id == tri_id)
                .ok_or_else(|| anyhow::anyhow!("Triangulation not found"))?;
            (tri.mesh.clone(), tri.name.clone())
        };

        let bounds = mesh.bounds();
        let mut z_lo = bounds.min.z;
        let mut z_hi = bounds.max.z;
        if let Some((range_lo, range_hi)) = z_range {
            if !range_lo.is_finite() || !range_hi.is_finite() || range_lo >= range_hi {
                anyhow::bail!("Contour Z range must be finite with min < max");
            }
            z_lo = z_lo.max(range_lo);
            z_hi = z_hi.min(range_hi);
            if z_lo >= z_hi {
                anyhow::bail!("Contour Z range does not overlap the triangulation");
            }
        }

        if (z_hi - z_lo).abs() < 1e-10 {
            anyhow::bail!("Triangulation has no Z extent to contour");
        }

        const MAX_CONTOUR_LEVELS: f64 = 100_000.0;
        let level_estimate = (z_hi - z_lo) / minor_interval;
        if level_estimate > MAX_CONTOUR_LEVELS {
            anyhow::bail!(
                "These settings would generate about {level_estimate:.0} contour levels over a Z extent of {:.1} m. \
                 Increase the minor interval, or restrict the Z range to contour a slice of the triangulation.",
                z_hi - z_lo
            );
        }

        let active_project = self
            .workspace
            .active_project()
            .ok_or_else(|| anyhow::anyhow!("No active project to store the contours in"))?;
        match &output_layer {
            ContourOutputLayer::New(name) => {
                if name.trim().is_empty() {
                    anyhow::bail!("Enter a name for the new contour layer");
                }
                if active_project.project.document.layer_id_by_name(name).is_some() {
                    anyhow::bail!("A layer named '{name}' already exists; select it as the output layer or choose a different name");
                }
            }
            ContourOutputLayer::Existing(layer_id) => {
                if active_project.project.document.layer(*layer_id).is_none() {
                    anyhow::bail!("The selected contour output layer no longer exists");
                }
            }
        }

        // Snapshot the project identity so the apply step can verify the
        // contours still have a home when the job completes.
        let project_runtime_id = active_project.runtime_id;

        let apply_tri_name = tri_name.clone();
        let compute = move |cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress| -> Result<Vec<(Vec<glam::DVec3>, crate::model::ObjectColor)>> {
            compute_contour_polylines(&mesh, z_lo, z_hi, minor_interval, major_interval, major_color, minor_color, cancel, progress)
        };
        let apply = move |app: &mut App, result: Result<Vec<(Vec<glam::DVec3>, crate::model::ObjectColor)>>| {
            let polylines = match result {
                Ok(polylines) => polylines,
                Err(error) => {
                    userspace_warn!("{}", tr_format!(literal = "Contour generation failed: %error%", error = format!("{error:#}")));
                    return;
                }
            };
            let project_is_active = app.workspace.active_project().is_some_and(|project| project.runtime_id == project_runtime_id);
            let Some(project) = app.workspace.projects.iter_mut().find(|project| project.runtime_id == project_runtime_id) else {
                userspace_warn!(
                    "{}",
                    tr_format!(literal = "Contours for '%name%' were discarded: the project was closed", name = apply_tri_name)
                );
                return;
            };
            let (layer_id, layer_name, create_layer) = match output_layer {
                ContourOutputLayer::New(layer_name) => {
                    if project.project.document.layer_id_by_name(&layer_name).is_some() {
                        userspace_warn!(
                            "{}",
                            tr_format!(
                                literal = "Contours for '%name%' were discarded: layer '%layer_name%' now exists",
                                name = apply_tri_name,
                                layer_name = layer_name
                            )
                        );
                        return;
                    }
                    let layer_id = project.project.document.allocate_layer_id();
                    (layer_id, layer_name, true)
                }
                ContourOutputLayer::Existing(layer_id) => {
                    let Some(layer_name) = project.project.document.layer(layer_id).map(|layer| layer.name.clone()) else {
                        userspace_warn!(
                            "{}",
                            tr_format!(
                                literal = "Contours for '%name%' were discarded: the selected output layer was deleted",
                                name = apply_tri_name
                            )
                        );
                        return;
                    };
                    (layer_id, layer_name, false)
                }
            };
            let objects: Vec<Object> = polylines
                .into_iter()
                .map(|(verts, color)| Object::Polyline {
                    id: project.project.document.allocate_object_id(),
                    layer: layer_id,
                    verts: verts.into_iter().map(crate::model::PolyVertex::straight).collect(),
                    closed: false,
                    color,
                    fill: crate::model::FillStyle::Clear,
                    line_weight: 1.0,
                })
                .collect();
            let line_count = objects.len();
            let command = if create_layer {
                let layer = Layer {
                    id: layer_id,
                    name: layer_name.clone(),
                    color_index: None,
                    color: [1.0, 1.0, 1.0, 1.0],
                    visible: true,
                    elevation: 0.0,
                };
                crate::model::Command::AddLayerSnapshot { layer, objects }
            } else {
                crate::model::Command::Batch(objects.into_iter().map(crate::model::Command::AddObject).collect())
            };
            app.execute_edit_for(project_runtime_id, command);
            if let Some(project) = app.workspace.active_project_mut() {
                project.loaded_layers.insert(layer_id);
            }
            if project_is_active {
                app.editor.active_layer = Some(layer_id);
            }
            userspace_log!(
                "{}",
                tr_format!(
                    literal = "Generated %line_count% contour polyline(s) for triangulation '%name%' in layer '%layer_name%'",
                    line_count = line_count,
                    name = apply_tri_name,
                    layer_name = layer_name
                )
            );
            app.invalidate_geometry();
        };
        self.spawn_job_reporting_progress("Generating contours…", vec![crate::app::jobs::JobKey::Triangulation(tri_id)], compute, apply);
        Ok(())
    }
}

/// Contour a mesh with a Z sweep: faces are sorted by minimum Z once and an
/// active-face set follows the ascending levels, so intermediate memory stays
/// O(faces + active faces + output) instead of copying each face index into
/// every level bucket it crosses (tall/vertical triangles made that
/// O(face-level intersections)).
#[allow(clippy::too_many_arguments)]
fn compute_contour_polylines(
    mesh: &mesh_data::Triangulation,
    z_lo: f64,
    z_hi: f64,
    minor_interval: f64,
    major_interval: f64,
    major_color: [f32; 4],
    minor_color: [f32; 4],
    cancel: &crate::app::jobs::CancelFlag,
    progress: &crate::model::progress::Progress,
) -> Result<Vec<(Vec<glam::DVec3>, crate::model::ObjectColor)>> {
    let verts_raw = mesh.vertices();
    let mut contour_faces: Vec<ContourFace> = mesh
        .face_vertex_indices_iter()
        .map(|face| {
            let vertices = [verts_raw[face[0]], verts_raw[face[1]], verts_raw[face[2]]];
            let z_min = vertices.iter().map(|vertex| vertex.z).fold(f64::INFINITY, f64::min);
            let z_max = vertices.iter().map(|vertex| vertex.z).fold(f64::NEG_INFINITY, f64::max);
            ContourFace { vertices, z_min, z_max }
        })
        .collect();
    contour_faces.sort_by(|a, b| a.z_min.total_cmp(&b.z_min));

    let levels = contour_levels(z_lo, z_hi, minor_interval, major_interval);
    // One pass per level, so levels finished is an exact measure of the work.
    let level_count = levels.len() as u64;
    let mut polylines: Vec<(Vec<glam::DVec3>, crate::model::ObjectColor)> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    let mut next_face = 0usize;

    for (levels_done, z_level) in levels.into_iter().enumerate() {
        if cancel.is_cancelled() {
            anyhow::bail!("Cancelled");
        }
        progress.set_items(levels_done as u64, level_count);
        // Admit faces whose span has reached this level, then retire the ones
        // it has passed. A face crosses the level when z_min <= level < z_max.
        while next_face < contour_faces.len() && contour_faces[next_face].z_min <= z_level {
            active.push(next_face);
            next_face += 1;
        }
        active.retain(|&face_index| contour_faces[face_index].z_max > z_level);

        let is_major = is_major_contour(z_level, major_interval);
        let color = if is_major { major_color } else { minor_color };
        let line_color = crate::model::ObjectColor::Fixed(color);
        let mut level_segments = Vec::new();
        for &face_index in &active {
            let face = &contour_faces[face_index];
            if let Some(seg) = triangle_contour_segment(face.vertices, z_level) {
                level_segments.push([glam::DVec3::new(seg[0].x, seg[0].y, seg[0].z), glam::DVec3::new(seg[1].x, seg[1].y, seg[1].z)]);
            }
        }
        polylines.extend(chain_contour_segments(level_segments).into_iter().map(|verts| (verts, line_color)));
    }

    if polylines.is_empty() {
        anyhow::bail!("No contour segments were generated for the selected intervals");
    }
    Ok(polylines)
}

struct ContourFace {
    vertices: [mesh_data::Vertex; 3],
    z_min: f64,
    z_max: f64,
}

fn contour_levels(z_lo: f64, z_hi: f64, minor_interval: f64, major_interval: f64) -> Vec<f64> {
    let mut levels = Vec::new();
    append_levels(&mut levels, z_lo, z_hi, minor_interval);
    append_levels(&mut levels, z_lo, z_hi, major_interval);
    levels.sort_by(f64::total_cmp);
    levels.dedup_by(|a, b| (*a - *b).abs() <= 1e-8);
    levels
}

fn append_levels(levels: &mut Vec<f64>, z_lo: f64, z_hi: f64, interval: f64) {
    let first = (z_lo / interval).ceil();
    let last = (z_hi / interval).floor();
    if first > last {
        return;
    }
    let count = (last - first) as usize;
    for i in 0..=count {
        levels.push((first + i as f64) * interval);
    }
}

fn is_major_contour(z_level: f64, major_interval: f64) -> bool {
    let nearest = (z_level / major_interval).round() * major_interval;
    (z_level - nearest).abs() <= 1e-6
}

/// Quantized endpoint key. Both triangles adjacent to a mesh edge interpolate
/// the crossing from the same two vertices, so matching endpoints agree to
/// float noise; quantizing at the kernel tolerance makes them hash-equal.
fn contour_point_key(p: glam::DVec3) -> (i64, i64, i64) {
    let q = crate::model::kernel::XY_TOL;
    ((p.x / q).round() as i64, (p.y / q).round() as i64, (p.z / q).round() as i64)
}

fn chain_contour_segments(segments: Vec<[glam::DVec3; 2]>) -> Vec<Vec<glam::DVec3>> {
    // Endpoint key -> segments touching that point, for O(1) chain extension.
    let mut by_endpoint: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    for (index, segment) in segments.iter().enumerate() {
        by_endpoint.entry(contour_point_key(segment[0])).or_default().push(index);
        by_endpoint.entry(contour_point_key(segment[1])).or_default().push(index);
    }

    let mut used = vec![false; segments.len()];
    let mut chains = Vec::new();

    let mut take_neighbor = |used: &mut Vec<bool>, point: glam::DVec3| -> Option<glam::DVec3> {
        let candidates = by_endpoint.get_mut(&contour_point_key(point))?;
        while let Some(index) = candidates.pop() {
            if used[index] {
                continue;
            }
            used[index] = true;
            let [a, b] = segments[index];
            let key = contour_point_key(point);
            return Some(if contour_point_key(a) == key { b } else { a });
        }
        None
    };

    for seed in 0..segments.len() {
        if used[seed] {
            continue;
        }
        used[seed] = true;
        let [a, b] = segments[seed];
        let mut chain: VecDeque<glam::DVec3> = VecDeque::from([a, b]);
        while let Some(next) = take_neighbor(&mut used, *chain.back().unwrap()) {
            chain.push_back(next);
        }
        while let Some(previous) = take_neighbor(&mut used, *chain.front().unwrap()) {
            chain.push_front(previous);
        }
        if chain.len() >= 2 {
            chains.push(chain.into_iter().collect());
        }
    }
    chains
}

pub(super) fn triangle_contour_segment(v: [mesh_data::Vertex; 3], z_level: f64) -> Option<[mesh_data::Vertex; 2]> {
    let mut pts: Vec<mesh_data::Vertex> = Vec::with_capacity(2);
    for i in 0..3 {
        let a = v[i];
        let b = v[(i + 1) % 3];
        if (a.z <= z_level && z_level < b.z) || (b.z <= z_level && z_level < a.z) {
            pts.push(lerp_at_z(a, b, z_level));
        }
    }
    if pts.len() == 2 { Some([pts[0], pts[1]]) } else { None }
}
