use std::time::Duration;

use glam::{DMat4, DQuat, DVec2, DVec3, dcamera};
use winit::{
    dpi::PhysicalPosition,
    event::{MouseButton, MouseScrollDelta},
    keyboard::KeyCode,
};

use crate::Size;

#[derive(Clone, Debug)]
pub(crate) struct Camera {
    pub(crate) position: DVec3,
    yaw: f64,
    pitch: f64,
    target: DVec3,
    up: DVec3,
}

impl Camera {
    pub(crate) fn new(position: DVec3, yaw: f64, pitch: f64) -> Self {
        Self {
            position,
            yaw,
            pitch,
            target: DVec3::ZERO,
            up: DVec3::Y,
        }
    }

    pub(crate) fn calc_matrix(&self) -> DMat4 {
        dcamera::rh::view::look_to_mat4(self.position, self.forward(), self.up)
    }

    pub(crate) fn forward(&self) -> DVec3 {
        let to_target = self.target - self.position;
        if to_target.length_squared() > f64::EPSILON {
            return to_target.normalize();
        }

        self.angle_forward()
    }

    fn angle_forward(&self) -> DVec3 {
        let (pitch_sin, pitch_cos) = self.pitch.sin_cos();
        let (yaw_sin, yaw_cos) = self.yaw.sin_cos();
        DVec3::new(pitch_cos * yaw_cos, pitch_cos * yaw_sin, pitch_sin).normalize()
    }

    fn angle_right(&self) -> DVec3 {
        let (yaw_sin, yaw_cos) = self.yaw.sin_cos();
        DVec3::new(-yaw_sin, yaw_cos, 0.0)
    }

    pub(crate) fn apply_angle_orientation(&mut self, target_distance: f64) {
        let forward = self.angle_forward();
        let right = self.angle_right();
        self.up = forward.cross(right).normalize();
        self.target = self.position + forward * target_distance.max(MIN_ORTHO_ZOOM);
    }

    pub(crate) fn sync_angles_from_forward(&mut self) {
        let forward = self.forward();
        let horizontal = forward.x.hypot(forward.y);
        if horizontal > f64::EPSILON {
            self.yaw = forward.y.atan2(forward.x);
        }
        self.pitch = forward.z.atan2(horizontal);
    }

    pub(crate) fn up(&self) -> DVec3 {
        self.up
    }

    pub(crate) fn target(&self) -> DVec3 {
        self.target
    }

    /// Reset to a top-down plan view centred on `center` with the given ortho half-height.
    /// After the next `update_camera` tick, `projection.zoom` will settle to `zoom`.
    /// Set `projection.zoom = zoom` in the same call site for immediate effect.
    pub(crate) fn reset_to_plan_view(&mut self, center: DVec3, zoom: f64) {
        use std::f64::consts::FRAC_PI_2;
        self.yaw = FRAC_PI_2;
        self.pitch = -FRAC_PI_2;
        self.up = DVec3::Y;
        self.target = center;
        // Camera height == zoom so `update_camera` re-derives the same zoom.
        self.position = center + DVec3::Z * zoom.max(MIN_ORTHO_ZOOM);
    }

    /// Place the camera at `position` looking along `forward` with the given
    /// `up`, keeping the focal point `target_distance` ahead.
    pub(crate) fn look_to(&mut self, position: DVec3, forward: DVec3, up: DVec3, target_distance: f64) {
        self.position = position;
        self.target = position + forward * target_distance.max(MIN_ORTHO_ZOOM);
        self.up = up;
        self.sync_angles_from_forward();
    }

    /// Centre the current view on `center` without changing its orientation.
    /// The camera distance tracks the orthographic half-height because the
    /// controller derives the projection zoom from that distance each frame.
    pub(crate) fn frame_keep_orientation(&mut self, center: DVec3, zoom: f64) {
        let forward = self.forward();
        self.target = center;
        self.position = center - forward * zoom.max(MIN_ORTHO_ZOOM);
    }
}

