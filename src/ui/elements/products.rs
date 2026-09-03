//! The Drill & Blast workspace's products panel, down the window's right edge.
//!
//! One region, claimed after the console and the bottom toolbar so it runs
//! from the viewport bar down to the toolbar's top edge and the two strips
//! below carry on underneath it, the way the explorer's column is claimed
//! before them and they stop at its edge.
//!
//! What is in it is a palette of cards rather than a list: a product is
//! recognised by its colour and its delay at a glance while a pattern is being
//! tied in, which is what the card shows. Interhole delays are the only
//! product so far - see [`crate::ui::state::DelayProduct`] - so the palette is
//! one section, and a new kind would be another beside it.

use crate::{
    i18n::tr,
    ui::{
        EditorState,
        state::{BlastCursor, DelayProduct, UiCommand},
        widgets::{
            collapsible_section::CollapsibleSection,
            context_menu::{ContextMenuAction, context_menu_popup},
            shifted,
            toolbar::GROUP_CORNER_RADIUS,
        },
    },
};

/// Id of the products panel. Shared with [`crate::ui::chrome`], which reads
/// the panel's resize interaction to light up its grip.
pub(crate) const PANEL_ID: &str = "products_panel";

/// Width the panel opens at: two cards across, with room for the section's
/// own margins.
const DEFAULT_WIDTH: f32 = 216.0;
/// Narrowest the panel may be dragged: one card and its margins.
const MIN_WIDTH: f32 = 128.0;
/// Widest, so dragging it out cannot swallow the scene.
const MAX_WIDTH: f32 = 420.0;

/// Height of one product card. Its three rows - the tie-in mark, the delay,
/// the name - are placed down it from the constants below.
const CARD_HEIGHT: f32 = 74.0;
/// Narrowest a card is laid out at, which is what decides how many fit across
/// the panel.
const MIN_CARD_WIDTH: f32 = 84.0;
/// Gap between one card and the next, across and down.
const CARD_GAP: f32 = 6.0;
/// Distance from a card's top edge to the centre of its tie-in mark.
const MARK_CENTER: f32 = 20.0;
/// ...to the centre of the delay it reads as.
const VALUE_CENTER: f32 = 41.0;
/// ...and to the centre of the name under it.
const NAME_CENTER: f32 = 61.0;
/// Clear space kept either side of a card's contents.
const CARD_INSET: f32 = 9.0;
/// Half the length of one arm of the New Product card's plus.
const PLUS_ARM: f32 = 7.0;

/// Draw the products panel and return what it claimed.
pub(crate) fn draw_products_panel(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::right(PANEL_ID)
        .resizable(true)
        .default_size(DEFAULT_WIDTH)
        .min_size(MIN_WIDTH)
        .max_size(MAX_WIDTH)
        .show_separator_line(crate::ui::chrome::show_separator_line(ui))
        .frame(crate::ui::chrome::region_frame(ui).inner_margin(egui::Margin::same(6)))
        .show(ui, |ui| {
            // A panel is as tall as its contents report, and that height is
            // what its own resize handle stores. Claim the whole column, or
            // the region's fill stops under the last card.
            ui.set_min_height(ui.available_height());
            CollapsibleSection::new("delay_palette", tr!(literal = "Delay Palette"))
                .default_open(true)
                .show(ui, |ui| draw_delay_palette(ui, editor, commands));
        })
        .response
        .rect
}

/// The palette itself: every stored product, then the cell that adds one.
///
/// The cards are packed across whatever width the panel has been dragged to,
/// so the palette is two columns at its default width and reflows rather than
/// clipping when it is narrowed.
///
/// A product is something a click in the scene applies, so the cards are live
/// only while the Tie Holes cursor is the one armed - see
/// [`BlastCursor::TieHoles`]. The New Product cell is not one of them: adding
/// to the palette is editing the palette, which is worth doing before a round
/// is tied in as much as during one.
fn draw_delay_palette(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    let tying = editor.cursors.blast == BlastCursor::TieHoles;
    let available = ui.available_width();
    let columns = (((available + CARD_GAP) / (MIN_CARD_WIDTH + CARD_GAP)).floor() as usize).max(1);
    let card_width = ((available - CARD_GAP * (columns - 1) as f32) / columns as f32).floor().max(1.0);

    // The New Product cell is laid out as one more card, so it fills the row
    // the products leave off in rather than starting a block of its own.
    let cells = editor.delay_products.len() + 1;
    let mut delete = None;
    let mut add = false;

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(CARD_GAP, CARD_GAP);
        for row in 0..cells.div_ceil(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = CARD_GAP;
                for cell in row * columns..((row + 1) * columns).min(cells) {
                    match editor.delay_products.get(cell) {
                        Some(product) => {
                            // `add_enabled_ui` fades what is painted inside it
                            // toward the background, which is the whole of what
                            // a card with nothing to apply it says.
                            if ui.add_enabled_ui(tying, |ui| draw_product_card(ui, product, card_width)).inner {
                                delete = Some(product.id);
                            }
                        }
                        None => add |= draw_new_product_card(ui, card_width),
                    }
                }
            });
        }
    });

    if add {
        editor.begin_new_delay_product();
    }
    if let Some(id) = delete {
        commands.push(UiCommand::DeleteDelayProduct(id));
    }
}

