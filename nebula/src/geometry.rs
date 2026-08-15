use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct SliceVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub normal: [f32; 3],
}

impl SliceVertex {
    pub const ATTRIBS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x3];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<SliceVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// Quad no plano XY (Z=0) em espaço de objeto, pra exibir uma fatia do volume 3D.
// UV mapeia diretamente pra (inline, xline) normalizados; a profundidade
// amostrada é fixa no shader (fatia do meio) por enquanto. Normal constante
// (0,0,1) porque o quad é plano — superfícies curvas (Fase 5) vão precisar de
// normais por vértice de verdade.
const N: [f32; 3] = [0.0, 0.0, 1.0];

pub const SLICE_VERTICES: [SliceVertex; 4] = [
    SliceVertex { position: [-1.0, -1.0, 0.0], uv: [0.0, 1.0], normal: N },
    SliceVertex { position: [1.0, -1.0, 0.0], uv: [1.0, 1.0], normal: N },
    SliceVertex { position: [1.0, 1.0, 0.0], uv: [1.0, 0.0], normal: N },
    SliceVertex { position: [-1.0, 1.0, 0.0], uv: [0.0, 0.0], normal: N },
];

pub const SLICE_INDICES: [u16; 6] = [0, 1, 2, 2, 3, 0];
