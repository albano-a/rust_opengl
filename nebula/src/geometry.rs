use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct SliceVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
}

impl SliceVertex {
    pub const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<SliceVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// Quad no plano XY (Z=0), pra exibir uma fatia do volume 3D. UV mapeia
// diretamente pra (inline, xline) normalizados; a profundidade amostrada é
// fixa no shader (fatia do meio) por enquanto.
pub const SLICE_VERTICES: [SliceVertex; 4] = [
    SliceVertex { position: [-1.0, -1.0, 0.0], uv: [0.0, 1.0] },
    SliceVertex { position: [1.0, -1.0, 0.0], uv: [1.0, 1.0] },
    SliceVertex { position: [1.0, 1.0, 0.0], uv: [1.0, 0.0] },
    SliceVertex { position: [-1.0, 1.0, 0.0], uv: [0.0, 0.0] },
];

pub const SLICE_INDICES: [u16; 6] = [0, 1, 2, 2, 3, 0];
