//! Sub-modules for individual UI panels, toolbars, and dialog widgets.
//!
//! Each module owns exactly one public `draw_*` function that returns an
//! `egui::Rect` describing the area it occupies.  This makes it easy for
//! `draw_ui()` to compute the canvas rect as the remaining space.
//!
//! [`bar_strip`] is the one piece shared between them: the layout the two bars
//! across the top of the window are laid out on.

pub(crate) mod block_model;
pub(crate) mod console;
pub(crate) mod cursors;
pub(crate) mod explorer;
pub(crate) mod main_menu;
pub(crate) mod products;
pub(crate) mod properties;
pub(crate) mod status_bar;
pub(crate) mod toolbars;
pub(crate) mod viewport_bar;

/// Lay a bar's contents out on a strip that never narrows past what they need,
/// and scroll it under the wheel once the window is narrower than that.
///
/// Blender's headers behave this way: a header that has run out of room stops
/// shrinking rather than letting what is on it run together, and the part that
/// falls off the end is reached by rolling the wheel over the bar. No
/// scrollbar is drawn - there is no room for one in a bar this shallow, and
/// the wheel is how Blender reaches it too - but egui's fade at the strip's
/// ends says which way there is more.
///
/// `add_contents` is handed the strip, which is `height` tall and as wide as
/// the bar or as wide as its contents need, whichever is more, and returns the
/// width they actually needed. That width is only known once they have been
/// laid out, so it is measured into the next frame, the way the viewport bar's
/// centre run is placed from its own last-frame width; a repaint is asked for
/// whenever it moves, so the strip is never left a frame behind what is on it.
///
/// The height is passed in rather than taken from the room the bar has,
/// because only one of the two bars is sized before its contents are: the
/// viewport bar hands down the height its panel was fixed at, the menu bar the
/// row height its own contents are centred on and grow out of.
pub(crate) fn bar_strip(ui: &mut egui::Ui, id_salt: &str, height: f32, add_contents: impl FnOnce(&mut egui::Ui, egui::Rect) -> f32) {
    let width_id = ui.make_persistent_id((id_salt, "needed_width"));
    let needed: f32 = ui.data(|data| data.get_temp(width_id)).unwrap_or(0.0);
    // A wheel with no sideways component still scrolls a strip that can only
    // go sideways, which is what the wheel over a header does in Blender.
    ui.style_mut().always_scroll_the_only_direction = true;
    let measured = egui::ScrollArea::horizontal()
        .id_salt(id_salt)
        .scroll_source(egui::containers::scroll_area::ScrollSource::MOUSE_WHEEL)
        .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden)
        // Across: fill the bar rather than shrinking to the contents, so the
        // clusters have the whole width to place themselves against. Down:
        // follow the contents, or the strip claims every point of window the
        // bar's panel had not already been sized out of.
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let visible = ui.max_rect();
            let strip = egui::Rect::from_min_size(visible.min, egui::vec2(visible.width().max(needed), height));
            // Claim the whole strip, or the scroll area measures only what the
            // contents happened to cover and has nowhere to scroll to.
            ui.allocate_rect(strip, egui::Sense::hover());
            add_contents(ui, strip)
        })
        .inner;
    if (measured - needed).abs() > 0.5 {
        ui.ctx().request_repaint();
    }
    ui.data_mut(|data| data.insert_temp(width_id, measured));
}
