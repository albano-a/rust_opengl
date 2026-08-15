/// Uma textura 3D de volume sísmico (amplitude/atributo) já carregada na GPU,
/// pronta pra ser amostrada por um shader.
///
/// Fase 3 só prova o caminho de upload (Python -> textura 3D); ray marching de
/// verdade e amostragem de múltiplas fatias ficam pra Fase 4.
pub struct Volume3D {
    pub bind_group: wgpu::BindGroup,
}

impl Volume3D {
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_group_layout: &wgpu::BindGroupLayout,
        dims: (u32, u32, u32),
        filterable: bool,
        data: &[f32],
    ) -> Self {
        let (width, height, depth) = dims;
        assert_eq!(
            data.len(),
            (width * height * depth) as usize,
            "tamanho dos dados não bate com width*height*depth"
        );

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("volume_texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: depth,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R32Float,
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
            bytemuck::cast_slice(data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                // R32Float = 4 bytes por texel.
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: depth,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Nem todo adapter suporta sampling linear de texturas R32Float
        // (feature FLOAT32_FILTERABLE). Sem ela, caímos pra nearest — funcional,
        // só com blocos visíveis em vez de interpolação suave.
        let filter = if filterable {
            wgpu::FilterMode::Linear
        } else {
            wgpu::FilterMode::Nearest
        };

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("volume_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: filter,
            min_filter: filter,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("volume_bind_group"),
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
            ],
        });

        Self { bind_group }
    }
}
