use super::*;

impl<'a> Graphics<'a> {
    pub(super) fn create_scene_cache_target(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration, blit_layout: &wgpu::BindGroupLayout) -> SceneCacheTarget {
        let scene_format = config.format.add_srgb_suffix();
        let view_formats = (scene_format != config.format).then_some(scene_format).into_iter().collect::<Vec<_>>();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Main Scene Cache"),
            size: wgpu::Extent3d {
                width: config.width.max(1),
                height: config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            // Read back by the blit that restores the cached scene under the
            // editor overlay, never copied - so the surface itself needs no
            // COPY_DST and this target works on every adapter.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &view_formats,
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Main Scene Cache View"),
            format: Some(scene_format),
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Main Scene Cache Bind Group"),
            layout: blit_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        SceneCacheTarget { view, bind_group }
    }

    /// Layout and pipeline for the fullscreen fetch that puts the cached scene
    /// back into the multisample target before the overlay draws over it.
    pub(super) fn create_scene_cache_blit(device: &wgpu::Device, scene_format: wgpu::TextureFormat, sample_count: u32) -> (wgpu::BindGroupLayout, wgpu::RenderPipeline) {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Main Scene Cache Blit Bind Group Layout"),
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
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/scene_cache_blit.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Main Scene Cache Blit Pipeline Layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        // The pass it runs in carries the scene depth buffer, which the blit
        // must leave exactly as the scene left it: every overlay draw after it
        // still tests against that depth.
        let mut depth = Self::depth_state(false, 0);
        depth.depth_compare = Some(wgpu::CompareFunction::Always);
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Main Scene Cache Blit Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
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
        });
        (layout, pipeline)
    }

    /// Keep expensive full-resolution block-model attachments resident only
    /// while this viewport has visible work that consumes them.
    pub(super) fn update_block_model_optional_targets(&mut self, needs_transparency: bool, needs_volume: bool) {
        if needs_transparency {
            if self.block_model_transparency_targets.is_none() {
                let targets = Self::create_block_model_transparency_targets(
                    &self.device,
                    &self.config,
                    &self.depth_view,
                    &self.block_model_transparency_fallback_bind_group_layout,
                    &self.block_model_transparency_composite_bind_group_layout,
                );
                self.block_model_transparency_targets = Some(targets);
            }
        } else {
            self.block_model_transparency_targets = None;
        }

        if needs_volume {
            if self.block_model_volume_target.is_none() {
                let target = Self::create_block_model_volume_target(
                    &self.device,
                    &self.config,
                    &self.block_model_volume_upscale_bind_group_layout,
                    &self.block_model_beam_bind_group_layout,
                );
                self.block_model_volume_target = Some(target);
            }
        } else {
            self.block_model_volume_target = None;
        }
    }

    pub(super) fn create_msaa_target(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration, sample_count: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("MSAA Color Target"),
            size: wgpu::Extent3d {
                width: config.width.max(1),
                height: config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: config.format.add_srgb_suffix(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    }

    pub(super) fn create_depth_target(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration, sample_count: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Scene Depth Target"),
            size: wgpu::Extent3d {
                width: config.width.max(1),
                height: config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    pub(super) fn create_block_model_transparency_targets(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        scene_depth_view: &wgpu::TextureView,
        transparency_fallback_layout: &wgpu::BindGroupLayout,
        composite_layout: &wgpu::BindGroupLayout,
    ) -> BlockModelTransparencyTargets {
        let size = wgpu::Extent3d {
            width: config.width.max(1),
            height: config.height.max(1),
            depth_or_array_layers: 1,
        };
        let accum_textures = vec![device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Block Model Transparency Accum"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })];
        let accum_views = accum_textures
            .iter()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect::<Vec<_>>();
        let transparency_fallback_bind_groups = vec![device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: transparency_fallback_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(scene_depth_view),
            }],
            label: Some("Block Model Transparency Bind Group"),
        })];
        let composite_bind_groups = vec![device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: composite_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&accum_views[0]),
            }],
            label: Some("Block Model Transparency Composite Bind Group"),
        })];
        BlockModelTransparencyTargets {
            _accum_textures: accum_textures,
            accum_views,
            transparency_fallback_bind_groups,
            composite_bind_groups,
        }
    }

    /// Off-screen target the volume raycast draws into so it can render at a
    /// reduced resolution during interaction and be upscaled afterwards. The
    /// texture is always full surface size; the raycast fills only a
    /// `scale`-sized top-left sub-rect (set via the render pass viewport), so
    /// the scale can change every frame without reallocating anything.
    pub(super) fn create_block_model_volume_target(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        layout: &wgpu::BindGroupLayout,
        beam_layout: &wgpu::BindGroupLayout,
    ) -> BlockModelVolumeTarget {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Block Model Volume Low-Res Target"),
            size: wgpu::Extent3d {
                width: config.width.max(1),
                height: config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format.add_srgb_suffix(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // vec4: xy = the sub-rect (in pixels) rendered this frame.
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Block Model Volume Upscale Params"),
            size: size_of::<[f32; 4]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
            label: Some("Block Model Volume Upscale Bind Group"),
        });
        // Beam pre-pass target: one texel per 8x8 tile of the full surface,
        // holding a conservative ray entry depth. Like the main target, only
        // the top-left `scaled/8` sub-rect is written each frame.
        let beam_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Block Model Volume Beam Target"),
            size: wgpu::Extent3d {
                width: config.width.max(1).div_ceil(8),
                height: config.height.max(1).div_ceil(8),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let beam_view = beam_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let beam_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: beam_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&beam_view),
            }],
            label: Some("Block Model Volume Beam Bind Group"),
        });
        BlockModelVolumeTarget {
            _texture: texture,
            view,
            params_buffer,
            bind_group,
            _beam_texture: beam_texture,
            beam_view,
            beam_bind_group,
        }
    }

    pub(super) fn depth_state(write_enabled: bool, bias: i32) -> wgpu::DepthStencilState {
        wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(write_enabled),
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState {
                // Reversed-Z also reverses the direction of a bias used to
                // pull overlays toward the camera.
                constant: -bias,
                slope_scale: -bias.signum() as f32,
                clamp: 0.0,
            },
        }
    }
}
