//! `Renderer`: motor de renderização Nebula, embutido num HWND emprestado de
//! um host nativo (ex: uma `QWindow` do PyQt5 via `createWindowContainer`).
//!
//! O HWND precisa permanecer válido durante toda a vida deste objeto — quem
//! garante isso é o lado Python, mantendo a QWindow viva enquanto o
//! `Renderer` existir.
//!
//! Os métodos Python (`#[pymethods]`) do `Renderer` estão espalhados por
//! vários arquivos, agrupados por domínio (feature `multiple-pymethods` do
//! PyO3 permite isso): `new`/`resize`/`orbit`/`pan`/`zoom`/`render` ficam
//! aqui (ciclo de vida principal); volumes em `volume_api.rs`; fatias em
//! `slice_api.rs`; texto/grid de eixo em `text_api.rs`.

use std::collections::HashMap;
use std::num::NonZeroIsize;

use glam::{Mat4, Vec3};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};

use crate::camera::{CameraKind, OrbitCamera, PanZoomCamera};
use crate::geometry::{SLICE_INDICES, SLICE_VERTICES, WIREFRAME_INDICES, WIREFRAME_VERTICES};
use crate::gpu_setup::{
    DEPTH_FORMAT, best_common_sample_count, create_depth_view, create_msaa_color_view,
};
use crate::pipelines;
use crate::slice_api::SliceEntry;
use crate::text::FontAtlas;
use crate::text_api::AxisGridConfig;
use crate::text_api::TextLabelEntry;
use crate::uniforms::SceneUniform;
use crate::volume_api::VolumeEntry;

// Capacidade do buffer dinâmico dos traços curtos de tick (uma linha por
// tick, 2 vértices cada) — recalculado a cada frame conforme a câmera gira.
// `AXIS_TICK_COUNT` mora em `text_api.rs` (é config do grid de eixo), mas o
// buffer é alocado aqui em `new()` junto com o resto dos buffers da GPU.
pub(crate) const AXIS_TICK_LINE_CAPACITY: usize =
    (crate::text_api::AXIS_TICK_COUNT as usize) * 3 * 2;

