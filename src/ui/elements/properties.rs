//! The explorer's properties panel: application settings plus the properties
//! of whatever is selected, switched between by the icon strip at its top.
//!
//! Sections are [`PropertyTab`]s. The four settings tabs and generic Object tab
//! are always present. Settings apply live - every committed edit becomes an
//! [`UiCommand::ApplyPreferences`], which saves the config file - so there is
//! no draft to confirm or discard. The Block Model, Triangulation and Design
//! tabs describe the last non-empty selection and only appear while there is
//! something for them to describe; clearing the selection leaves them in
//! place, as Blender's properties editor does with its active object.

use thousands::Separable;

use crate::{
    i18n::{tr, tr_format},
    model::{Document, FillStyle, ObjectColor, ObjectId, SceneEntityId, block_model::OpenBlockModel, triangulation::TriangulationId},
    rendering::color::{byte_to_linear_rgba, color32_to_rgba, linear_to_srgb_byte, rgba_to_color32},
    ui::{
        UiCommand, UiProjectView,
        state::{EditorState, PreferencesDraft, PropertyTab},
        themed_icon, unthemed_icon,
        widgets::{
            collapsible_section::CollapsibleSection,
            menu::{self, MenuFieldBool, MenuFieldColor32, MenuFieldCombo, MenuFieldF32, MenuFieldF64, MenuFieldU32, menu_field_label},
            viewport::BlockModelProperties,
        },
    },
};

/// Width of the tab column down the panel's right edge.
const TAB_COLUMN_WIDTH: f32 = 28.0;
/// Height of one tab button.
const TAB_BUTTON_HEIGHT: f32 = 26.0;
/// Gap between the settings tabs and the selection tabs.
const TAB_GROUP_GAP: f32 = 10.0;
/// Width the labelled fields give their control, leaving room for the label in
/// a panel far narrower than a dialog.
/// Shortest the panel may be dragged, and the height it keeps when the side
/// panel is too short to honour [`MIN_TREE_HEIGHT`] as well.
const MIN_HEIGHT: f32 = 120.0;
/// Tree height the panel always leaves above itself - roughly three rows, so
/// the sections stay reachable however short the window gets.
const MIN_TREE_HEIGHT: f32 = 72.0;

/// Effective outer-height limits for the properties panel in the available
/// workspace. Kept public within the UI so the console and bottom-toolbar
/// stack can stop at the same two horizontal lines.
pub(crate) fn height_limits(available_height: f32) -> (f32, f32) {
    let min = MIN_HEIGHT.min(available_height);
    let max = (available_height - MIN_TREE_HEIGHT).max(min).min(available_height);
    (min, max)
}

/// What the panel can show this frame, given the current selection.
struct PropertyContext {
    /// Scene entities described by the generic Object tab.
    entities: Vec<SceneEntityId>,
    /// Selected block model to describe, if any.
    block_model: Option<crate::model::block_model::BlockModelId>,
    /// Selected triangulation to describe, if any.
    triangulation: Option<TriangulationId>,
    /// Selected document objects, in selection order.
    objects: Vec<ObjectId>,
    /// The subset of `objects` that are polylines.
    polylines: Vec<ObjectId>,
}

impl PropertyContext {
    fn tab_available(&self, tab: PropertyTab) -> bool {
        match tab {
            PropertyTab::BlockModel => self.block_model.is_some(),
            PropertyTab::Triangulation => self.triangulation.is_some(),
            PropertyTab::Design => !self.objects.is_empty(),
            _ => true,
        }
    }
}

/// Id of the properties panel. Shared with [`crate::ui::chrome`], which reads
/// the panel's resize interaction to light up its grip.
pub(crate) const PANEL_ID: &str = "explorer_properties";

