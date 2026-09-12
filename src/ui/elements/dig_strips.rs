//! The dig-block list and flitch-to-flitch strip clipboard.
use crate::{
    i18n::tr,
    ui::{
        EditorState,
        state::{BlastShapeRef, UiCommand},
        widgets::{
            data_grid::{GridRow, grid_row},
            explorer::explorer_note,
            island::{Island, IslandResponse, Side},
        },
    },
};

const PANEL_ID: &str = "planning_dig_blocks";

pub(crate) fn draw_panel(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) -> IslandResponse<()> {
    Island::new(PANEL_ID, Side::Right)
        .fill(crate::ui::widgets::tree_row_colors(ui).0)
        .default_width(260.0)
        .min_width(200.0)
        .max_width(420.0)
        .show(ui, |ui, _| {
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
}
