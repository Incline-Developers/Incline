//! Application preferences and selection appearance controls.

use crate::{
    i18n::tr,
    model::{Document, FillStyle, ObjectColor, ObjectId, SceneEntityId, block_model::OpenBlockModel},
    rendering::color::{byte_to_linear_rgba, color32_to_rgba, linear_to_srgb_byte, rgba_to_color32},
    ui::{
        UiCommand, UiProjectView,
        state::{EditorState, PreferencesDraft, PropertyTab},
        widgets::{
            collapsible_section::CollapsibleSection,
            context_menu::{FIELD_WIDTH, context_menu_fields},
            menu::{self, MenuFieldBool, MenuFieldColor32, MenuFieldCombo, MenuFieldF32, MenuFieldF64, MenuFieldU32},
            viewport::BlockModelProperties,
        },
    },
};

pub(crate) fn draw_preferences(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>) {
    if !editor.show_preferences {
        return;
    }
    let mut open = true;
    let available = ui.ctx().content_rect().size() - egui::vec2(24.0, 24.0);
    let scale = (available.x / 600.0).min(available.y / 480.0).clamp(0.1, 1.0);
    let size = egui::vec2(600.0, 480.0) * scale;
    crate::ui::widgets::menu::DragableMenu::new("preferences", tr!("preferences-title"))
        .open(&mut open)
        .fixed_size(size)
        .inner_margin(egui::Margin::ZERO)
        .show(ui.ctx(), |ui| {
            let body_height = (size.y - menu::TITLE_BAR_HEIGHT).max(0.0);
            let (body, _) = ui.allocate_exact_size(egui::vec2(size.x, body_height), egui::Sense::hover());
            let navigation = egui::Rect::from_min_max(body.min, egui::pos2(body.left() + 150.0 * scale, body.bottom()));
            let page = egui::Rect::from_min_max(egui::pos2(navigation.right() + 1.0, body.top()), body.max);
            let (surface, stripe) = crate::ui::widgets::tree_row_colors(ui);
            ui.painter().rect_filled(navigation, 0.0, surface);
            ui.painter().add(crate::ui::widgets::explorer::stripe_bands(
                navigation.x_range(),
                navigation.top(),
                navigation.bottom(),
                crate::ui::widgets::explorer::row_height(ui),
                stripe,
            ));
            ui.painter()
                .line_segment([navigation.right_top(), navigation.right_bottom()], ui.visuals().widgets.noninteractive.bg_stroke);
            ui.scope_builder(egui::UiBuilder::new().id_salt("preferences_navigation").max_rect(navigation), |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(navigation));
                let row_height = crate::ui::widgets::explorer::row_height(ui);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.spacing_mut().interact_size.y = row_height;
                ui.spacing_mut().button_padding.y = 0.0;
                for (tab, label) in [
                    (PropertyTab::Interface, tr!(literal = "Interface")),
                    (PropertyTab::Camera, tr!(literal = "Camera")),
                    (PropertyTab::Performance, tr!(literal = "Performance")),
                    (PropertyTab::Developer, tr!(literal = "Developer")),
                ] {
                    let response = crate::ui::widgets::explorer::ExplorerEntry::new(ui.id().with(tab as u8), label)
                        .selected(editor.active_property_tab == tab)
                        .show(ui)
                        .response;
                    if response.clicked() {
                        editor.active_property_tab = tab;
                    }
                }
            });
            ui.scope_builder(egui::UiBuilder::new().id_salt("preferences_page").max_rect(page.shrink2(egui::vec2(12.0, 8.0))), |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(page));
                egui::ScrollArea::both()
                    .id_salt(("preferences_page_scroll", editor.active_property_tab as u8))
                    .auto_shrink([false; 2])
                    .show(ui, |ui| match editor.active_property_tab {
                        PropertyTab::Interface => draw_interface_settings(ui, editor, commands),
                        PropertyTab::Camera => draw_camera_settings(ui, editor, commands),
                        PropertyTab::Performance => draw_performance_settings(ui, editor, commands),
                        PropertyTab::Developer => draw_developer_settings(ui, editor, commands),
                        // Not one of this dialog's own tabs: set from the planning
                        // viewport's Reserves panel, which this dialog never shows.
                        PropertyTab::Reserves => {}
                    });
            });
        });
    editor.show_preferences = open;
}

