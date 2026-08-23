use super::*;
use crate::userspace_log;

impl<'a> Graphics<'a> {
    pub(crate) async fn new(window: Arc<Window>) -> Result<Graphics<'a>> {
        let window_size = window.inner_size();
        // The web backend's ResizeObserver may not have delivered its first
        // event when asynchronous WebGPU initialization begins. Read the
        // attached canvas directly so the first surface is not permanently
        // configured as the 1x1 placeholder below.
        #[cfg(target_arch = "wasm32")]
        let window_size = {
            use winit::platform::web::WindowExtWebSys;

            window.canvas().map_or(window_size, |canvas| {
                let scale_factor = window.scale_factor();
                winit::dpi::PhysicalSize::new(
                    (f64::from(canvas.client_width().max(1)) * scale_factor).round() as u32,
                    (f64::from(canvas.client_height().max(1)) * scale_factor).round() as u32,
                )
            })
        };
        // Minimized/hidden Wayland windows may initially report 0×0. wgpu
        // surfaces, projection aspect ratios and attachments all require a
        // non-zero placeholder until the first real resize arrives.
        let size = winit::dpi::PhysicalSize::new(window_size.width.max(1), window_size.height.max(1));

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });

        let surface = instance.create_surface(window.clone())?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| anyhow!("No compatible GPU adapter found: {e:?}"))?;

        let adapter_info = adapter.get_info();
        userspace_log!(
            "GPU Adapter: {} / {} / {:?} / {:?}",
            adapter_info.vendor,
            adapter_info.name,
            adapter_info.backend,
            adapter_info.device_type
        );
        userspace_log!("GPU Driver: {} {}", adapter_info.driver, adapter_info.driver_info);

        let adapter_limits = adapter.limits();
        // Take everything the adapter offers for buffer size: large surfaces
        // can tessellate to multi-GiB vertex streams.
        let required_limits = wgpu::Limits {
            max_buffer_size: adapter_limits.max_buffer_size,
            // Take the adapter's real storage-buffer binding size instead of
            // wgpu's conservative 128 MiB default. The block-model volume
            // raycaster binds dense per-brick tables and a cell pool as single
            // storage buffers; at 128 MiB a large translucent model overflows
            // them and silently falls back to the far slower instanced-cube
            // path. The per-stage storage-buffer *count* limit is untouched
            // (the volume bind group already sits at the default 8).
            max_storage_buffer_binding_size: adapter_limits.max_storage_buffer_binding_size,
            // Full-resolution raster previews may opt into the adapter's
            // larger texture limit instead of wgpu's conservative default.
            max_texture_dimension_2d: adapter_limits.max_texture_dimension_2d,
            ..wgpu::Limits::default()
        };
        userspace_log!(
            "GPU Limits: max_buffer_size={} MiB, max_storage_buffer_binding_size={} MiB, max_texture_dimension_2d={}, max_bind_groups={}",
            required_limits.max_buffer_size / (1024 * 1024),
            required_limits.max_storage_buffer_binding_size / (1024 * 1024),
            adapter_limits.max_texture_dimension_2d,
            adapter_limits.max_bind_groups
        );
        if required_limits.max_buffer_size < COMFORTABLE_MAX_BUFFER_SIZE {
            crate::userspace_warn!(
                "GPU supports a maximum buffer size of {} MiB; large scenes may not display fully",
                required_limits.max_buffer_size / (1024 * 1024)
            );
        }

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: wgpu::Features::empty(),
                required_limits,
                label: None,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
            .await?;

        // wgpu treats uncaptured errors as fatal panics by default; a
        // validation failure (e.g. an oversized allocation) should degrade to
        // missing geometry, not lose the user's session.
        device.on_uncaptured_error(Arc::new(|error: wgpu::Error| {
            crate::userspace_error!("wgpu error (continuing): {error}");
        }));

        let surface_caps = surface.get_capabilities(&adapter);
        // Browser WebGPU surfaces expose only the base `*Unorm` canvas
        // formats, while native backends commonly expose their `*UnormSrgb`
        // counterparts directly. The configured format must come from the
        // capability list, but a compatible view may differ in sRGB-ness.
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|format| matches!(*format, wgpu::TextureFormat::Rgba8UnormSrgb | wgpu::TextureFormat::Bgra8UnormSrgb))
            .or_else(|| {
                surface_caps
                    .formats
                    .iter()
                    .copied()
                    .find(|format| matches!(*format, wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm))
            })
            .ok_or_else(|| anyhow!("Surface reports no supported 8-bit sRGB-compatible format"))?;
        let present_mode = surface_caps
            .present_modes
            .iter()
            .copied()
            .find(|m| *m == wgpu::PresentMode::Fifo)
            .or_else(|| surface_caps.present_modes.first().copied())
            .ok_or_else(|| anyhow!("Surface reports no supported present modes"))?;
        userspace_log!("Surface present mode: {present_mode:?}");
        let alpha_mode = surface_caps
            .alpha_modes
            .first()
            .copied()
            .ok_or_else(|| anyhow!("Surface reports no supported alpha modes"))?;
        // Scene shaders output linear colour and therefore render through an
        // sRGB view. egui applies gamma itself and renders through the base
        // unorm view of the same surface texture.
        let scene_format = surface_format.add_srgb_suffix();
        let gui_format = surface_format.remove_srgb_suffix();
        let view_formats = vec![if surface_format.is_srgb() { gui_format } else { scene_format }];
        let supports_scene_cache = surface_caps.usages.contains(wgpu::TextureUsages::COPY_DST);
        if !supports_scene_cache {
            log::warn!("Surface does not support COPY_DST; main-scene caching is disabled on this adapter");
        }
        let surface_usage = if supports_scene_cache {
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        };
        let config = wgpu::SurfaceConfiguration {
            usage: surface_usage,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode,
            view_formats,
            desired_maximum_frame_latency: 2,
        };
        let sample_count = MSAA_SAMPLE_COUNT;
        let (msaa_color, msaa_view) = Self::create_msaa_target(&device, &config, sample_count);
        let scene_cache = supports_scene_cache.then(|| Self::create_scene_cache_target(&device, &config));
        let (depth_texture, depth_view) = Self::create_depth_target(&device, &config, sample_count);

        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/shader.wgsl"));
        let surface_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/surface.wgsl"));
        let grid_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/grid.wgsl"));
        let block_model_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/block_model.wgsl"));
        let block_model_volume_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/block_model_volume.wgsl"));
        let block_model_transparency_fallback_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/block_model_transparency_fallback.wgsl"));
        let block_model_transparency_composite_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/block_model_transparency_composite.wgsl"));
        let block_model_volume_upscale_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/block_model_volume_upscale.wgsl"));
        let stroke_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/stroke.wgsl"));
        let edge_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/edge.wgsl"));
        let point_cloud_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/point_cloud.wgsl"));
        let drill_hole_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/drill_hole.wgsl"));
        let design_point_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/design_point.wgsl"));

        let camera = Camera::new(DVec3::new(0.0, 0.0, 10.0), (-90.0_f64).to_radians(), 0.0);
        let projection = Projection::new(config.width, config.height, INITIAL_CAMERA_Z_NEAR, INITIAL_CAMERA_Z_FAR);
        let camera_controller = CameraController::new(0.6, 0.005, CAMERA_ROTATE_SENSITIVITY);
        let fly_camera_controller = FlyCameraController::new(232., CAMERA_ROTATE_SENSITIVITY);

        let mut camera_uniform = CameraUniform::new();
        camera_uniform.update_view_proj(&camera, &projection, DVec3::ZERO, 1.0);
        camera_uniform.update_viewport(config.width, config.height);

        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Camera Buffer"),
            contents: bytemuck::cast_slice(&[camera_uniform]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let camera_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
            label: Some("camera_bind_group_layout"),
        });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
            label: Some("camera_bind_group"),
        });

        let initial_grid_uniform = GridUniform::new(DVec3::ZERO, crate::app::io::default_renderer_background_color(), &camera, &projection, 1.0, false);
        let grid_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("XY Grid Uniform Buffer"),
            contents: bytemuck::bytes_of(&initial_grid_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let grid_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("XY Grid Bind Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let grid_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("XY Grid Bind Group"),
            layout: &grid_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: grid_buffer.as_entire_binding(),
            }],
        });

        let render_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Render Pipeline Layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout)],
            immediate_size: 0,
        });
        let grid_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("XY Grid Pipeline Layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout), Some(&grid_bind_group_layout)],
            immediate_size: 0,
        });

        let style_bind_group_layout_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let surface_style_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[style_bind_group_layout_entry],
            label: Some("surface_style_bind_group_layout"),
        });
        let surface_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Surface Pipeline Layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout), Some(&surface_style_bind_group_layout)],
            immediate_size: 0,
        });
        // Per-chunk rebase offset for triangulation surfaces (group 2); block
        // model pipelines keep the plain two-group surface layout above.
        let surface_chunk_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
            label: Some("surface_chunk_bind_group_layout"),
        });
        let raster_surface_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raster_surface_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let tri_surface_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Triangulation Surface Pipeline Layout"),
            bind_group_layouts: &[
                Some(&camera_bind_group_layout),
                Some(&surface_style_bind_group_layout),
                Some(&surface_chunk_bind_group_layout),
                Some(&raster_surface_bind_group_layout),
            ],
            immediate_size: 0,
        });
        let block_model_transparency_fallback_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: true,
                },
                count: None,
            }],
            label: Some("block_model_transparency_fallback_bind_group_layout"),
        });
        let block_model_transparency_fallback_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Block Model Transparency Fallback Pipeline Layout"),
            bind_group_layouts: &[
                Some(&camera_bind_group_layout),
                Some(&surface_style_bind_group_layout),
                Some(&block_model_transparency_fallback_bind_group_layout),
            ],
            immediate_size: 0,
        });
        let block_model_transparency_composite_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
            label: Some("block_model_transparency_composite_bind_group_layout"),
        });
        let block_model_transparency_composite_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Block Model Transparency Composite Pipeline Layout"),
            bind_group_layouts: &[Some(&block_model_transparency_composite_bind_group_layout)],
            immediate_size: 0,
        });
        let block_model_volume_upscale_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
            label: Some("block_model_volume_upscale_bind_group_layout"),
        });
        let block_model_volume_upscale_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Block Model Volume Upscale Pipeline Layout"),
            bind_group_layouts: &[Some(&block_model_volume_upscale_bind_group_layout)],
            immediate_size: 0,
        });
        let block_model_volume_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
            label: Some("block_model_volume_bind_group_layout"),
        });
        // Beam pre-pass output, read by the main raycast (group 3):
        // R32Float, textureLoad only, so unfilterable.
        let block_model_beam_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
            label: Some("block_model_beam_bind_group_layout"),
        });
        let block_model_volume_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Block Model Volume Pipeline Layout"),
            bind_group_layouts: &[
                Some(&camera_bind_group_layout),
                Some(&block_model_volume_bind_group_layout),
                Some(&block_model_transparency_fallback_bind_group_layout),
                Some(&block_model_beam_bind_group_layout),
            ],
            immediate_size: 0,
        });
        let block_model_beam_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Block Model Beam Pipeline Layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout), Some(&block_model_volume_bind_group_layout)],
            immediate_size: 0,
        });
        let edge_style_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[style_bind_group_layout_entry],
            label: Some("edge_style_bind_group_layout"),
        });
        let edge_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Edge Pipeline Layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout), Some(&edge_style_bind_group_layout)],
            immediate_size: 0,
        });

        let vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        }];
        let surface_vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<SurfaceVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        }];
        // One instance per block: lower.xyz + grade, then upper.xyz + pad.
        // The shader expands vertex_index 0..36 into the cube's faces.
        let block_model_vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<BlockInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Float32x3],
        }];

        let stroke_vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<StrokeVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x3, 3 => Float32x2, 4 => Float32],
        }];
        let edge_instance_buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<EdgeInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        }];
        let point_uncolored_instance_buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<PointPosition>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3],
        }];
        let point_colored_instance_buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<PointInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Unorm8x4],
        }];
        let drill_hole_instance_buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<DrillSegmentInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4],
        }];

        let create_stroke_pipeline = |label, depth_stencil| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&render_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &stroke_shader,
                    entry_point: Some("vs_main"),
                    buffers: &stroke_vertex_buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &stroke_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: scene_format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil,
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let stroke_render_pipeline = create_stroke_pipeline("Depth-tested Stroke Render Pipeline", Some(Self::depth_state(false, -1)));
        let opaque_stroke_render_pipeline = create_stroke_pipeline("Opaque Depth-writing Stroke Render Pipeline", Some(Self::depth_state(true, -1)));
        let edge_render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Instanced Triangulation Edge Pipeline"),
            layout: Some(&edge_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &edge_shader,
                entry_point: Some("vs_main"),
                buffers: &edge_instance_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &edge_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(Self::depth_state(false, -1)),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        // Point splats reuse the edge pipeline layout (camera + one style
        // uniform) but write depth so clouds occlude correctly against
        // meshes and themselves.
        let point_cloud_colored_render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Colored Point Cloud Pipeline"),
            layout: Some(&edge_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &point_cloud_shader,
                entry_point: Some("vs_colored"),
                buffers: &point_colored_instance_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &point_cloud_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    // Imported point colors are opaque. Avoiding the blend
                    // unit preserves the result while allowing the
                    // depth/color path to use the cheapest opaque writes.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(Self::depth_state(true, 0)),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let point_cloud_uncolored_render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Uncolored Point Cloud Pipeline"),
            layout: Some(&edge_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &point_cloud_shader,
                entry_point: Some("vs_uncolored"),
                buffers: &point_uncolored_instance_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &point_cloud_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    // The fallback point-cloud color is currently opaque.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(Self::depth_state(true, 0)),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let create_drill_hole_pipeline = |label, depth_stencil| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&render_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &drill_hole_shader,
                    entry_point: Some("vs_main"),
                    buffers: &drill_hole_instance_buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &drill_hole_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: scene_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(depth_stencil),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let drill_hole_render_pipeline = create_drill_hole_pipeline("Opaque Drillhole Cylinder Pipeline", Self::depth_state(true, 0));
        let mut xray_drill_hole_depth = Self::depth_state(false, 0);
        xray_drill_hole_depth.depth_compare = Some(wgpu::CompareFunction::Always);
        let xray_drill_hole_render_pipeline = create_drill_hole_pipeline("X-Ray Drillhole Cylinder Pipeline", xray_drill_hole_depth);
        let mut overlay_depth = Self::depth_state(false, 0);
        overlay_depth.depth_compare = Some(wgpu::CompareFunction::Always);
        let design_point_render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Design Point Overlay Pipeline"),
            layout: Some(&edge_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &design_point_shader,
                entry_point: Some("vs_main"),
                buffers: &point_uncolored_instance_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &design_point_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(overlay_depth.clone()),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let overlay_render_pipeline = create_stroke_pipeline("Editor Overlay Render Pipeline", Some(overlay_depth));

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Render Pipeline"),
            layout: Some(&render_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(Self::depth_state(true, 0)),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        // Triangulation surface pipelines use position-only vertices with a per-draw colour
        // uniform.
        let create_tri_surface_pipeline = |label, write_depth, depth_compare| {
            let mut depth = Self::depth_state(write_depth, 0);
            depth.depth_compare = Some(depth_compare);
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&tri_surface_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &surface_shader,
                    entry_point: Some("vs_main"),
                    buffers: &surface_vertex_buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &surface_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: scene_format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: Some(depth),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let surface_render_pipeline = create_tri_surface_pipeline("Opaque Triangulation Surface Pipeline", true, wgpu::CompareFunction::GreaterEqual);
        let transparent_surface_render_pipeline = create_tri_surface_pipeline("Transparent Triangulation Surface Pipeline", false, wgpu::CompareFunction::GreaterEqual);
        let grid_render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Infinite XY Grid Pipeline"),
            layout: Some(&grid_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &grid_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &grid_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            // The grid is a background construction overlay, not an opaque
            // floor: it never writes depth and is drawn before scene
            // geometry, so nothing in the scene is ever occluded by it.
            depth_stencil: Some(Self::depth_state(false, 0)),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        // Flat plan-view images for undraped rasters: drawn first, pinned to
        // the far plane, no depth writes, so all scene geometry covers them.
        let raster_plane_shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/raster_plane.wgsl"));
        let raster_plane_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Raster Plane Pipeline Layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout), Some(&raster_surface_bind_group_layout)],
            immediate_size: 0,
        });
        let raster_plane_vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: (size_of::<f32>() * 4) as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
        }];
        let raster_plane_render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Raster Plane Pipeline"),
            layout: Some(&raster_plane_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &raster_plane_shader,
                entry_point: Some("vs_main"),
                buffers: &raster_plane_vertex_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &raster_plane_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(Self::depth_state(false, 0)),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let create_block_model_surface_pipeline = |label, write_depth, depth_compare| {
            let mut depth = Self::depth_state(write_depth, 0);
            depth.depth_compare = Some(depth_compare);
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&surface_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &block_model_shader,
                    entry_point: Some("vs_main"),
                    buffers: &block_model_vertex_buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &block_model_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: scene_format,
                        // Every fragment this pipeline draws is opaque (the
                        // chunk builder routes alpha < 0.98 to the translucent
                        // path), so skip blending entirely: zoomed-in views
                        // are fill-bound and blending doubles the per-sample
                        // colour traffic at 4x MSAA for no visual effect.
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    // The cube corner tables in block_model.wgsl deliberately
                    // wind every face inward. Exterior faces are therefore
                    // classified as back-facing and the far/interior faces as
                    // front-facing. Cull the latter so the camera-facing cube
                    // shells remain visible. The fragment shader derives its
                    // normal from derivatives and does not rely on the winding.
                    cull_mode: Some(wgpu::Face::Front),
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: Some(depth),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let block_model_render_pipeline = create_block_model_surface_pipeline("Opaque Block Model Surface Pipeline", true, wgpu::CompareFunction::GreaterEqual);
        let block_model_volume_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Block Model Volume Raycast Pipeline"),
            layout: Some(&block_model_volume_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &block_model_volume_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &block_model_volume_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            // The raycast now renders into the single-sample off-screen
            // volume target (upscaled afterwards), not the MSAA surface, so
            // this must be 1 to match the attachment.
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let block_model_beam_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Block Model Volume Beam Pipeline"),
            layout: Some(&block_model_beam_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &block_model_volume_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &block_model_volume_shader,
                entry_point: Some("fs_beam"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let block_model_transparency_fallback_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Block Model Transparency Fallback Pipeline"),
            layout: Some(&block_model_transparency_fallback_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &block_model_transparency_fallback_shader,
                entry_point: Some("vs_main"),
                buffers: &block_model_vertex_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &block_model_transparency_fallback_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let block_model_transparency_composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Block Model Transparency Composite Pipeline"),
            layout: Some(&block_model_transparency_composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &block_model_transparency_composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &block_model_transparency_composite_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let block_model_volume_upscale_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Block Model Volume Upscale Pipeline"),
            layout: Some(&block_model_volume_upscale_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &block_model_volume_upscale_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &block_model_volume_upscale_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    // Premultiplied over: matches the direct volume pass
                    // this replaces.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        // Document-object xray pipeline keeps position+colour per-vertex.
        let create_surface_pipeline = |label, write_depth, depth_compare, shader_module: &wgpu::ShaderModule| {
            let mut depth = Self::depth_state(write_depth, 0);
            depth.depth_compare = Some(depth_compare);
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&render_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: shader_module,
                    entry_point: Some("vs_main"),
                    buffers: &vertex_buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader_module,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: scene_format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: Some(depth),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let xray_render_pipeline = create_surface_pipeline("X-Ray Document Fill Pipeline", false, wgpu::CompareFunction::Always, &shader);
        let transparent_document_fill_pipeline = create_surface_pipeline("Transparent Document Fill Pipeline", false, wgpu::CompareFunction::GreaterEqual, &shader);

        let lyon_buffer: VertexBuffers<Vertex, u32> = VertexBuffers::new();
        let lyon_vertex_gpu = Self::create_stream_buffer(&device, "Lyon Vertex Buffer", size_of::<Vertex>(), wgpu::BufferUsages::VERTEX);
        let lyon_index_gpu = Self::create_stream_buffer(&device, "Lyon Index Buffer", size_of::<u32>(), wgpu::BufferUsages::INDEX);
        let stroke_vertex_gpu = Self::create_stream_buffer(&device, "Stroke Vertex Buffer", size_of::<StrokeVertex>(), wgpu::BufferUsages::VERTEX);
        let stroke_index_gpu = Self::create_stream_buffer(&device, "Stroke Index Buffer", size_of::<u32>(), wgpu::BufferUsages::INDEX);
        let overlay_vertex_gpu = Self::create_stream_buffer(&device, "Editor Overlay Vertex Buffer", size_of::<StrokeVertex>(), wgpu::BufferUsages::VERTEX);
        let overlay_index_gpu = Self::create_stream_buffer(&device, "Editor Overlay Index Buffer", size_of::<u32>(), wgpu::BufferUsages::INDEX);
        let dynamic_vertex_gpu = Self::create_stream_buffer(&device, "Dynamic Scene Vertex Buffer", size_of::<StrokeVertex>(), wgpu::BufferUsages::VERTEX);
        let dynamic_index_gpu = Self::create_stream_buffer(&device, "Dynamic Scene Index Buffer", size_of::<u32>(), wgpu::BufferUsages::INDEX);
        let text_vertex_gpu = Self::create_stream_buffer(&device, "Document Text Vertex Buffer", size_of::<Vertex>(), wgpu::BufferUsages::VERTEX);
        let text_index_gpu = Self::create_stream_buffer(&device, "Document Text Index Buffer", size_of::<u32>(), wgpu::BufferUsages::INDEX);

        let text_system = TextSystem::new();
        let gui = Gui::new(&window, &device, gui_format);
        // High-precision block-model attachments are created on first visible
        // use; ordinary document and topology views pay no full-screen VRAM
        // cost for them.
        let block_model_transparency_targets = None;
        let block_model_volume_target = None;
        let design_point_gpu = DesignPointGpuCache::new(&device, &edge_style_bind_group_layout);
        Ok(Self {
            gui,
            text_system,
            surface_render_pipeline,
            transparent_surface_render_pipeline,
            grid_render_pipeline,
            raster_plane_render_pipeline,
            block_model_render_pipeline,
            block_model_volume_pipeline,
            block_model_beam_pipeline,
            block_model_beam_bind_group_layout,
            block_model_transparency_fallback_pipeline,
            block_model_transparency_composite_pipeline,
            block_model_volume_upscale_pipeline,
            block_model_volume_upscale_bind_group_layout,
            block_model_transparency_fallback_bind_group_layout,
            block_model_transparency_composite_bind_group_layout,
            block_model_volume_bind_group_layout,
            surface_style_bind_group_layout,
            surface_chunk_bind_group_layout,
            raster_surface_bind_group_layout,
            render_pipeline,
            transparent_document_fill_pipeline,
            xray_render_pipeline,
            opaque_stroke_render_pipeline,
            stroke_render_pipeline,
            edge_render_pipeline,
            point_cloud_colored_render_pipeline,
            point_cloud_uncolored_render_pipeline,
            drill_hole_render_pipeline,
            xray_drill_hole_render_pipeline,
            design_point_render_pipeline,
            edge_style_bind_group_layout,
            overlay_render_pipeline,
            lyon_vertex_gpu,
            lyon_index_gpu,
            stroke_vertex_gpu,
            stroke_index_gpu,
            overlay_vertex_gpu,
            overlay_index_gpu,
            dynamic_vertex_gpu,
            dynamic_index_gpu,
            text_vertex_gpu,
            text_index_gpu,
            camera_buffer,
            camera_bind_group,
            grid_buffer,
            grid_bind_group,
            msaa_color,
            msaa_view,
            scene_cache,
            scene_cache_key: None,
            depth_texture,
            depth_view,
            block_model_transparency_targets,
            block_model_volume_target,
            #[cfg(not(target_arch = "wasm32"))]
            instance,
            #[cfg(not(target_arch = "wasm32"))]
            adapter,
            window,
            surface,
            queue,
            device,
            config,
            sample_count,
            size,
            lyon_buffer,
            lyon_vertex_capacity: 1,
            lyon_index_capacity: 1,
            camera,
            camera_uniform,
            camera_controller,
            fly_camera_controller,
            projection,
            mouse_pressed: None,
            fly_mode_enabled: false,
            slice_view: None,
            stroke_index_buf: Vec::new(),
            stroke_vertex_buf: Vec::new(),
            stroke_vertex_capacity: 1,
            stroke_index_capacity: 1,
            overlay_vertex_buf: Vec::new(),
            overlay_index_buf: Vec::new(),
            overlay_vertex_capacity: 1,
            overlay_index_capacity: 1,
            dynamic_vertex_buf: Vec::new(),
            dynamic_index_buf: Vec::new(),
            dynamic_vertex_capacity: 1,
            dynamic_index_capacity: 1,
            text_vertex_buf: Vec::new(),
            text_index_buf: Vec::new(),
            text_vertex_capacity: 1,
            text_index_capacity: 1,
            text_draw_batches: Vec::new(),
            frame_index: 0,
            last_text_cache_trim_frame: 0,
            last_interaction: None,
            geometry_dirty: true,
            cached_document_revision: u64::MAX,
            cached_render_style_key: None,
            cached_bounds_document_revision: u64::MAX,
            cached_scene_bounds: None,
            cached_object_aabbs: Vec::new(),
            overlay_dirty: true,
            cached_scale_factor: 0.0,
            cached_measurement_state: (false, None, None, Vec::new()),
            cached_poly_finish_dialog: false,
            pick_records: Vec::new(),
            text_pick_records: Vec::new(),
            document_draw_batches: Vec::new(),
            orbit_marker: None,
            scene_origin: DVec3::ZERO,
            vertical_exaggeration: 1.0,
            triangulation_gpu: TriangulationGpuCache::default(),
            static_strokes: StaticStrokeCache::default(),
            block_model_gpu: BlockModelGpuCache::default(),
            point_cloud_gpu: PointCloudGpuCache::default(),
            drill_hole_gpu: DrillHoleGpuCache::default(),
            design_point_gpu,
            raster_gpu: RasterGpuCache::default(),
            chunk_render_stats: (0, 0),
            plot_preview: None,
            plot_preview_key: None,
            pending_screenshot: None,
            slice_preview: None,
            embedded_slice_preview: None,
            embedded_preview_scene_key: None,
            detached_preview_scene_key: None,
        })
    }

    pub(crate) fn reconfigure(&mut self) {
        self.resize(self.size);
    }

    pub(crate) fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.mark_interaction();
            self.projection.resize(new_size.width, new_size.height);
            self.camera_uniform.update_viewport(new_size.width, new_size.height);
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
            let (msaa_color, msaa_view) = Self::create_msaa_target(&self.device, &self.config, self.sample_count);
            self.msaa_color = msaa_color;
            self.msaa_view = msaa_view;
            self.scene_cache = self
                .config
                .usage
                .contains(wgpu::TextureUsages::COPY_DST)
                .then(|| Self::create_scene_cache_target(&self.device, &self.config));
            self.scene_cache_key = None;
            let (depth_texture, depth_view) = Self::create_depth_target(&self.device, &self.config, self.sample_count);
            self.depth_texture = depth_texture;
            self.depth_view = depth_view;
            // Any lazily-created attachments refer to the old size/depth
            // view. Drop them now and recreate only if the resized viewport
            // actually renders a block model.
            self.block_model_transparency_targets = None;
            self.block_model_volume_target = None;
            // Document geometry is stored in world space and screen-space stroke
            // sizing is handled by the viewport uniform. Resizing therefore only
            // requires new surface-sized attachments; rebuilding and re-uploading
            // the entire document here makes interactive resize needlessly laggy.
            self.overlay_dirty = true;
        }
    }
}
