//! Left-side explorer panel for the active project's retained content.

use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use crate::{
    model::{Document, block_model::OpenBlockModel},
    ui::{
        EditorState, UiCommand, UiProjectView,
        elements::properties::draw_properties,
        fonts::bold,
        unthemed_icon,
        widgets::explorer::{ExplorerEntry, ExplorerHeader, explorer_note, fill_trailing_stripes},
    },
};

/// Grey colour used for inactive (not loaded) layers and triangulations.
const INACTIVE_TEXT_COLOR: egui::Color32 = egui::Color32::from_gray(140);

/// Section heading tints, keyed to the icons each section's entries use.
///
/// One colour serves both themes: each holds at least a 3:1 contrast ratio
/// against the light panel (white) and the dark one alike.
const HEADER_PROJECTS: egui::Color32 = egui::Color32::from_rgb(0xD6, 0x6E, 0x1E);
const HEADER_DESIGNS: egui::Color32 = egui::Color32::from_rgb(0x44, 0x62, 0xBF);
const HEADER_TRIANGULATIONS: egui::Color32 = egui::Color32::from_rgb(0xAE, 0x58, 0xDB);
const HEADER_RASTERS: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x91, 0x99);
const HEADER_POINT_CLOUDS: egui::Color32 = egui::Color32::from_rgb(0xC9, 0x3B, 0x2C);
const HEADER_BLOCK_MODELS: egui::Color32 = egui::Color32::from_rgb(0x69, 0x8F, 0x3F);
const HEADER_DRILL_HOLES: egui::Color32 = egui::Color32::from_rgb(0xDB, 0x5F, 0x58);

#[cfg(not(target_arch = "wasm32"))]
const MODIFIED_TIME_CACHE_TTL: Duration = Duration::from_secs(30);

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
enum ModifiedTimeCacheEntry {
    Loading,
    Ready { checked_at: Instant, line: Option<String> },
}

#[cfg(not(target_arch = "wasm32"))]
fn modified_time_cache() -> &'static Mutex<HashMap<PathBuf, ModifiedTimeCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, ModifiedTimeCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(not(target_arch = "wasm32"))]
fn format_modified_tooltip_line(modified: SystemTime) -> String {
    let local = chrono::DateTime::<chrono::Local>::from(modified);
    format!("Modified {}", local.format("%d-%b-%Y %H:%M"))
}

/// Return a cached modified-time line and queue a bounded background stat when
/// absent or stale. File metadata can block on network-mounted paths, so hover
/// rendering must never perform the filesystem call itself.
#[cfg(not(target_arch = "wasm32"))]
fn modified_tooltip_line(ctx: &egui::Context, path: &Path) -> Option<String> {
    let now = Instant::now();
    let mut cache = modified_time_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match cache.get(path) {
        Some(ModifiedTimeCacheEntry::Loading) => return None,
        Some(ModifiedTimeCacheEntry::Ready { checked_at, line }) if now.duration_since(*checked_at) < MODIFIED_TIME_CACHE_TTL => {
            return line.clone();
        }
        _ => {}
    }
    let path = path.to_path_buf();
    cache.insert(path.clone(), ModifiedTimeCacheEntry::Loading);
    drop(cache);

    let repaint = ctx.clone();
    crate::app::jobs::spawn_io_task(move || {
        let line = std::fs::metadata(&path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(format_modified_tooltip_line);
        modified_time_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path, ModifiedTimeCacheEntry::Ready { checked_at: Instant::now(), line });
        repaint.request_repaint();
    });
    None
}

/// Attach a hover tooltip of `main` text plus the file's modified time.
///
/// Browser entries name a browser source rather than a file, and the wasm
/// target has neither `std::fs` nor a working `std::time::Instant`, so there
/// the tooltip is the main line alone.
fn on_hover_file_details(response: egui::Response, main: &str, path: Option<&Path>) -> egui::Response {
    #[cfg(target_arch = "wasm32")]
    let _ = path;
    response.on_hover_ui(|ui| {
        ui.label(main);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(line) = path.and_then(|path| modified_tooltip_line(ui.ctx(), path)) {
            ui.weak(line);
        }
    })
}

