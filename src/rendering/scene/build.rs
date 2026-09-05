//! Document/editor scene assembly entry points.

use std::collections::{HashMap, HashSet};

use glam::{DVec3, Mat4, Vec3};
use lyon::tessellation::VertexBuffers;

use crate::{
    model::{
        Document, FillStyle, Object, ObjectId, PolyVertex, SceneEntityId,
        geometry::{PolylineFillMesh, triangulate_polyline_fill},
    },
    rendering::{
        StrokeVertex, Vertex,
        geometry::{DrawContext, draw_line, draw_screen_cross, tessellate_polyline_stroke},
        graphics::{DOC_LINE_WIDTH, DOC_TEXT_FONT_SIZE, TEXT_EDIT_INDICATOR_COLOR, YELLOW_HIGHLIGHT_COLOR, make_translucent, text_bounds_corners_with_layout_width},
        pick::{PickRecord, TextPickRecord, world_bounds_from_local_positions},
        scene::document::{fill_polyline_hatch, fill_polyline_solid, polyline_hatch_spacing},
        text::{Text, TextBox, TextSystem},
    },
    ui::state::EditorState,
};

pub(crate) struct DocumentSceneBuildInput<'a> {
    pub(crate) editor: &'a EditorState,
    pub(crate) document: &'a Document,
    /// Objects owned by the static stroke chunk cache; skipped here.
    pub(crate) static_ids: &'a HashSet<ObjectId>,
    pub(crate) fill_cache: &'a mut PolylineFillCache,
    pub(crate) text_system: &'a mut TextSystem,
    pub(crate) lyon_buffer: &'a mut VertexBuffers<Vertex, u32>,
    pub(crate) stroke_vertex_buf: &'a mut Vec<StrokeVertex>,
    pub(crate) stroke_index_buf: &'a mut Vec<u32>,
    pub(crate) text_vertex_buf: &'a mut Vec<Vertex>,
    pub(crate) text_index_buf: &'a mut Vec<u32>,
    pub(crate) text_draw_batches: &'a mut Vec<TextDrawBatch>,
    pub(crate) pick_records: &'a mut Vec<PickRecord>,
    pub(crate) text_pick_records: &'a mut Vec<TextPickRecord>,
    pub(crate) document_draw_batches: &'a mut Vec<DocumentDrawBatch>,
    pub(crate) scene_origin: DVec3,
    pub(crate) scale_factor: f32,
}

/// World-space fill meshes survive selection, style changes and scene rebasing.
/// Compare source geometry so a color-only object revision does not triangulate
/// again, while edits, undo and replacement documents still invalidate it.
#[derive(Default)]
pub(crate) struct PolylineFillCache {
    entries: HashMap<ObjectId, CachedPolylineFill>,
}

struct CachedPolylineFill {
    verts: Vec<PolyVertex>,
    mesh: Option<PolylineFillMesh>,
}

impl PolylineFillCache {
    fn retain_document(&mut self, document: &Document) {
        self.entries
            .retain(|id, _| matches!(document.get_object(*id), Some(Object::Polyline { closed: true, fill, .. }) if *fill != FillStyle::Clear));
    }

