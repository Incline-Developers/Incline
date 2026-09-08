//! The Drill & Blast workspace's products panel, down the window's right edge.
//!
//! One region, claimed after the console and the bottom toolbar so it runs
//! from the viewport bar down to the toolbar's top edge and the two strips
//! below carry on underneath it, the way the explorer's column is claimed
//! before them and they stop at its edge.
//!
//! It is built out of the explorer's own parts - the banded rows, the coloured
//! section heading carrying the only symbol in the panel, the lit row for the
//! one that is armed - so the two side panels read as one interface rather
//! than as a tree beside a board of cards. A product fits a row: the delay, in
//! the colour a tie-in laid with it is drawn in, and the name after it.
//! Interhole delays are the only product so far - see
//! [`crate::ui::state::DelayProduct`] - so the palette is one section, and a
//! new kind would be another beside it.
//!
//! Nothing in the panel is a button: a product is chosen by clicking its row,
//! and the palette itself is edited from the section heading's right-click
//! menu, so the list holds products and only products.

use crate::{
    i18n::tr,
    ui::{
        EditorState,
        state::DelayProduct,
        unthemed_icon,
        widgets::{
            context_menu::{ContextMenuAction, context_menu_popup},
            explorer::{ExplorerEntry, ExplorerHeader, explorer_note, paint_fixed_stripes, reserve_fixed_stripes},
        },
    },
};

/// Id of the products panel. Shared with [`crate::ui::chrome`], which reads
/// the panel's resize interaction to light up its grip.
pub(crate) const PANEL_ID: &str = "products_panel";

/// Width the panel opens at: a delay, its name, and the heading above them,
/// without the column being wider than the little it has to say.
const DEFAULT_WIDTH: f32 = 190.0;
/// Narrowest the panel may be dragged. Rows truncate rather than clip.
const MIN_WIDTH: f32 = 140.0;
/// Widest, so dragging it out cannot swallow the scene.
const MAX_WIDTH: f32 = 360.0;

/// Heading tint for the palette: the red the heading's own tie-in icon is
/// drawn in, so the section reads as one mark rather than as a symbol beside
/// a differently coloured name. Like the explorer's tints it is one colour for
/// both themes, holding better than 4:1 against the light panel and the dark
/// one alike.
const HEADER_DELAY_PALETTE: egui::Color32 = egui::Color32::from_rgb(0xE2, 0x3B, 0x46);

/// Space between a product's delay and the name after it.
const LABEL_GAP: f32 = 6.0;

/// Draw the products panel and return what it claimed.
pub(crate) fn draw_products_panel(ui: &mut egui::Ui, editor: &mut EditorState) -> egui::Rect {
    // The explorer's row colours, because these are the explorer's rows: the
    // two side panels share one palette rather than each mixing its own.
    let (surface, stripe) = crate::ui::widgets::tree_row_colors(ui);
    egui::Panel::right(PANEL_ID)
        .resizable(true)
        .default_size(DEFAULT_WIDTH)
        .min_size(MIN_WIDTH)
        .max_size(MAX_WIDTH)
        .show_separator_line(crate::ui::chrome::show_separator_line(ui))
        .frame(crate::ui::chrome::region_frame(ui).fill(surface).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| {
            // Prevent content from forcing the panel wider than the user has dragged it.
            ui.set_max_width(ui.available_width());
            // The palette can outgrow a short viewport. Keep that content
            // inside the height the surrounding bottom panels left us; if it
            // overflows, scroll it rather than allowing the side panel's frame
            // and chrome rect to expand across those panels.
            egui::ScrollArea::vertical()
                .id_salt("products_panel_scroll")
                .auto_shrink([false; 2])
                .min_scrolled_height(0.0)
                .max_height(ui.available_height())
                .show(ui, |ui| {
                    // A name longer than the column ends in an ellipsis rather
                    // than widening the panel.
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    // Rows carry their own height and butt up against each
                    // other, so the banding tiles the list without gaps.
                    ui.spacing_mut().item_spacing.y = 0.0;

                    // Reserved before any row is laid out and filled once the
                    // list's height is known: see `paint_fixed_stripes`.
                    let (stripes_slot, stripes_top) = reserve_fixed_stripes(ui);
                    let (toggle, header, _) = ExplorerHeader::new(egui::Id::new("delay_palette_collapse"), tr!(literal = "Delay Palette"))
                        .icon(unthemed_icon!("tie_holes.svg"))
                        .color(HEADER_DELAY_PALETTE)
                        .show(ui, |ui| draw_delay_palette(ui, editor));
                    // Adding to the palette is editing the palette rather than
                    // one product in it, so it hangs off the section heading -
                    // the way the explorer's own section menus do - instead of
                    // taking a permanent row at the foot of the list.
                    context_menu_popup(&toggle.union(header.inner), tr!(literal = "Delay Palette"), |ui| {
                        if ContextMenuAction::new(tr!(literal = "New Product")).show(ui).clicked() {
                            editor.begin_new_delay_product();
                            ui.close();
                        }
                    });
                    paint_fixed_stripes(ui, stripes_slot, stripes_top, stripe);
                });
        })
        .response
        .rect
}

