//! Flitch-owned strip drawings and the dig blocks formed jointly with blasts.
use std::hash::{DefaultHasher, Hash, Hasher};

use glam::{DVec2, DVec3};

use crate::{
    model::{Command, Object, SceneEntityId, arrangement},
    ui::state::{BenchSelection, BlastOutline},
};

/// The plan faces one flitch's ground is divided into, north-west first.
///
/// A flitch is cut by its own strips *and* by the blast boundaries of the bench
/// holding it, so a strip crossing a blast boundary yields one block per blast.
/// The bench's own cut lines stand in for those boundaries: a blast boundary is
/// made of bench cuts and stretches of the bench footprint, and the bench
/// footprint lies outside this flitch's, the pit narrowing as it goes down.
///
/// Shared by the derivation the panel reads and the geometry job that cuts the
/// flitch solid apart, so the blocks drawn and the blocks measured are the same
/// blocks.
pub(crate) fn dig_block_faces(footprint: &[Vec<DVec2>], strips: &[Object], bench_cuts: &[Object]) -> Vec<(arrangement::Face, DVec2)> {
    let mut cuts = arrangement::cut_lines(strips);
    cuts.extend(arrangement::cut_lines(bench_cuts));
    let mut faces: Vec<_> = arrangement::subdivide(footprint, &cuts)
        .into_iter()
        .filter_map(|face| arrangement::representative_point(&face).map(|anchor| (face, anchor)))
        .collect();
    // Numbered by where they sit, so a block keeps its number while its
    // neighbours are redrawn.
    faces.sort_by(|a, b| b.1.y.total_cmp(&a.1.y).then_with(|| a.1.x.total_cmp(&b.1.x)));
    faces
}

impl crate::app::App<'_> {
    pub(crate) fn copy_dig_strips(&mut self) {
        if !self.editor.is_dig_strips_step() {
            return;
        }
        self.editor.dig_clipboard = self
            .scene_document
            .objects()
            .iter()
            .filter(|object| self.editor.selected_handles.contains(&SceneEntityId::Object(object.id())))
            .filter(|object| matches!(object, Object::Polyline { .. }))
            .cloned()
            .collect();
        self.redraw_requested = true;
    }

    pub(crate) fn paste_dig_strips(&mut self) {
        if !self.editor.is_dig_strips_step() || self.editor.dig_clipboard.is_empty() {
            return;
        }
        let Some((_, band)) = self.editor.planning_cut_target() else { return };
        let Some(layer) = self.ensure_bench_cut_layer() else { return };
        let mut commands = Vec::new();
        let mut selected = std::collections::HashSet::new();
        let Some(document) = self.workspace.active_document_mut() else { return };
        for source in &self.editor.dig_clipboard {
            let id = document.allocate_object_id();
            let mut object = source.with_id_and_layer(id, layer);
            if let Object::Polyline { verts, .. } = &mut object {
                for vertex in verts {
                    vertex.pos.z = band.top;
                }
            }
            selected.insert(SceneEntityId::Object(id));
            commands.push(Command::AddObject(object));
        }
        self.execute_edit(Command::Batch(commands));
        self.editor.selected_handles = selected;
        self.invalidate_geometry();
    }

    pub(crate) fn sync_dig_blocks(&mut self) {
        // View shows the blocks as cut-apart geometry, so the derived outlines
        // are only needed by the steps that draw and list them.
        if !self.editor.is_planning_cut_step() {
            if !self.editor.dig_outlines.is_empty() {
                self.editor.dig_outlines.clear();
                self.editor.dig_outlines_key = None;
                self.invalidate_overlay();
            }
            return;
        }
        let Some(document) = self.workspace.active_document() else { return };
        let mut hash = DefaultHasher::new();
        self.workspace.active_project().map(|project| project.runtime_id).hash(&mut hash);
        document.revision().hash(&mut hash);
        self.editor.is_dig_strips_step().hash(&mut hash);
        self.editor.selected_blast.hash(&mut hash);
        // Blast boundaries cut the strips, so a derivation that ran before the
        // blast outlines settled has to be redone once they change.
        self.editor.blasting_outlines_key.hash(&mut hash);
        for row in &self.editor.solids_view_selection {
            row.solid.hash(&mut hash);
            row.band.map(|b| (b.base.to_bits(), b.top.to_bits(), b.is_flitch)).hash(&mut hash);
        }
        for cache in self.solid_view_cache.values() {
            cache.key.hash(&mut hash);
            cache.is_built().hash(&mut hash);
        }
        let key = hash.finish();
        if self.editor.dig_outlines_key == Some(key) {
            return;
        }
        let mut outlines = Vec::new();
        for solid in document.solids() {
            let Some(geometry) = self.solid_view_cache.get(&solid.id).and_then(super::solids_view::ViewSolid::geometry) else {
                continue;
            };
            for bench in solid.benching.benches() {
                let blasts: Vec<_> = self
                    .editor
                    .blasting_outlines
                    .iter()
                    .filter(|blast| blast.solid == solid.id && (blast.bench_base - bench.base).abs() < 1e-6)
                    .collect();
                for flitch in &bench.flitches {
                    let band = BenchSelection {
                        base: flitch.base,
                        top: flitch.top(),
                        is_flitch: true,
                    };
                    if !super::solids_view::selected(&self.editor.solids_view_selection, solid.id, Some(band)) {
                        continue;
                    }
                    let drawing = solid.blasting.drawing(flitch.base, true);
                    if !self.editor.is_dig_strips_step() && drawing.is_none_or(|drawing| drawing.cuts.is_empty()) {
                        continue;
                    }
                    let Some(footprint) = geometry.flitch_footprints().get(&flitch.base.to_bits()) else {
                        continue;
                    };
                    let bench_cuts = solid.blasting.bench(bench.base).map(|entry| entry.cuts.as_slice()).unwrap_or_default();
                    let faces = dig_block_faces(footprint, drawing.map_or(&[][..], |drawing| &drawing.cuts), bench_cuts);
                    for (index, (face, anchor)) in faces.into_iter().enumerate() {
                        if self.editor.is_solids_view()
                            && let Some(selected) = self.editor.selected_blast
                        {
                            let Some(blast) = blasts
                                .iter()
                                .find(|blast| crate::ui::state::BlastShapeRef::new(blast.solid, blast.bench_base, blast.anchor) == selected)
                            else {
                                continue;
                            };
                            let blast_face: Vec<Vec<DVec2>> = blast.rings.iter().map(|ring| ring.iter().map(|p| p.truncate()).collect()).collect();
                            if !arrangement::point_in_face(&blast_face, anchor) {
                                continue;
                            }
                        }
                        let area = face
                            .iter()
                            .enumerate()
                            .map(|(i, ring)| arrangement::signed_area(ring).abs() * if i == 0 { 1.0 } else { -1.0 })
                            .sum();
                        outlines.push(BlastOutline {
                            solid: solid.id,
                            bench_base: flitch.base,
                            plane: flitch.top(),
                            name: (index + 1).to_string(),
                            anchor: anchor.to_array(),
                            area,
                            rings: face
                                .into_iter()
                                .map(|ring| ring.into_iter().map(|p| DVec3::new(p.x, p.y, flitch.top())).collect())
                                .collect(),
                        });
                    }
                }
            }
        }
        self.editor.dig_outlines = outlines;
        self.editor.dig_outlines_key = Some(key);
        self.invalidate_overlay();
    }
}
