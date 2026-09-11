use std::collections::HashSet;

use winit::keyboard::PhysicalKey;

use super::{frustum::Frustum, *};
use crate::{
    rendering::pick::{clamped_range, local_vertex_world, slab_clipped_screen_segment, slab_clipped_segment, slab_screen_point},
    ui::state::ActiveTool,
};

/// Ground distance a view with nothing to frame spans across the viewport, in
/// metres. Mine design starts at pit scale, so an empty scene opens on a few
/// hundred metres rather than the couple of metres a unit zoom would give.
const DEFAULT_VIEW_WIDTH: f64 = 200.0;

/// Ortho half-height that puts [`DEFAULT_VIEW_WIDTH`] across the viewport at
/// this aspect ratio - the fallback zoom whenever there are no extents to fit.
fn default_zoom(aspect: f64) -> f64 {
    // A degenerate surface (zero width, a minimised window) would otherwise
    // turn a near-zero aspect into an astronomical half-height.
    DEFAULT_VIEW_WIDTH / (2.0 * aspect.clamp(0.1, 10.0))
}

/// Merge per-object AABBs into a single scene AABB, or `None` when empty.
fn merge_aabbs(aabbs: &[(DVec3, DVec3)]) -> Option<(DVec3, DVec3)> {
    aabbs.iter().copied().reduce(|(acc_min, acc_max), (min, max)| (acc_min.min(min), acc_max.max(max)))
}

/// What a scene pick landed on.
///
/// `entity` is the scene entity the selection sets are keyed by; `hole` is
/// filled in when that entity is a drill hole dataset and says which of its
/// holes was actually under the cursor. Production selects the dataset and
/// ignores it; Drill & Blast works a hole at a time and does not.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScenePick {
    pub(crate) entity: SceneEntityId,
    pub(crate) world: DVec3,
    pub(crate) hole: Option<DrillHoleRef>,
}

#[derive(Clone, Copy, Debug)]
struct ScreenRect {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

impl ScreenRect {
    fn new(start: (f32, f32), end: (f32, f32)) -> Self {
        Self {
            min_x: f64::from(start.0.min(end.0)),
            max_x: f64::from(start.0.max(end.0)),
            min_y: f64::from(start.1.min(end.1)),
            max_y: f64::from(start.1.max(end.1)),
        }
    }

    fn contains(self, point: DVec2) -> bool {
        point.x >= self.min_x && point.x <= self.max_x && point.y >= self.min_y && point.y <= self.max_y
    }

    fn corners(self) -> [DVec2; 4] {
        [
            DVec2::new(self.min_x, self.min_y),
            DVec2::new(self.max_x, self.min_y),
            DVec2::new(self.max_x, self.max_y),
            DVec2::new(self.min_x, self.max_y),
        ]
    }
}

fn point_in_polygon(point: DVec2, polygon: &[DVec2]) -> bool {
    let mut left = false;
    let mut right = false;
    for (index, &from) in polygon.iter().enumerate() {
        let to = polygon[(index + 1) % polygon.len()];
        let side = (to - from).perp_dot(point - from);
        left |= side > 0.0;
        right |= side < 0.0;
    }
    !(left && right)
}

fn polygon_touches_rect(polygon: &[DVec2], rect: ScreenRect) -> bool {
    if polygon.iter().any(|&point| rect.contains(point)) {
        return true;
    }
    if polygon
        .iter()
        .enumerate()
        .any(|(index, &from)| segment_intersects_rect(from, polygon[(index + 1) % polygon.len()], rect.min_x, rect.max_x, rect.min_y, rect.max_y))
    {
        return true;
    }
    rect.corners().iter().any(|&corner| point_in_polygon(corner, polygon))
}

fn polygon_inside_rect(polygon: &[DVec2], rect: ScreenRect) -> bool {
    polygon.iter().all(|&point| rect.contains(point))
}

/// One wall of the cut below (`wall` picks which); a point's depth here is positive on the side the section shows.
fn clip_polygon_to_wall(polygon: &[DVec3], slab: SectionSlab, wall: f64) -> Vec<DVec3> {
    let depth = |point: DVec3| slab.half_width - wall * slab.signed_distance(point);
    let mut kept = Vec::with_capacity(polygon.len() + 1);
    for (index, &to) in polygon.iter().enumerate() {
        let from = polygon[(index + polygon.len() - 1) % polygon.len()];
        let (from_depth, to_depth) = (depth(from), depth(to));
        if (from_depth < 0.0) != (to_depth < 0.0) {
            kept.push(from.lerp(to, from_depth / (from_depth - to_depth)));
        }
        if to_depth >= 0.0 {
            kept.push(to);
        }
    }
    kept
}

/// The part of a convex world-space polygon that lies between the section's two walls, or `None` when the section shows none of it.
fn slab_clipped_polygon(slab: Option<SectionSlab>, polygon: &[DVec3]) -> Option<Vec<DVec3>> {
    let Some(slab) = slab else {
        return Some(polygon.to_vec());
    };
    let mut kept = polygon.to_vec();
    for wall in [1.0, -1.0] {
        kept = clip_polygon_to_wall(&kept, slab, wall);
        if kept.len() < 3 {
            return None;
        }
    }
    Some(kept)
}

fn project_polygon(view_proj: &DMat4, screen: Size, polygon: &[DVec3]) -> Option<Vec<DVec2>> {
    polygon.iter().map(|&point| crate::rendering::pick::world_to_screen(view_proj, point, screen)).collect()
}

/// Every rendered triangle of one pick record, cut to the part the section shows and projected to the screen; a stroke quad can span the slab with both ends beyond the walls, so only the cut piece is judged.
fn visible_screen_polygons(group: &PickGeometry<'_>, record: &PickRecord, scene_origin: DVec3, slab: Option<SectionSlab>, view_proj: &DMat4, screen: Size) -> Vec<Vec<DVec2>> {
    let mut polygons = Vec::new();
    let stroke_indices = &group.stroke_indices[clamped_range(record.stroke_index_range, group.stroke_indices.len())];
    let fill_indices = &group.fill_indices[clamped_range(record.fill_index_range, group.fill_indices.len())];
    for indices in stroke_indices.as_chunks::<3>().0 {
        let [Some(a), Some(b), Some(c)] = indices.map(|index| group.stroke_verts.get(index as usize)) else {
            continue;
        };
        let corners = [a, b, c].map(|vertex| local_vertex_world(vertex.pos, scene_origin));
        if let Some(shown) = slab_clipped_polygon(slab, &corners)
            && let Some(projected) = project_polygon(view_proj, screen, &shown)
        {
            polygons.push(projected);
        }
    }
    for indices in fill_indices.as_chunks::<3>().0 {
        let [Some(a), Some(b), Some(c)] = indices.map(|index| group.fill_verts.get(index as usize)) else {
            continue;
        };
        let corners = [a, b, c].map(|vertex| local_vertex_world(vertex.pos, scene_origin));
        if let Some(shown) = slab_clipped_polygon(slab, &corners)
            && let Some(projected) = project_polygon(view_proj, screen, &shown)
        {
            polygons.push(projected);
        }
    }
    polygons
}

fn visible_screen_vertices<'group>(
    group: &'group PickGeometry<'_>,
    record: &PickRecord,
    scene_origin: DVec3,
    slab: Option<SectionSlab>,
    view_proj: &'group DMat4,
    screen: Size,
) -> impl Iterator<Item = DVec2> + 'group {
    let stroke = clamped_range(record.stroke_range, group.stroke_verts.len());
    let fill = clamped_range(record.fill_range, group.fill_verts.len());
    group.stroke_verts[stroke]
        .iter()
        .map(|vertex| vertex.pos)
        .chain(group.fill_verts[fill].iter().map(|vertex| vertex.pos))
        .filter_map(move |position| slab_screen_point(slab, view_proj, screen, local_vertex_world(position, scene_origin)))
}

fn hole_legs(hole: &crate::model::drill_hole::DrillHole) -> impl Iterator<Item = (DVec3, DVec3)> + '_ {
    let point = (hole.trace.len() == 1).then(|| (hole.trace[0].position, hole.trace[0].position));
    hole.trace.windows(2).map(|pair| (pair[0].position, pair[1].position)).chain(point)
}

/// The part of a text label's quad the section shows, projected to the screen; the label is one quad, clipped whole, so one straddling a wall is measured only by its shown part.
fn visible_text_polygon(record: &TextPickRecord, slab: Option<SectionSlab>, view_proj: &DMat4, screen: Size) -> Option<Vec<DVec2>> {
    project_polygon(view_proj, screen, &slab_clipped_polygon(slab, &record.corners)?)
}

fn pick_record_touches_rect(
    group: &PickGeometry<'_>,
    record: &PickRecord,
    scene_origin: DVec3,
    slab: Option<SectionSlab>,
    view_proj: &DMat4,
    screen: Size,
    rect: ScreenRect,
) -> bool {
    if !pick_record_bounds_may_touch_rect(record, view_proj, screen, rect) {
        return false;
    }
    if visible_screen_vertices(group, record, scene_origin, slab, view_proj, screen).any(|point| rect.contains(point)) {
        return true;
    }
    visible_screen_polygons(group, record, scene_origin, slab, view_proj, screen)
        .iter()
        .any(|polygon| polygon_touches_rect(polygon, rect))
}

fn pick_record_bounds_may_touch_rect(record: &PickRecord, view_proj: &DMat4, screen: Size, rect: ScreenRect) -> bool {
    let cursor = DVec2::new((rect.min_x + rect.max_x) * 0.5, (rect.min_y + rect.max_y) * 0.5);
    // `projected_box_overlaps` accepts one symmetric threshold. Using the
    // larger half-extent is conservative for a non-square selection box and
    // still rejects records wholly outside it before walking their streams.
    let threshold = ((rect.max_x - rect.min_x) * 0.5).max((rect.max_y - rect.min_y) * 0.5);
    crate::model::spatial::projected_box_overlaps(record.world_bounds.0, record.world_bounds.1, view_proj, screen, cursor, threshold)
}

