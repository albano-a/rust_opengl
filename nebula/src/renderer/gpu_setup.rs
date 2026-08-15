//! Helpers de infraestrutura wgpu que não pertencem a nenhum domínio
//! específico (volume/fatia/texto) — formato de profundidade, texturas de
//! profundidade/MSAA, detecção de sample count suportado.

pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

// Amplitude sísmica amostrada não deveria ser interpolada silenciosamente
// entre vizinhos, e sampling linear de R32Float depende da feature
// FLOAT32_FILTERABLE (nem todo adapter tem) — nearest evita as duas questões
// de uma vez. Vale pra todo volume, não é uma escolha por instância.
pub(crate) const VOLUME_FILTERABLE: bool = false;

pub(crate) fn create_depth_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    sample_count: u32,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth_texture"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Maior sample count de MSAA que tanto o formato de cor da surface quanto o
/// formato de profundidade suportam ao mesmo tempo (os dois precisam bater
/// no mesmo render pass) — tentado em ordem decrescente a partir de 8x
/// (pedido explícito do usuário: "8x é melhor"), caindo pra 4x/2x/1x se o
/// adapter não suportar. `1` = sem MSAA (nunca falha: todo adapter suporta
/// sample count 1).
pub(crate) fn best_common_sample_count(
    adapter: &wgpu::Adapter,
    color_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
) -> u32 {
    let color_features = adapter.get_texture_format_features(color_format);
    let depth_features = adapter.get_texture_format_features(depth_format);
    for &count in &[8u32, 4, 2, 1] {
        if color_features.flags.sample_count_supported(count)
            && depth_features.flags.sample_count_supported(count)
        {
            return count;
        }
    }
    1
}

/// Textura de cor multisampled onde a cena é desenhada de verdade — resolvida
/// (`resolve_target`) na textura de 1 sample da swapchain no fim do render
/// pass. `None` quando `sample_count <= 1` (MSAA indisponível/desligado):
/// nesse caso a cena desenha direto na swapchain, sem esse passo extra.
pub(crate) fn create_msaa_color_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    sample_count: u32,
) -> Option<wgpu::TextureView> {
    if sample_count <= 1 {
        return None;
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("msaa_color_texture"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format: config.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    Some(texture.create_view(&wgpu::TextureViewDescriptor::default()))
}