/// Draw the properties panel at the bottom of the explorer's side panel.
///
/// A region of its own rather than the foot of the tree's: the two are
/// separate surfaces, so the gap and the grip between them say the seam can be
/// dragged. Returns the panel's bounding rect.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_properties(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    project: &UiProjectView,
    block_models: &[OpenBlockModel],
    document: &Document,
    commands: &mut Vec<UiCommand>,
    geometry_dirty: &mut bool,
) -> egui::Rect {
    let context = collect_context(editor, project, block_models, document);
    // A selection tab whose selection has gone shows settings meanwhile, but
    // stays the chosen tab: selecting another block model or design brings it
    // straight back rather than making the user pick the icon again.
    let shown_tab = if context.tab_available(editor.active_property_tab) {
        editor.active_property_tab
    } else {
        PropertyTab::Object
    };

    // The tab column sits on the tree's darker list surface; the properties
    // themselves sit on the lighter panel fill, and the selected tab is filled
    // with that same colour so it reads as continuous with its contents.
    let content_fill = ui.visuals().panel_fill;
    let column_fill = crate::ui::widgets::recessed_chrome_fill(ui);

    // A third of the side panel by default: enough for the longer settings
    // tabs and the block-model colour ramp without the tree feeling squeezed.
    // Only the first frame uses this; after that the panel remembers its
    // dragged size.
    let available_height = ui.available_height();
    let (min_height, max_height) = height_limits(available_height);
    let default_height = (available_height / 3.0).clamp(180.0, 640.0).clamp(min_height, max_height);
    // Never take the last of the tree's height: a short window would otherwise
    // leave it with a sliver the section headers spill straight out of.
    egui::Panel::bottom(PANEL_ID)
        .resizable(true)
        .default_size(default_height)
        .min_size(min_height)
        .max_size(max_height)
        .show_separator_line(crate::ui::chrome::show_separator_line(ui))
        .frame(crate::ui::chrome::region_frame(ui).fill(content_fill).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| {
            // A panel ends up as tall as its contents report, and that height
            // is what gets stored as its resizable size. Claim the whole
            // allotted height so the panel keeps its default rather than
            // collapsing onto the shortest tab's fields.
            // A panel paints its frame around the rect its contents claim, and
            // that rect is also what its resize handle stores. Claim the whole
            // allotted height, or the fill stops partway down and the tree's
            // darker surface shows through beneath the fields.
            ui.set_min_height(ui.available_height());
            egui::Panel::right("explorer_properties_tabs")
                .exact_size(TAB_COLUMN_WIDTH)
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::NONE.fill(column_fill))
                .show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    draw_tab_strip(ui, editor, shown_tab, &context, content_fill);
                });

            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                // `both`, not `vertical`: a panel dragged narrower than the
                // widest control scrolls sideways to reach it. With vertical-only
                // scrolling egui clips horizontal overflow to the *panel* edge,
                // not the content area, so overwide rows paint over the tab column.
                egui::ScrollArea::both().id_salt("explorer_properties_scroll").auto_shrink([false; 2]).show(ui, |ui| {
                    // Leave room for the floating scrollbar so the right-aligned
                    // controls are never tucked underneath it.
                    ui.set_max_width((ui.available_width() - 10.0).max(1.0));
                    // Headings and field labels get whatever their row's control
                    // leaves them, which in a panel this narrow is regularly less
                    // than the text: end it in an ellipsis rather than wrapping
                    // the fields apart.
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    // The panel is built from the same menu fields the floating
                    // menus are, so it takes the same control styling - keyed to
                    // the panel's own surface rather than a card's.
                    crate::ui::widgets::menu::apply_menu_style(ui, content_fill);
                    match shown_tab {
                        PropertyTab::Interface => draw_interface_settings(ui, editor, commands),
                        PropertyTab::Camera => draw_camera_settings(ui, editor, commands),
                        PropertyTab::Performance => draw_performance_settings(ui, editor, commands),
                        PropertyTab::Developer => draw_developer_settings(ui, editor, commands),
                        PropertyTab::Object => draw_object_tab(ui, editor, project, block_models, document, &context),
                        PropertyTab::BlockModel => draw_block_model_tab(ui, editor, block_models, &context, commands),
                        PropertyTab::Triangulation => draw_triangulation_tab(ui, project, &context, commands, geometry_dirty),
                        PropertyTab::Design => draw_design_tab(ui, editor, document, &context, commands, geometry_dirty),
                    }
                });
            });
        })
        .response
        .rect
}

fn collect_context(editor: &mut EditorState, project: &UiProjectView, block_models: &[OpenBlockModel], document: &Document) -> PropertyContext {
    // Only a selection that holds something replaces what the panel describes.
    // Clearing the selection leaves the last one in place, so the tabs stay
    // where they were instead of dropping the user back onto the settings -
    // the same way Blender's properties editor keeps describing the active
    // object after everything is deselected.
    if !editor.selected_handles.is_empty() {
        editor.property_entities = editor.selected_handles.iter().copied().collect();
        editor.property_entities.sort_by_key(|entity| match entity {
            SceneEntityId::Object(id) => (0, id.0),
            SceneEntityId::Triangulation(id) => (1, id.0),
            SceneEntityId::BlockModel(id) => (2, id.0),
            SceneEntityId::PointCloud(id) => (3, id.0),
            SceneEntityId::DrillHole(id) => (4, id.0),
        });
        // Remember which model the tab describes, so a multi-model selection
        // does not switch between them as the set is re-walked.
        editor.viewport_block_model_id = block_models
            .iter()
            .find(|model| editor.viewport_block_model_id == Some(model.id) && editor.selected_handles.contains(&SceneEntityId::BlockModel(model.id)))
            .or_else(|| block_models.iter().find(|model| editor.selected_handles.contains(&SceneEntityId::BlockModel(model.id))))
            .map(|model| model.id);
        editor.viewport_triangulation_id = project
            .triangulations
            .iter()
            .find(|tri| editor.viewport_triangulation_id == Some(tri.id) && editor.selected_handles.contains(&SceneEntityId::Triangulation(tri.id)))
            .or_else(|| {
                project
                    .triangulations
                    .iter()
                    .find(|tri| editor.selected_handles.contains(&SceneEntityId::Triangulation(tri.id)))
            })
            .map(|tri| tri.id);
        editor.property_objects = editor
            .selected_handles
            .iter()
            .filter_map(|handle| match handle {
                SceneEntityId::Object(id) => Some(*id),
                _ => None,
            })
            .collect();
    }

    // What is remembered outlives the selection, but not the thing itself: a
    // deleted object or a closed model stops being described.
    editor.viewport_block_model_id = editor
        .viewport_block_model_id
        .filter(|id| block_models.iter().any(|model| model.id == *id && model.state.loaded));
    editor.viewport_triangulation_id = editor
        .viewport_triangulation_id
        .filter(|id| project.triangulations.iter().any(|tri| tri.id == *id && tri.is_loaded));
    editor.property_objects.retain(|&id| document.get_object(id).is_some());
    editor.property_entities.retain(|entity| match entity {
        SceneEntityId::Object(id) => document.get_object(*id).is_some(),
        SceneEntityId::Triangulation(id) => project.triangulations.iter().any(|item| item.id == *id && item.is_loaded),
        SceneEntityId::BlockModel(id) => block_models.iter().any(|item| item.id == *id && item.state.loaded),
        SceneEntityId::PointCloud(id) => project.point_clouds.iter().any(|item| item.id == *id && item.is_loaded),
        SceneEntityId::DrillHole(id) => project.drill_holes.iter().any(|item| item.id == *id && item.is_loaded),
    });

    let objects = editor.property_objects.clone();
    let polylines = objects
        .iter()
        .copied()
        .filter(|&id| matches!(document.get_object(id), Some(crate::model::Object::Polyline { .. })))
        .collect();

    PropertyContext {
        entities: editor.property_entities.clone(),
        block_model: editor.viewport_block_model_id,
        triangulation: editor.viewport_triangulation_id,
        objects,
        polylines,
    }
}

