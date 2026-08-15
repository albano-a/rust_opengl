//! `SliceEntry` e os métodos Python do `Renderer` que lidam com fatias
//! (`AxisAlignedImage`) — `add_slice`/`remove_slice`/`set_slice_visible`/
//! `set_slice_axis_index`, `nudge_slice`, `pick_slice`, `project_to_screen`.

use glam::{Mat4, Vec2, Vec3, Vec4};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::renderer::Renderer;
use super::spatial::{slice_model_matrix, slice_move_axis, volume_placement_matrix};
use super::uniforms::SliceParamsUniform;

/// Uma fatia (AxisAlignedImage) de um volume específico: qual eixo (Inline/
/// Crossline/Time) e em que posição normalizada (0..1, relativa ao próprio
/// volume, não à survey). Vários slices podem apontar pro mesmo volume (ex:
/// uma seção Inline e uma Crossline do mesmo dataset, visíveis ao mesmo
/// tempo).
///
/// `model` é o resultado já composto de `volume_placement_matrix(origin,
/// extent) * slice_model_matrix(axis, index)` — recalculado sempre que o
/// eixo/posição da fatia OU o posicionamento do volume mudam (ver
/// `refresh_slice`), cacheado aqui pra `pick_slice` reusar sem duplicar a
/// lógica de composição.
pub(crate) struct SliceEntry {
    pub(crate) volume_id: u64,
    pub(crate) axis: u32,
    pub(crate) index: f32,
    pub(crate) visible: bool,
    pub(crate) model: Mat4,
    pub(crate) params_buffer: wgpu::Buffer,
    pub(crate) params_bind_group: wgpu::BindGroup,
}

#[pymethods]
impl Renderer {
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
                "volume {volume_id} not found"
            )));
        }

        let model = self.compute_slice_model(volume_id, axis, index);
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
            SliceEntry {
                volume_id,
                axis,
                index,
                visible: true,
                model,
                params_buffer,
                params_bind_group,
            },
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
            .ok_or_else(|| PyRuntimeError::new_err(format!("slice {slice_id} not found")))?;
        slice.visible = visible;
        Ok(())
    }

    /// Muda o eixo (0=Inline, 1=Crossline, 2=Time) e/ou a posição (0..1) de
    /// uma fatia já existente — é o que o slider/combobox de eixo do
    /// Andromeda vai chamar a cada movimento, sem recriar nada na GPU além
    /// de reescrever um uniform pequeno.
    fn set_slice_axis_index(&mut self, slice_id: u64, axis: u32, index: f32) -> PyResult<()> {
        if !self.slices.contains_key(&slice_id) {
            return Err(PyRuntimeError::new_err(format!(
                "slice {slice_id} not found"
            )));
        }
        let index = index.clamp(0.0, 1.0);
        {
            let slice = self.slices.get_mut(&slice_id).unwrap();
            slice.axis = axis;
            slice.index = index;
        }
        self.refresh_slice(slice_id);
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
        let (volume_id, axis, index, model) = {
            let slice = self.slices.get(&slice_id).ok_or_else(|| {
                PyRuntimeError::new_err(format!("slice {slice_id} not found"))
            })?;
            (slice.volume_id, slice.axis, slice.index, slice.model)
        };

        // `move_axis` precisa refletir quanto de espaço de mundo de verdade
        // uma mudança de `index` realmente percorre: `cube_scale` (formato
        // da survey) MULTIPLICADO pela `extent` do volume nesse eixo — um
        // volume que só cobre 30% da survey num eixo (`set_volume_placement`)
        // anda só 30% da distância por unidade de índice, comparado a um
        // volume que cobre a survey inteira. `anchor` é a posição real da
        // fatia agora (não presume origin=0 — a fatia pode estar no meio da
        // survey se o volume dela começa lá).
        let extent = self
            .volumes
            .get(&volume_id)
            .map(|v| v.extent)
            .unwrap_or(Vec3::ONE);
        let axis_extent = match axis {
            0 => extent.x,
            1 => extent.y,
            _ => extent.z,
        };
        let move_axis = slice_move_axis(axis) * self.cube_scale * axis_extent;
        let cube_model = Mat4::from_scale(self.cube_scale) * model;
        let anchor = (cube_model * Vec4::new(0.0, 0.0, 0.0, 1.0)).truncate();
        let view_proj = self.camera.view_proj();
        let eps = 0.05;
        let a = view_proj * (anchor - move_axis * eps).extend(1.0);
        let b = view_proj * (anchor + move_axis * eps).extend(1.0);
        let a_ndc = a.truncate() / a.w.abs().max(1e-4);
        let b_ndc = b.truncate() / b.w.abs().max(1e-4);
        // Tela: X igual à NDC, Y invertido (NDC +Y é pra cima, tela +Y é pra baixo).
        let raw_dir = Vec2::new(b_ndc.x - a_ndc.x, -(b_ndc.y - a_ndc.y));
        let screen_dir = if raw_dir.length_squared() > 1e-6 {
            raw_dir.normalize()
        } else {
            Vec2::X
        };

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
    /// de cada fatia visível, usando o `model` já cacheado nela. Entre os
    /// planos atingidos dentro dos limites do quad (-1..1 local), devolve o
    /// mais próximo da câmera.
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
            // `slice.model` já inclui o posicionamento do volume dentro da
            // survey (`compute_slice_model`/`refresh_slice`) — só falta
            // `cube_scale`, igualzinho ao shader (`scene.model * slice.model`,
            // ver volume_slice.wgsl), senão o plano testado não bate com o
            // que está desenhado de verdade.
            let model = Mat4::from_scale(self.cube_scale) * slice.model;
            let normal = (model * Vec4::new(0.0, 0.0, 1.0, 0.0))
                .truncate()
                .normalize();
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
        let world = Vec3::new(x, y, z) * self.cube_scale;
        let clip = self.camera.view_proj() * Vec4::new(world.x, world.y, world.z, 1.0);
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
}

