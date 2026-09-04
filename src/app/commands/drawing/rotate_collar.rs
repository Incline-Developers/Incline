//! Drill & Blast's Rotate Collar tool: re-aim a pattern without re-laying it.
//!
//! Every selected hole swings about its own collar, so a round that was
//! surveyed onto the ground stays where it was surveyed and only the angles
//! change. The gizmo is two rings rather than three because a hole is a
//! cylinder: azimuth and dip are the whole of what a rig can be set to, and a
//! third ring would only offer a spin about the hole's own axis that no
//! drilled hole can tell apart from none at all.
//!
//! The captured-originals machinery is Move Collar's - see
//! [`super::move_tool`] - down to the one [`crate::app::CollarMoveSession`]
//! both gestures share, so a turn is settled, cancelled and rolled back by
//! every path a move already was.

use glam::DVec3;

use crate::{
    app::{App, CollarRotateDrag},
    logging::CommandReportSpec,
    model::{
        Command,
        drill_hole::{CollarRotation, DrillHoleId, HoleOrientation, HolePlacement, MAX_HOLE_DIP},
    },
    ui::state::ROTATE_GIZMO_AZIMUTH_RING,
};

impl<'a> App<'a> {
    /// Preview `rotation` over the selected holes without committing it.
    pub(crate) fn preview_collar_rotation(&mut self, rotation: CollarRotation) {
        self.ensure_collar_session();
        // The session is taken rather than borrowed so the originals can be
        // read while the datasets holding them are written, without cloning
        // every hole again on each frame of a drag.
        let Some(session) = self.collar_move_session.take() else {
            return;
        };
        self.write_collar_placements(&session.originals, |hole, original| hole.set_rotated_placement(original, rotation));
        let anchor = session.originals.first().and_then(|(_, placement)| placement.rotated_orientation(rotation));
        self.collar_move_session = Some(session);
        self.collar_rotation = Some(rotation);
        self.editor.rotate_preview_active = true;
        // The panel reads out what the holes now point at, so a ring drag
        // moves the numbers a driller would be handed rather than leaving them
        // showing where the selection started.
        if let Some(orientation) = anchor {
            self.editor.rotate_panel_azimuth = orientation.azimuth;
            self.editor.rotate_panel_dip = orientation.dip;
            self.editor.rotate_panel_last_preview = [orientation.azimuth, orientation.dip];
        }
        self.invalidate_topology_bounds_and_redraw();
    }

    /// Settle whatever turn is standing, which is what Apply and Enter mean.
    ///
    /// The two gestures produce different kinds of turn - a ring drag builds a
    /// delta, so a pattern that was not uniform keeps its spread; typing an
    /// angle sets an absolute, so every hole ends up on the one setup - and
    /// whichever was used last is what the viewport is showing. Committing the
    /// standing turn rather than re-reading the panel is what makes Apply mean
    /// "keep this", for both.
    pub(crate) fn apply_pending_collar_rotation(&mut self) {
        // With nothing previewed the panel's angles are the holes' own, so
        // this settles to no edit at all - which `apply_collar_rotation`
        // recognises and drops.
        let rotation = self.collar_rotation.unwrap_or(CollarRotation::Absolute(HoleOrientation {
            azimuth: self.editor.rotate_panel_azimuth,
            dip: self.editor.rotate_panel_dip,
        }));
        self.apply_collar_rotation(rotation);
    }

