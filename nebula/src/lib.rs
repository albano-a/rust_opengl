mod camera;
mod colormap;
mod font;
mod geometry;
mod text;
mod volume;

use std::collections::HashMap;
use std::num::NonZeroIsize;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec2, Vec3, Vec4};
use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle};

use camera::{CameraKind, OrbitCamera, PanZoomCamera};
use colormap::Colormap;
use geometry::{
    LineVertex, SliceVertex, SLICE_INDICES, SLICE_VERTICES, WIREFRAME_INDICES, WIREFRAME_VERTICES,
};
use text::{build_text_mesh, FontAtlas, TextVertex};
use volume::Volume3D;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

// Amplitude sísmica amostrada não deveria ser interpolada silenciosamente
// entre vizinhos, e sampling linear de R32Float depende da feature
// FLOAT32_FILTERABLE (nem todo adapter tem) — nearest evita as duas questões
// de uma vez. Vale pra todo volume, não é uma escolha por instância.
const VOLUME_FILTERABLE: bool = false;

// Tamanho dos labels do grid de eixo (INLINE/CROSSLINE/TIME + valores dos
// ticks) — dois lugares únicos pra ajustar, em vez de mexer em cada chamada
// de `insert_text_label` espalhada pelo `configure_axis_grid`. Unidade é a
// mesma do cubo -1..1 (ver `TextParamsUniform`/`text.wgsl`).
const AXIS_TICK_TEXT_SCALE: f32 = 0.05;
const AXIS_CAPTION_TEXT_SCALE: f32 = 0.07;

// Quantos ticks por eixo (incluindo os dois extremos) — 5 casa bem com o
// cubo unitário (0%, 25%, 50%, 75%, 100%) sem lotar a tela de números.
const AXIS_TICK_COUNT: u32 = 5;

// Base dos ids internos dos labels do grid de eixo — bem longe de qualquer
// id que o lado Python normalmente escolhe (slices, volumes, labels
// próprios), pra nunca colidir.
const AXIS_GRID_ID_BASE: u64 = 9_000_000;

// Capacidade do buffer dinâmico dos traços curtos de tick (uma linha por
// tick, 2 vértices cada) — recalculado a cada frame conforme a câmera gira.
const AXIS_TICK_LINE_CAPACITY: usize = (AXIS_TICK_COUNT as usize) * 3 * 2;

/// Tudo que os shaders precisam saber sobre a cena por frame: câmera (pra
/// projetar vértices) e luz (pra shading). Um bind group só em vez de dois,
/// já que ambos são "globals" recalculados a cada frame.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SceneUniform {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    // xyz usados; w é só padding pra alinhamento de 16 bytes em uniform buffers.
    light_position: [f32; 4],
    camera_position: [f32; 4],
    // x = 1.0 aplica iluminação (visão 3D orbital), 0.0 não aplica (visão 2D
    // pan/zoom — seção é dado cru, não superfície lit).
    flags: [f32; 4],
    // Eixos da câmera em coordenadas de mundo, usados pro billboard dos
    // labels de texto (cada caractere sempre de frente pra tela).
    camera_right: [f32; 4],
    camera_up: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SliceParamsUniform {
    model: [[f32; 4]; 4],
    axis: u32,
    index: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct TextParamsUniform {
    // xyz = posição no mundo do centro do label; w = escala.
    anchor_scale: [f32; 4],
    // rgb = cor; a não usado.
    color: [f32; 4],
}

// Convenção espacial do cubo sísmico (cubo unitário -1..1 em cada eixo,
// mesma caixa do wireframe em geometry.rs): mundo X = Inline, mundo Y =
// Time (fatia rasa fica em cima), mundo Z = Crossline. `slice_move_axis`
// devolve a direção de translação de cada tipo de fatia; `slice_model_matrix`
// gira o quad plano (que nasce na origem, no plano XY local) pra ficar
// perpendicular ao eixo certo e o translada até a posição normalizada
// (0..1 -> -1..1) — é isso que faz Inline/Crossline/Time virarem três
// planos de verdade se cruzando dentro do cubo, em vez de um quad fixo só
// trocando de textura.
fn slice_move_axis(axis: u32) -> Vec3 {
    match axis {
        0 => Vec3::X, // Inline
        2 => Vec3::Y, // Time
        _ => Vec3::Z, // Crossline
    }
}

fn slice_model_matrix(axis: u32, index: f32) -> Mat4 {
    let pos = index.clamp(0.0, 1.0) * 2.0 - 1.0;
    match axis {
        0 => {
            // Inline fixo em X=pos; plano varre Crossline (Z) e Time (Y).
            Mat4::from_translation(Vec3::new(pos, 0.0, 0.0))
                * Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_2)
        }
        2 => {
            // Time fixo em Y=pos; plano varre Inline (X) e Crossline (Z).
            Mat4::from_translation(Vec3::new(0.0, pos, 0.0))
                * Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2)
        }
        _ => {
            // Crossline fixo em Z=pos; plano varre Inline (X) e Time (Y) —
            // é o quad plano na sua orientação original, só transladado.
            Mat4::from_translation(Vec3::new(0.0, 0.0, pos))
        }
    }
}

/// Um volume carregado na GPU (textura 3D) junto com o colormap/clim usados
/// pra colori-lo. Um `Renderer` pode ter vários, cada um com seu próprio id
/// escolhido pelo lado Python (mesmo id que o Andromeda já usa pro dataset).
struct VolumeEntry {
    volume: Volume3D,
    colormap: Colormap,
    clim: (f32, f32),
}

