//! Criação das bind group layouts e render pipelines usadas pelo `Renderer`
//! — extraído de `Renderer::new()` (que já era gigante só de boilerplate
//! wgpu) pra cada pipeline ficar isolado e legível separadamente.

use crate::gpu::geometry::{LineVertex, SliceVertex};
use crate::gpu::text::TextVertex;

use super::gpu_setup::{DEPTH_FORMAT, VOLUME_FILTERABLE};

pub(crate) fn create_scene_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("scene_bind_group_layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

pub(crate) fn create_volume_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("volume_bind_group_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: VOLUME_FILTERABLE,
                    },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(if VOLUME_FILTERABLE {
                    wgpu::SamplerBindingType::Filtering
                } else {
                    wgpu::SamplerBindingType::NonFiltering
                }),
                count: None,
            },
        ],
    })
}

pub(crate) fn create_colormap_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("colormap_bind_group_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D1,
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
    })
}

pub(crate) fn create_slice_params_bind_group_layout(
    device: &wgpu::Device,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("slice_params_bind_group_layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            // VERTEX porque `slice.model` posiciona o quad no espaço 3D
            // (Fase 4); FRAGMENT porque `slice.axis`/`slice.index` ainda
            // escolhem a coordenada amostrada.
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

pub(crate) fn create_text_params_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("text_params_bind_group_layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

pub(crate) fn create_font_atlas_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("font_atlas_bind_group_layout"),
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
        ],
    })
}

/// Pipeline das fatias (`AxisAlignedImage`): textura 3D + colormap + luz
/// headlight opcional (visão 2D desliga). Blend normal (não `REPLACE`) pra
/// opacidade ajustável; depth normal, escreve profundidade (fatia é
/// geometria "real" que oclui/é ocluída por outras fatias e pelo wireframe).
pub(crate) fn create_slice_pipeline(
    device: &wgpu::Device,
    scene_bind_group_layout: &wgpu::BindGroupLayout,
    volume_bind_group_layout: &wgpu::BindGroupLayout,
    colormap_bind_group_layout: &wgpu::BindGroupLayout,
    slice_params_bind_group_layout: &wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
    sample_count: u32,
) -> wgpu::RenderPipeline {
    let slice_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("volume_slice_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/volume_slice.wgsl").into()),
    });

    let slice_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("slice_pipeline_layout"),
        bind_group_layouts: &[
            Some(scene_bind_group_layout),
            Some(volume_bind_group_layout),
            Some(colormap_bind_group_layout),
            Some(slice_params_bind_group_layout),
        ],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("slice_pipeline"),
        layout: Some(&slice_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &slice_shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(SliceVertex::layout())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &slice_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                // Blend normal (não REPLACE): com opacidade default 1.0 o
                // resultado é idêntico a opaco (src cobre 100%, dst não
                // contribui) — só passa a misturar de verdade quando o
                // usuário abaixa a opacidade do volume (ver colormap.rs).
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    })
}

/// Pipeline do wireframe da caixa do cubo: sem textura, sem luz — só a
/// câmera (`@group(0)`), position+color por vértice, `LineList`. É um
/// objeto de verdade na cena (`depth_compare: Less`), não um HUD: uma
/// aresta fica escondida atrás de uma fatia opaca na frente dela. Não
/// escreve profundidade, então também não bloqueia nada desenhado depois
/// (os ticks do grid de eixo, o texto).
pub(crate) fn create_wireframe_pipeline(
    device: &wgpu::Device,
    scene_bind_group_layout: &wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
    sample_count: u32,
) -> wgpu::RenderPipeline {
    let wireframe_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("wireframe_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/wireframe.wgsl").into()),
    });

    let wireframe_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wireframe_pipeline_layout"),
            bind_group_layouts: &[Some(scene_bind_group_layout)],
            immediate_size: 0,
        });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("wireframe_pipeline"),
        layout: Some(&wireframe_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &wireframe_shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(LineVertex::layout())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &wireframe_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    })
}

/// Pipeline de texto GPU nativo (labels de eixo, e futuramente nomes de
/// poço): fonte bitmap embutida (`font.rs`). O grid (wireframe + ticks +
/// texto) é um objeto de verdade na cena — `depth_compare: Less` faz um
/// label ficar escondido atrás de uma fatia opaca na frente dele, igual
/// qualquer geometria normal. Não escreve profundidade (um label não
/// deveria ocluir outro atrás dele por causa de um pixel transparente do
/// glifo).
pub(crate) fn create_text_pipeline(
    device: &wgpu::Device,
    scene_bind_group_layout: &wgpu::BindGroupLayout,
    text_params_bind_group_layout: &wgpu::BindGroupLayout,
    font_atlas_bind_group_layout: &wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
    sample_count: u32,
) -> wgpu::RenderPipeline {
    let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("text_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/text.wgsl").into()),
    });

    let text_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("text_pipeline_layout"),
        bind_group_layouts: &[
            Some(scene_bind_group_layout),
            Some(text_params_bind_group_layout),
            Some(font_atlas_bind_group_layout),
        ],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("text_pipeline"),
        layout: Some(&text_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &text_shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(TextVertex::layout())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &text_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    })
}
