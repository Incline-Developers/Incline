//! Overlay scene assembly.

use glam::{DMat4, DVec3};

use crate::{
    Size,
    model::{
        Document, Object, ObjectPoint,
        geometry::{circle_polyline_vertices, compact_circle_center},
    },
    rendering::{
        StrokeVertex, Vertex,
        geometry::{DrawContext, draw_line, draw_screen_cross, draw_screen_point_marker, draw_screen_sphere, tessellate_polyline_stroke},
        graphics::{DOC_LINE_WIDTH, MEASUREMENT_COLOR, PREVIEW_COLOR},
        pick::world_to_screen,
    },
    ui::state::{ActiveTool, EditorState},
};

pub(crate) struct OverlaySceneBuildInput<'a> {
    pub(crate) editor: &'a EditorState,
    pub(crate) document: &'a Document,
    pub(crate) overlay_vertex_buf: &'a mut Vec<StrokeVertex>,
    pub(crate) overlay_index_buf: &'a mut Vec<u32>,
    pub(crate) view_proj: DMat4,
    pub(crate) screen_size: Size,
    pub(crate) scene_origin: DVec3,
    pub(crate) scale_factor: f32,
}

pub(crate) fn rebuild_editor_overlay(input: OverlaySceneBuildInput<'_>) {
    let OverlaySceneBuildInput {
        editor,
        document,
        overlay_vertex_buf,
        overlay_index_buf,
        view_proj,
        screen_size,
        scene_origin,
        scale_factor,
    } = input;

    overlay_vertex_buf.clear();
    overlay_index_buf.clear();

    let mut unused_fill_vertices: Vec<Vertex> = Vec::new();
    let mut unused_fill_indices = Vec::new();
    let mut overlay = DrawContext {
        stroke_vertex_buf: overlay_vertex_buf,
        stroke_index_buf: overlay_index_buf,
        fill_vertex_buf: &mut unused_fill_vertices,
        fill_index_buf: &mut unused_fill_indices,
        scene_origin,
        scale_factor,
    };

    let stroke_preview = editor.pending_stroke.clone();
    for pair in stroke_preview.windows(2) {
        draw_line(&mut overlay, pair[0], pair[1], DOC_LINE_WIDTH, PREVIEW_COLOR);
    }
    if editor.poly_finish_dialog {
        // Dialog is open: draw a dashed closing line from last point to first point.
        // Dash size is fixed in screen pixels so it stays visible at any zoom level.
        if let (Some(&first), Some(&last)) = (stroke_preview.first(), stroke_preview.last()) {
            let total_world = (first - last).length();
            if total_world > 1e-9 {
                let px_per_world = {
                    let s0 = world_to_screen(&view_proj, last, screen_size);
                    let s1 = world_to_screen(&view_proj, first, screen_size);
                    if let (Some(a), Some(b)) = (s0, s1) {
                        let sdx = b.x - a.x;
                        let sdy = b.y - a.y;
                        (sdx * sdx + sdy * sdy).sqrt() / total_world
                    } else {
                        1.0
                    }
                };
                let dash_world = 6.0 / px_per_world;
                let gap_world = 4.0 / px_per_world;
                let step = dash_world + gap_world;
                let dir = (first - last) / total_world;
                let mut t = 0.0f64;
                while t < total_world {
                    let t0 = t;
                    let t1 = (t + dash_world).min(total_world);
                    draw_line(&mut overlay, last + dir * t0, last + dir * t1, DOC_LINE_WIDTH, PREVIEW_COLOR);
                    t += step;
                }
            }
        }
    } else if let (Some(&last), Some(cursor)) = (editor.pending_stroke.last(), editor.cursor_world) {
        draw_line(&mut overlay, last, cursor, DOC_LINE_WIDTH, PREVIEW_COLOR);
    }

    if editor.active_tool == ActiveTool::MakeCircle
        && let Some(draft) = editor.circle_draft.as_ref()
    {
        draw_screen_cross(&mut overlay, draft.center, 7.0, 1.5, PREVIEW_COLOR);
        if let Some(radius) = draft.preview_radius(editor.cursor_world) {
            let bearing = editor.cursor_world.map_or(glam::DVec2::X, |cursor| cursor.truncate() - draft.center.truncate());
            if let Some(verts) = circle_polyline_vertices(draft.center, radius, bearing) {
                draw_line(&mut overlay, draft.center, verts[0].pos, 1.5, PREVIEW_COLOR);
                tessellate_polyline_stroke(&mut overlay, &verts, true, DOC_LINE_WIDTH, PREVIEW_COLOR);
            }
        }
    }

    if editor.cursor_snapped
        && let Some(position) = editor.cursor_world
    {
        draw_screen_point_marker(&mut overlay, position, PREVIEW_COLOR);
    }

    if editor.active_tool == ActiveTool::VerticalSlice
        && let Some(start) = editor.slice_pending_start
    {
        draw_screen_cross(&mut overlay, start, 8.0, 2.0, PREVIEW_COLOR);
        if let Some(cursor) = editor.cursor_world {
            // The slice line is flat in XY; preview it at the start's elevation.
            let end = DVec3::new(cursor.x, cursor.y, start.z);
            draw_line(&mut overlay, start, end, 2.0, PREVIEW_COLOR);
        }
    }

    if editor.active_tool == ActiveTool::MeasureDistance
        && let Some(start) = editor.measurement_start
    {
        let end = editor.measurement_end.or(editor.cursor_world);
        if let Some(end) = end {
            draw_line(&mut overlay, start, end, 2.0, MEASUREMENT_COLOR);
            draw_screen_cross(&mut overlay, end, 8.0, 2.0, MEASUREMENT_COLOR);
        }
        draw_screen_cross(&mut overlay, start, 8.0, 2.0, MEASUREMENT_COLOR);
    }

    if editor.active_tool == ActiveTool::MeasureBatterAngle {
        let mut points = editor.batter_angle_points.clone();
        if points.len() < 3
            && let Some(cursor) = editor.cursor_world
        {
            points.push(cursor);
        }

        if let Some(&first) = points.first() {
            draw_screen_cross(&mut overlay, first, 8.0, 2.0, MEASUREMENT_COLOR);
        }
        if points.len() >= 2 {
            draw_line(&mut overlay, points[0], points[1], 2.0, MEASUREMENT_COLOR);
            draw_screen_cross(&mut overlay, points[1], 8.0, 2.0, MEASUREMENT_COLOR);
        }
        if points.len() >= 3 {
            if let Some(measurement) = crate::ui::state::batter_angle_measurement(points.as_slice()) {
                draw_line(&mut overlay, measurement.projection, points[2], 2.0, MEASUREMENT_COLOR);
                draw_screen_cross(&mut overlay, measurement.projection, 6.0, 1.5, MEASUREMENT_COLOR);
            } else {
                draw_line(&mut overlay, points[1], points[2], 2.0, MEASUREMENT_COLOR);
            }
            draw_screen_cross(&mut overlay, points[2], 8.0, 2.0, MEASUREMENT_COLOR);
        }
    }

    let marker_target_id = editor.move_vertex_target.map(|(id, _)| id);
    if let Some(target_id) = marker_target_id
        && let Some(obj) = document.get_object(target_id)
    {
        match (obj, editor.move_vertex_target.map(|(_, point)| point)) {
            (Object::Polyline { verts, closed, .. }, Some(ObjectPoint::Center)) => {
                if let Some(center) = compact_circle_center(verts, *closed) {
                    draw_screen_sphere(&mut overlay, center, 8.0, PREVIEW_COLOR);
                }
            }
            (Object::Polyline { verts, .. }, Some(ObjectPoint::Vertex(selected_index))) => {
                for (index, v) in verts.iter().enumerate() {
                    let selected = index == selected_index;
                    draw_screen_sphere(
                        &mut overlay,
                        v.pos,
                        if selected { 8.0 } else { 5.0 },
                        if selected { PREVIEW_COLOR } else { crate::ui::SELECTION_COLOR_F32 },
                    );
                }
            }
            (Object::Point { pos, .. }, Some(ObjectPoint::Vertex(0))) => {
                draw_screen_sphere(&mut overlay, *pos, 8.0, PREVIEW_COLOR);
            }
            _ => {}
        }
    }

    if !editor.fuse_segments.is_empty() || editor.fuse_awaiting_endpoint.is_some() {
        let mut chain_pts: Vec<DVec3> = Vec::new();
        for seg in &editor.fuse_segments {
            if let Some(Object::Polyline { verts, .. }) = document.get_object(seg.object_id) {
                let mut ordered: Vec<DVec3> = if seg.closed {
                    (0..verts.len()).map(|offset| verts[(seg.start_index + offset) % verts.len()].pos).collect()
                } else if seg.reversed {
                    verts.iter().rev().map(|v| v.pos).collect()
                } else {
                    verts.iter().map(|v| v.pos).collect()
                };
                if chain_pts.last().zip(ordered.first()).is_some_and(|(a, b)| (*a - *b).length_squared() <= 1.0e-16) {
                    ordered.remove(0);
                }
                chain_pts.extend(ordered);
            }
        }
        for pair in chain_pts.windows(2) {
            draw_line(&mut overlay, pair[0], pair[1], DOC_LINE_WIDTH, PREVIEW_COLOR);
        }

        if let Some(tail) = editor.fuse_chain_tail {
            draw_screen_sphere(&mut overlay, tail, 9.0, crate::ui::SELECTION_COLOR_F32);
        }

        if editor.fuse_awaiting_endpoint.is_none()
            && let Some(endpoint) = editor.fuse_close_marker
        {
            draw_screen_sphere(&mut overlay, endpoint, 7.0, PREVIEW_COLOR);
        }

        if editor.fuse_awaiting_endpoint.is_some() {
            for &(_, marker) in &editor.fuse_endpoint_markers {
                let is_tail = editor.fuse_chain_tail.is_some_and(|tail| (tail - marker).length_squared() < 1e-10);
                if !is_tail {
                    draw_screen_sphere(&mut overlay, marker, 7.0, PREVIEW_COLOR);
                }
            }
        }
    }
}