fn draw_tab_strip(ui: &mut egui::Ui, editor: &mut EditorState, shown_tab: PropertyTab, context: &PropertyContext, content_fill: egui::Color32) {
    // No leading space: the first tab starts flush with the panel's top edge,
    // so its notch meets the separator above it when it is the selected one.
    ui.spacing_mut().item_spacing.y = 1.0;
    tab_button(ui, editor, shown_tab, PropertyTab::Interface, themed_icon!(ui, "open_preferences.svg"), content_fill);
    tab_button(ui, editor, shown_tab, PropertyTab::Camera, themed_icon!(ui, "properties_camera.svg"), content_fill);
    tab_button(
        ui,
        editor,
        shown_tab,
        PropertyTab::Performance,
        themed_icon!(ui, "properties_performance.svg"),
        content_fill,
    );
    tab_button(ui, editor, shown_tab, PropertyTab::Developer, themed_icon!(ui, "properties_developer.svg"), content_fill);
    // Object starts the data/object group and stays available even before a
    // selection exists, so it can be the stable startup tab.
    ui.add_space(TAB_GROUP_GAP);
    tab_button(ui, editor, shown_tab, PropertyTab::Object, unthemed_icon!("properties_object.svg"), content_fill);
    if context.block_model.is_some() {
        tab_button(ui, editor, shown_tab, PropertyTab::BlockModel, unthemed_icon!("section_block_models.svg"), content_fill);
    }
    if context.triangulation.is_some() {
        tab_button(ui, editor, shown_tab, PropertyTab::Triangulation, unthemed_icon!("triangulation.svg"), content_fill);
    }
    if !context.objects.is_empty() {
        tab_button(ui, editor, shown_tab, PropertyTab::Design, unthemed_icon!("section_designs.svg"), content_fill);
    }
}

/// One tab in the right-hand column.
///
/// The selected tab is filled with the properties background and squared off,
/// so it reads as a notch cut straight through the column into the contents
/// beside it. No tooltip: the icon column is small enough that a hover label
/// covers its neighbours.
fn tab_button(ui: &mut egui::Ui, editor: &mut EditorState, shown_tab: PropertyTab, tab: PropertyTab, icon: egui::ImageSource<'static>, content_fill: egui::Color32) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), TAB_BUTTON_HEIGHT), egui::Sense::click());
    let selected = shown_tab == tab;
    if selected {
        ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, content_fill);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, content_fill.gamma_multiply(0.45));
    }
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(16.0));
    egui::Image::new(icon).fit_to_exact_size(icon_rect.size()).paint_at(ui, icon_rect);
    if response.clicked() {
        editor.active_property_tab = tab;
    }
}

/// Whether a field's edit is finished, rather than mid-drag.
///
/// Preferences are written to the config file as they are applied, and object
/// edits become undo entries, so a drag must land once rather than on every
/// frame it moves.
fn committed(response: &egui::Response) -> bool {
    response.drag_stopped() || (response.changed() && !response.dragged())
}