/// Uma fatia (AxisAlignedImage) de um volume específico: qual eixo (Inline/
/// Crossline/Time) e em que posição normalizada. Vários slices podem apontar
/// pro mesmo volume (ex: uma seção Inline e uma Crossline do mesmo dataset,
/// visíveis ao mesmo tempo).
struct SliceEntry {
    volume_id: u64,
    axis: u32,
    index: f32,
    visible: bool,
    params_buffer: wgpu::Buffer,
    params_bind_group: wgpu::BindGroup,
}

/// Um label de texto (billboard) já tesselado — a malha (`vertex_buffer`/
/// `index_buffer`) é construída uma vez a partir da string, na criação;
/// mudar posição/cor/escala só reescreve `params_buffer`.
struct TextLabelEntry {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
    params_buffer: wgpu::Buffer,
    params_bind_group: wgpu::BindGroup,
    visible: bool,
}

/// Grid numerado dos 3 eixos do cubo sísmico (Inline/Crossline/Time), no
/// espírito de um eixo 3D de matplotlib/Petrel: o texto em si (`TextLabelEntry`,
/// já em `text_labels`) é criado uma única vez em `configure_axis_grid` — só
/// a *posição* de cada label é recalculada a cada frame em `render()`,
/// porque qual aresta do cubo é "a de trás" (a que não atrapalha a leitura
/// do volume) depende de pra onde a câmera está olhando agora.
#[derive(Clone)]
struct AxisGridConfig {
    /// (label_id, axis [0=Inline/X, 1=Crossline/Z, 2=Time/Y], t 0..1 ao
    /// longo da aresta).
    tick_ids: Vec<(u64, u32, f32)>,
    /// (label_id, axis) — nome do eixo, plantado no meio da aresta escolhida.
    caption_ids: Vec<(u64, u32)>,
}

/// Ponto num eixo do cubo, deslocado `out` unidades pra fora da caixa -1..1
/// nas duas direções perpendiculares ao eixo `axis` — usado tanto pro ponto
/// que fica exatamente na aresta (`out=1.0`) quanto pros pontos mais afastados
/// (tick, texto do valor, nome do eixo). `back` são os sinais (±1) do lado do
/// cubo escolhido como "de trás" da câmera pra cada um dos 3 eixos do mundo.
fn axis_grid_point(axis: u32, local: f32, back: Vec3, out: f32) -> Vec3 {
    match axis {
        0 => Vec3::new(local, back.y * out, back.z * out), // Inline varia em X
        1 => Vec3::new(back.x * out, back.y * out, local), // Crossline varia em Z
        _ => Vec3::new(back.x * out, local, back.z * out), // Time varia em Y
    }
}

fn create_depth_view(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth_texture"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Motor de renderização Nebula, embutido num HWND emprestado de um host nativo
/// (ex: uma `QWindow` do PyQt5 via `createWindowContainer`).
///
/// O HWND precisa permanecer válido durante toda a vida deste objeto — quem garante
/// isso é o lado Python, mantendo a QWindow viva enquanto o Renderer existir.
#[pyclass]
struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,

    camera: CameraKind,
    scene_buffer: wgpu::Buffer,
    scene_bind_group: wgpu::BindGroup,

    slice_pipeline: wgpu::RenderPipeline,
    slice_vertex_buffer: wgpu::Buffer,
    slice_index_buffer: wgpu::Buffer,
    num_slice_indices: u32,

    wireframe_pipeline: wgpu::RenderPipeline,
    wireframe_vertex_buffer: wgpu::Buffer,
    wireframe_index_buffer: wgpu::Buffer,
    num_wireframe_indices: u32,

    text_pipeline: wgpu::RenderPipeline,
    text_params_bind_group_layout: wgpu::BindGroupLayout,
    font_atlas: FontAtlas,

    // Traços curtos de tick do grid de eixo — desenhados com o mesmo
    // `wireframe_pipeline` (mesmo formato de vértice, mesmo bind group de
    // cena), só que num buffer à parte porque o conteúdo muda a cada frame
    // (a aresta escolhida depende de pra onde a câmera está olhando).
    axis_tick_lines_buffer: wgpu::Buffer,
    num_axis_tick_line_vertices: u32,
    axis_grid: Option<AxisGridConfig>,

    volume_bind_group_layout: wgpu::BindGroupLayout,
    colormap_bind_group_layout: wgpu::BindGroupLayout,
    slice_params_bind_group_layout: wgpu::BindGroupLayout,

    volumes: HashMap<u64, VolumeEntry>,
    slices: HashMap<u64, SliceEntry>,
    text_labels: HashMap<u64, TextLabelEntry>,
}

