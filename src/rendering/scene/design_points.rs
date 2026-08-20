//! Persistent GPU markers for editable design vertices.
//!
//! `View Points` and vertex-editing tools must expose every source vertex, so
//! this cache deliberately has no display LOD.  One compact position instance
//! per vertex keeps camera movement on the GPU instead of re-projecting and
//! tessellating thousands of egui circles every frame.

use std::{
    collections::HashSet,
    hash::{DefaultHasher, Hash, Hasher},
};

use glam::DVec3;
use wgpu::util::DeviceExt;

use crate::{
    model::{Document, Object, SceneEntityId, geometry::compact_circle_center},
    rendering::scene::PointPosition,
};

/// Keep allocations comfortably below the per-buffer device limit and make
/// very large documents drawable on adapters with smaller limits.
const TARGET_CHUNK_BYTES: usize = 16 * 1024 * 1024;

/// Mirrors `DesignPointStyle` in `design_point.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DesignPointStyleUniform {
    color: [f32; 4],
    /// x: marker size in physical pixels.
    options: [f32; 4],
    /// Positions are already relative to the renderer's floating origin.
    origin: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DesignPointCacheKey {
    content_key: u64,
    hidden_key: u64,
    scene_origin_bits: [u64; 3],
}

pub(crate) struct DesignPointChunk {
    pub(crate) instance_buffer: wgpu::Buffer,
    pub(crate) instance_count: u32,
}

pub(crate) struct DesignPointGpuCache {
    pub(crate) chunks: Vec<DesignPointChunk>,
    pub(crate) outer_style_bind_group: wgpu::BindGroup,
    pub(crate) inner_style_bind_group: wgpu::BindGroup,
    outer_style_buffer: wgpu::Buffer,
    inner_style_buffer: wgpu::Buffer,
    key: Option<DesignPointCacheKey>,
    enabled: bool,
    scale_factor: f32,
}

impl DesignPointGpuCache {
    pub(crate) fn new(device: &wgpu::Device, style_layout: &wgpu::BindGroupLayout) -> Self {
        let outer_style = style_uniform([0.0, 0.0, 0.0, 1.0], 8.0);
        let inner_style = style_uniform([1.0, 1.0, 1.0, 1.0], 6.0);
        let outer_style_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Design Point Outer Style"),
            contents: bytemuck::bytes_of(&outer_style),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let inner_style_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Design Point Inner Style"),
            contents: bytemuck::bytes_of(&inner_style),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = |label, buffer: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: style_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            })
        };
        let outer_style_bind_group = bind_group("Design Point Outer Style Bind Group", &outer_style_buffer);
        let inner_style_bind_group = bind_group("Design Point Inner Style Bind Group", &inner_style_buffer);
        Self {
            chunks: Vec::new(),
            outer_style_bind_group,
            inner_style_bind_group,
            outer_style_buffer,
            inner_style_buffer,
            key: None,
            enabled: false,
            scale_factor: 1.0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        document: &Document,
        hidden: &HashSet<SceneEntityId>,
        scene_origin: DVec3,
        scale_factor: f32,
        enabled: bool,
        content_dirty: bool,
    ) {
        let scale_factor = scale_factor.max(1.0);
        if (self.scale_factor - scale_factor).abs() > f32::EPSILON {
            self.scale_factor = scale_factor;
            let outer = style_uniform([0.0, 0.0, 0.0, 1.0], 8.0 * scale_factor);
            let inner = style_uniform([1.0, 1.0, 1.0, 1.0], 6.0 * scale_factor);
            queue.write_buffer(&self.outer_style_buffer, 0, bytemuck::bytes_of(&outer));
            queue.write_buffer(&self.inner_style_buffer, 0, bytemuck::bytes_of(&inner));
        }

        if !enabled {
            if self.enabled {
                self.chunks.clear();
                self.key = None;
                self.enabled = false;
            }
            return;
        }

        // `scene_document()` preserves each source object's revision, while
        // its own aggregate revision mainly reflects how many layers were
        // appended. Recompute this compact source signature only when the
        // renderer reports content dirtiness; selection-only rebuilds then
        // avoid re-uploading every marker in a large document.
        let content_key = if !self.enabled || content_dirty {
            design_point_content_key(document)
        } else {
            self.key.map(|key| key.content_key).unwrap_or_else(|| design_point_content_key(document))
        };
        let key = cache_key(content_key, hidden, scene_origin);
        // Geometry invalidation also covers selection/style changes. Once the
        // source revisions, visibility, and floating origin still produce the
        // same key, keep the existing GPU buffers instead of re-uploading every
        // design vertex for an unrelated editor-state change.
        if self.enabled && self.key == Some(key) {
            return;
        }

        let positions = collect_design_point_positions(document, hidden, scene_origin);
        let device_limit = usize::try_from(device.limits().max_buffer_size).unwrap_or(usize::MAX);
        let chunk_bytes = TARGET_CHUNK_BYTES.min(device_limit).max(size_of::<PointPosition>());
        let instances_per_chunk = (chunk_bytes / size_of::<PointPosition>()).min(u32::MAX as usize).max(1);
        self.chunks = positions
            .chunks(instances_per_chunk)
            .map(|instances| DesignPointChunk {
                instance_buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Design Point Instances"),
                    contents: bytemuck::cast_slice(instances),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                instance_count: instances.len() as u32,
            })
            .collect();
        self.key = Some(key);
        self.enabled = true;
    }
}

