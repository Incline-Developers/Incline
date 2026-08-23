//! Custom egui widget implementations used across the UI.

pub(crate) mod context_menu;
pub(crate) mod explorer;
pub(crate) mod menu;
pub(crate) mod toolbar;
pub(crate) mod viewport;

/// Step a colour by `delta` levels per channel.
///
/// Panel colours sit near the ends of the range, where a gamma-space lerp
/// toward black or white moves too little to be predictable. Counting levels
/// gives the same visible step in either theme.
pub(crate) fn shifted(color: egui::Color32, delta: i16) -> egui::Color32 {
    let shift = |channel: u8| (i16::from(channel) + delta).clamp(0, 255) as u8;
    egui::Color32::from_rgb(shift(color.r()), shift(color.g()), shift(color.b()))
}

/// Base and alternating row colours for the explorer tree.
///
/// The base rows take the properties panel's background, so the two halves of
/// the side panel share one palette; the banded rows sit just below it. The
/// banding is there to carry the eye along a row, not to draw stripes, so the
/// step is a handful of levels. The console keeps its own stronger pairing -
/// it is a log, not part of this panel.
pub(crate) fn tree_row_colors(ui: &egui::Ui) -> (egui::Color32, egui::Color32) {
    let surface = ui.visuals().panel_fill;
    (surface, shifted(surface, if ui.visuals().dark_mode { -5 } else { -6 }))
}

/// Fill for the properties panel's tab column.
///
/// A deeper step than the tree's banding: the column is a separate surface
/// rather than a row, and has to read as recessed beside the tab it selects.
pub(crate) fn recessed_chrome_fill(ui: &egui::Ui) -> egui::Color32 {
    shifted(ui.visuals().panel_fill, if ui.visuals().dark_mode { -10 } else { -12 })
}