/// Runs `add_fields` against the editor's preferences draft and applies it as
/// soon as a field reports a finished edit.
///
/// The draft is held by the editor rather than rebuilt each frame because a
/// `DragValue` computes each step from the value it is handed: one that reset
/// to the unedited value every frame could never accumulate.
fn settings_section(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    commands: &mut Vec<UiCommand>,
    heading: &str,
    add_fields: impl FnOnce(&mut egui::Ui, &mut PreferencesDraft) -> bool,
    restore_defaults: Option<fn(&mut PreferencesDraft)>,
) {
    let saved = editor.current_preferences();
    let draft = editor.preferences_draft.get_or_insert(saved);
    menu::menu_section(ui, heading);
    let changed = add_fields(ui, draft);
    let draft = *draft;

    if let Some(reset_tab) = restore_defaults {
        ui.add_space(10.0);
        let mut restored = draft;
        reset_tab(&mut restored);
        // Extend, not the panel's global Truncate: a full-width button on a
        // narrow panel should keep its label and be reachable by scrolling
        // rather than read "Restore Def…".
        let restore_clicked = ui
            .scope(|ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.add_enabled(draft != restored, egui::Button::new(tr!(literal = "Restore Defaults")))
                    .on_hover_text(tr!("properties-restore-defaults", heading = heading))
                    .clicked()
            })
            .inner;
        if restore_clicked {
            commands.push(UiCommand::ApplyPreferences(restored));
        } else if changed && draft != saved {
            commands.push(UiCommand::ApplyPreferences(draft));
        }
    } else if changed && draft != saved {
        commands.push(UiCommand::ApplyPreferences(draft));
    }
    ui.add_space(6.0);
}

fn reset_interface_defaults(draft: &mut PreferencesDraft) {
    let defaults = PreferencesDraft::default();
    draft.renderer_background_color = defaults.renderer_background_color;
    draft.dark_mode = defaults.dark_mode;
    draft.show_console = defaults.show_console;
    draft.panel_chrome = defaults.panel_chrome;
    draft.show_world_axis_gizmo = defaults.show_world_axis_gizmo;
    draft.show_xy_grid = defaults.show_xy_grid;
    draft.show_scale_bar = defaults.show_scale_bar;
}

fn reset_developer_defaults(draft: &mut PreferencesDraft) {
    let defaults = PreferencesDraft::default();
    draft.frame_counter_enabled = defaults.frame_counter_enabled;
    draft.debug_chunk_coloring = defaults.debug_chunk_coloring;
    draft.debug_clip_planes = defaults.debug_clip_planes;
}

fn reset_camera_defaults(draft: &mut PreferencesDraft) {
    let defaults = PreferencesDraft::default();
    draft.plan_orbit_sensitivity = defaults.plan_orbit_sensitivity;
    draft.plan_zoom_sensitivity = defaults.plan_zoom_sensitivity;
    draft.plan_invert_vertical_look = defaults.plan_invert_vertical_look;
    draft.plan_invert_horizontal_look = defaults.plan_invert_horizontal_look;
    draft.plan_zoom_towards_cursor = defaults.plan_zoom_towards_cursor;
    draft.fly_field_of_view_degrees = defaults.fly_field_of_view_degrees;
    draft.fly_mouse_look_sensitivity = defaults.fly_mouse_look_sensitivity;
    draft.fly_invert_vertical_look = defaults.fly_invert_vertical_look;
    draft.fly_invert_horizontal_look = defaults.fly_invert_horizontal_look;
    draft.fly_near_clip_limit = defaults.fly_near_clip_limit;
    draft.fly_max_clip_span = defaults.fly_max_clip_span;
}

fn reset_performance_defaults(draft: &mut PreferencesDraft) {
    let defaults = PreferencesDraft::default();
    draft.snap_poll_rate = defaults.snap_poll_rate;
    draft.frame_rate_cap = defaults.frame_rate_cap;
    draft.resize_frame_rate_cap = defaults.resize_frame_rate_cap;
    draft.block_model_interaction_resolution_divisor = defaults.block_model_interaction_resolution_divisor;
    draft.show_block_model_boundary_highlights = defaults.show_block_model_boundary_highlights;
    draft.downscale_raster_previews = defaults.downscale_raster_previews;
}

fn draw_interface_settings(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    // Language is not here: it is a status bar picker, so it is reachable
    // without opening a panel - see `elements::status_bar`.
    settings_section(
        ui,
        editor,
        commands,
        &tr!(literal = "Interface"),
        |ui, draft| {
            let mut changed = false;

            let [r, g, b, _] = draft.renderer_background_color;
            let mut background = egui::Color32::from_rgb(linear_to_srgb_byte(r), linear_to_srgb_byte(g), linear_to_srgb_byte(b));
            let response = MenuFieldColor32::new(tr!(literal = "Background"), &mut background).show(ui);
            if response.changed() {
                draft.renderer_background_color = [
                    byte_to_linear_rgba(background.r()),
                    byte_to_linear_rgba(background.g()),
                    byte_to_linear_rgba(background.b()),
                    1.0,
                ];
            }
            changed |= committed(&response);
            changed |= committed(&MenuFieldBool::new(tr!(literal = "Dark mode"), &mut draft.dark_mode).show(ui));
            changed |= committed(&MenuFieldBool::new(tr!(literal = "Show console"), &mut draft.show_console).show(ui));
            changed |= committed(&MenuFieldBool::new(tr!(literal = "Panel chrome"), &mut draft.panel_chrome).show(ui));
            changed |= committed(&MenuFieldBool::new(tr!(literal = "World axis gizmo"), &mut draft.show_world_axis_gizmo).show(ui));
            changed |= committed(&MenuFieldBool::new(tr!(literal = "XY grid"), &mut draft.show_xy_grid).show(ui));
            changed |= committed(&MenuFieldBool::new(tr!(literal = "Scale bar"), &mut draft.show_scale_bar).show(ui));
            changed
        },
        Some(reset_interface_defaults),
    );
}

