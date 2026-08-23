//! Canvas overlay helpers: orbit marker, cursor highlights, and view gizmos.

/// Draw the orbit indicator (compass rose) on the canvas.
///
/// The marker is clipped to `clip_rect` so it doesn't bleed over panels.
/// Returns early if the position is outside the clip area.
/// Draw a screen-space world orientation gizmo in the bottom-left of the viewport.
/// Side of the world-axis gizmo.
const GIZMO_SIZE: f32 = 76.0;
/// Gap between the gizmo and the viewport's top-right corner.
const GIZMO_MARGIN: f32 = 16.0;

/// Where the world-axis gizmo lands in `canvas_rect`, without drawing it, or
/// [`egui::Rect::NOTHING`] when the viewport is too small to carry it.
///
/// The view tools hang off the bottom of this, and the overlays that have to
/// keep clear of *them* need it before anything is drawn - so the position is
/// arithmetic the caller can ask for early rather than something only the
/// drawing pass knows.
pub(crate) fn orientation_gizmo_rect(canvas_rect: egui::Rect) -> egui::Rect {
    if canvas_rect.width() < 88.0 || canvas_rect.height() < 88.0 {
        return egui::Rect::NOTHING;
    }
    egui::Rect::from_min_size(
        egui::pos2(canvas_rect.right() - GIZMO_MARGIN - GIZMO_SIZE, canvas_rect.top() + GIZMO_MARGIN),
        egui::Vec2::splat(GIZMO_SIZE),
    )
}

/// Draw the world-axis gizmo in the viewport's top-right corner.
///
/// Returns the rect it occupies, which is what the view tools below it anchor
/// to; [`egui::Rect::NOTHING`] when the viewport is too small to draw it.
pub(crate) fn draw_orientation_gizmo(
    ui: &mut egui::Ui,
    canvas_rect: egui::Rect,
    camera_forward: [f32; 3],
    camera_up: [f32; 3],
    commands: &mut Vec<crate::ui::state::UiCommand>,
) -> egui::Rect {
    let gizmo_rect = orientation_gizmo_rect(canvas_rect);
    if !gizmo_rect.is_positive() {
        return egui::Rect::NOTHING;
    }

    const SIZE: f32 = GIZMO_SIZE;

    egui::Area::new(egui::Id::new("world_orientation_gizmo"))
        .order(egui::Order::Middle)
        .fixed_pos(gizmo_rect.min)
        .show(ui.ctx(), |ui| {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click());
            let forward = normalize3(camera_forward).unwrap_or([0.0, 0.0, -1.0]);
            let up = normalize3(camera_up).unwrap_or([0.0, 1.0, 0.0]);
            let right = normalize3(cross3(forward, up)).unwrap_or([1.0, 0.0, 0.0]);
            let origin = rect.center();
            let axis_defs = [
                ([1.0, 0.0, 0.0], "X", egui::Color32::from_rgb(235, 55, 55)),
                ([0.0, 1.0, 0.0], "Y", egui::Color32::from_rgb(118, 210, 38)),
                ([0.0, 0.0, 1.0], "Z", egui::Color32::from_rgb(58, 136, 225)),
            ];

            let mut nodes: Vec<_> = axis_defs
                .into_iter()
                .flat_map(|(axis, label, color)| {
                    [1.0_f32, -1.0].into_iter().map(move |sign| {
                        let signed_axis = [axis[0] * sign, axis[1] * sign, axis[2] * sign];
                        let screen = egui::vec2(dot3(signed_axis, right), -dot3(signed_axis, up));
                        let depth = dot3(signed_axis, forward);
                        let screen_len = screen.length();
                        let dir = if screen_len >= 0.001 { screen / screen_len } else { egui::vec2(0.0, -1.0) };
                        let length = 31.0 * screen_len;
                        AxisGizmoNode {
                            axis: signed_axis,
                            positive: sign > 0.0,
                            label,
                            color,
                            depth,
                            dir,
                            pos: origin + dir * length,
                        }
                    })
                })
                .collect();
            nodes.sort_by(|a, b| b.depth.total_cmp(&a.depth));

            let mut painter = ui.painter_at(rect);
            painter.set_clip_rect(canvas_rect);

            let draw_origin_dot = |painter: &egui::Painter| {
                painter.circle_filled(origin, 4.0, egui::Color32::from_rgba_unmultiplied(238, 242, 246, 235));
            };
            let mut origin_dot_drawn = false;

            for node in &nodes {
                if !origin_dot_drawn && node.depth < 0.0 {
                    draw_origin_dot(&painter);
                    origin_dot_drawn = true;
                }

                let front_factor = ((-node.depth + 1.0) * 0.5).clamp(0.0, 1.0);
                let alpha = lerp_u8(80, 245, front_factor);
                let color = egui::Color32::from_rgba_unmultiplied(node.color.r(), node.color.g(), node.color.b(), alpha);
                let stem_alpha = lerp_u8(45, 180, front_factor);
                let stem_color = egui::Color32::from_rgba_unmultiplied(node.color.r(), node.color.g(), node.color.b(), stem_alpha);

                if node.pos.distance(origin) > 3.0 {
                    painter.line_segment([origin, node.pos], egui::Stroke::new(2.0, stem_color));
                }

                if node.positive {
                    painter.circle_filled(node.pos, 8.0, color);
                    painter.text(
                        node.pos,
                        egui::Align2::CENTER_CENTER,
                        node.label,
                        egui::FontId::proportional(12.5),
                        egui::Color32::from_rgb(25, 32, 40),
                    );
                } else {
                    painter.circle_stroke(node.pos, 6.0, egui::Stroke::new(1.4, color));
                }
            }

            if !origin_dot_drawn {
                draw_origin_dot(&painter);
            }

            if response.clicked()
                && let Some(pos) = response.hover_pos()
                && let Some(axis) = nearest_axis_node(pos, &nodes)
            {
                commands.push(crate::ui::state::UiCommand::SetStandardView(standard_view_for_axis(axis)));
            }
        })
        .response
        .rect
}

