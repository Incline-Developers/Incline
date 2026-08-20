//! Top-level UI module: egui state, rendering pipeline, and the main draw_ui entry point.
//!
//! The `Gui` struct owns the egui context, winit state, and wgpu renderer.
//! `render()` processes input, calls `draw_ui()`, and feeds paint jobs back to wgpu.

pub(crate) mod dialogs;
pub(crate) mod elements;
pub(crate) mod fonts;
pub(crate) mod state;
pub(crate) mod widgets;

/// Select a themed icon by name, picking from `icons_dark/` or `icons_light/` based on `dark_mode`.
///
/// Expands to an `egui::ImageSource<'static>` suitable for `egui::Image::new(...)`.
macro_rules! themed_icon {
    ($ui:expr, $name:literal) => {{
        if $ui.visuals().dark_mode {
            egui::include_image!(concat!("../../../res/ui/icons_dark/", $name))
        } else {
            egui::include_image!(concat!("../../../res/ui/icons_light/", $name))
        }
    }};
}

/// Select an icon by name, picking from `icons/`.
///
/// Expands to an `egui::ImageSource<'static>` suitable for `egui::Image::new(...)`.
macro_rules! unthemed_icon {
    ($name:literal) => {{ egui::include_image!(concat!("../../../res/ui/icons/", $name)) }};
}

use egui_wgpu::ScreenDescriptor;
pub(crate) use themed_icon;
pub(crate) use unthemed_icon;
use winit::{event::WindowEvent, window::Window};

use crate::{
    model::{Document, block_model::OpenBlockModel},
    rendering::color::{color32_to_rgba, rgba_to_color32},
    ui::{
        fonts::setup_custom_fonts,
        state::{ActiveTool, EditorState, UiCommand, UiFrameOutput, UiProjectView, UiTriangulationEntry},
        widgets::viewport::ViewportLabel,
    },
};

/// Linear-space equivalent of [`SELECTION_COLOR`] (sRGB 87, 163, 255) for the renderer.
pub(crate) const SELECTION_COLOR_F32: [f32; 4] = [0.0953, 0.3662, 1.0, 1.0];
pub(crate) const SELECTION_COLOR: egui::Color32 = egui::Color32::from_rgb(87, 163, 255);

/// Owned egui GUI state: context, winit bridge, and wgpu tessellation renderer.
///
/// Created once at application startup; mutated each frame via `handle_event`
/// and `render`.
pub(crate) struct Gui {
    ctx: egui::Context,
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    #[cfg(target_arch = "wasm32")]
    pending_pastes: Vec<String>,
}

impl Gui {
    pub(crate) fn new(window: &Window, device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let ctx = egui::Context::default();
        setup_custom_fonts(&ctx);
        egui_extras::install_image_loaders(&ctx);
        ctx.global_style_mut(|style| {
            style.interaction.show_tooltips_only_when_still = false;
            style.interaction.tooltip_delay = 0.0;
        });
        ctx.set_visuals(theme_visuals(false, SELECTION_COLOR));

        let state = egui_winit::State::new(ctx.clone(), egui::ViewportId::ROOT, window, Some(window.scale_factor() as f32), window.theme(), None);
        let renderer = egui_wgpu::Renderer::new(device, surface_format, egui_wgpu::RendererOptions::default());

        Self {
            ctx,
            state,
            renderer,
            #[cfg(target_arch = "wasm32")]
            pending_pastes: Vec::new(),
        }
    }

    pub(crate) fn handle_event(&mut self, window: &Window, event: &WindowEvent) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    pub(crate) fn pointer_over_ui(&self) -> bool {
        self.ctx.is_pointer_over_egui()
    }

    /// Queue text received from the browser's paste event for the next egui frame.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn queue_paste(&mut self, text: String) {
        let text = text.replace("\r\n", "\n");
        if !text.is_empty() {
            self.pending_pastes.push(text);
        }
    }

    pub(crate) fn register_native_texture(&mut self, device: &wgpu::Device, view: &wgpu::TextureView) -> egui::TextureId {
        self.renderer.register_native_texture(device, view, wgpu::FilterMode::Linear)
    }

    pub(crate) fn update_native_texture(&mut self, device: &wgpu::Device, id: egui::TextureId, view: &wgpu::TextureView) {
        self.renderer.update_egui_texture_from_wgpu_texture(device, view, wgpu::FilterMode::Linear, id);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        window: &Window,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        editor: &mut EditorState,
        document: &mut Document,
        project: &UiProjectView,
        block_models: &[OpenBlockModel],
        drill_holes: &[crate::model::drill_hole::OpenDrillHoleDataset],
        screen_size: [u32; 2],
        orbit_marker: Option<(f32, f32)>,
        camera_forward: [f32; 3],
        camera_up: [f32; 3],
        world_per_physical_pixel: Option<f64>,
    ) -> UiFrameOutput {
        let selection_color = SELECTION_COLOR;
        let visuals = &self.ctx.global_style().visuals;
        if visuals.dark_mode != editor.dark_mode || visuals.selection.stroke.color != selection_color {
            self.ctx.set_visuals(theme_visuals(editor.dark_mode, selection_color));
        }
        #[cfg(not(target_arch = "wasm32"))]
        let raw_input = self.state.take_egui_input(window);
        #[cfg(target_arch = "wasm32")]
        let mut raw_input = self.state.take_egui_input(window);
        #[cfg(target_arch = "wasm32")]
        {
            // egui-winit's WASM build has only an in-process clipboard. Drop
            // its synthetic paste so the real DOM paste event below is never
            // applied a second time.
            raw_input.events.retain(|event| !matches!(event, egui::Event::Paste(_)));
            raw_input.events.extend(self.pending_pastes.drain(..).map(egui::Event::Paste));
        }
        let mut geometry_dirty = false;
        let mut commands = Vec::new();
        // egui may run the UI closure more than once while resolving layout.
        // Keep console rows identical across those passes, even if a warning is
        // logged while the frame is being built.
        let console_snapshot = crate::logging::console_snapshot();
        let frame_context = UiFrameContext {
            orbit_marker,
            camera_forward,
            camera_up,
            world_per_physical_pixel,
            console_snapshot: &console_snapshot,
        };
        let full_output = self.ctx.run_ui(raw_input, |ui| {
            geometry_dirty |= draw_ui(ui, editor, document, project, block_models, drill_holes, &mut commands, frame_context);
        });

        let repaint_after = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|output| output.repaint_delay)
            .filter(|delay| *delay != std::time::Duration::MAX);
        #[cfg(target_arch = "wasm32")]
        mirror_copy_text_to_browser_clipboard(&full_output.platform_output);
        self.state.handle_platform_output(window, full_output.platform_output);

        for (id, image_delta) in &full_output.textures_delta.set {
            self.renderer.update_texture(device, queue, *id, image_delta);
        }

        let pixels_per_point = full_output.pixels_per_point;
        let paint_jobs = self.ctx.tessellate(full_output.shapes, pixels_per_point);
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: screen_size,
            pixels_per_point,
        };
        let extra_command_buffers = self.renderer.update_buffers(device, queue, encoder, &paint_jobs, &screen_descriptor);

        for command_buffer in extra_command_buffers {
            queue.submit(std::iter::once(command_buffer));
        }

        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer.render(&mut render_pass.forget_lifetime(), &paint_jobs, &screen_descriptor);
        }

        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }

        UiFrameOutput {
            repaint_after,
            geometry_dirty,
            commands,
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn mirror_copy_text_to_browser_clipboard(platform_output: &egui::PlatformOutput) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let clipboard = window.navigator().clipboard();

    for command in &platform_output.commands {
        let egui::OutputCommand::CopyText(text) = command else {
            continue;
        };
        let write = clipboard.write_text(text);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = wasm_bindgen_futures::JsFuture::from(write).await {
                log::warn!("Could not copy text to the browser clipboard: {error:?}");
            }
        });
    }
}

