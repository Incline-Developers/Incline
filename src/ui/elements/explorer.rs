//! Left-side explorer panel: design databases (.pidb) and triangulations.

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

fn pidb_project_collapse_id(path: Option<&Path>, runtime_id: u32) -> egui::Id {
    path.map_or_else(|| egui::Id::new(("pidb_project_unsaved", runtime_id)), |path| egui::Id::new(("pidb_project_path", path)))
}

/// Draw the left explorer panel.
///
/// Shows the active PIDB path and the collapsible data sections. Returns the
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

                ExplorerHeader::new(egui::Id::new("design_db_colapse"), unthemed_icon!("design_database.svg"), bold("Design Databases"))
                    .default_open(true)
                    .show(ui, |ui| {
                        if project.projects.is_empty() {
                            ui.label("No open .pidb");
                        }
                        for entry in &project.projects {
                            // Every project in this section is a PIDB, so the extension adds no
                            // useful distinction to its display name.
                            let stem = entry.name.strip_suffix(".pidb").unwrap_or(&entry.name);
                            let title = if entry.dirty { format!("{stem} *") } else { stem.to_owned() };
                            #[cfg(not(target_arch = "wasm32"))]
                            let path_tooltip = entry.path.as_deref().and_then(|p| p.to_str()).unwrap_or("Unsaved").to_owned();
                            #[cfg(target_arch = "wasm32")]
                            let path_tooltip = entry.path.as_deref().and_then(|p| p.to_str()).map(str::to_owned).unwrap_or_else(|| {
                                if entry.stored_in_browser {
                                    "Saved in browser storage".to_owned()
                                } else {
                                    "Not saved in browser storage".to_owned()
                                }
                            });

                            let runtime_id = entry.runtime_id;
                            // Reverting a PIDB intentionally gives its model a new
                            // runtime namespace. Its explorer row is still the same
                            // saved file, so keep the UI identity path-based across
                            // that swap to avoid resetting the collapse state and
                            // changing widget ids at the same screen position.
                            let collapse_id = pidb_project_collapse_id(entry.path.as_deref(), runtime_id);

                            let contains_active_layer = editor.active_layer.is_some_and(|active_layer| entry.layers.iter().any(|layer| layer.id == active_layer));
                            let label = if entry.is_active || contains_active_layer {
                                bold(&title).color(crate::ui::SELECTION_COLOR)
                            } else {
                                bold(&title)
                            };

                            let is_any_layer_loaded = entry.layers.iter().any(|layer| layer.is_loaded);
                            let project_icon = if is_any_layer_loaded {
                                unthemed_icon!("data_loaded.svg")
                            } else {
                                unthemed_icon!("data_unloaded.svg")
                            };

                            let (toggle_response, header_response, _body_response) = ExplorerHeader::new(collapse_id, project_icon, label).default_open(true).show(ui, |ui| {
                                if entry.layers.is_empty() {
                                    ui.label("No layers under this .pidb");
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
                                    let layer_resp =
                                        ui.add(ExplorerEntry::new(egui::Id::new(("explorer_layer", layer_id)), unthemed_icon!("layer.svg"), layer_label).selected(is_active));
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
                                        } else if ui.button("Load").clicked() {
                                            commands.push(UiCommand::LoadLayer(layer_id));
                                            ui.close();
                                        }
                                        if layer.is_loaded && ui.button("Select All Objects").clicked() {
                                            commands.push(UiCommand::SelectAllObjectsInLayer(layer_id));
                                            ui.close();
                                        }
                                        if ui.button("Rename").clicked() {
                                            commands.push(UiCommand::BeginRenameLayer(layer_id));
                                            ui.close();
                                        }
                                        #[cfg(not(target_arch = "wasm32"))]
                                        if layer.dirty && entry.path.is_some() && ui.button("Discard Changes…").clicked() {
                                            commands.push(UiCommand::RequestDiscardLayerChanges(layer_id));
                                            ui.close();
                                        }
                                        if ui.button("Duplicate Layer").clicked() {
                                            commands.push(UiCommand::DuplicateLayer(layer_id));
                                            ui.close();
                                        }
                                        if ui.button("Move Layer...").clicked() {
                                            commands.push(UiCommand::BeginMoveLayer(layer_id));
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button("Delete Layer").clicked() {
                                            commands.push(UiCommand::RequestDeleteLayer(layer_id));
                                            ui.close();
                                        }
                                    });
                                }
                            });

                            let content_response = header_response.inner;
                            let should_activate = toggle_response.double_clicked() || header_response.response.double_clicked();
                            let project_header_response = on_hover_file_details(
                                toggle_response.union(header_response.response).union(content_response.clone()),
                                &path_tooltip,
                                entry.path.as_deref(),
                            );

                            if should_activate {
                                commands.push(UiCommand::ActivatePidb(runtime_id));
                            }

                            project_header_response.context_menu(|ui| {
                                if !entry.is_active && ui.button("Activate").clicked() {
                                    commands.push(UiCommand::ActivatePidb(runtime_id));
                                    ui.close();
                                }
                                if ui.button("Save").clicked() {
                                    commands.push(UiCommand::SavePidb(runtime_id));
                                    ui.close();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                if ui.button("Save As…").clicked() {
                                    commands.push(UiCommand::SavePidbAs(runtime_id));
                                    ui.close();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                if entry.dirty && entry.path.is_some() && ui.button("Discard Changes…").clicked() {
                                    commands.push(UiCommand::RequestDiscardPidbChanges(runtime_id));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Load All Layers").clicked() {
                                    commands.push(UiCommand::LoadAllLayers(runtime_id));
                                    ui.close();
                                }
                                if ui.button("Unload All Layers").clicked() {
                                    commands.push(UiCommand::UnloadAllLayers(runtime_id));
                                    ui.close();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                if entry.path.is_some() && ui.button("Reveal in Explorer").clicked() {
                                    if let Some(path) = entry.path.clone() {
                                        commands.push(UiCommand::RevealPidb(path));
                                    }
                                    ui.close();
                                }
                                ui.separator();
                                #[cfg(not(target_arch = "wasm32"))]
                                if ui.button("Close PIDB").clicked() {
                                    commands.push(UiCommand::ClosePidb(runtime_id));
                                    ui.close();
                                }
                                #[cfg(target_arch = "wasm32")]
                                if ui.button("Delete Project").clicked() {
                                    commands.push(UiCommand::ClosePidb(runtime_id));
                                    ui.close();
                                }
                            });
                        }
                    });

                ExplorerHeader::new(egui::Id::new("triangulation_colapse"), unthemed_icon!("triangulations_section.svg"), bold("Triangulations"))
                    .default_open(!project.triangulations.is_empty())
                    .show(ui, |ui| {
                        if project.triangulations.is_empty() {
                            ui.label("No open triangulations");
                        }

                        // Helper closure: render one tri entry row and attach its context menu.
                        let render_tri_entry = |ui: &mut egui::Ui, commands: &mut Vec<UiCommand>, tri: &crate::ui::UiTriangulationEntry, removable: bool| {
                            let tri_path = if tri.is_saved {
                                tri.path.to_str().unwrap_or("").to_owned()
                            } else {
                                "Unsaved triangulation".to_owned()
                            };
                            let tri_path_buf = tri.path.clone();

                            let label = if tri.is_loaded {
                                let dirty_marker = if tri.is_saved { "" } else { " *" };
                                let stats = format!("{}{}", tri.name, dirty_marker);
                                bold(&stats)
                            } else {
                                egui::RichText::new(&tri.name).color(INACTIVE_TEXT_COLOR)
                            };

                            let icon = unthemed_icon!("triangulation.svg");
                            let response = on_hover_file_details(
                                ui.add(ExplorerEntry::new(egui::Id::new(("explorer_triangulation", &tri.path)), icon, label).selected(tri.is_active)),
                                &tri_path,
                                tri.is_saved.then_some(tri.path.as_path()),
                            );

                            if response.double_clicked() {
                                if tri.is_loaded {
                                    commands.push(UiCommand::CloseTriangulation(tri.id.unwrap()));
                                } else {
                                    commands.push(UiCommand::LoadTriangulation(tri_path_buf.clone()));
                                }
                            } else if response.clicked() && tri.is_loaded {
                                commands.push(UiCommand::ActivateTriangulation(tri.id.unwrap()));
                            }

                            if tri.is_loaded {
                                let tri_id = tri.id.unwrap();
                                let tri_visible = tri.visible;
                                #[cfg(not(target_arch = "wasm32"))]
                                let tri_saved = tri.is_saved;
                                response.context_menu(|ui| {
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::CloseTriangulation(tri_id));
                                        ui.close();
                                    }

                                    if ui.button(if tri_visible { "Hide" } else { "Show" }).clicked() {
                                        commands.push(UiCommand::ToggleTriangulationVisible(tri_id));
                                        ui.close();
                                    }
                                    ui.separator();
                                    #[cfg(target_arch = "wasm32")]
                                    if ui.button("Download").clicked() {
                                        commands.push(UiCommand::ExportTriangulationAs(tri_id, crate::model::formats::MeshFormat::Obj));
                                        ui.close();
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if tri_saved && ui.button("Reveal in Explorer").clicked() {
                                        commands.push(UiCommand::RevealTriangulation(tri_id));
                                        ui.close();
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if !tri_saved && ui.button("Save As...").clicked() {
                                        commands.push(UiCommand::SaveTriangulationAs(tri_id));
                                        ui.close();
                                    }
                                    // Browser triangulations have no file to stop tracking:
                                    // removing one deletes it from browser storage.
                                    #[cfg(target_arch = "wasm32")]
                                    if removable {
                                        ui.separator();
                                        if ui.button("Delete").clicked() {
                                            commands.push(UiCommand::RemoveTriangulation(tri_path_buf.clone()));
                                            ui.close();
                                        }
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if tri_saved && removable && ui.button("Remove Triangulation").clicked() {
                                        ui.separator();
                                        commands.push(UiCommand::RemoveTriangulation(tri_path_buf.clone()));
                                        ui.close();
                                    }
                                });
                            } else {
                                response.context_menu(|ui| {
                                    ui.set_min_width(70.);
                                    if ui.button("Load").clicked() {
                                        commands.push(UiCommand::LoadTriangulation(tri_path_buf.clone()));
                                        ui.close();
                                    }
                                    #[cfg(target_arch = "wasm32")]
                                    if ui.button("Download").clicked() {
                                        commands.push(UiCommand::DownloadStoredTriangulation(tri_path_buf.clone()));
                                        ui.close();
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if tri.is_saved && ui.button("Reveal in Explorer").clicked() {
                                        commands.push(UiCommand::RevealPath(tri_path_buf.clone()));
                                        ui.close();
                                    }
                                    if removable {
                                        ui.separator();
                                        #[cfg(target_arch = "wasm32")]
                                        let remove_label = "Delete";
                                        #[cfg(not(target_arch = "wasm32"))]
                                        let remove_label = "Remove Triangulation";
                                        if ui.button(remove_label).clicked() {
                                            commands.push(UiCommand::RemoveTriangulation(tri_path_buf.clone()));
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        };

                        let mut tri_index = 0;
                        while tri_index < project.triangulations.len() {
                            let tri = &project.triangulations[tri_index];
                            let Some(dir) = tri.group.as_ref() else {
                                render_tri_entry(ui, commands, tri, true);
                                tri_index += 1;
                                continue;
                            };

                            let dir = dir.clone();
                            let folder_start = tri_index;
                            tri_index += 1;
                            while tri_index < project.triangulations.len() && project.triangulations[tri_index].group.as_deref() == Some(dir.as_path()) {
                                tri_index += 1;
                            }
                            let folder_tris = &project.triangulations[folder_start..tri_index];
                            let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or(dir.to_str().unwrap_or("folder")).to_owned();
                            let dir_for_menu = dir.clone();
                            let collapse_id = ui.make_persistent_id(("tri_folder", &dir));
                            let any_loaded = folder_tris.iter().any(|t| t.is_loaded);
                            let any_unloaded = folder_tris.iter().any(|t| !t.is_loaded);
                            let img = if any_loaded {
                                unthemed_icon!("data_loaded.svg")
                            } else {
                                unthemed_icon!("data_unloaded.svg")
                            };
                            let (toggle_resp, header_resp, _) = ExplorerHeader::new(collapse_id, img, bold(&dir_name)).show(ui, |ui| {
                                for tri in folder_tris {
                                    render_tri_entry(ui, commands, tri, false);
                                }
                            });
                            let folder_response = toggle_resp.union(header_resp.response).union(header_resp.inner);
                            folder_response.context_menu(|ui| {
                                if any_unloaded && ui.button("Load All").clicked() {
                                    commands.push(UiCommand::ConfirmLoadAllTriangulationsInFolder(dir_for_menu.clone()));
                                    ui.close();
                                }
                                if any_loaded && ui.button("Unload All").clicked() {
                                    commands.push(UiCommand::CloseAllTriangulationsInFolder(dir_for_menu.clone()));
                                    ui.close();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                if ui.button("Open in Explorer").clicked() {
                                    commands.push(UiCommand::OpenDirectory(dir_for_menu.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Remove Folder").clicked() {
                                    commands.push(UiCommand::RemoveTriangulationFolder(dir_for_menu));
                                    ui.close();
                                }
                            });
                        }
                    });

                ExplorerHeader::new("textures_dropdown".into(), unthemed_icon!("raster.svg"), bold("Rasters"))
                    .default_open(!project.raster_textures.is_empty())
                    .show(ui, |ui| {
                        if project.raster_textures.is_empty() {
                            ui.label("No image textures");
                        }
                        for raster in &project.raster_textures {
                            let label = if raster.is_loaded {
                                bold(&raster.name)
                            } else {
                                egui::RichText::new(&raster.name).color(INACTIVE_TEXT_COLOR)
                            };
                            let details = if raster.is_loaded {
                                format!(
                                    "{} · {} × {}\n{}\n{}",
                                    raster.driver_name,
                                    raster.source_size[0],
                                    raster.source_size[1],
                                    raster.path.display(),
                                    raster.projection
                                )
                            } else {
                                raster.path.display().to_string()
                            };
                            let response = on_hover_file_details(
                                ui.add(ExplorerEntry::new(egui::Id::new(("explorer_raster", &raster.path)), unthemed_icon!("raster.svg"), label).selected(raster.is_draped)),
                                &details,
                                Some(raster.path.as_path()),
                            );
                            if response.double_clicked() {
                                if raster.is_loaded {
                                    commands.push(UiCommand::UnloadRaster(raster.path.clone()));
                                } else {
                                    commands.push(UiCommand::LoadRaster(raster.path.clone()));
                                }
                            }
                            response.context_menu(|ui| {
                                if raster.is_loaded {
                                    if ui.button("Drape over surface").clicked() {
                                        commands.push(UiCommand::DrapeRaster(raster.path.clone()));
                                        ui.close();
                                    }
                                    if raster.is_draped && ui.button("Undrape all").clicked() {
                                        commands.push(UiCommand::UndrapeRaster(raster.path.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::UnloadRaster(raster.path.clone()));
                                        ui.close();
                                    }
                                } else if ui.button("Load").clicked() {
                                    commands.push(UiCommand::LoadRaster(raster.path.clone()));
                                    ui.close();
                                }
                                if project.active_triangulation_for_menu.is_some() && ui.button("Clear active triangulation texture").clicked() {
                                    commands.push(UiCommand::ClearActiveTriangulationRaster);
                                    ui.close();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                if ui.button("Reveal in Explorer").clicked() {
                                    commands.push(UiCommand::RevealRaster(raster.path.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Remove").clicked() {
                                    commands.push(UiCommand::RemoveRaster(raster.path.clone()));
                                    ui.close();
                                }
                            });
                        }
                    });

                ExplorerHeader::new(
                    egui::Id::new("point_clouds_collapse"),
                    // Placeholder icon until point clouds get their own.
                    unthemed_icon!("point_clouds_section.svg"),
                    bold("Point Clouds"),
                )
                .default_open(!project.point_clouds.is_empty())
                .show(ui, |ui| {
                    if project.point_clouds.is_empty() {
                        ui.label("No open point clouds");
                    }
                    for point_cloud in &project.point_clouds {
                        let label = if point_cloud.is_loaded {
                            bold(&point_cloud.name)
                        } else {
                            egui::RichText::new(&point_cloud.name).color(INACTIVE_TEXT_COLOR)
                        };
                        let tooltip = if point_cloud.is_loaded {
                            format!("{}\n{} point(s)", point_cloud.path.display(), point_cloud.point_count)
                        } else {
                            point_cloud.path.display().to_string()
                        };
                        let response = on_hover_file_details(
                            ui.add(ExplorerEntry::new(
                                egui::Id::new(("explorer_point_cloud", &point_cloud.path)),
                                unthemed_icon!("point_cloud_entry.svg"),
                                label,
                            )),
                            &tooltip,
                            Some(point_cloud.path.as_path()),
                        );

                        if response.double_clicked() {
                            if let Some(id) = point_cloud.id {
                                commands.push(UiCommand::ClosePointCloud(id));
                            } else {
                                commands.push(UiCommand::LoadPointCloud(point_cloud.path.clone()));
                            }
                        }

                        let cloud_path = point_cloud.path.clone();
                        response.context_menu(|ui| {
                            if let Some(id) = point_cloud.id {
                                if ui.button("Unload").clicked() {
                                    commands.push(UiCommand::ClosePointCloud(id));
                                    ui.close();
                                }
                                if ui.button(if point_cloud.visible { "Hide" } else { "Show" }).clicked() {
                                    commands.push(UiCommand::TogglePointCloudVisible(id));
                                    ui.close();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                #[cfg(not(target_arch = "wasm32"))]
                                if ui.button("Reveal in Explorer").clicked() {
                                    commands.push(UiCommand::RevealPointCloud(id));
                                    ui.close();
                                }
                                ui.separator();
                            } else {
                                if ui.button("Load").clicked() {
                                    commands.push(UiCommand::LoadPointCloud(cloud_path.clone()));
                                    ui.close();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                if ui.button("Reveal in Explorer").clicked() {
                                    commands.push(UiCommand::RevealPath(cloud_path.clone()));
                                    ui.close();
                                }
                            }
                            if ui.button("Remove Point Cloud").clicked() {
                                commands.push(UiCommand::RemovePointCloud(cloud_path.clone()));
                                ui.close();
                            }
                        });
                    }
                });

                ExplorerHeader::new(egui::Id::new("block_models_collapse"), unthemed_icon!("block_models_section.svg"), bold("Block Models"))
                    .default_open(!project.block_models.is_empty())
                    .show(ui, |ui| {
                        if project.block_models.is_empty() {
                            ui.label("No open block models");
                        }
                        for block_model in &project.block_models {
                            let dirty_marker = if block_model.source.generated { " *" } else { "" };
                            let label_text = format!("{}{dirty_marker}", block_model.name);
                            let label = if block_model.is_loaded {
                                bold(&label_text)
                            } else {
                                egui::RichText::new(&label_text).color(INACTIVE_TEXT_COLOR)
                            };
                            let response = on_hover_file_details(
                                ui.add(
                                    ExplorerEntry::new(
                                        egui::Id::new(("explorer_block_model", &block_model.source.path)),
                                        unthemed_icon!("block_model_entry.svg"),
                                        label,
                                    )
                                    .selected(block_model.is_active),
                                ),
                                &if block_model.source.generated {
                                    format!("Unsaved block model\n{} colour variable(s)", block_model.variable_count)
                                } else {
                                    format!("{}\n{} colour variable(s)", block_model.source.path.display(), block_model.variable_count)
                                },
                                (!block_model.source.generated).then_some(block_model.source.path.as_path()),
                            );
                            if response.double_clicked() {
                                if let Some(id) = block_model.id {
                                    commands.push(UiCommand::CloseBlockModel(id));
                                } else {
                                    commands.push(UiCommand::LoadBlockModel(block_model.source.clone()));
                                }
                            }

                            response.context_menu(|ui| {
                                if let Some(id) = block_model.id {
                                    if ui.button(if block_model.source.generated { "Close" } else { "Unload" }).clicked() {
                                        commands.push(UiCommand::CloseBlockModel(id));
                                        ui.close();
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if block_model.source.generated && ui.button("Save As…").clicked() {
                                        commands.push(UiCommand::SaveBlockModelAs(id));
                                        ui.close();
                                    }
                                    if ui.button(if block_model.visible { "Hide" } else { "Show" }).clicked() {
                                        commands.push(UiCommand::ToggleBlockModelVisible(id));
                                        ui.close();
                                    }
                                    if ui.button("Create Ore Triangulation...").clicked() {
                                        commands.push(UiCommand::OpenCreateOreTriangulation);
                                        ui.close();
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if !block_model.source.generated && ui.button("Reveal in Explorer").clicked() {
                                        commands.push(UiCommand::RevealBlockModel(id));
                                        ui.close();
                                    }
                                    ui.separator();
                                } else {
                                    if ui.button("Load").clicked() {
                                        commands.push(UiCommand::LoadBlockModel(block_model.source.clone()));
                                        ui.close();
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if ui.button("Reveal in Explorer").clicked() {
                                        commands.push(UiCommand::RevealPath(block_model.source.path.clone()));
                                        ui.close();
                                    }
                                }
                                if ui.button("Remove").clicked() {
                                    commands.push(UiCommand::RemoveBlockModel(block_model.source.clone()));
                                    ui.close();
                                }
                            });
                        }
                    });

                ExplorerHeader::new(egui::Id::new("drill_holes_collapse"), unthemed_icon!("drill_holes_section.svg"), bold("Drill Holes"))
                    .default_open(!project.drill_holes.is_empty())
                    .show(ui, |ui| {
                        if project.drill_holes.is_empty() {
                            ui.label("No open drill holes");
                        }
                        for dataset in &project.drill_holes {
                            let label = if dataset.is_loaded {
                                bold(&dataset.name)
                            } else {
                                egui::RichText::new(&dataset.name).color(INACTIVE_TEXT_COLOR)
                            };
                            let source_path = dataset.source.primary_path();
                            let tooltip = if dataset.is_loaded {
                                format!("{}\n{} hole(s)\n{} colour field(s)", source_path.display(), dataset.hole_count, dataset.field_count)
                            } else {
                                source_path.display().to_string()
                            };
                            let response = on_hover_file_details(
                                ui.add(ExplorerEntry::new(
                                    egui::Id::new(("explorer_drill_hole", &dataset.source)),
                                    unthemed_icon!("drill_holes_section.svg"),
                                    label,
                                )),
                                &tooltip,
                                Some(source_path),
                            );
                            if response.double_clicked() {
                                if let Some(id) = dataset.id {
                                    commands.push(UiCommand::CloseDrillHole(id));
                                } else {
                                    commands.push(UiCommand::LoadDrillHole(dataset.source.clone()));
                                }
                            }
                            response.context_menu(|ui| {
                                if let Some(id) = dataset.id {
                                    if ui.button("Unload").clicked() {
                                        commands.push(UiCommand::CloseDrillHole(id));
                                        ui.close();
                                    }
                                    if ui.button(if dataset.visible { "Hide" } else { "Show" }).clicked() {
                                        commands.push(UiCommand::ToggleDrillHoleVisible(id));
                                        ui.close();
                                    }
                                    if ui.button("Colour by…").clicked() {
                                        commands.push(UiCommand::OpenDrillHoleColorDialog(id));
                                        ui.close();
                                    }
                                    if ui.button("Create Block Model…").clicked() {
                                        commands.push(UiCommand::OpenCreateBlockModel(Some(id)));
                                        ui.close();
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if ui.button("Reveal in Explorer").clicked() {
                                        commands.push(UiCommand::RevealPath(source_path.to_owned()));
                                        ui.close();
                                    }
                                    ui.separator();
                                } else {
                                    if ui.button("Load").clicked() {
                                        commands.push(UiCommand::LoadDrillHole(dataset.source.clone()));
                                        ui.close();
                                    }
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if ui.button("Reveal in Explorer").clicked() {
                                        commands.push(UiCommand::RevealPath(source_path.to_owned()));
                                        ui.close();
                                    }
                                }
                                if ui.button("Remove").clicked() {
                                    commands.push(UiCommand::RemoveDrillHole(dataset.source.clone()));
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
