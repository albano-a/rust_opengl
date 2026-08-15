//! `TextLabelEntry`/`AxisGridConfig` e os métodos Python do `Renderer` que
//! lidam com texto GPU nativo e com o grid numerado de eixo —
//! `add_text_label`/`remove_text_label`/`set_text_label_visible`/
//! `set_text_label_position`, `configure_axis_grid`.

use glam::Vec3;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::gpu::camera::CameraKind;
use crate::gpu::geometry::LineVertex;
use crate::gpu::text::build_text_mesh;
use crate::renderer::Renderer;
use super::spatial::axis_grid_point;
use super::uniforms::TextParamsUniform;

// Tamanho dos labels do grid de eixo (INLINE/CROSSLINE/TIME + valores dos
// ticks) — dois lugares únicos pra ajustar, em vez de mexer em cada chamada
// de `insert_text_label` espalhada pelo `configure_axis_grid`. Unidade é a
// mesma do cubo -1..1 (ver `TextParamsUniform`/`text.wgsl`).
const AXIS_TICK_TEXT_SCALE: f32 = 0.02;
const AXIS_CAPTION_TEXT_SCALE: f32 = 0.03;

const AXIS_INLINE_COLOR: (f32, f32, f32) = (220. / 255., 80. / 255., 80. / 255.); // #dc5050
const AXIS_XLINE_COLOR: (f32, f32, f32) = (80. / 255., 200. / 255., 120. / 255.); // #50c878
const AXIS_Z_COLOR: (f32, f32, f32) = (80. / 255., 140. / 255., 220. / 255.); // #508cdc

// Quantos ticks por eixo (incluindo os dois extremos).
pub(crate) const AXIS_TICK_COUNT: u32 = 10;

// Base dos ids internos dos labels do grid de eixo — bem longe de qualquer
// id que o lado Python normalmente escolhe (slices, volumes, labels
// próprios), pra nunca colidir.
const AXIS_GRID_ID_BASE: u64 = 9_000_000;

/// Um label de texto (billboard) já tesselado — a malha (`vertex_buffer`/
/// `index_buffer`) é construída uma vez a partir da string, na criação;
/// mudar posição/cor/escala só reescreve `params_buffer`.
pub(crate) struct TextLabelEntry {
    pub(crate) vertex_buffer: wgpu::Buffer,
    pub(crate) index_buffer: wgpu::Buffer,
    pub(crate) num_indices: u32,
    pub(crate) params_buffer: wgpu::Buffer,
    pub(crate) params_bind_group: wgpu::BindGroup,
    pub(crate) visible: bool,
}

/// Grid numerado dos 3 eixos do cubo sísmico (Inline/Crossline/Time), no
/// espírito de um eixo 3D de matplotlib/Petrel: o texto em si (`TextLabelEntry`,
/// já em `text_labels`) é criado uma única vez em `configure_axis_grid` — só
/// a *posição* de cada label é recalculada a cada frame em `render()`,
/// porque qual aresta do cubo é "a de trás" (a que não atrapalha a leitura
/// do volume) depende de pra onde a câmera está olhando agora.
#[derive(Clone)]
pub(crate) struct AxisGridConfig {
    /// (label_id, axis [0=Inline/X, 1=Crossline/Y, 2=Time/Z], t 0..1 ao
    /// longo da aresta).
    tick_ids: Vec<(u64, u32, f32)>,
    /// (label_id, axis) — nome do eixo, plantado no meio da aresta escolhida.
    caption_ids: Vec<(u64, u32)>,
}

