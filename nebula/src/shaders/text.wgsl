// Texto GPU nativo (labels de eixo, nomes de poço na Fase 5, etc.): cada
// caractere é um quad billboard (sempre de frente pra câmera) amostrando um
// atlas de fonte bitmap. Sem luz, sempre visível por cima de tudo (mesmo
// espírito de um HUD/anotação — igual o Petrel deixa os labels dos eixos
// sempre legíveis, não escondidos atrás da geometria).
struct SceneUniform {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    light_position: vec4<f32>,
    camera_position: vec4<f32>,
    flags: vec4<f32>,
    // xyz = eixo "direita" da câmera em coordenadas de mundo, usado pro
    // billboard do texto ficar sempre de frente pra tela.
    camera_right: vec4<f32>,
    // xyz = eixo "cima" da câmera em coordenadas de mundo.
    camera_up: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> scene: SceneUniform;

struct TextParams {
    // xyz = posição no mundo do centro do label; w = escala.
    anchor_scale: vec4<f32>,
    // rgb = cor do texto; a não usado.
    color: vec4<f32>,
};

@group(1) @binding(0)
var<uniform> text: TextParams;

@group(2) @binding(0)
var atlas_texture: texture_2d<f32>;
@group(2) @binding(1)
var atlas_sampler: sampler;

struct VertexInput {
    @location(0) local_offset: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    // `scene.model` só escala o PONTO de ancoragem (pra ele acompanhar o
    // paralelepípedo quando o cubo não é equidimensional, ex: 250x350x100)
    // — o deslocamento do billboard (o quad de cada caractere) é somado
    // DEPOIS, em unidades de mundo reais, não-escaladas. Se a escala não-
    // uniforme entrasse no billboard também, as letras ficariam esticadas/
    // espremidas de forma diferente conforme o ângulo da câmera (o
    // deslocamento é ao longo de `camera_right`/`camera_up`, direções
    // arbitrárias que não estão alinhadas com os eixos X/Y/Z da escala).
    let anchor_world = (scene.model * vec4<f32>(text.anchor_scale.xyz, 1.0)).xyz;
    let world_pos = anchor_world
        + scene.camera_right.xyz * input.local_offset.x * text.anchor_scale.w
        + scene.camera_up.xyz * input.local_offset.y * text.anchor_scale.w;

    out.clip_position = scene.view_proj * vec4<f32>(world_pos, 1.0);
    out.uv = input.uv;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let coverage = textureSample(atlas_texture, atlas_sampler, input.uv).r;
    if coverage < 0.05 {
        discard;
    }
    return vec4<f32>(text.color.rgb, coverage);
}
