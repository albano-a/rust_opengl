//! Structs `#[repr(C)]` que espelham exatamente os uniforms lidos pelos
//! shaders WGSL (`volume_slice.wgsl`, `wireframe.wgsl`, `text.wgsl`) — layout
//! de bytes tem que bater byte a byte com o `struct` do lado WGSL.

use bytemuck::{Pod, Zeroable};

/// Tudo que os shaders precisam saber sobre a cena por frame: câmera (pra
/// projetar vértices) e luz (pra shading). Um bind group só em vez de dois,
/// já que ambos são "globals" recalculados a cada frame.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub(crate) struct SceneUniform {
    pub view_proj: [[f32; 4]; 4],
    pub model: [[f32; 4]; 4],
    // xyz usados; w é só padding pra alinhamento de 16 bytes em uniform buffers.
    pub light_position: [f32; 4],
    pub camera_position: [f32; 4],
    // x = 1.0 aplica iluminação (visão 3D orbital), 0.0 não aplica (visão 2D
    // pan/zoom — seção é dado cru, não superfície lit).
    pub flags: [f32; 4],
    // Eixos da câmera em coordenadas de mundo, usados pro billboard dos
    // labels de texto (cada caractere sempre de frente pra tela).
    pub camera_right: [f32; 4],
    pub camera_up: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub(crate) struct SliceParamsUniform {
    pub model: [[f32; 4]; 4],
    pub axis: u32,
    pub index: f32,
    pub _pad: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub(crate) struct TextParamsUniform {
    // xyz = posição no mundo do centro do label; w = escala.
    pub anchor_scale: [f32; 4],
    // rgb = cor; a não usado.
    pub color: [f32; 4],
}