const MIN_ORTHO_ZOOM: f64 = 1.0e-4;
pub(crate) const PERSPECTIVE_FOV_Y: f64 = 40.0_f64.to_radians();
const MIN_PERSPECTIVE_NEAR: f64 = 0.25;
const MAX_PERSPECTIVE_DEPTH_SPAN: f64 = 150_000.0;
const PERSPECTIVE_NEAR_PADDING_FRACTION: f64 = 0.1;

fn padded_perspective_near(min_depth: f64, padding: f64, far: f64, near_limit: f64) -> f64 {
    // A scene-scale padding value is useful at the far plane but disastrous
    // at the near plane: subtracting (say) 25 m from geometry 20 m away would
    // collapse the near plane to its hard minimum and throw away perspective
    // depth precision. Keep at most a 10% margin in front of the nearest bounds.
    let near_padding = padding.min(min_depth.max(0.0) * PERSPECTIVE_NEAR_PADDING_FRACTION);
    (min_depth - near_padding).max(near_limit).min(far * (1.0 - f64::EPSILON))
}

#[derive(Clone, Debug)]
pub(crate) struct Projection {
    aspect: f64,
    znear: f64,
    zfar: f64,
    pub(crate) zoom: f64,
    perspective: bool,
    perspective_fov_y: f64,
    min_perspective_near: f64,
    max_perspective_depth_span: f64,
}

impl Projection {
    pub(crate) fn new(width: u32, height: u32, znear: f64, zfar: f64) -> Self {
        Self {
            aspect: width.max(1) as f64 / height.max(1) as f64,
            znear,
            zfar,
            zoom: 1.0,
            perspective: false,
            perspective_fov_y: PERSPECTIVE_FOV_Y,
            min_perspective_near: MIN_PERSPECTIVE_NEAR,
            max_perspective_depth_span: MAX_PERSPECTIVE_DEPTH_SPAN,
        }
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        self.aspect = width.max(1) as f64 / height.max(1) as f64;
    }

    pub(crate) fn set_symmetric_depth_extent(&mut self, half_depth: f64) {
        let half_depth = half_depth.max(1.0);
        self.znear = -half_depth;
        self.zfar = half_depth;
    }

    /// Fit the clip planes to camera-space depths measured along the view
    /// direction. Orthographic projection tightly fits both signed bounds;
    /// perspective keeps its near plane in front of the camera.
    pub(crate) fn set_view_depth_range(&mut self, min_depth: f64, max_depth: f64, padding: f64) {
        if !self.perspective {
            self.znear = min_depth - padding;
            self.zfar = (max_depth + padding).max(self.znear + 1.0);
            return;
        }

        let requested_far = (max_depth + padding).max(self.min_perspective_near * 2.0);
        self.znear = padded_perspective_near(min_depth, padding, requested_far, self.min_perspective_near);
        self.zfar = requested_far.min(self.znear + self.max_perspective_depth_span);
    }

    pub(crate) fn expand_view_depth_range(&mut self, min_depth: f64, max_depth: f64, padding: f64) {
        if !self.perspective {
            self.znear = self.znear.min(min_depth - padding);
            self.zfar = self.zfar.max(max_depth + padding).max(self.znear + 1.0);
            return;
        }

        let requested_far = (max_depth + padding).max(self.min_perspective_near * 2.0);
        if max_depth + padding > self.min_perspective_near {
            let near = padded_perspective_near(min_depth, padding, requested_far.max(self.zfar), self.min_perspective_near);
            self.znear = self.znear.min(near);
        }
        self.zfar = self.zfar.max(requested_far).min(self.znear + self.max_perspective_depth_span);
    }

    pub(crate) fn calc_matrix(&self) -> DMat4 {
        let projection = if self.perspective {
            dcamera::rh::proj::directx::perspective(self.perspective_fov_y, self.aspect, self.znear, self.zfar)
        } else {
            let half_w = self.aspect * self.zoom;
            let half_h = self.zoom;
            dcamera::rh::proj::directx::orthographic(-half_w, half_w, -half_h, half_h, self.znear, self.zfar)
        };

        // WebGPU pipelines cannot change their depth comparison dynamically,
        // so both projection modes use the same reversed depth convention.
        // Orthographic precision remains linear and otherwise unchanged;
        // perspective gains the floating-point precision benefits of reversed-Z.
        // In D3D/WebGPU clip space this remaps z to w-z: near 0 -> 1, far 1 -> 0.
        let reverse_depth = DMat4::from_cols(glam::DVec4::X, glam::DVec4::Y, glam::DVec4::new(0.0, 0.0, -1.0, 0.0), glam::DVec4::new(0.0, 0.0, 1.0, 1.0));
        reverse_depth * projection
    }

