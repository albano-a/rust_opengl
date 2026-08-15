/// LUT de colormap contínuo (1D) + `clim` (faixa de valores mapeada pra 0..1
/// antes de indexar a tabela). Separado do bind group do volume de propósito:
/// trocar de colormap ou ajustar clim não deveria exigir recriar a textura 3D
/// do volume, e vice-versa — ciclos de vida independentes.
///
/// A origem da paleta (matplotlib, ou Petrel/Paradigm convertidos pra VisPy no
/// Andromeda) é problema do lado Python: o Nebula só recebe RGBA já amostrado
/// em N pontos fixos, igual o volume só recebe um array de voxels já pronto.
pub struct Colormap {
    pub bind_group: wgpu::BindGroup,
    clim_buffer: wgpu::Buffer,
}

impl Colormap {
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_group_layout: &wgpu::BindGroupLayout,
        rgba: &[u8],
        clim: (f32, f32),
    ) -> Self {
        assert_eq!(rgba.len() % 4, 0, "RGBA precisa ser múltiplo de 4 bytes");
        let resolution = (rgba.len() / 4) as u32;

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("colormap_texture"),
            size: wgpu::Extent3d {
                width: resolution,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D1,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(resolution * 4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: resolution,
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Rgba8Unorm é filtrável em qualquer hardware, sem a dor de cabeça do
        // FLOAT32_FILTERABLE que tivemos com a textura de volume (Fase 3) —
        // faz sentido usar Linear aqui: colormap contínuo deve interpolar.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("colormap_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let clim_buffer = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("clim_buffer"),
                contents: bytemuck::cast_slice(&[[clim.0, clim.1, 0.0, 0.0f32]]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        );

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("colormap_bind_group"),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: clim_buffer.as_entire_binding(),
                },
            ],
        });

        Self { bind_group, clim_buffer }
    }

    pub fn set_clim(&self, queue: &wgpu::Queue, clim: (f32, f32)) {
        queue.write_buffer(
            &self.clim_buffer,
            0,
            bytemuck::cast_slice(&[[clim.0, clim.1, 0.0, 0.0f32]]),
        );
    }
}