    fn mesh(&mut self, id: ObjectId, verts: &[PolyVertex]) -> Option<&PolylineFillMesh> {
        let entry = self.entries.entry(id).or_insert_with(|| CachedPolylineFill {
            verts: verts.to_vec(),
            mesh: triangulate_polyline_fill(verts),
        });
        if entry.verts != verts {
            entry.verts = verts.to_vec();
            entry.mesh = triangulate_polyline_fill(verts);
        }
        entry.mesh.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocumentPrimitive {
    Fill,
    Stroke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocumentRenderStage {
    Opaque,
    Translucent,
    Overlay,
    AlwaysVisible,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DocumentDrawBatch {
    pub(crate) primitive: DocumentPrimitive,
    pub(crate) index_range: (u32, u32),
    pub(crate) center: DVec3,
    stage: DocumentRenderStage,
}

impl DocumentDrawBatch {
    pub(crate) fn stage(self, xray_enabled: bool) -> DocumentRenderStage {
        if xray_enabled { DocumentRenderStage::AlwaysVisible } else { self.stage }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TextDrawBatch {
    pub(crate) index_range: (u32, u32),
    stage: DocumentRenderStage,
}

impl TextDrawBatch {
    pub(crate) fn stage(self, xray_enabled: bool) -> DocumentRenderStage {
        if xray_enabled { DocumentRenderStage::AlwaysVisible } else { self.stage }
    }
}

#[derive(Clone, Copy)]
struct DocumentObjectRanges {
    entity: Option<SceneEntityId>,
    stroke_index_range: (u32, u32),
    fill_index_range: (u32, u32),
    center: DVec3,
}

pub(crate) fn rebuild_document_scene(input: DocumentSceneBuildInput<'_>) {
    let DocumentSceneBuildInput {
        editor,
        document,
        static_ids,
        fill_cache,
        text_system,
        lyon_buffer,
        stroke_vertex_buf,
        stroke_index_buf,
        text_vertex_buf,
        text_index_buf,
        text_draw_batches,
        pick_records,
        text_pick_records,
        document_draw_batches,
        scene_origin,
        scale_factor,
    } = input;

    fill_cache.retain_document(document);
    lyon_buffer.indices.clear();
    lyon_buffer.vertices.clear();
    stroke_index_buf.clear();
    stroke_vertex_buf.clear();
    text_vertex_buf.clear();
    text_index_buf.clear();
    text_draw_batches.clear();
    pick_records.clear();
    text_pick_records.clear();
    document_draw_batches.clear();

    let mut object_ranges = Vec::new();

    {
        let mut draw_ctx = DrawContext {
            stroke_vertex_buf,
            stroke_index_buf,
            fill_vertex_buf: &mut lyon_buffer.vertices,
            fill_index_buf: &mut lyon_buffer.indices,
            scene_origin,
            scale_factor,
        };

        // The document is the sole source of geometry. Draw each object in world
        // space, honoring layer visibility / hide / freeze, and record a pick range
        // unless frozen so objects select and highlight. Selected objects are
        // emitted last so overlapping selected strokes remain visible.
        let mut objects: Vec<&Object> = document.objects().iter().collect();
        objects.sort_by_key(|object| {
            let handle = SceneEntityId::Object(object.id());
            editor.selected_handles.contains(&handle) || editor.tool_highlight_id == Some(object.id())
        });
        for object in objects {
            if static_ids.contains(&object.id()) {
                continue;
            }
            let handle = SceneEntityId::Object(object.id());
            let layer_visible = document.layer(object.layer()).map(|layer| layer.visible).unwrap_or(true);
            if !layer_visible || editor.hidden_handles.contains(&handle) {
                continue;
            }
            let rgba = document.object_rgba(object);
            let stroke_start = draw_ctx.stroke_vertex_buf.len() as u32;
            let stroke_index_start = draw_ctx.stroke_index_buf.len() as u32;
            let fill_start = draw_ctx.fill_vertex_buf.len() as u32;
            let fill_index_start = draw_ctx.fill_index_buf.len() as u32;
            let mut fill_opaque = false;
            match object {
                Object::Point { pos, .. } => {
                    draw_screen_cross(&mut draw_ctx, *pos, 6.0, DOC_LINE_WIDTH, rgba);
                }
                Object::Polyline {
                    verts, closed, fill, line_weight, ..
                } => {
                    let line_rgba = rgba;
                    let fill_rgba = document.object_fill_rgba(object);
                    tessellate_polyline_stroke(&mut draw_ctx, verts, *closed, *line_weight, line_rgba);
                    if *closed
                        && verts.len() >= 2
                        && *fill != crate::model::FillStyle::Clear
                        && let Some(fill_mesh) = fill_cache.mesh(object.id(), verts)
                    {
                        match fill {
                            crate::model::FillStyle::Solid => {
                                fill_opaque = fill_rgba[3] >= 1.0 - f32::EPSILON;
                                fill_polyline_solid(draw_ctx.fill_vertex_buf, draw_ctx.fill_index_buf, fill_mesh, fill_rgba, draw_ctx.scene_origin);
                            }
                            crate::model::FillStyle::Slashes | crate::model::FillStyle::Crosses => {
                                let hatch_spacing = polyline_hatch_spacing(fill_mesh, 15.0);
                                fill_polyline_hatch(&mut draw_ctx, fill_mesh, fill_rgba, 45.0, hatch_spacing, *line_weight);
                                if *fill == crate::model::FillStyle::Crosses {
                                    fill_polyline_hatch(&mut draw_ctx, fill_mesh, fill_rgba, 135.0, hatch_spacing, *line_weight);
                                }
                            }
                            crate::model::FillStyle::Clear => {}
                        }
                    }
                }
                Object::Text {
                    pos, content, height, rotation, ..
                } => {
                    let is_editing = editor.editing_labels_id == Some(object.id());
                    let (render_content, render_height, render_rotation, render_rgba) = if is_editing {
                        (
                            &editor.pending_text,
                            editor.pending_text_height,
                            editor.pending_text_rotation_degrees.to_radians(),
                            editor.pending_text_color,
                        )
                    } else {
                        (content, *height, *rotation, rgba)
                    };
                    let mut textbox = TextBox::new(vec![Text::new(render_content.clone(), render_rgba)], 1.0);
                    textbox.font_size = DOC_TEXT_FONT_SIZE;
                    let scale = (render_height / textbox.font_size as f64).max(f64::EPSILON) as f32;
                    let layout_bounds = (f32::MAX, f32::MAX);
                    let layout_width = f64::from(textbox.layout_width(text_system, layout_bounds)) * f64::from(scale);
                    let corners = text_bounds_corners_with_layout_width(*pos, render_content, render_height, render_rotation, layout_width);
                    text_pick_records.push(TextPickRecord { entity: handle, corners });
                    if is_editing || editor.selected_handles.contains(&handle) {
                        let indicator_color = if is_editing { TEXT_EDIT_INDICATOR_COLOR } else { crate::ui::SELECTION_COLOR_F32 };
                        for edge in corners.iter().copied().zip(corners.iter().copied().cycle().skip(1)).take(4) {
                            draw_line(&mut draw_ctx, edge.0, edge.1, 2.0, indicator_color);
                        }
                    }
                    let local = *pos - scene_origin;
                    let matrix = Mat4::from_translation(local.as_vec3()) * Mat4::from_rotation_z(render_rotation as f32) * Mat4::from_scale(Vec3::new(scale, -scale, scale));
                    let text_index_start = text_index_buf.len() as u32;
                    textbox.append_meshes(text_system, layout_bounds, matrix, text_vertex_buf, text_index_buf);
                    let text_index_end = text_index_buf.len() as u32;
                    if text_index_start < text_index_end {
                        push_text_batch(
                            text_draw_batches,
                            TextDrawBatch {
                                index_range: (text_index_start, text_index_end),
                                stage: text_render_stage(is_editing),
                            },
                        );
                    }
                }
            }
            let stroke_end = draw_ctx.stroke_vertex_buf.len() as u32;
            let stroke_index_end = draw_ctx.stroke_index_buf.len() as u32;
            let fill_end = draw_ctx.fill_vertex_buf.len() as u32;
            let fill_index_end = draw_ctx.fill_index_buf.len() as u32;
            if stroke_index_end > stroke_index_start || fill_index_end > fill_index_start {
                object_ranges.push(DocumentObjectRanges {
                    entity: Some(handle),
                    stroke_index_range: (stroke_index_start, stroke_index_end),
                    fill_index_range: (fill_index_start, fill_index_end),
                    center: object_center(object),
                });
            }
            if (stroke_end > stroke_start || fill_end > fill_start)
                && !editor.frozen_handles.contains(&handle)
                && let Some(world_bounds) = world_bounds_from_local_positions(
                    draw_ctx.stroke_vertex_buf[stroke_start as usize..stroke_end as usize]
                        .iter()
                        .map(|vertex| vertex.pos)
                        .chain(draw_ctx.fill_vertex_buf[fill_start as usize..fill_end as usize].iter().map(|vertex| vertex.pos)),
                    scene_origin,
                )
            {
                pick_records.push(PickRecord {
                    entity: handle,
                    world_bounds,
                    stroke_range: (stroke_start, stroke_end),
                    stroke_index_range: (stroke_index_start, stroke_index_end),
                    fill_range: (fill_start, fill_end),
                    fill_index_range: (fill_index_start, fill_index_end),
                    fill_opaque,
                });
            }
        }
    }

    if !editor.translucent_handles.is_empty() {
        for rec in pick_records.iter() {
            if !editor.translucent_handles.contains(&rec.entity) {
                continue;
            }
            let stroke = rec.stroke_range.0 as usize..rec.stroke_range.1 as usize;
            for vertex in &mut stroke_vertex_buf[stroke] {
                make_translucent(&mut vertex.color);
            }
            let fill = rec.fill_range.0 as usize..rec.fill_range.1 as usize;
            for vertex in &mut lyon_buffer.vertices[fill] {
                make_translucent(&mut vertex.color);
            }
        }
    }

    if !editor.tri_hover_handles.is_empty() {
        for rec in pick_records.iter() {
            if !editor.tri_hover_handles.contains(&rec.entity) {
                continue;
            }
            let stroke = rec.stroke_range.0 as usize..rec.stroke_range.1 as usize;
            for vertex in &mut stroke_vertex_buf[stroke] {
                vertex.color = YELLOW_HIGHLIGHT_COLOR;
            }
            let fill = rec.fill_range.0 as usize..rec.fill_range.1 as usize;
            for vertex in &mut lyon_buffer.vertices[fill] {
                vertex.color = YELLOW_HIGHLIGHT_COLOR;
            }
        }
    }

    if !editor.selected_handles.is_empty() {
        for rec in pick_records.iter() {
            if !editor.selected_handles.contains(&rec.entity) {
                continue;
            }
            let stroke = rec.stroke_range.0 as usize..rec.stroke_range.1 as usize;
            for vertex in &mut stroke_vertex_buf[stroke] {
                vertex.color = crate::ui::SELECTION_COLOR_F32;
            }
            let fill = rec.fill_range.0 as usize..rec.fill_range.1 as usize;
            for vertex in &mut lyon_buffer.vertices[fill] {
                vertex.color = crate::ui::SELECTION_COLOR_F32;
            }
        }
    }

    if let Some(highlight_id) = editor.tool_highlight_id {
        let handle = SceneEntityId::Object(highlight_id);
        if let Some(rec) = pick_records.iter().find(|r| r.entity == handle) {
            let stroke = rec.stroke_range.0 as usize..rec.stroke_range.1 as usize;
            for vertex in &mut stroke_vertex_buf[stroke] {
                vertex.color = crate::ui::SELECTION_COLOR_F32;
            }
            let fill = rec.fill_range.0 as usize..rec.fill_range.1 as usize;
            for vertex in &mut lyon_buffer.vertices[fill] {
                vertex.color = crate::ui::SELECTION_COLOR_F32;
            }
        }
    }

    rebuild_document_draw_batches(
        document_draw_batches,
        &object_ranges,
        editor,
        stroke_vertex_buf,
        stroke_index_buf,
        &lyon_buffer.vertices,
        &lyon_buffer.indices,
    );
}

fn object_center(object: &Object) -> DVec3 {
    match object {
        Object::Point { pos, .. } | Object::Text { pos, .. } => *pos,
        Object::Polyline { verts, .. } => average_positions(verts.iter().map(|vertex| vertex.pos)),
    }
}

fn average_positions(points: impl Iterator<Item = DVec3>) -> DVec3 {
    let (sum, count) = points.fold((DVec3::ZERO, 0usize), |(sum, count), point| (sum + point, count + 1));
    if count == 0 { DVec3::ZERO } else { sum / count as f64 }
}

fn rebuild_document_draw_batches(
    batches: &mut Vec<DocumentDrawBatch>,
    objects: &[DocumentObjectRanges],
    editor: &EditorState,
    stroke_vertices: &[StrokeVertex],
    stroke_indices: &[u32],
    fill_vertices: &[Vertex],
    fill_indices: &[u32],
) {
    batches.clear();
    for object in objects {
        let always_visible = object
            .entity
            .is_some_and(|entity| editor.editing_labels_id.is_some_and(|id| entity == SceneEntityId::Object(id)));
        let overlay = object.entity.is_some_and(|entity| {
            editor.selected_handles.contains(&entity)
                || editor.tri_hover_handles.contains(&entity)
                || editor.tool_highlight_id.is_some_and(|id| entity == SceneEntityId::Object(id))
        });
        append_alpha_runs(
            batches,
            DocumentPrimitive::Stroke,
            object.stroke_index_range,
            object.center,
            overlay,
            always_visible,
            stroke_indices,
            |index| stroke_vertices.get(index as usize).map(|vertex| vertex.color[3]),
        );
        append_alpha_runs(
            batches,
            DocumentPrimitive::Fill,
            object.fill_index_range,
            object.center,
            overlay,
            always_visible,
            fill_indices,
            |index| fill_vertices.get(index as usize).map(|vertex| vertex.color[3]),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn append_alpha_runs(
    batches: &mut Vec<DocumentDrawBatch>,
    primitive: DocumentPrimitive,
    index_range: (u32, u32),
    center: DVec3,
    overlay: bool,
    always_visible: bool,
    indices: &[u32],
    alpha_for_vertex: impl Fn(u32) -> Option<f32>,
) {
    let start = (index_range.0 as usize).min(indices.len());
    let end = (index_range.1 as usize).min(indices.len());
    let end = start + (end.saturating_sub(start) / 3) * 3;
    if start >= end {
        return;
    }

    let stage_for_triangle = |triangle_start: usize| {
        if always_visible {
            DocumentRenderStage::AlwaysVisible
        } else if overlay {
            DocumentRenderStage::Overlay
        } else if indices.get(triangle_start).and_then(|&index| alpha_for_vertex(index)).unwrap_or(1.0) < 0.999 {
            DocumentRenderStage::Translucent
        } else {
            DocumentRenderStage::Opaque
        }
    };

    let mut run_start = start;
    let mut run_stage = stage_for_triangle(start);
    for triangle_start in (start + 3..end).step_by(3) {
        let stage = stage_for_triangle(triangle_start);
        if stage == run_stage {
            continue;
        }
        push_document_batch(
            batches,
            DocumentDrawBatch {
                primitive,
                index_range: (run_start as u32, triangle_start as u32),
                center,
                stage: run_stage,
            },
        );
        run_start = triangle_start;
        run_stage = stage;
    }
    push_document_batch(
        batches,
        DocumentDrawBatch {
            primitive,
            index_range: (run_start as u32, end as u32),
            center,
            stage: run_stage,
        },
    );
}

fn push_document_batch(batches: &mut Vec<DocumentDrawBatch>, batch: DocumentDrawBatch) {
    // Fill and stroke indices live in separate streams, so batches of one
    // primitive can remain contiguous even when the other primitive was
    // appended between them. Coalescing opaque/overlay runs keeps large point
    // and other documents at a handful of draw calls; translucent runs retain
    // per-object centers for back-to-front sorting.
    if let Some(previous) = batches.iter_mut().rev().find(|previous| previous.primitive == batch.primitive)
        && previous.stage == batch.stage
        && previous.index_range.1 == batch.index_range.0
        && (batch.stage != DocumentRenderStage::Translucent || previous.center == batch.center)
    {
        previous.index_range.1 = batch.index_range.1;
        return;
    }
    batches.push(batch);
}

fn push_text_batch(batches: &mut Vec<TextDrawBatch>, batch: TextDrawBatch) {
    if let Some(previous) = batches.last_mut()
        && previous.stage == batch.stage
        && previous.index_range.1 == batch.index_range.0
        && batch.stage != DocumentRenderStage::Translucent
    {
        previous.index_range.1 = batch.index_range.1;
    } else {
        batches.push(batch);
    }
}

fn text_render_stage(is_editing: bool) -> DocumentRenderStage {
    if is_editing {
        DocumentRenderStage::AlwaysVisible
    } else {
        // Match an ordinary selected-text box: draw late while retaining
        // scene depth.
        DocumentRenderStage::Overlay
    }
}

pub(crate) struct DynamicSceneBuildInput<'a> {
    pub(crate) editor: &'a EditorState,
    pub(crate) dynamic_vertex_buf: &'a mut Vec<StrokeVertex>,
    pub(crate) dynamic_index_buf: &'a mut Vec<u32>,
    pub(crate) scene_origin: DVec3,
    pub(crate) scale_factor: f32,
}

/// Per-frame geometry for the live drawing tools: the batter/berm preview.
/// Stroke-only and tiny, so rebuilding it every frame while a tool is active
/// is cheap - unlike the full document scene it replaces in that role.
pub(crate) fn rebuild_dynamic_scene(input: DynamicSceneBuildInput<'_>) {
    let DynamicSceneBuildInput {
        editor,
        dynamic_vertex_buf,
        dynamic_index_buf,
        scene_origin,
        scale_factor,
    } = input;

    dynamic_vertex_buf.clear();
    dynamic_index_buf.clear();

    let mut unused_fill_vertices: Vec<Vertex> = Vec::new();
    let mut unused_fill_indices: Vec<u32> = Vec::new();
    let mut draw_ctx = DrawContext {
        stroke_vertex_buf: dynamic_vertex_buf,
        stroke_index_buf: dynamic_index_buf,
        fill_vertex_buf: &mut unused_fill_vertices,
        fill_index_buf: &mut unused_fill_indices,
        scene_origin,
        scale_factor,
    };

    if editor.batter_berm_dialog_open {
        const BATTER_BERM_PREVIEW_COLOR: [f32; 4] = [1.0, 0.86, 0.0, 1.0];

        for ring in &editor.batter_berm_rings_world {
            for pair in ring.windows(2) {
                draw_line(&mut draw_ctx, pair[0], pair[1], 2.0, BATTER_BERM_PREVIEW_COLOR);
            }
            if editor.batter_berm_preview_closed
                && ring.len() >= 2
                && let (Some(&first), Some(&last)) = (ring.first(), ring.last())
            {
                draw_line(&mut draw_ctx, last, first, 2.0, BATTER_BERM_PREVIEW_COLOR);
            }
        }
    }
}
