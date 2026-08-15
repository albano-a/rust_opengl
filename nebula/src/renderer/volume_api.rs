//! `VolumeEntry` e os métodos Python do `Renderer` que lidam com volumes e
//! com a extensão da survey — `add_volume`/`remove_volume`,
//! `set_survey_extent`, `set_volume_placement`,
//! `set_volume_colormap`/`set_volume_clim`/`set_volume_opacity`.

use glam::Vec3;
use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::gpu::colormap::Colormap;
use crate::gpu::volume::Volume3D;
use crate::renderer::Renderer;
use super::gpu_setup::VOLUME_FILTERABLE;

/// Um volume carregado na GPU (textura 3D) junto com o colormap/clim usados
/// pra colori-lo. Um `Renderer` pode ter vários, cada um com seu próprio id
/// escolhido pelo lado Python (mesmo id que o Andromeda já usa pro dataset).
///
/// `origin`/`extent` posicionam esse volume DENTRO da survey (que tem seu
/// próprio tamanho fixo, ver `Renderer::cube_scale`/`set_survey_extent`) —
/// frações 0..1 do espaço normalizado da survey, não do volume. Default
/// (0,0,0)/(1,1,1) = volume cobre a survey inteira (caso comum: a sísmica
/// principal). Um volume menor (ex: inversão cobrindo só uma parte da área,
/// ou um low-frequency model com sampling mais grosso) usa origin/extent
/// menores que 1, igual o Andromeda já faz (`_positions_from_db` em
/// `vispy_3D_visualization_controller.py`, offset+scale relativos à survey).
pub(crate) struct VolumeEntry {
    pub(crate) volume: Volume3D,
    pub(crate) colormap: Colormap,
    pub(crate) clim: (f32, f32),
    pub(crate) origin: Vec3,
    pub(crate) extent: Vec3,
}

#[pymethods]
impl Renderer {
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
                "expected {expected} elements ({width}x{height}x{depth}), received {}",
                data.item_count()
            )));
        }

        let values = Python::attach(|py| data.to_vec(py))
            .map_err(|e| PyRuntimeError::new_err(format!("failed to read buffer: {e}")))?;

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

        // Default: volume cobre a survey inteira (origin=0, extent=1) — é o
        // caso comum (a sísmica principal) e também o comportamento correto
        // se `set_volume_placement` nunca for chamado. Volumes menores (ex:
        // inversão cobrindo só parte da área) chamam `set_volume_placement`
        // depois pra se posicionar dentro da survey.
        self.volumes.insert(
            id,
            VolumeEntry {
                volume,
                colormap,
                clim,
                origin: Vec3::ZERO,
                extent: Vec3::ONE,
            },
        );

        Ok(())
    }

    /// Remove um volume e qualquer fatia que ainda apontasse pra ele.
    fn remove_volume(&mut self, id: u64) {
        self.volumes.remove(&id);
        self.slices.retain(|_, slice| slice.volume_id != id);
    }

    /// Define a extensão fixa da survey (em amostras: quantas inlines,
    /// crosslines e amostras de tempo/profundidade ela cobre) — chamado uma
    /// vez, na criação do projeto (mesmo momento em que o Andromeda grava a
    /// `Survey` no banco), não a cada volume importado. Controla o formato
    /// do wireframe/grid de eixo (`cube_scale`, o maior eixo vira a
    /// referência "1.0", os outros encolhem proporcionalmente) — volumes
    /// importados depois (sísmica, inversão, low-frequency model, fácies)
    /// não mudam mais esse formato; eles se posicionam DENTRO dele via
    /// `set_volume_placement`.
    fn set_survey_extent(&mut self, width: u32, height: u32, depth: u32) {
        let max_dim = width.max(height).max(depth).max(1) as f32;
        self.cube_scale = Vec3::new(
            width as f32 / max_dim,
            height as f32 / max_dim,
            depth as f32 / max_dim,
        );
    }

    /// Posiciona o volume `id` dentro do cubo -1..1 da survey — pra quando
    /// ele não cobre a survey inteira (ex: uma inversão que só cobre
    /// inlines 500-800 de uma survey 0-1250, ou um low-frequency model
    /// reamostrado mais grosso). `origin_*`/`extent_*` são frações 0..1 do
    /// espaço da survey (não do próprio volume): origin é onde o volume
    /// começa, extent é quanto dele ocupa. O lado Python calcula essas duas
    /// frações a partir dos campos que o Andromeda já tem no banco (min/max/
    /// sampling da survey vs. do volume — mesma conta de `_positions_from_db`
    /// em `vispy_3D_visualization_controller.py`, só que expressa como
    /// fração em vez de offset+escala em amostras). Sem chamar isso, o
    /// volume assume que cobre a survey inteira (origin=0, extent=1).
    #[allow(clippy::too_many_arguments)]
    fn set_volume_placement(
        &mut self,
        id: u64,
        origin_inline: f32,
        origin_crossline: f32,
        origin_time: f32,
        extent_inline: f32,
        extent_crossline: f32,
        extent_time: f32,
    ) -> PyResult<()> {
        {
            let entry = self
                .volumes
                .get_mut(&id)
                .ok_or_else(|| PyRuntimeError::new_err(format!("volume {id} not found")))?;
            entry.origin = Vec3::new(origin_inline, origin_crossline, origin_time);
            entry.extent = Vec3::new(extent_inline, extent_crossline, extent_time);
        }

        // Toda fatia já existente desse volume precisa recalcular sua
        // posição — o posicionamento do volume mudou debaixo dela.
        let affected: Vec<u64> = self
            .slices
            .iter()
            .filter(|(_, slice)| slice.volume_id == id)
            .map(|(&slice_id, _)| slice_id)
            .collect();
        for slice_id in affected {
            self.refresh_slice(slice_id);
        }

        Ok(())
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
                "rgba must have a multiple of 4 elements (N RGBA colors)",
            ));
        }
        let entry = self
            .volumes
            .get_mut(&id)
            .ok_or_else(|| PyRuntimeError::new_err(format!("volume {id} not found")))?;

        let bytes = Python::attach(|py| rgba.to_vec(py))
            .map_err(|e| PyRuntimeError::new_err(format!("failed to read buffer: {e}")))?;

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
            .ok_or_else(|| PyRuntimeError::new_err(format!("volume {id} not found")))?;
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
            .ok_or_else(|| PyRuntimeError::new_err(format!("volume {id} not found")))?;
        entry.colormap.set_opacity(&self.queue, opacity);
        Ok(())
    }
}