fn draw_camera_settings(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    settings_section(
        ui,
        editor,
        commands,
        &tr!(literal = "Camera"),
        |ui, draft| {
            let mut changed = false;
            CollapsibleSection::new("camera_plan_mode", tr!(literal = "Plan Mode")).default_open(true).show(ui, |ui| {
                changed |= committed(
                    &MenuFieldF64::new(tr!(literal = "Orbit sensitivity"), &mut draft.plan_orbit_sensitivity, 0.0001..=0.02)
                        .speed(0.0001)
                        .max_decimals(4)
                        .show(ui),
                );
                changed |= committed(
                    &MenuFieldF64::new(tr!(literal = "Zoom sensitivity"), &mut draft.plan_zoom_sensitivity, 0.0001..=0.05)
                        .speed(0.0001)
                        .max_decimals(4)
                        .show(ui),
                );
                changed |= committed(&MenuFieldBool::new(tr!(literal = "Invert vertical"), &mut draft.plan_invert_vertical_look).show(ui));
                changed |= committed(&MenuFieldBool::new(tr!(literal = "Invert horizontal"), &mut draft.plan_invert_horizontal_look).show(ui));
                changed |= committed(&MenuFieldBool::new(tr!(literal = "Zoom to cursor"), &mut draft.plan_zoom_towards_cursor).show(ui));
            });

            ui.add_space(4.0);
            CollapsibleSection::new("camera_fly_mode", tr!(literal = "Fly Mode")).show(ui, |ui| {
                changed |= committed(
                    &MenuFieldF64::new(tr!(literal = "Field of view"), &mut draft.fly_field_of_view_degrees, 20.0..=120.0)
                        .suffix(tr!(literal = "°"))
                        .show(ui),
                );
                changed |= committed(
                    &MenuFieldF64::new(tr!(literal = "Look sensitivity"), &mut draft.fly_mouse_look_sensitivity, 0.0001..=0.02)
                        .speed(0.0001)
                        .max_decimals(4)
                        .show(ui),
                );
                changed |= committed(&MenuFieldBool::new(tr!(literal = "Invert vertical"), &mut draft.fly_invert_vertical_look).show(ui));
                changed |= committed(&MenuFieldBool::new(tr!(literal = "Invert horizontal"), &mut draft.fly_invert_horizontal_look).show(ui));
                changed |= committed(
                    &MenuFieldF64::new(tr!(literal = "Near clip limit"), &mut draft.fly_near_clip_limit, 0.01..=100.0)
                        .speed(0.01)
                        .suffix(tr!(literal = "m"))
                        .show(ui),
                );
                changed |= committed(
                    &MenuFieldF64::new(tr!(literal = "Max clip span"), &mut draft.fly_max_clip_span, 100.0..=1_000_000.0)
                        .speed(100.0)
                        .suffix(tr!(literal = "m"))
                        .show(ui),
                );
            });
            changed
        },
        Some(reset_camera_defaults),
    );
}

fn draw_performance_settings(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    settings_section(
        ui,
        editor,
        commands,
        &tr!(literal = "Performance"),
        |ui, draft| {
            let mut changed = false;
            changed |= committed(
                &MenuFieldU32::new(tr!(literal = "Snap polling"), &mut draft.snap_poll_rate, 5..=1000)
                    .suffix(tr!(literal = " Hz"))
                    .show(ui),
            );
            changed |= committed(
                &MenuFieldU32::new(tr!(literal = "Frame rate cap"), &mut draft.frame_rate_cap, 20..=1000)
                    .suffix(tr!(literal = " FPS"))
                    .show(ui),
            );
            changed |= committed(
                &MenuFieldU32::new(tr!(literal = "Cap while resizing"), &mut draft.resize_frame_rate_cap, 20..=1000)
                    .suffix(tr!(literal = " FPS"))
                    .show(ui),
            );
            changed |= committed(
                &MenuFieldU32::new(tr!(literal = "Block model downscale"), &mut draft.block_model_interaction_resolution_divisor, 1..=64)
                    .suffix(tr!(literal = "x"))
                    .show(ui),
            );
            changed |= committed(
                &MenuFieldBool::new(tr!(literal = "Reflective block edges"), &mut draft.show_block_model_boundary_highlights)
                    .help_text(tr!(
                        literal = "Adds a view-dependent rim highlight at block and material boundaries. Leaving this off slightly reduces volume-rendering work."
                    ))
                    .show(ui),
            );
            changed |= committed(
            &MenuFieldBool::new(tr!(literal = "Downscale rasters"), &mut draft.downscale_raster_previews)
                .help_text(tr!(
                    literal = "Limits newly loaded GeoTIFF previews to 4096 pixels on their longest side. Disable to use full resolution up to the GPU's texture limit, which uses more memory."
                ))
                .show(ui),
        );
            changed
        },
        Some(reset_performance_defaults),
    );
}