/// Where the middle-drag pan takes its deltas from. Native uses device
/// motion; the web differences cursor positions instead, since Chromium's
/// first movement value after a press can jump the view by a page-sized step.
const MIDDLE_PAN_FROM_CURSOR: bool = cfg!(target_arch = "wasm32");

/// How far, in physical pixels, a click may miss a string or trace and still
/// fix the centre on it: twice the snap glyph's reach.
const ROTATION_CENTRE_PICK_PX: f32 = SNAP_THRESHOLD_PX * 2.0;

/// Where the eye sits after a rigid turn about a fixed `centre`: the same
/// offset (`screen_x` right, `screen_y` up, `depth` forward) in the new basis.
pub(super) fn eye_keeping_centre(centre: DVec3, screen_x: f64, screen_y: f64, depth: f64, basis: (DVec3, DVec3, DVec3)) -> DVec3 {
    let (forward, right, up) = basis;
    centre - right * screen_x - up * screen_y - forward * depth
}

/// The eye slid along the view onto the section plane through `center`, which
/// an orthographic view cannot see; left alone when the view runs along it.
pub(super) fn slide_onto_plane(eye: DVec3, center: DVec3, forward: DVec3, normal: DVec3) -> DVec3 {
    let incidence = forward.dot(normal);
    if incidence.abs() < crate::rendering::camera::MIN_SECTION_INCIDENCE.sin() {
        return eye;
    }
    eye - forward * ((eye - center).dot(normal) / incidence)
}

/// The move within a vertical section plane that shifts the view by `dx` along
/// screen right and `dy` up, so a drag tracks the hand at any orbit; held to
/// the pitch clamp's own bound, since edge-on by yaw the move is unbounded.
pub(super) fn in_plane_screen_move(dx: f64, dy: f64, right: DVec3, up: DVec3, normal: DVec3) -> Option<DVec3> {
    let strike = DVec3::Z.cross(normal).normalize_or(DVec3::X);
    let (a, b, c, d) = (strike.dot(right), DVec3::Z.dot(right), strike.dot(up), DVec3::Z.dot(up));
    let determinant = a * d - b * c;
    if determinant.abs() < 1.0e-9 {
        return None;
    }
    let along = (dx * d - dy * b) / determinant;
    let rise = (dy * a - dx * c) / determinant;
    let shift = strike * along + DVec3::Z * rise;
    Some(shift.clamp_length_max(dx.hypot(dy) / crate::rendering::camera::MIN_SECTION_INCIDENCE.sin()))
}

/// Camera forward and up for a standard view; the section takes the same pair
/// to turn its plane to, so the two views agree on where "north" looks.
pub(super) fn standard_view_basis(view: crate::ui::state::StandardView) -> (DVec3, DVec3) {
    match view {
        crate::ui::state::StandardView::Up => (DVec3::NEG_Z, DVec3::Y),
        crate::ui::state::StandardView::Down => (DVec3::Z, DVec3::Y),
        crate::ui::state::StandardView::North => (DVec3::NEG_Y, DVec3::Z),
        crate::ui::state::StandardView::South => (DVec3::Y, DVec3::Z),
        crate::ui::state::StandardView::West => (DVec3::X, DVec3::Z),
        crate::ui::state::StandardView::East => (DVec3::NEG_X, DVec3::Z),
    }
}

/// The inverse of `eye_keeping_centre`: the eye's offset from a fixed `centre`.
pub(super) fn eye_offset_from(centre: DVec3, eye: DVec3, basis: (DVec3, DVec3, DVec3)) -> (f64, f64, f64) {
    let (forward, right, up) = basis;
    let to_centre = centre - eye;
    (to_centre.dot(right), to_centre.dot(up), to_centre.dot(forward))
}

impl<'a> Graphics<'a> {
    pub(crate) fn process_mouse_motion(&mut self, dx: f64, dy: f64) -> bool {
        if self.fly_mode_enabled && self.mouse_pressed == Some(MouseButton::Right) {
            self.fly_camera_controller.process_mouse_motion(dx, dy);
            return true;
        }
        if MIDDLE_PAN_FROM_CURSOR || self.mouse_pressed != Some(MouseButton::Middle) {
            return false;
        }
        self.pan_by(dx, dy)
    }

    /// Accumulate a middle-drag pan.
    fn pan_by(&mut self, dx: f64, dy: f64) -> bool {
        if self.fly_mode_enabled {
            return false;
        }
        if let Some(slice) = self.slice_view.as_mut() {
            // Same accumulation convention as the camera controller's pan so
            // the drag feel matches; consumed by `update_slice_camera`.
            slice.pan.x += -dx;
            slice.pan.y += dy;
            return true;
        }
        self.camera_controller.process_mouse(Some(MouseButton::Middle), dx, dy)
    }

    /// On the web every cursor move reaches the camera, even one egui claims,
    /// so the tracked position doesn't go stale while the pointer crosses a panel.
    pub(crate) fn track_cursor_through_gui(&mut self, mouse_loc: (f32, f32)) -> bool {
        MIDDLE_PAN_FROM_CURSOR && self.set_mouse_location(mouse_loc)
    }

    pub(crate) fn set_mouse_location(&mut self, mouse_loc: (f32, f32)) -> bool {
        let mouse_loc = self.window_to_viewport_px(mouse_loc);
        let previous_mouse_loc = self.camera_controller.mouse_loc;
        self.camera_controller.mouse_loc = mouse_loc;
        let dx = f64::from(mouse_loc.0 - previous_mouse_loc.0);
        let dy = f64::from(mouse_loc.1 - previous_mouse_loc.1);

        if MIDDLE_PAN_FROM_CURSOR && self.mouse_pressed == Some(MouseButton::Middle) {
            return self.pan_by(dx, dy);
        }

        if self.mouse_pressed == Some(MouseButton::Right) && !self.fly_mode_enabled {
            if let Some(slice) = self.slice_view.as_mut() {
                // Rebuilt from slice state each tick, so a rotation here would be overwritten; it accumulates on slice state instead (see `begin_slice_orbit_drag`).
                if !slice.orbit_dragging {
                    return false;
                }
                slice.orbit += DVec2::new(dx, dy);
                return true;
            }
            return self.camera_controller.process_mouse(self.mouse_pressed, dx, dy);
        }

        false
    }

    /// Convert a window-space physical pixel coordinate (e.g. a raw winit
    /// cursor position, or `EditorState::cursor_screen_px`) into the
    /// viewport-relative space `screen_size()`/`view_proj()` operate in.
    pub(crate) fn window_to_viewport_px(&self, px: (f32, f32)) -> (f32, f32) {
        (px.0 - self.viewport_rect.x as f32, px.1 - self.viewport_rect.y as f32)
    }

    /// Convert a viewport-relative pixel coordinate back to window space, for
    /// values that end up compared against raw cursor positions or drawn by
    /// egui (which lays out in window space).
    pub(crate) fn viewport_to_window_px(&self, px: (f32, f32)) -> (f32, f32) {
        (px.0 + self.viewport_rect.x as f32, px.1 + self.viewport_rect.y as f32)
    }

    /// Project a world point to a window-space physical pixel, for UI/tool
    /// overlays that egui draws or that are hit-tested against a raw cursor
    /// position. Internal camera-ray picking should use `screen_size()`
    /// directly instead, since it already compares against the
    /// viewport-relative `camera_controller.mouse_loc`.
    pub(crate) fn world_to_window_px(&self, view_proj: &DMat4, world: DVec3) -> Option<(f32, f32)> {
        let px = crate::rendering::pick::world_to_screen(view_proj, world, self.screen_size())?;
        Some(self.viewport_to_window_px((px.x as f32, px.y as f32)))
    }

    /// Like [`world_to_window_px`] but keeps points whose depth falls outside
    /// the scene-fitted near/far slab. Tool previews (offset, batter/berm) are
    /// foreground overlays, not scene geometry: their screen position stays
    /// meaningful even when a tightly-fitted depth range - now fitted to the
    /// panel-cropped viewport frustum - would reject the world point itself.
    pub(crate) fn world_to_window_px_unclipped_depth(&self, view_proj: &DMat4, world: DVec3) -> Option<(f32, f32)> {
        let px = crate::rendering::pick::world_to_screen_unclipped_depth(view_proj, world, self.screen_size())?;
        Some(self.viewport_to_window_px((px.x as f32, px.y as f32)))
    }

    pub(super) fn screen_size(&self) -> Size {
        (self.viewport_rect.width as f32, self.viewport_rect.height as f32)
    }

    /// How far the view has to slide for what sits at the centre of the scene
    /// region to sit at the centre of the window instead, in viewport pixels.
    fn window_centring_offset_px(&self) -> DVec2 {
        let window_centre = DVec2::new(f64::from(self.size.width), f64::from(self.size.height)) / 2.0;
        let viewport_centre = DVec2::new(
            f64::from(self.viewport_rect.x) + f64::from(self.viewport_rect.width) / 2.0,
            f64::from(self.viewport_rect.y) + f64::from(self.viewport_rect.height) / 2.0,
        );
        window_centre - viewport_centre
    }

    /// Slide the view by `shift` viewport pixels, the camera moving the other
    /// way from the content it shows. Orthographic only: under perspective a
    /// pixel is not a fixed world distance.
    fn translate_view_by_pixels(&mut self, shift: DVec2) {
        if self.projection.is_perspective() {
            return;
        }
        let world_per_pixel = 2.0 * self.projection.zoom / f64::from(self.viewport_rect.height.max(1));
        let forward = self.camera.forward();
        let right = forward.cross(self.camera.up()).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        self.camera.translate((up * shift.y - right * shift.x) * world_per_pixel);
    }