/// Top-level UI layout: panels, toolbars, dialogs, and canvas overlay.
///
/// Returns `true` if geometry needs to be rebuilt (e.g. selection state changed).
#[derive(Clone, Copy)]
struct UiFrameContext<'a> {
    orbit_marker: Option<(f32, f32)>,
    camera_forward: [f32; 3],
    camera_up: [f32; 3],
    world_per_physical_pixel: Option<f64>,
    console_snapshot: &'a crate::logging::ConsoleSnapshot,
}

fn viewport_label_text(editor: &EditorState) -> Option<String> {
    if editor.active_tool == ActiveTool::VerticalSlice {
        return Some(
            if editor.slice_pending_start.is_none() {
                "Click the first point of the slice line"
            } else {
                "Click the second point of the slice line"
            }
            .to_string(),
        );
    }

    if editor.active_tool == ActiveTool::MeasureDistance
        && let (Some(start), Some(end)) = (editor.measurement_start, editor.measurement_end)
    {
        return Some(format!("{:.3} meters", start.distance(end)));
    }

    if editor.active_tool == ActiveTool::MeasureBatterAngle {
        if let Some(measurement) = state::batter_angle_measurement(editor.batter_angle_points.as_slice()) {
            return Some(format!("{:.2}° batter angle", measurement.angle_degrees));
        }
        return Some(
            match editor.batter_angle_points.len() {
                0 => "Select first crest/toe point",
                1 => "Select second crest/toe point",
                _ => "Select opposite berm point",
            }
            .to_string(),
        );
    }

    if editor.slice_mode_enabled {
        return Some("Slice view: middle-drag pan · W/S move slab · Q/E rotate · Esc exit".to_string());
    }

    if editor.active_tool == ActiveTool::MakeCircle {
        return Some(match editor.circle_draft.as_ref() {
            None => "Click the circle centre".to_string(),
            Some(draft) if draft.radius_text.is_empty() => "Click a perimeter point or type a radius".to_string(),
            Some(draft) if draft.typed_radius().is_some() => "Press Enter to use the typed radius, or click to use the pointer radius".to_string(),
            Some(_) => "Enter a positive decimal radius".to_string(),
        });
    }

    match editor.active_tool {
        ActiveTool::Move if editor.selected_handles.is_empty() => Some("Select an item"),
        ActiveTool::OffsetElement if editor.offset_awaiting_side_pick => Some("Choose offset side"),
        ActiveTool::OffsetElement if editor.offset_target_ids.is_empty() => Some("Select a line or polygon"),
        ActiveTool::DrapeToTopology if editor.drape_phase == state::DrapePhase::Designs => Some("Select designs"),
        ActiveTool::DrapeToTopology => Some("Select topologies"),
        ActiveTool::RelimitLine if editor.relimit_confirming_end => Some("Choose relimit side"),
        ActiveTool::RelimitLine if editor.relimit_waiting_for_pick => Some("Select line to relimit to"),
        ActiveTool::RelimitLine if editor.relimit_source_id.is_none() || editor.relimit_awaiting_source_pick => Some("Select line to relimit"),
        ActiveTool::FuseIntoPolygon if editor.fuse_awaiting_endpoint.is_some() => Some("Select the endpoint to join"),
        ActiveTool::FuseIntoPolygon if !editor.fuse_segments.is_empty() => Some("Select the next line to fuse"),
        ActiveTool::FuseIntoPolygon => Some("Select a line to fuse"),
        ActiveTool::SplitAtPoints if editor.split_poly_id.is_none() => Some("Select a polygon or open line"),
        ActiveTool::SplitAtPoints if editor.split_selected_verts[0].is_none() => Some("Select a split point"),
        ActiveTool::SplitAtPoints if editor.split_selected_verts[1].is_none() => Some("Select second split point"),
        ActiveTool::Chamfer if editor.chamfer_corner_index.is_none() => Some("Select a polygon vertex"),
        ActiveTool::Bezier if editor.bezier_poly_id.is_none() => Some("Select a polyline or polygon"),
        ActiveTool::Bezier if editor.bezier_selected_verts[0].is_none() => Some("Click first vertex"),
        ActiveTool::Bezier if editor.bezier_selected_verts[1].is_none() => Some("Click second vertex"),
        ActiveTool::ExplodePolygon => Some("Select a polygon"),
        ActiveTool::BatterBermOffset if editor.batter_berm_target_id.is_none() => Some("Select a polygon"),
        ActiveTool::DeleteElement => Some("Select an item"),
        _ => None,
    }
    .map(str::to_string)
}

