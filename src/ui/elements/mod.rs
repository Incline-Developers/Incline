//! Sub-modules for individual UI panels, toolbars, and dialog widgets.
//!
//! Each module owns exactly one public `draw_*` function that returns an
//! `egui::Rect` describing the area it occupies.  This makes it easy for
//! `draw_ui()` to compute the canvas rect as the remaining space.

pub(crate) mod block_model;
pub(crate) mod console;
pub(crate) mod cursors;
pub(crate) mod explorer;
#[cfg(not(target_os = "macos"))]
pub(crate) mod main_menu;
pub(crate) mod preferences;
pub(crate) mod status_bar;
pub(crate) mod toolbars;