fn style_uniform(color: [f32; 4], size_px: f32) -> DesignPointStyleUniform {
    DesignPointStyleUniform {
        color,
        options: [size_px, 0.0, 0.0, 0.0],
        origin: [0.0; 4],
    }
}

fn cache_key(content_key: u64, hidden: &HashSet<SceneEntityId>, scene_origin: DVec3) -> DesignPointCacheKey {
    let hidden_key = hidden.iter().fold(hidden.len() as u64, |acc, entity| {
        let mut hasher = DefaultHasher::new();
        entity.hash(&mut hasher);
        acc ^ hasher.finish()
    });
    DesignPointCacheKey {
        content_key,
        hidden_key,
        scene_origin_bits: scene_origin.to_array().map(f64::to_bits),
    }
}

fn design_point_content_key(document: &Document) -> u64 {
    let mut hasher = DefaultHasher::new();
    document.layers().len().hash(&mut hasher);
    for layer in document.layers() {
        layer.id.hash(&mut hasher);
        layer.visible.hash(&mut hasher);
    }
    document.objects().len().hash(&mut hasher);
    for object in document.objects() {
        object.id().hash(&mut hasher);
        object.layer().hash(&mut hasher);
        document.object_revision(object.id()).hash(&mut hasher);
    }
    hasher.finish()
}

fn collect_design_point_positions(document: &Document, hidden: &HashSet<SceneEntityId>, scene_origin: DVec3) -> Vec<PointPosition> {
    let mut positions = Vec::new();
    for object in document.objects() {
        let entity = SceneEntityId::Object(object.id());
        let visible = !hidden.contains(&entity) && document.layer(object.layer()).is_none_or(|layer| layer.visible);
        if !visible {
            continue;
        }
        let mut push = |position: DVec3| {
            if position.is_finite() {
                positions.push(PointPosition {
                    pos: (position - scene_origin).as_vec3().to_array(),
                });
            }
        };
        match object {
            Object::Point { pos, .. } => push(*pos),
            Object::Polyline { verts, closed, .. } => {
                if let Some(center) = compact_circle_center(verts, *closed) {
                    push(center);
                } else {
                    verts.iter().for_each(|vertex| push(vertex.pos));
                }
            }
            Object::Text { .. } => {}
        }
    }
    positions
}