#[pymethods]
impl Renderer {
    #[new]
    fn new(hwnd: isize, width: u32, height: u32, mode: String) -> PyResult<Self> {
        let hwnd = NonZeroIsize::new(hwnd)
            .ok_or_else(|| PyRuntimeError::new_err("hwnd não pode ser zero"))?;

        let mut win32_handle = Win32WindowHandle::new(hwnd);
        win32_handle.hinstance = None;
        let raw_window_handle = RawWindowHandle::Win32(win32_handle);
        let raw_display_handle = RawDisplayHandle::Windows(WindowsDisplayHandle::new());

        let instance = wgpu::Instance::default();

        // SAFETY: o chamador (lado Python) garante que o HWND permanece válido
        // enquanto este `Renderer` existir.
        let surface = unsafe {
            instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(raw_display_handle),
                    raw_window_handle,
                })
                .map_err(|e| PyRuntimeError::new_err(format!("falha ao criar surface: {e}")))?
        };

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .map_err(|e| PyRuntimeError::new_err(format!("nenhum adapter wgpu disponível: {e}")))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            ..Default::default()
        }))
        .map_err(|e| PyRuntimeError::new_err(format!("falha ao criar device: {e}")))?;

        // Por padrão, um erro de validação não capturado faz o wgpu chamar panic!()
        // no processo inteiro (derrubando o interpretador Python junto). Isso acontece
        // de forma inofensiva durante o teardown da janela (ex: um resize chega depois
        // do HWND já ter sido destruído pelo Qt) — aqui só logamos em vez de crashar.
        device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
            eprintln!("[nebula] erro wgpu não capturado (ignorado): {error}");
        }));

        let config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or_else(|| PyRuntimeError::new_err("surface incompatível com o adapter"))?;
        let surface_format = config.format;
        surface.configure(&device, &config);

        let depth_view = create_depth_view(&device, &config);

        let aspect = config.width as f32 / config.height as f32;
        let camera = match mode.as_str() {
            "panzoom" | "2d" => CameraKind::PanZoom(PanZoomCamera::new(aspect)),
            _ => CameraKind::Orbit(OrbitCamera::new(aspect)),
        };

        let scene_bind_group_layout =
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
            });

        let initial_eye = camera.eye();
        let (initial_right, initial_up) = camera.basis();
        let initial_scene_uniform = SceneUniform {
            view_proj: camera.view_proj().to_cols_array_2d(),
            model: Mat4::IDENTITY.to_cols_array_2d(),
            // Luz "headlight": acompanha a câmera em vez de ficar fixa no mundo,
            // pra o lado da superfície que você está olhando ficar sempre bem
            // iluminado, não importa de que ângulo — é assim que o Petrel faz.
            light_position: [initial_eye.x, initial_eye.y, initial_eye.z, 0.0],
            camera_position: [initial_eye.x, initial_eye.y, initial_eye.z, 0.0],
            flags: [camera.lighting_enabled(), 0.0, 0.0, 0.0],
            camera_right: [initial_right.x, initial_right.y, initial_right.z, 0.0],
            camera_up: [initial_up.x, initial_up.y, initial_up.z, 0.0],
        };

        let scene_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("scene_buffer"),
                contents: bytemuck::cast_slice(&[initial_scene_uniform]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        );

        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene_bind_group"),
            layout: &scene_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scene_buffer.as_entire_binding(),
            }],
        });

        let volume_bind_group_layout =
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
            });

        let colormap_bind_group_layout =
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
            });

        let slice_params_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("slice_params_bind_group_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // VERTEX porque `slice.model` posiciona o quad no
                    // espaço 3D (Fase 4); FRAGMENT porque `slice.axis`/
                    // `slice.index` ainda escolhem a coordenada amostrada.
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let slice_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("volume_slice_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("volume_slice.wgsl").into()),
        });

        let slice_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("slice_pipeline_layout"),
            bind_group_layouts: &[
                Some(&scene_bind_group_layout),
                Some(&volume_bind_group_layout),
                Some(&colormap_bind_group_layout),
                Some(&slice_params_bind_group_layout),
            ],
            immediate_size: 0,
        });

        let slice_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let slice_vertex_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("slice_vertex_buffer"),
                contents: bytemuck::cast_slice(&SLICE_VERTICES),
                usage: wgpu::BufferUsages::VERTEX,
            },
        );

        let slice_index_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("slice_index_buffer"),
                contents: bytemuck::cast_slice(&SLICE_INDICES),
                usage: wgpu::BufferUsages::INDEX,
            },
        );

        // Wireframe da caixa do cubo: pipeline separado e bem mais simples
        // (sem textura, sem luz) — só a câmera (`@group(0)`), position+color
        // por vértice, desenhado como `LineList`. Depth sempre passa e nunca
        // escreve: é uma referência espacial que deve ficar visível por cima
        // de tudo, não geometria que oclui ou é ocluída pelas fatias.
        let wireframe_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wireframe_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("wireframe.wgsl").into()),
        });

        let wireframe_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wireframe_pipeline_layout"),
                bind_group_layouts: &[Some(&scene_bind_group_layout)],
                immediate_size: 0,
            });

        let wireframe_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                // Depth normal (Less) e sem escrever: o wireframe respeita o
                // que já foi desenhado (não aparece "por trás" de uma fatia
                // opaca — antes usava `Always`, o que fazia a caixa vazar
                // através das fatias e parecer transparência), mas também
                // não bloqueia nada que seja desenhado depois dele.
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let wireframe_vertex_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("wireframe_vertex_buffer"),
                contents: bytemuck::cast_slice(&WIREFRAME_VERTICES),
                usage: wgpu::BufferUsages::VERTEX,
            },
        );

        let wireframe_index_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("wireframe_index_buffer"),
                contents: bytemuck::cast_slice(&WIREFRAME_INDICES),
                usage: wgpu::BufferUsages::INDEX,
            },
        );

        // Texto GPU nativo (labels de eixo, e futuramente nomes de poço):
        // fonte bitmap embutida (`font.rs`), sem depender de nada do lado
        // Python pra existir — o Nebula é dono do próprio texto.
        let text_params_bind_group_layout =
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
            });

        let font_atlas_bind_group_layout =
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
            });

        let font_atlas = FontAtlas::new(&device, &queue, &font_atlas_bind_group_layout);

        let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("text.wgsl").into()),
        });

        let text_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text_pipeline_layout"),
            bind_group_layouts: &[
                Some(&scene_bind_group_layout),
                Some(&text_params_bind_group_layout),
                Some(&font_atlas_bind_group_layout),
            ],
            immediate_size: 0,
        });

        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
            // Texto é anotação tipo HUD (igual o Petrel deixa os labels de
            // eixo sempre legíveis) — sempre visível por cima de tudo, não
            // ocluído pela geometria 3D. Sem escrever profundidade, pra não
            // atrapalhar nada desenhado depois.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Capacidade fixa, reescrita (não recriada) a cada frame via
        // `queue.write_buffer` — bem mais barato que alocar um buffer novo
        // por frame só porque o usuário orbitou a câmera.
        let axis_tick_lines_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("axis_tick_lines_buffer"),
            size: (AXIS_TICK_LINE_CAPACITY * std::mem::size_of::<LineVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            depth_view,
            camera,
            scene_buffer,
            scene_bind_group,
            slice_pipeline,
            slice_vertex_buffer,
            slice_index_buffer,
            num_slice_indices: SLICE_INDICES.len() as u32,
            wireframe_pipeline,
            wireframe_vertex_buffer,
            wireframe_index_buffer,
            num_wireframe_indices: WIREFRAME_INDICES.len() as u32,
            text_pipeline,
            text_params_bind_group_layout,
            font_atlas,
            axis_tick_lines_buffer,
            num_axis_tick_line_vertices: 0,
            axis_grid: None,
            volume_bind_group_layout,
            colormap_bind_group_layout,
            slice_params_bind_group_layout,
            volumes: HashMap::new(),
            slices: HashMap::new(),
            text_labels: HashMap::new(),
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth_view(&self.device, &self.config);
        self.camera.set_aspect(width as f32 / height as f32);
    }

    /// Botão esquerdo do mouse arrastando: gira a câmera em torno do alvo.
    fn orbit(&mut self, dx: f32, dy: f32) {
        self.camera.orbit(dx, dy);
    }

    /// Botão do meio arrastando: translada o alvo (pan) no plano da tela.
    fn pan(&mut self, dx: f32, dy: f32) {
        self.camera.pan(dx, dy);
    }

    /// Botão direito arrastando (ou scroll): aproxima/afasta a câmera.
    fn zoom(&mut self, delta: f32) {
        self.camera.zoom(delta);
    }

    /// Envia um volume escalar (ex: amplitude sísmica, ou classes de fácies)
    /// pra GPU como textura 3D, sob o id escolhido pelo lado Python (o mesmo
    /// id que o Andromeda já usa pro dataset). `data` precisa suportar o
    /// protocolo de buffer do Python (ex: um array numpy `float32`
    /// C-contíguo) com exatamente `width * height * depth` elementos, na
    /// ordem (inline, xline, amostra). Recém-criado, o volume usa um
    /// colormap cinza neutro até `set_volume_colormap` ser chamado — assim
    /// nunca existe um estado "volume sem colormap" pra tratar à parte.
    fn add_volume(
        &mut self,
        id: u64,
        width: u32,
        height: u32,
        depth: u32,
        data: PyBuffer<f32>,
    ) -> PyResult<()> {
        let expected = (width as usize) * (height as usize) * (depth as usize);
        if data.item_count() != expected {
            return Err(PyRuntimeError::new_err(format!(
                "esperava {expected} elementos ({width}x{height}x{depth}), recebi {}",
                data.item_count()
            )));
        }

        let values = Python::attach(|py| data.to_vec(py))
            .map_err(|e| PyRuntimeError::new_err(format!("falha ao ler o buffer: {e}")))?;

        let volume = Volume3D::upload(
            &self.device,
            &self.queue,
            &self.volume_bind_group_layout,
            (width, height, depth),
            VOLUME_FILTERABLE,
            &values,
        );

        let clim = (0.0_f32, 1.0_f32);
        let colormap = Colormap::upload(
            &self.device,
            &self.queue,
            &self.colormap_bind_group_layout,
            &[0, 0, 0, 255, 255, 255, 255, 255],
            clim,
            false,
        );

        self.volumes.insert(id, VolumeEntry { volume, colormap, clim });
        Ok(())
    }

    /// Remove um volume e qualquer fatia que ainda apontasse pra ele.
    fn remove_volume(&mut self, id: u64) {
        self.volumes.remove(&id);
        self.slices.retain(|_, slice| slice.volume_id != id);
    }

    /// Define a paleta de cores usada pra mapear o valor amostrado do volume
    /// `id` em cor. `rgba` precisa ser um array `uint8` C-contíguo de shape
    /// `(N, 4)` — a origem (matplotlib, ou Petrel/Paradigm convertidos pra
    /// VisPy no Andromeda) não importa pro Nebula: o lado Python já resolve
    /// isso e manda a paleta pronta, amostrada em N pontos. `discrete=true`
    /// pra fácies (classes categóricas, sem interpolar entre cores vizinhas);
    /// `discrete=false` pra sísmica/atributos contínuos.
    fn set_volume_colormap(&mut self, id: u64, rgba: PyBuffer<u8>, discrete: bool) -> PyResult<()> {
        if rgba.item_count() % 4 != 0 {
            return Err(PyRuntimeError::new_err(
                "rgba precisa ter um múltiplo de 4 elementos (N cores RGBA)",
            ));
        }
        let entry = self
            .volumes
            .get_mut(&id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("volume {id} não encontrado")))?;

        let bytes = Python::attach(|py| rgba.to_vec(py))
            .map_err(|e| PyRuntimeError::new_err(format!("falha ao ler o buffer: {e}")))?;

        entry.colormap = Colormap::upload(
            &self.device,
            &self.queue,
            &self.colormap_bind_group_layout,
            &bytes,
            entry.clim,
            discrete,
        );

        Ok(())
    }

    /// Ajusta a faixa de valores (`clim`) do volume `id`, mapeada pros
    /// extremos do colormap — equivalente ao `clim=(min, max)` do Andromeda.
    /// Não recria a textura do colormap, só reescreve o uniform.
    fn set_volume_clim(&mut self, id: u64, min: f32, max: f32) -> PyResult<()> {
        let entry = self
            .volumes
            .get_mut(&id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("volume {id} não encontrado")))?;
        entry.clim = (min, max);
        entry.colormap.set_clim(&self.queue, entry.clim);
        Ok(())
    }

    /// Opacidade do volume `id` (0..1, default 1.0 = totalmente opaco) —
    /// equivalente ao slider de opacidade que o usuário já tem no Andromeda.
    fn set_volume_opacity(&mut self, id: u64, opacity: f32) -> PyResult<()> {
        let entry = self
            .volumes
            .get_mut(&id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("volume {id} não encontrado")))?;
        entry.colormap.set_opacity(&self.queue, opacity);
        Ok(())
    }

    /// Adiciona uma fatia (equivalente ao `AxisAlignedImage` do VisPy) do
    /// volume `volume_id`, sob o id `slice_id` escolhido pelo lado Python.
    /// `axis`: 0 = Inline, 1 = Crossline, 2 = Time (mesma convenção do
    /// `AXIS_CONFIG` do diálogo 2D do Andromeda). `index` é a posição
    /// normalizada (0..1) ao longo desse eixo. Vários slices podem apontar
    /// pro mesmo volume ao mesmo tempo (ex: uma Inline e uma Crossline
    /// visíveis juntas).
    fn add_slice(&mut self, slice_id: u64, volume_id: u64, axis: u32, index: f32) -> PyResult<()> {
        if !self.volumes.contains_key(&volume_id) {
            return Err(PyRuntimeError::new_err(format!(
                "volume {volume_id} não encontrado"
            )));
        }

        let model = slice_model_matrix(axis, index);
        let params_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &self.device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("slice_params_buffer"),
                contents: bytemuck::cast_slice(&[SliceParamsUniform {
                    model: model.to_cols_array_2d(),
                    axis,
                    index,
                    _pad: [0.0; 2],
                }]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        );
        let params_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("slice_params_bind_group"),
            layout: &self.slice_params_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buffer.as_entire_binding(),
            }],
        });

        self.slices.insert(
            slice_id,
            SliceEntry { volume_id, axis, index, visible: true, params_buffer, params_bind_group },
        );
        Ok(())
    }

    fn remove_slice(&mut self, slice_id: u64) {
        self.slices.remove(&slice_id);
    }

    fn set_slice_visible(&mut self, slice_id: u64, visible: bool) -> PyResult<()> {
        let slice = self
            .slices
            .get_mut(&slice_id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("slice {slice_id} não encontrada")))?;
        slice.visible = visible;
        Ok(())
    }

    /// Muda o eixo (0=Inline, 1=Crossline, 2=Time) e/ou a posição (0..1) de
    /// uma fatia já existente — é o que o slider/combobox de eixo do
    /// Andromeda vai chamar a cada movimento, sem recriar nada na GPU além
    /// de reescrever um uniform pequeno.
    fn set_slice_axis_index(&mut self, slice_id: u64, axis: u32, index: f32) -> PyResult<()> {
        let slice = self
            .slices
            .get_mut(&slice_id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("slice {slice_id} não encontrada")))?;
        let index = index.clamp(0.0, 1.0);
        slice.axis = axis;
        slice.index = index;
        let model = slice_model_matrix(axis, index);
        self.queue.write_buffer(
            &slice.params_buffer,
            0,
            bytemuck::cast_slice(&[SliceParamsUniform {
                model: model.to_cols_array_2d(),
                axis,
                index,
                _pad: [0.0; 2],
            }]),
        );
        Ok(())
    }

    /// "Folheia" uma fatia arrastando o mouse na tela (`screen_dx`,
    /// `screen_dy` em pixels), movendo-a fisicamente ao longo do seu próprio
    /// eixo no grid 3D — não um heurístico genérico de "arrastar pra baixo
    /// sempre avança". Projeta a direção do eixo de movimento da fatia
    /// (Inline/Time/Crossline, em coordenadas de mundo) pra tela sob a
    /// câmera atual, e usa a componente do arrasto do mouse alinhada com essa
    /// direção — arrastar "ao longo" do eixo do grid avança a fatia,
    /// arrastar perpendicular a ele não faz quase nada. Devolve o novo
    /// `index` (0..1) pro lado Python manter slider/combobox sincronizados.
    fn nudge_slice(&mut self, slice_id: u64, screen_dx: f32, screen_dy: f32) -> PyResult<f32> {
        let (axis, index) = {
            let slice = self
                .slices
                .get(&slice_id)
                .ok_or_else(|| PyRuntimeError::new_err(format!("slice {slice_id} não encontrada")))?;
            (slice.axis, slice.index)
        };

        let move_axis = slice_move_axis(axis);
        let view_proj = self.camera.view_proj();
        let pos = move_axis * (index * 2.0 - 1.0);
        let eps = 0.05;
        let a = view_proj * (pos - move_axis * eps).extend(1.0);
        let b = view_proj * (pos + move_axis * eps).extend(1.0);
        let a_ndc = a.truncate() / a.w.abs().max(1e-4);
        let b_ndc = b.truncate() / b.w.abs().max(1e-4);
        // Tela: X igual à NDC, Y invertido (NDC +Y é pra cima, tela +Y é pra baixo).
        let raw_dir = Vec2::new(b_ndc.x - a_ndc.x, -(b_ndc.y - a_ndc.y));
        let screen_dir = if raw_dir.length_squared() > 1e-6 { raw_dir.normalize() } else { Vec2::X };

        let along = screen_dx * screen_dir.x + screen_dy * screen_dir.y;
        let new_index = (index + along * 0.003).clamp(0.0, 1.0);

        self.set_slice_axis_index(slice_id, axis, new_index)?;
        Ok(new_index)
    }

    /// Descobre qual fatia está embaixo do cursor (coordenadas de tela em
    /// pixels, origem no canto superior esquerdo, mesmo sistema dos eventos
    /// de mouse do Qt) — permite Ctrl+arrastar direto em cima de qualquer
    /// fatia da cena, sem precisar selecioná-la antes numa combobox.
    ///
    /// Faz o de sempre: desprojeta o pixel num raio em espaço de mundo
    /// (usando a inversa de `view_proj`) e testa interseção contra o plano
    /// de cada fatia visível, usando a própria `slice_model_matrix` — o
    /// plano da fatia é sempre o local Z=0 transformado por esse `model`.
    /// Entre os planos atingidos dentro dos limites do quad (-1..1 local),
    /// devolve o mais próximo da câmera.
    fn pick_slice(&self, screen_x: f32, screen_y: f32) -> Option<u64> {
        let width = self.config.width.max(1) as f32;
        let height = self.config.height.max(1) as f32;
        let ndc_x = (screen_x / width) * 2.0 - 1.0;
        let ndc_y = 1.0 - (screen_y / height) * 2.0;

        let inv_view_proj = self.camera.view_proj().inverse();
        let near = inv_view_proj * Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
        let far = inv_view_proj * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
        let ray_origin = near.truncate() / near.w;
        let ray_end = far.truncate() / far.w;
        let ray_dir = (ray_end - ray_origin).normalize();

        let mut best: Option<(u64, f32)> = None;
        for (&id, slice) in self.slices.iter() {
            if !slice.visible {
                continue;
            }
            let model = slice_model_matrix(slice.axis, slice.index);
            let normal = (model * Vec4::new(0.0, 0.0, 1.0, 0.0)).truncate().normalize();
            let point_on_plane = (model * Vec4::new(0.0, 0.0, 0.0, 1.0)).truncate();

            let denom = normal.dot(ray_dir);
            if denom.abs() < 1e-6 {
                continue;
            }
            let t = (point_on_plane - ray_origin).dot(normal) / denom;
            if t < 0.0 {
                continue;
            }

            let hit_world = ray_origin + ray_dir * t;
            let local = model.inverse() * Vec4::new(hit_world.x, hit_world.y, hit_world.z, 1.0);
            if local.x.abs() <= 1.0 && local.y.abs() <= 1.0 {
                let closer = match best {
                    Some((_, best_t)) => t < best_t,
                    None => true,
                };
                if closer {
                    best = Some((id, t));
                }
            }
        }

        best.map(|(id, _)| id)
    }

    /// Projeta um ponto do mundo (`x,y,z`, mesma convenção do cubo -1..1 do
    /// wireframe) pra coordenada de tela em pixels (origem no canto superior
    /// esquerdo, igual `QPoint`) sob a câmera atual. Usado pro lado Python
    /// posicionar overlays 2D (labels de eixo, e mais tarde o nome/rótulo da
    /// cabeça de poço na Fase 5) sem o Nebula precisar saber renderizar
    /// texto. Devolve `None` se o ponto está atrás da câmera.
    fn project_to_screen(&self, x: f32, y: f32, z: f32) -> Option<(f32, f32)> {
        let clip = self.camera.view_proj() * Vec4::new(x, y, z, 1.0);
        if clip.w <= 1e-4 {
            return None;
        }
        let ndc_x = clip.x / clip.w;
        let ndc_y = clip.y / clip.w;
        let width = self.config.width as f32;
        let height = self.config.height as f32;
        let screen_x = (ndc_x * 0.5 + 0.5) * width;
        let screen_y = (1.0 - (ndc_y * 0.5 + 0.5)) * height;
        Some((screen_x, screen_y))
    }

    /// Adiciona um label de texto GPU nativo (billboard, sempre de frente
    /// pra câmera) na posição `x,y,z` do mundo. `text` pode ter `\n` pra
    /// várias linhas. `color` é `(r,g,b)` 0..1. `scale` controla o tamanho
    /// (mesma unidade de mundo do cubo -1..1). Fonte bitmap embutida — não
    /// precisa de nenhum setup prévio do lado Python.
    #[allow(clippy::too_many_arguments)]
    fn add_text_label(
        &mut self,
        id: u64,
        x: f32,
        y: f32,
        z: f32,
        text: String,
        r: f32,
        g: f32,
        b: f32,
        scale: f32,
    ) {
        self.insert_text_label(id, Vec3::new(x, y, z), &text, (r, g, b), scale);
    }

    fn remove_text_label(&mut self, id: u64) {
        self.text_labels.remove(&id);
    }

    fn set_text_label_visible(&mut self, id: u64, visible: bool) -> PyResult<()> {
        let label = self
            .text_labels
            .get_mut(&id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("label {id} não encontrado")))?;
        label.visible = visible;
        Ok(())
    }

    /// Move um label já existente sem retesselar o texto (só reescreve o
    /// uniform de posição/escala).
    fn set_text_label_position(&mut self, id: u64, x: f32, y: f32, z: f32) -> PyResult<()> {
        let label = self
            .text_labels
            .get(&id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("label {id} não encontrado")))?;
        // A escala já gravada no uniform precisa ser preservada — não temos
        // ela guardada à parte, então relemos não é possível sem um readback
        // da GPU; como só x/y/z mudam aqui, reescrevemos só os 3 primeiros
        // floats do uniform (offset 0), deixando `scale` (offset 12) intacto.
        self.queue.write_buffer(&label.params_buffer, 0, bytemuck::cast_slice(&[x, y, z]));
        Ok(())
    }

    /// Configura o grid numerado dos 3 eixos (Inline/Crossline/Time) do cubo
    /// sísmico: nomes + valores reais de tick (não normalizados -1..1),
    /// sempre posicionados na aresta do cubo que está "de costas" pra câmera
    /// no momento — recalculado a cada `render()`, então acompanha o orbit
    /// sozinho, igual um eixo 3D de matplotlib ou o gizmo do Petrel/Ocean.
    /// `width`/`height`/`depth` são as dimensões do volume (mesma convenção
    /// de `add_volume`: Inline, Crossline, amostra/Time). Chamar de novo
    /// substitui o grid anterior (útil se o volume mudar de tamanho).
    fn configure_axis_grid(&mut self, width: u32, height: u32, depth: u32) {
        if let Some(old) = self.axis_grid.take() {
            for &(id, _, _) in &old.tick_ids {
                self.text_labels.remove(&id);
            }
            for &(id, _) in &old.caption_ids {
                self.text_labels.remove(&id);
            }
        }

        const AXIS_NAMES: [&str; 3] = ["INLINE", "CROSSLINE", "TIME"];
        let dims = [width.max(1), height.max(1), depth.max(1)];

        let mut tick_ids = Vec::new();
        let mut caption_ids = Vec::new();

        for axis in 0..3u32 {
            let dim = dims[axis as usize];

            let caption_id = AXIS_GRID_ID_BASE + axis as u64 * 100 + 50;
            self.insert_text_label(
                caption_id,
                Vec3::ZERO,
                AXIS_NAMES[axis as usize],
                (0.55, 0.83, 1.0),
                AXIS_CAPTION_TEXT_SCALE,
            );
            caption_ids.push((caption_id, axis));

            for i in 0..AXIS_TICK_COUNT {
                let t = i as f32 / (AXIS_TICK_COUNT - 1) as f32;
                let raw_value = t * (dim - 1) as f32;
                // Time é invertido: topo do cubo (t=1, Y=+1) é a amostra 0
                // (mais rasa), fundo (t=0, Y=-1) é a última amostra.
                let value = if axis == 2 { (dim - 1) as f32 - raw_value } else { raw_value };
                let id = AXIS_GRID_ID_BASE + axis as u64 * 100 + i as u64;
                self.insert_text_label(
                    id,
                    Vec3::ZERO,
                    &(value.round() as i64).to_string(),
                    (0.75, 0.9, 1.0),
                    AXIS_TICK_TEXT_SCALE,
                );
                tick_ids.push((id, axis, t));
            }
        }

        self.axis_grid = Some(AxisGridConfig { tick_ids, caption_ids });
    }

    fn render(&mut self) -> PyResult<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(PyRuntimeError::new_err(
                    "erro de validação ao obter frame da surface",
                ));
            }
        };

        // Objeto não gira sozinho — só a câmera se move. A luz acompanha a
        // câmera ("headlight"), então o lado que você está olhando fica sempre
        // bem iluminado, do jeito que o Petrel faz.
        let eye = self.camera.eye();
        let (right, up) = self.camera.basis();

        let scene_uniform = SceneUniform {
            view_proj: self.camera.view_proj().to_cols_array_2d(),
            model: Mat4::IDENTITY.to_cols_array_2d(),
            light_position: [eye.x, eye.y, eye.z, 0.0],
            camera_position: [eye.x, eye.y, eye.z, 0.0],
            flags: [self.camera.lighting_enabled(), 0.0, 0.0, 0.0],
            camera_right: [right.x, right.y, right.z, 0.0],
            camera_up: [up.x, up.y, up.z, 0.0],
        };

        self.queue
            .write_buffer(&self.scene_buffer, 0, bytemuck::cast_slice(&[scene_uniform]));

        self.update_axis_grid();

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render_encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.2,
                            b: 0.3,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Enquanto nenhuma fatia foi adicionada (add_slice ainda não
            // chamado), só limpamos a tela.
            render_pass.set_pipeline(&self.slice_pipeline);
            render_pass.set_bind_group(0, &self.scene_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.slice_vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.slice_index_buffer.slice(..), wgpu::IndexFormat::Uint16);

            for slice in self.slices.values() {
                if !slice.visible {
                    continue;
                }
                let Some(volume) = self.volumes.get(&slice.volume_id) else {
                    continue;
                };
                render_pass.set_bind_group(1, &volume.volume.bind_group, &[]);
                render_pass.set_bind_group(2, &volume.colormap.bind_group, &[]);
                render_pass.set_bind_group(3, &slice.params_bind_group, &[]);
                render_pass.draw_indexed(0..self.num_slice_indices, 0, 0..1);
            }

            // Wireframe por cima de tudo (depth sempre passa) — é a
            // referência espacial do cubo, precisa ficar visível mesmo
            // atravessando as fatias.
            render_pass.set_pipeline(&self.wireframe_pipeline);
            render_pass.set_bind_group(0, &self.scene_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.wireframe_vertex_buffer.slice(..));
            render_pass
                .set_index_buffer(self.wireframe_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..self.num_wireframe_indices, 0, 0..1);

            // Traços curtos de tick do grid de eixo — mesmo pipeline/bind
            // group do wireframe (só posição+cor), buffer separado porque o
            // conteúdo é recalculado a cada frame em `update_axis_grid()`.
            if self.num_axis_tick_line_vertices > 0 {
                render_pass.set_vertex_buffer(0, self.axis_tick_lines_buffer.slice(..));
                render_pass.draw(0..self.num_axis_tick_line_vertices, 0..1);
            }

            // Labels de texto por último — igual o wireframe, sempre visíveis
            // por cima de tudo (anotação tipo HUD).
            if !self.text_labels.is_empty() {
                render_pass.set_pipeline(&self.text_pipeline);
                render_pass.set_bind_group(0, &self.scene_bind_group, &[]);
                render_pass.set_bind_group(2, &self.font_atlas.bind_group, &[]);
                for label in self.text_labels.values() {
                    if !label.visible || label.num_indices == 0 {
                        continue;
                    }
                    render_pass.set_bind_group(1, &label.params_bind_group, &[]);
                    render_pass.set_vertex_buffer(0, label.vertex_buffer.slice(..));
                    render_pass.set_index_buffer(label.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                    render_pass.draw_indexed(0..label.num_indices, 0, 0..1);
                }
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);

        Ok(())
    }
}