fn draw_developer_settings(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    settings_section(
        ui,
        editor,
        commands,
        &tr!(literal = "Developer"),
        |ui, draft| {
            let mut changed = false;
            changed |= committed(&MenuFieldBool::new(tr!(literal = "Frame counter"), &mut draft.frame_counter_enabled).show(ui));
            changed |= committed(
                &MenuFieldBool::new(tr!(literal = "Colour GPU chunks"), &mut draft.debug_chunk_coloring)
                    .help_text(tr!(literal = "Visualises the Morton spatial chunking used for frustum culling."))
                    .show(ui),
            );
            changed |= committed(
                &MenuFieldBool::new(tr!(literal = "Camera clip planes"), &mut draft.debug_clip_planes)
                    .help_text(tr!(literal = "Shows the live near and far projection distances in the status bar."))
                    .show(ui),
            );
            changed
        },
        Some(reset_developer_defaults),
    );
}

struct EntityDetails {
    name: String,
    kind: String,
    id: String,
    layer: Option<String>,
    source: Option<String>,
    visible: bool,
    locked: bool,
    bounds: Option<(glam::DVec3, glam::DVec3)>,
}

fn entity_details(entity: SceneEntityId, editor: &EditorState, project: &UiProjectView, block_models: &[OpenBlockModel], document: &Document) -> Option<EntityDetails> {
    let locked = editor.frozen_handles.contains(&entity);
    match entity {
        SceneEntityId::Object(id) => {
            let object = document.get_object(id)?;
            let layer = document.layer(object.layer());
            Some(EntityDetails {
                name: object.kind_name().to_owned(),
                kind: tr!(literal = "Design Object"),
                id: format!("object:{}", id.0),
                layer: layer.map(|layer| layer.name.clone()),
                source: None,
                visible: layer.is_some_and(|layer| layer.visible) && !document.is_object_hidden(id) && !editor.hidden_handles.contains(&entity),
                locked,
                bounds: object.world_bounds(),
            })
        }
        SceneEntityId::Triangulation(id) => {
            let item = project.triangulations.iter().find(|item| item.id == id && item.is_loaded)?;
            Some(EntityDetails {
                name: item.name.clone(),
                kind: tr!(literal = "Triangulation"),
                id: format!("triangulation:{}", id.0),
                layer: None,
                source: item.source_name.clone(),
                visible: item.visible && !editor.hidden_handles.contains(&entity),
                locked,
                bounds: item.bounds,
            })
        }
        SceneEntityId::BlockModel(id) => {
            let model = block_models.iter().find(|item| item.id == id && item.state.loaded)?;
            let item = project.block_models.iter().find(|item| item.id == id);
            let source = item.and_then(|item| item.source_name.clone());
            Some(EntityDetails {
                name: model.name.clone(),
                kind: tr!(literal = "Block Model"),
                id: format!("block-model:{}", id.0),
                layer: None,
                source,
                visible: model.visible && !editor.hidden_handles.contains(&entity),
                locked,
                bounds: item.and_then(|item| item.bounds).or_else(|| model.world_bounds()),
            })
        }
        SceneEntityId::PointCloud(id) => {
            let item = project.point_clouds.iter().find(|item| item.id == id && item.is_loaded)?;
            Some(EntityDetails {
                name: item.name.clone(),
                kind: tr!(literal = "Point Cloud"),
                id: format!("point-cloud:{}", id.0),
                layer: None,
                source: item.source_name.clone(),
                visible: item.visible && !editor.hidden_handles.contains(&entity),
                locked,
                bounds: item.bounds,
            })
        }
        SceneEntityId::DrillHole(id) => {
            let item = project.drill_holes.iter().find(|item| item.id == id && item.is_loaded)?;
            Some(EntityDetails {
                name: item.name.clone(),
                kind: tr!(literal = "Drill Holes"),
                id: format!("drill-hole:{}", id.0),
                layer: None,
                source: item.source_name.clone(),
                visible: item.visible && !editor.hidden_handles.contains(&entity),
                locked,
                bounds: item.bounds,
            })
        }
    }
}

fn common_text<'a>(mut values: impl Iterator<Item = &'a str>) -> String {
    let Some(first) = values.next() else {
        return String::new();
    };
    if values.all(|value| value == first) { first.to_owned() } else { tr!(literal = "Mixed") }
}

fn common_state(details: &[EntityDetails], value: impl Fn(&EntityDetails) -> bool, on: &str, off: &str) -> String {
    let Some(first) = details.first().map(&value) else {
        return off.to_owned();
    };
    if details.iter().skip(1).all(|detail| value(detail) == first) {
        if first { on.to_owned() } else { off.to_owned() }
    } else {
        tr!(literal = "Mixed")
    }
}

fn selection_bounds(details: &[EntityDetails]) -> Option<(glam::DVec3, glam::DVec3)> {
    let mut bounds = details.iter().map(|detail| detail.bounds);
    let (mut min, mut max) = bounds.next()??;
    for next in bounds {
        let (next_min, next_max) = next?;
        min = min.min(next_min);
        max = max.max(next_max);
    }
    Some((min, max))
}

fn spatial_value(value: f64) -> String {
    format!("{} m", crate::model::plot::format_quantity(value, 3))
}

