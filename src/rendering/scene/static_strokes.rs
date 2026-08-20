//! Persistent GPU cache for stroke geometry of stable polylines.
//!
//! The per-rebuild scene builder re-tessellates every object on any document
//! change, which is fine for hand-drawn documents but ruinous once an
//! operation (contouring, string imports) drops tens of thousands of dense
//! polylines into the document. This cache claims those polylines, groups them
//! into per-layer chunks with their own GPU buffers, and re-tessellates only
//! the chunks whose members actually changed. Everything else — points, text,
//! filled polygons, and any polyline the editor is currently styling
//! (selection, highlight, translucency) — stays on the per-rebuild path.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
};

use glam::DVec3;

use crate::{
    model::{Document, FillStyle, LayerId, Object, ObjectId, SceneEntityId},
    rendering::{
        StrokeVertex, Vertex,
        geometry::{DrawContext, tessellate_polyline_stroke},
        pick::{PickRecord, world_bounds_from_local_positions},
    },
    ui::state::EditorState,
};

/// Soft per-chunk stroke-vertex budget (~27 MiB of vertex data). Small enough
/// that re-tessellating one chunk (an edited or newly selected member) stays
/// within a frame; large enough that contour-scale documents need only dozens
/// of draw calls.
const CHUNK_VERTEX_BUDGET: usize = 512 * 1024;

pub(crate) struct StaticStrokeChunk {
    layer: LayerId,
    /// Mirrors the layer's visibility each sync so draw and pick can skip the
    /// chunk without rebuilding anything when a layer is toggled.
    pub(crate) layer_visible: bool,
    members: Vec<ObjectId>,
    /// Estimated stroke vertices including members assigned since the last
    /// rebuild; used only to decide when to start a new chunk.
    estimated_vertices: usize,
    estimated_indices: usize,
    dirty: bool,
    /// CPU copies are retained for picking (cursor pick and box select read
    /// vertex positions back).
    pub(crate) vertices: Vec<StrokeVertex>,
    pub(crate) indices: Vec<u32>,
    /// Per-member pick records with ranges into this chunk's buffers (fill
    /// ranges are always empty — filled polylines are ineligible).
    pub(crate) records: Vec<PickRecord>,
    /// Union of member pick bounds, used to reject this entire CPU stream on
    /// cursor queries that land elsewhere.
    pub(crate) world_bounds: Option<(DVec3, DVec3)>,
    pub(crate) vertex_gpu: Option<wgpu::Buffer>,
    pub(crate) index_gpu: Option<wgpu::Buffer>,
    vertex_capacity: usize,
    index_capacity: usize,
    pub(crate) index_count: u32,
}

impl StaticStrokeChunk {
    fn new(layer: LayerId) -> Self {
        Self {
            layer,
            layer_visible: true,
            members: Vec::new(),
            estimated_vertices: 0,
            estimated_indices: 0,
            dirty: false,
            vertices: Vec::new(),
            indices: Vec::new(),
            records: Vec::new(),
            world_bounds: None,
            vertex_gpu: None,
            index_gpu: None,
            vertex_capacity: 0,
            index_capacity: 0,
            index_count: 0,
        }
    }

    pub(crate) fn drawable(&self) -> bool {
        self.layer_visible && self.index_count > 0
    }
}

#[derive(Default)]
pub(crate) struct StaticStrokeCache {
    chunks: Vec<StaticStrokeChunk>,
    /// Chunk index and last-built fingerprint per claimed object.
    object_chunk: HashMap<ObjectId, (usize, u64)>,
    claimed: HashSet<ObjectId>,
    cached_scene_origin: DVec3,
    cached_scale_factor: f32,
}

/// Everything the cache bakes into vertices besides the object's own data.
/// A mismatch forces the member's chunk to rebuild.
fn fingerprint(object_revision: u64, rgba: [f32; 4], layer: LayerId) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    object_revision.hash(&mut hasher);
    rgba.map(f32::to_bits).hash(&mut hasher);
    layer.hash(&mut hasher);
    hasher.finish()
}

