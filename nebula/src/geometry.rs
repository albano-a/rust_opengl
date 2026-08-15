use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
}

impl Vertex {
    pub const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// Cubo unitário centrado na origem, um vértice por canto (cor identifica o canto,
// só pra deixar a orientação óbvia visualmente enquanto orbita).
pub const CUBE_VERTICES: [Vertex; 8] = [
    Vertex { position: [-0.5, -0.5, -0.5], color: [0.1, 0.1, 0.1] },
    Vertex { position: [0.5, -0.5, -0.5], color: [1.0, 0.1, 0.1] },
    Vertex { position: [0.5, 0.5, -0.5], color: [1.0, 1.0, 0.1] },
    Vertex { position: [-0.5, 0.5, -0.5], color: [0.1, 1.0, 0.1] },
    Vertex { position: [-0.5, -0.5, 0.5], color: [0.1, 0.1, 1.0] },
    Vertex { position: [0.5, -0.5, 0.5], color: [1.0, 0.1, 1.0] },
    Vertex { position: [0.5, 0.5, 0.5], color: [1.0, 1.0, 1.0] },
    Vertex { position: [-0.5, 0.5, 0.5], color: [0.1, 1.0, 1.0] },
];

#[rustfmt::skip]
pub const CUBE_INDICES: [u16; 36] = [
    // front (z = +0.5)
    4, 5, 6, 6, 7, 4,
    // back (z = -0.5)
    1, 0, 3, 3, 2, 1,
    // left (x = -0.5)
    0, 4, 7, 7, 3, 0,
    // right (x = +0.5)
    5, 1, 2, 2, 6, 5,
    // top (y = +0.5)
    3, 7, 6, 6, 2, 3,
    // bottom (y = -0.5)
    0, 1, 5, 5, 4, 0,
];
