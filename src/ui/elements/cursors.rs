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
/// Returns the rect it occupies, or [`egui::Rect::NOTHING`] when the viewport
/// is too small to draw it.
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

/// Half-width of the cursor's crosshair arms, in points.
const CURSOR_ARM: f32 = 12.0;
/// Half-width of the glyph a snap cursor carries inside its crosshair. Smaller
/// than the arms in the same proportion the toolbar icons use, so the cursor
/// and the button that arms it read as the same mark.
const CURSOR_GLYPH: f32 = 8.0;
/// Width of the dark halo drawn under every cursor stroke. The scene behind it
/// is arbitrary - a bright raster, a dark background - so the mark carries its
/// own contrast rather than relying on what it lands on.
const CURSOR_HALO_WIDTH: f32 = 3.2;
/// Width of the light core drawn over the halo, matching the 1.46-in-24 stroke
/// the cursor icons in `res/ui/` are drawn with.
const CURSOR_CORE_WIDTH: f32 = 1.45;
/// Radius of the hole left in the middle of a bare crosshair. The point the
/// cursor is reporting is the one thing it must not cover.
const CURSOR_CENTRE_GAP: f32 = 3.5;
/// Radius of the dot marking the point a snap actually caught. Small enough to
/// sit inside the aperture without filling it back in.
const SNAP_DOT_RADIUS: f32 = 2.2;

/// The mark drawn in place of the pointer, chosen by the active workspace's
/// cursor rather than by the active tool: what a pick *does* is the question
/// the cursor answers.
#[derive(Clone, Copy)]
enum CursorGlyph {
    /// Crosshair alone - a plain pick.
    None,
    Circle,
    Square,
    Triangle,
}

/// The mark for the cursor the active workspace is holding.
///
/// A cursor wears one or the other, never both: a crosshair where the pick is
/// plain and has only a position to report, and a bare shape where it has a
/// target, which the shape then frames rather than covers.
fn cursor_glyph(editor: &crate::ui::state::EditorState) -> CursorGlyph {
    use crate::ui::state::{BlastCursor, CursorMode, Workspace};

    match editor.active_workspace {
        Workspace::Production => match editor.cursors.production {
            CursorMode::Select => CursorGlyph::None,
            CursorMode::SnapToPoint => CursorGlyph::Circle,
            CursorMode::SnapToLine => CursorGlyph::Square,
            CursorMode::SnapToSurface => CursorGlyph::Triangle,
        },
        Workspace::DrillAndBlast => match editor.cursors.blast {
            BlastCursor::Select => CursorGlyph::None,
        },
        Workspace::Geology => CursorGlyph::None,
    }
}

/// Give the system pointer back after a frame in which the scene had it.
///
/// [`egui::PlatformOutput`]'s cursor icon is sticky between frames: a request
/// stands until something asks for a different one, so hiding the pointer once
/// would hide it over the whole application for good. Only a request this
/// module makes is withdrawn - `None` and `Progress` are asked for nowhere
/// else - which leaves an icon a widget set earlier in the frame, a text
/// field's or a panel seam's, exactly as that widget wanted it.
fn release_pointer(ctx: &egui::Context) {
    if matches!(ctx.output(|output| output.cursor_icon), egui::CursorIcon::None | egui::CursorIcon::Progress) {
        ctx.set_cursor_icon(egui::CursorIcon::Default);
    }
}

