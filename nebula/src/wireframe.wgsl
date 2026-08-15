// Wireframe da caixa do cubo sísmico: linhas sólidas, sem luz, sempre visíveis
// por cima do resto da cena (depth test sempre passa, sem escrever no depth
// buffer) — é uma referência espacial, não geometria "real" que deveria
// ocluir ou ser ocluída pelas fatias.
struct SceneUniform {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    light_position: vec4<f32>,
    camera_position: vec4<f32>,
    flags: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> scene: SceneUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = scene.view_proj * scene.model * vec4<f32>(input.position, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(input.color, 1.0);
}