/// Draw one product's card, and report whether its menu asked for it to go.
fn draw_product_card(ui: &mut egui::Ui, product: &DelayProduct, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, CARD_HEIGHT), egui::Sense::click());
    let mut delete = false;
    context_menu_popup(&response, format!("{} ms {}", product.delay_ms, product.name), |ui| {
        if ContextMenuAction::new(tr!(literal = "Delete Product")).show(ui).clicked() {
            delete = true;
            ui.close();
        }
    });

    if !ui.is_rect_visible(rect) {
        return delete;
    }
    paint_card_frame(ui, rect, response.hovered());
    paint_tie_in_mark(ui, rect, product.color);
    let text = ui.visuals().text_color();
    paint_row(ui, rect, VALUE_CENTER, &product.delay_ms.to_string(), 19.0, text);
    paint_row(ui, rect, NAME_CENTER, &product.name.to_uppercase(), 9.0, ui.visuals().weak_text_color());
    delete
}

/// Draw the cell that opens the New Product dialog, and report a click on it.
fn draw_new_product_card(ui: &mut egui::Ui, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, CARD_HEIGHT), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        paint_card_frame(ui, rect, response.hovered());
        // The plus stands where a product's mark and delay both are, so the
        // cell reads as the same shape with nothing in it yet.
        let color = if response.hovered() {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        let center = egui::pos2(rect.center().x, rect.top() + (MARK_CENTER + VALUE_CENTER) / 2.0);
        let stroke = egui::Stroke::new(2.0, color);
        ui.painter().line_segment([center - egui::vec2(PLUS_ARM, 0.0), center + egui::vec2(PLUS_ARM, 0.0)], stroke);
        ui.painter().line_segment([center - egui::vec2(0.0, PLUS_ARM), center + egui::vec2(0.0, PLUS_ARM)], stroke);
        paint_row(ui, rect, NAME_CENTER, &tr!(literal = "New Product").to_uppercase(), 9.0, color);
    }
    response.clicked()
}

/// The card's surface: a step up from the section it sits in, lit while the
/// pointer is over it.
fn paint_card_frame(ui: &egui::Ui, rect: egui::Rect, hovered: bool) {
    let dark = ui.visuals().dark_mode;
    let panel = ui.visuals().panel_fill;
    let fill = shifted(panel, if dark { 14 } else { -6 } + if hovered { 8 } else { 0 });
    let border = shifted(panel, if dark { 30 } else { -34 });
    ui.painter().rect_filled(rect, GROUP_CORNER_RADIUS, fill);
    ui.painter()
        .rect_stroke(rect, GROUP_CORNER_RADIUS, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
}

/// The tie-in mark at the head of a product's card: the run of cord it delays,
/// with the connector itself sitting in the middle of it.
///
/// Drawn rather than iconified because the colour is the product's own, and it
/// is the one thing on the card that is read from across the room.
fn paint_tie_in_mark(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let painter = ui.painter();
    let y = rect.top() + MARK_CENTER;
    let left = rect.left() + CARD_INSET;
    let right = rect.right() - CARD_INSET;
    if right <= left {
        return;
    }
    painter.line_segment([egui::pos2(left, y), egui::pos2(right, y)], egui::Stroke::new(2.0, color));

    // The connector: a bar across the cord, with the arrow that says which way
    // the round travels running into it.
    let bar = rect.center().x + 1.0;
    let stroke = egui::Stroke::new(1.8, color);
    painter.line_segment([egui::pos2(bar, y - 5.0), egui::pos2(bar, y + 5.0)], stroke);
    painter.line_segment([egui::pos2(bar - 6.0, y - 4.0), egui::pos2(bar - 2.0, y)], stroke);
    painter.line_segment([egui::pos2(bar - 2.0, y), egui::pos2(bar - 6.0, y + 4.0)], stroke);
}

/// Paint one of a card's rows of text, centred and clipped to the card.
fn paint_row(ui: &egui::Ui, rect: egui::Rect, offset: f32, text: &str, size: f32, color: egui::Color32) {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: egui::FontId::proportional(size),
            color,
            ..Default::default()
        },
    );
    // A name longer than the card ends in an ellipsis rather than running out
    // over its neighbour.
    job.wrap = egui::text::TextWrapping::truncate_at_width((rect.width() - 2.0 * CARD_INSET).max(0.0));
    let galley = ui.painter().layout_job(job);
    let center = egui::pos2(rect.center().x, rect.top() + offset);
    ui.painter().galley(center - galley.size() / 2.0, galley, color);
}