fn spatial_vector_rows(ui: &mut egui::Ui, label: &str, value: glam::DVec3) {
    read_only_row(ui, &format!("{label} X"), &spatial_value(value.x));
    read_only_row(ui, &format!("{label} Y"), &spatial_value(value.y));
    read_only_row(ui, &format!("{label} Z"), &spatial_value(value.z));
}

/// Generic information shared by every selectable scene entity. Type-specific
/// appearance and geometry controls remain in the neighbouring tabs.
fn draw_object_tab(ui: &mut egui::Ui, editor: &EditorState, project: &UiProjectView, block_models: &[OpenBlockModel], document: &Document, context: &PropertyContext) {
    menu::menu_section(ui, tr!(literal = "Object"));
    let details = context
        .entities
        .iter()
        .filter_map(|&entity| entity_details(entity, editor, project, block_models, document))
        .collect::<Vec<_>>();
    if details.is_empty() {
        ui.label(egui::RichText::new(tr!(literal = "No object selected")).color(ui.visuals().weak_text_color()));
        ui.add_space(6.0);
        return;
    }

    let heading = if details.len() == 1 {
        details[0].name.clone()
    } else {
        tr_format!(literal = "%count% objects", count = details.len())
    };
    ui.label(egui::RichText::new(heading).strong());
    ui.add_space(4.0);

    CollapsibleSection::new("object_general", tr!(literal = "General")).default_open(true).show(ui, |ui| {
        if let [detail] = details.as_slice() {
            read_only_row(ui, &tr!(literal = "Name"), &detail.name);
            read_only_row(ui, &tr!(literal = "Type"), &detail.kind);
            read_only_row(ui, &tr!(literal = "ID"), &detail.id);
            if let Some(layer) = &detail.layer {
                read_only_row(ui, &tr!(literal = "Layer"), layer);
            }
            if let Some(source) = &detail.source {
                read_only_row(ui, &tr!(literal = "Source"), source);
            }
        } else {
            read_only_row(ui, &tr!(literal = "Selected"), &details.len().separate_with_commas());
            read_only_row(ui, &tr!(literal = "Type"), &common_text(details.iter().map(|detail| detail.kind.as_str())));
            if let Some(layer) = details.first().and_then(|detail| detail.layer.as_deref())
                && details.iter().all(|detail| detail.layer.as_deref() == Some(layer))
            {
                read_only_row(ui, &tr!(literal = "Layer"), layer);
            }
            if let Some(source) = details.first().and_then(|detail| detail.source.as_deref())
                && details.iter().all(|detail| detail.source.as_deref() == Some(source))
            {
                read_only_row(ui, &tr!(literal = "Source"), source);
            }
        }
        read_only_row(
            ui,
            &tr!(literal = "Visibility"),
            &common_state(&details, |detail| detail.visible, &tr!(literal = "Visible"), &tr!(literal = "Hidden")),
        );
        read_only_row(
            ui,
            &tr!(literal = "Editing"),
            &common_state(&details, |detail| detail.locked, &tr!(literal = "Locked"), &tr!(literal = "Unlocked")),
        );
    });

    ui.add_space(4.0);
    CollapsibleSection::new("object_transform", tr!(literal = "Transform")).default_open(true).show(ui, |ui| {
        if let Some((min, max)) = selection_bounds(&details) {
            spatial_vector_rows(ui, &tr!(literal = "Centre"), (min + max) * 0.5);
            spatial_vector_rows(ui, &tr!(literal = "Size"), (max - min).max(glam::DVec3::ZERO));
        } else {
            ui.label(egui::RichText::new(tr!(literal = "No spatial extent available")).color(ui.visuals().weak_text_color()));
        }
    });
    ui.add_space(6.0);
}

fn draw_block_model_tab(ui: &mut egui::Ui, editor: &mut EditorState, block_models: &[OpenBlockModel], context: &PropertyContext, commands: &mut Vec<UiCommand>) {
    let Some(model) = context.block_model.and_then(|id| block_models.iter().find(|model| model.id == id)) else {
        return;
    };
    ui.add_space(4.0);
    ui.label(egui::RichText::new(&model.name).strong());
    ui.add_space(4.0);
    BlockModelProperties::new("block_model_properties", model).show(ui, editor, commands);
    ui.add_space(6.0);
}

/// The Triangulation tab: what the selected surface is made of, and the face
/// colour it is drawn with.
fn draw_triangulation_tab(ui: &mut egui::Ui, project: &UiProjectView, context: &PropertyContext, commands: &mut Vec<UiCommand>, geometry_dirty: &mut bool) {
    let Some(triangulation) = context.triangulation.and_then(|id| project.triangulations.iter().find(|tri| tri.id == id)) else {
        return;
    };
    ui.add_space(4.0);
    ui.label(egui::RichText::new(&triangulation.name).strong());
    ui.add_space(4.0);

    let mut color32 = rgba_to_color32(triangulation.color);
    let response = MenuFieldColor32::new(tr!(literal = "Face colour"), &mut color32).show(ui);
    if committed(&response) {
        commands.push(UiCommand::SetTriangulationColor(triangulation.id, color32_to_rgba(color32)));
        *geometry_dirty = true;
    }

    ui.add_space(6.0);
    CollapsibleSection::new("triangulation_geometry", tr!(literal = "Geometry"))
        .default_open(true)
        .show(ui, |ui| {
            read_only_row(ui, &tr!(literal = "Vertices"), &triangulation.vertex_count.separate_with_commas());
            read_only_row(ui, &tr!(literal = "Triangles"), &triangulation.triangle_count.separate_with_commas());
        });
    ui.add_space(6.0);
}