#[pymethods]
impl Renderer {
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
            .ok_or_else(|| PyRuntimeError::new_err(format!("label {id} not found")))?;
        label.visible = visible;
        Ok(())
    }

    /// Move um label já existente sem retesselar o texto (só reescreve o
    /// uniform de posição/escala).
    fn set_text_label_position(&mut self, id: u64, x: f32, y: f32, z: f32) -> PyResult<()> {
        let label = self
            .text_labels
            .get(&id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("label {id} not found")))?;
        // A escala já gravada no uniform precisa ser preservada — não temos
        // ela guardada à parte, então relemos não é possível sem um readback
        // da GPU; como só x/y/z mudam aqui, reescrevemos só os 3 primeiros
        // floats do uniform (offset 0), deixando `scale` (offset 12) intacto.
        self.queue
            .write_buffer(&label.params_buffer, 0, bytemuck::cast_slice(&[x, y, z]));
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

        const AXIS_NAMES: [&str; 3] = ["IL", "XL", "ZL"];
        const AXIS_COLORS: [(f32, f32, f32); 3] = [
            AXIS_INLINE_COLOR, // #dc5050
            AXIS_XLINE_COLOR,  // #50c878
            AXIS_Z_COLOR,      // #508cdc
        ];
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
                AXIS_COLORS[axis as usize],
                AXIS_CAPTION_TEXT_SCALE,
            );
            caption_ids.push((caption_id, axis));

            for i in 0..AXIS_TICK_COUNT {
                let t = i as f32 / (AXIS_TICK_COUNT - 1) as f32;
                let raw_value = t * (dim - 1) as f32;
                // Time é invertido: topo do cubo (t=1, Y=+1) é a amostra 0
                // (mais rasa), fundo (t=0, Y=-1) é a última amostra.
                let value = if axis == 2 {
                    (dim - 1) as f32 - raw_value
                } else {
                    raw_value
                };
                let id = AXIS_GRID_ID_BASE + axis as u64 * 100 + i as u64;
                self.insert_text_label(
                    id,
                    Vec3::ZERO,
                    &(value.round() as i64).to_string(),
                    AXIS_COLORS[axis as usize],
                    AXIS_TICK_TEXT_SCALE,
                );
                tick_ids.push((id, axis, t));
            }
        }

        self.axis_grid = Some(AxisGridConfig {
            tick_ids,
            caption_ids,
        });
    }
}

