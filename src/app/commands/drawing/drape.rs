use std::collections::HashSet;

use glam::DVec3;

use crate::{
    app::App,
    i18n::{tr, tr_format},
    logging::CommandReportSpec,
    model::{
        Command, Object, SceneEntityId,
        triangulation::{OpenTriangulation, TriangulationId},
    },
    ui::state::{ActiveTool, DrapePhase},
    userspace_warn,
};

impl<'a> App<'a> {
    /// Advance from design selection to topology selection, or apply the drape
    /// when both selection steps are complete.
    pub(crate) fn confirm_drape_selection(&mut self) {
        if self.editor.active_tool != ActiveTool::DrapeToTopology {
            return;
        }

        match self.editor.drape_phase {
            DrapePhase::Designs => {
                let active_object_ids = self.active_project_object_ids();
                let object_ids: Vec<_> = self
                    .editor
                    .selected_handles
                    .iter()
                    .filter_map(|handle| match handle {
                        SceneEntityId::Object(id) if active_object_ids.contains(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                if object_ids.is_empty() {
                    userspace_warn!("{}", tr!(literal = "Select one or more design objects to drape"));
                    return;
                }

                self.editor.drape_object_ids = object_ids;
                self.editor.drape_phase = DrapePhase::Topologies;
                self.editor.selected_handles.clear();
                self.invalidate_geometry();
            }
            DrapePhase::Topologies => {
                let loaded_ids: HashSet<_> = self.triangulations.iter().map(|topology| topology.id).collect();
                let topology_ids: Vec<_> = self
                    .editor
                    .selected_handles
                    .iter()
                    .filter_map(|handle| match handle {
                        SceneEntityId::Triangulation(id) if loaded_ids.contains(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                if topology_ids.is_empty() {
                    userspace_warn!("{}", tr!(literal = "Select one or more topologies to drape onto"));
                    return;
                }
                self.apply_drape_to_topologies(&topology_ids);
            }
        }
    }

    pub(crate) fn cancel_drape(&mut self) {
        self.editor.drape_phase = DrapePhase::Designs;
        self.editor.drape_object_ids.clear();
        self.editor.selection_box_start_px = None;
        self.editor.selection_box_current_px = None;
        self.editor.selected_handles.clear();
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_geometry();
    }

    fn apply_drape_to_topologies(&mut self, topology_ids: &[TriangulationId]) {
        let selected_ids: HashSet<_> = topology_ids.iter().copied().collect();
        let surfaces: Vec<_> = self.triangulations.iter().filter(|topology| selected_ids.contains(&topology.id)).collect();
        if surfaces.is_empty() {
            userspace_warn!("{}", tr!(literal = "The selected topologies are no longer loaded"));
            return;
        }

        let mut intersected_vertices = 0usize;
        let mut changed_vertices = 0usize;
        let replacements: Vec<_> = self
            .editor
            .drape_object_ids
            .iter()
            .filter_map(|id| {
                let before = self.active_document().get_object(*id)?.clone();
                let mut after = before.clone();
                let (intersected, changed) = drape_object(&mut after, &surfaces);
                intersected_vertices += intersected;
                changed_vertices += changed;
                (before != after).then_some(Command::Replace { before, after })
            })
            .collect();
        let changed_objects = replacements.len();

        if !replacements.is_empty()
            && let Some(project) = self.workspace.active_project_mut()
        {
            self.history.execute(&mut project.project.document, Command::Batch(replacements));
        }

        let selected_objects = self.editor.drape_object_ids.clone();
        self.editor.selected_handles = selected_objects.iter().copied().map(SceneEntityId::Object).collect();
        self.editor.drape_object_ids.clear();
        self.editor.drape_phase = DrapePhase::Designs;
        self.editor.active_tool = ActiveTool::None;
        self.invalidate_geometry();

        if intersected_vertices == 0 {
            userspace_warn!("{}", tr!(literal = "None of the selected design vertices intersect the selected topologies"));
        } else {
            crate::logging::report_completed_action(
                CommandReportSpec::new(
                    tr!(literal = "Drape to Topology"),
                    tr_format!(
                        literal = "%objects% changed object(s) · %changed% of %intersected% intersecting vertices moved",
                        objects = changed_objects,
                        changed = changed_vertices,
                        intersected = intersected_vertices
                    ),
                ),
                tr_format!(
                    literal = "Draped %intersected% vertices; %changed% changed elevation",
                    intersected = intersected_vertices,
                    changed = changed_vertices
                ),
            );
        }
    }
}

/// Move every stored design vertex vertically to the uppermost selected
/// topology at the same XY coordinate. A vertex outside all selected topology
/// footprints is deliberately left untouched.
fn drape_object(object: &mut Object, surfaces: &[&OpenTriangulation]) -> (usize, usize) {
    let mut intersected = 0usize;
    let mut changed = 0usize;
    let mut drape_point = |point: &mut DVec3| {
        let Some(z) = uppermost_topology_z(*point, surfaces) else {
            return;
        };
        intersected += 1;
        if point.z != z {
            point.z = z;
            changed += 1;
        }
    };

    match object {
        Object::Point { pos, .. } | Object::Text { pos, .. } => drape_point(pos),
        Object::Polyline { verts, .. } => {
            for vertex in verts {
                drape_point(&mut vertex.pos);
            }
        }
    }
    (intersected, changed)
}

fn uppermost_topology_z(point: DVec3, surfaces: &[&OpenTriangulation]) -> Option<f64> {
    surfaces
        .iter()
        .filter_map(|surface| {
            let bounds = surface.mesh.bounds();
            if point.x < bounds.min.x || point.x > bounds.max.x || point.y < bounds.min.y || point.y > bounds.max.y {
                return None;
            }
            let z_span = (bounds.max.z - bounds.min.z).abs();
            let origin_z = bounds.max.z + (z_span * 1.0e-6).max(1.0);
            surface.spatial.ray_hit(&surface.mesh, DVec3::new(point.x, point.y, origin_z), -DVec3::Z).map(|hit| hit.z)
        })
        .max_by(f64::total_cmp)
}