/// Draw the compact MakeCircle radius editor beside (but never under) the cursor.
fn draw_circle_radius_input(ui: &mut egui::Ui, editor: &mut EditorState, commands: &mut Vec<UiCommand>, viewport_rect: egui::Rect) -> bool {
    if editor.active_tool != ActiveTool::MakeCircle {
        return false;
    }
    let (Some(draft), Some((cursor_x_px, cursor_y_px))) = (editor.circle_draft.as_ref(), editor.cursor_screen_px) else {
        return false;
    };

    const BOX_WIDTH: f32 = 132.0;
    const BOX_HEIGHT: f32 = 34.0;
    const CURSOR_GAP: f32 = 12.0;

    let pixels_per_point = ui.ctx().pixels_per_point();
    let cursor = egui::pos2(cursor_x_px / pixels_per_point, cursor_y_px / pixels_per_point);
    let max_x = (viewport_rect.right() - BOX_WIDTH).max(viewport_rect.left());
    let max_y = (viewport_rect.bottom() - BOX_HEIGHT).max(viewport_rect.top());
    let x = if cursor.x + CURSOR_GAP + BOX_WIDTH <= viewport_rect.right() {
        cursor.x + CURSOR_GAP
    } else {
        cursor.x - CURSOR_GAP - BOX_WIDTH
    }
    .clamp(viewport_rect.left(), max_x);
    let y = if cursor.y + CURSOR_GAP + BOX_HEIGHT <= viewport_rect.bottom() {
        cursor.y + CURSOR_GAP
    } else {
        cursor.y - CURSOR_GAP - BOX_HEIGHT
    }
    .clamp(viewport_rect.top(), max_y);

    let live_mouse_radius = draft.mouse_radius(editor.cursor_world).map_or_else(|| "0.000".to_string(), |radius| format!("{radius:.3}"));
    let typed_invalid = !draft.radius_text.is_empty() && draft.typed_radius().is_none();
    let cursor_screen_px = Some((cursor_x_px, cursor_y_px));
    let mut changed = false;
    let mut cancel_center = false;
    let mut commit_typed_radius = false;

    egui::Area::new(egui::Id::new("make_circle_radius_input"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(x, y))
        .show(ui.ctx(), |ui| {
            let stroke_color = if typed_invalid {
                egui::Color32::from_rgb(220, 70, 70)
            } else {
                ui.visuals().widgets.inactive.bg_stroke.color
            };
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .stroke(egui::Stroke::new(if typed_invalid { 1.5 } else { 1.0 }, stroke_color))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    let draft = editor.circle_draft.as_mut().expect("circle draft checked before drawing radius input");
                    let was_empty = draft.radius_text.is_empty();
                    ui.horizontal_centered(|ui| {
                        let response = ui.add_sized(
                            [94.0, 22.0],
                            egui::TextEdit::singleline(&mut draft.radius_text)
                                .id_source("make_circle_radius_text")
                                .hint_text(live_mouse_radius.as_str())
                                .char_limit(32),
                        );
                        ui.label("m");

                        if draft.focus_requested || !response.has_focus() {
                            response.request_focus();
                            draft.focus_requested = false;
                        }
                        if response.changed() {
                            draft.note_radius_text_changed(was_empty, cursor_screen_px);
                            changed = true;
                        }

                        let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));
                        let escape_pressed = ui.input(|input| input.key_pressed(egui::Key::Escape));
                        if response.has_focus() && enter_pressed && draft.typed_radius().is_some() {
                            commit_typed_radius = true;
                        }
                        if response.has_focus() && escape_pressed {
                            if draft.radius_text.is_empty() {
                                cancel_center = true;
                            } else {
                                draft.radius_text.clear();
                                draft.typing_origin_screen_px = None;
                                draft.focus_requested = true;
                                changed = true;
                            }
                        }
                    });
                });
        });

    if cancel_center {
        editor.circle_draft = None;
        changed = true;
    } else if commit_typed_radius {
        commands.push(UiCommand::CommitCircleTypedRadius);
    }
    changed
}