impl Renderer {
    /// Cria (ou substitui, se `id` já existir) um label de texto GPU nativo —
    /// mesma lógica usada tanto por `add_text_label` (label "solto", pedido
    /// pelo lado Python) quanto por `configure_axis_grid` (labels do grid de
    /// eixo, cuja posição inicial não importa porque `render()` recalcula a
    /// cada frame).
    fn insert_text_label(&mut self, id: u64, pos: Vec3, text: &str, color: (f32, f32, f32), scale: f32) {
        let (vertices, indices) = build_text_mesh(text);

        let vertex_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &self.device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("text_vertex_buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            },
        );
        let index_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &self.device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("text_index_buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            },
        );

        let params = TextParamsUniform {
            anchor_scale: [pos.x, pos.y, pos.z, scale],
            color: [color.0, color.1, color.2, 1.0],
        };
        let params_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &self.device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("text_params_buffer"),
                contents: bytemuck::cast_slice(&[params]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        );
        let params_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text_params_bind_group"),
            layout: &self.text_params_bind_group_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: params_buffer.as_entire_binding() }],
        });

        self.text_labels.insert(
            id,
            TextLabelEntry {
                vertex_buffer,
                index_buffer,
                num_indices: indices.len() as u32,
                params_buffer,
                params_bind_group,
                visible: true,
            },
        );
    }

    /// Recalcula, a cada frame, em qual aresta do cubo cada eixo do grid
    /// deve aparecer (a que está "de costas" pra câmera agora) e reescreve
    /// só as posições dos labels já existentes — nenhuma malha é
    /// retesselada, só uniforms pequenos. Também reconstrói o buffer dos
    /// traços curtos de tick (`axis_tick_lines_buffer`), desenhado pelo
    /// `wireframe_pipeline` logo depois da caixa do cubo.
    fn update_axis_grid(&mut self) {
        let Some(grid) = self.axis_grid.clone() else {
            self.num_axis_tick_line_vertices = 0;
            return;
        };

        let eye = self.camera.eye();
        let target = match &self.camera {
            CameraKind::Orbit(c) => c.target,
            CameraKind::PanZoom(c) => c.target,
        };
        let dir = eye - target;
        let dir = if dir.length_squared() > 1e-6 { dir.normalize() } else { Vec3::Z };
        let sgn = |v: f32| if v >= 0.0 { 1.0_f32 } else { -1.0_f32 };
        // Lado do cubo escolhido pra cada eixo é o oposto de onde a câmera
        // está — a aresta "de trás", que não atravessa o volume renderizado.
        let back = Vec3::new(-sgn(dir.x), -sgn(dir.y), -sgn(dir.z));

        const EDGE_OUT: f32 = 1.0;
        const TICK_OUT: f32 = 1.08;
        const LABEL_OUT: f32 = 1.2;
        const CAPTION_OUT: f32 = 1.4;
        const TICK_COLOR: [f32; 3] = [0.75, 0.9, 1.0];

        let mut tick_lines: Vec<LineVertex> = Vec::with_capacity(grid.tick_ids.len() * 2);
        for &(id, axis, t) in &grid.tick_ids {
            let local = -1.0 + 2.0 * t;
            let p_edge = axis_grid_point(axis, local, back, EDGE_OUT);
            let p_tick = axis_grid_point(axis, local, back, TICK_OUT);
            tick_lines.push(LineVertex { position: p_edge.to_array(), color: TICK_COLOR });
            tick_lines.push(LineVertex { position: p_tick.to_array(), color: TICK_COLOR });

            let p_label = axis_grid_point(axis, local, back, LABEL_OUT);
            if let Some(label) = self.text_labels.get(&id) {
                self.queue.write_buffer(
                    &label.params_buffer,
                    0,
                    bytemuck::cast_slice(&[p_label.x, p_label.y, p_label.z]),
                );
            }
        }

        for &(id, axis) in &grid.caption_ids {
            let p_caption = axis_grid_point(axis, 0.0, back, CAPTION_OUT);
            if let Some(label) = self.text_labels.get(&id) {
                self.queue.write_buffer(
                    &label.params_buffer,
                    0,
                    bytemuck::cast_slice(&[p_caption.x, p_caption.y, p_caption.z]),
                );
            }
        }

        self.queue
            .write_buffer(&self.axis_tick_lines_buffer, 0, bytemuck::cast_slice(&tick_lines));
        self.num_axis_tick_line_vertices = tick_lines.len() as u32;
    }
}

#[pymodule]
fn nebula(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Renderer>()?;
    Ok(())
}
