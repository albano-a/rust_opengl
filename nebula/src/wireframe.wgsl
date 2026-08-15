// Wireframe da caixa do cubo sísmico: linhas sólidas, sem luz. É um objeto
// de verdade na cena, não um HUD — o pipeline (`lib.rs`) usa
// `depth_compare: Less`, então uma aresta fica escondida atrás de uma fatia
// opaca que esteja na frente dela, igual qualquer geometria normal. Não
// escreve no depth buffer (`depth_write_enabled: false`), então também não
// bloqueia nada desenhado depois dele (os ticks do grid de eixo, o texto).
struct SceneUniform {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    light_position: vec4<f32>,
    camera_position: vec4<f32>,
    flags: vec4<f32>,
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
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
