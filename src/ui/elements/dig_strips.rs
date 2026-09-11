//! The dig-block list and flitch-to-flitch strip clipboard.
use crate::{
    i18n::tr,
    ui::{
        EditorState, chrome,
        state::{BlastShapeRef, UiCommand},
        widgets::{
            data_grid::{GridRow, grid_row},
            explorer::explorer_note,
        },
    },
};

pub(crate) fn draw_panel(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::Panel::right("planning_dig_blocks")
        .resizable(true)
        .default_size(260.0)
        .min_size(200.0)
        .max_size(420.0)
        .show_separator_line(chrome::show_separator_line(ui))
        .frame(chrome::region_frame(ui))
        .show(ui, |ui| {
            ui.label(crate::ui::fonts::bold(&tr!("planning-dig-blocks")));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!editor.selected_handles.is_empty(), egui::Button::new(tr!("planning-dig-copy")))
                    .on_hover_text(tr!(literal = "Ctrl+C"))
                    .clicked()
                {
                    commands.push(UiCommand::CopyDigStrips);
                }
                if ui
                    .add_enabled(
                        !editor.dig_clipboard.is_empty() && editor.planning_cut_target().is_some(),
                        egui::Button::new(tr!("planning-dig-paste")),
                    )
                    .on_hover_text(tr!(literal = "Ctrl+V"))
                    .clicked()
                {
                    commands.push(UiCommand::PasteDigStrips);
                }
            });
            ui.separator();
            if editor.planning_cut_target().is_none() {
                explorer_note(ui, tr!("planning-dig-select-flitch"));
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                for block in &editor.dig_outlines {
                    let id = BlastShapeRef::new(block.solid, block.bench_base, block.anchor);
                    let label = format!("{} · {:.0} m²", block.name, block.area);
                    if grid_row(ui, GridRow::new(&label).selected(editor.selected_dig_block == Some(id))).clicked() {
                        commands.push(UiCommand::SelectDigBlock(id));
                    }
                }
            });
        })
        .response
        .rect
}