impl Renderer {
    /// Cria (ou substitui, se `id` já existir) um label de texto GPU nativo —
    /// mesma lógica usada tanto por `add_text_label` (label "solto", pedido
    /// pelo lado Python) quanto por `configure_axis_grid` (labels do grid de
    /// eixo, cuja posição inicial não importa porque `render()` recalcula a
    /// cada frame).
    pub(crate) fn insert_text_label(
        &mut self,
        id: u64,
        pos: Vec3,
        text: &str,
        color: (f32, f32, f32),
        scale: f32,
    ) {
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
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buffer.as_entire_binding(),
            }],
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
    pub(crate) fn update_axis_grid(&mut self) {
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
        let dir = if dir.length_squared() > 1e-6 {
            dir.normalize()
        } else {
            Vec3::Y
        };
        let sgn = |v: f32| if v >= 0.0 { 1.0_f32 } else { -1.0_f32 };
        // Em X/Y, o lado do cubo escolhido é o MESMO lado de onde a câmera
        // está, não o oposto. Parece contra-intuitivo ("não devia ficar
        // escondido atrás?"), mas o texto respeita profundidade contra o que
        // já foi desenhado (`depth_compare: Less`, ver
        // `create_text_pipeline`), então a pergunta é "que canto do cubo
        // projeta pra fora da silhueta na tela". O canto mais PRÓXIMO da
        // câmera é sempre um vértice real da silhueta (é literalmente o
        // ponto do cubo mais perto do observador, não tem como ficar
        // "escondido atrás" da própria caixa) — o canto mais distante não
        // tem essa garantia: em ângulos elevados/de esguelha, a projeção em
        // perspectiva empurra o canto de trás pra dentro da silhueta.
        // Y: canto mais próximo da câmera, pelo motivo explicado acima —
        // só usado pela aresta de Inline (fixa Y,Z) e de Time (fixa X,Y).
        // Z (qual dos dois "anéis" horizontais — de cima ou de baixo —
        // hospeda os ticks de Inline/Crossline; Z é o eixo "pra cima" da
        // câmera, ver `OrbitCamera` em camera.rs) é o oposto de propósito:
        // olhando de cima pra baixo, os labels ficam embaixo (não competindo
        // com a face de cima, que é a que está de frente pra você); olhando
        // de baixo pra cima, ficam em cima — pedido explícito do usuário.
        // X é um caso à parte pro eixo inteiro (Crossline E Time, as duas
        // arestas que fixam X): sempre à esquerda da câmera, não no canto
        // geometricamente mais próximo — usa o vetor "direita" da própria
        // câmera (`camera.basis()`, o mesmo do billboard de texto) em vez da
        // direção câmera-alvo. Motivo de ser um X só pras duas: antes cada
        // uma escolhia seu próprio canto "mais próximo" de forma independente,
        // e podiam discordar — a aresta de Time (vertical, sobe/desce)
        // acabava numa aresta de trás qualquer, projetando pro meio da tela
        // em vez de ficar ao lado da caixa, mesmo com Crossline do lado
        // certo. Usando o mesmo X pras duas, a aresta de Time sempre sai do
        // mesmo canto onde a aresta de Crossline começa — sempre visível,
        // nunca no meio.
        let (camera_right, _camera_up) = self.camera.basis();
        let side_x = if camera_right.x.abs() > 1e-4 {
            -sgn(camera_right.x)
        } else {
            sgn(dir.x)
        };
        let near_side = Vec3::new(side_x, sgn(dir.y), -sgn(dir.z));
        let side_for_axis = |_axis: u32| near_side;

        // Deslocamentos pequenos — a caixa tem meia-largura 1.0, então
        // qualquer coisa muito acima disso já fica "flutuando" longe da
        // aresta em vez de colada nela.
        const EDGE_OUT: f32 = 1.0;
        const TICK_OUT: f32 = 1.01;
        const LABEL_OUT: f32 = 1.05;
        const CAPTION_OUT: f32 = 1.12;
        const TICK_COLOR: [f32; 3] = [1.0, 1.0, 1.0];

        let mut tick_lines: Vec<LineVertex> = Vec::with_capacity(grid.tick_ids.len() * 2);
        for &(id, axis, t) in &grid.tick_ids {
            let local = -1.0 + 2.0 * t;
            let side = side_for_axis(axis);
            let p_edge = axis_grid_point(axis, local, side, EDGE_OUT);
            let p_tick = axis_grid_point(axis, local, side, TICK_OUT);
            tick_lines.push(LineVertex {
                position: p_edge.to_array(),
                color: TICK_COLOR,
            });
            tick_lines.push(LineVertex {
                position: p_tick.to_array(),
                color: TICK_COLOR,
            });

            let p_label = axis_grid_point(axis, local, side, LABEL_OUT);
            if let Some(label) = self.text_labels.get(&id) {
                self.queue.write_buffer(
                    &label.params_buffer,
                    0,
                    bytemuck::cast_slice(&[p_label.x, p_label.y, p_label.z]),
                );
            }
        }

        for &(id, axis) in &grid.caption_ids {
            let p_caption = axis_grid_point(axis, 0.0, side_for_axis(axis), CAPTION_OUT);
            if let Some(label) = self.text_labels.get(&id) {
                self.queue.write_buffer(
                    &label.params_buffer,
                    0,
                    bytemuck::cast_slice(&[p_caption.x, p_caption.y, p_caption.z]),
                );
            }
        }

        self.queue.write_buffer(
            &self.axis_tick_lines_buffer,
            0,
            bytemuck::cast_slice(&tick_lines),
        );
        self.num_axis_tick_line_vertices = tick_lines.len() as u32;
    }
}
