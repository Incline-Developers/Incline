//! Shared layout for the Solids and Schedule Setup subpages.
//!
//! The step tree and the content category list are wired up; item rows and
//! property fields are still scaffold — they render their empty grids so the
//! layout is real, and fill in with the feature. The grids themselves are the
//! reusable [`data_grid`](crate::ui::widgets::data_grid) widgets.
use crate::{
    i18n::tr,
    ui::{
        chrome,
        fonts::bold,
        state::PlanningPage,
        unthemed_icon,
        widgets::{
            context_menu::{ContextMenuAction, context_menu_popup},
            data_grid::{DataGrid, GridRow, PropertyTable, grid_row},
            explorer::{ExplorerEntry, ExplorerHeader, paint_fixed_stripes, reserve_fixed_stripes},
        },
    },
};

fn striped_list(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
    egui::ScrollArea::vertical().auto_shrink([false; 2]).min_scrolled_height(0.0).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        let (slot, top) = reserve_fixed_stripes(ui);
        content(ui);
        paint_fixed_stripes(ui, slot, top, crate::ui::widgets::tree_row_colors(ui).1);
    });
}

pub(crate) fn draw_steps(ui: &mut egui::Ui, page: PlanningPage) {
    let selection_id = egui::Id::new(("planning_configuration_selected", page));
    let mut configuration = ui.data(|data| data.get_temp::<bool>(selection_id)).unwrap_or(false);
    striped_list(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            if ExplorerEntry::new(egui::Id::new("planning_configuration"), bold(&tr!("planning-configuration")))
                .leading_icon(unthemed_icon!("step_complete.svg"), egui::Color32::WHITE)
                .header_aligned_icon()
                .selected(configuration)
                .show(ui)
                .response
                .clicked()
            {
                configuration = true;
            }
        });
        ExplorerHeader::new(egui::Id::new("planning_site_data"), tr!("planning-site-data"))
            .icon(unthemed_icon!("step_pending.svg"))
            .show(ui, |ui| {
                if ExplorerEntry::new(egui::Id::new("planning_site_contents"), bold(&tr!("planning-content")))
                    .leading_icon(unthemed_icon!("step_pending.svg"), egui::Color32::WHITE)
                    .header_aligned_icon()
                    .selected(!configuration)
                    .show(ui)
                    .response
                    .clicked()
                {
                    configuration = false;
                }
            });
    });
    ui.data_mut(|data| data.insert_temp(selection_id, configuration));
}

fn category_labels() -> [String; 4] {
    [tr!("planning-dumps"), tr!("planning-stockpiles"), tr!("planning-loaders"), tr!("planning-trucks")]
}

fn draw_content_categories(ui: &mut egui::Ui, rect: egui::Rect, page: PlanningPage) -> usize {
    let category_id = egui::Id::new(("planning_site_category", page));
    let mut category = ui.data(|data| data.get_temp::<usize>(category_id)).unwrap_or(0).min(3);
    DataGrid::new("planning_categories", rect, &tr!("planning-content"))
        .column_header(&tr!("planning-content-type"))
        .show(ui, |ui| {
            for (index, label) in category_labels().iter().enumerate() {
                if grid_row(ui, GridRow::new(label).selected(category == index)).clicked() {
                    category = index;
                }
            }
        });
    ui.data_mut(|data| data.insert_temp(category_id, category));
    category
}

fn draw_items(ui: &mut egui::Ui, rect: egui::Rect, category: usize) {
    let label = &category_labels()[category];
    let new_label = match category {
        0 => tr!("planning-new-dump"),
        1 => tr!("planning-new-stockpile"),
        2 => tr!("planning-new-loader"),
        _ => tr!("planning-new-truck"),
    };
    DataGrid::new("planning_items", rect, label).column_header(&tr!("planning-name")).show(ui, |ui| {
        // No item rows yet; the whole body is free space that offers "New ...".
        let body = ui.available_rect_before_wrap();
        if body.is_positive() {
            let response = ui.interact(body, ui.id().with(("new_item_space", category)), egui::Sense::click());
            context_menu_popup(&response, label, |ui| {
                ContextMenuAction::new(new_label.clone()).enabled(false).show(ui);
            });
        }
    });
}

fn draw_item_properties(ui: &mut egui::Ui, rect: egui::Rect) {
    PropertyTable::new("planning_properties", rect, &tr!("planning-properties")).show(ui, |rows| {
        rows.header(&tr!("planning-property"), &tr!("planning-value"));
        // Fields arrive once an item is selectable.
    });
}

fn draw_configuration(ui: &mut egui::Ui, rect: egui::Rect, page: PlanningPage) {
    let name_id = egui::Id::new(("planning_schedule_name", page));
    let mut schedule_name = ui.data(|data| data.get_temp::<String>(name_id)).unwrap_or_default();
    PropertyTable::new("planning_configuration", rect, &tr!("planning-configuration")).show(ui, |rows| {
        rows.header(&tr!("planning-property"), &tr!("planning-value"));
        rows.field(&tr!("planning-schedule-name"), &mut schedule_name, None);
    });
    ui.data_mut(|data| data.insert_temp(name_id, schedule_name));
}

pub(crate) fn draw_details(ui: &mut egui::Ui, page: PlanningPage) -> egui::Rect {
    egui::CentralPanel::default()
        .frame(chrome::region_frame(ui))
        .show(ui, |ui| {
            let configuration = ui
                .data(|data| data.get_temp::<bool>(egui::Id::new(("planning_configuration_selected", page))))
                .unwrap_or(false);
            let available = ui.available_rect_before_wrap();
            let area = available.shrink2(egui::vec2(0.0, 10.0_f32.min(available.height() * 0.5)));
            if configuration {
                let table = egui::Rect::from_min_size(area.min, egui::vec2(area.width().min(520.0), area.height()));
                draw_configuration(ui, table, page);
                ui.allocate_rect(area, egui::Sense::hover());
                return;
            }
            let gap = 12.0;
            let column_space = (area.width() - gap * 2.0).max(0.0);
            let left_width = (column_space * 0.25).min(260.0);
            let left = egui::Rect::from_min_max(area.min, egui::pos2(area.left() + left_width, area.bottom()));
            let middle = egui::Rect::from_min_size(egui::pos2(left.right() + gap, area.top()), egui::vec2((column_space * 0.32).min(360.0), area.height()));
            let properties_origin = egui::pos2(middle.right() + gap, area.top());
            let properties = egui::Rect::from_min_size(properties_origin, egui::vec2((area.right() - properties_origin.x).clamp(0.0, 440.0), area.height()));

            let category = draw_content_categories(ui, left, page);
            draw_items(ui, middle, category);
            draw_item_properties(ui, properties);
            ui.allocate_rect(area, egui::Sense::hover());
        })
        .response
        .rect
}
