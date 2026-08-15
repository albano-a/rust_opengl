use bytemuck::{Pod, Zeroable};

use crate::font;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct TextVertex {
    pub local_offset: [f32; 2],
    pub uv: [f32; 2],
}

impl TextVertex {
    pub const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TextVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// Layout do texto em "unidades de label" (escaladas pelo `scale` do label no
// mundo): cada caractere ocupa uma célula de CHAR_ADVANCE de largura;
// CHAR_W/CHAR_H é o tamanho do quad desenhado dentro dessa célula (menor que
// o avanço, pra sobrar um respiro natural entre letras).
const CHAR_ADVANCE: f32 = 0.6;
const CHAR_W: f32 = 0.5;
const CHAR_H: f32 = 0.8;
const LINE_HEIGHT: f32 = 1.0;

/// Gera a malha (vértices + índices) de uma string, centralizada horizontal
/// e verticalmente em torno da origem local — quem posiciona de verdade no
/// mundo é o `anchor` do label (billboard, ver `text.wgsl`). Suporta `\n`
/// pra texto em várias linhas (os labels de canto do wireframe usam isso:
/// "IL 0\nXL 0\nT 0").
pub fn build_text_mesh(text: &str) -> (Vec<TextVertex>, Vec<u16>) {
    let lines: Vec<&str> = text.split('\n').collect();
    let num_lines = lines.len() as f32;
    let max_len = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32;

    let total_width = max_len * CHAR_ADVANCE;
    let total_height = num_lines * LINE_HEIGHT;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for (line_idx, line) in lines.iter().enumerate() {
        let y_top = total_height / 2.0 - (line_idx as f32) * LINE_HEIGHT;
        for (col_idx, ch) in line.chars().enumerate() {
            let Some((u0, v0, u1, v1)) = font::glyph_uv(ch) else {
                continue;
            };
            if ch == ' ' {
                continue;
            }

            let x0 = (col_idx as f32) * CHAR_ADVANCE - total_width / 2.0;
            let x1 = x0 + CHAR_W;
            let y1 = y_top;
            let y0 = y_top - CHAR_H;

            let base = vertices.len() as u16;
            vertices.push(TextVertex { local_offset: [x0, y0], uv: [u0, v1] });
            vertices.push(TextVertex { local_offset: [x1, y0], uv: [u1, v1] });
            vertices.push(TextVertex { local_offset: [x1, y1], uv: [u1, v0] });
            vertices.push(TextVertex { local_offset: [x0, y1], uv: [u0, v0] });
            indices.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 3, base]);
        }
    }

    (vertices, indices)
}

/// Atlas da fonte bitmap — construído uma vez em `Renderer::new()`, nunca
/// muda depois. Todos os labels de texto compartilham este único bind group
/// (`@group(2)` do `text_pipeline`).
pub struct FontAtlas {
    pub bind_group: wgpu::BindGroup,
}

impl FontAtlas {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, bind_group_layout: &wgpu::BindGroupLayout) -> Self {
        let (raster, width, height) = font::build_atlas_raster();

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("font_atlas_texture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
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
            &raster,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Linear porque a fonte já nasce ampliada (`PIXEL_SCALE` em
        // font.rs) — suaviza a borda dos blocos em vez de deixar tudo
        // serrilhado quando o label é billboard num tamanho pequeno na tela.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("font_atlas_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("font_atlas_bind_group"),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        Self { bind_group }
    }
}
