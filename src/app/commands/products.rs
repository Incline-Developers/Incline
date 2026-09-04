//! The Drill & Blast workspace's stored products.
//!
//! Interhole delays are the only kind so far. They belong to the palette
//! rather than to any one project's data, so they live in `EditorState`
//! alongside the rest of the interaction state and are mutated here, through
//! the command round-trip, rather than by the panel that draws them.

use crate::{
    app::{App, commands::view::config_from},
    i18n::{tr, tr_format},
    ui::state::{DelayProduct, DelayProductId},
    userspace_log, userspace_warn,
};

impl App<'_> {
    /// Add a product to the palette, at the end of what is already there.
    pub(crate) fn add_delay_product(&mut self, delay_ms: u32, name: String, color: egui::Color32) {
        let id = DelayProductId(self.editor.next_delay_product_id);
        self.editor.next_delay_product_id += 1;
        userspace_log!("{}", tr_format!(literal = "Added product %delay_ms% ms %name%", delay_ms = delay_ms, name = name.clone()));
        self.editor.delay_products.push(DelayProduct { id, delay_ms, name, color });
        // The palette reads as a scale from the shortest delay to the longest,
        // so a new product takes its place in that run rather than landing at
        // the end of it. Stable, so two on the same delay stay in the order
        // they were added.
        self.editor.delay_products.sort_by_key(|product| product.delay_ms);
        // A product is added to be used: it becomes the one a tie-in is laid
        // with, which is also what fills the palette's selection when the
        // first product arrives.
        self.editor.active_delay_product = Some(id);
        self.persist_delay_products();
    }

    /// Drop one stored product from the palette.
    pub(crate) fn delete_delay_product(&mut self, id: DelayProductId) {
        let Some(index) = self.editor.delay_products.iter().position(|product| product.id == id) else {
            userspace_warn!("{}", tr!(literal = "That product is no longer in the palette"));
            return;
        };
        let product = self.editor.delay_products.remove(index);
        userspace_log!(
            "{}",
            tr_format!(literal = "Deleted product %delay_ms% ms %name%", delay_ms = product.delay_ms, name = product.name.clone())
        );
        // The selection cannot stand on a card that has gone; the palette
        // falls back to its first, and to nothing while it is empty.
        if self.editor.active_delay_product == Some(id) {
            self.editor.active_delay_product = self.editor.delay_products.first().map(|product| product.id);
        }
        self.persist_delay_products();
    }

    /// Write the palette back to the config file.
    ///
    /// The whole file is rewritten from the preferences as they stand, so a
    /// product added here cannot roll back a setting changed since startup -
    /// see [`config_from`].
    fn persist_delay_products(&mut self) {
        let preferences = self.editor.current_preferences();
        let products = self.editor.delay_products.iter().map(DelayProduct::to_stored).collect();
        if let Err(error) = crate::app::io::save_config(&config_from(&preferences, products)) {
            userspace_warn!("{}", tr_format!(literal = "Failed to save products: %error%", error = error));
        }
    }
}