#[allow(clippy::too_many_arguments)]
fn draw_ui(
    root_ui: &mut egui::Ui,
    editor: &mut EditorState,
    document: &mut Document,
    project: &UiProjectView,
    block_models: &[OpenBlockModel],
    drill_holes: &[crate::model::drill_hole::OpenDrillHoleDataset],
    commands: &mut Vec<UiCommand>,
    frame_context: UiFrameContext<'_>,
) -> bool {
    let mut geometry_dirty = false;

    // --- Panel layout: compute rects for all fixed panels ---
    let project_active = project.has_active_project;
    let editing_enabled = project.has_active_project && editor.active_layer.is_some() && !editor.fly_mode_enabled && !editor.slice_mode_enabled;

    #[cfg(not(target_os = "macos"))]
    let main_menu_rect = crate::ui::elements::main_menu::draw_main_menu(root_ui, editor, project, commands);
    // macOS exposes these actions through the system menu bar in `mac.rs`.
    // An empty rect keeps the rest of the shared panel layout unchanged.
    #[cfg(target_os = "macos")]
    let main_menu_rect = egui::Rect::NOTHING;

    // --- Draw all toolbar panels ---
    let top_toolbar_rect = elements::toolbars::draw_top_toolbar(root_ui, editor, project, commands, editor.can_undo, editor.can_redo);
    let status_bar_rect = elements::status_bar::draw_status_bar(root_ui, editor);
    let explorer_rect = elements::explorer::draw_explorer(root_ui, editor, project, commands);

    // The console belongs below the bottom toolbar. Reserve the toolbar's height
    // before showing the console so dragging it to its maximum cannot starve the
    // toolbar (or the side panels drawn afterward) of layout space.
    let console_rect = editor.show_console.then(|| {
        let max_height = (root_ui.available_height() - elements::toolbars::BOTTOM_TOOLBAR_HEIGHT).max(0.0);
        elements::console::draw_console(root_ui, max_height, frame_context.console_snapshot)
    });
    if console_rect.is_none() {
        // `Panel::show` creates one direct child of `root_ui`. Keep the root
        // auto-id sequence identical when this optional panel is absent, or
        // every panel drawn after it receives a different unique id.
        root_ui.skip_ahead_auto_ids(1);
    }

    let bottom_toolbar_rect = elements::toolbars::draw_bottom_toolbar(root_ui, editor, &mut geometry_dirty, commands);
    let left_toolbar_rect = elements::toolbars::draw_left_toolbar(root_ui, editor, editing_enabled, project_active, commands);

    let right_toolbar_rect = elements::toolbars::draw_right_toolbar(root_ui, editor, commands);

    // --- Compute canvas rect (area not occupied by panels) ---
    let canvas_bottom = console_rect.map_or_else(
        || status_bar_rect.top().min(bottom_toolbar_rect.top()),
        |rect| status_bar_rect.top().min(bottom_toolbar_rect.top()).min(rect.top()),
    );
    let canvas_rect = egui::Rect::from_min_max(
        egui::pos2(explorer_rect.right().max(left_toolbar_rect.right()), main_menu_rect.bottom().max(top_toolbar_rect.bottom())),
        egui::pos2(right_toolbar_rect.left(), canvas_bottom),
    );

    if let (Some(start), Some(end)) = (editor.selection_box_start_px, editor.selection_box_current_px) {
        // Box selection: left-to-right = cross select (dashed green), right-to-left = window select
        // (solid blue).
        let cross_select = end.0 > start.0;
        let box_color = if cross_select {
            egui::Color32::from_rgb(80, 220, 100) // green for cross/touch select
        } else {
            SELECTION_COLOR // theme colour for window select
        };
        let pixels_per_point = root_ui.ctx().pixels_per_point();
        let selection_rect = egui::Rect::from_two_pos(
            egui::pos2(start.0 / pixels_per_point, start.1 / pixels_per_point),
            egui::pos2(end.0 / pixels_per_point, end.1 / pixels_per_point),
        )
        .intersect(canvas_rect);
        root_ui
            .painter()
            .rect_filled(selection_rect, 0.0, egui::Color32::from_rgba_unmultiplied(box_color.r(), box_color.g(), box_color.b(), 14));
        if cross_select {
            // Dashed border for cross selection
            let painter = root_ui.painter();
            let dash = 6.0;
            let gap = 4.0;
            let stroke = egui::Stroke::new(1.0, box_color);
            let r = selection_rect;
            for seg in dashed_rect_segments(r, dash, gap) {
                painter.line_segment(seg, stroke);
            }
        } else {
            root_ui
                .painter()
                .rect_stroke(selection_rect, 0.0, egui::Stroke::new(1.0, box_color), egui::StrokeKind::Inside);
        }
    }

    // Exact geometry attached to the currently open Create Triangulation
    // failure. Segments identify the contributing breaklines; high-contrast
    // markers locate the degenerate endpoints or XY conflict itself.
    if editor.tri_create_failure.is_some() && (!editor.tri_create_diagnostic_segments_screen_px.is_empty() || !editor.tri_create_diagnostic_markers_screen_px.is_empty()) {
        let pixels_per_point = root_ui.ctx().pixels_per_point();
        let painter = root_ui.painter().with_clip_rect(canvas_rect);
        let conflict_color = egui::Color32::from_rgb(245, 65, 65);
        for [start, end] in &editor.tri_create_diagnostic_segments_screen_px {
            if let (Some(start), Some(end)) = (start, end) {
                let segment = [
                    egui::pos2(start.0 / pixels_per_point, start.1 / pixels_per_point),
                    egui::pos2(end.0 / pixels_per_point, end.1 / pixels_per_point),
                ];
                painter.line_segment(segment, egui::Stroke::new(5.0, egui::Color32::BLACK));
                painter.line_segment(segment, egui::Stroke::new(3.0, conflict_color));
            }
        }
        for &(x, y) in &editor.tri_create_diagnostic_markers_screen_px {
            let position = egui::pos2(x / pixels_per_point, y / pixels_per_point);
            painter.circle_filled(position, 9.0, conflict_color);
            painter.circle_filled(position, 5.0, egui::Color32::YELLOW);
            painter.circle_stroke(position, 9.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
        }
    }

    // Move tool: draw 3-axis gizmo
    if editor.active_tool == ActiveTool::Move
        && !editor.selected_handles.is_empty()
        && let Some(center_px) = editor.move_gizmo_center_px
    {
        let ppp = root_ui.ctx().pixels_per_point();
        let center = egui::pos2(center_px.0 / ppp, center_px.1 / ppp);
        let painter = root_ui.painter();
        let planes = [
            (egui::Color32::from_rgb(220, 190, 45), "XY"),
            (egui::Color32::from_rgb(210, 65, 190), "XZ"),
            (egui::Color32::from_rgb(45, 190, 205), "YZ"),
        ];
        for (plane_idx, (color, label)) in planes.into_iter().enumerate() {
            let Some(quad_px) = editor.move_gizmo_plane_handles_px[plane_idx] else {
                continue;
            };
            let points = quad_px.map(|point| egui::pos2(point.0 / ppp, point.1 / ppp)).to_vec();
            let is_active = editor.move_gizmo_hovered_plane == Some(plane_idx as u8) || editor.gizmo_drag_plane_index == Some(plane_idx as u8);
            let fill = if is_active {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120)
            } else {
                egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 75)
            };
            let stroke_color = if is_active { egui::Color32::WHITE } else { color };
            painter.add(egui::Shape::convex_polygon(
                points.clone(),
                fill,
                egui::Stroke::new(if is_active { 2.0 } else { 1.0 }, stroke_color),
            ));
            let label_position = points.iter().fold(egui::Pos2::ZERO, |sum, point| sum + point.to_vec2()) / points.len() as f32;
            painter.text(label_position, egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(9.0), stroke_color);
        }
        let axes = [
            (editor.move_gizmo_x_tip_px, egui::Color32::from_rgb(220, 50, 50), 0u8, "X"),
            (editor.move_gizmo_y_tip_px, egui::Color32::from_rgb(50, 200, 50), 1u8, "Y"),
            (editor.move_gizmo_z_tip_px, egui::Color32::from_rgb(50, 100, 220), 2u8, "Z"),
        ];
        for (tip_opt, color, axis_idx, label) in axes {
            let is_active = editor.move_gizmo_hovered_axis == Some(axis_idx) || editor.gizmo_drag_axis_index == Some(axis_idx);
            let draw_color = if is_active { egui::Color32::WHITE } else { color };
            let width = if is_active { 3.0 } else { 2.0 };
            if let Some(tip_px) = tip_opt {
                let tip = egui::pos2(tip_px.0 / ppp, tip_px.1 / ppp);
                painter.line_segment([center, tip], egui::Stroke::new(width, draw_color));
                painter.circle_filled(tip, 5.0, draw_color);
                painter.text(
                    egui::pos2(tip.x + 4.0, tip.y - 8.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::proportional(12.0),
                    draw_color,
                );
            }
        }
        painter.circle_filled(center, 4.0, egui::Color32::WHITE);
    }

    // Move tool: numeric delta panel
    if editor.active_tool == ActiveTool::Move && !editor.selected_handles.is_empty() {
        dialogs::editing::draw_move_panel(root_ui, editor, commands, canvas_rect);
    }

    // Chamfer tool: gizmo overlay + dock panel
    if editor.active_tool == ActiveTool::Chamfer {
        // Hover sphere: show on nearest valid corner before the user clicks one
        if let Some(hover_px) = editor.chamfer_hover_corner_px {
            let ppp = root_ui.ctx().pixels_per_point();
            let pos = egui::pos2(hover_px.0 / ppp, hover_px.1 / ppp);
            root_ui.painter().with_clip_rect(canvas_rect).circle_filled(pos, 6.0, egui::Color32::from_rgb(255, 220, 50));
        }

        // Draw the radius gizmo: cylinder+cone arrow always visible from the corner outward.
        if let Some(corner_px) = editor.chamfer_gizmo_corner_px {
            let ppp = root_ui.ctx().pixels_per_point();
            let corner = egui::pos2(corner_px.0 / ppp, corner_px.1 / ppp);
            let painter = root_ui.painter();
            let color = if editor.chamfer_gizmo_hovered || editor.chamfer_gizmo_drag_start_px.is_some() {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_rgb(255, 200, 50)
            };
            const CORNER_R: f32 = 5.0;
            const STUB_LEN: f32 = 40.0;
            const SHAFT_W: f32 = 3.5;
            const HEAD_LEN: f32 = 12.0;
            const HEAD_W: f32 = 7.0;
            const HANDLE_R: f32 = 6.0;

            if let Some(handle_px) = editor.chamfer_gizmo_handle_px {
                let handle = egui::pos2(handle_px.0 / ppp, handle_px.1 / ppp);
                let raw_vec = handle - corner;
                // Use the edge stub direction when radius ≈ 0 so the arrow is still visible.
                let dir = if raw_vec.length() > 4.0 {
                    raw_vec.normalized()
                } else if let Some(ed) = editor.chamfer_gizmo_bisector_px {
                    egui::vec2(ed.0, ed.1).normalized()
                } else {
                    egui::vec2(1.0, 0.0)
                };
                let tip = if raw_vec.length() > STUB_LEN { handle } else { corner + dir * STUB_LEN };
                // Shaft starts just past the corner circle so it doesn't overlap it.
                let shaft_start = corner + dir * (CORNER_R + 1.0);
                let shaft_end = tip - dir * HEAD_LEN;
                if (shaft_end - shaft_start).length() > 1.0 {
                    painter.line_segment([shaft_start, shaft_end], egui::Stroke::new(SHAFT_W, color));
                }
                let perp = egui::vec2(-dir.y, dir.x);
                painter.add(egui::Shape::convex_polygon(
                    vec![tip, shaft_end + perp * HEAD_W, shaft_end - perp * HEAD_W],
                    color,
                    egui::Stroke::NONE,
                ));
                // Handle circle only when radius > 0 and handle is away from corner.
                if raw_vec.length() > 4.0 {
                    painter.circle_filled(handle, HANDLE_R, color);
                }
            } else if let Some(ed) = editor.chamfer_gizmo_bisector_px {
                let dir = egui::vec2(ed.0, ed.1).normalized();
                let shaft_start = corner + dir * (CORNER_R + 1.0);
                let tip = corner + dir * STUB_LEN;
                let shaft_end = tip - dir * HEAD_LEN;
                painter.line_segment([shaft_start, shaft_end], egui::Stroke::new(SHAFT_W, color));
                let perp = egui::vec2(-dir.y, dir.x);
                painter.add(egui::Shape::convex_polygon(
                    vec![tip, shaft_end + perp * HEAD_W, shaft_end - perp * HEAD_W],
                    color,
                    egui::Stroke::NONE,
                ));
            }
            painter.circle_filled(corner, CORNER_R, color);
        }

        // Draw the chamfer preview polyline. A segment is drawn only when
        // both of its adjacent source vertices are visible, so clipped
        // vertices cannot cause previews to connect nonadjacent points.
        if !editor.chamfer_preview_screen_px.is_empty() {
            let ppp = root_ui.ctx().pixels_per_point();
            let pts: Vec<Option<egui::Pos2>> = editor
                .chamfer_preview_screen_px
                .iter()
                .map(|point| point.map(|(x, y)| egui::pos2(x / ppp, y / ppp)))
                .collect();
            let painter = root_ui.painter().with_clip_rect(canvas_rect);
            let stroke = egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 220, 0));
            let n = pts.len();
            for i in 0..n {
                if let (Some(a), Some(b)) = (pts[i], pts[(i + 1) % n]) {
                    painter.line_segment([a, b], stroke);
                }
            }
        }

        dialogs::editing::draw_chamfer_panel(root_ui, editor, commands, canvas_rect);
    }

    if matches!(editor.active_tool, ActiveTool::Move | ActiveTool::DeleteElement)
        && let Some(hover_px) = editor.tool_hover_vertex_px
    {
        let ppp = root_ui.ctx().pixels_per_point();
        let pos = egui::pos2(hover_px.0 / ppp, hover_px.1 / ppp);
        let painter = root_ui.painter().with_clip_rect(canvas_rect);
        let color = SELECTION_COLOR;
        painter.circle_filled(pos, 6.0, color);
        painter.circle_stroke(pos, 7.5, egui::Stroke::new(1.5, egui::Color32::WHITE));
    }

    // Split At Points tool: vertex dots and selected split points
    if editor.active_tool == ActiveTool::SplitAtPoints {
        let ppp = root_ui.ctx().pixels_per_point();
        let painter = root_ui.painter().with_clip_rect(canvas_rect);

        for &(x, y) in editor.split_poly_verts_screen_px.iter().flatten() {
            painter.circle_filled(egui::pos2(x / ppp, y / ppp), 5.0, egui::Color32::WHITE);
        }

        for selected in editor.split_selected_verts.iter().flatten() {
            if let Some(&Some((x, y))) = editor.split_poly_verts_screen_px.get(*selected) {
                let pos = egui::pos2(x / ppp, y / ppp);
                painter.circle_filled(pos, 7.0, SELECTION_COLOR);
                painter.circle_stroke(pos, 8.5, egui::Stroke::new(1.5, egui::Color32::WHITE));
            }
        }
    }

    // Bezier tool: source span, vertex dots, control handles, and dashed preview.
    if editor.active_tool == ActiveTool::Bezier {
        let ppp = root_ui.ctx().pixels_per_point();
        let painter = root_ui.painter().with_clip_rect(canvas_rect);

        // The exact source path that Apply will remove.
        for pair in editor.bezier_span_screen_px.windows(2) {
            if let [Some(a), Some(b)] = pair {
                painter.line_segment(
                    [egui::pos2(a.0 / ppp, a.1 / ppp), egui::pos2(b.0 / ppp, b.1 / ppp)],
                    egui::Stroke::new(3.0, egui::Color32::from_rgb(255, 150, 50)),
                );
            }
        }

        // All polygon vertices as white dots
        for &(x, y) in editor.bezier_poly_verts_screen_px.iter().flatten() {
            painter.circle_filled(egui::pos2(x / ppp, y / ppp), 5.0, egui::Color32::WHITE);
        }

        // Selected vertices in selection colour
        for slot in 0..2usize {
            if let Some(vi) = editor.bezier_selected_verts[slot]
                && let Some(&Some((x, y))) = editor.bezier_poly_verts_screen_px.get(vi)
            {
                painter.circle_filled(egui::pos2(x / ppp, y / ppp), 7.0, SELECTION_COLOR);
            }
        }

        // Dashed yellow replacement curve; segments need both endpoints visible.
        if !editor.bezier_preview_screen_px.is_empty() {
            let pts: Vec<Option<egui::Pos2>> = editor
                .bezier_preview_screen_px
                .iter()
                .map(|point| point.map(|(x, y)| egui::pos2(x / ppp, y / ppp)))
                .collect();
            let dash_stroke = egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 220, 0));
            for pair in pts.windows(2) {
                if let [Some(a), Some(b)] = pair {
                    for seg in dashed_line_segments(*a, *b, 6.0, 4.0) {
                        painter.line_segment(seg, dash_stroke);
                    }
                }
            }
        }

        // Control point gizmos (only when both vertices are selected)
        if let (Some(cp1_px), Some(cp2_px)) = (editor.bezier_cp1_screen_px, editor.bezier_cp2_screen_px) {
            // Tangent lines from anchor vertices to control points
            if let Some(vi) = editor.bezier_selected_verts[0]
                && let Some(&Some(v_px)) = editor.bezier_poly_verts_screen_px.get(vi)
            {
                painter.line_segment(
                    [egui::pos2(v_px.0 / ppp, v_px.1 / ppp), egui::pos2(cp1_px.0 / ppp, cp1_px.1 / ppp)],
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(255, 150, 50, 200)),
                );
            }
            if let Some(vj) = editor.bezier_selected_verts[1]
                && let Some(&Some(v_px)) = editor.bezier_poly_verts_screen_px.get(vj)
            {
                painter.line_segment(
                    [egui::pos2(v_px.0 / ppp, v_px.1 / ppp), egui::pos2(cp2_px.0 / ppp, cp2_px.1 / ppp)],
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(50, 150, 255, 200)),
                );
            }

            const HANDLE_R: f32 = 6.0;
            let cp1_active = editor.bezier_hover_cp == Some(0) || editor.bezier_dragging_cp == Some(0);
            let cp2_active = editor.bezier_hover_cp == Some(1) || editor.bezier_dragging_cp == Some(1);
            let cp1_color = if cp1_active { egui::Color32::WHITE } else { egui::Color32::from_rgb(255, 150, 50) };
            let cp2_color = if cp2_active { egui::Color32::WHITE } else { egui::Color32::from_rgb(50, 150, 255) };

            let cp1_pos = egui::pos2(cp1_px.0 / ppp, cp1_px.1 / ppp);
            let cp2_pos = egui::pos2(cp2_px.0 / ppp, cp2_px.1 / ppp);
            painter.circle_stroke(cp1_pos, HANDLE_R, egui::Stroke::new(2.0, cp1_color));
            painter.circle_filled(cp1_pos, 3.5, cp1_color);
            painter.circle_stroke(cp2_pos, HANDLE_R, egui::Stroke::new(2.0, cp2_color));
            painter.circle_filled(cp2_pos, 3.5, cp2_color);
        }

        if editor.bezier_dialog_open {
            dialogs::editing::draw_bezier_panel(root_ui, editor, commands, canvas_rect);
        }
    }

    // --- Startup & global dialogs ---
    #[cfg(target_arch = "wasm32")]
    let naming_browser_pidb = editor.new_pidb_dialog_open;
    #[cfg(not(target_arch = "wasm32"))]
    let naming_browser_pidb = false;
    if project.needs_startup_dialog && !naming_browser_pidb {
        dialogs::editing::draw_select_pidb_dialog(root_ui, editor, commands);
    }
    dialogs::files::draw_vertical_exaggeration_dialog(root_ui, editor, canvas_rect);
    dialogs::editing::draw_move_to_layer_dialog(root_ui, editor, project, commands);
    dialogs::editing::draw_set_selection_z_dialog(root_ui, editor, commands);
    dialogs::editing::draw_insert_point_at_elevation_dialog(root_ui, editor, commands);
    dialogs::editing::draw_move_layer_dialog(root_ui, editor, project, commands);

    // --- Canvas right-click context menu ---
    if editor.canvas_context_menu_open
        && let Some((px, py)) = editor.canvas_context_menu_px
    {
        dialogs::editing::draw_right_click_context(root_ui, editor, project, commands, &mut geometry_dirty, document, px, py);
    }

    // --- Tool-specific dialogs ---

    geometry_dirty |= draw_circle_radius_input(root_ui, editor, commands, canvas_rect);

    // Browser PIDB creation
    #[cfg(target_arch = "wasm32")]
    if editor.new_pidb_dialog_open {
        crate::ui::dialogs::editing::draw_create_pidb_dialog(root_ui, commands, editor, canvas_rect);
    }

    // Create Layer
    if editor.new_layer_dialog_open {
        dialogs::editing::draw_create_layer_dialog(root_ui, commands, editor, project, canvas_rect);
    }

    // Rename Layer
    if editor.renaming_layer.is_some() {
        dialogs::editing::draw_rename_layer_dialog(root_ui, commands, editor);
    }

    // Drape to Topology
    if editor.active_tool == ActiveTool::DrapeToTopology {
        dialogs::editing::draw_drape_selection_panel(root_ui, editor, commands, canvas_rect);
    }

    // Offset Element
    if editor.active_tool == ActiveTool::OffsetElement && !editor.offset_dialog_open && !editor.offset_awaiting_side_pick {
        commands.push(UiCommand::OpenOffsetDialog);
    }
    if editor.offset_dialog_open {
        dialogs::editing::draw_offset_dialog(root_ui, commands, editor, canvas_rect);
    }
    if editor.offset_awaiting_side_pick && !editor.offset_preview_screen_px.is_empty() {
        let ppp = root_ui.ctx().pixels_per_point();
        // Entries stay index-aligned with the world arrays; a clipped vertex
        // is `None` so guides pair the right endpoints and preview ranges
        // never shift onto different vertices.
        let pts: Vec<Option<egui::Pos2>> = editor
            .offset_preview_screen_px
            .iter()
            .map(|point| point.map(|(x, y)| egui::pos2(x / ppp, y / ppp)))
            .collect();
        let src_pts: Vec<Option<egui::Pos2>> = editor
            .offset_source_screen_px
            .iter()
            .map(|point| point.map(|(x, y)| egui::pos2(x / ppp, y / ppp)))
            .collect();
        let painter = root_ui.painter().with_clip_rect(canvas_rect);
        let yellow = egui::Color32::from_rgb(255, 220, 0);
        let guide = egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(255, 230, 40, 220));
        for (from, to) in src_pts.iter().zip(pts.iter()) {
            if let (Some(from), Some(to)) = (from, to) {
                for seg in dashed_line_segments(*from, *to, 6.0, 4.0) {
                    painter.line_segment(seg, guide);
                }
            }
        }
        let stroke = egui::Stroke::new(2.0, yellow);
        for &(start, end, closed) in &editor.offset_preview_ranges {
            if start >= end || end > pts.len() {
                continue;
            }
            for i in start..end {
                let next = if closed { start + ((i - start + 1) % (end - start)) } else { i + 1 };
                if next < end
                    && let (Some(a), Some(b)) = (pts[i], pts[next])
                {
                    painter.line_segment([a, b], stroke);
                }
            }
        }
    }

    if let Some(label) = viewport_label_text(editor) {
        ViewportLabel::new("viewport_tool_label", label, canvas_rect).show(root_ui.ctx());
    }
    // Slice view: config dock + top-down minimap
    if editor.slice_mode_enabled {
        dialogs::editing::draw_slice_panel(root_ui, editor, commands, canvas_rect);
        widgets::viewport::ViewportMiniMap::new("slice_minimap", canvas_rect).show(root_ui.ctx(), editor, commands);
    }
    // Batter Berm
    if editor.active_tool == ActiveTool::BatterBermOffset && !editor.batter_berm_dialog_open {
        commands.push(UiCommand::OpenBatterBermDialog);
    }
    if editor.batter_berm_dialog_open {
        dialogs::editing::draw_batter_berm_dialog(root_ui, commands, editor, canvas_rect);
    }
    // Relimit Line
    if editor.active_tool == ActiveTool::RelimitLine
        && !editor.relimit_dialog_open
        && !editor.relimit_awaiting_source_pick
        && !editor.relimit_waiting_for_pick
        && !editor.relimit_confirming_end
    {
        commands.push(UiCommand::OpenRelimitDialog);
    }
    if editor.relimit_dialog_open {
        dialogs::editing::draw_relimit_dialog(root_ui, commands, editor, canvas_rect);
    }
    // Sphere on the chosen resize endpoint (absolute / relative modes)
    if let Some(ep_px) = editor.relimit_resize_end_px {
        let ppp = root_ui.ctx().pixels_per_point();
        let pos = egui::pos2(ep_px.0 / ppp, ep_px.1 / ppp);
        root_ui.painter().with_clip_rect(canvas_rect).circle_filled(pos, 7.0, egui::Color32::from_rgb(255, 220, 0));
    }
    if editor.relimit_confirming_end
        && let (Some(from), Some(to)) = (editor.relimit_preview_from_px, editor.relimit_preview_to_px)
    {
        let ppp = root_ui.ctx().pixels_per_point();
        let color = if editor.relimit_preview_is_extension {
            egui::Color32::from_rgb(255, 220, 0) // yellow = growing
        } else {
            egui::Color32::from_rgb(220, 60, 60) // red = shrinking
        };
        root_ui
            .painter()
            .with_clip_rect(canvas_rect)
            .line_segment([egui::pos2(from.0 / ppp, from.1 / ppp), egui::pos2(to.0 / ppp, to.1 / ppp)], egui::Stroke::new(2.5, color));
    }

    // Text editing
    if editor.text_editing_enabled {
        dialogs::editing::draw_text_edit_dialog(root_ui, commands, editor, &mut geometry_dirty, canvas_rect);
    }

    // Polygon finish (MakePoly)
    if editor.poly_finish_dialog {
        dialogs::editing::draw_finish_polygon_dialog(root_ui, commands, editor, canvas_rect);
    }

    // --- Canvas overlays ---

    // Orbit marker (clipped to the 3D viewport)
    if let Some((ox, oy)) = frame_context.orbit_marker {
        elements::cursors::draw_orbit_marker(root_ui, ox, oy, canvas_rect);
    }

    if editor.show_world_axis_gizmo {
        elements::cursors::draw_orientation_gizmo(root_ui, canvas_rect, frame_context.camera_forward, frame_context.camera_up, commands);
    }

    let block_model_overlay = widgets::viewport::ColorScaleLegend::new("block_model_color_scale_legend", block_models, canvas_rect).show(root_ui.ctx(), editor, commands);
    let world_per_point = frame_context.world_per_physical_pixel.map(|scale| scale * f64::from(root_ui.ctx().pixels_per_point()));
    if editor.show_scale_bar {
        widgets::viewport::ViewportScaleBar::new("viewport_scale_bar", canvas_rect).show(root_ui.ctx(), world_per_point, editor.renderer_background_color, block_model_overlay);
    }

    elements::preferences::draw_preferences(root_ui, editor, commands);
    geometry_dirty |= draw_global_dialogs(root_ui, editor, document, project, block_models, drill_holes, commands);

    geometry_dirty
}