/// The palette itself: one row per stored product.
///
/// Every row is live whether or not the Tie Holes tool is armed. Which
/// product a tie-in will be laid with is a standing choice, not a state of
/// the tool: it is worth setting before the tool is picked up, and the
/// palette always stands on one - the first, until another is clicked - so
/// arming the tool never lands on nothing.
fn draw_delay_palette(ui: &mut egui::Ui, editor: &mut EditorState) {
    let mut select = None;
    let mut delete = None;

    if editor.delay_products.is_empty() {
        explorer_note(ui, tr!(literal = "No products"));
    }
    // The palette stands on its first product whenever the selection has
    // nothing to point at - a config that never named one, say - so the row
    // a tie-in would use is always a marked one. Settled before the rows are
    // laid out, so the mark appears in the same frame rather than the next.
    if editor.active_delay_product.is_none() {
        editor.active_delay_product = editor.delay_products.first().map(|product| product.id);
    }
    for product in &editor.delay_products {
        let chosen = editor.active_delay_product == Some(product.id);
        // Two marks, because the row has to be readable as chosen at a
        // glance in a list of near-identical rows: the lit fill the explorer
        // gives its active layer, and a dot in the label gutter in the
        // product's own colour - the colour a tie-in laid with it is drawn
        // in. The gutter is reserved on every row, so the dot marks one
        // without shifting the others.
        let mut entry = ExplorerEntry::new(egui::Id::new(("delay_product", product.id)), product_label(ui, product)).selected(chosen);
        if chosen {
            entry = entry.leading_icon(unthemed_icon!("product_active.svg"), product.color);
        }
        let response = entry.show(ui).response;
        if response.clicked() {
            select = Some(product.id);
        }
        let label = format!("{} {}", product.delay_ms, product.name);
        context_menu_popup(&response, label.clone(), |ui| {
            if ContextMenuAction::new(tr!(literal = "Delete Product")).show(ui).clicked() {
                delete = Some((product.id, label.clone()));
                ui.close();
            }
        });
    }

    if let Some(id) = select {
        editor.active_delay_product = Some(id);
    }
    // Deleting a product is not undoable, so it goes through the same
    // confirmation every other destructive delete does; the command is only
    // pushed once that dialog is accepted.
    if let Some(pending) = delete {
        editor.pending_delete_delay_product = Some(pending);
    }
}

/// One product as a row's label: the delay in the product's own colour, with
/// its name after it in the weight the panel gives secondary text.
///
/// Laid out as one [`egui::text::LayoutJob`] rather than as two widgets, so
/// the row stays a single label that carries the click, the context menu and
/// the selection fill, and truncates as a whole when the panel is narrowed.
fn product_label(ui: &egui::Ui, product: &DelayProduct) -> egui::WidgetText {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &product.delay_ms.to_string(),
        0.0,
        egui::TextFormat {
            font_id: crate::ui::fonts::bold_font(font.size),
            color: product.color,
            ..Default::default()
        },
    );
    job.append(
        &product.name,
        LABEL_GAP,
        egui::TextFormat {
            font_id: font,
            color: ui.visuals().weak_text_color(),
            ..Default::default()
        },
    );
    job.into()
}
