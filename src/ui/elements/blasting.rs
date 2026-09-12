//! The Blasting step's panel: the blasts of whichever benches are selected.
//!
//! The shapes themselves are derived geometry, drawn over the scene. What
//! this panel is for is reading off and correcting their names, which is the
//! only part of a blast the user owns.

use crate::{
    i18n::tr,
    ui::{
        EditorState,
        state::{BlastShapeRef, RenameTarget, UiCommand},
        widgets::{
            context_menu::{ContextMenuAction, context_menu_popup},
            data_grid::{GridRow, grid_row},
            explorer::explorer_note,
            island::{Island, IslandResponse, Side},
        },
    },
};

const PANEL_ID: &str = "planning_blasting_panel";

pub(crate) fn draw_panel(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) -> IslandResponse<()> {
    Island::new(PANEL_ID, Side::Right)
        .fill(crate::ui::widgets::tree_row_colors(ui).0)
        .default_width(260.0)
        .min_width(200.0)
        .max_width(420.0)
        .show(ui, |ui, _| {
            ui.set_min_height(ui.available_height());
            ui.label(crate::ui::fonts::bold(&tr!("planning-blasts")));
            ui.separator();
            if editor.blasting_outlines.is_empty() {
                explorer_note(ui, tr!("planning-blasts-empty"));
                return;
            }
            // The tools grey out without one bench to draw on - see
            // `EditorState::blasting_bench` - so say so rather than leaving
            // the user to work out why the column is dead.
            if editor.blasting_bench().is_none() {
                explorer_note(ui, tr!("planning-blasts-one-bench"));
            }
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            egui::ScrollArea::vertical().auto_shrink([false; 2]).min_scrolled_height(0.0).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut last_bench = None;
                for outline in &editor.blasting_outlines {
                    // Numbering restarts per bench, so a blast only reads
                    // unambiguously underneath the RL it belongs to.
                    if last_bench != Some(outline.bench_base.to_bits()) {
                        last_bench = Some(outline.bench_base.to_bits());
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(ui.spacing().indent);
                            ui.label(crate::ui::fonts::bold(&super::solids_view::format_rl(outline.bench_base)));
                        });
                    }
                    let label = tr!("planning-blast-row", name = outline.name.clone(), area = format!("{:.0}", outline.area));
                    let blast = BlastShapeRef::new(outline.solid, outline.bench_base, anchor_of(outline));
                    let response = grid_row(ui, GridRow::new(&label).selected(editor.selected_blast == Some(blast)));
                    if editor.scroll_to_blast && editor.selected_blast == Some(blast) {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                    if response.clicked() {
                        commands.push(UiCommand::SelectBlast(Some(blast)));
                    }
                    if response.double_clicked() {
                        commands.push(UiCommand::BeginRenameItem(RenameTarget::BlastShape(blast)));
                    }
                    response.clone().on_hover_text(tr!(literal = "Double-click to rename"));
                    context_menu_popup(&response, &outline.name, |ui| {
                        if ContextMenuAction::new(tr!(literal = "Rename Blast")).show(ui).clicked() {
                            commands.push(UiCommand::BeginRenameItem(RenameTarget::BlastShape(blast)));
                            ui.close();
                        }
                        if ContextMenuAction::new(tr!(literal = "Reset Blast Name")).show(ui).clicked() {
                            commands.push(UiCommand::ResetBlastName(blast));
                            ui.close();
                        }
                    });
                }
            });
            editor.scroll_to_blast = false;
        })
}

/// The anchor the derivation stored for this outline. Recomputing it here
/// would risk disagreeing with the stored one over a rounding step, so the
/// outline carries the point it was matched on.
fn anchor_of(outline: &crate::ui::state::BlastOutline) -> [f64; 2] {
    outline.anchor
}