/// Draw dialogs after the workspace so modal lifecycle state is never hidden.
/// These dialogs are global application state even when their contents refer
/// to workspace data.
fn draw_global_dialogs(
    root_ui: &mut egui::Ui,
    editor: &mut EditorState,
    document: &mut Document,
    project: &UiProjectView,
    block_models: &[OpenBlockModel],
    drill_holes: &[crate::model::drill_hole::OpenDrillHoleDataset],
    commands: &mut Vec<UiCommand>,
) -> bool {
    let mut geometry_dirty = false;
    dialogs::drill_hole::draw_drill_hole_color_dialog(root_ui, editor, drill_holes, commands);

    // Exit confirmation
    if editor.exit_confirm_open {
        dialogs::confirmations::draw_exit_confirm_dialog(root_ui, commands, editor);
    }

    // Delete selection confirmation
    if editor.delete_confirm_open {
        dialogs::confirmations::draw_delete_confirm_dialog(root_ui, commands, editor);
    }

    // Delete layer confirmation
    if editor.pending_delete_layer.is_some() {
        dialogs::confirmations::draw_delete_layer_confirm_dialog(root_ui, commands, editor);
    }

    // Dirty PIDB close confirmation
    if editor.pending_close_pidb.is_some() {
        dialogs::confirmations::draw_close_pidb_dialog(root_ui, commands, editor, project);
    }

    // Dirty PIDB discard-changes confirmation
    if editor.pending_discard_pidb.is_some() {
        #[cfg(not(target_arch = "wasm32"))]
        dialogs::confirmations::draw_discard_pidb_dialog(root_ui, commands, editor, project);
    }

    // Dirty layer discard-changes confirmation
    if editor.pending_discard_layer.is_some() {
        #[cfg(not(target_arch = "wasm32"))]
        dialogs::confirmations::draw_discard_layer_dialog(root_ui, commands, editor);
    }

    // Load-all triangulations folder confirmation
    if editor.confirm_load_all_folder.is_some() {
        dialogs::confirmations::draw_confirm_load_all_folder_dialog(root_ui, commands, editor);
    }

    // Close unsaved triangulation confirmation
    if editor.tri_close_unsaved.is_some() {
        dialogs::confirmations::draw_close_unsaved_tri_dialog(root_ui, commands, editor);
    }

    // Close unsaved generated block-model confirmation
    if editor.block_model_close_unsaved.is_some() {
        dialogs::confirmations::draw_close_unsaved_block_model_dialog(root_ui, commands, editor);
    }

    // Close in-memory point-cloud confirmation
    if editor.point_cloud_close_unsaved.is_some() {
        dialogs::confirmations::draw_close_unsaved_point_cloud_dialog(root_ui, commands, editor);
    }

    // Create Triangulation (always mark geometry dirty while open)
    if editor.tri_create_open {
        geometry_dirty = true;
        dialogs::triangulation::draw_tri_create_main_dialog(root_ui, editor, document, commands);
    }

    if editor.tri_create_failure.is_some() {
        dialogs::triangulation::draw_tri_create_failure_dialog(root_ui, editor, commands);
    }

    if editor.tri_cut_poly_open {
        dialogs::triangulation::draw_cut_poly_dialog(root_ui, editor, document, project, commands);
    }

    if editor.tri_cut_z_open {
        dialogs::triangulation::draw_cut_z_dialog(root_ui, editor, project, commands);
    }

    if editor.tri_cut_surface_open {
        dialogs::triangulation::draw_cut_surface_dialog(root_ui, editor, project, commands);
    }

    if editor.tri_cut_pitshell_open {
        dialogs::triangulation::draw_cut_topology_to_pit_shell_dialog(root_ui, editor, project, commands);
    }

    if editor.tri_include_solid_open {
        dialogs::triangulation::draw_include_solid_dialog(root_ui, editor, project, commands);
    }

    if editor.tri_contour_open {
        dialogs::triangulation::draw_contour_dialog(root_ui, editor, project, commands);
    }
    if editor.point_cloud_tin_open {
        dialogs::triangulation::draw_point_cloud_tin_dialog(root_ui, editor, project, commands);
    }
    if editor.triangulation_pick_target.is_some() {
        dialogs::triangulation::draw_triangulation_pick_prompt(root_ui, editor);
    }
    if editor.block_model_create_open {
        elements::block_model::draw_create_block_model_dialog(root_ui, editor, drill_holes, commands);
    }
    if editor.ore_triangulation_open {
        elements::block_model::draw_ore_triangulation_dialog(root_ui, editor, block_models, commands);
    }

    if editor.plot_dialog.is_some() {
        dialogs::plot::draw_plot_dialog(root_ui, editor, project, commands);
    }

    // import export menus
    dialogs::import_export::draw_import_menu(root_ui, editor, project, commands);
    dialogs::import_export::draw_export_menu(root_ui, editor, project, commands);

    geometry_dirty
}

