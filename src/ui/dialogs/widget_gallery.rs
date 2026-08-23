//! Debug gallery of the floating-menu widget family.
//!
//! One live example of every widget a dialog can be built from, in the frame
//! they all sit in, so the look of the set can be judged as a set. Debug
//! builds only - it is a workbench, not a feature - and it owns nothing: the
//! values it edits exist to be dragged around and thrown away.

use std::path::PathBuf;

use crate::ui::{
    state::EditorState,
    widgets::menu::{
        self, DragableMenu, MenuButton, MenuField, MenuFieldBool, MenuFieldColor32, MenuFieldCombo, MenuFieldF64, MenuFieldFilePicker, MenuFieldRgba, MenuFieldText, MenuFieldU32,
    },
};

/// The throwaway values the gallery's controls edit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WidgetGalleryState {
    pub(crate) text: String,
    pub(crate) elevation: f64,
    pub(crate) count: u32,
    pub(crate) enabled: bool,
    pub(crate) shape: GalleryShape,
    pub(crate) color: egui::Color32,
    pub(crate) rgba: [f32; 4],
    pub(crate) files: Vec<PathBuf>,
    pub(crate) blend: f32,
    pub(crate) checked: bool,
    pub(crate) side: bool,
}

impl Default for WidgetGalleryState {
    fn default() -> Self {
        Self {
            text: "Pit shell 1".to_owned(),
            elevation: 1275.5,
            count: 12,
            enabled: true,
            shape: GalleryShape::Circle,
            color: egui::Color32::from_rgb(0xD6, 0x6E, 0x1E),
            rgba: [0.27, 0.38, 0.75, 1.0],
            files: Vec::new(),
            blend: 0.35,
            checked: true,
            side: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GalleryShape {
    Circle,
    Square,
    Triangle,
}

impl GalleryShape {
    fn label(self) -> &'static str {
        match self {
            Self::Circle => "Circle",
            Self::Square => "Square",
            Self::Triangle => "Triangle",
        }
    }
}

/// Draw the widget gallery when `editor.widget_gallery_open` is set.
pub(crate) fn draw_widget_gallery(ui: &mut egui::Ui, editor: &mut EditorState) {
    if !editor.widget_gallery_open {
        return;
    }
    let mut open = true;
    let state = &mut editor.widget_gallery;
    DragableMenu::new("Widget Gallery")
        .open(&mut open)
        .min_width(340.0)
        .max_width(360.0)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui.ctx(), |ui| {
            ui.set_max_width(340.0);

            menu::menu_note(ui, "Every menu widget, live. Debug builds only.");

            menu::menu_section(ui, "Fields");
            MenuFieldText::new("Name", &mut state.text).hint_text("Unnamed").show(ui);
            MenuFieldF64::new("Elevation", &mut state.elevation, -10_000.0..=10_000.0)
                .suffix(" m")
                .help_text("Drag to change, or click to type a value.")
                .show(ui);
            MenuFieldU32::new("Segments", &mut state.count, 1..=512).show(ui);
            let shape_label = state.shape.label();
            MenuFieldCombo::new(
                "widget_gallery_shape",
                "Shape",
                &mut state.shape,
                shape_label,
                [GalleryShape::Circle, GalleryShape::Square, GalleryShape::Triangle].map(|shape| (shape, shape.label().into())),
            )
            .show(ui);
            MenuFieldBool::new("Snap to grid", &mut state.enabled)
                .help_text("The sliding toggle used for on/off fields.")
                .show(ui);
            MenuFieldColor32::new("Line colour", &mut state.color).show(ui);
            MenuFieldRgba::new("Fill colour", &mut state.rgba).show(ui);
            MenuFieldFilePicker::new("Source", &state.files).show(ui);
            MenuField::new("Opacity").show(ui, |ui, row_height| {
                ui.add_sized([120.0, row_height], egui::Slider::new(&mut state.blend, 0.0..=1.0).show_value(false))
            });
            // A field row lays its control out from the right, so the pair is
            // added in reverse to read "Above | Below".
            MenuField::new("Side").show(ui, |ui, _| {
                ui.horizontal(|ui| {
                    if ui.add(MenuButton::new("Below").selected(state.side).min_width(58.0)).clicked() {
                        state.side = true;
                    }
                    if ui.add(MenuButton::new("Above").selected(!state.side).min_width(58.0)).clicked() {
                        state.side = false;
                    }
                })
                .response
            });

            menu::menu_section(ui, "Stock controls");
            ui.checkbox(&mut state.checked, "Checkbox");
            ui.horizontal(|ui| {
                ui.label("A label,");
                ui.label(egui::RichText::new("weak text,").weak());
                ui.hyperlink_to("and a link", "https://inclinedesign.net");
            });
            ui.add(egui::ProgressBar::new(state.blend).desired_height(6.0).corner_radius(3.0));

            menu::menu_section(ui, "Buttons");
            ui.horizontal(|ui| {
                ui.add(MenuButton::new("Primary").primary());
                ui.add(MenuButton::new("Secondary"));
                ui.add(MenuButton::new("Danger").danger());
            });
            ui.horizontal(|ui| {
                ui.add(MenuButton::new("Disabled").enabled(false));
                ui.add(MenuButton::new("Disabled").primary().enabled(false));
                ui.add(egui::Button::new("egui Button"));
            });

            menu::menu_section(ui, "Actions");
            menu::menu_actions(ui, |ui| {
                let _ = ui.add(MenuButton::new("Apply").primary());
                let _ = ui.add(MenuButton::new("Cancel"));
                let _ = ui.add(MenuButton::new("Delete").danger());
            });
        });
    if !open {
        editor.widget_gallery_open = false;
    }
}