#[pyclass]
pub(crate) struct Renderer {
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) config: wgpu::SurfaceConfiguration,
    pub(crate) depth_view: wgpu::TextureView,
    // Quantos samples de MSAA o adapter realmente suporta pros formatos de
    // cor/profundidade em uso (ver `best_common_sample_count`) — 1 = sem
    // MSAA. Todos os pipelines são criados com esse valor fixo (não dá pra
    // mudar depois sem recriar tudo, então é decidido uma vez em `new`).
    pub(crate) sample_count: u32,
    // `None` quando `sample_count == 1` — nesse caso a cena desenha direto
    // na textura da swapchain, sem essa textura intermediária.
    pub(crate) msaa_color_view: Option<wgpu::TextureView>,

    pub(crate) camera: CameraKind,
    pub(crate) scene_buffer: wgpu::Buffer,
    pub(crate) scene_bind_group: wgpu::BindGroup,
    // Escala não-uniforme aplicada a toda a cena (`SceneUniform::model`) pra
    // o cubo virar um paralelepípedo de verdade quando o volume não é
    // equidimensional (ex: 250 inlines x 350 crosslines x 100 amostras) —
    // sem isso, tudo era forçado num cubo -1..1 mesmo quando os eixos têm
    // extensões bem diferentes. Normalizada pelo maior eixo (o maior lado
    // fica em 1.0, os outros encolhem proporcionalmente), definida por
    // `set_survey_extent` (não mais por `add_volume` — survey e volume são
    // conceitos separados, ver `volume_api.rs`). Usada tanto no shader
    // quanto localmente em `pick_slice`/`nudge_slice`/`project_to_screen`,
    // que precisam do mesmo espaço que a GPU realmente desenha.
    pub(crate) cube_scale: Vec3,

    pub(crate) slice_pipeline: wgpu::RenderPipeline,
    pub(crate) slice_vertex_buffer: wgpu::Buffer,
    pub(crate) slice_index_buffer: wgpu::Buffer,
    pub(crate) num_slice_indices: u32,

    pub(crate) wireframe_pipeline: wgpu::RenderPipeline,
    pub(crate) wireframe_vertex_buffer: wgpu::Buffer,
    pub(crate) wireframe_index_buffer: wgpu::Buffer,
    pub(crate) num_wireframe_indices: u32,

    pub(crate) text_pipeline: wgpu::RenderPipeline,
    pub(crate) text_params_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) font_atlas: FontAtlas,

    // Traços curtos de tick do grid de eixo — desenhados com o mesmo
    // `wireframe_pipeline` (mesmo formato de vértice, mesmo bind group de
    // cena), só que num buffer à parte porque o conteúdo muda a cada frame
    // (a aresta escolhida depende de pra onde a câmera está olhando).
    pub(crate) axis_tick_lines_buffer: wgpu::Buffer,
    pub(crate) num_axis_tick_line_vertices: u32,
    pub(crate) axis_grid: Option<AxisGridConfig>,

    pub(crate) volume_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) colormap_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) slice_params_bind_group_layout: wgpu::BindGroupLayout,

    pub(crate) volumes: HashMap<u64, VolumeEntry>,
    pub(crate) slices: HashMap<u64, SliceEntry>,
    pub(crate) text_labels: HashMap<u64, TextLabelEntry>,
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

        // Sem essa feature, o wgpu só garante os sample counts do spec da
        // WebGPU ([1,4]) mesmo em adapters cujo hardware suporta mais (8x,
        // por exemplo) — `adapter.get_texture_format_features` (usado em
        // `best_common_sample_count`) já reflete a capacidade real do
        // hardware, mas só vira válido de verdade se essa feature for pedida
        // aqui. `& adapter.features()` pra nunca pedir algo que o adapter
        // não tem (adapter mais antigo/software cai de volta pro [1,4] do
        // spec, sem quebrar).
        let required_features =
            wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES & adapter.features();

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features,
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

        let sample_count = best_common_sample_count(&adapter, surface_format, DEPTH_FORMAT);
        let depth_view = create_depth_view(&device, &config, sample_count);
        let msaa_color_view = create_msaa_color_view(&device, &config, sample_count);

        let aspect = config.width as f32 / config.height as f32;
        let camera = match mode.as_str() {
            "panzoom" | "2d" => CameraKind::PanZoom(PanZoomCamera::new(aspect)),
            _ => CameraKind::Orbit(OrbitCamera::new(aspect)),
        };

        let scene_bind_group_layout = pipelines::create_scene_bind_group_layout(&device);

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

        let volume_bind_group_layout = pipelines::create_volume_bind_group_layout(&device);
        let colormap_bind_group_layout = pipelines::create_colormap_bind_group_layout(&device);
        let slice_params_bind_group_layout =
            pipelines::create_slice_params_bind_group_layout(&device);

        let slice_pipeline = pipelines::create_slice_pipeline(
            &device,
            &scene_bind_group_layout,
            &volume_bind_group_layout,
            &colormap_bind_group_layout,
            &slice_params_bind_group_layout,
            surface_format,
            sample_count,
        );

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

        let wireframe_pipeline = pipelines::create_wireframe_pipeline(
            &device,
            &scene_bind_group_layout,
            surface_format,
            sample_count,
        );

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
            pipelines::create_text_params_bind_group_layout(&device);
        let font_atlas_bind_group_layout = pipelines::create_font_atlas_bind_group_layout(&device);
        let font_atlas = FontAtlas::new(&device, &queue, &font_atlas_bind_group_layout);

        let text_pipeline = pipelines::create_text_pipeline(
            &device,
            &scene_bind_group_layout,
            &text_params_bind_group_layout,
            &font_atlas_bind_group_layout,
            surface_format,
            sample_count,
        );

        // Capacidade fixa, reescrita (não recriada) a cada frame via
        // `queue.write_buffer` — bem mais barato que alocar um buffer novo
        // por frame só porque o usuário orbitou a câmera.
        let axis_tick_lines_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("axis_tick_lines_buffer"),
            size: (AXIS_TICK_LINE_CAPACITY * std::mem::size_of::<crate::geometry::LineVertex>())
                as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            depth_view,
            sample_count,
            msaa_color_view,
            camera,
            scene_buffer,
            scene_bind_group,
            cube_scale: Vec3::ONE,
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
        self.depth_view = create_depth_view(&self.device, &self.config, self.sample_count);
        self.msaa_color_view =
            create_msaa_color_view(&self.device, &self.config, self.sample_count);
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
            model: Mat4::from_scale(self.cube_scale).to_cols_array_2d(),
            light_position: [eye.x, eye.y, eye.z, 0.0],
            camera_position: [eye.x, eye.y, eye.z, 0.0],
            flags: [self.camera.lighting_enabled(), 0.0, 0.0, 0.0],
            camera_right: [right.x, right.y, right.z, 0.0],
            camera_up: [up.x, up.y, up.z, 0.0],
        };

        self.queue.write_buffer(
            &self.scene_buffer,
            0,
            bytemuck::cast_slice(&[scene_uniform]),
        );

        self.update_axis_grid();

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render_encoder"),
            });

        // Com MSAA (`sample_count > 1`), a cena é desenhada na textura
        // multisampled (`msaa_color_view`) e resolvida (`resolve_target`) na
        // textura de 1 sample da swapchain no fim do pass — não precisa
        // `Store` o conteúdo multisampled em si, só o resultado resolvido.
        // Sem MSAA (adapter não suporta nem 2x), desenha direto na swapchain,
        // igual antes.
        let (color_view, resolve_target, color_store) = match &self.msaa_color_view {
            Some(msaa_view) => (msaa_view, Some(&view), wgpu::StoreOp::Discard),
            None => (&view, None, wgpu::StoreOp::Store),
        };

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: color_store,
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
            render_pass
                .set_index_buffer(self.slice_index_buffer.slice(..), wgpu::IndexFormat::Uint16);

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

            // Wireframe por cima das fatias na ordem de desenho, mas
            // respeitando profundidade (ver `create_wireframe_pipeline`) —
            // é a referência espacial do cubo.
            render_pass.set_pipeline(&self.wireframe_pipeline);
            render_pass.set_bind_group(0, &self.scene_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.wireframe_vertex_buffer.slice(..));
            render_pass.set_index_buffer(
                self.wireframe_index_buffer.slice(..),
                wgpu::IndexFormat::Uint16,
            );
            render_pass.draw_indexed(0..self.num_wireframe_indices, 0, 0..1);

            // Traços curtos de tick do grid de eixo — mesmo pipeline/bind
            // group do wireframe (só posição+cor), buffer separado porque o
            // conteúdo é recalculado a cada frame em `update_axis_grid()`.
            if self.num_axis_tick_line_vertices > 0 {
                render_pass.set_vertex_buffer(0, self.axis_tick_lines_buffer.slice(..));
                render_pass.draw(0..self.num_axis_tick_line_vertices, 0..1);
            }

            // Labels de texto por último.
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
                    render_pass
                        .set_index_buffer(label.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                    render_pass.draw_indexed(0..label.num_indices, 0, 0..1);
                }
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);

        Ok(())
    }
}
