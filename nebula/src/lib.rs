mod camera;
mod colormap;
mod geometry;
mod volume;

use std::collections::HashMap;
use std::num::NonZeroIsize;

use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle};

use camera::{CameraKind, OrbitCamera, PanZoomCamera};
use colormap::Colormap;
use geometry::{SliceVertex, SLICE_INDICES, SLICE_VERTICES};
use volume::Volume3D;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

// Amplitude sísmica amostrada não deveria ser interpolada silenciosamente
// entre vizinhos, e sampling linear de R32Float depende da feature
// FLOAT32_FILTERABLE (nem todo adapter tem) — nearest evita as duas questões
// de uma vez. Vale pra todo volume, não é uma escolha por instância.
const VOLUME_FILTERABLE: bool = false;

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
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SliceParamsUniform {
    axis: u32,
    index: f32,
    _pad: [f32; 2],
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

    volume_bind_group_layout: wgpu::BindGroupLayout,
    colormap_bind_group_layout: wgpu::BindGroupLayout,
    slice_params_bind_group_layout: wgpu::BindGroupLayout,

    volumes: HashMap<u64, VolumeEntry>,
    slices: HashMap<u64, SliceEntry>,
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
        let initial_scene_uniform = SceneUniform {
            view_proj: camera.view_proj().to_cols_array_2d(),
            model: Mat4::IDENTITY.to_cols_array_2d(),
            // Luz "headlight": acompanha a câmera em vez de ficar fixa no mundo,
            // pra o lado da superfície que você está olhando ficar sempre bem
            // iluminado, não importa de que ângulo — é assim que o Petrel faz.
            light_position: [initial_eye.x, initial_eye.y, initial_eye.z, 0.0],
            camera_position: [initial_eye.x, initial_eye.y, initial_eye.z, 0.0],
            flags: [camera.lighting_enabled(), 0.0, 0.0, 0.0],
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
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
                    blend: Some(wgpu::BlendState::REPLACE),
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
            volume_bind_group_layout,
            colormap_bind_group_layout,
            slice_params_bind_group_layout,
            volumes: HashMap::new(),
            slices: HashMap::new(),
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

        let params_buffer = wgpu::util::DeviceExt::create_buffer_init(
            &self.device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("slice_params_buffer"),
                contents: bytemuck::cast_slice(&[SliceParamsUniform { axis, index, _pad: [0.0; 2] }]),
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
        slice.axis = axis;
        slice.index = index;
        self.queue.write_buffer(
            &slice.params_buffer,
            0,
            bytemuck::cast_slice(&[SliceParamsUniform { axis, index, _pad: [0.0; 2] }]),
        );
        Ok(())
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

        let scene_uniform = SceneUniform {
            view_proj: self.camera.view_proj().to_cols_array_2d(),
            model: Mat4::IDENTITY.to_cols_array_2d(),
            light_position: [eye.x, eye.y, eye.z, 0.0],
            camera_position: [eye.x, eye.y, eye.z, 0.0],
            flags: [self.camera.lighting_enabled(), 0.0, 0.0, 0.0],
        };

        self.queue
            .write_buffer(&self.scene_buffer, 0, bytemuck::cast_slice(&[scene_uniform]));

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
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);

        Ok(())
    }
}

#[pymodule]
fn nebula(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Renderer>()?;
    Ok(())
}