impl Renderer {
    /// Matriz completa de uma fatia dentro do cubo -1..1 da SURVEY: primeiro
    /// posiciona/orienta a fatia dentro do espaço local do seu próprio
    /// volume (`slice_model_matrix`), depois encolhe/translada esse espaço
    /// local pro pedaço certo do espaço da survey
    /// (`volume_placement_matrix`, usando `origin`/`extent` do volume — ver
    /// `set_volume_placement`). Volume não encontrado (não deveria
    /// acontecer, `add_slice` já valida) cai no caso "cobre a survey
    /// inteira" em vez de dar panic.
    pub(crate) fn compute_slice_model(&self, volume_id: u64, axis: u32, index: f32) -> Mat4 {
        let (origin, extent) = self
            .volumes
            .get(&volume_id)
            .map(|v| (v.origin, v.extent))
            .unwrap_or((Vec3::ZERO, Vec3::ONE));
        volume_placement_matrix(origin, extent) * slice_model_matrix(axis, index)
    }

    /// Recalcula `model` de uma fatia (a partir do seu `axis`/`index`/
    /// `volume_id` atuais) e reescreve só o uniform na GPU — chamado depois
    /// de mudar o eixo/posição da própria fatia (`set_slice_axis_index`) ou
    /// depois de reposicionar o volume dela dentro da survey
    /// (`set_volume_placement`).
    pub(crate) fn refresh_slice(&mut self, slice_id: u64) {
        let Some((volume_id, axis, index)) = self
            .slices
            .get(&slice_id)
            .map(|s| (s.volume_id, s.axis, s.index))
        else {
            return;
        };
        let model = self.compute_slice_model(volume_id, axis, index);
        let slice = self.slices.get_mut(&slice_id).unwrap();
        slice.model = model;
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
    }
}