/// A labelled value the panel only reports, laid out like the editable fields
/// beside it so the columns line up.
fn read_only_row(ui: &mut egui::Ui, label: &str, value: &str) {
    let row_height = ui.spacing().interact_size.y;
    let row_width = ui.available_width();
    // The same column width the fields resolve for themselves, or the values
    // would not line up under them.
    let value_width = crate::ui::widgets::menu::field_column_for(ui, label, false);
    ui.allocate_ui_with_layout(egui::vec2(row_width, row_height), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.allocate_ui_with_layout(egui::vec2(value_width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).color(ui.visuals().weak_text_color()));
        });
        let label_width = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(label_width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            menu_field_label(ui, label.into(), None);
        });
    });
}

fn fill_style_label(style: FillStyle) -> String {
    match style {
        FillStyle::Clear => tr!(literal = "Clear"),
        FillStyle::Crosses => tr!(literal = "Crosses"),
        FillStyle::Slashes => tr!(literal = "Slashes"),
        FillStyle::Solid => tr!(literal = "Solid"),
    }
}

fn draw_design_tab(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document, context: &PropertyContext, commands: &mut Vec<UiCommand>, geometry_dirty: &mut bool) {
    ui.add_space(4.0);
    let heading = match context.objects.as_slice() {
        [id] => document
            .get_object(*id)
            .map(|object| object.kind_name().to_owned())
            .unwrap_or_else(|| tr!(literal = "Object")),
        objects => tr_format!(literal = "%count% objects", count = objects.len()),
    };
    ui.label(egui::RichText::new(heading).strong());
    ui.add_space(4.0);

    // Fields describe the first selection and write to all of it, matching how
    // the batch commands behave.
    let first_color = context
        .objects
        .first()
        .and_then(|&id| document.get_object(id))
        .map(|object| document.object_rgba(object))
        .unwrap_or([0.0; 4]);
    let mut color32 = rgba_to_color32(first_color);
    let response = MenuFieldColor32::new(tr!(literal = "Line colour"), &mut color32).show(ui);
    if committed(&response) {
        commands.push(UiCommand::BatchSetObjectColor(context.objects.clone(), ObjectColor::Fixed(color32_to_rgba(color32))));
        *geometry_dirty = true;
    }

    if context.polylines.is_empty() {
        return;
    }

    let (first_closed, first_fill, first_line_weight) = match context.polylines.first().and_then(|&id| document.get_object(id)) {
        Some(crate::model::Object::Polyline { closed, fill, line_weight, .. }) => (*closed, *fill, *line_weight),
        _ => (false, FillStyle::Clear, 1.0),
    };

    let closed_label = tr!(literal = "Closed");
    let open_label = tr!(literal = "Open");
    let mut closed = first_closed;
    MenuFieldCombo::new(
        "design_shape",
        tr!(literal = "Shape"),
        &mut closed,
        if first_closed { closed_label.clone() } else { open_label.clone() },
        [(true, closed_label.into()), (false, open_label.into())],
    )
    .show(ui);
    if closed != first_closed {
        commands.push(UiCommand::BatchSetPolylineClosed(context.polylines.clone(), closed));
        *geometry_dirty = true;
    }

    let mut fill = first_fill;
    MenuFieldCombo::new(
        "design_fill",
        tr!(literal = "Fill"),
        &mut fill,
        fill_style_label(first_fill),
        [FillStyle::Clear, FillStyle::Crosses, FillStyle::Slashes, FillStyle::Solid].map(|style| (style, fill_style_label(style).into())),
    )
    .show(ui);
    if fill != first_fill {
        commands.push(UiCommand::BatchSetObjectFill(context.polylines.clone(), fill));
        *geometry_dirty = true;
    }

    // Seed the in-progress value from the document whenever the selection
    // changes; between those points the drag owns it.
    if editor.design_line_weight_input.as_ref().is_none_or(|(object_ids, _)| object_ids != &context.polylines) {
        editor.design_line_weight_input = Some((context.polylines.clone(), first_line_weight));
    }
    if let Some((_, line_weight)) = editor.design_line_weight_input.as_mut() {
        let response = MenuFieldF32::new(tr!(literal = "Line weight"), line_weight, 0.1..=20.0)
            .help_text(tr!(literal = "Stroke width used to draw the selected polylines."))
            .speed(0.1)
            .show(ui);
        let line_weight = *line_weight;
        if committed(&response) {
            commands.push(UiCommand::BatchSetPolylineLineWeight(context.polylines.clone(), line_weight));
            *geometry_dirty = true;
        }
    }
    ui.add_space(6.0);
}
