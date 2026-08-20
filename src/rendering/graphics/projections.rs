//! World-to-screen projection updates for UI/tool overlays.

use super::*;

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

fn move_gizmo_plane_quad(center: (f32, f32), first_tip: Option<(f32, f32)>, second_tip: Option<(f32, f32)>) -> Option<[(f32, f32); 4]> {
    let direction = |tip: (f32, f32)| {
        let delta = (tip.0 - center.0, tip.1 - center.1);
        let length = (delta.0 * delta.0 + delta.1 * delta.1).sqrt();
        (length > 1.0e-4).then_some((delta.0 / length, delta.1 / length))
    };
    let first = direction(first_tip?)?;
    let second = direction(second_tip?)?;
    // Do not offer a plane handle when that plane projects edge-on.
    if (first.0 * second.1 - first.1 * second.0).abs() < 0.18 {
        return None;
    }
    let point = |first_distance: f32, second_distance: f32| {
        (
            center.0 + first.0 * first_distance + second.0 * second_distance,
            center.1 + first.1 * first_distance + second.1 * second_distance,
        )
    };
    const INNER: f32 = 12.0;
    const OUTER: f32 = 28.0;
    Some([point(INNER, INNER), point(OUTER, INNER), point(OUTER, OUTER), point(INNER, OUTER)])
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

impl<'a> Graphics<'a> {
    pub(super) fn update_tool_projections(&self, editor: &mut EditorState, document: &Document) {
        if let Some(failure) = &editor.tri_create_failure {
            let vp = self.view_proj();
            let screen = self.screen_size();
            editor.tri_create_diagnostic_markers_screen_px = failure
                .diagnostic
                .markers_world
                .iter()
                .filter_map(|&point| crate::rendering::pick::world_to_screen(&vp, point, screen).map(|projected| (projected.x as f32, projected.y as f32)))
                .collect();
            editor.tri_create_diagnostic_segments_screen_px = failure
                .diagnostic
                .segments_world
                .iter()
                .map(|segment| segment.map(|point| crate::rendering::pick::world_to_screen(&vp, point, screen).map(|projected| (projected.x as f32, projected.y as f32))))
                .collect();
        } else {
            editor.tri_create_diagnostic_markers_screen_px.clear();
            editor.tri_create_diagnostic_segments_screen_px.clear();
        }

        if editor.offset_awaiting_side_pick {
            let vp = self.view_proj();
            let screen = self.screen_size();
            // One entry per source vertex: a clipped vertex must keep its
            // slot so guide pairing and preview ranges stay index-aligned.
            editor.offset_source_screen_px = editor
                .offset_source_world
                .iter()
                .map(|&p| crate::rendering::pick::world_to_screen(&vp, p, screen).map(|sp| (sp.x as f32, sp.y as f32)))
                .collect();
            editor.offset_preview_screen_px = editor
                .offset_preview_world
                .iter()
                .map(|&p| crate::rendering::pick::world_to_screen(&vp, p, screen).map(|sp| (sp.x as f32, sp.y as f32)))
                .collect();
        } else {
            editor.offset_source_screen_px.clear();
            editor.offset_preview_screen_px.clear();
            editor.offset_preview_ranges.clear();
        }

        if editor.batter_berm_dialog_open && !editor.batter_berm_rings_world.is_empty() {
            let vp = self.view_proj();
            let screen = self.screen_size();
            editor.batter_berm_source_screen_px = editor
                .batter_berm_source_world
                .iter()
                .map(|&p| crate::rendering::pick::world_to_screen(&vp, p, screen).map(|sp| (sp.x as f32, sp.y as f32)))
                .collect();
            editor.batter_berm_rings_screen_px = editor
                .batter_berm_rings_world
                .iter()
                .map(|ring| {
                    ring.iter()
                        .map(|&p| crate::rendering::pick::world_to_screen(&vp, p, screen).map(|sp| (sp.x as f32, sp.y as f32)))
                        .collect()
                })
                .collect();
        } else {
            editor.batter_berm_source_screen_px.clear();
            editor.batter_berm_rings_screen_px.clear();
        }

        use crate::ui::state::ActiveTool;
        if editor.active_tool == ActiveTool::Move && (!editor.selected_handles.is_empty() || editor.move_vertex_target.is_some()) {
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
                let c = sum / count as f64;
                let vp = self.view_proj();
                let sz = self.screen_size();
                const GIZMO_PX: f64 = 70.0;
                let project = |w: DVec3| -> Option<(f32, f32)> { crate::rendering::pick::world_to_screen(&vp, w, sz).map(|s| (s.x as f32, s.y as f32)) };
                let raw_px = |axis: DVec3| -> f64 {
                    if let (Some(c_px), Some(t_px)) = (project(c), project(c + axis)) {
                        let dx = (t_px.0 - c_px.0) as f64;
                        let dy = (t_px.1 - c_px.1) as f64;
                        (dx * dx + dy * dy).sqrt()
                    } else {
                        1.0
                    }
                };
                let tip_px = |axis: DVec3| -> Option<(f32, f32)> {
                    let c_px = project(c)?;
                    let t_px = project(c + axis)?;
                    let dx = (t_px.0 - c_px.0) as f64;
                    let dy = (t_px.1 - c_px.1) as f64;
                    let len = (dx * dx + dy * dy).sqrt();
                    if len < 1e-4 {
                        return None;
                    }
                    let nx = (dx / len * GIZMO_PX) as f32;
                    let ny = (dy / len * GIZMO_PX) as f32;
                    Some((c_px.0 + nx, c_px.1 + ny))
                };
                editor.move_gizmo_center_px = project(c);
                editor.move_gizmo_x_tip_px = tip_px(DVec3::X);
                editor.move_gizmo_y_tip_px = tip_px(DVec3::Y);
                editor.move_gizmo_z_tip_px = tip_px(DVec3::Z);
                editor.move_gizmo_plane_handles_px = if let Some(center_px) = editor.move_gizmo_center_px {
                    [
                        move_gizmo_plane_quad(center_px, editor.move_gizmo_x_tip_px, editor.move_gizmo_y_tip_px),
                        move_gizmo_plane_quad(center_px, editor.move_gizmo_x_tip_px, editor.move_gizmo_z_tip_px),
                        move_gizmo_plane_quad(center_px, editor.move_gizmo_y_tip_px, editor.move_gizmo_z_tip_px),
                    ]
                } else {
                    [None; 3]
                };
                editor.move_gizmo_x_px_per_world = raw_px(DVec3::X).max(0.001);
                editor.move_gizmo_y_px_per_world = raw_px(DVec3::Y).max(0.001);
                editor.move_gizmo_z_px_per_world = raw_px(DVec3::Z).max(0.001);
            } else {
                editor.move_gizmo_center_px = None;
                editor.move_gizmo_x_tip_px = None;
                editor.move_gizmo_y_tip_px = None;
                editor.move_gizmo_z_tip_px = None;
                editor.move_gizmo_plane_handles_px = [None; 3];
            }
        } else {
            editor.move_gizmo_center_px = None;
            editor.move_gizmo_x_tip_px = None;
            editor.move_gizmo_y_tip_px = None;
            editor.move_gizmo_z_tip_px = None;
            editor.move_gizmo_plane_handles_px = [None; 3];
        }

        if editor.active_tool == ActiveTool::Chamfer {
            use crate::app::commands::drawing::chamfer::chamfer_corner;
            let vp = self.view_proj();
            let screen = self.screen_size();
            let project = |w: DVec3| -> Option<(f32, f32)> { crate::rendering::pick::world_to_screen(&vp, w, screen).map(|s| (s.x as f32, s.y as f32)) };

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
            let screen = self.screen_size();
            let project = |w: DVec3| -> Option<(f32, f32)> { crate::rendering::pick::world_to_screen(&vp, w, screen).map(|s| (s.x as f32, s.y as f32)) };

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
            let screen = self.screen_size();
            if let Some(oid) = editor.split_poly_id {
                if let Some(Object::Polyline { verts, .. }) = document.get_object(oid) {
                    editor.split_poly_verts_screen_px = verts
                        .iter()
                        .map(|v| crate::rendering::pick::world_to_screen(&vp, v.pos, screen).map(|sp| (sp.x as f32, sp.y as f32)))
                        .collect();
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
                && let Some((index, (from, to))) = projected_relimit_candidate_nearest_cursor(cursor, &editor.relimit_candidates, start, end, &vp, screen)
                && let Some(candidate) = editor.relimit_candidates.get(index)
            {
                editor.relimit_hover_end = candidate.end;
                editor.relimit_intersection_3d = Some(candidate.target);
                editor.relimit_preview_is_extension = candidate.is_extension;
                editor.relimit_preview_from_px = Some(from);
                editor.relimit_preview_to_px = Some(to);
            }

            let show = editor.relimit_dialog_open && matches!(editor.relimit_mode, RelimitMode::AbsoluteLength | RelimitMode::RelativeLength);
            if show {
                editor.relimit_resize_end_px = editor.relimit_source_id.and_then(|oid| {
                    if let Some(Object::Polyline { verts, .. }) = document.get_object(oid) {
                        let v = match editor.relimit_resize_end {
                            TrimEnd::Start => verts.first()?.pos,
                            TrimEnd::End => verts.last()?.pos,
                        };
                        crate::rendering::pick::world_to_screen(&vp, v, screen).map(|s| (s.x as f32, s.y as f32))
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