    pub(crate) fn clip_planes(&self) -> (f64, f64) {
        (self.znear, self.zfar)
    }

    pub(crate) fn is_perspective(&self) -> bool {
        self.perspective
    }

    pub(crate) fn perspective_fov_y(&self) -> f64 {
        self.perspective_fov_y
    }

    pub(crate) fn configure_perspective(&mut self, field_of_view_degrees: f64, near_clip_limit: f64, max_clip_span: f64) {
        self.perspective_fov_y = field_of_view_degrees.to_radians();
        self.min_perspective_near = near_clip_limit;
        self.max_perspective_depth_span = max_clip_span;
        if self.perspective {
            self.znear = self.znear.max(self.min_perspective_near);
            self.zfar = self.zfar.max(self.znear * (1.0 + f64::EPSILON)).min(self.znear + self.max_perspective_depth_span);
        }
    }

    pub(crate) fn set_perspective(&mut self, perspective: bool) {
        if perspective && !self.perspective {
            let far = self.znear.abs().max(self.zfar.abs()).max(self.min_perspective_near * 2.0);
            self.znear = (far * 1.0e-4).max(self.min_perspective_near).min(far * (1.0 - f64::EPSILON));
            self.zfar = far.min(self.znear + self.max_perspective_depth_span);
        } else if !perspective && self.perspective {
            let half_depth = self.zfar.max(1.0);
            self.znear = -half_depth;
            self.zfar = half_depth;
        }
        self.perspective = perspective;
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CameraUniform {
    pub(crate) view_proj: [[f32; 4]; 4],
    cam_forward: [f32; 4],
    cam_position: [f32; 4],
    viewport: [f32; 4],
    inv_view_proj: [[f32; 4]; 4],
    /// xy: the viewport rect's offset in physical pixels within whatever
    /// target it's rendered into - `@builtin(position)` in a fragment shader
    /// reports pixels in the *target's* space, not the viewport sub-rect's
    /// own space, so a shader reconstructing a screen-relative position (the
    /// XY grid) needs to subtract this first. Zero for every render target
    /// that's filled from its own origin (plots, slice previews, the offscreen
    /// volume raycast target) - only the main window's toolbar-bounded canvas
    /// has a nonzero offset. zw: unused padding.
    viewport_origin: [f32; 4],
}

impl CameraUniform {
    pub(crate) fn new() -> Self {
        Self {
            view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            cam_forward: [0.; 4],
            cam_position: [0.; 4],
            viewport: [1.0, 1.0, 0.0, 0.0],
            inv_view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            viewport_origin: [0.0; 4],
        }
    }

    pub(crate) fn update_view_proj(&mut self, camera: &Camera, projection: &Projection, scene_origin: DVec3, vertical_exaggeration: f64) {
        let rebased_view = dcamera::rh::view::look_to_mat4(camera.position - scene_origin, camera.forward(), camera.up);
        let exaggeration = DMat4::from_scale(DVec3::new(1.0, 1.0, vertical_exaggeration));
        let view_proj = projection.calc_matrix() * rebased_view * exaggeration;
        self.view_proj = view_proj.as_mat4().to_cols_array_2d();
        self.inv_view_proj = view_proj.inverse().as_mat4().to_cols_array_2d();
        let forward = camera.forward();
        self.cam_forward = [forward.x as f32, forward.y as f32, forward.z as f32, 0.];
        self.cam_position = [
            (camera.position.x - scene_origin.x) as f32,
            (camera.position.y - scene_origin.y) as f32,
            (camera.position.z - scene_origin.z) as f32,
            0.,
        ];
    }

    pub(crate) fn update_viewport(&mut self, width: u32, height: u32) {
        self.viewport = [width.max(1) as f32, height.max(1) as f32, self.viewport[2], self.viewport[3]];
    }

    /// Set the viewport rect's offset - see `viewport_origin`. Callers that
    /// render into their own dedicated, origin-filled target (everything
    /// except the main window's canvas) should leave this at its default of
    /// zero rather than calling it.
    pub(crate) fn set_viewport_origin(&mut self, x: u32, y: u32) {
        self.viewport_origin = [x as f32, y as f32, 0.0, 0.0];
    }

    /// Interaction-quality knobs packed into the otherwise-unused `viewport.zw`
    /// so no new bind group is needed. `lod_boost` (`z`) divides the volume
    /// raycaster's LOD footprint threshold - `> 1` coarsens bricks sooner (and
    /// also caps its per-ray step count). `render_scale` (`w`, `0..=1`) is the
    /// fraction of full resolution the raycast is drawn at before upscaling.
    /// Both revert to `1.0` (full quality) when the camera settles.
    pub(crate) fn set_interaction_quality(&mut self, lod_boost: f32, render_scale: f32) {
        self.viewport[2] = lod_boost;
        self.viewport[3] = render_scale;
    }

    /// Fraction of full resolution the volume raycast is currently drawn at.
    pub(crate) fn render_scale(&self) -> f32 {
        if self.viewport[3] <= 0.0 { 1.0 } else { self.viewport[3] }
    }
}

#[derive(Debug)]
pub(crate) struct CameraController {
    amount_left: f64,
    amount_right: f64,
    amount_forward: f64,
    amount_backward: f64,
    amount_up: f64,
    amount_down: f64,
    pan: DVec2,
    rotate_horizontal: f64,
    rotate_vertical: f64,
    scroll: f64,
    speed: f64,
    zoom_sensitivity: f64,
    rotate_sensitivity: f64,
    invert_vertical_look: bool,
    invert_horizontal_look: bool,
    zoom_towards_cursor: bool,
    pub(crate) mouse_loc: (f32, f32),
    orbit_anchor: Option<DVec3>,
    view_transition: Option<ViewTransition>,
}

impl CameraController {
    pub(crate) fn new(speed: f64, zoom_sensitivity: f64, rotate_sensitivity: f64) -> Self {
        Self {
            amount_left: 0.0,
            amount_right: 0.0,
            amount_forward: 0.0,
            amount_backward: 0.0,
            amount_up: 0.0,
            amount_down: 0.0,
            pan: DVec2::ZERO,
            rotate_horizontal: 0.0,
            rotate_vertical: 0.0,
            scroll: 0.0,
            speed,
            zoom_sensitivity,
            rotate_sensitivity,
            invert_vertical_look: false,
            invert_horizontal_look: false,
            zoom_towards_cursor: true,
            mouse_loc: (0., 0.),
            orbit_anchor: None,
            view_transition: None,
        }
    }

    pub(crate) fn configure_preferences(
        &mut self,
        orbit_sensitivity: f64,
        zoom_sensitivity: f64,
        invert_vertical_look: bool,
        invert_horizontal_look: bool,
        zoom_towards_cursor: bool,
    ) {
        self.rotate_sensitivity = orbit_sensitivity;
        self.zoom_sensitivity = zoom_sensitivity;
        self.invert_vertical_look = invert_vertical_look;
        self.invert_horizontal_look = invert_horizontal_look;
        self.zoom_towards_cursor = zoom_towards_cursor;
    }

    pub(crate) fn begin_orbit(&mut self, anchor: DVec3) {
        self.view_transition = None;
        self.orbit_anchor = Some(anchor);
    }

    pub(crate) fn cancel_view_transition(&mut self) {
        self.view_transition = None;
    }

    pub(crate) fn end_orbit(&mut self) {
        self.orbit_anchor = None;
    }

    pub(crate) fn begin_view_transition(&mut self, camera: &Camera, forward: DVec3, up_hint: DVec3, target_distance: f64) {
        let end_forward = forward.normalize_or(camera.forward());
        let end_up = orthonormal_up(end_forward, up_hint);
        self.view_transition = Some(ViewTransition {
            start_forward: camera.forward(),
            end_forward,
            end_up,
            target: camera.target(),
            distance: target_distance.max(MIN_ORTHO_ZOOM),
            elapsed: Duration::ZERO,
            duration: Duration::from_millis(280),
        });
    }

    pub(crate) fn has_view_transition(&self) -> bool {
        self.view_transition.is_some()
    }

    pub(crate) fn update_view_transition(&mut self, camera: &mut Camera, projection: &mut Projection, dt: Duration) {
        let Some(transition) = self.view_transition.as_mut() else {
            return;
        };

        transition.elapsed += dt;
        let duration = transition.duration.as_secs_f64().max(f64::EPSILON);
        let linear_t = (transition.elapsed.as_secs_f64() / duration).clamp(0.0, 1.0);
        let t = ease_out_cubic(linear_t);
        let forward = slerp_unit_with_axis_hint(transition.start_forward, transition.end_forward, transition.end_up, t);
        let up = orthonormal_up(forward, transition.end_up);

        camera.target = transition.target;
        camera.position = transition.target - forward * transition.distance;
        camera.up = up;
        camera.sync_angles_from_forward();
        projection.zoom = transition.distance;

        if linear_t >= 1.0 {
            self.view_transition = None;
        }
    }

    pub(crate) fn process_mouse(&mut self, mouse_pressed: Option<MouseButton>, mouse_dx: f64, mouse_dy: f64) -> bool {
        if let Some(button) = mouse_pressed {
            match button {
                MouseButton::Right => {
                    self.view_transition = None;
                    self.rotate_horizontal += mouse_dx;
                    self.rotate_vertical += mouse_dy;
                    return true;
                }
                MouseButton::Middle => {
                    self.view_transition = None;
                    // Accumulate device deltas until next frame for smooth 1:1 drag.
                    self.pan.y += mouse_dy;
                    self.pan.x += -mouse_dx;
                    return true;
                }
                _ => {}
            }
        } else {
            self.pan = DVec2::ZERO;
        }
        false
    }

    pub(crate) fn process_scroll(&mut self, delta: &MouseScrollDelta) {
        self.view_transition = None;
        self.scroll += match delta {
            // Assuming a line is about 100 pixels
            MouseScrollDelta::LineDelta(_, scroll) => *scroll as f64 * 100.0,
            MouseScrollDelta::PixelDelta(PhysicalPosition { y: scroll, .. }) => *scroll,
        };
    }

    pub(crate) fn has_pending_updates(&self) -> bool {
        self.amount_left != 0.0
            || self.amount_right != 0.0
            || self.amount_forward != 0.0
            || self.amount_backward != 0.0
            || self.amount_up != 0.0
            || self.amount_down != 0.0
            || self.pan != DVec2::ZERO
            || self.rotate_horizontal != 0.0
            || self.rotate_vertical != 0.0
            || self.scroll != 0.0
            || self.view_transition.is_some()
    }

    pub(crate) fn update_camera(&mut self, camera: &mut Camera, projection: &mut Projection, dt: Duration, screen_size: Size) {
        let dt = dt.as_secs_f64();

        // Calculate direction unit vectors (Z-up: horizontal plane is XY)
        let (yaw_sin, yaw_cos) = camera.yaw.sin_cos();

        let movement_forward = DVec3::new(yaw_cos, yaw_sin, 0.0);
        let movement_right = DVec3::new(-yaw_sin, yaw_cos, 0.0);
        let scrollward = camera.forward();
        let right = scrollward.cross(camera.up).normalize_or(movement_right.normalize_or_zero());
        let up = right.cross(scrollward).normalize_or_zero();
        let max_pitch = std::f64::consts::FRAC_PI_2;

        // Movement updates
        let forward_movement = (self.amount_forward - self.amount_backward) * self.speed * dt;
        let right_movement = (self.amount_right - self.amount_left) * self.speed * dt;
        let vertical_movement = (self.amount_up - self.amount_down) * self.speed * dt;

        // Position-based transformations
        let to_target_distance_modifier = (camera.target - camera.position).dot(scrollward).abs();

        camera.position += movement_forward * forward_movement;
        camera.position += movement_right * right_movement;
        camera.position.z += vertical_movement;

        // Update target
        camera.target = camera.position + scrollward * to_target_distance_modifier;

        // Scroll / zoom-like motion
        let mouse_loc_rel = point(self.mouse_loc.0, self.mouse_loc.1, screen_size);
        let zoom_scale = if self.scroll > 0.0 {
            1. - (1. / (1. + self.zoom_sensitivity * self.scroll))
        } else {
            self.zoom_sensitivity * self.scroll
        };
        let zoom_factor = to_target_distance_modifier * zoom_scale;
        let aspect = screen_size.0 as f64 / screen_size.1.max(1.0) as f64;
        let zoom_lateral = if self.zoom_towards_cursor {
            (right * mouse_loc_rel.x * aspect + up * mouse_loc_rel.y) * zoom_factor
        } else {
            DVec3::ZERO
        };
        camera.position += scrollward * zoom_factor + zoom_lateral;
        camera.target += zoom_lateral;
        self.scroll = 0.0;

        // Panning motion: map mouse pixels directly to world units for CAD-like drag feel.
        let world_per_pixel = (2.0 * projection.zoom / screen_size.1.max(1.0) as f64).max(0.0);
        let pan_delta = up * self.pan.y * world_per_pixel + right * self.pan.x * world_per_pixel;
        camera.position += pan_delta;
        camera.target += pan_delta;

        self.pan = DVec2::ZERO;

        // Orbit position and target around the selected pivot.
        if self.rotate_horizontal != 0.0 || self.rotate_vertical != 0.0 {
            let pivot = self.orbit_anchor.unwrap_or(camera.target);
            let horizontal_sign = if self.invert_horizontal_look { -1.0 } else { 1.0 };
            let vertical_sign = if self.invert_vertical_look { 1.0 } else { -1.0 };
            let horizontal_rotate_angle = self.rotate_horizontal * self.rotate_sensitivity * horizontal_sign;
            let desired_pitch_delta = self.rotate_vertical * self.rotate_sensitivity * vertical_sign;
            let next_pitch = (camera.pitch + desired_pitch_delta).clamp(-max_pitch, max_pitch);
            let vertical_rotate_angle = next_pitch - camera.pitch;
            let horizontal_rotation = DQuat::from_rotation_z(-horizontal_rotate_angle);

            camera.yaw += horizontal_rotate_angle;
            camera.position = horizontal_rotation.mul_vec3(camera.position - pivot) + pivot;
            camera.target = horizontal_rotation.mul_vec3(camera.target - pivot) + pivot;
            camera.up = horizontal_rotation.mul_vec3(camera.up).normalize_or_zero();

            camera.pitch = next_pitch;
            let rotated_scrollward = camera.forward();
            let rotated_right = rotated_scrollward.cross(camera.up).normalize_or(right.normalize_or_zero());
            let vertical_rotation = DQuat::from_axis_angle(rotated_right, vertical_rotate_angle);
            camera.position = vertical_rotation.mul_vec3(camera.position - pivot) + pivot;
            camera.target = vertical_rotation.mul_vec3(camera.target - pivot) + pivot;
            camera.up = vertical_rotation.mul_vec3(camera.up).normalize_or_zero();

            self.rotate_horizontal = 0.0;
            self.rotate_vertical = 0.0;
        }

        // Scale orthographic bounds by camera-to-target distance for zoom effect
        let dist = (camera.target - camera.position).dot(camera.forward()).abs();
        projection.zoom = dist.max(MIN_ORTHO_ZOOM);
    }
}

#[derive(Debug)]
struct ViewTransition {
    start_forward: DVec3,
    end_forward: DVec3,
    end_up: DVec3,
    target: DVec3,
    distance: f64,
    elapsed: Duration,
    duration: Duration,
}

fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

fn orthonormal_up(forward: DVec3, up_hint: DVec3) -> DVec3 {
    let fallback_up = if forward.z.abs() > 0.9 { DVec3::Y } else { DVec3::Z };
    let right = {
        let preferred = forward.cross(up_hint);
        if preferred.length_squared() > f64::EPSILON {
            preferred.normalize()
        } else {
            forward.cross(fallback_up).normalize_or(DVec3::X)
        }
    };
    right.cross(forward).normalize_or(up_hint)
}

fn slerp_unit(from: DVec3, to: DVec3, t: f64) -> DVec3 {
    let from = from.normalize_or_zero();
    let to = to.normalize_or(from);
    let dot = from.dot(to).clamp(-1.0, 1.0);

    if dot > 0.9995 {
        return from.lerp(to, t).normalize_or(to);
    }

    if dot < -0.9995 {
        let axis = from.cross(if from.z.abs() < 0.9 { DVec3::Z } else { DVec3::Y }).normalize_or(DVec3::X);
        return DQuat::from_axis_angle(axis, std::f64::consts::PI * t).mul_vec3(from).normalize_or(to);
    }

    let theta = dot.acos();
    let sin_theta = theta.sin();
    let a = ((1.0 - t) * theta).sin() / sin_theta;
    let b = (t * theta).sin() / sin_theta;
    (from * a + to * b).normalize_or(to)
}

fn slerp_unit_with_axis_hint(from: DVec3, to: DVec3, axis_hint: DVec3, t: f64) -> DVec3 {
    let from = from.normalize_or_zero();
    let to = to.normalize_or(from);
    let dot = from.dot(to).clamp(-1.0, 1.0);

    if dot >= -0.9995 {
        return slerp_unit(from, to, t);
    }

    let hinted_axis = axis_hint.reject_from(from);
    let fallback_axis = from.cross(if from.z.abs() < 0.9 { DVec3::Z } else { DVec3::Y }).normalize_or(DVec3::X);
    let axis = hinted_axis.normalize_or(fallback_axis);
    DQuat::from_axis_angle(axis, std::f64::consts::PI * t).mul_vec3(from).normalize_or(to)
}
#[derive(Debug)]
pub(crate) struct FlyCameraController {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    boost: bool,
    analog_movement: DVec3,
    look_delta: DVec2,
    speed: f64,
    look_sensitivity: f64,
    invert_vertical_look: bool,
    invert_horizontal_look: bool,
    velocity: DVec3,
    skip_movement_frame: bool,
}

impl FlyCameraController {
    pub(crate) fn new(speed: f64, look_sensitivity: f64) -> Self {
        Self {
            forward: false,
            backward: false,
            left: false,
            right: false,
            up: false,
            down: false,
            boost: false,
            analog_movement: DVec3::ZERO,
            look_delta: DVec2::ZERO,
            speed,
            look_sensitivity,
            invert_vertical_look: false,
            invert_horizontal_look: false,
            velocity: DVec3::ZERO,
            skip_movement_frame: false,
        }
    }

    pub(crate) fn configure_preferences(&mut self, look_sensitivity: f64, invert_vertical_look: bool, invert_horizontal_look: bool) {
        self.look_sensitivity = look_sensitivity;
        self.invert_vertical_look = invert_vertical_look;
        self.invert_horizontal_look = invert_horizontal_look;
    }

    pub(crate) fn begin_capture(&mut self) {
        self.clear_input();
        self.skip_movement_frame = true;
    }

    pub(crate) fn process_mouse_motion(&mut self, dx: f64, dy: f64) {
        self.look_delta += DVec2::new(dx, dy);
    }

    pub(crate) fn process_key(&mut self, key: KeyCode, pressed: bool) -> bool {
        match key {
            KeyCode::KeyW => self.forward = pressed,
            KeyCode::KeyS => self.backward = pressed,
            KeyCode::KeyA => self.left = pressed,
            KeyCode::KeyD => self.right = pressed,
            KeyCode::KeyE => self.up = pressed,
            KeyCode::KeyQ => self.down = pressed,
            KeyCode::ShiftLeft | KeyCode::ShiftRight => self.boost = pressed,
            _ => return false,
        }
        true
    }

    pub(crate) fn process_scroll(&mut self, delta: &MouseScrollDelta) {
        let steps = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
            MouseScrollDelta::PixelDelta(PhysicalPosition { y, .. }) => *y / 100.0,
        };
        self.adjust_speed(steps);
    }

    pub(crate) fn adjust_speed(&mut self, steps: f64) -> f64 {
        self.speed = (self.speed * 1.25_f64.powf(steps)).clamp(0.01, 1.0e9);
        self.speed
    }

    pub(crate) fn clear_input(&mut self) {
        self.forward = false;
        self.backward = false;
        self.left = false;
        self.right = false;
        self.up = false;
        self.down = false;
        self.boost = false;
        self.analog_movement = DVec3::ZERO;
        self.look_delta = DVec2::ZERO;
        self.velocity = DVec3::ZERO;
        self.skip_movement_frame = false;
    }

    pub(crate) fn has_pending_updates(&self) -> bool {
        self.forward
            || self.backward
            || self.left
            || self.right
            || self.up
            || self.down
            || self.analog_movement != DVec3::ZERO
            || self.look_delta != DVec2::ZERO
            || self.velocity != DVec3::ZERO
    }

    pub(crate) fn update_camera(&mut self, camera: &mut Camera, dt: Duration) {
        let target_distance = camera.position.distance(camera.target);
        let horizontal_sign = if self.invert_horizontal_look { 1.0 } else { -1.0 };
        let vertical_sign = if self.invert_vertical_look { 1.0 } else { -1.0 };
        camera.yaw += self.look_delta.x * self.look_sensitivity * horizontal_sign;
        camera.pitch = (camera.pitch + self.look_delta.y * self.look_sensitivity * vertical_sign).clamp(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
        camera.apply_angle_orientation(target_distance);
        self.look_delta = DVec2::ZERO;

        let forward_amount = f64::from(i8::from(self.forward) - i8::from(self.backward)) + self.analog_movement.y;
        let right_amount = f64::from(i8::from(self.left) - i8::from(self.right)) - self.analog_movement.x;
        let vertical_amount = f64::from(i8::from(self.up) - i8::from(self.down)) + self.analog_movement.z;
        let input_direction = camera.angle_forward() * forward_amount + camera.angle_right() * right_amount + DVec3::Z * vertical_amount;
        let speed_multiplier = if self.boost { 3.0 } else { 1.0 };
        let target_velocity = input_direction.normalize_or_zero() * self.speed * speed_multiplier;
        let movement_dt = if self.skip_movement_frame {
            self.skip_movement_frame = false;
            0.0
        } else {
            dt.as_secs_f64().min(0.1)
        };

        // Exponential interpolation gives the same acceleration feel at any
        // frame rate. Releasing movement keys eases cleanly back to rest.
        let response = if target_velocity == DVec3::ZERO { 12.0 } else { 8.0 };
        let blend = 1.0 - (-response * movement_dt).exp();
        self.velocity = self.velocity.lerp(target_velocity, blend);
        if target_velocity == DVec3::ZERO && self.velocity.length_squared() < 1.0e-8 {
            self.velocity = DVec3::ZERO;
        }
        let movement = self.velocity * movement_dt;

        camera.position += movement;
        camera.target += movement;
    }
}

// Translates a point from pixel coordinates to wgpu NDC coordinates.
// Returns a DVec3 with z=0; only x and y are meaningful for 2D mouse position use.
pub(crate) fn point(x: f32, y: f32, screen: Size) -> DVec3 {
    let scale_x = 2.0 / screen.0 as f64;
    let scale_y = 2.0 / screen.1 as f64;
    let new_x = -1.0 + x as f64 * scale_x;
    let new_y = 1.0 - y as f64 * scale_y;
    DVec3::new(new_x, new_y, 0.)
}

/// Unproject a screen pixel to a world point on the plane `z = plane_z`.
///
/// The projection is orthographic, so every view ray is parallel to the camera
/// `forward` vector: we find the point under the cursor on the camera focal
/// plane, then march along `forward` until it meets the requested Z plane.
/// `zoom` is `Projection::zoom`, which equals the camera-to-target distance.
/// Returns `None` when the view direction is parallel to the plane.
pub(crate) fn screen_to_world_on_plane(camera: &Camera, zoom: f64, aspect: f64, screen: Size, mouse_px: (f32, f32), plane_z: f64) -> Option<DVec3> {
    let rel = point(mouse_px.0, mouse_px.1, screen);
    let forward = camera.forward();
    let right = forward.cross(camera.up()).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    let focal = camera.position + forward * zoom + right * rel.x * aspect * zoom + up * rel.y * zoom;
    if forward.z.abs() <= f64::EPSILON {
        return None;
    }
    let t = (plane_z - focal.z) / forward.z;
    Some(focal + forward * t)
}
