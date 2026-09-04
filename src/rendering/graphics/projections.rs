//! World-to-screen projection updates for UI/tool overlays.

use super::*;
use crate::{
    app::commands::drawing::rotate_collar::{ring_axis, ring_basis},
    ui::state::{MoveGizmoScreen, ROTATE_GIZMO_AZIMUTH_RING, ROTATE_GIZMO_DIP_RING, RotateGizmoScreen},
};

pub(crate) type ScreenSegmentPx = ((f32, f32), (f32, f32));

fn point_to_screen_segment_distance_sq(point: (f32, f32), segment: ScreenSegmentPx) -> f32 {
    let (a, b) = segment;
    let ab = (b.0 - a.0, b.1 - a.1);
    let ap = (point.0 - a.0, point.1 - a.1);
    let length_sq = ab.0 * ab.0 + ab.1 * ab.1;
    let t = if length_sq > f32::EPSILON {
        ((ap.0 * ab.0 + ap.1 * ab.1) / length_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let delta = (point.0 - (a.0 + t * ab.0), point.1 - (a.1 + t * ab.1));
    delta.0 * delta.0 + delta.1 * delta.1
}

fn nearest_screen_segment(cursor: (f32, f32), segments: impl IntoIterator<Item = (usize, ScreenSegmentPx)>) -> Option<(usize, ScreenSegmentPx)> {
    segments
        .into_iter()
        .filter_map(|(index, segment)| {
            let distance_sq = point_to_screen_segment_distance_sq(cursor, segment);
            distance_sq.is_finite().then_some((index, segment, distance_sq))
        })
        .min_by(|(_, _, a), (_, _, b)| a.total_cmp(b))
        .map(|(index, segment, _)| (index, segment))
}

/// How far off the run between the anchor and the cursor a collar may stand
/// and still be tied in, in physical pixels.
///
/// Measured on screen rather than in the ground: a row of holes is never
/// exactly straight, and what the user is aiming along is the row as they can
/// see it. The collar marker is 11px across, so this is a corridor a little
/// wider than the marks themselves.
pub(crate) const TIE_CORRIDOR_PIXELS: f32 = 12.0;

/// Screen length of a Move gizmo axis that lies square to the camera, in
/// logical points. Every other part of the gizmo is sized from this.
const GIZMO_LENGTH_POINTS: f64 = 80.0;
/// Ring radius as a fraction of the axis length, matching Blender's
/// view-aligned translate handle.
const GIZMO_RING_RATIO: f32 = 0.2;
/// Distance along the diagonal between two axes at which their plane handle
/// sits, and the handle's half-extent, both as fractions of the axis length.
const GIZMO_PLANE_OFFSET: f64 = 0.8;
const GIZMO_PLANE_HALF_EXTENT: f64 = 0.1;
/// Foreshortening below which an axis handle is hidden, and above which it is
/// fully opaque. An axis pointing at the camera projects to nothing useful, so
/// it fades out instead of collapsing into a stub pointing in a direction that
/// flips with sub-pixel camera movement.
const GIZMO_AXIS_FADE_MIN: f32 = 0.2;
const GIZMO_AXIS_FADE_FULL: f32 = 0.44;
/// The same for plane handles, which stay useful until they are nearly
/// edge-on: measured as how squarely the plane faces the camera.
const GIZMO_PLANE_FADE_MIN: f32 = 0.175;
const GIZMO_PLANE_FADE_FULL: f32 = 0.25;

/// Points sampled around each Rotate Collar ring. Enough that a ring reads as
/// a circle at the size it is drawn, and cheap enough to reproject per frame.
const ROTATE_RING_SEGMENTS: usize = 72;
/// Ring radius as a fraction of the Move gizmo's arm length, so the two tools'
/// gizmos come up at about the same size over the same selection.
const ROTATE_RING_RATIO: f64 = 0.85;
/// A ring turning edge-on cannot be followed round, so it fades out over this
/// band of how squarely it faces the camera and stops being clickable at the
/// bottom of it - the same rule the Move gizmo's plane handles follow.
const ROTATE_RING_FADE_MIN: f32 = 0.1;
const ROTATE_RING_FADE_FULL: f32 = 0.28;

/// How far across the view the initiation card's scale probe reaches, in world
/// units. Long enough to measure cleanly, short enough that a perspective
/// view's scale is still the collar's own.
const CARD_SCALE_PROBE_WORLD: f64 = 1.0;

fn fade_ramp(value: f32, min: f32, max: f32) -> f32 {
    if value <= min {
        0.0
    } else if value >= max {
        1.0
    } else {
        (value - min) / (max - min)
    }
}

/// The holes a tie-in run from `anchor` to the cursor would pass through, in
/// the order the round travels, starting with the anchor itself.
///
/// Everything standing in the corridor between the two is taken, not just what
/// is under the pointer: aiming down a row is how a round is tied in, and a
/// row is tied one leg per click only when the legs between are found for the
/// user. Order is by distance along the run, so a chain drawn back on itself
/// still fires the way it was drawn.
pub(crate) fn tie_chain_between(holes: &[crate::model::drill_hole::DrillHole], anchor: usize, cursor_px: (f32, f32), project: impl Fn(DVec3) -> Option<(f32, f32)>) -> Vec<usize> {
    let Some(anchor_hole) = holes.get(anchor) else {
        return Vec::new();
    };
    let Some(start) = project(anchor_hole.collar_position()) else {
        return Vec::new();
    };
    let run = (cursor_px.0 - start.0, cursor_px.1 - start.1);
    let length_sq = run.0 * run.0 + run.1 * run.1;
    if length_sq <= f32::EPSILON {
        return Vec::new();
    }
    let mut along: Vec<(f32, usize)> = holes
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != anchor)
        .filter_map(|(index, hole)| {
            let point = project(hole.collar_position())?;
            let offset = (point.0 - start.0, point.1 - start.1);
            let t = (offset.0 * run.0 + offset.1 * run.1) / length_sq;
            // Behind the anchor, or beyond where the pointer has reached: not
            // on the run being drawn.
            if !(0.0..=1.0).contains(&t) {
                return None;
            }
            let across = (offset.0 - t * run.0, offset.1 - t * run.1);
            (across.0.hypot(across.1) <= TIE_CORRIDOR_PIXELS).then_some((t, index))
        })
        .collect();
    if along.is_empty() {
        return Vec::new();
    }
    along.sort_by(|(a, _), (b, _)| a.total_cmp(b));
    std::iter::once(anchor).chain(along.into_iter().map(|(_, index)| index)).collect()
}

pub(crate) fn projected_relimit_candidate_nearest_cursor(
    cursor: (f32, f32),
    candidates: &[crate::ui::state::RelimitCandidate],
    source_start: DVec3,
    source_end: DVec3,
    view_proj: &DMat4,
    screen: Size,
) -> Option<(usize, ScreenSegmentPx)> {
    use crate::ui::state::TrimEnd;

    let projected = candidates.iter().enumerate().filter_map(|(index, candidate)| {
        let moving = match candidate.end {
            TrimEnd::Start => source_start,
            TrimEnd::End => source_end,
        };
        let from = crate::rendering::pick::world_to_screen(view_proj, moving, screen)?;
        let to = crate::rendering::pick::world_to_screen(view_proj, candidate.target, screen)?;
        Some((index, ((from.x as f32, from.y as f32), (to.x as f32, to.y as f32))))
    });
    nearest_screen_segment(cursor, projected)
}

/// The camera-plane reference a gizmo sizes itself against: two world
/// directions lying in the view plane, the screen vectors one world unit along
/// each produces, and the pixels per world unit that follows from them.
///
/// Both gizmos measure against this, which is what makes "80 logical points"
/// mean the same size on screen whichever one is up.
struct GizmoScreenScale {
    right: DVec3,
    up: DVec3,
    right_px: (f64, f64),
    up_px: (f64, f64),
    px_per_world: f64,
}

fn gizmo_screen_scale(center: DVec3, center_px: (f32, f32), forward: DVec3, camera_up_hint: DVec3, project: &impl Fn(DVec3) -> Option<(f32, f32)>) -> Option<GizmoScreenScale> {
    let up_hint = if forward.cross(camera_up_hint).length_squared() > 1.0e-9 {
        camera_up_hint
    } else {
        DVec3::X
    };
    let right = forward.cross(up_hint).normalize();
    let up = right.cross(forward).normalize();
    let screen_vector = |direction: DVec3| -> Option<(f64, f64)> {
        let tip = project(center + direction)?;
        Some((f64::from(tip.0 - center_px.0), f64::from(tip.1 - center_px.1)))
    };
    let right_px = screen_vector(right).unwrap_or((1.0, 0.0));
    let up_px = screen_vector(up).unwrap_or((0.0, -1.0));
    let px_per_world = right_px.0.hypot(right_px.1).max(up_px.0.hypot(up_px.1));
    (px_per_world.partial_cmp(&1.0e-9) == Some(std::cmp::Ordering::Greater)).then_some(GizmoScreenScale {
        right,
        up,
        right_px,
        up_px,
        px_per_world,
    })
}

/// Project the Rotate Collar gizmo around `center`: an azimuth ring lying flat
/// and a dip ring standing in the vertical plane the holes point along, so the
/// dip ring swings round as the bearing is turned and always shows the plane
/// the toe actually moves in.
///
/// Rings are sampled in world space and projected point by point, so
/// perspective shapes them into the ellipses they really are.
fn build_rotate_gizmo(
    center: DVec3,
    azimuth_degrees: f64,
    forward: DVec3,
    camera_up_hint: DVec3,
    length_px: f32,
    project: impl Fn(DVec3) -> Option<(f32, f32)>,
) -> RotateGizmoScreen {
    let Some(center_px) = project(center) else {
        return RotateGizmoScreen::default();
    };
    let Some(scale) = gizmo_screen_scale(center, center_px, forward, camera_up_hint, &project) else {
        return RotateGizmoScreen::default();
    };
    let radius = f64::from(length_px) * ROTATE_RING_RATIO / scale.px_per_world;

    let mut gizmo = RotateGizmoScreen {
        center_px: Some(center_px),
        scale_factor: (f64::from(length_px) / GIZMO_LENGTH_POINTS) as f32,
        ..RotateGizmoScreen::default()
    };
    for ring in [ROTATE_GIZMO_AZIMUTH_RING, ROTATE_GIZMO_DIP_RING] {
        let index = usize::from(ring);
        let normal = ring_axis(ring, azimuth_degrees);
        let towards_view = normal.dot(forward);
        // A ring is fully readable face-on and useless edge-on, which is
        // exactly how squarely its axis points along the view.
        gizmo.ring_fade[index] = fade_ramp(towards_view.abs() as f32, ROTATE_RING_FADE_MIN, ROTATE_RING_FADE_FULL);
        // Screen angles grow clockwise (screen Y runs down), and a ring whose
        // far face is towards the camera reads the other way round again; both
        // are folded into this one sign.
        gizmo.ring_sign[index] = if towards_view < 0.0 { -1.0 } else { 1.0 };
        if gizmo.ring_fade[index] <= 0.0 {
            continue;
        }
        let [across, along] = ring_basis(ring, azimuth_degrees);
        gizmo.ring_px[index] = (0..ROTATE_RING_SEGMENTS)
            .filter_map(|step| {
                let theta = std::f64::consts::TAU * step as f64 / ROTATE_RING_SEGMENTS as f64;
                project(center + (across * theta.cos() + along * theta.sin()) * radius)
            })
            .collect();
    }
    gizmo
}

/// Project the Move gizmo around `center`, Blender-style: fixed on-screen
/// size, each axis foreshortened by its own angle to the camera, and
/// handles faded out as they turn towards the view direction.
fn build_move_gizmo(center: DVec3, forward: DVec3, camera_up_hint: DVec3, length_px: f32, project: impl Fn(DVec3) -> Option<(f32, f32)>) -> MoveGizmoScreen {
    let Some(center_px) = project(center) else {
        return MoveGizmoScreen::default();
    };

    // Camera-plane basis: the reference for "unforeshortened" screen size,
    // and the drag basis for the view-aligned ring.
    let Some(scale) = gizmo_screen_scale(center, center_px, forward, camera_up_hint, &project) else {
        return MoveGizmoScreen::default();
    };
    let world_length = f64::from(length_px) / scale.px_per_world;

    let mut gizmo = MoveGizmoScreen {
        center_px: Some(center_px),
        scale_factor: (f64::from(length_px) / GIZMO_LENGTH_POINTS) as f32,
        ring_radius_px: length_px * GIZMO_RING_RATIO,
        view_axes: Some([scale.right, scale.up]),
        view_basis_px: [scale.right_px, scale.up_px],
        ..MoveGizmoScreen::default()
    };

    let axes = [DVec3::X, DVec3::Y, DVec3::Z];
    // How much of its full screen length each axis keeps: 1 when square to
    // the camera, 0 when aimed at it.
    let mut projected_ratio = [0.0f32; 3];
    for (index, axis) in axes.into_iter().enumerate() {
        let tip = project(center + axis * world_length);
        gizmo.axis_tip_px[index] = tip;
        let span = tip.map_or(0.0, |tip| f64::from(tip.0 - center_px.0).hypot(f64::from(tip.1 - center_px.1)));
        projected_ratio[index] = (span / f64::from(length_px)).clamp(0.0, 1.0) as f32;
        gizmo.axis_fade[index] = fade_ramp(projected_ratio[index], GIZMO_AXIS_FADE_MIN, GIZMO_AXIS_FADE_FULL);
        gizmo.axis_px_per_world[index] = (span / world_length).max(1.0e-6);
    }

    // Plane handles are small diamonds on the diagonal between their two
    // axes, built in world space so perspective shapes them correctly.
    let offset = GIZMO_PLANE_OFFSET * world_length;
    let half_extent = GIZMO_PLANE_HALF_EXTENT * world_length;
    for (index, [first, second]) in [[0usize, 1], [0, 2], [1, 2]].into_iter().enumerate() {
        let normal = 3 - first - second;
        // A plane stops being usable as it turns edge-on, which is exactly
        // when its normal turns square to the view.
        let facing = (1.0 - projected_ratio[normal] * projected_ratio[normal]).max(0.0).sqrt();
        gizmo.plane_fade[index] = fade_ramp(facing, GIZMO_PLANE_FADE_MIN, GIZMO_PLANE_FADE_FULL);
        if gizmo.plane_fade[index] <= 0.0 {
            continue;
        }
        let diagonal = (axes[first] + axes[second]).normalize();
        let across = (axes[second] - axes[first]).normalize();
        let handle_center = center + diagonal * offset;
        let corners = [
            project(handle_center - diagonal * half_extent),
            project(handle_center + across * half_extent),
            project(handle_center + diagonal * half_extent),
            project(handle_center - across * half_extent),
        ];
        gizmo.plane_quad_px[index] = match corners {
            [Some(a), Some(b), Some(c), Some(d)] => Some([a, b, c, d]),
            _ => None,
        };
    }

    gizmo
}

/// Where a collar gesture's gizmo stands, and the bearing its dip ring should
/// lie in: the mean of the selected collars, and the orientation of the anchor
/// hole among them.
///
/// The anchor is the lowest-numbered hole of the lowest-numbered dataset - the
/// same order `App::collar_targets` sorts into - so which hole the panel and
/// the dip ring speak for does not shift about with hash order.
fn selected_collar_anchor(editor: &EditorState, drill_holes: &[OpenDrillHoleDataset]) -> Option<(DVec3, f64)> {
    let hole_at = |target: &crate::model::drill_hole::DrillHoleRef| {
        drill_holes
            .iter()
            .find(|dataset| dataset.id == target.dataset && dataset.state.loaded)
            .and_then(|dataset| dataset.dataset.holes.get(target.hole))
    };
    let mut selected: Vec<_> = editor.selected_drill_holes.iter().filter(|target| hole_at(target).is_some()).collect();
    if selected.is_empty() {
        return None;
    }
    selected.sort_by_key(|target| (target.dataset.0, target.hole));
    let sum: DVec3 = selected.iter().filter_map(|target| hole_at(target)).map(|hole| hole.collar).sum();
    // A hole with no length below its collar has no bearing of its own; north
    // is the same stand-in the survey resolver falls back on.
    let azimuth = selected
        .first()
        .and_then(|target| hole_at(target))
        .and_then(|hole| hole.orientation())
        .map_or(0.0, |orientation| orientation.azimuth);
    Some((sum / selected.len() as f64, azimuth))
}

impl<'a> Graphics<'a> {
    /// Project the Move gizmo around `center` using the live camera.
    fn project_move_gizmo(&self, center: DVec3) -> MoveGizmoScreen {
        let view_proj = self.view_proj();
        build_move_gizmo(
            center,
            self.camera.forward(),
            self.camera.up(),
            (GIZMO_LENGTH_POINTS * self.window.scale_factor()) as f32,
            // The gizmo is a foreground overlay sized to constant screen space,
            // so its arms reach furthest in world units exactly when the camera
            // is zoomed in and the scene-fitted depth slab is thinnest - project
            // without that slab's rejection or the arms vanish.
            |world| self.world_to_window_px_unclipped_depth(&view_proj, world),
        )
    }

    /// Project the Rotate Collar gizmo around `center`, with its dip ring
    /// standing in the vertical plane `azimuth_degrees` names.
    fn project_rotate_gizmo(&self, center: DVec3, azimuth_degrees: f64) -> RotateGizmoScreen {
        let view_proj = self.view_proj();
        build_rotate_gizmo(
            center,
            azimuth_degrees,
            self.camera.forward(),
            self.camera.up(),
            (GIZMO_LENGTH_POINTS * self.window.scale_factor()) as f32,
            // Sized to constant screen space like the Move gizmo, so it wants
            // the same freedom from the scene-fitted depth slab.
            |world| self.world_to_window_px_unclipped_depth(&view_proj, world),
        )
    }

    pub(super) fn update_tool_projections(&self, editor: &mut EditorState, document: &Document, drill_holes: &[OpenDrillHoleDataset]) {
        // Where the snap landed, for the drawn cursor to mark. The snapped
        // point is the one the tool will use, and it is not the pointer: it
        // can sit anywhere inside the snap threshold of it.
        editor.snap_marker_px = editor
            .cursor_snapped
            .then_some(editor.cursor_world)
            .flatten()
            .and_then(|world| self.world_to_window_px(&self.view_proj(), world));

        if editor.active_workspace == crate::ui::state::Workspace::DrillAndBlast {
            let view_proj = self.view_proj();
            // The delay card is drawn at a world size, so each one carries the
            // screen scale measured at its own collar: project a probe one
            // world unit across the view and take the pixels it covers. Under
            // perspective that scale falls off with depth, so it cannot be one
            // constant for the whole view.
            let forward = self.camera.forward();
            let probe_offset = forward.cross(self.camera.up()).normalize_or_zero() * CARD_SCALE_PROBE_WORLD;
            editor.initiation_cards = drill_holes
                .iter()
                .filter(|dataset| dataset.state.loaded && dataset.visible && !editor.hidden_handles.contains(&dataset.entity_id()))
                .flat_map(|dataset| {
                    dataset.dataset.initiations.iter().filter_map(|initiation| {
                        let hole = dataset.dataset.holes.get(initiation.hole)?;
                        let collar = hole.collar_position();
                        let screen_px = self.world_to_window_px(&view_proj, collar)?;
                        let probe_px = self.world_to_window_px_unclipped_depth(&view_proj, collar + probe_offset)?;
                        Some(crate::ui::state::InitiationCard {
                            target: crate::model::drill_hole::DrillHoleRef {
                                dataset: dataset.id,
                                hole: initiation.hole,
                            },
                            delay_ms: initiation.delay_ms,
                            screen_px,
                            px_per_world: (probe_px.0 - screen_px.0).hypot(probe_px.1 - screen_px.1) / CARD_SCALE_PROBE_WORLD as f32,
                        })
                    })
                })
                .collect();
        } else {
            editor.initiation_cards.clear();
        }

        if let Some(failure) = &editor.tri_create_failure {
            let vp = self.view_proj();
            editor.tri_create_diagnostic_markers_screen_px = failure.diagnostic.markers_world.iter().filter_map(|&point| self.world_to_window_px(&vp, point)).collect();
            editor.tri_create_diagnostic_segments_screen_px = failure
                .diagnostic
                .segments_world
                .iter()
                .map(|segment| segment.map(|point| self.world_to_window_px(&vp, point)))
                .collect();
        } else {
            editor.tri_create_diagnostic_markers_screen_px.clear();
            editor.tri_create_diagnostic_segments_screen_px.clear();
        }

        if editor.offset_awaiting_side_pick {
            let vp = self.view_proj();
            // One entry per source vertex: a clipped vertex must keep its
            // slot so guide pairing and preview ranges stay index-aligned.
            editor.offset_source_screen_px = editor.offset_source_world.iter().map(|&p| self.world_to_window_px_unclipped_depth(&vp, p)).collect();
            editor.offset_preview_screen_px = editor.offset_preview_world.iter().map(|&p| self.world_to_window_px_unclipped_depth(&vp, p)).collect();
        } else {
            editor.offset_source_screen_px.clear();
            editor.offset_preview_screen_px.clear();
            editor.offset_preview_ranges.clear();
        }

        if editor.batter_berm_dialog_open && !editor.batter_berm_rings_world.is_empty() {
            let vp = self.view_proj();
            editor.batter_berm_source_screen_px = editor.batter_berm_source_world.iter().map(|&p| self.world_to_window_px_unclipped_depth(&vp, p)).collect();
            editor.batter_berm_rings_screen_px = editor
                .batter_berm_rings_world
                .iter()
                .map(|ring| ring.iter().map(|&p| self.world_to_window_px_unclipped_depth(&vp, p)).collect())
                .collect();
        } else {
            editor.batter_berm_source_screen_px.clear();
            editor.batter_berm_rings_screen_px.clear();
        }

        use crate::ui::state::ActiveTool;
        // Both collar tools stand their gizmo at the middle of the collars
        // they are holding, read from the datasets themselves so it follows a
        // live preview the way the design gizmo follows the moved objects.
        let collar_anchor = editor.active_tool.acts_on_collars().then(|| selected_collar_anchor(editor, drill_holes)).flatten();
        editor.rotate_gizmo = match collar_anchor.filter(|_| editor.active_tool.rotates()) {
            // The dip ring stands in the vertical plane the anchor hole points
            // along, so it always shows the plane its toe would swing in.
            Some((center, azimuth)) => self.project_rotate_gizmo(center, azimuth),
            None => RotateGizmoScreen::default(),
        };
        if editor.active_tool == ActiveTool::MoveCollar {
            editor.move_gizmo = match collar_anchor {
                Some((center, _)) => self.project_move_gizmo(center),
                None => MoveGizmoScreen::default(),
            };
        } else if editor.active_tool == ActiveTool::Move && (!editor.selected_handles.is_empty() || editor.move_vertex_target.is_some()) {
            let mut sum = DVec3::ZERO;
            let mut count = 0usize;
            if let Some((target_id, point)) = editor.move_vertex_target {
                if let Some(obj) = document.get_object(target_id) {
                    match (obj, point) {
                        (Object::Polyline { verts, .. }, crate::model::ObjectPoint::Vertex(vertex_index)) => {
                            if let Some(vertex) = verts.get(vertex_index) {
                                sum += vertex.pos;
                                count += 1;
                            }
                        }
                        (Object::Polyline { verts, closed, .. }, crate::model::ObjectPoint::Center) => {
                            if let Some(center) = crate::model::geometry::compact_circle_center(verts, *closed) {
                                sum += center;
                                count += 1;
                            }
                        }
                        (Object::Point { pos, .. }, crate::model::ObjectPoint::Vertex(0)) => {
                            sum += *pos;
                            count += 1;
                        }
                        _ => {}
                    }
                }
            } else {
                for &handle in &editor.selected_handles {
                    if let SceneEntityId::Object(id) = handle
                        && let Some(obj) = document.get_object(id)
                    {
                        match obj {
                            Object::Polyline { verts, .. } => {
                                for vertex in verts {
                                    sum += vertex.pos;
                                    count += 1;
                                }
                            }
                            Object::Point { pos, .. } | Object::Text { pos, .. } => {
                                sum += *pos;
                                count += 1;
                            }
                        }
                    }
                }
            }
            if count > 0 {
                editor.move_gizmo = self.project_move_gizmo(sum / count as f64);
            } else {
                editor.move_gizmo = MoveGizmoScreen::default();
            }
        } else {
            editor.move_gizmo = MoveGizmoScreen::default();
        }

        if editor.active_tool == ActiveTool::Chamfer {
            use crate::app::commands::drawing::chamfer::chamfer_corner;
            let vp = self.view_proj();
            // Preview + gizmo geometry is a foreground overlay offset from the
            // source corner, so keep it drawable even when a tightly-fitted
            // depth slab would reject the displaced point.
            let project = |w: DVec3| -> Option<(f32, f32)> { self.world_to_window_px_unclipped_depth(&vp, w) };

            let corner_data = editor.chamfer_poly_id.and_then(|oid| {
                let ci = editor.chamfer_corner_index?;
                if let Some(Object::Polyline { verts, closed: true, .. }) = document.get_object(oid) {
                    (verts.len() >= 3 && ci < verts.len()).then(|| (verts.clone(), ci))
                } else {
                    None
                }
            });

            if let Some((ref verts, ci)) = corner_data {
                use crate::app::commands::drawing::chamfer::chamfer_max_radius;
                editor.chamfer_max_radius = chamfer_max_radius(verts, ci);
                editor.chamfer_radius = editor.chamfer_radius.min(editor.chamfer_max_radius);
                let chamfered = chamfer_corner(verts, ci, editor.chamfer_radius, editor.chamfer_segments);
                editor.chamfer_preview_screen_px = chamfered.iter().map(|v| project(v.pos)).collect();
                editor.chamfer_hover_corner_px = None;

                let n = verts.len();
                let corner = verts[ci].pos;
                let next = verts[(ci + 1) % n].pos;
                let c_px = project(corner);
                let n_px = project(next);
                let edge_stub_dir = if let (Some(cp), Some(np)) = (c_px, n_px) {
                    let dx = np.0 - cp.0;
                    let dy = np.1 - cp.1;
                    let l = (dx * dx + dy * dy).sqrt().max(1e-6);
                    Some((dx / l, dy / l))
                } else {
                    None
                };
                editor.chamfer_gizmo_bisector_px = edge_stub_dir;

                let edge_2d = DVec2::new(next.x - corner.x, next.y - corner.y);
                let edge_len = edge_2d.length();
                if edge_len > 1e-10 {
                    let edge_dir = edge_2d / edge_len;
                    let handle_world = DVec3::new(corner.x + edge_dir.x * editor.chamfer_radius, corner.y + edge_dir.y * editor.chamfer_radius, corner.z);
                    editor.chamfer_gizmo_corner_px = c_px;
                    editor.chamfer_gizmo_handle_px = project(handle_world);
                    if let (Some(cp), Some(np)) = (c_px, n_px) {
                        let dx = np.0 - cp.0;
                        let dy = np.1 - cp.1;
                        let screen_edge_len = (dx * dx + dy * dy).sqrt();
                        if screen_edge_len > 1e-4 {
                            editor.chamfer_gizmo_edge_screen_dir = Some((dx / screen_edge_len, dy / screen_edge_len));
                            editor.chamfer_gizmo_px_per_world = screen_edge_len as f64 / edge_len;
                        }
                    }
                } else {
                    editor.chamfer_gizmo_corner_px = None;
                    editor.chamfer_gizmo_handle_px = None;
                }
            } else {
                editor.chamfer_preview_screen_px.clear();
                editor.chamfer_gizmo_corner_px = None;
                editor.chamfer_gizmo_handle_px = None;
                editor.chamfer_gizmo_bisector_px = None;
                editor.chamfer_gizmo_edge_screen_dir = None;
                editor.chamfer_max_radius = f64::MAX;
            }
        } else {
            editor.chamfer_preview_screen_px.clear();
            editor.chamfer_hover_corner_px = None;
            editor.chamfer_gizmo_corner_px = None;
            editor.chamfer_gizmo_handle_px = None;
            editor.chamfer_gizmo_bisector_px = None;
            editor.chamfer_gizmo_edge_screen_dir = None;
            editor.chamfer_gizmo_drag_start_px = None;
            editor.chamfer_gizmo_hovered = false;
            editor.chamfer_max_radius = f64::MAX;
        }

        if editor.active_tool == ActiveTool::Bezier {
            use crate::app::commands::drawing::bezier::{bezier_eval, directed_span_points};
            let vp = self.view_proj();
            // Control points and the curve they bow toward are a foreground
            // overlay that can sit well off the source line's depth, so project
            // without the scene-fitted depth slab's rejection.
            let project = |w: DVec3| -> Option<(f32, f32)> { self.world_to_window_px_unclipped_depth(&vp, w) };

            if let Some(oid) = editor.bezier_poly_id {
                if let Some(Object::Polyline { verts, closed, .. }) = document.get_object(oid) {
                    let verts = verts.clone();
                    let n = verts.len();
                    editor.bezier_poly_closed = *closed;

                    editor.bezier_poly_verts_screen_px = verts.iter().map(|v| project(v.pos)).collect();

                    if let [Some(vi), Some(vj)] = editor.bezier_selected_verts
                        && vi < n
                        && vj < n
                    {
                        let cp1 = DVec3::from(editor.bezier_cp1);
                        let cp2 = DVec3::from(editor.bezier_cp2);
                        editor.bezier_cp1_screen_px = project(cp1);
                        editor.bezier_cp2_screen_px = project(cp2);

                        let segs = editor.bezier_segments.max(2) as usize;
                        let source_span = directed_span_points(&verts, *closed, vi, vj);
                        editor.bezier_span_screen_px = source_span.iter().map(|&point| project(point)).collect();

                        let start = verts[vi].pos;
                        let end = verts[vj].pos;
                        editor.bezier_preview_screen_px = (0..=segs)
                            .map(|sample| {
                                let t = sample as f64 / segs as f64;
                                project(bezier_eval(start, cp1, cp2, end, t))
                            })
                            .collect();
                    } else {
                        editor.bezier_cp1_screen_px = None;
                        editor.bezier_cp2_screen_px = None;
                        editor.bezier_span_screen_px.clear();
                        editor.bezier_preview_screen_px.clear();
                    }
                } else {
                    editor.bezier_poly_verts_screen_px.clear();
                    editor.bezier_cp1_screen_px = None;
                    editor.bezier_cp2_screen_px = None;
                    editor.bezier_span_screen_px.clear();
                    editor.bezier_preview_screen_px.clear();
                }
            } else {
                editor.bezier_poly_verts_screen_px.clear();
                editor.bezier_cp1_screen_px = None;
                editor.bezier_cp2_screen_px = None;
                editor.bezier_span_screen_px.clear();
                editor.bezier_preview_screen_px.clear();
            }
        } else {
            editor.bezier_poly_verts_screen_px.clear();
            editor.bezier_cp1_screen_px = None;
            editor.bezier_cp2_screen_px = None;
            editor.bezier_span_screen_px.clear();
            editor.bezier_preview_screen_px.clear();
            editor.bezier_hover_cp = None;
            editor.bezier_dragging_cp = None;
        }

        if editor.active_tool == ActiveTool::SplitAtPoints {
            let vp = self.view_proj();
            if let Some(oid) = editor.split_poly_id {
                if let Some(Object::Polyline { verts, .. }) = document.get_object(oid) {
                    editor.split_poly_verts_screen_px = verts.iter().map(|v| self.world_to_window_px(&vp, v.pos)).collect();
                } else {
                    editor.split_poly_verts_screen_px.clear();
                }
            } else {
                editor.split_poly_verts_screen_px.clear();
            }
        } else {
            editor.split_poly_verts_screen_px.clear();
        }

        {
            use crate::ui::state::{RelimitMode, TrimEnd};
            let vp = self.view_proj();
            let screen = self.screen_size();

            editor.relimit_intersection_3d = None;
            editor.relimit_preview_from_px = None;
            editor.relimit_preview_to_px = None;
            if editor.relimit_confirming_end
                && let Some(cursor) = editor.cursor_screen_px
                && let Some((start, end)) = editor.relimit_source_id.and_then(|oid| match document.get_object(oid) {
                    Some(Object::Polyline { verts, .. }) if verts.len() >= 2 => Some((verts.first()?.pos, verts.last()?.pos)),
                    _ => None,
                })
                && let Some((index, (from, to))) =
                    projected_relimit_candidate_nearest_cursor(self.window_to_viewport_px(cursor), &editor.relimit_candidates, start, end, &vp, screen)
                && let Some(candidate) = editor.relimit_candidates.get(index)
            {
                editor.relimit_hover_end = candidate.end;
                editor.relimit_intersection_3d = Some(candidate.target);
                editor.relimit_preview_is_extension = candidate.is_extension;
                editor.relimit_preview_from_px = Some(self.viewport_to_window_px(from));
                editor.relimit_preview_to_px = Some(self.viewport_to_window_px(to));
            }

            let show = editor.relimit_dialog_open && matches!(editor.relimit_mode, RelimitMode::AbsoluteLength | RelimitMode::RelativeLength);
            if show {
                editor.relimit_resize_end_px = editor.relimit_source_id.and_then(|oid| {
                    if let Some(Object::Polyline { verts, .. }) = document.get_object(oid) {
                        let v = match editor.relimit_resize_end {
                            TrimEnd::Start => verts.first()?.pos,
                            TrimEnd::End => verts.last()?.pos,
                        };
                        self.world_to_window_px(&vp, v)
                    } else {
                        None
                    }
                });
            } else {
                editor.relimit_resize_end_px = None;
            }
        }
    }
}