/// Hide the system pointer over the open scene and draw the active cursor in
/// its place; hand the pointer back everywhere else.
///
/// The scene is the only place the drawn cursor belongs, so the pointer is
/// only taken when it is inside `canvas_rect` *and* over nothing egui owns.
/// [`egui::Context::layer_id_at`] answers the second half, but note what it
/// reports over open scene: the background layer, not nothing. Panels and the
/// root ui all paint there, so the background layer means "no floating thing
/// here" rather than "no egui here", and `canvas_rect` is what keeps the
/// explorer, the toolbars and the bars out.
///
/// Everything that floats over the scene and takes input (the orientation
/// gizmo, the slice minimap, the tool panels, every menu and dialog) has an
/// area of its own and gets the ordinary pointer back; the two overlays that
/// are decoration only opt out with `Area::interactable(false)`.
///
/// Painting happens on its own foreground layer with no `Sense`, so the mark
/// sits above the scene without consuming a single click.
pub(crate) fn draw_tool_cursor(ctx: &egui::Context, editor: &crate::ui::state::EditorState, canvas_rect: egui::Rect, camera_active: bool) {
    // A running task owns the pointer everywhere, scene included.
    if editor.background_busy {
        ctx.set_cursor_icon(egui::CursorIcon::Progress);
        return;
    }

    // Flying grabs the pointer to the window, so there is no position to draw
    // at - just hide it, which is what the grab used to do for itself.
    if camera_active && editor.fly_mode_enabled {
        ctx.set_cursor_icon(egui::CursorIcon::None);
        return;
    }

    // `pointer_hover_pos` is already in points and goes `None` once the
    // pointer leaves the window, so the mark cannot be stranded at the edge.
    let Some(pos) = ctx.pointer_hover_pos() else {
        release_pointer(ctx);
        return;
    };
    // Open scene reports as the background layer rather than as nothing, so
    // that is the one layer the cursor is allowed to cover.
    let over_scene_only = ctx.layer_id_at(pos).is_none_or(|layer| layer == egui::LayerId::background());
    // A widget drag that started elsewhere - a minimap pan, a slider - keeps
    // the pointer for as long as it runs, however far over the scene it
    // travels. Scene drags never trip this: the viewport is painted, not a
    // widget, so egui has no id to be holding.
    if !canvas_rect.contains(pos) || !over_scene_only || ctx.egui_is_using_pointer() {
        release_pointer(ctx);
        return;
    }
    ctx.set_cursor_icon(egui::CursorIcon::None);

    let mut painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("tool_cursor")));
    painter.set_clip_rect(canvas_rect);

    // A snapped cursor is sitting on real geometry rather than on the Z plane,
    // which is worth saying at the pointer instead of only in the status bar.
    let core = if editor.cursor_snapped {
        egui::Color32::from_rgb(255, 220, 50)
    } else {
        egui::Color32::from_rgba_unmultiplied(238, 242, 246, 235)
    };
    let halo = egui::Color32::from_rgba_unmultiplied(12, 16, 20, 190);
    let glyph = cursor_glyph(editor);

    // Halo first, then core, so the dark outline never lands on top of the
    // mark it is there to separate from the scene.
    for (color, width) in [(halo, CURSOR_HALO_WIDTH), (core, CURSOR_CORE_WIDTH)] {
        let stroke = egui::Stroke::new(width, color);
        match glyph {
            // Arms only, and stopping short of the middle: the point the
            // cursor is reporting is the one thing it must not cover.
            CursorGlyph::None => {
                for direction in [egui::vec2(1.0, 0.0), egui::vec2(-1.0, 0.0), egui::vec2(0.0, 1.0), egui::vec2(0.0, -1.0)] {
                    painter.line_segment([pos + direction * CURSOR_CENTRE_GAP, pos + direction * CURSOR_ARM], stroke);
                }
            }
            CursorGlyph::Circle => {
                painter.circle_stroke(pos, CURSOR_GLYPH, stroke);
            }
            CursorGlyph::Square => {
                painter.rect_stroke(
                    egui::Rect::from_center_size(pos, egui::Vec2::splat(CURSOR_GLYPH * 2.0)),
                    0.0,
                    stroke,
                    egui::StrokeKind::Middle,
                );
            }
            CursorGlyph::Triangle => {
                // The same upright triangle the snap-to-surface icon carries,
                // centred on the pointer rather than sitting on it.
                let apex = pos - egui::vec2(0.0, CURSOR_GLYPH);
                let left = pos + egui::vec2(-CURSOR_GLYPH, CURSOR_GLYPH * 0.82);
                let right = pos + egui::vec2(CURSOR_GLYPH, CURSOR_GLYPH * 0.82);
                painter.line_segment([apex, left], stroke);
                painter.line_segment([left, right], stroke);
                painter.line_segment([right, apex], stroke);
            }
        }
    }

    // The point the snap actually caught. It is not the pointer - a snap pulls
    // the placement onto the geometry, up to the snap threshold away - so the
    // dot is drawn where the tool will place, not where the mouse is.
    if let Some((x, y)) = editor.snap_marker_px {
        let target = egui::pos2(x / ctx.pixels_per_point(), y / ctx.pixels_per_point());
        painter.circle_filled(target, SNAP_DOT_RADIUS + 1.2, halo);
        painter.circle_filled(target, SNAP_DOT_RADIUS, core);
    }
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
