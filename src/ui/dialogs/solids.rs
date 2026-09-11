//! The Solids Setup subpage's New Solid dialog (Solids step).

use crate::{
    i18n::tr,
    model::{SolidKind, block_model::BlockModelId, triangulation::TriangulationId},
    ui::{
        EditorState, UiProjectView,
        state::UiCommand,
        widgets::menu::{self, DragableMenu, MenuButton, MenuFieldCombo, MenuFieldText},
    },
};

pub(crate) fn kind_label(kind: SolidKind) -> String {
    match kind {
        SolidKind::Pit => tr!(literal = "Pit"),
        SolidKind::Dump => tr!(literal = "Dump"),
        SolidKind::Stockpile => tr!(literal = "Stockpile"),
    }
}

/// Label for a triangulation reference: the surface's name, or a placeholder
/// when it is unset or the surface it named is no longer in the project.
pub(crate) fn triangulation_label(project: &UiProjectView, id: Option<TriangulationId>) -> String {
    match id {
        None => tr!(literal = "None"),
        Some(id) => project
            .triangulations
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| tr!(literal = "Missing surface"), |entry| entry.name.clone()),
    }
}

/// Block-model counterpart of [`triangulation_label`].
pub(crate) fn block_model_label(project: &UiProjectView, id: Option<BlockModelId>) -> String {
    match id {
        None => tr!(literal = "None"),
        Some(id) => project
            .block_models
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| tr!(literal = "Missing block model"), |entry| entry.name.clone()),
    }
}

/// Every triangulation in the project, offered after an explicit "None".
pub(crate) fn triangulation_options(project: &UiProjectView) -> Vec<(Option<TriangulationId>, String)> {
    std::iter::once((None, tr!(literal = "None")))
        .chain(project.triangulations.iter().map(|entry| (Some(entry.id), entry.name.clone())))
        .collect()
}

pub(crate) fn block_model_options(project: &UiProjectView) -> Vec<(Option<BlockModelId>, String)> {
    std::iter::once((None, tr!(literal = "None")))
        .chain(project.block_models.iter().map(|entry| (Some(entry.id), entry.name.clone())))
        .collect()
}

/// Draw the dialog that adds one solid to the project.
///
/// A solid is a name, the design surface it is bounded by (a pit shell or a
/// dump design), the topography that surface is measured against, what kind
/// of volume it is, and - for a pit, whose material comes out of the ground -
/// the block model it reserves against. Dumps and stockpiles are placed
/// material, so their model stays optional.
pub(crate) fn draw_new_solid_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    if !editor.new_solid_open {
        return;
    }
    let mut open = true;
    let mut close = false;
    DragableMenu::new("new_solid_dialog", tr!(literal = "New Solid"))
        .open(&mut open)
        .min_width(320.0)
        .show(ui.ctx(), |ui| {
            MenuFieldText::new(tr!(literal = "Name"), &mut editor.new_solid_name)
                .hint_text(tr!(literal = "Required"))
                .show(ui);
            let selected_kind = kind_label(editor.new_solid_kind);
            MenuFieldCombo::new(
                "new_solid_kind",
                tr!(literal = "Type"),
                &mut editor.new_solid_kind,
                selected_kind,
                SolidKind::ALL.into_iter().map(|kind| (kind, kind_label(kind).into())),
            )
            .show(ui);
            let surface_text = triangulation_label(project, editor.new_solid_surface);
            MenuFieldCombo::new(
                "new_solid_surface",
                tr!(literal = "Surface"),
                &mut editor.new_solid_surface,
                surface_text,
                triangulation_options(project).into_iter().map(|(id, name)| (id, name.into())),
            )
            .help_text(tr!(literal = "The pit or dump design itself"))
            .show(ui);
            let topography_text = triangulation_label(project, editor.new_solid_topography);
            MenuFieldCombo::new(
                "new_solid_topography",
                tr!(literal = "Topography"),
                &mut editor.new_solid_topography,
                topography_text,
                triangulation_options(project).into_iter().map(|(id, name)| (id, name.into())),
            )
            .help_text(tr!(literal = "The surface the design is measured against"))
            .show(ui);
            let block_model_text = block_model_label(project, editor.new_solid_block_model);
            MenuFieldCombo::new(
                "new_solid_block_model",
                tr!(literal = "Block Model"),
                &mut editor.new_solid_block_model,
                block_model_text,
                block_model_options(project).into_iter().map(|(id, name)| (id, name.into())),
            )
            .help_text(if editor.new_solid_kind.requires_block_model() {
                tr!(literal = "Reserved against this model")
            } else {
                tr!(literal = "Optional for dumps and stockpiles")
            })
            .show(ui);
            menu::menu_actions(ui, |ui| {
                let can_add = !editor.new_solid_name.trim().is_empty();
                let submitted = menu::dialog_confirm_pressed(ui.ctx());
                if (submitted || ui.add(MenuButton::new(tr!(literal = "Add Solid")).primary().enabled(can_add)).clicked()) && can_add {
                    commands.push(UiCommand::AddSolid {
                        name: editor.new_solid_name.trim().to_owned(),
                        kind: editor.new_solid_kind,
                        surface: editor.new_solid_surface,
                        topography: editor.new_solid_topography,
                        block_model: editor.new_solid_block_model,
                    });
                    close = true;
                }
                if ui.add(MenuButton::new(tr!(literal = "Cancel"))).clicked() || menu::dialog_cancel_pressed(ui.ctx()) {
                    close = true;
                }
            });
        });
    if close || !open {
        editor.new_solid_open = false;
        editor.new_solid_name.clear();
        editor.new_solid_kind = SolidKind::Pit;
        editor.new_solid_surface = None;
        editor.new_solid_topography = None;
        editor.new_solid_block_model = None;
    }
}
