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
    SliceVertex {
        position: [-1.0, -1.0, 0.0],
        uv: [0.0, 1.0],
        normal: N,
    },
    SliceVertex {
        position: [1.0, -1.0, 0.0],
        uv: [1.0, 1.0],
        normal: N,
    },
    SliceVertex {
        position: [1.0, 1.0, 0.0],
        uv: [1.0, 0.0],
        normal: N,
    },
    SliceVertex {
        position: [-1.0, 1.0, 0.0],
        uv: [0.0, 0.0],
        normal: N,
    },
];

pub const SLICE_INDICES: [u16; 6] = [0, 1, 2, 2, 3, 0];

/// Vértice sem textura pro wireframe da caixa do cubo sísmico — só posição e
/// cor sólida, sem luz.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct LineVertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
}

impl LineVertex {
    pub const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// Caixa unitária (-1..1 em cada eixo) delimitando o volume sísmico — mesma
// convenção espacial usada por `slice_model_matrix` no lib.rs (X=Inline,
// Y=Crossline, Z=Time, mundo Z-up). Cor ciano clara, parecida com o
// wireframe de referência do Petrel/Ocean.
const R: f32 = 255.0;
const G: f32 = 255.0;
const B: f32 = 255.0;
const WIREFRAME_COLOR: [f32; 3] = [R / 255.0, G / 255.0, B / 255.0];

pub const WIREFRAME_VERTICES: [LineVertex; 8] = [
    LineVertex {
        position: [-1.0, -1.0, -1.0],
        color: WIREFRAME_COLOR,
    },
    LineVertex {
        position: [1.0, -1.0, -1.0],
        color: WIREFRAME_COLOR,
    },
    LineVertex {
        position: [1.0, 1.0, -1.0],
        color: WIREFRAME_COLOR,
    },
    LineVertex {
        position: [-1.0, 1.0, -1.0],
        color: WIREFRAME_COLOR,
    },
    LineVertex {
        position: [-1.0, -1.0, 1.0],
        color: WIREFRAME_COLOR,
    },
    LineVertex {
        position: [1.0, -1.0, 1.0],
        color: WIREFRAME_COLOR,
    },
    LineVertex {
        position: [1.0, 1.0, 1.0],
        color: WIREFRAME_COLOR,
    },
    LineVertex {
        position: [-1.0, 1.0, 1.0],
        color: WIREFRAME_COLOR,
    },
];

// 12 arestas do cubo, como pares de índices pra `PrimitiveTopology::LineList`.
pub const WIREFRAME_INDICES: [u16; 24] = [
    0, 1, 1, 2, 2, 3, 3, 0, // face de trás
    4, 5, 5, 6, 6, 7, 7, 4, // face da frente
    0, 4, 1, 5, 2, 6, 3, 7, // arestas verticais
];
