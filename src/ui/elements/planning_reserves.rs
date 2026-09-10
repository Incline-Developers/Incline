//! Session-only reserves layout scaffold. Sample records do not modify project data.
use crate::{
    i18n::tr,
    ui::{
        chrome,
        widgets::data_grid::PropertyTable,
    },
};

pub(crate) const PANEL_ID: &str = "planning_reserves_data";

fn records() -> [String; 3] {
    [tr!("planning-sample-reserve"), tr!("planning-sample-dump"), tr!("planning-sample-stockpile")]
}

fn selected(ui: &egui::Ui) -> usize {
    ui.data(|data| data.get_temp::<usize>(egui::Id::new("planning_reserve_selected"))).unwrap_or(0).min(2)
}

pub(crate) fn draw_data_panel(ui: &mut egui::Ui) -> egui::Rect {
    egui::Panel::right(PANEL_ID)
        .resizable(true)
        .default_size(330.0)
        .min_size(240.0)
        .max_size(520.0)
        .show_separator_line(chrome::show_separator_line(ui))
        .frame(chrome::region_frame(ui))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.label(egui::RichText::new(tr!("planning-placeholder")).weak());
            let index = selected(ui);
            let names = records();
            let values = [
                ["64,673,589", "85,195,126", "92,657,244", "48.75", "2,222.42", "1,251.98", "778.50"],
                ["32,450,000", "0", "58,410,000", "0.00", "3,140.00", "1,860.00", "720.00"],
                ["1,250,000", "2,187,500", "0", "52.30", "1,820.00", "940.00", "765.00"],
            ][index];
            PropertyTable::new("reserves_data_table", ui.available_rect_before_wrap(), &names[index]).show(ui, |rows| {
                rows.header(&tr!("planning-structure"), &tr!("planning-value"));
                rows.readonly(&tr!("planning-name"), &names[index], None, None);
                rows.readonly(&tr!("planning-data-source"), &tr!("planning-sample-data"), None, None);
                rows.header(&tr!("planning-mining"), &tr!("planning-value"));
                for (key, value, unit) in [
                    (tr!("planning-volume"), values[0], "m³"),
                    (tr!("planning-ore-tonnes"), values[1], "t"),
                    (tr!("planning-waste-tonnes"), values[2], "t"),
                    (tr!("planning-grade"), values[3], "%"),
                ] {
                    rows.readonly(&key, value, Some(unit), None);
                }
                rows.header(&tr!("planning-centroid"), &tr!("planning-value"));
                for (key, value) in [("X", values[4]), ("Y", values[5]), ("Z", values[6])] {
                    rows.readonly(key, value, Some("m"), None);
                }
            });
        })
        .response
        .rect
}
