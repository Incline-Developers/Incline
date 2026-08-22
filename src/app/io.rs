// Application configuration and editor storage helpers.

use std::io;
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Write, path::PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) fn default_renderer_background_color() -> [f32; 4] {
    crate::rendering::color::hex_to_linear_rgba(0x232c36)
}

pub(crate) const fn default_snap_poll_rate() -> u32 {
    30
}

pub(crate) const fn default_frame_rate_cap() -> u32 {
    144
}

pub(crate) const fn default_resize_frame_rate_cap() -> u32 {
    80
}

pub(crate) const fn default_block_model_interaction_resolution_divisor() -> u32 {
    1
}

pub(crate) const fn default_show_block_model_boundary_highlights() -> bool {
    true
}

pub(crate) const fn default_downscale_raster_previews() -> bool {
    true
}

pub(crate) const fn default_show_world_axis_gizmo() -> bool {
    true
}

pub(crate) const fn default_show_xy_grid() -> bool {
    true
}

pub(crate) const fn default_show_scale_bar() -> bool {
    false
}

pub(crate) const fn default_show_console() -> bool {
    true
}

pub(crate) const fn default_plan_orbit_sensitivity() -> f64 {
    0.003
}

pub(crate) const fn default_plan_zoom_sensitivity() -> f64 {
    0.005
}

pub(crate) const fn default_plan_zoom_towards_cursor() -> bool {
    true
}

pub(crate) const fn default_fly_field_of_view_degrees() -> f64 {
    40.0
}

pub(crate) const fn default_fly_mouse_look_sensitivity() -> f64 {
    0.003
}

pub(crate) const fn default_fly_near_clip_limit() -> f64 {
    0.25
}

pub(crate) const fn default_fly_max_clip_span() -> f64 {
    150_000.0
}