    /// Settle a turn at `rotation` as one undo step.
    ///
    /// The live preview is rolled back first - positions and epochs both - so
    /// the command applies from the state the gesture started in and undo has
    /// somewhere clean to return to. Applying it immediately rewrites the same
    /// positions the preview was already showing, so nothing moves on screen.
    /// This mirrors `apply_collar_move_delta` exactly; only the rule written
    /// into each hole differs.
    pub(crate) fn apply_collar_rotation(&mut self, rotation: CollarRotation) {
        self.ensure_collar_session();
        let Some(session) = self.collar_move_session.take() else {
            self.reset_rotate_editor_state();
            return;
        };
        // A project switch under a live preview leaves the holes where they
        // were rather than committing them against whatever is open now.
        let same_project = self.workspace.active_project().map(|project| project.runtime_id) == Some(session.project_runtime_id);
        self.write_collar_placements(&session.originals, |hole, original| hole.set_rotated_placement(original, CollarRotation::IDENTITY));
        self.restore_collar_epochs(&session.epochs);

        // A turn that changes no hole's orientation is not an edit. Asking the
        // holes rather than the angles catches the cases the angles cannot:
        // an absolute target the selection already points at, and a hole with
        // no length below its collar to aim.
        let turned = session
            .originals
            .iter()
            .filter(|(_, placement)| placement.rotated_orientation(rotation).is_some_and(|after| Some(after) != placement.orientation()))
            .count();
        if !same_project || turned == 0 {
            self.reset_rotate_editor_state();
            self.invalidate_topology_bounds_and_redraw();
            return;
        }

        let mut per_dataset: Vec<(DrillHoleId, Vec<(usize, HolePlacement)>)> = Vec::new();
        for (target, placement) in session.originals {
            match per_dataset.iter_mut().find(|(id, _)| *id == target.dataset) {
                Some((_, holes)) => holes.push((target.hole, placement)),
                None => per_dataset.push((target.dataset, vec![(target.hole, placement)])),
            }
        }
        let commands: Vec<Command> = per_dataset
            .into_iter()
            .map(|(dataset, originals)| Command::RotateCollars { dataset, originals, rotation })
            .collect();
        self.execute_edit(if commands.len() == 1 {
            commands.into_iter().next().expect("checked length")
        } else {
            Command::Batch(commands)
        });
        let described = crate::ui::state::describe_collar_rotation(rotation);
        crate::logging::report_completed_action(
            CommandReportSpec::new(
                crate::i18n::tr!(literal = "Rotate Collar"),
                crate::i18n::tr_format!(literal = "%count% hole(s)", count = turned),
            ),
            crate::i18n::tr_format!(literal = "Turned %count% drillhole collar(s) %rotation%", count = turned, rotation = described),
        );
        self.reset_rotate_editor_state();
        self.invalidate_topology_bounds_and_redraw();
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    /// Where the selection points now, and whether its holes agree about it.
    ///
    /// The orientation reported is the first hole's - the anchor the panel
    /// seeds from - so a mixed selection still has something to show and to
    /// turn from rather than the fields going blank.
    pub(crate) fn selected_collar_orientation(&self) -> Option<(HoleOrientation, bool)> {
        let mut orientations = self.collar_targets().into_iter().filter_map(|target| {
            let dataset = self.drill_holes.iter().find(|dataset| dataset.id == target.dataset)?;
            dataset.dataset.holes.get(target.hole)?.orientation()
        });
        let anchor = orientations.next()?;
        // Angles that round to the same tenth of a degree are the same setup:
        // a pattern laid by one generator carries float noise between holes
        // that the panel should not report as a disagreement.
        let same = |a: f64, b: f64| (a - b).abs() < 0.05;
        let mixed = orientations.any(|other| !same(other.azimuth, anchor.azimuth) || !same(other.dip, anchor.dip));
        Some((anchor, mixed))
    }

    /// Keep the panel's angles showing where the selected holes point, for
    /// every frame in which no turn of the user's own is standing over them.
    pub(crate) fn refresh_rotate_panel_readout(&mut self) {
        if self.editor.rotate_preview_active {
            return;
        }
        match self.selected_collar_orientation() {
            Some((orientation, mixed)) => {
                self.editor.rotate_panel_azimuth = orientation.azimuth;
                self.editor.rotate_panel_dip = orientation.dip;
                self.editor.rotate_panel_last_preview = [orientation.azimuth, orientation.dip];
                self.editor.rotate_panel_mixed = mixed;
            }
            None => self.editor.rotate_panel_mixed = false,
        }
    }

    /// Start a ring drag at `cursor_px`.
    ///
    /// The sweep is measured as the cursor's angle about the gizmo centre on
    /// screen, which is what the ring under the pointer is drawn as. A turn
    /// already standing is continued rather than restarted, so releasing and
    /// grabbing again picks up where the last drag left off.
    pub(crate) fn begin_collar_rotate_drag(&mut self, ring: u8, cursor_px: (f32, f32)) {
        if !self.editing_ready() || self.collar_targets().is_empty() {
            return;
        }
        let Some(center_px) = self.editor.rotate_gizmo.center_px else {
            return;
        };
        self.ensure_collar_session();
        let start = self.collar_rotation.unwrap_or(CollarRotation::IDENTITY);
        // How far the anchor hole may still be tipped either way before it
        // would pass vertical. Bounding the accumulated delta by that keeps a
        // drag reversible, rather than piling up angle in a dead zone once the
        // holes themselves have clamped.
        //
        // Measured against the anchor as it was *captured*, not as the preview
        // currently shows it: the delta this drag accumulates is counted from
        // there, so the room left either side has to be as well.
        let anchor_dip = self
            .collar_move_session
            .as_ref()
            .and_then(|session| session.originals.first())
            .and_then(|(_, placement)| placement.orientation())
            .map_or(0.0, |orientation| orientation.dip);
        let dip_room = (-MAX_HOLE_DIP - anchor_dip, MAX_HOLE_DIP - anchor_dip);

        self.editor.rotate_gizmo_drag_ring = Some(ring);
        self.collar_rotate_drag = Some(CollarRotateDrag {
            ring,
            center_px,
            last_angle: screen_angle(center_px, cursor_px),
            swept: 0.0,
            start,
            dip_room,
        });
    }

    /// Follow the cursor round the ring being dragged.
    pub(crate) fn collar_rotate_drag_to_cursor(&mut self) {
        let Some(drag) = self.collar_rotate_drag.as_mut() else {
            return;
        };
        let Some(cursor_px) = self.editor.cursor_screen_px else {
            return;
        };
        let angle = screen_angle(drag.center_px, cursor_px);
        // Accumulate the step rather than the absolute angle, so a sweep that
        // crosses the atan2 discontinuity keeps going instead of jumping a
        // full turn, and a deliberate multi-turn sweep is honoured.
        drag.swept += wrap_to_half_turn(angle - drag.last_angle);
        drag.last_angle = angle;
        let ring = drag.ring;
        let swept = drag.swept;
        let start = drag.start;
        let dip_room = drag.dip_room;

        let sign = self.editor.rotate_gizmo.ring_sign[usize::from(ring).min(1)];
        // The sweep, turned from screen pixels into a rotation about the
        // ring's own world axis.
        let turn = (sign * swept).to_degrees();
        let rotation = advance(start, ring, turn, dip_room);
        self.preview_collar_rotation(rotation);
    }

    /// Release the ring. The turn stands as a preview; the panel's Apply, or
    /// any path that has to settle a pending gesture, is what commits it.
    pub(crate) fn finish_collar_rotate_drag(&mut self) {
        if self.collar_rotate_drag.take().is_none() {
            return;
        }
        self.editor.rotate_gizmo_drag_ring = None;
        self.invalidate_geometry();
        self.invalidate_overlay();
    }

    /// Forget a standing turn, leaving the holes wherever the caller has
    /// already put them. The rollback itself belongs to `cancel_move_delta`,
    /// which owns the shared session both collar gestures write through.
    pub(crate) fn reset_rotate_editor_state(&mut self) {
        self.collar_rotation = None;
        self.collar_rotate_drag = None;
        self.editor.rotate_gizmo_drag_ring = None;
        self.editor.rotate_gizmo_hovered_ring = None;
        self.editor.rotate_preview_active = false;
    }
}

/// Carry `start` forward by `turn` degrees about `ring`'s world axis.
///
/// The two rings drive the two numbers on a drill plan and nothing else: the
/// azimuth ring swings the bearing, the dip ring tilts within it. Azimuth runs
/// clockwise seen from above, which is the negative direction about +Z, so the
/// bearing moves against the rotation the ring reports.
fn advance(start: CollarRotation, ring: u8, turn: f64, dip_room: (f64, f64)) -> CollarRotation {
    match start {
        CollarRotation::Absolute(orientation) => CollarRotation::Absolute(match ring {
            ROTATE_GIZMO_AZIMUTH_RING => HoleOrientation {
                azimuth: (orientation.azimuth - turn).rem_euclid(360.0),
                ..orientation
            },
            _ => HoleOrientation {
                dip: (orientation.dip + turn).clamp(-MAX_HOLE_DIP, MAX_HOLE_DIP),
                ..orientation
            },
        }),
        CollarRotation::Delta { azimuth, dip } => match ring {
            ROTATE_GIZMO_AZIMUTH_RING => CollarRotation::Delta { azimuth: azimuth - turn, dip },
            _ => CollarRotation::Delta {
                azimuth,
                dip: (dip + turn).clamp(dip_room.0, dip_room.1),
            },
        },
    }
}

/// The cursor's angle about the gizmo centre, in screen radians. Screen Y runs
/// down, so this grows clockwise on screen - which is what
/// [`crate::ui::state::RotateGizmoScreen::ring_sign`] is there to convert.
fn screen_angle(center_px: (f32, f32), cursor_px: (f32, f32)) -> f64 {
    f64::from(cursor_px.1 - center_px.1).atan2(f64::from(cursor_px.0 - center_px.0))
}

/// Fold an angle step into (-pi, pi], so one frame's movement is read as the
/// short way round rather than as very nearly a full turn the other way.
fn wrap_to_half_turn(angle: f64) -> f64 {
    let wrapped = (angle + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    // `rem_euclid` lands exactly on -pi where the step is a clean half turn;
    // the half-open interval is what keeps the sign of a sweep stable.
    if wrapped <= -std::f64::consts::PI { std::f64::consts::PI } else { wrapped }
}

/// The world axis each ring turns about, given the bearing the holes point
/// along. Used both to build the rings and to decide which way round a sweep
/// across one of them reads on screen.
pub(crate) fn ring_axis(ring: u8, azimuth_degrees: f64) -> DVec3 {
    if ring == ROTATE_GIZMO_AZIMUTH_RING {
        DVec3::Z
    } else {
        // Horizontal axis square to the bearing: turning about it is what
        // raises or drops the toe, leaving the bearing alone.
        let bearing = azimuth_degrees.to_radians();
        DVec3::new(bearing.cos(), -bearing.sin(), 0.0)
    }
}

/// The two world directions spanning a ring's plane, for sampling it.
pub(crate) fn ring_basis(ring: u8, azimuth_degrees: f64) -> [DVec3; 2] {
    if ring == ROTATE_GIZMO_AZIMUTH_RING {
        [DVec3::X, DVec3::Y]
    } else {
        let bearing = azimuth_degrees.to_radians();
        // The vertical plane the holes point along: their bearing, and up.
        [DVec3::new(bearing.sin(), bearing.cos(), 0.0), DVec3::Z]
    }
}