    /// Frame the origin on the centre of the window rather than the centre of
    /// the scene region, for as long as the startup splash is up.
    ///
    /// The splash is centred on the window, but the scene is only the part of
    /// the window the panels leave, so the origin behind the splash would sit
    /// off to one side of it by half of what the panels take. Only the
    /// difference from the offset already applied is panned each frame, which
    /// matters because the first frame's layout is provisional - egui settles
    /// the panel sizes a frame or two later, and a one-shot taken on frame 0
    /// bakes in a rect that no longer exists. Tracking the layout also keeps
    /// the framing right if a panel is dragged while the splash is up.
    ///
    /// When the splash goes, so does the tracking, for good: the camera keeps
    /// the framing it has, so nothing moves under the user at that moment.
    pub(super) fn track_startup_view_framing(&mut self, splash_visible: bool) {
        let Some(applied) = self.startup_view_offset else {
            return;
        };
        if !splash_visible {
            self.startup_view_offset = None;
            return;
        }
        let wanted = self.window_centring_offset_px();
        // Sub-pixel corrections are not worth a camera move, but they are
        // worth remembering, or they accumulate into one that is.
        if (wanted - applied).abs().max_element() >= 0.5 {
            self.translate_view_by_pixels(wanted - applied);
            self.startup_view_offset = Some(wanted);
        }
    }

    pub(crate) fn zoom(&self) -> f64 {
        self.projection.zoom
    }

    pub(crate) fn screen_size_pub(&self) -> Size {
        self.screen_size()
    }

    pub(crate) fn view_proj(&self) -> DMat4 {
        self.projection.calc_matrix() * self.camera.calc_matrix() * self.exaggeration_matrix()
    }

    pub(super) fn exaggeration_matrix(&self) -> DMat4 {
        DMat4::from_translation(self.scene_origin) * DMat4::from_scale(DVec3::new(1.0, 1.0, self.vertical_exaggeration)) * DMat4::from_translation(-self.scene_origin)
    }

    pub(super) fn exaggerate_point(&self, point: DVec3) -> DVec3 {
        self.scene_origin
            + DVec3::new(
                point.x - self.scene_origin.x,
                point.y - self.scene_origin.y,
                (point.z - self.scene_origin.z) * self.vertical_exaggeration,
            )
    }

    pub(super) fn unexaggerate_point(&self, point: DVec3) -> DVec3 {
        self.scene_origin
            + DVec3::new(
                point.x - self.scene_origin.x,
                point.y - self.scene_origin.y,
                (point.z - self.scene_origin.z) / self.vertical_exaggeration,
            )
    }

    pub(super) fn cursor_model_ray(&self) -> (DVec3, DVec3) {
        // Unproject through the exact matrix used for rendering. The previous
        // implementation moved the origin 1e9 units behind the cursor, which
        // lost enough floating-point precision to visibly offset BVH hits.
        let screen = self.screen_size();
        let cursor = self.camera_controller.mouse_loc;
        let ndc = crate::rendering::camera::point(cursor.0, cursor.1, screen);
        let inverse = self.view_proj().inverse();
        let near_h = inverse * DVec4::new(ndc.x, ndc.y, 1.0, 1.0);
        let far_h = inverse * DVec4::new(ndc.x, ndc.y, 0.0, 1.0);
        let near = near_h.truncate() / near_h.w;
        let far = far_h.truncate() / far_h.w;
        (near, (far - near).normalize())
    }

    /// True (unexaggerated) world point where viewport-relative physical
    /// pixel `px` meets the current section plane, or `None` when there is
    /// no sound answer. A slice camera looks horizontally, so this unprojects
    /// onto the section plane rather than a horizontal Z plane.
    /// `cursor_world_at_target_depth` cannot substitute: its offset from the
    /// section changes with zoom, so picks at different zoom land off-plane.
    pub(super) fn section_point_at_px(&self, px: (f32, f32)) -> Option<DVec3> {
        let screen = self.screen_size();
        let aspect = screen.0 as f64 / screen.1.max(1.0) as f64;
        let slice = self.slice_view.as_ref()?;
        let on_section = screen_to_world_on_section_plane(&self.camera, self.projection.zoom, aspect, screen, px, slice.center, slice.normal());
        // Feeds the coordinate readout, measure tools, and placed geometry;
        // `None` covers a degenerate viewport or an edge-on section.
        on_section.filter(|point| point.is_finite()).map(|point| self.unexaggerate_point(point))
    }

    /// World coordinate under the current cursor, using the last cursor
    /// position tracked by the camera controller. In plan and 3D views the
    /// point lies on the plane `z = plane_z`; in the vertical slice view it
    /// lies on the section plane and `plane_z` is ignored.
    pub(crate) fn cursor_world(&self, plane_z: f64) -> Option<DVec3> {
        // In slice mode this is always the section-plane point under the cursor.
        if self.slice_view.is_some() {
            return self.section_point_at_px(self.camera_controller.mouse_loc);
        }
        let screen = self.screen_size();
        let aspect = screen.0 as f64 / screen.1.max(1.0) as f64;
        let displayed_plane_z = self.scene_origin.z + (plane_z - self.scene_origin.z) * self.vertical_exaggeration;
        screen_to_world_on_plane(&self.camera, self.projection.zoom, aspect, screen, self.camera_controller.mouse_loc, displayed_plane_z)
            .map(|point| self.unexaggerate_point(point))
    }

