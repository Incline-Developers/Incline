use std::collections::HashSet;

use winit::keyboard::PhysicalKey;

use super::{frustum::Frustum, *};
use crate::rendering::pick::{clamped_range, triangle_weights};

/// Merge per-object AABBs into a single scene AABB, or `None` when empty.
fn merge_aabbs(aabbs: &[(DVec3, DVec3)]) -> Option<(DVec3, DVec3)> {
    aabbs.iter().copied().reduce(|(acc_min, acc_max), (min, max)| (acc_min.min(min), acc_max.max(max)))
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

fn triangle_touches_rect(triangle: [DVec2; 3], rect: ScreenRect) -> bool {
    if triangle.iter().any(|&point| rect.contains(point)) {
        return true;
    }
    if triangle
        .iter()
        .copied()
        .zip(triangle.iter().copied().cycle().skip(1))
        .take(3)
        .any(|(a, b)| segment_intersects_rect(a, b, rect.min_x, rect.max_x, rect.min_y, rect.max_y))
    {
        return true;
    }
    rect.corners()
        .iter()
        .any(|&corner| triangle_weights(corner, triangle[0], triangle[1], triangle[2]).is_some())
}

fn quad_touches_rect(corners: [DVec2; 4], rect: ScreenRect) -> bool {
    triangle_touches_rect([corners[0], corners[1], corners[2]], rect) || triangle_touches_rect([corners[0], corners[2], corners[3]], rect)
}

fn projected_text_corners(record: &TextPickRecord, view_proj: &DMat4, screen: Size) -> Option<[DVec2; 4]> {
    let [Some(a), Some(b), Some(c), Some(d)] = record.corners.map(|corner| crate::rendering::pick::world_to_screen(view_proj, corner, screen)) else {
        return None;
    };
    Some([a, b, c, d])
}

fn local_vertex_to_screen(position: [f32; 3], scene_origin: DVec3, view_proj: &DMat4, screen: Size) -> Option<DVec2> {
    let world = DVec3::from_array(position.map(f64::from)) + scene_origin;
    crate::rendering::pick::world_to_screen(view_proj, world, screen)
}

fn pick_record_touches_rect(group: &PickGeometry<'_>, record: &PickRecord, scene_origin: DVec3, view_proj: &DMat4, screen: Size, rect: ScreenRect) -> bool {
    if !pick_record_bounds_may_touch_rect(record, view_proj, screen, rect) {
        return false;
    }
    let stroke_range = clamped_range(record.stroke_range, group.stroke_verts.len());
    let fill_range = clamped_range(record.fill_range, group.fill_verts.len());
    if group.stroke_verts[stroke_range]
        .iter()
        .map(|vertex| vertex.pos)
        .chain(group.fill_verts[fill_range].iter().map(|vertex| vertex.pos))
        .filter_map(|position| local_vertex_to_screen(position, scene_origin, view_proj, screen))
        .any(|point| rect.contains(point))
    {
        return true;
    }

    let stroke_indices = &group.stroke_indices[clamped_range(record.stroke_index_range, group.stroke_indices.len())];
    for indices in stroke_indices.as_chunks::<3>().0 {
        let [Some(a), Some(b), Some(c)] = [
            group.stroke_verts.get(indices[0] as usize),
            group.stroke_verts.get(indices[1] as usize),
            group.stroke_verts.get(indices[2] as usize),
        ] else {
            continue;
        };
        let [Some(a), Some(b), Some(c)] = [a, b, c].map(|vertex| local_vertex_to_screen(vertex.pos, scene_origin, view_proj, screen)) else {
            continue;
        };
        if triangle_touches_rect([a, b, c], rect) {
            return true;
        }
    }

    let fill_indices = &group.fill_indices[clamped_range(record.fill_index_range, group.fill_indices.len())];
    for indices in fill_indices.as_chunks::<3>().0 {
        let [Some(a), Some(b), Some(c)] = [
            group.fill_verts.get(indices[0] as usize),
            group.fill_verts.get(indices[1] as usize),
            group.fill_verts.get(indices[2] as usize),
        ] else {
            continue;
        };
        let [Some(a), Some(b), Some(c)] = [a, b, c].map(|vertex| local_vertex_to_screen(vertex.pos, scene_origin, view_proj, screen)) else {
            continue;
        };
        if triangle_touches_rect([a, b, c], rect) {
            return true;
        }
    }

    false
}

fn pick_record_bounds_may_touch_rect(record: &PickRecord, view_proj: &DMat4, screen: Size, rect: ScreenRect) -> bool {
    let cursor = DVec2::new((rect.min_x + rect.max_x) * 0.5, (rect.min_y + rect.max_y) * 0.5);
    // `projected_box_overlaps` accepts one symmetric threshold. Using the
    // larger half-extent is conservative for a non-square selection box and
    // still rejects records wholly outside it before walking their streams.
    let threshold = ((rect.max_x - rect.min_x) * 0.5).max((rect.max_y - rect.min_y) * 0.5);
    crate::model::spatial::projected_box_overlaps(record.world_bounds.0, record.world_bounds.1, view_proj, screen, cursor, threshold)
}

impl<'a> Graphics<'a> {
    pub(crate) fn process_mouse_motion(&mut self, dx: f64, dy: f64) -> bool {
        if self.fly_mode_enabled && self.mouse_pressed == Some(MouseButton::Right) {
            self.fly_camera_controller.process_mouse_motion(dx, dy);
            return true;
        }

        if self.mouse_pressed == Some(MouseButton::Middle)
            && let Some(slice) = self.slice_view.as_mut()
        {
            // Same accumulation convention as the camera controller's pan so
            // the drag feel matches; consumed by `update_slice_camera`.
            slice.pan.x += -dx;
            slice.pan.y += dy;
            return true;
        }

        if !self.fly_mode_enabled && self.mouse_pressed == Some(MouseButton::Middle) {
            return self.camera_controller.process_mouse(self.mouse_pressed, dx, dy);
        }

        false
    }

    pub(crate) fn set_mouse_location(&mut self, mouse_loc: (f32, f32)) -> bool {
        let mouse_loc = self.window_to_viewport_px(mouse_loc);
        let previous_mouse_loc = self.camera_controller.mouse_loc;
        self.camera_controller.mouse_loc = mouse_loc;

        if self.mouse_pressed == Some(MouseButton::Right) && !self.fly_mode_enabled && self.slice_view.is_none() {
            let dx = mouse_loc.0 - previous_mouse_loc.0;
            let dy = mouse_loc.1 - previous_mouse_loc.1;
            return self.camera_controller.process_mouse(self.mouse_pressed, dx.into(), dy.into());
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

    /// World coordinate under the current cursor on the plane `z = plane_z`,
    /// using the last cursor position tracked by the camera controller.
    pub(crate) fn cursor_world(&self, plane_z: f64) -> Option<DVec3> {
        // A slice camera looks horizontally, so it never intersects a
        // horizontal Z plane. In that view the useful drawing surface is the
        // vertical slice plane at the camera target depth.
        if self.slice_view.is_some() {
            return Some(self.unexaggerate_point(self.cursor_world_at_target_depth()));
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
        );
        let text_hit = pick_text(&self.text_pick_records, &view_proj, screen, self.camera_controller.mouse_loc, frozen);

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

        let hit = match (document_hit, surface_hit) {
            (Some(document), Some(surface)) => {
                // In x-ray mode visible lines render through surfaces, so a document
                // hit always takes precedence over the surface beneath it.
                if xray_enabled {
                    Some((document.entity, document.world))
                } else {
                    // Compare camera-ray depths: prefer whichever hit is closer.
                    // `pick_nearest` returns the nearest 2D vertex which may not be
                    // exactly under the cursor, but for geometry placed above a surface
                    // the vertex depth is still smaller than the surface hit below it.
                    let doc_depth = (document.world - ray_origin).dot(direction);
                    let surf_depth = (surface.1 - ray_origin).dot(direction);
                    if doc_depth <= surf_depth {
                        Some((document.entity, document.world))
                    } else {
                        Some(surface)
                    }
                }
            }
            (Some(document), None) => Some((document.entity, document.world)),
            (None, surface) => surface,
        };
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
    ) -> Option<(SceneEntityId, DVec3)> {
        let document_or_surface = self.pick_at_cursor(threshold_px, triangulations, hidden, frozen, xray_enabled);
        // X-ray explicitly gives document geometry priority through opaque
        // assets. Assets remain pickable where no document geometry is hit.
        if xray_enabled && document_or_surface.is_some() {
            return document_or_surface;
        }

        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let (ray_origin, ray_direction) = self.cursor_model_ray();
        let drill_hole = SceneQuery::nearest_drill_hole_entity(drill_holes, hidden, frozen, ray_origin, ray_direction, &view_proj, screen, threshold_px);
        let block_model = self.block_model_gpu.nearest_visible_entity_hit(ray_origin, ray_direction, hidden, frozen);
        let point_cloud = self.point_cloud_gpu.nearest_visible_entity_at_screen(
            &view_proj,
            screen,
            DVec2::new(f64::from(self.camera_controller.mouse_loc.0), f64::from(self.camera_controller.mouse_loc.1)),
            threshold_px,
            hidden,
            frozen,
        );

        document_or_surface
            .into_iter()
            .chain(drill_hole)
            .chain(block_model)
            .chain(point_cloud)
            .min_by(|(_, a), (_, b)| (*a - ray_origin).dot(ray_direction).total_cmp(&(*b - ray_origin).dot(ray_direction)))
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
        (!self.nonselectable_asset_occludes(hit.1, hidden, &view_proj, screen)).then_some(hit)
    }

    fn nonselectable_asset_occludes(&self, candidate: DVec3, hidden: &HashSet<SceneEntityId>, view_proj: &DMat4, screen: Size) -> bool {
        let clip = *view_proj * candidate.extend(1.0);
        if clip.w.abs() <= f64::EPSILON {
            return false;
        }
        let candidate_depth = clip.z / clip.w;
        let block_depth = crate::rendering::query::ray_through_world_point(view_proj, candidate)
            .and_then(|(origin, direction)| self.block_model_gpu.nearest_opaque_hit(origin, direction, hidden))
            .and_then(|point| {
                let clip = *view_proj * point.extend(1.0);
                (clip.w.abs() > f64::EPSILON).then_some(clip.z / clip.w)
            });
        let point_depth = crate::rendering::pick::world_to_screen(view_proj, candidate, screen)
            .and_then(|screen_point| self.point_cloud_gpu.nearest_depth_at_screen(view_proj, screen, screen_point, hidden));
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
                let stroke = clamped_range(record.stroke_range, group.stroke_verts.len());
                let fill = clamped_range(record.fill_range, group.fill_verts.len());
                let points = group.stroke_verts[stroke]
                    .iter()
                    .map(|vertex| vertex.pos)
                    .chain(group.fill_verts[fill].iter().map(|vertex| vertex.pos));
                let mut any = false;
                let enclosed = points
                    .filter_map(|position| {
                        let world = DVec3::from_array(position.map(f64::from)) + self.scene_origin;
                        crate::rendering::pick::world_to_screen(&view_proj, world, screen)
                    })
                    .all(|point| {
                        any = true;
                        rect.contains(point)
                    });
                if any && enclosed && seen.insert(record.entity) {
                    hits.push(record.entity);
                }
            }
        }

        for record in &self.text_pick_records {
            if frozen.contains(&record.entity) {
                continue;
            }
            let Some(corners) = projected_text_corners(record, &view_proj, screen) else {
                continue;
            };
            if corners.iter().all(|&point| rect.contains(point)) && seen.insert(record.entity) {
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
        let mut hits = Vec::new();
        let mut seen = HashSet::new();

        for group in self.pick_geometry_groups() {
            for record in group.records {
                if frozen.contains(&record.entity) {
                    continue;
                }
                if pick_record_touches_rect(&group, record, self.scene_origin, &view_proj, screen, rect) && seen.insert(record.entity) {
                    hits.push(record.entity);
                }
            }
        }

        for record in &self.text_pick_records {
            if frozen.contains(&record.entity) {
                continue;
            }
            let Some(corners) = projected_text_corners(record, &view_proj, screen) else {
                continue;
            };
            if quad_touches_rect(corners, rect) && seen.insert(record.entity) {
                hits.push(record.entity);
            }
        }

        hits
    }

    /// Begin an orbit with the anchor at the surface or geometry point under the cursor.
    /// Falls back to the current-target depth when nothing is hit.
    /// Called from the app level where triangulations are available.
    pub(crate) fn begin_orbit_at_surface(
        &mut self,
        triangulations: &[OpenTriangulation],
        drill_holes: &[OpenDrillHoleDataset],
        hidden: &HashSet<SceneEntityId>,
        frozen: &HashSet<SceneEntityId>,
        document: &Document,
        snap_index: &crate::model::spatial::ObjectSnapIndex,
    ) {
        // Prefer snapping to a nearby document vertex so the orbit pivot lands
        // on actual geometry (lines, polylines, points) when one is close.
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        let snap_pt = SceneQuery::snap(
            document,
            snap_index,
            triangulations,
            hidden,
            frozen,
            &CursorMode::SnapToPoint,
            &view_proj,
            screen,
            self.camera_controller.mouse_loc,
            SNAP_THRESHOLD_PX,
            false,
        );

        let pt = if let Some(p) = snap_pt {
            p
        } else {
            let (ray_origin, direction) = self.cursor_model_ray();
            let triangulation_hit = SceneQuery::nearest_surface(triangulations, hidden, Some(frozen), ray_origin, direction).map(|(_, world)| world);
            let drill_hole_hit = SceneQuery::nearest_drill_hole_entity(drill_holes, hidden, frozen, ray_origin, direction, &view_proj, screen, 0.0).map(|(_, world)| world);
            let block_model_hit = self.block_model_gpu.nearest_visible_hit(ray_origin, direction, hidden, frozen);
            triangulation_hit
                .into_iter()
                .chain(drill_hole_hit)
                .chain(block_model_hit)
                .min_by(|a, b| (*a - ray_origin).dot(direction).total_cmp(&(*b - ray_origin).dot(direction)))
                .unwrap_or_else(|| {
                    // No asset surface hit - try picking any document object
                    // near the cursor so the pivot lands on visible geometry rather than
                    // at the (possibly stale) camera-target depth.
                    self.pick_at_cursor(SNAP_THRESHOLD_PX, triangulations, hidden, frozen, false)
                        .map(|(_, world)| world)
                        .unwrap_or_else(|| self.unexaggerate_point(self.cursor_world_at_target_depth()))
                })
        };

        self.camera.sync_angles_from_forward();
        self.camera_controller.begin_orbit(self.exaggerate_point(pt));
        self.orbit_marker = Some(pt);
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
                    (center, 1.0_f64)
                } else {
                    // Plan view: right == X, up == Y.
                    let zoom_h = size.y / 2.0;
                    let zoom_w = size.x / (2.0 * aspect);
                    // 10 % padding so content never touches the viewport edge.
                    (center, zoom_h.max(zoom_w) * 1.1)
                }
            }
            None => (DVec3::ZERO, 1.0_f64),
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
            1.0
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

    pub(crate) fn set_standard_view(&mut self, view: crate::ui::state::StandardView) {
        let (forward, up) = match view {
            crate::ui::state::StandardView::Up => (DVec3::NEG_Z, DVec3::Y),
            crate::ui::state::StandardView::Down => (DVec3::Z, DVec3::Y),
            crate::ui::state::StandardView::North => (DVec3::NEG_Y, DVec3::Z),
            crate::ui::state::StandardView::South => (DVec3::Y, DVec3::Z),
            crate::ui::state::StandardView::West => (DVec3::X, DVec3::Z),
            crate::ui::state::StandardView::East => (DVec3::NEG_X, DVec3::Z),
        };
        self.camera_controller.begin_view_transition(&self.camera, forward, up, self.projection.zoom);
        self.camera_controller.end_orbit();
        self.orbit_marker = None;
    }

    /// Keep the depth range tight around the current scene. An oversized range
    /// loses enough precision that back-side mesh edges can compare equal to
    /// the front surface and bleed through it, especially in perspective.
    pub(super) fn fit_depth_to_scene(
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
        // scene-relative AABBs. This lags the live camera by at most one frame,
        // which the depth-range padding absorbs. Only the lateral planes are
        // read, so it does not depend on the near/far we are about to set.
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

    /// Ensure an in-progress drawing remains inside the camera's clip volume
    /// even when its drawing plane lies outside the committed scene.
    pub(super) fn include_pending_stroke_in_depth(&mut self, editor: &EditorState) {
        if editor.pending_stroke.is_empty() && editor.circle_draft.is_none() {
            return;
        }

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

    pub(super) fn upload_camera_uniform(&mut self, interaction_resolution_divisor: u32) {
        self.camera_uniform
            .update_view_proj(&self.camera, &self.projection, self.scene_origin, self.vertical_exaggeration);
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
    fn update_slice_camera(&mut self, dt: Duration) {
        let screen = self.screen_size();
        let mouse_loc = self.camera_controller.mouse_loc;
        let Some(slice) = self.slice_view.as_mut() else {
            return;
        };
        // Matches the zoom feel of the main `CameraController` (see init.rs).
        const SLICE_ZOOM_SENSITIVITY: f64 = 0.005;
        let step = dt.as_secs_f64().min(0.1);

        // Q/E: rotate the slice line about its centre. Q turns the view left
        // (counter-clockwise seen from above), E right.
        let rotate_amount = f64::from(i8::from(slice.input.rotate_left)) - f64::from(i8::from(slice.input.rotate_right));
        if rotate_amount != 0.0 {
            let angle = rotate_amount * slice.rotate_speed * step;
            slice.direction = DVec2::from_angle(angle).rotate(slice.direction).normalize_or(slice.direction);
        }

        let strike = DVec3::new(slice.direction.x, slice.direction.y, 0.0);
        let forward = slice.forward();

        // W/S: move the slab along its normal.
        let slab_amount = f64::from(i8::from(slice.input.forward)) - f64::from(i8::from(slice.input.backward));
        if slab_amount != 0.0 {
            slice.center += forward * slab_amount * slice.move_speed * step;
        }

        // Middle-drag pan within the section plane: horizontal = strike,
        // vertical = elevation. Pixel-to-world matches the ortho pan feel.
        if slice.pan != DVec2::ZERO {
            let world_per_pixel = (2.0 * self.projection.zoom / screen.1.max(1.0) as f64).max(0.0);
            slice.center += (strike * slice.pan.x + DVec3::Z * slice.pan.y) * world_per_pixel;
            slice.pan = DVec2::ZERO;
        }

        // Scroll: ortho zoom toward the cursor, mirroring the main controller.
        if slice.scroll != 0.0 {
            let zoom_scale = if slice.scroll > 0.0 {
                1.0 - (1.0 / (1.0 + SLICE_ZOOM_SENSITIVITY * slice.scroll))
            } else {
                SLICE_ZOOM_SENSITIVITY * slice.scroll
            };
            let zoom_factor = self.projection.zoom * zoom_scale;
            let mouse_ndc = crate::rendering::camera::point(mouse_loc.0, mouse_loc.1, screen);
            let aspect = screen.0 as f64 / screen.1.max(1.0) as f64;
            slice.center += (strike * mouse_ndc.x * aspect + DVec3::Z * mouse_ndc.y) * zoom_factor;
            self.projection.zoom = (self.projection.zoom - zoom_factor).max(1.0e-4);
            slice.scroll = 0.0;
        }

        // The camera sits on the slice plane itself so the symmetric
        // znear/zfar slab is centred on the plane.
        self.camera.look_to(slice.center, forward, DVec3::Z, self.projection.zoom.max(1.0));
    }

    pub(crate) fn update(&mut self, dt: Duration, interaction_resolution_divisor: u32) {
        if self.slice_view.is_some() {
            self.update_slice_camera(dt);
        } else if self.camera_controller.has_view_transition() {
            self.camera_controller.update_view_transition(&mut self.camera, &mut self.projection, dt);
        } else if self.fly_mode_enabled {
            self.fly_camera_controller.update_camera(&mut self.camera, dt);
        } else {
            let screen_size = self.screen_size();
            self.camera_controller.update_camera(&mut self.camera, &mut self.projection, dt, screen_size);
        }
        self.upload_camera_uniform(interaction_resolution_divisor);
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
                    slice.scroll += match delta {
                        MouseScrollDelta::LineDelta(_, scroll) => f64::from(*scroll) * 100.0,
                        MouseScrollDelta::PixelDelta(position) => position.y,
                    };
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
        let right = forward.cross(self.camera.up()).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        let screen = self.screen_size();
        let mouse_ndc = crate::rendering::camera::point(self.camera_controller.mouse_loc.0, self.camera_controller.mouse_loc.1, screen);
        let aspect = screen.0 as f64 / screen.1.max(1.0) as f64;
        let focal_dist = (self.camera.target() - self.camera.position).dot(forward).abs();
        self.camera.position + forward * focal_dist + right * mouse_ndc.x * aspect * self.projection.zoom + up * mouse_ndc.y * self.projection.zoom
    }

    pub(crate) fn orbit_marker_screen_pos(&self) -> Option<(f32, f32)> {
        let marker = self.orbit_marker?;
        let view_proj = self.view_proj();
        let screen = self.screen_size();
        // The marker is a foreground interaction overlay, not scene geometry.
        // In a near-horizontal view the scene-fitted depth slab may exclude a
        // void fallback pivot even though its screen X/Y is perfectly valid.
        // Project without the geometry helper's depth-range rejection so the
        // marker remains visible throughout the orbit.
        crate::rendering::pick::world_to_screen_unclipped_depth(&view_proj, marker, screen).map(|v| self.viewport_to_window_px((v.x as f32, v.y as f32)))
    }
}