struct PropertyContext {
    objects: Vec<ObjectId>,
    polylines: Vec<ObjectId>,
}

pub(crate) fn draw_selection_appearance(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    project: &UiProjectView,
    document: &Document,
    commands: &mut Vec<UiCommand>,
    geometry_dirty: &mut bool,
) {
    let mut objects: Vec<_> = editor
        .selected_handles
        .iter()
        .filter_map(|id| match id {
            SceneEntityId::Object(id) if document.get_object(*id).is_some() => Some(*id),
            _ => None,
        })
        .collect();
    objects.sort_by_key(|id| id.0);
    let mut selected_types = [false; 7];
    for handle in &editor.selected_handles {
        let kind = match handle {
            SceneEntityId::Object(id) => match document.get_object(*id) {
                Some(crate::model::Object::Polyline { .. }) => 0,
                Some(crate::model::Object::Point { .. }) => 1,
                Some(crate::model::Object::Text { .. }) => 2,
                None => continue,
            },
            SceneEntityId::Triangulation(_) => 3,
            SceneEntityId::BlockModel(_) => 4,
            SceneEntityId::DrillHole(_) => 5,
            SceneEntityId::PointCloud(_) => 6,
        };
        selected_types[kind] = true;
    }
    let show_headings = selected_types.into_iter().filter(|selected| *selected).count() > 1;
    for (kind, heading) in [(0, tr!("context-polylines")), (1, tr!("context-points")), (2, tr!(literal = "Text"))] {
        let objects: Vec<_> = objects
            .iter()
            .copied()
            .filter(|id| match document.get_object(*id) {
                Some(crate::model::Object::Polyline { .. }) => kind == 0,
                Some(crate::model::Object::Point { .. }) => kind == 1,
                Some(crate::model::Object::Text { .. }) => kind == 2,
                None => false,
            })
            .collect();
        if objects.is_empty() {
            continue;
        }
        if show_headings {
            context_menu_fields(ui, |ui| menu::menu_section(ui, heading));
        }
        let polylines = if kind == 0 { objects.clone() } else { Vec::new() };
        context_menu_fields(ui, |ui| {
            draw_design_tab(
                ui,
                editor,
                document,
                &PropertyContext {
                    objects: objects.clone(),
                    polylines,
                },
                commands,
                geometry_dirty,
            );
        });
        if crate::ui::widgets::context_menu::ContextMenuAction::new(tr!(literal = "Move to Layer..."))
            .show(ui)
            .clicked()
        {
            let target_layer = project
                .projects
                .iter()
                .find(|entry| entry.is_active)
                .and_then(|entry| entry.layers.first())
                .map(|layer| layer.id);
            editor.move_to_layer_dialog = Some(crate::ui::state::MoveToLayerDialog {
                object_ids: objects,
                target_layer,
                copy: false,
            });
            commands.push(UiCommand::CloseCanvasContextMenu);
        }
    }
    let triangulations: Vec<_> = project
        .triangulations
        .iter()
        .filter(|tri| tri.is_loaded && editor.selected_handles.contains(&SceneEntityId::Triangulation(tri.id)))
        .collect();
    let has_appearance = !objects.is_empty() || !triangulations.is_empty();
    if let Some(first) = triangulations.first() {
        context_menu_fields(ui, |ui| {
            if show_headings {
                menu::menu_section(ui, tr!(literal = "Triangulations"));
            }
            let mut color = rgba_to_color32(first.color);
            if committed(&MenuFieldColor32::new(tr!(literal = "Face colour"), &mut color).show(ui)) {
                for tri in triangulations {
                    commands.push(UiCommand::SetTriangulationColor(tri.id, color32_to_rgba(color)));
                }
                *geometry_dirty = true;
            }
        });
    }
    if has_appearance {
        crate::ui::widgets::context_menu::context_menu_separator(ui);
    }
}