/// Take the next row position for the alternating background.
///
/// The count runs across the whole tree rather than per section, so the
/// stripes stay in step across a section boundary.
fn next_stripe(row_index: &mut usize) -> usize {
    let index = *row_index;
    *row_index += 1;
    index
}

/// Draw the left explorer panel.
///
/// Shows the active project path, the collapsible data sections, and the
/// properties panel below them. Returns the panel's bounding rect.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_explorer(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    project: &UiProjectView,
    block_models: &[OpenBlockModel],
    document: &Document,
    commands: &mut Vec<UiCommand>,
    geometry_dirty: &mut bool,
) -> egui::Rect {
    // Read before the panel closure borrows the editor for the tree.
    let (can_undo, can_redo) = (editor.can_undo, editor.can_redo);
    // The tree's lit rows are the properties panel's background, so the two
    // halves of the side panel share one palette.
    let (surface, stripe) = crate::ui::widgets::tree_row_colors(ui);
    egui::Panel::left("explorer_panel")
        .resizable(true)
        .default_size(280.0)
        // The properties panel below the tree lays its fields out at a fixed
        // minimum width; keep the panel wide enough to hold them rather than
        // clipping their right-hand controls.
        .min_size(220.0)
        .frame(egui::Frame::side_top_panel(ui.style()).fill(surface).inner_margin(egui::Margin::ZERO))
        .show(ui, |ui| {
            // Prevent content from forcing the panel wider than the user has dragged it.
            ui.set_max_width(ui.available_width());

            crate::ui::elements::toolbars::draw_explorer_toolbar(ui, project, commands, can_undo, can_redo);
            draw_properties(ui, editor, block_models, document, commands, geometry_dirty);

            // Sections other than Projects belong to the active project, so
            // they follow its contents: see `ExplorerHeader::auto_open`.
            let epoch = project.active_project_epoch;

            // Keep the scroll area's contents as wide as the side panel even
            // when every section is collapsed. `ScrollArea` otherwise shrinks
            // horizontally to the headers' intrinsic width; a visible
            // `ExplorerEntry` masks that by requesting all available width,
            // which made panel resizing depend on whether an entry existed.
            let mut row_index = 0usize;
            // Vertical shrinking is off so the banding below the last row has
            // the full panel height to run into: see `fill_trailing_stripes`.
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                // Empty-state messages should behave like explorer entries at
                // narrow widths: stay on one line and end with an ellipsis.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                // Rows carry their own height and butt up against each other,
                // so the stripes tile the list without gaps.
                ui.spacing_mut().item_spacing.y = 0.0;

                ExplorerHeader::new(egui::Id::new("projects_collapse"), "Projects")
                    .icon(unthemed_icon!("section_projects.svg"))
                    .stripe(next_stripe(&mut row_index), stripe)
                    .color(HEADER_PROJECTS)
                    .dirty(project.tracked_projects.iter().any(|entry| entry.dirty))
                    .default_open(true)
                    .show(ui, |ui| {
                        if project.tracked_projects.is_empty() {
                            explorer_note(ui, "No tracked projects", next_stripe(&mut row_index), stripe);
                        }
                        // Only the active project can be deactivated, and closing it is
                        // exactly that: no project active, so the welcome splash returns.
                        let active_runtime_id = project.projects.iter().find(|entry| entry.is_active).map(|entry| entry.runtime_id);
                        for tracked in &project.tracked_projects {
                            let title = if tracked.dirty { format!("{} *", tracked.name) } else { tracked.name.clone() };
                            #[cfg(not(target_arch = "wasm32"))]
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(
                                        egui::Id::new(("explorer_project", &tracked.path)),
                                        if tracked.is_active { bold(&title) } else { egui::RichText::new(&title) },
                                    )
                                    .selected(tracked.is_active)
                                    .stripe(next_stripe(&mut row_index), stripe),
                                ),
                                &tracked.path.display().to_string(),
                                Some(&tracked.path),
                            );
                            #[cfg(target_arch = "wasm32")]
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(
                                        egui::Id::new(("explorer_project", tracked.id)),
                                        if tracked.is_active { bold(&title) } else { egui::RichText::new(&title) },
                                    )
                                    .selected(tracked.is_active)
                                    .stripe(next_stripe(&mut row_index), stripe),
                                ),
                                if tracked.stored_in_browser {
                                    "Saved in browser storage"
                                } else {
                                    "Not saved in browser storage"
                                },
                                None,
                            );

                            if response.double_clicked() && !tracked.is_active {
                                #[cfg(not(target_arch = "wasm32"))]
                                commands.push(UiCommand::ActivateTrackedProject(tracked.path.clone()));
                                #[cfg(target_arch = "wasm32")]
                                commands.push(UiCommand::ActivateTrackedProject(tracked.id));
                            }
                            response.context_menu(|ui| {
                                // Deactivating a dirty project routes through the save/discard/cancel dialog.
                                if let Some(runtime_id) = active_runtime_id.filter(|_| tracked.is_active) {
                                    if ui.button("Deactivate Project").clicked() {
                                        commands.push(UiCommand::CloseProject(runtime_id));
                                        ui.close();
                                    }
                                } else if ui.button("Activate Project").clicked() {
                                    #[cfg(not(target_arch = "wasm32"))]
                                    commands.push(UiCommand::ActivateTrackedProject(tracked.path.clone()));
                                    #[cfg(target_arch = "wasm32")]
                                    commands.push(UiCommand::ActivateTrackedProject(tracked.id));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Remove Project").clicked() {
                                    #[cfg(not(target_arch = "wasm32"))]
                                    commands.push(UiCommand::RemoveTrackedProject(tracked.path.clone()));
                                    #[cfg(target_arch = "wasm32")]
                                    commands.push(UiCommand::RemoveTrackedProject(tracked.id));
                                    ui.close();
                                }
                            });
                        }
                    });

                let designs_dirty = project.projects.first().is_some_and(|entry| entry.designs_dirty);
                ExplorerHeader::new(egui::Id::new("designs_collapse"), "Designs")
                    .icon(unthemed_icon!("layer.svg"))
                    .stripe(next_stripe(&mut row_index), stripe)
                    .color(HEADER_DESIGNS)
                    .dirty(designs_dirty)
                    .auto_open(project.projects.first().map_or(0, |entry| entry.layers.len()), epoch)
                    .show(ui, |ui| {
                        let Some(entry) = project.projects.first() else {
                            explorer_note(ui, "No open project", next_stripe(&mut row_index), stripe);
                            return;
                        };
                        if entry.layers.is_empty() {
                            explorer_note(ui, "No design layers", next_stripe(&mut row_index), stripe);
                        }
                        for layer in &entry.layers {
                            let layer_id = layer.id;
                            let is_active = editor.active_layer == Some(layer_id);
                            let layer_name = if layer.dirty { format!("{} *", layer.name) } else { layer.name.clone() };
                            let layer_label = if layer.is_loaded {
                                bold(&layer_name)
                            } else {
                                egui::RichText::new(&layer_name).color(INACTIVE_TEXT_COLOR)
                            };
                            let layer_resp = ui.add(
                                ExplorerEntry::new(egui::Id::new(("explorer_layer", layer_id)), layer_label)
                                    .selected(is_active)
                                    .stripe(next_stripe(&mut row_index), stripe),
                            );
                            if layer_resp.double_clicked() {
                                if layer.is_loaded {
                                    commands.push(UiCommand::UnloadLayer(layer_id));
                                } else {
                                    commands.push(UiCommand::LoadLayer(layer_id));
                                }
                            }
                            layer_resp.context_menu(|ui| {
                                if layer.is_loaded {
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::UnloadLayer(layer_id));
                                        ui.close();
                                    }
                                    if ui.button("Select All Objects").clicked() {
                                        commands.push(UiCommand::SelectAllObjectsInLayer(layer_id));
                                        ui.close();
                                    }
                                } else if ui.button("Load").clicked() {
                                    commands.push(UiCommand::LoadLayer(layer_id));
                                    ui.close();
                                }
                                if ui.button("Rename").clicked() {
                                    commands.push(UiCommand::BeginRenameLayer(layer_id));
                                    ui.close();
                                }
                                if ui.button("Duplicate").clicked() {
                                    commands.push(UiCommand::DuplicateLayer(layer_id));
                                    ui.close();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                if layer.dirty && entry.path.is_some() && ui.button("Discard Changes...").clicked() {
                                    commands.push(UiCommand::RequestDiscardLayerChanges(layer_id));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Delete from Project").clicked() {
                                    commands.push(UiCommand::RequestDeleteLayer(layer_id));
                                    ui.close();
                                }
                            });
                        }
                    });

                let triangulations_dirty = project.triangulations_membership_dirty || project.triangulations.iter().any(|item| item.dirty);
                ExplorerHeader::new(egui::Id::new("triangulations_collapse"), "Triangulations")
                    .icon(unthemed_icon!("triangulation.svg"))
                    .stripe(next_stripe(&mut row_index), stripe)
                    .color(HEADER_TRIANGULATIONS)
                    .dirty(triangulations_dirty)
                    .auto_open(project.triangulations.len(), epoch)
                    .show(ui, |ui| {
                        if project.triangulations.is_empty() {
                            explorer_note(ui, "No triangulations", next_stripe(&mut row_index), stripe);
                        }

                        // Helper closure: render one tri entry row and attach its context menu.
                        let render_tri_entry = |ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, tri: &crate::ui::UiTriangulationEntry, stripe_index: usize| {
                            let tri_path = format!(
                                "ID: triangulation:{}{}",
                                tri.id.0,
                                tri.source_name.as_deref().map(|name| format!("\nSource: {name}")).unwrap_or_default()
                            );
                            let tri_id = tri.id;

                            let label = if tri.is_loaded {
                                let dirty_marker = if tri.dirty { " *" } else { "" };
                                let stats = format!("{}{}", tri.name, dirty_marker);
                                bold(&stats)
                            } else {
                                egui::RichText::new(&tri.name).color(INACTIVE_TEXT_COLOR)
                            };

                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(egui::Id::new(("explorer_triangulation", tri.id)), label)
                                        .selected(tri.is_active)
                                        .stripe(stripe_index, stripe),
                                ),
                                &tri_path,
                                None,
                            );

                            if response.double_clicked() {
                                if tri.is_loaded {
                                    commands.push(UiCommand::CloseTriangulation(tri_id));
                                } else {
                                    commands.push(UiCommand::LoadTriangulation(tri_id));
                                }
                            } else if response.clicked() && tri.is_loaded {
                                commands.push(UiCommand::ActivateTriangulation(tri_id));
                            }

                            let tri_loaded = tri.is_loaded;
                            let tri_visible = tri.visible;
                            response.context_menu(|ui| {
                                if tri_loaded {
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::CloseTriangulation(tri_id));
                                        ui.close();
                                    }
                                    if ui.button(if tri_visible { "Hide" } else { "Show" }).clicked() {
                                        commands.push(UiCommand::ToggleTriangulationVisible(tri_id));
                                        ui.close();
                                    }
                                } else if ui.button("Load").clicked() {
                                    commands.push(UiCommand::LoadTriangulation(tri_id));
                                    ui.close();
                                }
                                #[cfg(target_arch = "wasm32")]
                                if ui.button("Download").clicked() {
                                    commands.push(UiCommand::ExportTriangulationAs(tri_id, crate::model::formats::MeshFormat::Obj));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Delete from Project").clicked() {
                                    commands.push(UiCommand::RemoveTriangulation(tri_id));
                                    ui.close();
                                }
                            });
                        };

                        for triangulation in &project.triangulations {
                            render_tri_entry(ui, commands, triangulation, next_stripe(&mut row_index));
                        }
                    });

                let rasters_dirty = project.rasters_membership_dirty || project.raster_textures.iter().any(|item| item.dirty);
                ExplorerHeader::new("rasters_collapse".into(), "Rasters")
                    .icon(unthemed_icon!("raster.svg"))
                    .stripe(next_stripe(&mut row_index), stripe)
                    .color(HEADER_RASTERS)
                    .dirty(rasters_dirty)
                    .auto_open(project.raster_textures.len(), epoch)
                    .show(ui, |ui| {
                        if project.raster_textures.is_empty() {
                            explorer_note(ui, "No image textures", next_stripe(&mut row_index), stripe);
                        }
                        for raster in &project.raster_textures {
                            let raster_label = if raster.dirty { format!("{} *", raster.name) } else { raster.name.clone() };
                            let label = if raster.is_loaded {
                                bold(&raster_label)
                            } else {
                                egui::RichText::new(&raster_label).color(INACTIVE_TEXT_COLOR)
                            };
                            let details = format!(
                                "ID: raster:{}{}\n{} · {} × {}\n{}",
                                raster.id.0,
                                raster.source_name.as_deref().map(|name| format!("\nSource: {name}")).unwrap_or_default(),
                                raster.driver_name,
                                raster.source_size[0],
                                raster.source_size[1],
                                raster.projection
                            );
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(egui::Id::new(("explorer_raster", raster.id)), label)
                                        .selected(raster.is_draped)
                                        .stripe(next_stripe(&mut row_index), stripe),
                                ),
                                &details,
                                None,
                            );
                            if response.double_clicked() {
                                if raster.is_loaded {
                                    commands.push(UiCommand::UnloadRaster(raster.id));
                                } else {
                                    commands.push(UiCommand::LoadRaster(raster.id));
                                }
                            }
                            response.context_menu(|ui| {
                                if raster.is_loaded {
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::UnloadRaster(raster.id));
                                        ui.close();
                                    }
                                    if ui.button(if raster.visible { "Hide" } else { "Show" }).clicked() {
                                        commands.push(UiCommand::ToggleRasterVisible(raster.id));
                                        ui.close();
                                    }
                                    if ui.button("Drape Over Surface").clicked() {
                                        commands.push(UiCommand::DrapeRaster(raster.id));
                                        ui.close();
                                    }
                                } else if ui.button("Load").clicked() {
                                    commands.push(UiCommand::LoadRaster(raster.id));
                                    ui.close();
                                }
                                // Unloading a raster keeps its drape, so offer the undrape in both states.
                                if raster.is_draped && ui.button("Undrape All").clicked() {
                                    commands.push(UiCommand::UndrapeRaster(raster.id));
                                    ui.close();
                                }
                                if project.active_triangulation_for_menu.is_some() && ui.button("Clear Active Triangulation Texture").clicked() {
                                    commands.push(UiCommand::ClearActiveTriangulationRaster);
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Delete from Project").clicked() {
                                    commands.push(UiCommand::RemoveRaster(raster.id));
                                    ui.close();
                                }
                            });
                        }
                    });

                let point_clouds_dirty = project.point_clouds_membership_dirty || project.point_clouds.iter().any(|item| item.dirty);
                ExplorerHeader::new(egui::Id::new("point_clouds_collapse"), "Point Clouds")
                    .icon(unthemed_icon!("section_point_clouds.svg"))
                    .stripe(next_stripe(&mut row_index), stripe)
                    .color(HEADER_POINT_CLOUDS)
                    .dirty(point_clouds_dirty)
                    .auto_open(project.point_clouds.len(), epoch)
                    .show(ui, |ui| {
                        if project.point_clouds.is_empty() {
                            explorer_note(ui, "No point clouds", next_stripe(&mut row_index), stripe);
                        }
                        for point_cloud in &project.point_clouds {
                            let dirty_marker = if point_cloud.dirty { " *" } else { "" };
                            let label_text = format!("{}{dirty_marker}", point_cloud.name);
                            let label = if point_cloud.is_loaded {
                                bold(&label_text)
                            } else {
                                egui::RichText::new(&label_text).color(INACTIVE_TEXT_COLOR)
                            };
                            let tooltip = format!(
                                "ID: point-cloud:{}{}\n{} point(s)",
                                point_cloud.id.0,
                                point_cloud.source_name.as_deref().map(|name| format!("\nSource: {name}")).unwrap_or_default(),
                                point_cloud.point_count
                            );
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(egui::Id::new(("explorer_point_cloud", point_cloud.id)), label).stripe(next_stripe(&mut row_index), stripe),
                                ),
                                &tooltip,
                                None,
                            );

                            if response.double_clicked() {
                                if point_cloud.is_loaded {
                                    commands.push(UiCommand::ClosePointCloud(point_cloud.id));
                                } else {
                                    commands.push(UiCommand::LoadPointCloud(point_cloud.id));
                                }
                            }

                            response.context_menu(|ui| {
                                if point_cloud.is_loaded {
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::ClosePointCloud(point_cloud.id));
                                        ui.close();
                                    }
                                    if ui.button(if point_cloud.visible { "Hide" } else { "Show" }).clicked() {
                                        commands.push(UiCommand::TogglePointCloudVisible(point_cloud.id));
                                        ui.close();
                                    }
                                } else if ui.button("Load").clicked() {
                                    commands.push(UiCommand::LoadPointCloud(point_cloud.id));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Delete from Project").clicked() {
                                    commands.push(UiCommand::RemovePointCloud(point_cloud.id));
                                    ui.close();
                                }
                            });
                        }
                    });

                let block_models_dirty = project.block_models_membership_dirty || project.block_models.iter().any(|item| item.dirty);
                ExplorerHeader::new(egui::Id::new("block_models_collapse"), "Block Models")
                    .icon(unthemed_icon!("section_block_models.svg"))
                    .stripe(next_stripe(&mut row_index), stripe)
                    .color(HEADER_BLOCK_MODELS)
                    .dirty(block_models_dirty)
                    .auto_open(project.block_models.len(), epoch)
                    .show(ui, |ui| {
                        if project.block_models.is_empty() {
                            explorer_note(ui, "No block models", next_stripe(&mut row_index), stripe);
                        }
                        for block_model in &project.block_models {
                            let is_selected = editor.selected_handles.contains(&crate::model::SceneEntityId::BlockModel(block_model.id));
                            let dirty_marker = if block_model.dirty { " *" } else { "" };
                            let label_text = format!("{}{dirty_marker}", block_model.name);
                            let label = if block_model.is_loaded {
                                bold(&label_text)
                            } else {
                                egui::RichText::new(&label_text).color(INACTIVE_TEXT_COLOR)
                            };
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(egui::Id::new(("explorer_block_model", block_model.id)), label)
                                        .selected(is_selected)
                                        .stripe(next_stripe(&mut row_index), stripe),
                                ),
                                &format!(
                                    "ID: block-model:{}{}\n{} colour variable(s)",
                                    block_model.id.0,
                                    block_model.source_name.as_deref().map(|name| format!("\nSource: {name}")).unwrap_or_default(),
                                    block_model.variable_count
                                ),
                                None,
                            );
                            if response.double_clicked() {
                                if block_model.is_loaded {
                                    commands.push(UiCommand::CloseBlockModel(block_model.id));
                                } else {
                                    commands.push(UiCommand::LoadBlockModel(block_model.id));
                                }
                            } else if response.clicked() && block_model.is_loaded {
                                // Selecting here is what reveals the model's
                                // properties tab, the same as picking it in
                                // the viewport does.
                                commands.push(UiCommand::SelectBlockModel(block_model.id));
                            }

                            response.context_menu(|ui| {
                                if block_model.is_loaded {
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::CloseBlockModel(block_model.id));
                                        ui.close();
                                    }
                                    if ui.button(if block_model.visible { "Hide" } else { "Show" }).clicked() {
                                        commands.push(UiCommand::ToggleBlockModelVisible(block_model.id));
                                        ui.close();
                                    }
                                } else if ui.button("Load").clicked() {
                                    commands.push(UiCommand::LoadBlockModel(block_model.id));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Delete from Project").clicked() {
                                    commands.push(UiCommand::RemoveBlockModel(block_model.id));
                                    ui.close();
                                }
                            });
                        }
                    });

                let drill_holes_dirty = project.drill_holes_membership_dirty || project.drill_holes.iter().any(|item| item.dirty);
                ExplorerHeader::new(egui::Id::new("drill_holes_collapse"), "Drill Holes")
                    .icon(unthemed_icon!("drill_hole.svg"))
                    .stripe(next_stripe(&mut row_index), stripe)
                    .color(HEADER_DRILL_HOLES)
                    .dirty(drill_holes_dirty)
                    .auto_open(project.drill_holes.len(), epoch)
                    .show(ui, |ui| {
                        if project.drill_holes.is_empty() {
                            explorer_note(ui, "No drill holes", next_stripe(&mut row_index), stripe);
                        }
                        for dataset in &project.drill_holes {
                            let dataset_label = if dataset.dirty { format!("{} *", dataset.name) } else { dataset.name.clone() };
                            let label = if dataset.is_loaded {
                                bold(&dataset_label)
                            } else {
                                egui::RichText::new(&dataset_label).color(INACTIVE_TEXT_COLOR)
                            };
                            let tooltip = format!(
                                "ID: drill-holes:{}{}\n{} hole(s)\n{} colour field(s)",
                                dataset.id.0,
                                dataset.source_name.as_deref().map(|name| format!("\nSource: {name}")).unwrap_or_default(),
                                dataset.hole_count,
                                dataset.field_count
                            );
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(egui::Id::new(("explorer_drill_hole", dataset.id)), label).stripe(next_stripe(&mut row_index), stripe),
                                ),
                                &tooltip,
                                None,
                            );
                            if response.double_clicked() {
                                if dataset.is_loaded {
                                    commands.push(UiCommand::CloseDrillHole(dataset.id));
                                } else {
                                    commands.push(UiCommand::LoadDrillHole(dataset.id));
                                }
                            }
                            response.context_menu(|ui| {
                                if dataset.is_loaded {
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::CloseDrillHole(dataset.id));
                                        ui.close();
                                    }
                                    if ui.button(if dataset.visible { "Hide" } else { "Show" }).clicked() {
                                        commands.push(UiCommand::ToggleDrillHoleVisible(dataset.id));
                                        ui.close();
                                    }
                                    if ui.button("Colour by...").clicked() {
                                        commands.push(UiCommand::OpenDrillHoleColorDialog(dataset.id));
                                        ui.close();
                                    }
                                } else if ui.button("Load").clicked() {
                                    commands.push(UiCommand::LoadDrillHole(dataset.id));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Delete from Project").clicked() {
                                    commands.push(UiCommand::RemoveDrillHole(dataset.id));
                                    ui.close();
                                }
                            });
                        }
                    });

                fill_trailing_stripes(ui, row_index, stripe);
            });
        })
        .response
        .rect
}