pub(crate) fn finite_clamped(value: f64, min: f64, max: f64, default: f64) -> f64 {
    if value.is_finite() { value.clamp(min, max) } else { default }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Session {
    /// Every native project remembered in the explorer.
    #[serde(default)]
    pub(crate) project_paths: Vec<PathBuf>,
    /// The one native project restored at startup.
    #[serde(default, alias = "active_path")]
    pub(crate) current_project_path: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Config {
    /// Use egui's dark visuals and the dark UI icon set.
    #[serde(default)]
    pub(crate) dark_mode: bool,
    /// Show the console pannel
    #[serde(default = "default_show_console")]
    pub(crate) show_console: bool,
    /// Linear RGBA clear colour used behind the rendered scene.
    #[serde(default = "default_renderer_background_color")]
    pub(crate) renderer_background_color: [f32; 4],
    #[serde(default = "default_snap_poll_rate")]
    pub(crate) snap_poll_rate: u32,
    #[serde(default = "default_frame_rate_cap")]
    pub(crate) frame_rate_cap: u32,
    #[serde(default = "default_resize_frame_rate_cap")]
    pub(crate) resize_frame_rate_cap: u32,
    #[serde(default = "default_block_model_interaction_resolution_divisor")]
    pub(crate) block_model_interaction_resolution_divisor: u32,
    /// Show the view-dependent Fresnel highlight drawn at block-model material
    /// boundaries.
    pub(crate) show_block_model_boundary_highlights: bool,
    /// Bound large GeoTIFF previews to reduce CPU/GPU memory use.
    #[serde(default = "default_downscale_raster_previews")]
    pub(crate) downscale_raster_previews: bool,
    #[serde(default)]
    pub(crate) frame_counter_enabled: bool,
    #[serde(default = "default_show_world_axis_gizmo")]
    pub(crate) show_world_axis_gizmo: bool,
    #[serde(default = "default_show_xy_grid")]
    pub(crate) show_xy_grid: bool,
    /// Show the cartographic distance scale in the viewport.
    #[serde(default = "default_show_scale_bar")]
    pub(crate) show_scale_bar: bool,
    #[serde(default)]
    pub(crate) debug_chunk_coloring: bool,
    #[serde(default)]
    pub(crate) debug_clip_planes: bool,
    #[serde(default = "default_plan_orbit_sensitivity")]
    pub(crate) plan_orbit_sensitivity: f64,
    #[serde(default = "default_plan_zoom_sensitivity")]
    pub(crate) plan_zoom_sensitivity: f64,
    #[serde(default)]
    pub(crate) plan_invert_vertical_look: bool,
    #[serde(default)]
    pub(crate) plan_invert_horizontal_look: bool,
    #[serde(default = "default_plan_zoom_towards_cursor")]
    pub(crate) plan_zoom_towards_cursor: bool,
    #[serde(default = "default_fly_field_of_view_degrees")]
    pub(crate) fly_field_of_view_degrees: f64,
    #[serde(default = "default_fly_mouse_look_sensitivity")]
    pub(crate) fly_mouse_look_sensitivity: f64,
    #[serde(default)]
    pub(crate) fly_invert_vertical_look: bool,
    #[serde(default)]
    pub(crate) fly_invert_horizontal_look: bool,
    #[serde(default = "default_fly_near_clip_limit")]
    pub(crate) fly_near_clip_limit: f64,
    #[serde(default = "default_fly_max_clip_span")]
    pub(crate) fly_max_clip_span: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dark_mode: false,
            show_console: default_show_console(),
            renderer_background_color: default_renderer_background_color(),
            snap_poll_rate: default_snap_poll_rate(),
            frame_rate_cap: default_frame_rate_cap(),
            resize_frame_rate_cap: default_resize_frame_rate_cap(),
            block_model_interaction_resolution_divisor: default_block_model_interaction_resolution_divisor(),
            show_block_model_boundary_highlights: default_show_block_model_boundary_highlights(),
            downscale_raster_previews: default_downscale_raster_previews(),
            frame_counter_enabled: false,
            show_world_axis_gizmo: default_show_world_axis_gizmo(),
            show_xy_grid: default_show_xy_grid(),
            show_scale_bar: default_show_scale_bar(),
            debug_chunk_coloring: false,
            debug_clip_planes: false,
            plan_orbit_sensitivity: default_plan_orbit_sensitivity(),
            plan_zoom_sensitivity: default_plan_zoom_sensitivity(),
            plan_invert_vertical_look: false,
            plan_invert_horizontal_look: false,
            plan_zoom_towards_cursor: default_plan_zoom_towards_cursor(),
            fly_field_of_view_degrees: default_fly_field_of_view_degrees(),
            fly_mouse_look_sensitivity: default_fly_mouse_look_sensitivity(),
            fly_invert_vertical_look: false,
            fly_invert_horizontal_look: false,
            fly_near_clip_limit: default_fly_near_clip_limit(),
            fly_max_clip_span: default_fly_max_clip_span(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_config(config: &Config) -> io::Result<()> {
    let path = data_path("config.toml")?;
    let contents = toml::to_string_pretty(config).map_err(io::Error::other)?;
    write_atomic(&path, contents.as_bytes())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_config() -> io::Result<Config> {
    let contents = fs::read_to_string(data_path("config.toml")?)?;
    toml::from_str(&contents).map_err(io::Error::other)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_session(session: &Session) -> io::Result<()> {
    let path = data_path("last_session.toml")?;
    let contents = toml::to_string_pretty(session).map_err(io::Error::other)?;
    write_atomic(&path, contents.as_bytes())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_session() -> io::Result<Session> {
    let contents = fs::read_to_string(data_path("last_session.toml")?)?;

    let session: Session = toml::from_str(&contents).map_err(io::Error::other)?;

    Ok(session)
}

/// Resolve a path inside the editor's data directory: the platform config
/// directory (`$XDG_CONFIG_HOME`, `~/Library/Application Support`,
/// `%APPDATA%`) under `incline/`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn data_path(relative: &str) -> io::Result<PathBuf> {
    dirs::config_dir()
        .map(|dir| dir.join("incline").join(relative))
        .ok_or_else(|| io::Error::other("no platform config directory"))
}

/// Write through the shared atomic-file helper so a crash, full disk, or a
/// concurrent writer cannot leave a truncated or missing file behind.
#[cfg(not(target_arch = "wasm32"))]
fn write_atomic(path: &std::path::Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::model::atomic_file::write_atomic(path, |file| {
        file.write_all(contents)?;
        Ok(())
    })
    .map_err(io::Error::other)
}

#[cfg(target_arch = "wasm32")]
const WEB_CONFIG_KEY: &str = "incline.config.v1";

#[cfg(target_arch = "wasm32")]
pub(crate) fn save_config(config: &Config) -> io::Result<()> {
    let json = serde_json::to_string(config).map_err(io::Error::other)?;
    let storage = web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .ok_or_else(|| io::Error::other("localStorage is unavailable"))?;
    storage
        .set_item(WEB_CONFIG_KEY, &json)
        .map_err(|error| io::Error::other(format!("localStorage write failed: {error:?}")))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_config() -> io::Result<Config> {
    let storage = web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .ok_or_else(|| io::Error::other("localStorage is unavailable"))?;
    let json = storage
        .get_item(WEB_CONFIG_KEY)
        .map_err(|error| io::Error::other(format!("localStorage read failed: {error:?}")))?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no browser config"))?;
    serde_json::from_str(&json).map_err(io::Error::other)
}