pub(crate) fn draw_block_model_controls(ui: &mut egui::Ui, editor: &mut EditorState, models: &[OpenBlockModel], commands: &mut Vec<UiCommand>, canvas: egui::Rect) {
    let selected: Vec<_> = models
        .iter()
        .filter(|model| model.state.loaded && editor.selected_handles.contains(&SceneEntityId::BlockModel(model.id)))
        .collect();
    let selection_id = egui::Id::new("block_model_controls_selection");
    let selection: Vec<_> = selected.iter().map(|model| model.id).collect();
    let selection_changed = ui.ctx().data_mut(|data| {
        let changed = data.get_temp::<Vec<crate::model::block_model::BlockModelId>>(selection_id).as_ref() != Some(&selection);
        data.insert_temp(selection_id, selection);
        changed
    });
    if selection_changed && !selected.is_empty() && !selected.iter().any(|model| Some(model.id) == editor.viewport_block_model_id) {
        editor.viewport_block_model_id = Some(selected[0].id);
    }
    let Some(model) = models.iter().find(|model| Some(model.id) == editor.viewport_block_model_id && model.state.loaded) else {
        return;
    };
    egui::Area::new(egui::Id::new("block_model_viewport_controls"))
        .anchor(
            egui::Align2::CENTER_BOTTOM,
            egui::vec2(
                canvas.center().x - ui.ctx().content_rect().center().x,
                canvas.bottom() - ui.ctx().content_rect().bottom() - 8.0,
            ),
        )
        .order(egui::Order::Middle)
        .show(ui.ctx(), |ui| {
            egui::Frame::window(ui.style()).show(ui, |ui| {
                ui.set_width((canvas.width() - 32.0).clamp(240.0, 780.0));
                egui::ScrollArea::both().max_height((canvas.height() * 0.4).max(80.0)).show(ui, |ui| {
                    MenuFieldCombo::new(
                        "filter_model",
                        tr!(literal = "Block model"),
                        &mut editor.viewport_block_model_id,
                        model.name.clone(),
                        models.iter().filter(|model| model.state.loaded).map(|model| (Some(model.id), model.name.clone().into())),
                    )
                    .show(ui);
                    if let Some(model) = models.iter().find(|model| Some(model.id) == editor.viewport_block_model_id && model.state.loaded) {
                        ui.push_id(model.id, |ui| {
                            BlockModelProperties::new(("block_model_controls", model.id), model).show(ui, editor, commands)
                        });
                    }
                });
            });
        });
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
    draft.vsync_enabled = defaults.vsync_enabled;
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
    // An adapter with no way to present out of step - a browser surface, say -
    // has nothing to offer here, and the cap it gates would never be applied.
    let vsync_switchable = editor.vsync_switchable;
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
            if vsync_switchable {
                changed |= committed(
                    &MenuFieldBool::new(tr!(literal = "Vertical sync"), &mut draft.vsync_enabled)
                        .help_text(tr!(
                            literal = "Presents in step with the display: no tearing, and the display sets the frame rate. Off, frames present as soon as they are drawn and the cap below applies."
                        ))
                        .show(ui),
                );
            }
            if !draft.vsync_enabled {
                changed |= committed(
                    &MenuFieldU32::new(tr!(literal = "Frame rate cap"), &mut draft.frame_rate_cap, 20..=1000)
                        .suffix(tr!(literal = " FPS"))
                        .show(ui),
                );
            }
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

fn fill_style_label(style: FillStyle) -> String {
    match style {
        FillStyle::Clear => tr!(literal = "Clear"),
        FillStyle::Crosses => tr!(literal = "Crosses"),
        FillStyle::Slashes => tr!(literal = "Slashes"),
        FillStyle::Solid => tr!(literal = "Solid"),
    }
}

fn draw_design_tab(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document, context: &PropertyContext, commands: &mut Vec<UiCommand>, geometry_dirty: &mut bool) {
    // Fields describe the first selection and write to all of it, matching how
    // the batch commands behave.
    let first_color = context
        .objects
        .first()
        .and_then(|&id| document.get_object(id))
        .map(|object| document.object_rgba(object))
        .unwrap_or([0.0; 4]);
    let mut color32 = rgba_to_color32(first_color);
    let colour_label = match context.objects.first().and_then(|&id| document.get_object(id)) {
        Some(crate::model::Object::Text { .. }) => tr!("context-text-colour"),
        _ => tr!(literal = "Line colour"),
    };
    let response = MenuFieldColor32::new(colour_label, &mut color32).show(ui);
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
    .width(FIELD_WIDTH)
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
    .width(FIELD_WIDTH)
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
            .width(FIELD_WIDTH)
            .speed(0.1)
            .show(ui);
        let line_weight = *line_weight;
        if committed(&response) {
            commands.push(UiCommand::BatchSetPolylineLineWeight(context.polylines.clone(), line_weight));
            *geometry_dirty = true;
        }
    }
}