/// Generate dashed line segments along the four edges of a rect.
/// Returns pairs of `[Pos2; 2]` suitable for `painter.line_segment`.
fn dashed_rect_segments(r: egui::Rect, dash: f32, gap: f32) -> Vec<[egui::Pos2; 2]> {
    let mut segs = Vec::new();
    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()];
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        let total = (b - a).length();
        let step = dash + gap;
        let mut t = 0.0f32;
        while t < total {
            let t_end = (t + dash).min(total);
            let frac_a = t / total;
            let frac_b = t_end / total;
            segs.push([a + (b - a) * frac_a, a + (b - a) * frac_b]);
            t += step;
        }
    }
    segs
}

fn dashed_line_segments(start: egui::Pos2, end: egui::Pos2, dash: f32, gap: f32) -> Vec<[egui::Pos2; 2]> {
    let delta = end - start;
    let length = delta.length();
    if length <= 0.0 {
        return Vec::new();
    }
    let dir = delta / length;
    let mut segments = Vec::new();
    let mut t = 0.0;
    while t < length {
        let a = start + dir * t;
        let b = start + dir * (t + dash).min(length);
        segments.push([a, b]);
        t += dash + gap;
    }
    segments
}

/// Build an `egui::Visuals` set with selection styling applied to the given theme.
fn theme_visuals(dark_mode: bool, selection_color: egui::Color32) -> egui::Visuals {
    let mut visuals = if dark_mode { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.selection.bg_fill = selection_color.gamma_multiply(0.35);
    visuals.selection.stroke.color = selection_color;
    visuals
}
