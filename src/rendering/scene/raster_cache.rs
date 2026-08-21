//! Persistent GPU textures for georeferenced raster previews.

use std::collections::{HashMap, HashSet};

use glam::DVec3;
use wgpu::util::DeviceExt;

use crate::model::raster::{OpenRasterTexture, RasterTextureId};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RasterMapUniform {
    uv_x: [f32; 4],
    uv_y: [f32; 4],
}

struct CachedRasterGpu {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    _sampler: wgpu::Sampler,
    map_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Scene-relative footprint quad for the flat plan view (triangle strip
    /// of `[x, y, u, v]`); `None` when the geotransform is degenerate.
    plane_vertex_buffer: Option<wgpu::Buffer>,
    world_to_uv: [f64; 6],
    scene_origin: DVec3,
}

#[derive(Default)]
pub(crate) struct RasterGpuCache {
    rasters: HashMap<RasterTextureId, CachedRasterGpu>,
    fallback: Option<CachedRasterGpu>,
}

impl RasterGpuCache {
    pub(crate) fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene_origin: DVec3, rasters: &[OpenRasterTexture], layout: &wgpu::BindGroupLayout) {
        if self.fallback.is_none() {
            self.fallback = Some(upload_raster(
                device,
                queue,
                layout,
                &[255, 255, 255, 255],
                [1, 1],
                [0.0; 6],
                scene_origin,
                "Fallback Raster",
            ));
        }

        let loaded: HashSet<_> = rasters.iter().filter(|raster| raster.state.loaded).map(|raster| raster.id).collect();
        self.rasters.retain(|id, _| loaded.contains(id));
        for raster in rasters {
            if !raster.state.loaded {
                continue;
            }
            if let Some(cached) = self.rasters.get_mut(&raster.id) {
                if cached.scene_origin != scene_origin {
                    let map = map_uniform(raster.world_to_uv, scene_origin);
                    queue.write_buffer(&cached.map_buffer, 0, bytemuck::bytes_of(&map));
                    if let (Some(buffer), Some(vertices)) = (&cached.plane_vertex_buffer, plane_vertices(cached.world_to_uv, scene_origin)) {
                        queue.write_buffer(buffer, 0, bytemuck::cast_slice(&vertices));
                    }
                    cached.scene_origin = scene_origin;
                }
                continue;
            }
            self.rasters.insert(
                raster.id,
                upload_raster(device, queue, layout, &raster.rgba, raster.preview_size, raster.world_to_uv, scene_origin, &raster.name),
            );
        }
    }

    pub(crate) fn bind_group(&self, id: Option<RasterTextureId>) -> &wgpu::BindGroup {
        &id.and_then(|id| self.rasters.get(&id))
            .or(self.fallback.as_ref())
            .expect("raster cache is synchronized before drawing")
            .bind_group
    }

    /// Bind group and footprint-quad vertex buffer for the flat plan view.
    pub(crate) fn plane(&self, id: RasterTextureId) -> Option<(&wgpu::BindGroup, &wgpu::Buffer)> {
        let cached = self.rasters.get(&id)?;
        Some((&cached.bind_group, cached.plane_vertex_buffer.as_ref()?))
    }
}

/// Triangle-strip corners `[x, y, u, v]` of the raster footprint in
/// scene-origin-relative world XY, from the inverted affine geotransform.
fn plane_vertices(world_to_uv: [f64; 6], scene_origin: DVec3) -> Option<[[f32; 4]; 4]> {
    let [a, b, c, d, e, f] = world_to_uv;
    let det = a * e - b * d;
    if !det.is_finite() || det.abs() < 1.0e-30 {
        return None;
    }
    let corner = |u: f64, v: f64| {
        let x = (e * (u - c) - b * (v - f)) / det;
        let y = (a * (v - f) - d * (u - c)) / det;
        [(x - scene_origin.x) as f32, (y - scene_origin.y) as f32, u as f32, v as f32]
    };
    Some([corner(0.0, 0.0), corner(1.0, 0.0), corner(0.0, 1.0), corner(1.0, 1.0)])
}

fn map_uniform(world_to_uv: [f64; 6], scene_origin: DVec3) -> RasterMapUniform {
    let [a, b, c, d, e, f] = world_to_uv;
    RasterMapUniform {
        uv_x: [a as f32, b as f32, (a * scene_origin.x + b * scene_origin.y + c) as f32, 0.0],
        uv_y: [d as f32, e as f32, (d * scene_origin.x + e * scene_origin.y + f) as f32, 0.0],
    }
}

#[allow(clippy::too_many_arguments)]
fn upload_raster(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    rgba: &[u8],
    size: [u32; 2],
    world_to_uv: [f64; 6],
    scene_origin: DVec3,
    name: &str,
) -> CachedRasterGpu {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some(name),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        rgba,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Raster Surface Sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let map = map_uniform(world_to_uv, scene_origin);
    let map_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Raster World-to-UV Uniform"),
        contents: bytemuck::bytes_of(&map),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Raster Surface Bind Group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: map_buffer.as_entire_binding(),
            },
        ],
    });
    let plane_vertex_buffer = plane_vertices(world_to_uv, scene_origin).map(|vertices| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Raster Plane Vertex Buffer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        })
    });
    CachedRasterGpu {
        _texture: texture,
        _view: view,
        _sampler: sampler,
        map_buffer,
        bind_group,
        plane_vertex_buffer,
        world_to_uv,
        scene_origin,
    }
}
