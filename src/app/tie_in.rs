//! Drill & Blast's tie-in: laying the surface connectors between holes, and
//! saying which hole the round starts at.
//!
//! A tie-in is drawn the way a row is walked. The first click anchors the
//! chain on a hole; from there the run to the pointer is previewed with every
//! hole standing in its visible corridor tied into it, and confirming lays those legs
//! and moves the anchor to the far end so the next leg carries on from it. The
//! chain is put down with Escape or a right click.
//!
//! What is previewed is what is laid: [`App::refresh_tie_preview`] is the only
//! thing that works out the legs, the overlay draws that list, and the commit
//! reads the same list back.

use crate::{
    app::{App, PICK_THRESHOLD_PX},
    i18n::{tr, tr_format},
    logging::CommandReportSpec,
    model::{
        Command,
        drill_hole::{DrillHoleRef, Initiation, OpenDrillHoleDataset, TieIn},
    },
    rendering::graphics::projections::tie_chain_between,
    ui::state::{BlastRoundSummary, InitiationDialog, SelectionMode, TieInRef, TiePreviewLeg, Workspace},
};

impl App<'_> {
    /// The dataset the workspace is tying in, if it is loaded and drawn.
    fn tie_target(&self) -> Option<&OpenDrillHoleDataset> {
        let id = self.editor.active_drill_hole?;
        let entity = crate::model::SceneEntityId::DrillHole(id);
        self.drill_holes
            .iter()
            .find(|dataset| dataset.id == id && dataset.state.loaded && !self.editor.hidden_handles.contains(&entity) && !self.editor.frozen_handles.contains(&entity))
    }

    fn pick_hole_at_cursor(&self) -> Option<DrillHoleRef> {
        let graphics = self.graphics.as_ref()?;
        let cursor = self.editor.cursor_screen_px?;
        let view_proj = graphics.view_proj();
        let mut best: Option<(f32, DrillHoleRef)> = None;
        for dataset in self.selectable_drill_holes() {
            let entity = dataset.entity_id();
            if !dataset.state.loaded || self.editor.hidden_handles.contains(&entity) || self.editor.frozen_handles.contains(&entity) {
                continue;
            }
            for (index, hole) in dataset.dataset.holes.iter().enumerate() {
                let Some(position) = graphics.world_to_window_px(&view_proj, hole.collar_position()) else {
                    continue;
                };
                let distance = (cursor.0 - position.0).hypot(cursor.1 - position.1);
                if distance <= PICK_THRESHOLD_PX && best.is_none_or(|(best_distance, _)| distance < best_distance) {
                    best = Some((distance, DrillHoleRef { dataset: dataset.id, hole: index }));
                }
            }
        }
        best.map(|(_, hole)| hole)
    }

    /// The legs a tie-in click would act on from where the chain stands now:
    /// what it would lay in the visible screen-space corridor.
    fn tie_preview_legs(&self) -> Vec<TiePreviewLeg> {
        if !self.editor.tying_holes() {
            return Vec::new();
        }
        let (Some(anchor), Some(cursor_px), Some(graphics), Some(dataset)) = (self.editor.tie_anchor, self.editor.cursor_screen_px, self.graphics.as_ref(), self.tie_target())
        else {
            return Vec::new();
        };
        if anchor.dataset != dataset.id {
            return Vec::new();
        }
        let holes = &dataset.dataset.holes;
        let view_proj = graphics.view_proj();
        let chain = tie_chain_between(holes, anchor.hole, cursor_px, |world| graphics.world_to_window_px(&view_proj, world));
        chain
            .windows(2)
            .filter_map(|pair| {
                let [from, to] = [pair[0], pair[1]];
                Some(TiePreviewLeg {
                    from,
                    to,
                    start: holes.get(from)?.collar_position(),
                    end: holes.get(to)?.collar_position(),
                    overwrite: dataset.dataset.tie_between(from, to).is_some(),
                })
            })
            .collect()
    }

    /// Bring the previewed legs up to date with the cursor and the camera.
    /// Called once a frame, and again before a click commits, so the two can
    /// never disagree about what a chain is about to tie.
    pub(crate) fn refresh_tie_preview(&mut self) {
        let anchor_world = self
            .editor
            .tying_holes()
            .then(|| {
                let anchor = self.editor.tie_anchor?;
                let dataset = self.tie_target().filter(|dataset| dataset.id == anchor.dataset)?;
                Some(dataset.dataset.holes.get(anchor.hole)?.collar_position())
            })
            .flatten();
        let path_end_world = if anchor_world.is_some() {
            self.graphics.as_ref().and_then(|graphics| graphics.cursor_world(self.editor.z_level))
        } else {
            None
        };
        let legs = self.tie_preview_legs();
        if legs != self.editor.tie_preview || anchor_world != self.editor.tie_anchor_world || path_end_world != self.editor.tie_path_end_world {
            self.editor.tie_preview = legs;
            self.editor.tie_anchor_world = anchor_world;
            self.editor.tie_path_end_world = path_end_world;
            self.invalidate_overlay();
        }
    }

    /// Work out what the active dataset's tie-in adds up to, for the products
    /// panel to read back: where the round starts, how many connectors it has,
    /// how long it runs, and what it never reaches.
    ///
    /// Walked only when the pattern's content has moved on - the revision is
    /// the key - so a dataset of any size is walked once per edit rather than
    /// once per frame.
    pub(crate) fn refresh_blast_round(&mut self) {
        let dataset = self.tie_target();
        let key = dataset.map(|dataset| (dataset.id.0, dataset.state.revision()));
        if key == self.editor.blast_round_key {
            return;
        }
        let summary = dataset.map_or_else(BlastRoundSummary::default, |dataset| {
            let data = &dataset.dataset;
            let times = data.firing_times();
            BlastRoundSummary {
                initiations: data
                    .initiations
                    .iter()
                    .filter_map(|initiation| Some((data.holes.get(initiation.hole)?.dhid.clone(), initiation.delay_ms)))
                    .collect(),
                connectors: data.ties.len(),
                duration_ms: times.iter().flatten().copied().max(),
                // Nothing is unreached until there is somewhere for a signal
                // to come from: a pattern with no initiation point is not a
                // round that fails to reach its holes.
                unreached: if !data.initiations.is_empty() {
                    times.iter().filter(|time| time.is_none()).count()
                } else {
                    0
                },
            }
        });
        self.editor.blast_round_key = key;
        self.editor.blast_round = summary;
    }

    /// Put down the running chain, if there is one.
    pub(crate) fn end_tie_chain(&mut self) {
        if self.editor.end_tie_chain() {
            self.invalidate_overlay();
        }
    }

    /// A left click in the scene with the Tie Holes tool armed: anchor a
    /// chain, or confirm the legs standing previewed under the pointer.
    pub(crate) fn tie_holes_click(&mut self) {
        let product = self.editor.active_product().cloned();
        if product.is_none() {
            crate::userspace_warn!("{}", tr!(literal = "Select a delay product in the palette before tying holes in"));
            return;
        }
        if self.tie_target().is_none() {
            crate::userspace_warn!("{}", tr!(literal = "Choose the drillhole dataset to tie in first"));
            return;
        }
        if self.editor.tie_anchor.is_none() {
            // A chain starts on a hole: a click on nothing has no run to
            // measure the next one along.
            if let Some(hole) = self.pick_hole_at_cursor() {
                self.editor.tie_anchor = Some(hole);
                self.refresh_tie_preview();
                self.redraw_requested = true;
            }
            return;
        }

        self.refresh_tie_preview();
        let legs = self.editor.tie_preview.clone();
        let (Some(dataset), Some(last)) = (self.tie_target(), legs.last()) else {
            return;
        };
        let dataset_id = dataset.id;
        let far_end = last.to;
        let [red, green, blue, _] = product.as_ref().map_or([255, 255, 255, 255], |product| product.color.to_srgba_unmultiplied());
        let before: Vec<TieIn> = legs.iter().filter_map(|leg| dataset.dataset.tie_between(leg.from, leg.to).cloned()).collect();
        let after: Vec<TieIn> = match product.as_ref() {
            None => Vec::new(),
            Some(product) => legs
                .iter()
                .map(|leg| TieIn {
                    from: leg.from,
                    to: leg.to,
                    delay_ms: product.delay_ms,
                    product: product.name.clone(),
                    color: [f32::from(red) / 255.0, f32::from(green) / 255.0, f32::from(blue) / 255.0],
                })
                .collect(),
        };
        let laid = after.len();
        let replaced = before.len();
        self.execute_edit(Command::SetTieIns {
            dataset: dataset_id,
            before,
            after,
        });
        // The chain carries on from where this leg finished, so a row is tied
        // in one click per leg rather than one click per end.
        self.editor.tie_anchor = Some(DrillHoleRef {
            dataset: dataset_id,
            hole: far_end,
        });
        self.refresh_tie_preview();
        let Some(product) = product else {
            return;
        };
        let detail = if replaced > 0 {
            tr_format!(
                literal = "Tied %count% connector(s) at %delay% ms with %product%, replacing %replaced%",
                count = laid,
                delay = product.delay_ms,
                product = &product.name,
                replaced = replaced
            )
        } else {
            tr_format!(
                literal = "Tied %count% connector(s) at %delay% ms with %product%",
                count = laid,
                delay = product.delay_ms,
                product = &product.name
            )
        };
        crate::logging::report_completed_action(
            CommandReportSpec::new(tr!(literal = "Tie Holes"), tr_format!(literal = "%count% connector(s)", count = laid)),
            detail,
        );
    }

    /// A left click with the Set Initiation Point tool active opens an editor
    /// for that collar. Nothing changes until the dialog is confirmed.
    pub(crate) fn set_initiation_at_cursor(&mut self) {
        let Some(hole) = self.pick_hole_at_cursor() else {
            return;
        };
        let Some(dataset) = self.drill_holes.iter().find(|dataset| dataset.id == hole.dataset) else {
            return;
        };
        let name = dataset.dataset.holes.get(hole.hole).map_or_else(|| tr!(literal = "hole"), |hole| hole.dhid.clone());
        let existing = dataset.dataset.initiations.iter().find(|initiation| initiation.hole == hole.hole).copied();
        self.editor.initiation_dialog = Some(InitiationDialog {
            target: hole,
            hole_name: name,
            delay_ms: existing.map_or(0, |initiation| initiation.delay_ms),
            existing: existing.is_some(),
        });
        self.redraw_requested = true;
    }

    /// Apply the initiation dialog's result to one collar, preserving all
    /// other starts in the round.
    pub(crate) fn set_initiation(&mut self, target: DrillHoleRef, delay_ms: Option<u32>) {
        let entity = crate::model::SceneEntityId::DrillHole(target.dataset);
        if self.editor.hidden_handles.contains(&entity) || self.editor.frozen_handles.contains(&entity) {
            return;
        }
        let Some(dataset) = self.drill_holes.iter().find(|dataset| dataset.id == target.dataset && dataset.state.loaded) else {
            return;
        };
        let before = dataset.dataset.initiations.iter().find(|initiation| initiation.hole == target.hole).copied();
        let after = delay_ms.map(|delay_ms| Initiation { hole: target.hole, delay_ms });
        if before == after {
            return;
        }
        let name = dataset.dataset.holes.get(target.hole).map_or_else(|| tr!(literal = "hole"), |hole| hole.dhid.clone());
        self.execute_edit(Command::SetInitiation {
            dataset: target.dataset,
            before,
            after,
        });
        let detail = match after {
            Some(initiation) => tr_format!(literal = "Initiation point set on %name% at %delay% ms", name = &name, delay = initiation.delay_ms),
            None => tr_format!(literal = "Initiation point lifted from %name%", name = &name),
        };
        crate::logging::report_completed_action(CommandReportSpec::new(tr!(literal = "Set Initiation Point"), name), detail);
    }

    /// Select the nearest visible tie under the pointer. Returns `false` when
    /// the ordinary scene picker should handle the click instead.
    pub(crate) fn select_tie_at_cursor(&mut self) -> bool {
        if self.editor.active_workspace != Workspace::DrillAndBlast {
            return false;
        }
        let (Some(cursor), Some(graphics)) = (self.editor.cursor_screen_px, self.graphics.as_ref()) else {
            return false;
        };
        let view_proj = graphics.view_proj();
        let mut best: Option<(f32, TieInRef)> = None;
        for dataset in self.selectable_drill_holes() {
            let entity = dataset.entity_id();
            if !dataset.state.loaded || self.editor.hidden_handles.contains(&entity) || self.editor.frozen_handles.contains(&entity) {
                continue;
            }
            for tie in &dataset.dataset.ties {
                let (Some(from), Some(to)) = (dataset.dataset.holes.get(tie.from), dataset.dataset.holes.get(tie.to)) else {
                    continue;
                };
                let (Some(a), Some(b)) = (
                    graphics.world_to_window_px(&view_proj, from.collar_position()),
                    graphics.world_to_window_px(&view_proj, to.collar_position()),
                ) else {
                    continue;
                };
                let (distance, along, length) = point_segment_hit(cursor, a, b);
                // Leave the collar-sized ends to ordinary hole selection. A
                // connector is selected from its visible run, not by stealing
                // a click aimed at either hole it joins.
                let clear_of_collars = along * length >= 7.0 && (1.0 - along) * length >= 7.0;
                if clear_of_collars && distance <= 8.0 && best.is_none_or(|(best_distance, _)| distance < best_distance) {
                    best = Some((distance, TieInRef::new(dataset.id, tie.from, tie.to)));
                }
            }
        }
        let Some((_, picked)) = best else {
            return false;
        };
        let mode = if self.modifiers.shift_key() {
            SelectionMode::Toggle
        } else if self.modifiers.control_key() {
            SelectionMode::Add
        } else if self.editor.selected_tie_ins.contains(&picked) {
            SelectionMode::Toggle
        } else {
            SelectionMode::Replace
        };
        match mode {
            SelectionMode::Replace => {
                self.editor.clear_scene_selection();
                self.editor.selected_tie_ins.insert(picked);
            }
            SelectionMode::Add => {
                self.editor.selected_tie_ins.insert(picked);
            }
            SelectionMode::Toggle => {
                if !self.editor.selected_tie_ins.remove(&picked) {
                    self.editor.selected_tie_ins.insert(picked);
                }
            }
        }
        self.redraw_requested = true;
        true
    }

    /// Delete every selected connector as one undo step.
    pub(crate) fn delete_selected_tie_ins(&mut self) {
        let selected = self.editor.selected_tie_ins.clone();
        let commands: Vec<_> = self
            .drill_holes
            .iter()
            .filter_map(|dataset| {
                let before: Vec<_> = dataset
                    .dataset
                    .ties
                    .iter()
                    .filter(|tie| selected.contains(&TieInRef::new(dataset.id, tie.from, tie.to)))
                    .cloned()
                    .collect();
                (!before.is_empty()).then_some(Command::SetTieIns {
                    dataset: dataset.id,
                    before,
                    after: Vec::new(),
                })
            })
            .collect();
        let count = commands
            .iter()
            .map(|command| match command {
                Command::SetTieIns { before, .. } => before.len(),
                _ => 0,
            })
            .sum::<usize>();
        if commands.is_empty() {
            self.editor.selected_tie_ins.clear();
            return;
        }
        self.execute_edit(Command::Batch(commands));
        self.editor.selected_tie_ins.clear();
        crate::logging::report_completed_action(
            CommandReportSpec::new(tr!(literal = "Delete Tie-Ins"), tr_format!(literal = "%count% connector(s)", count = count)),
            tr_format!(literal = "Deleted %count% selected tie-in connector(s)", count = count),
        );
    }
}

fn point_segment_hit(point: (f32, f32), a: (f32, f32), b: (f32, f32)) -> (f32, f32, f32) {
    let ab = (b.0 - a.0, b.1 - a.1);
    let length_sq = ab.0 * ab.0 + ab.1 * ab.1;
    if length_sq <= f32::EPSILON {
        return ((point.0 - a.0).hypot(point.1 - a.1), 0.0, 0.0);
    }
    let t = (((point.0 - a.0) * ab.0 + (point.1 - a.1) * ab.1) / length_sq).clamp(0.0, 1.0);
    ((point.0 - (a.0 + t * ab.0)).hypot(point.1 - (a.1 + t * ab.1)), t, length_sq.sqrt())
}