/// Whether the cache may own this object's stroke geometry. Anything the
/// editor is currently restyling stays on the per-rebuild path, which already
/// implements recoloring, fills, and draw-on-top ordering for it.
fn eligible(object: &Object, editor: &EditorState, rgba: [f32; 4]) -> bool {
    let Object::Polyline { closed, fill, .. } = object else {
        return false;
    };
    if *closed && *fill != FillStyle::Clear {
        return false;
    }
    // Transparent strokes stay in the document stream so they can be routed
    // through the non-depth-writing transparency pass.
    if rgba[3] < 0.999 {
        return false;
    }
    let handle = SceneEntityId::Object(object.id());
    !(editor.hidden_handles.contains(&handle)
        || editor.frozen_handles.contains(&handle)
        || editor.selected_handles.contains(&handle)
        || editor.translucent_handles.contains(&handle)
        || editor.tri_hover_handles.contains(&handle)
        || editor.tool_highlight_id == Some(object.id()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StrokeEstimate {
    vertices: usize,
    indices: usize,
}

#[derive(Clone, Copy)]
struct StaticStrokeLimits {
    vertices: usize,
    indices: usize,
    buffer_bytes: usize,
}

impl StaticStrokeLimits {
    fn from_device(device: &wgpu::Device) -> Self {
        let buffer_bytes = usize::try_from(device.limits().max_buffer_size).unwrap_or(usize::MAX);
        Self {
            vertices: (buffer_bytes / size_of::<StrokeVertex>()).min(CHUNK_VERTEX_BUDGET),
            // Every stroke primitive is a triangle, so leave no partial one.
            indices: buffer_bytes / size_of::<u32>() / 3 * 3,
            buffer_bytes,
        }
    }

    fn contains(self, estimate: StrokeEstimate) -> bool {
        estimate.vertices <= self.vertices && estimate.indices <= self.indices
    }
}

/// Conservative bounds for the straight-polyline tessellator. Bulged lines
/// stay on the checked main stream because one logical arc may expand to
/// thousands of primitives and does not make a predictable static-cache
/// member.
fn estimate_stroke(object: &Object) -> Option<StrokeEstimate> {
    let Object::Polyline { verts, closed, .. } = object else {
        return None;
    };
    if verts.iter().any(|vertex| vertex.bulge.abs() > f64::EPSILON) {
        return None;
    }
    let segments = verts.len().saturating_sub(1).saturating_add(usize::from(*closed && verts.len() >= 2));
    let joins = if *closed && verts.len() >= 2 { verts.len() } else { verts.len().saturating_sub(2) };
    // A segment emits four vertices/six indices. A maximum-resolution round
    // join emits eighteen vertices/forty-eight indices. Degenerate and nearly
    // collinear geometry only reduces these counts.
    Some(StrokeEstimate {
        vertices: segments.saturating_mul(4).saturating_add(joins.saturating_mul(18)),
        indices: segments.saturating_mul(6).saturating_add(joins.saturating_mul(48)),
    })
}

fn can_append(current: usize, next: usize, limit: usize) -> bool {
    current.checked_add(next).is_some_and(|combined| combined <= limit)
}

impl StaticStrokeCache {
    /// Object ids whose stroke geometry this cache owns; the scene builder
    /// must skip them.
    pub(crate) fn claimed(&self) -> &HashSet<ObjectId> {
        &self.claimed
    }

    pub(crate) fn chunks(&self) -> &[StaticStrokeChunk] {
        &self.chunks
    }

    /// Reconcile the cache with the current document and editor state,
    /// re-tessellating and re-uploading only chunks with changed members.
    /// Runs on every geometry rebuild, so per-object work here must stay
    /// cheap (a fingerprint compare) for unchanged objects.
    pub(crate) fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, document: &Document, editor: &EditorState, scene_origin: DVec3, scale_factor: f32) {
        let limits = StaticStrokeLimits::from_device(device);
        // Origin and scale factor are baked into every vertex.
        if scene_origin != self.cached_scene_origin || (scale_factor - self.cached_scale_factor).abs() > f32::EPSILON {
            self.chunks.clear();
            self.object_chunk.clear();
            self.cached_scene_origin = scene_origin;
            self.cached_scale_factor = scale_factor;
        }

        self.claimed.clear();
        for object in document.objects() {
            let rgba = document.object_rgba(object);
            if !eligible(object, editor, rgba) {
                continue;
            }
            // Oversized and bulged members remain on the main stream, whose
            // checked truncation handles them without creating an unbounded
            // cache allocation.
            let Some(estimate) = estimate_stroke(object).filter(|estimate| limits.contains(*estimate)) else {
                continue;
            };
            let id = object.id();
            let fp = fingerprint(document.object_revision(id), rgba, object.layer());
            match self.object_chunk.get(&id).copied() {
                Some((chunk_index, cached_fp)) => {
                    let moved_layer = self.chunks[chunk_index].layer != object.layer();
                    if moved_layer {
                        self.remove_member(id, chunk_index);
                        self.assign(object.layer(), id, estimate, limits);
                    } else if cached_fp != fp {
                        self.chunks[chunk_index].dirty = true;
                    }
                }
                None => self.assign(object.layer(), id, estimate, limits),
            }
            self.claimed.insert(id);
        }

        // Members that vanished or became ineligible release their chunk.
        let stale: Vec<(ObjectId, usize)> = self
            .object_chunk
            .iter()
            .filter(|(id, _)| !self.claimed.contains(id))
            .map(|(&id, &(chunk_index, _))| (id, chunk_index))
            .collect();
        for (id, chunk_index) in stale {
            self.remove_member(id, chunk_index);
            self.object_chunk.remove(&id);
        }

        for chunk in &mut self.chunks {
            chunk.layer_visible = document.layer(chunk.layer).map(|layer| layer.visible).unwrap_or(true);
        }

        for chunk_index in 0..self.chunks.len() {
            if self.chunks[chunk_index].dirty {
                self.rebuild_chunk(chunk_index, device, queue, document, scene_origin, scale_factor, limits);
            }
        }
    }

    fn assign(&mut self, layer: LayerId, id: ObjectId, estimate: StrokeEstimate, limits: StaticStrokeLimits) {
        let chunk_index = self
            .chunks
            .iter()
            .position(|chunk| {
                chunk.layer == layer
                    && can_append(chunk.estimated_vertices, estimate.vertices, limits.vertices)
                    && can_append(chunk.estimated_indices, estimate.indices, limits.indices)
            })
            .unwrap_or_else(|| {
                self.chunks.push(StaticStrokeChunk::new(layer));
                self.chunks.len() - 1
            });
        let chunk = &mut self.chunks[chunk_index];
        chunk.members.push(id);
        chunk.estimated_vertices += estimate.vertices;
        chunk.estimated_indices += estimate.indices;
        chunk.dirty = true;
        // The real fingerprint is stored when the chunk rebuilds.
        self.object_chunk.insert(id, (chunk_index, 0));
    }

    fn remove_member(&mut self, id: ObjectId, chunk_index: usize) {
        let chunk = &mut self.chunks[chunk_index];
        chunk.members.retain(|member| *member != id);
        chunk.dirty = true;
    }

    #[allow(clippy::too_many_arguments)]
    fn rebuild_chunk(
        &mut self,
        chunk_index: usize,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        document: &Document,
        scene_origin: DVec3,
        scale_factor: f32,
        limits: StaticStrokeLimits,
    ) {
        let chunk = &mut self.chunks[chunk_index];
        chunk.vertices.clear();
        chunk.indices.clear();
        chunk.records.clear();

        // Eligible polylines never emit fill geometry; these stay empty.
        let mut unused_fill_vertices: Vec<Vertex> = Vec::new();
        let mut unused_fill_indices: Vec<u32> = Vec::new();

        for member_index in 0..chunk.members.len() {
            let id = chunk.members[member_index];
            let Some(object) = document.get_object(id) else {
                continue;
            };
            let Object::Polyline { verts, closed, line_weight, .. } = object else {
                continue;
            };
            let rgba = document.object_rgba(object);
            let stroke_start = chunk.vertices.len() as u32;
            let stroke_index_start = chunk.indices.len() as u32;
            {
                let mut draw_ctx = DrawContext {
                    stroke_vertex_buf: &mut chunk.vertices,
                    stroke_index_buf: &mut chunk.indices,
                    fill_vertex_buf: &mut unused_fill_vertices,
                    fill_index_buf: &mut unused_fill_indices,
                    scene_origin,
                    scale_factor,
                };
                tessellate_polyline_stroke(&mut draw_ctx, verts, *closed, *line_weight, rgba);
            }
            let stroke_end = chunk.vertices.len() as u32;
            if stroke_end > stroke_start
                && let Some(world_bounds) =
                    world_bounds_from_local_positions(chunk.vertices[stroke_start as usize..stroke_end as usize].iter().map(|vertex| vertex.pos), scene_origin)
            {
                chunk.records.push(PickRecord {
                    entity: SceneEntityId::Object(id),
                    world_bounds,
                    stroke_range: (stroke_start, stroke_end),
                    stroke_index_range: (stroke_index_start, chunk.indices.len() as u32),
                    fill_range: (0, 0),
                    fill_index_range: (0, 0),
                    fill_opaque: false,
                });
            }
            let fp = fingerprint(document.object_revision(id), rgba, object.layer());
            self.object_chunk.insert(id, (chunk_index, fp));
        }

        chunk.estimated_vertices = chunk.vertices.len();
        chunk.estimated_indices = chunk.indices.len();
        chunk.world_bounds = chunk
            .records
            .iter()
            .map(|record| record.world_bounds)
            .reduce(|(min_a, max_a), (min_b, max_b)| (min_a.min(min_b), max_a.max(max_b)));
        chunk.dirty = false;

        let vertex_uploaded = upload(
            device,
            queue,
            &mut chunk.vertex_gpu,
            &mut chunk.vertex_capacity,
            bytemuck::cast_slice(&chunk.vertices),
            wgpu::BufferUsages::VERTEX,
            "Static Stroke Chunk Vertex Buffer",
            limits.buffer_bytes,
        );
        let index_uploaded = upload(
            device,
            queue,
            &mut chunk.index_gpu,
            &mut chunk.index_capacity,
            bytemuck::cast_slice(&chunk.indices),
            wgpu::BufferUsages::INDEX,
            "Static Stroke Chunk Index Buffer",
            limits.buffer_bytes,
        );
        chunk.index_count = if vertex_uploaded && index_uploaded {
            u32::try_from(chunk.indices.len()).unwrap_or(0)
        } else {
            0
        };
    }
}

/// (Re)create `buffer` if `data` outgrew it, then write `data`.
#[allow(clippy::too_many_arguments)]
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &mut Option<wgpu::Buffer>,
    capacity_bytes: &mut usize,
    data: &[u8],
    usage: wgpu::BufferUsages,
    label: &'static str,
    max_buffer_bytes: usize,
) -> bool {
    if data.is_empty() {
        return true;
    }
    if data.len() > max_buffer_bytes {
        crate::userspace_error!(
            "{label} needs {} MiB, exceeding the GPU's {} MiB per-buffer limit; this stroke chunk will not be drawn",
            data.len() / (1024 * 1024),
            max_buffer_bytes / (1024 * 1024),
        );
        *buffer = None;
        *capacity_bytes = 0;
        return false;
    }
    if buffer.is_none() || data.len() > *capacity_bytes {
        *capacity_bytes = checked_buffer_capacity(data.len(), max_buffer_bytes).expect("static stroke data was checked against the device buffer limit");
        *buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: *capacity_bytes as wgpu::BufferAddress,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }
    if let Some(buffer) = buffer {
        queue.write_buffer(buffer, 0, data);
    }
    true
}

fn checked_buffer_capacity(required: usize, maximum: usize) -> Option<usize> {
    if required == 0 || required > maximum {
        return None;
    }
    Some(required.checked_next_power_of_two().unwrap_or(maximum).min(maximum))
}