    /// All pickable CPU geometry: the per-rebuild stream plus every visible
    /// static stroke chunk. Chunks whose layer is hidden are excluded, matching
    /// the stream path where hidden objects emit no pick records.
    pub(super) fn pick_geometry_groups(&self) -> Vec<PickGeometry<'_>> {
        let mut groups = vec![PickGeometry {
            world_bounds: None,
            records: &self.pick_records,
            stroke_verts: &self.stroke_vertex_buf,
            stroke_indices: &self.stroke_index_buf,
            fill_verts: &self.lyon_buffer.vertices,
            fill_indices: &self.lyon_buffer.indices,
        }];
        for chunk in self.static_strokes.chunks() {
            if !chunk.layer_visible || chunk.records.is_empty() {
                continue;
            }
            groups.push(PickGeometry {
                world_bounds: chunk.world_bounds,
                records: &chunk.records,
                stroke_verts: &chunk.vertices,
                stroke_indices: &chunk.indices,
                fill_verts: &[],
                fill_indices: &[],
            });
        }
        groups
    }

    /// Nearest rendered entity geometry under the cursor, as `(handle, world)`
    /// with the geometry's true world position (including Z). Frozen handles are
    /// visible but excluded from picking. `None` if no geometry is within
    /// `threshold_px`.
    pub(crate) fn pick_at_cursor(
        &self,
        threshold_px: f32,
        triangulations: &[OpenTriangulation],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        xray_enabled: bool,
    ) -> Option<(SceneEntityId, DVec3)> {
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let geometry_hit = pick_nearest(
            &self.pick_geometry_groups(),
            self.scene_origin,
            &view_proj,
            screen,
            self.camera_controller.mouse_loc,
            threshold_px,
            frozen,
            self.section_slab(),
        );
        let text_hit = pick_text(&self.text_pick_records, &view_proj, screen, self.camera_controller.mouse_loc, frozen, self.section_slab());

        let (ray_origin, direction) = self.cursor_model_ray();
        let document_hit = match (geometry_hit, text_hit) {
            (Some(geometry), Some(text)) => {
                let geometry_depth = (geometry.world - ray_origin).dot(direction);
                let text_depth = (text.world - ray_origin).dot(direction);
                Some(if text_depth < geometry_depth { text } else { geometry })
            }
            (geometry, text) => geometry.or(text),
        };
        let surface_hit = SceneQuery::nearest_surface(triangulations, hidden, Some(frozen), ray_origin, direction);

        // A hit within the pick radius can be several pixels from the cursor.
        // Test its visibility at the line itself: comparing against the surface
        // under the cursor shifts the clickable region on sloping triangles.
        let document_hit = document_hit.filter(|hit| xray_enabled || !SceneQuery::surface_occludes_pick(triangulations, hidden, &view_proj, self.scene_origin, hit.world));
        let hit = document_hit.map(|hit| (hit.entity, hit.world)).or(surface_hit);
        // Reject before either return path: x-ray skips the occlusion filter that would otherwise catch a surface hit far behind the section.
        let hit = hit.filter(|(_, world)| self.slab_contains(*world));
        if xray_enabled {
            return hit;
        }
        hit.filter(|(_, world)| !self.nonselectable_asset_occludes(*world, hidden, &view_proj, screen))
    }

    /// Pick across every selectable scene family. The legacy picker remains
    /// available to editing tools that intentionally accept only design
    /// objects and triangulations.
    pub(crate) fn pick_scene_entity_at_cursor(
        &self,
        threshold_px: f32,
        triangulations: &[OpenTriangulation],
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        xray_enabled: bool,
    ) -> Option<ScenePick> {
        let plain = |(entity, world): (SceneEntityId, DVec3)| ScenePick { entity, world, hole: None };
        let document_or_surface = self.pick_at_cursor(threshold_px, triangulations, hidden, frozen, xray_enabled);
        // X-ray explicitly gives document geometry priority through opaque
        // assets. Assets remain pickable where no document geometry is hit.
        if xray_enabled && let Some(hit) = document_or_surface {
            return Some(plain(hit));
        }

        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let (ray_origin, ray_direction) = self.cursor_model_ray();
        let drill_hole = SceneQuery::nearest_drill_hole(
            drill_holes,
            hidden,
            frozen,
            ray_origin,
            ray_direction,
            self.camera.forward(),
            &view_proj,
            screen,
            threshold_px,
        )
        .map(|(hole, world)| ScenePick {
            entity: SceneEntityId::DrillHole(hole.dataset),
            world,
            hole: Some(hole),
        });
        let block_model = self.block_model_gpu.nearest_visible_entity_hit(ray_origin, ray_direction, hidden, frozen);
        let point_cloud = self.point_cloud_gpu.nearest_visible_entity_at_screen(
            &view_proj,
            screen,
            DVec2::new(f64::from(self.camera_controller.mouse_loc.0), f64::from(self.camera_controller.mouse_loc.1)),
            threshold_px,
            hidden,
            frozen,
            self.section_slab(),
        );

        document_or_surface
            .map(plain)
            .into_iter()
            .chain(drill_hole)
            .chain(block_model.map(plain))
            .chain(point_cloud.map(plain))
            // Unbounded-ray hits can sit outside the rendered slab; without this filter one could win over a candidate the user can see.
            .filter(|pick| self.slab_contains(pick.world))
            .min_by(|a, b| (a.world - ray_origin).dot(ray_direction).total_cmp(&(b.world - ray_origin).dot(ray_direction)))
    }

    /// Pick only loaded triangulations. Dialog field pickers use this path so
    /// design strings drawn over a surface do not steal the click intended for
    /// the surface selector.
    pub(crate) fn pick_triangulation_at_cursor(
        &self,
        triangulations: &[OpenTriangulation],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
    ) -> Option<(SceneEntityId, DVec3)> {
        let (ray_origin, direction) = self.cursor_model_ray();
        let hit = SceneQuery::nearest_surface(triangulations, hidden, Some(frozen), ray_origin, direction)?;
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        (self.slab_contains(hit.1) && !self.nonselectable_asset_occludes(hit.1, hidden, &view_proj, screen)).then_some(hit)
    }

    /// Whether `world` lies inside the section's slab; a no-op in plan/3D views, which show all the ground.
    fn slab_contains(&self, world: DVec3) -> bool {
        self.section_slab().is_none_or(|slab| slab.contains(world))
    }

    /// Whether an asset that cannot be selected itself stands in front of a candidate pick and hides it. Both probes use the pick's own slab, so an asset thrown away beyond a wall cannot veto a pick seen through it.
    fn nonselectable_asset_occludes(&self, candidate: DVec3, hidden: &HashSet<SceneEntityId>, view_proj: &DMat4, screen: Size) -> bool {
        let clip = *view_proj * candidate.extend(1.0);
        if clip.w.abs() <= f64::EPSILON {
            return false;
        }
        let candidate_depth = clip.z / clip.w;
        let block_depth = crate::rendering::query::ray_through_world_point(view_proj, candidate)
            .and_then(|(origin, direction)| self.block_model_gpu.nearest_opaque_hit(origin, direction, hidden))
            .filter(|point| self.slab_contains(*point))
            .and_then(|point| {
                let clip = *view_proj * point.extend(1.0);
                (clip.w.abs() > f64::EPSILON).then_some(clip.z / clip.w)
            });
        let point_depth = crate::rendering::pick::world_to_screen(view_proj, candidate, screen)
            .and_then(|screen_point| self.point_cloud_gpu.nearest_depth_at_screen(view_proj, screen, screen_point, hidden, self.section_slab()));
        block_depth
            .into_iter()
            .chain(point_depth)
            .max_by(f64::total_cmp)
            .is_some_and(|depth| candidate_depth < depth - 1.0e-6)
    }

    /// Return design entities whose rendered geometry is fully enclosed by a
    /// physical-pixel selection rectangle.
    pub(crate) fn entities_in_screen_rect(&self, start_px: (f32, f32), end_px: (f32, f32), frozen: &HashSet<SceneEntityId>) -> Vec<SceneEntityId> {
        let rect = ScreenRect::new(self.window_to_viewport_px(start_px), self.window_to_viewport_px(end_px));
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let slab = self.section_slab();
        let mut hits = Vec::new();
        let mut seen = HashSet::new();

        for group in self.pick_geometry_groups() {
            for record in group.records {
                if frozen.contains(&record.entity) {
                    continue;
                }
                if !pick_record_bounds_may_touch_rect(record, &view_proj, screen, rect) {
                    continue;
                }
                // Enclosure is judged on the part the section shows, not raw vertices: a run spanning wall to wall has none inside them, so cut triangles are measured too, with vertices covering point-only geometry.
                let polygons = visible_screen_polygons(&group, record, self.scene_origin, slab, &view_proj, screen);
                let mut any = !polygons.is_empty();
                let mut enclosed = polygons.iter().all(|polygon| polygon_inside_rect(polygon, rect));
                for point in visible_screen_vertices(&group, record, self.scene_origin, slab, &view_proj, screen) {
                    any = true;
                    enclosed &= rect.contains(point);
                }
                if any && enclosed && seen.insert(record.entity) {
                    hits.push(record.entity);
                }
            }
        }

        for record in &self.text_pick_records {
            if frozen.contains(&record.entity) {
                continue;
            }
            let Some(polygon) = visible_text_polygon(record, slab, &view_proj, screen) else {
                continue;
            };
            if polygon_inside_rect(&polygon, rect) && seen.insert(record.entity) {
                hits.push(record.entity);
            }
        }

        hits
    }

    /// Cross-select entities whose visible rendered geometry touches the box,
    /// including a box wholly inside a fill or text quad.
    pub(crate) fn entities_touching_screen_rect(&self, start_px: (f32, f32), end_px: (f32, f32), frozen: &HashSet<SceneEntityId>) -> Vec<SceneEntityId> {
        let rect = ScreenRect::new(self.window_to_viewport_px(start_px), self.window_to_viewport_px(end_px));
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let slab = self.section_slab();
        let mut hits = Vec::new();
        let mut seen = HashSet::new();

        for group in self.pick_geometry_groups() {
            for record in group.records {
                if frozen.contains(&record.entity) {
                    continue;
                }
                if pick_record_touches_rect(&group, record, self.scene_origin, slab, &view_proj, screen, rect) && seen.insert(record.entity) {
                    hits.push(record.entity);
                }
            }
        }

        for record in &self.text_pick_records {
            if frozen.contains(&record.entity) {
                continue;
            }
            let Some(polygon) = visible_text_polygon(record, slab, &view_proj, screen) else {
                continue;
            };
            if polygon_touches_rect(&polygon, rect) && seen.insert(record.entity) {
                hits.push(record.entity);
            }
        }

        hits
    }

    /// The individual drill holes a selection rectangle takes.
    ///
    /// The same left-to-right / right-to-left convention the design box
    /// selection uses: `cross_select` takes a hole whose trace touches the
    /// box at all, and a window select takes only holes drawn wholly inside
    /// it. Holes are tested by their projected trace, so a hole standing
    /// behind the camera contributes nothing rather than wrapping around.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn drill_holes_in_screen_rect(
        &self,
        drill_holes: &[OpenDrillHoleDataset],
        start_px: (f32, f32),
        end_px: (f32, f32),
        cross_select: bool,
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
    ) -> Vec<DrillHoleRef> {
        let rect = ScreenRect::new(self.window_to_viewport_px(start_px), self.window_to_viewport_px(end_px));
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let slab = self.section_slab();
        let mut hits = Vec::new();

        for dataset in drill_holes.iter().filter(|dataset| dataset.state.loaded) {
            let entity = dataset.entity_id();
            if hidden.contains(&entity) || frozen.contains(&entity) {
                continue;
            }
            for (index, hole) in dataset.dataset.holes.iter().enumerate() {
                // Only the trace length between the walls is drawn, so each leg is cut at the walls (not stations): a hole passing through between two stations is still selectable there.
                let legs: Vec<Option<(DVec2, DVec2)>> = hole_legs(hole)
                    .filter_map(|(from, to)| slab_clipped_segment(slab, from, to))
                    .map(|(from, to)| {
                        match (
                            crate::rendering::pick::world_to_screen(&view_proj, from, screen),
                            crate::rendering::pick::world_to_screen(&view_proj, to, screen),
                        ) {
                            (Some(from), Some(to)) => Some((from, to)),
                            _ => None,
                        }
                    })
                    .collect();
                let taken = if cross_select {
                    legs.iter()
                        .flatten()
                        .any(|&(from, to)| rect.contains(from) || rect.contains(to) || segment_intersects_rect(from, to, rect.min_x, rect.max_x, rect.min_y, rect.max_y))
                } else {
                    !legs.is_empty() && legs.iter().all(|leg| leg.is_some_and(|(from, to)| rect.contains(from) && rect.contains(to)))
                };
                if taken {
                    hits.push(DrillHoleRef { dataset: dataset.id, hole: index });
                }
            }
        }

        hits
    }

    /// The tie-in connectors a Drill & Blast selection rectangle takes.
    ///
    /// Crossing selection accepts a connector that touches the box; window
    /// selection requires both ends, and therefore the whole straight
    /// connector, to be inside it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tie_ins_in_screen_rect(
        &self,
        drill_holes: &[OpenDrillHoleDataset],
        start_px: (f32, f32),
        end_px: (f32, f32),
        cross_select: bool,
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
    ) -> Vec<TieInRef> {
        let rect = ScreenRect::new(self.window_to_viewport_px(start_px), self.window_to_viewport_px(end_px));
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let slab = self.section_slab();
        let mut hits = Vec::new();

        for dataset in drill_holes.iter().filter(|dataset| dataset.state.loaded) {
            let entity = dataset.entity_id();
            if hidden.contains(&entity) || frozen.contains(&entity) {
                continue;
            }
            for tie in &dataset.dataset.ties {
                let (Some(from), Some(to)) = (dataset.dataset.holes.get(tie.from), dataset.dataset.holes.get(tie.to)) else {
                    continue;
                };
                let (start, end) = (from.collar_position(), to.collar_position());
                let Some((a, b)) = slab_clipped_screen_segment(slab, &view_proj, screen, start, end) else {
                    continue;
                };
                let taken = if cross_select {
                    segment_intersects_rect(a, b, rect.min_x, rect.max_x, rect.min_y, rect.max_y)
                } else {
                    rect.contains(a) && rect.contains(b)
                };
                if taken {
                    hits.push(TieInRef::new(dataset.id, tie.from, tie.to));
                }
            }
        }

        hits
    }

    /// Anchors a plan orbit on the fixed centre, else on the pivot a C pick
    /// would take. The section orbits by a separate path.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_orbit_at_surface(
        &mut self,
        triangulations: &[OpenTriangulation],
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        document: &Document,
        snap_index: &crate::model::spatial::ObjectSnapIndex,
        working_plane_z: f64,
        rotation_centre: Option<DVec3>,
    ) {
        let pt = rotation_centre.unwrap_or_else(|| self.plan_pivot_near_cursor(triangulations, drill_holes, hidden, frozen, document, snap_index, working_plane_z));
        self.camera.sync_angles_from_forward();
        self.camera_controller.begin_orbit(self.exaggerate_point(pt));
        self.orbit_marker = rotation_centre.is_none().then_some(pt);
    }

    /// Asset under the cursor, else object, working plane, or eye depth.
    fn orbit_point_under_cursor(
        &self,
        triangulations: &[OpenTriangulation],
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        working_plane_z: f64,
    ) -> DVec3 {
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        {
            let (ray_origin, direction) = self.cursor_model_ray();
            let triangulation_hit = SceneQuery::nearest_surface(triangulations, hidden, Some(frozen), ray_origin, direction).map(|(_, world)| world);
            let drill_hole_hit =
                SceneQuery::nearest_drill_hole(drill_holes, hidden, frozen, ray_origin, direction, self.camera.forward(), &view_proj, screen, 0.0).map(|(_, world)| world);
            let block_model_hit = self.block_model_gpu.nearest_visible_hit(ray_origin, direction, hidden, frozen);
            // A point cloud has no ray-castable surface, so pivot on the nearest
            // splat under the cursor instead - otherwise orbiting over a selected
            // cloud spins about a stale camera-target depth.
            let point_cloud_hit = self
                .point_cloud_gpu
                .nearest_visible_entity_at_screen(
                    &view_proj,
                    screen,
                    DVec2::new(f64::from(self.camera_controller.mouse_loc.0), f64::from(self.camera_controller.mouse_loc.1)),
                    ROTATION_CENTRE_PICK_PX,
                    hidden,
                    frozen,
                    self.section_slab(),
                )
                .map(|(_, world)| world);
            triangulation_hit
                .into_iter()
                .chain(drill_hole_hit)
                .chain(block_model_hit)
                .chain(point_cloud_hit)
                .min_by(|a, b| (*a - ray_origin).dot(direction).total_cmp(&(*b - ray_origin).dot(direction)))
                .unwrap_or_else(|| {
                    // No asset surface hit - try picking any document object
                    // near the cursor, then the working plane under it, then
                    // the camera-target depth if the view can't meet the plane.
                    self.pick_at_cursor(ROTATION_CENTRE_PICK_PX, triangulations, hidden, frozen, false)
                        .map(|(_, world)| world)
                        .or_else(|| {
                            // Reject a working-plane hit behind the viewer (a tilted view can put the plane there).
                            self.cursor_world(working_plane_z).filter(|point| self.in_front_of_eye(*point))
                        })
                        .unwrap_or_else(|| self.unexaggerate_point(self.cursor_world_at_target_depth()))
                })
        }
    }

    /// Nearest string or trace within reach, else section plane or plan pivot.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pick_rotation_centre(
        &self,
        triangulations: &[OpenTriangulation],
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        document: &Document,
        snap_index: &crate::model::spatial::ObjectSnapIndex,
        working_plane_z: f64,
    ) -> Option<DVec3> {
        if self.slice_view.is_some() {
            return self
                .string_or_trace_near_cursor(drill_holes, hidden, frozen, document, snap_index)
                .or_else(|| self.section_point_at_px(self.camera_controller.mouse_loc));
        }
        Some(self.plan_pivot_near_cursor(triangulations, drill_holes, hidden, frozen, document, snap_index, working_plane_z))
    }

    /// Nearest string or trace point within reach, since the eye aims at the
    /// line and not the surface above it, else the surface under the cursor.
    #[allow(clippy::too_many_arguments)]
    fn plan_pivot_near_cursor(
        &self,
        triangulations: &[OpenTriangulation],
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        document: &Document,
        snap_index: &crate::model::spatial::ObjectSnapIndex,
        working_plane_z: f64,
    ) -> DVec3 {
        self.string_or_trace_near_cursor(drill_holes, hidden, frozen, document, snap_index)
            .unwrap_or_else(|| self.orbit_point_under_cursor(triangulations, drill_holes, hidden, frozen, working_plane_z))
    }

    /// The nearer of the closest string and trace points within reach.
    fn string_or_trace_near_cursor(
        &self,
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        document: &Document,
        snap_index: &crate::model::spatial::ObjectSnapIndex,
    ) -> Option<DVec3> {
        let string = self.string_point_near_cursor(document, snap_index, hidden, frozen, ROTATION_CENTRE_PICK_PX);
        let trace = self.trace_point_near_cursor(drill_holes, hidden, frozen, ROTATION_CENTRE_PICK_PX);
        [string, trace].into_iter().flatten().min_by(|a, b| a.1.total_cmp(&b.1)).map(|(point, _)| point)
    }

    /// Screen position of a fixed centre; `None` behind a perspective eye.
    pub(crate) fn rotation_centre_screen_pos(&self, centre: DVec3) -> Option<(f32, f32)> {
        if self.projection.is_perspective() && !self.in_front_of_eye(centre) {
            return None;
        }
        self.world_to_window_px_unclipped_depth(&self.view_proj(), centre)
    }

    /// Whether a true world point lies ahead of the eye, not behind it.
    fn in_front_of_eye(&self, world: DVec3) -> bool {
        (self.exaggerate_point(world) - self.camera.position).dot(self.camera.forward()) > 0.0
    }

    /// The nearest of `segments` to the cursor on screen, clipped to the slab
    /// when one is up, within `threshold_px`; a lone point is a zero-length one.
    fn nearest_on_screen(
        &self,
        view_proj: &DMat4,
        cursor: DVec2,
        slab: Option<crate::rendering::camera::SectionSlab>,
        threshold_px: f32,
        segments: impl Iterator<Item = (DVec3, DVec3)>,
        best: &mut Option<(DVec3, f64)>,
    ) {
        use crate::rendering::pick::{closest_t_on_segment, perspective_correct_segment_point, slab_clipped_segment, world_to_screen_unclipped_depth};
        let screen = self.screen_size();
        let perspective = self.projection.is_perspective();
        let mut best_distance = best.map_or(f64::from(threshold_px).powi(2), |(_, distance)| distance);
        for (a, b) in segments {
            // The slab's normal is horizontal, so it clips true world points as it
            // clips displayed ones.
            let Some((a, b)) = slab_clipped_segment(slab, a, b) else {
                continue;
            };
            // Unclipped depth: what is drawn can be picked from either side of the
            // plane; only a perspective eye has a behind, where a projection would mirror.
            if perspective && !(self.in_front_of_eye(a) && self.in_front_of_eye(b)) {
                continue;
            }
            let (Some(sa), Some(sb)) = (world_to_screen_unclipped_depth(view_proj, a, screen), world_to_screen_unclipped_depth(view_proj, b, screen)) else {
                continue;
            };
            let t = closest_t_on_segment(cursor, sa, sb);
            let distance = (sa + (sb - sa) * t).distance_squared(cursor);
            if distance < best_distance {
                best_distance = distance;
                *best = Some((perspective_correct_segment_point(view_proj, a, b, t), distance));
            }
        }
    }

    /// The string point nearest the cursor on screen within `threshold_px`,
    /// vertex or body alike, arcs included, with its squared screen distance.
    fn string_point_near_cursor(
        &self,
        document: &Document,
        snap_index: &crate::model::spatial::ObjectSnapIndex,
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        threshold_px: f32,
    ) -> Option<(DVec3, f64)> {
        let view_proj = self.view_proj();
        let cursor = DVec2::new(f64::from(self.camera_controller.mouse_loc.0), f64::from(self.camera_controller.mouse_loc.1));
        let slab = self.section_slab();
        let mut best = None;
        let mut arc = Vec::new();
        for index in snap_index.candidates(&view_proj, self.screen_size(), cursor, f64::from(threshold_px)) {
            let object = &document.objects()[index];
            let entity = SceneEntityId::Object(object.id());
            if hidden.contains(&entity) || frozen.contains(&entity) || !document.layer(object.layer()).is_none_or(|layer| layer.loaded) {
                continue;
            }
            match object {
                crate::model::Object::Point { pos, .. } => self.nearest_on_screen(&view_proj, cursor, slab, threshold_px, std::iter::once((*pos, *pos)), &mut best),
                crate::model::Object::Polyline { verts, closed, .. } => {
                    let count = verts.len();
                    let segments = if *closed { count } else { count.saturating_sub(1) };
                    for k in 0..segments {
                        let (a, b, bulge) = (verts[k].pos, verts[(k + 1) % count].pos, verts[k].bulge);
                        if bulge.abs() <= f64::EPSILON {
                            self.nearest_on_screen(&view_proj, cursor, slab, threshold_px, std::iter::once((a, b)), &mut best);
                        } else {
                            arc.clear();
                            arc.extend(crate::model::geometry::tessellate_bulge_segment(a, b, bulge));
                            self.nearest_on_screen(&view_proj, cursor, slab, threshold_px, arc.windows(2).map(|pair| (pair[0], pair[1])), &mut best);
                        }
                    }
                }
                _ => {}
            }
        }
        best
    }

    /// The drill-trace point nearest the cursor on screen within `threshold_px`,
    /// drawn intervals only, measured as a string is so the two feel alike.
    fn trace_point_near_cursor(
        &self,
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        threshold_px: f32,
    ) -> Option<(DVec3, f64)> {
        let view_proj = self.view_proj();
        let cursor = DVec2::new(f64::from(self.camera_controller.mouse_loc.0), f64::from(self.camera_controller.mouse_loc.1));
        let slab = self.section_slab();
        let mut best = None;
        for dataset in drill_holes.iter().filter(|dataset| dataset.state.loaded) {
            let entity = dataset.entity_id();
            if hidden.contains(&entity) || frozen.contains(&entity) {
                continue;
            }
            for hole in &dataset.dataset.holes {
                let drawn =
                    |from: f64, to: f64| hole.render_ranges.is_empty() || hole.render_ranges.iter().any(|(start, end)| *start <= (from + to) * 0.5 && (from + to) * 0.5 < *end);
                let legs = hole
                    .trace
                    .windows(2)
                    .filter(|pair| drawn(pair[0].depth, pair[1].depth))
                    .map(|pair| (pair[0].position, pair[1].position));
                let collar = (hole.trace.len() == 1).then(|| (hole.collar_position(), hole.collar_position()));
                self.nearest_on_screen(&view_proj, cursor, slab, threshold_px, legs.chain(collar), &mut best);
            }
        }
        best
    }

    /// Find the nearest snap target for the current cursor position.
    /// Returns `None` in `CursorMode::Select` or when nothing is within the snap threshold.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn snap_cursor(
        &self,
        document: &Document,
        snap_index: &crate::model::spatial::ObjectSnapIndex,
        triangulations: &[OpenTriangulation],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        mode: &CursorMode,
        xray_enabled: bool,
    ) -> Option<DVec3> {
        if *mode == CursorMode::Select {
            return None;
        }
        if self.slice_view.is_some() {
            // A section snaps to strings only, inside its slab, whatever the mode.
            return self
                .string_point_near_cursor(document, snap_index, hidden, frozen, SNAP_THRESHOLD_PX)
                .map(|(point, _)| point);
        }
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let candidate = SceneQuery::snap(
            document,
            snap_index,
            triangulations,
            hidden,
            frozen,
            mode,
            &view_proj,
            screen,
            self.camera_controller.mouse_loc,
            SNAP_THRESHOLD_PX,
            xray_enabled,
        )?;
        if !xray_enabled && self.nonselectable_asset_occludes(candidate, hidden, &view_proj, screen) {
            None
        } else {
            Some(candidate)
        }
    }

    /// Reset the camera to a top-down plan view that fits all visible content.
    /// Falls back to a default unit zoom centred on the origin when empty.
    pub(crate) fn fit_to_extents(
        &mut self,
        document: &Document,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        hidden: &HashSet<SceneEntityId>,
    ) {
        let screen = self.screen_size();
        let aspect = (screen.0 as f64 / screen.1.max(1.0) as f64).max(1e-9);

        let bounds = scene_bounds(document, triangulations, block_models, drill_holes, point_clouds, hidden);
        let (center, zoom) = match bounds {
            Some((min, max)) => {
                let center = (min + max) * 0.5;
                let size = max - min;
                // Degenerate (single point or all collinear on one axis): unit zoom.
                if size.length() < 1e-6 {
                    (center, default_zoom(aspect))
                } else {
                    // Plan view: right == X, up == Y.
                    let zoom_h = size.y / 2.0;
                    let zoom_w = size.x / (2.0 * aspect);
                    // 10 % padding so content never touches the viewport edge.
                    (center, zoom_h.max(zoom_w) * 1.1)
                }
            }
            None => (DVec3::ZERO, default_zoom(aspect)),
        };

        let zoom = zoom.max(1e-4);
        self.projection.zoom = zoom;
        let camera_distance = if self.fly_mode_enabled {
            zoom / (self.projection.perspective_fov_y() * 0.5).tan()
        } else {
            zoom
        };
        self.camera.reset_to_plan_view(center, camera_distance);
        // Nothing to frame means the origin is all there is to look at, so it
        // belongs where the startup view puts it, not at the centre of the
        // scene region - see `track_startup_view_framing`.
        if bounds.is_none() {
            self.translate_view_by_pixels(self.window_centring_offset_px());
        }
        self.scene_origin = center;
        self.triangulation_gpu.clear();
        self.block_model_gpu.clear();
        self.drill_hole_gpu = Default::default();
        self.geometry_dirty = true;
        // Update znear/zfar immediately so snap/pick work before the first render.
        self.fit_depth_to_scene(document, triangulations, block_models, drill_holes, point_clouds, hidden);
    }

    /// World-space bounds of everything currently visible, for callers that
    /// need to frame the scene without moving the camera (plot sheets).
    pub(crate) fn scene_extents(
        &self,
        document: &Document,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        hidden: &HashSet<SceneEntityId>,
    ) -> Option<(DVec3, DVec3)> {
        scene_bounds(document, triangulations, block_models, drill_holes, point_clouds, hidden)
    }

    /// The point the viewport is currently looking at, in world (not
    /// vertically exaggerated) coordinates.
    pub(crate) fn view_center(&self) -> DVec3 {
        self.unexaggerate_point(self.camera.target())
    }

    /// Frame all visible content while keeping the current camera orientation
    /// (orbit/tilt unchanged) - only position, target and zoom are adjusted.
    /// No-op when there is nothing visible to frame.
    pub(crate) fn zoom_to_extents(
        &mut self,
        document: &Document,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        hidden: &HashSet<SceneEntityId>,
    ) {
        let Some((min, max)) = scene_bounds(document, triangulations, block_models, drill_holes, point_clouds, hidden) else {
            return;
        };
        if self.slice_view.is_some() {
            self.zoom_slice_to_extents(min, max);
            return;
        }
        let center = (min + max) * 0.5;
        let forward = self.camera.forward();
        let right = forward.cross(self.camera.up()).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();

        // Project the eight bounding-box corners onto the view's right/up axes
        // to find the half-extents the ortho frustum must cover at this angle.
        let mut half_w = 0.0_f64;
        let mut half_h = 0.0_f64;
        for i in 0..8 {
            let corner = DVec3::new(
                if (i & 1) == 0 { min.x } else { max.x },
                if (i & 2) == 0 { min.y } else { max.y },
                if (i & 4) == 0 { min.z } else { max.z },
            );
            let mut d = corner - center;
            d.z *= self.vertical_exaggeration;
            half_w = half_w.max(d.dot(right).abs());
            half_h = half_h.max(d.dot(up).abs());
        }

        let screen = self.screen_size();
        let aspect = (screen.0 as f64 / screen.1.max(1.0) as f64).max(1e-9);
        let zoom = if half_w <= 1e-9 && half_h <= 1e-9 {
            default_zoom(aspect)
        } else {
            // 10 % padding, matching fit_to_extents.
            (half_h.max(half_w / aspect) * 1.1).max(1e-4)
        };

        self.projection.zoom = zoom;
        let camera_distance = if self.projection.is_perspective() {
            zoom / (self.projection.perspective_fov_y() * 0.5).tan()
        } else {
            zoom
        };
        self.camera.frame_keep_orientation(center, camera_distance);
        self.scene_origin = center;
        self.triangulation_gpu.clear();
        self.block_model_gpu.clear();
        self.drill_hole_gpu = Default::default();
        self.geometry_dirty = true;
        // Update znear/zfar immediately so snap/pick work before the first render.
        self.fit_depth_to_scene(document, triangulations, block_models, drill_holes, point_clouds, hidden);
    }

    /// Zoom to extents within a section: the zoom and the in-plane anchor
    /// move, the section itself does not. Its direction and where the slab
    /// sits along its normal are what the view is *of*, so framing must not
    /// quietly cut somewhere else.
    ///
    /// Bounds arrive in model elevations and the section works in displayed
    /// ones, so they are stretched by the vertical exaggeration first.
    fn zoom_slice_to_extents(&mut self, min: DVec3, max: DVec3) {
        let screen = self.screen_size();
        let aspect = (screen.0 as f64 / screen.1.max(1.0) as f64).max(1e-9);
        let corners = std::array::from_fn::<DVec3, 8, _>(|i| {
            self.exaggerate_point(DVec3::new(
                if (i & 1) == 0 { min.x } else { max.x },
                if (i & 2) == 0 { min.y } else { max.y },
                if (i & 4) == 0 { min.z } else { max.z },
            ))
        });
        let target = self.exaggerate_point((min + max) * 0.5);
        let Some(slice) = self.slice_view.as_mut() else {
            return;
        };
        let (forward, right, up) = slice.camera_basis();

        // The section cuts the scene box at whatever angle it runs, so measure the corners along the view's own axes rather than the box's.
        let (mut half_width, mut half_height) = (0.0_f64, 0.0_f64);
        for corner in corners {
            let offset = corner - target;
            half_width = half_width.max(offset.dot(right).abs());
            half_height = half_height.max(offset.dot(up).abs());
        }
        self.projection.zoom = if half_width <= 1e-9 && half_height <= 1e-9 {
            default_zoom(aspect)
        } else {
            // 10 % padding, matching the plan view's fit.
            (half_height.max(half_width / aspect) * 1.1).max(1e-4)
        };

        // Slide the anchor within the plane until the scene's centre is the screen's; ortho, so the eye's depth off the plane doesn't enter into it.
        let offset = target - slice.camera_position();
        if let Some(shift) = in_plane_screen_move(offset.dot(right), offset.dot(up), right, up, slice.normal()) {
            slice.center += shift;
        }
        self.camera.look_to(slice.camera_position(), forward, up, self.projection.zoom.max(1.0));
    }

    pub(crate) fn set_standard_view(&mut self, view: crate::ui::state::StandardView) {
        let (forward, up) = standard_view_basis(view);
        self.camera_controller.begin_view_transition(&self.camera, forward, up, self.projection.zoom);
        self.camera_controller.end_orbit();
        self.orbit_marker = None;
    }

    /// Keep the depth range tight around the current scene. An oversized range
    /// loses enough precision that back-side mesh edges can compare equal to
    /// the front surface and bleed through it, especially in perspective.
    /// Bring the cached scene bounds up to date without fitting to them; used by the depth fit below, and by a section, which reads bounds but sets its own clip range.
    pub(super) fn refresh_scene_bounds(
        &mut self,
        document: &Document,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        hidden: &HashSet<SceneEntityId>,
    ) {
        if self.geometry_dirty || self.cached_bounds_document_revision != document.revision() {
            self.cached_object_aabbs = visible_object_aabbs(document, triangulations, block_models, drill_holes, point_clouds, hidden);
            self.cached_scene_bounds = merge_aabbs(&self.cached_object_aabbs);
            self.cached_bounds_document_revision = document.revision();
        }
    }

    pub(super) fn fit_depth_to_scene(
        &mut self,
        document: &Document,
        triangulations: &[OpenTriangulation],
        block_models: &[OpenBlockModel],
        drill_holes: &[OpenDrillHoleDataset],
        point_clouds: &[OpenPointCloud],
        hidden: &HashSet<SceneEntityId>,
    ) {
        self.refresh_scene_bounds(document, triangulations, block_models, drill_holes, point_clouds, hidden);
        let Some((scene_min, scene_max)) = self.cached_scene_bounds else {
            let depth = (self.projection.zoom * 4.0).max(10.0);
            self.projection.set_view_depth_range(0.0, depth, 1.0);
            return;
        };

        // Fit the clip range to geometry that is actually within the camera's
        // field of view. A model that sits far off to the side (a second block
        // model loaded thousands of metres away) must not stretch the range:
        // in an angled view its depth along the forward axis is enormous, and
        // the resulting near/far span destroys depth precision - the volume
        // raycaster then reconstructs sample points from a ray origin millions
        // of units away and floating-point cancellation fills the visible model
        // with black speckle. The lateral frustum planes do not depend on the
        // near/far we are about to compute, so this is not circular.
        // Build the frustum from the scene-origin-relative view-projection the
        // GPU uses (small coordinates → good f32 precision), and test the same
        // scene-relative AABBs. Refresh from the live camera: slice previews
        // change their camera before fitting and may cache the resulting frame,
        // so using their previous uniform can leave visible objects clipped.
        // Only the lateral planes are read, so the old near/far do not matter.
        self.camera_uniform
            .update_view_proj(&self.camera, &self.projection, self.scene_origin, self.vertical_exaggeration);
        let frustum = Frustum::from_view_proj(glam::Mat4::from_cols_array_2d(&self.camera_uniform.view_proj));
        let scene_origin = self.scene_origin;
        let mut min_depth = f64::INFINITY;
        let mut max_depth = f64::NEG_INFINITY;
        for &(object_min, object_max) in &self.cached_object_aabbs {
            let lateral_in_view = frustum.intersects_aabb_lateral((object_min - scene_origin).as_vec3(), (object_max - scene_origin).as_vec3());
            if lateral_in_view {
                let (object_near, object_far) = self.aabb_depth_range(object_min, object_max);
                min_depth = min_depth.min(object_near);
                max_depth = max_depth.max(object_far);
            }
        }
        // Nothing is in view sideways (camera turned fully away, or bounds we
        // could not classify): fall back to the whole scene so the range is
        // never left empty.
        if min_depth > max_depth {
            let (scene_near, scene_far) = self.aabb_depth_range(scene_min, scene_max);
            min_depth = scene_near;
            max_depth = scene_far;
        }

        let padding = (self.projection.zoom * 0.25).max(1.0);
        self.projection.set_view_depth_range(min_depth, max_depth, padding);
    }

    /// Depth range `(min, max)` of a world-space AABB measured along the view
    /// forward axis, honouring the vertical exaggeration applied to rendered
    /// geometry.
    fn aabb_depth_range(&self, min: DVec3, max: DVec3) -> (f64, f64) {
        let forward = self.camera.forward();
        let mut min_depth = f64::INFINITY;
        let mut max_depth = f64::NEG_INFINITY;
        for i in 0..8 {
            let corner = DVec3::new(
                if (i & 1) == 0 { min.x } else { max.x },
                if (i & 2) == 0 { min.y } else { max.y },
                if (i & 4) == 0 { min.z } else { max.z },
            );
            let depth = (self.exaggerate_point(corner) - self.camera.position).dot(forward);
            min_depth = min_depth.min(depth);
            max_depth = max_depth.max(depth);
        }
        (min_depth, max_depth)
    }

    /// Keep drawing, slice-line and measurement previews inside the clip volume
    /// even when their endpoints lie outside the committed scene.
    pub(super) fn include_tool_previews_in_depth(&mut self, editor: &EditorState) {
        let forward = self.camera.forward();
        let depth_from_camera = |point: DVec3| {
            let point = self.exaggerate_point(point);
            (point - self.camera.position).dot(forward)
        };
        let mut min_depth = f64::INFINITY;
        let mut max_depth = f64::NEG_INFINITY;
        for depth in editor
            .pending_stroke
            .iter()
            .copied()
            .filter(|point| point.x.is_finite() && point.y.is_finite() && point.z.is_finite())
            .map(depth_from_camera)
        {
            min_depth = min_depth.min(depth);
            max_depth = max_depth.max(depth);
        }

        if !editor.pending_stroke.is_empty()
            && !editor.poly_finish_dialog
            && let Some(cursor) = editor.cursor_world
            && cursor.x.is_finite()
            && cursor.y.is_finite()
            && cursor.z.is_finite()
        {
            let depth = depth_from_camera(cursor);
            min_depth = min_depth.min(depth);
            max_depth = max_depth.max(depth);
        }

        if let Some(draft) = editor.circle_draft.as_ref()
            && let Some(radius) = draft.preview_radius(editor.cursor_world)
        {
            let plan_forward = forward.truncate();
            let plan_direction = if plan_forward.length_squared() > f64::EPSILON {
                plan_forward.normalize()
            } else {
                DVec2::X
            };
            let offset = DVec3::new(plan_direction.x * radius, plan_direction.y * radius, 0.0);
            for depth in [draft.center - offset, draft.center + offset].map(depth_from_camera) {
                min_depth = min_depth.min(depth);
                max_depth = max_depth.max(depth);
            }
        }

        let mut include = |point: DVec3| {
            if point.is_finite() {
                let depth = depth_from_camera(point);
                min_depth = min_depth.min(depth);
                max_depth = max_depth.max(depth);
            }
        };
        // Tie overlays are emitted directly from their stored preview state.
        if let Some(anchor) = editor.tie_anchor_world {
            include(anchor);
            if let Some(end) = editor.tie_path_end_world {
                include(end);
            }
        }
        for leg in &editor.tie_preview {
            include(leg.start);
            include(leg.end);
        }
        match editor.active_tool {
            ActiveTool::MakeCircle => {
                // The centre cross is visible even before a radius is available.
                if let Some(draft) = &editor.circle_draft {
                    include(draft.center);
                }
            }
            ActiveTool::VerticalSlice => {
                if let Some(start) = editor.slice_pending_start {
                    include(start);
                    if let Some(cursor) = editor.cursor_world {
                        // Match the flat XY preview drawn in scene::overlays.
                        include(DVec3::new(cursor.x, cursor.y, start.z));
                    }
                }
            }
            ActiveTool::MeasureDistance => {
                if let Some(start) = editor.measurement_start {
                    include(start);
                    if let Some(end) = editor.measurement_end.or(editor.cursor_world) {
                        include(end);
                    }
                }
            }
            ActiveTool::MeasureBatterAngle => {
                let mut points = editor.batter_angle_points.clone();
                if points.len() < 3
                    && let Some(cursor) = editor.cursor_world
                {
                    points.push(cursor);
                }
                for &point in &points {
                    include(point);
                }
                if let Some(measurement) = crate::ui::state::batter_angle_measurement(&points) {
                    include(measurement.projection);
                }
            }
            _ => {}
        }

        if min_depth.is_finite() {
            let padding = (self.projection.zoom * 0.25).max(1.0);
            self.projection.expand_view_depth_range(min_depth, max_depth, padding);
        }
    }

    /// Keep generated batter/berm preview rings inside the clip volume. They
    /// are not committed document geometry yet, so scene bounds omit them.
    pub(super) fn include_batter_berm_preview_in_depth(&mut self, editor: &EditorState) {
        if !editor.batter_berm_dialog_open || editor.batter_berm_rings_world.is_empty() {
            return;
        }

        let forward = self.camera.forward();
        let (min_depth, max_depth) = editor
            .batter_berm_rings_world
            .iter()
            .flatten()
            .copied()
            .filter(|point| point.x.is_finite() && point.y.is_finite() && point.z.is_finite())
            .map(|point| {
                let point = self.exaggerate_point(point);
                (point - self.camera.position).dot(forward)
            })
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), depth| (min.min(depth), max.max(depth)));

        if min_depth.is_finite() {
            let padding = (self.projection.zoom * 0.25).max(1.0);
            self.projection.expand_view_depth_range(min_depth, max_depth, padding);
        }
    }

    /// Write the camera uniform the fragment shaders read. `slab` is the section clip (or `None`); it's an argument because the plot renders the whole ground through this buffer while a section is up in the viewport.
    pub(super) fn upload_camera_uniform(&mut self, interaction_resolution_divisor: u32, slab: Option<SectionSlab>) {
        self.camera_uniform
            .update_view_proj(&self.camera, &self.projection, self.scene_origin, self.vertical_exaggeration);
        self.camera_uniform.set_section_slab(slab, self.scene_origin);
        // While the camera is being orbited/panned/zoomed, draw the volume
        // raycast at a reduced per-axis resolution and upscale. The reduced
        // resolution also coarsens the raycaster's footprint-driven brick LOD
        // implicitly and arms its in-shader step cap, so the explicit LOD boost
        // stays at 1.0. See `CameraUniform::set_interaction_quality` and the
        // volume upscale pass.
        const MOTION_LOD_BOOST: f32 = 1.0;
        let motion_render_scale = 1.0 / interaction_resolution_divisor.clamp(1, 64) as f32;
        if self.is_camera_active() || self.needs_continuous_redraw() {
            self.mark_interaction();
        }
        if self.interaction_active() {
            self.camera_uniform.set_interaction_quality(MOTION_LOD_BOOST, motion_render_scale);
        } else {
            self.camera_uniform.set_interaction_quality(1.0, 1.0);
        }
        self.queue.write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[self.camera_uniform]));
    }

    /// Slice-mode per-frame camera update: integrate held W/S/A/D movement
    /// and Q/E rotation, consume accumulated pan/scroll deltas, then derive
    /// the camera from the slice state (which stays the single source of
    /// truth).
    fn update_slice_camera(&mut self, dt: Duration, rotation_centre: Option<DVec3>) {
        let screen = self.screen_size();
        let mouse_loc = self.camera_controller.mouse_loc;
        let fixed_centre = rotation_centre.map(|centre| self.exaggerate_point(centre));
        let Some(slice) = self.slice_view.as_mut() else {
            return;
        };
        let pending = slice.has_pending_updates();
        // Matches the zoom feel of the main `CameraController` (see init.rs).
        const SLICE_ZOOM_SENSITIVITY: f64 = 0.005;
        const SLICE_WALK_SECONDS_PER_NOTCH: f64 = 0.25;
        const WHEEL_NOTCH_PIXELS: f64 = 100.0;
        let step = dt.as_secs_f64().min(0.1);

        // Q/E: rotate the slice line about its centre. Q turns the view left
        // (counter-clockwise seen from above), E right.
        let rotate_amount = f64::from(i8::from(slice.input.rotate_left)) - f64::from(i8::from(slice.input.rotate_right));
        if rotate_amount != 0.0 {
            // Turn about the fixed centre when set, else the anchor; the eye rides along.
            slice.turn(rotate_amount * slice.rotate_speed * step, fixed_centre);
        }

        // A gizmo click eases the section round to a standard view over the same moment the plan view's camera takes, in the same two parts a click can ask for: the line turns, and the orbit unwinds.
        let turning = slice.advance_turn(dt);
        if let Some((turn_step, _, _)) = turning {
            slice.turn(turn_step, fixed_centre);
        }

        // Right-drag orbits only the eye, so the plane being drawn on never moves under a stroke (Q/E turn the section itself); settled before the walk/pan/zoom below since those act on this orientation.
        if !slice.orbit_dragging || self.mouse_pressed != Some(MouseButton::Right) {
            slice.orbit_dragging = false;
        }
        let anchored = fixed_centre
            .filter(|_| slice.orbit != DVec2::ZERO || turning.is_some())
            .map(|centre| (centre, eye_offset_from(centre, slice.camera_position(), slice.camera_basis())));
        if let Some((_, yaw, pitch)) = turning {
            slice.yaw = yaw;
            slice.pitch = pitch;
        }
        if slice.orbit != DVec2::ZERO {
            let angles = self.camera_controller.orbit_angles(slice.orbit.x, slice.orbit.y);
            // Matches the plan view's sign: it rotates the camera position by the negated horizontal angle, so forward turns by its negative.
            slice.yaw -= angles.x;
            // Clamped to within `MIN_SECTION_INCIDENCE` of straight up/down so the view never grazes the section, where no pixel has a point on it and every placement tool goes quiet with no visible cause.
            let pitch_limit = std::f64::consts::FRAC_PI_2 - crate::rendering::camera::MIN_SECTION_INCIDENCE;
            slice.pitch = (slice.pitch + angles.y).clamp(-pitch_limit, pitch_limit);
            slice.orbit = DVec2::ZERO;
        }

        let normal = slice.normal();
        let (forward, right, up) = slice.camera_basis();
        slice.update_viewing_side(forward);
        if let Some((centre, (screen_x, screen_y, depth))) = anchored {
            // The eye turns about the centre; the anchor takes its foot below.
            let eye = eye_keeping_centre(centre, screen_x, screen_y, depth, (forward, right, up));
            slice.view_offset = eye - slice.center;
        }

        // W/S and Shift+wheel both walk the slab along its normal, in the same metres, added together and applied once; forward is away from the eye, not `+normal`.
        let held_seconds = (f64::from(i8::from(slice.input.forward)) - f64::from(i8::from(slice.input.backward))) * step;
        let walked_seconds = slice.walk / WHEEL_NOTCH_PIXELS * SLICE_WALK_SECONDS_PER_NOTCH;
        slice.walk = 0.0;
        if held_seconds + walked_seconds != 0.0 {
            slice.center += super::walk_direction(normal, slice.viewing_from_front) * (held_seconds + walked_seconds) * slice.move_speed;
        }

        // Pan and zoom move the section anchor within its plane by the move
        // that tracks the hand on screen.
        if slice.pan != DVec2::ZERO {
            let world_per_pixel = (2.0 * self.projection.zoom / screen.1.max(1.0) as f64).max(0.0);
            if let Some(shift) = in_plane_screen_move(slice.pan.x * world_per_pixel, slice.pan.y * world_per_pixel, right, up, normal) {
                slice.center += shift;
            }
            slice.pan = DVec2::ZERO;
        }

        // Scroll: ortho zoom toward the cursor, along the same axes as the pan.
        if slice.scroll != 0.0 {
            let zoom_scale = if slice.scroll > 0.0 {
                1.0 - (1.0 / (1.0 + SLICE_ZOOM_SENSITIVITY * slice.scroll))
            } else {
                SLICE_ZOOM_SENSITIVITY * slice.scroll
            };
            let zoom_factor = self.projection.zoom * zoom_scale;
            let mouse_ndc = crate::rendering::camera::point(mouse_loc.0, mouse_loc.1, screen);
            let aspect = screen.0 as f64 / screen.1.max(1.0) as f64;
            if let Some(shift) = in_plane_screen_move(mouse_ndc.x * aspect * zoom_factor, mouse_ndc.y * zoom_factor, right, up, normal) {
                slice.center += shift;
            }
            self.projection.zoom = (self.projection.zoom - zoom_factor).max(1.0e-4);
            slice.scroll = 0.0;
        }

        // On input the eye slides onto the plane and the anchor takes its
        // foot, so the overview, depth range and Q/E follow the screen.
        if pending {
            slice.set_eye(slide_onto_plane(slice.camera_position(), slice.center, forward, normal));
        }

        // The camera sits on the section plane, so the znear/zfar range stays centred on it; the shown slab is the shader clip, not this range, so orbiting only changes the angle, not what's cut.
        self.camera.look_to(slice.camera_position(), forward, up, self.projection.zoom.max(1.0));
    }

    pub(crate) fn update(&mut self, dt: Duration, interaction_resolution_divisor: u32, rotation_centre: Option<DVec3>) {
        if self.slice_view.is_some() {
            self.update_slice_camera(dt, rotation_centre);
        } else if self.camera_controller.has_view_transition() {
            self.camera_controller.update_view_transition(&mut self.camera, &mut self.projection, dt);
        } else if self.fly_mode_enabled {
            self.fly_camera_controller.update_camera(&mut self.camera, dt);
        } else {
            let screen_size = self.screen_size();
            self.camera_controller.update_camera(&mut self.camera, &mut self.projection, dt, screen_size);
        }
        self.upload_camera_uniform(interaction_resolution_divisor, self.section_slab());
    }

    pub(crate) fn input(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::MouseWheel { delta, .. } => {
                // Zoom is applied and cleared within a single update tick, so
                // `needs_continuous_redraw` only briefly reflects it and the
                // low-quality volume path can be missed for a scroll burst.
                // Mark interaction directly so the cooldown covers the whole
                // burst (and 150 ms after), like camera drags and resizes.
                self.mark_interaction();
                if let Some(slice) = self.slice_view.as_mut() {
                    slice.scroll += super::scroll_pixels(delta);
                } else if self.fly_mode_enabled {
                    self.fly_camera_controller.process_scroll(delta);
                } else {
                    self.camera_controller.process_scroll(delta);
                }
                true
            }
            WindowEvent::MouseInput { button, state, .. } => {
                let pressing = *state == ElementState::Pressed;
                let camera_button = matches!(button, MouseButton::Left | MouseButton::Middle | MouseButton::Right);
                if pressing {
                    if *button == MouseButton::Right && !self.fly_mode_enabled {
                        // The app promotes a right press to orbit only after it
                        // moves past the context-click threshold.
                    } else if camera_button && (self.mouse_pressed.is_none() || *button == MouseButton::Right) {
                        self.mouse_pressed = Some(*button);
                    }
                    if self.fly_mode_enabled && *button == MouseButton::Right {
                        self.fly_camera_controller.begin_capture();
                    }
                } else if self.mouse_pressed == Some(*button) {
                    self.mouse_pressed = None;
                }
                if !pressing && *button == MouseButton::Right {
                    self.camera_controller.end_orbit();
                    self.fly_camera_controller.clear_input();
                    self.orbit_marker = None;
                }
                self.sync_cursor_grab();
                true
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    state,
                    physical_key: PhysicalKey::Code(key),
                    ..
                },
                ..
            } => {
                let fly_active = self.fly_mode_enabled && self.mouse_pressed == Some(MouseButton::Right);
                fly_active && self.fly_camera_controller.process_key(*key, *state == ElementState::Pressed)
            }
            _ => false,
        }
    }

    pub(super) fn cursor_world_at_target_depth(&self) -> DVec3 {
        let forward = self.camera.forward();
        let screen = self.screen_size();
        let aspect = screen.0 as f64 / screen.1.max(1.0) as f64;
        let focal_dist = (self.camera.target() - self.camera.position).dot(forward).abs();
        let offset = crate::rendering::camera::view_plane_offset(&self.camera, self.projection.zoom, aspect, screen, self.camera_controller.mouse_loc);
        self.camera.position + forward * focal_dist + offset
    }

    pub(crate) fn orbit_marker_screen_pos(&self) -> Option<(f32, f32)> {
        let marker = self.orbit_marker?;
        // The marker is a foreground interaction overlay, not scene geometry.
        // In a near-horizontal view the scene-fitted depth slab may exclude a
        // void fallback pivot even though its screen X/Y is perfectly valid.
        // Project without the geometry helper's depth-range rejection so the
        // marker remains visible throughout the orbit.
        self.world_to_window_px_unclipped_depth(&self.view_proj(), marker)
    }
}
