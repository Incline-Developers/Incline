//! Left-side explorer panel for the active project's retained content.

use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use crate::ui::{
    EditorState, UiCommand, UiProjectView,
    fonts::bold,
    unthemed_icon,
    widgets::explorer::{ExplorerEntry, ExplorerHeader},
};

/// Grey colour used for inactive (not loaded) layers and triangulations.
const INACTIVE_TEXT_COLOR: egui::Color32 = egui::Color32::from_gray(140);

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

/// Draw the left explorer panel.
///
/// Shows the active project path and the collapsible data sections. Returns the
/// panel's bounding rect.
pub(crate) fn draw_explorer(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) -> egui::Rect {
    let panel_fill = if ui.visuals().dark_mode { ui.visuals().panel_fill } else { egui::Color32::WHITE };
    egui::Panel::left("explorer_panel")
        .resizable(true)
        .default_size(250.0)
        .min_size(180.0)
        .frame(egui::Frame::side_top_panel(ui.style()).fill(panel_fill))
        .show(ui, |ui| {
            // Prevent content from forcing the panel wider than the user has dragged it.
            ui.set_max_width(ui.available_width());

            ui.add_space(5.);

            // Keep the scroll area's contents as wide as the side panel even
            // when every section is collapsed. `ScrollArea` otherwise shrinks
            // horizontally to the headers' intrinsic width; a visible
            // `ExplorerEntry` masks that by requesting all available width,
            // which made panel resizing depend on whether an entry existed.
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                // Empty-state messages should behave like explorer entries at
                // narrow widths: stay on one line and end with an ellipsis.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);

                ExplorerHeader::new(egui::Id::new("projects_collapse"), "Projects")
                    .dirty(project.tracked_projects.iter().any(|entry| entry.dirty))
                    .default_open(true)
                    .show(ui, |ui| {
                        if project.tracked_projects.is_empty() {
                            ui.label("No tracked projects");
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
                                        unthemed_icon!("open_mining_format.svg"),
                                        if tracked.is_active { bold(&title) } else { egui::RichText::new(&title) },
                                    )
                                    .selected(tracked.is_active),
                                ),
                                &tracked.path.display().to_string(),
                                Some(&tracked.path),
                            );
                            #[cfg(target_arch = "wasm32")]
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(
                                        egui::Id::new(("explorer_project", tracked.id)),
                                        unthemed_icon!("open_mining_format.svg"),
                                        if tracked.is_active { bold(&title) } else { egui::RichText::new(&title) },
                                    )
                                    .selected(tracked.is_active),
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
                    .dirty(designs_dirty)
                    .default_open(true)
                    .show(ui, |ui| {
                        let Some(entry) = project.projects.first() else {
                            ui.label("No open project");
                            return;
                        };
                        if entry.layers.is_empty() {
                            ui.label("No design layers");
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
                            let layer_resp = ui.add(ExplorerEntry::new(egui::Id::new(("explorer_layer", layer_id)), unthemed_icon!("layer.svg"), layer_label).selected(is_active));
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
                    .dirty(triangulations_dirty)
                    .default_open(!project.triangulations.is_empty())
                    .show(ui, |ui| {
                        if project.triangulations.is_empty() {
                            ui.label("No triangulations");
                        }

                        // Helper closure: render one tri entry row and attach its context menu.
                        let render_tri_entry = |ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, tri: &crate::ui::UiTriangulationEntry, _removable: bool| {
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

                            let icon = unthemed_icon!("triangulation.svg");
                            let response = on_hover_file_details(
                                ui.add(ExplorerEntry::new(egui::Id::new(("explorer_triangulation", tri.id)), icon, label).selected(tri.is_active)),
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
                            render_tri_entry(ui, commands, triangulation, true);
                        }
                    });

                let rasters_dirty = project.rasters_membership_dirty || project.raster_textures.iter().any(|item| item.dirty);
                ExplorerHeader::new("rasters_collapse".into(), "Rasters")
                    .dirty(rasters_dirty)
                    .default_open(!project.raster_textures.is_empty())
                    .show(ui, |ui| {
                        if project.raster_textures.is_empty() {
                            ui.label("No image textures");
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
                                ui.add(ExplorerEntry::new(egui::Id::new(("explorer_raster", raster.id)), unthemed_icon!("raster.svg"), label).selected(raster.is_draped)),
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
                    .dirty(point_clouds_dirty)
                    .default_open(!project.point_clouds.is_empty())
                    .show(ui, |ui| {
                        if project.point_clouds.is_empty() {
                            ui.label("No point clouds");
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
                                ui.add(ExplorerEntry::new(
                                    egui::Id::new(("explorer_point_cloud", point_cloud.id)),
                                    unthemed_icon!("point_clouds_section.svg"),
                                    label,
                                )),
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
                    .dirty(block_models_dirty)
                    .default_open(!project.block_models.is_empty())
                    .show(ui, |ui| {
                        if project.block_models.is_empty() {
                            ui.label("No block models");
                        }
                        for block_model in &project.block_models {
                            let dirty_marker = if block_model.dirty { " *" } else { "" };
                            let label_text = format!("{}{dirty_marker}", block_model.name);
                            let label = if block_model.is_loaded {
                                bold(&label_text)
                            } else {
                                egui::RichText::new(&label_text).color(INACTIVE_TEXT_COLOR)
                            };
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(egui::Id::new(("explorer_block_model", block_model.id)), unthemed_icon!("block_models_section.svg"), label)
                                        .selected(block_model.is_active),
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
                    .dirty(drill_holes_dirty)
                    .default_open(!project.drill_holes.is_empty())
                    .show(ui, |ui| {
                        if project.drill_holes.is_empty() {
                            ui.label("No drill holes");
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
                                ui.add(ExplorerEntry::new(
                                    egui::Id::new(("explorer_drill_hole", dataset.id)),
                                    unthemed_icon!("drill_holes_section.svg"),
                                    label,
                                )),
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
            });
        })
        .response
        .rect
}