pub(crate) fn draw_orbit_marker(ui: &mut egui::Ui, ox: f32, oy: f32, clip_rect: egui::Rect) {
    let ppp = ui.ctx().pixels_per_point();
    let pos = egui::pos2(ox / ppp, oy / ppp);
    if !clip_rect.contains(pos) {
        return;
    }
    let mut painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("orbit_marker")));
    painter.set_clip_rect(clip_rect);
    let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(255, 180, 0, 220));
    let r = 4.0;
    painter.circle_stroke(pos, r, stroke);
    painter.line_segment([pos - egui::vec2(r + 4.0, 0.0), pos + egui::vec2(r + 4.0, 0.0)], stroke);
    painter.line_segment([pos - egui::vec2(0.0, r + 4.0), pos + egui::vec2(0.0, r + 4.0)], stroke);
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize3(v: [f32; 3]) -> Option<[f32; 3]> {
    let len = dot3(v, v).sqrt();
    (len > f32::EPSILON).then(|| [v[0] / len, v[1] / len, v[2] / len])
}

fn lerp_u8(from: u8, to: u8, t: f32) -> u8 {
    (from as f32 + (to as f32 - from as f32) * t.clamp(0.0, 1.0)).round() as u8
}

#[derive(Clone, Copy)]
struct AxisGizmoNode {
    axis: [f32; 3],
    positive: bool,
    label: &'static str,
    color: egui::Color32,
    depth: f32,
    dir: egui::Vec2,
    pos: egui::Pos2,
}

fn nearest_axis_node(pos: egui::Pos2, nodes: &[AxisGizmoNode]) -> Option<[f32; 3]> {
    const NODE_HIT_RADIUS: f32 = 12.0;
    const STEM_HIT_RADIUS: f32 = 6.0;

    let mut best: Option<([f32; 3], f32)> = None;
    for node in nodes {
        let node_dist = pos.distance(node.pos);
        if node_dist <= NODE_HIT_RADIUS && best.is_none_or(|(_, best_dist)| node_dist < best_dist) {
            best = Some((node.axis, node_dist));
        }

        let stem_point = node.pos - node.dir * 15.0;
        let stem_dist = distance_to_segment(pos, stem_point, node.pos);
        if stem_dist <= STEM_HIT_RADIUS {
            let weighted_dist = stem_dist + 4.0;
            if best.is_none_or(|(_, best_dist)| weighted_dist < best_dist) {
                best = Some((node.axis, weighted_dist));
            }
        }
    }
    best.map(|(axis, _)| axis)
}

fn distance_to_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ap = p - a;
    let ab = b - a;
    let len_sq = ab.dot(ab);
    if len_sq <= f32::EPSILON {
        return ap.length();
    }
    let t = (ap.dot(ab) / len_sq).clamp(0.0, 1.0);
    (ap - ab * t).length()
}

fn standard_view_for_axis(axis: [f32; 3]) -> crate::ui::state::StandardView {
    if axis[0] > 0.5 {
        crate::ui::state::StandardView::East
    } else if axis[0] < -0.5 {
        crate::ui::state::StandardView::West
    } else if axis[1] > 0.5 {
        crate::ui::state::StandardView::North
    } else if axis[1] < -0.5 {
        crate::ui::state::StandardView::South
    } else if axis[2] > 0.5 {
        crate::ui::state::StandardView::Up
    } else {
        crate::ui::state::StandardView::Down
    }
}
