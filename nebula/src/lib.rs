mod camera;
mod colormap;
mod geometry;
mod volume;

use std::num::NonZeroIsize;

use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle};

use camera::OrbitCamera;
use colormap::Colormap;
use geometry::{SliceVertex, SLICE_INDICES, SLICE_VERTICES};
use volume::Volume3D;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

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

    camera: OrbitCamera,
    scene_buffer: wgpu::Buffer,
    scene_bind_group: wgpu::BindGroup,

    slice_pipeline: wgpu::RenderPipeline,
    slice_vertex_buffer: wgpu::Buffer,
    slice_index_buffer: wgpu::Buffer,
    num_slice_indices: u32,

    volume_bind_group_layout: wgpu::BindGroupLayout,
    volume_filterable: bool,
    volume: Option<Volume3D>,

    colormap_bind_group_layout: wgpu::BindGroupLayout,
    colormap: Colormap,
    clim: (f32, f32),
}

#[pymethods]
impl Renderer {
    #[new]
    fn new(hwnd: isize, width: u32, height: u32) -> PyResult<Self> {
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

        // Sampling da textura de volume é sempre "nearest": amplitude sísmica
        // amostrada não deveria ser interpolada silenciosamente entre vizinhos,
        // e sampling linear de R32Float depende da feature FLOAT32_FILTERABLE
        // (nem todo adapter tem) — nearest evita as duas questões de uma vez.
        let volume_filterable = false;

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

        let camera = OrbitCamera::new(config.width as f32 / config.height as f32);

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
                                filterable: volume_filterable,
                            },
                            view_dimension: wgpu::TextureViewDimension::D3,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(if volume_filterable {
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

        // Cinza simples até o Python chamar `set_colormap` com uma paleta de
        // verdade — evita o Renderer ficar num estado "sem colormap" que
        // precisaria de tratamento especial no render().
        let clim = (0.0_f32, 1.0_f32);
        let colormap = Colormap::upload(
            &device,
            &queue,
            &colormap_bind_group_layout,
            &[0, 0, 0, 255, 255, 255, 255, 255],
            clim,
        );

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
            volume_filterable,
            volume: None,
            colormap_bind_group_layout,
            colormap,
            clim,
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

    /// Envia um volume escalar (ex: amplitude sísmica) pra GPU como textura 3D.
    /// `data` precisa suportar o protocolo de buffer do Python (ex: um array
    /// numpy `float32` C-contíguo) com exatamente `width * height * depth`
    /// elementos, na ordem (inline, xline, amostra).
    fn load_volume(&mut self, width: u32, height: u32, depth: u32, data: PyBuffer<f32>) -> PyResult<()> {
        let expected = (width as usize) * (height as usize) * (depth as usize);
        if data.item_count() != expected {
            return Err(PyRuntimeError::new_err(format!(
                "esperava {expected} elementos ({width}x{height}x{depth}), recebi {}",
                data.item_count()
            )));
        }

        let values = Python::attach(|py| data.to_vec(py))
            .map_err(|e| PyRuntimeError::new_err(format!("falha ao ler o buffer: {e}")))?;

        self.volume = Some(Volume3D::upload(
            &self.device,
            &self.queue,
            &self.volume_bind_group_layout,
            (width, height, depth),
            self.volume_filterable,
            &values,
        ));

        Ok(())
    }

    /// Define a paleta de cores (colormap contínuo) usada pra mapear o valor
    /// amostrado do volume em cor. `rgba` precisa ser um array `uint8`
    /// C-contíguo de shape `(N, 4)` — a origem (matplotlib, ou Petrel/Paradigm
    /// convertidos pra VisPy no Andromeda) não importa pro Nebula: o lado
    /// Python já resolve isso e manda a paleta pronta, amostrada em N pontos.
    fn set_colormap(&mut self, rgba: PyBuffer<u8>) -> PyResult<()> {
        if rgba.item_count() % 4 != 0 {
            return Err(PyRuntimeError::new_err(
                "rgba precisa ter um múltiplo de 4 elementos (N cores RGBA)",
            ));
        }

        let bytes = Python::attach(|py| rgba.to_vec(py))
            .map_err(|e| PyRuntimeError::new_err(format!("falha ao ler o buffer: {e}")))?;

        self.colormap = Colormap::upload(
            &self.device,
            &self.queue,
            &self.colormap_bind_group_layout,
            &bytes,
            self.clim,
        );

        Ok(())
    }

    /// Ajusta a faixa de valores (`clim`) mapeada pros extremos do colormap —
    /// equivalente ao `clim=(min, max)` do Andromeda. Não recria a textura do
    /// colormap, só reescreve o uniform.
    fn set_clim(&mut self, min: f32, max: f32) {
        self.clim = (min, max);
        self.colormap.set_clim(&self.queue, self.clim);
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

            // Enquanto nenhum volume foi carregado (load_volume ainda não chamado),
            // só limpamos a tela — não há bind group de textura pra desenhar.
            if let Some(volume) = &self.volume {
                render_pass.set_pipeline(&self.slice_pipeline);
                render_pass.set_bind_group(0, &self.scene_bind_group, &[]);
                render_pass.set_bind_group(1, &volume.bind_group, &[]);
                render_pass.set_bind_group(2, &self.colormap.bind_group, &[]);
                render_pass.set_vertex_buffer(0, self.slice_vertex_buffer.slice(..));
                render_pass
                    .set_index_buffer(self.slice_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
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
