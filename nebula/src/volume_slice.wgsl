struct SceneUniform {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    // xyz usados; w é só padding pra alinhamento de 16 bytes em uniform buffers.
    light_position: vec4<f32>,
    camera_position: vec4<f32>,
    // x = 1.0 se a iluminação (ambiente+difusa) deve ser aplicada, 0.0 se não.
    // A visão 2D (câmera PanZoom) é leitura de dado, não superfície lit —
    // aplicar sombreamento nela escureceria a seção sem motivo nenhum.
    flags: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> scene: SceneUniform;

@group(1) @binding(0)
var volume_texture: texture_3d<f32>;
@group(1) @binding(1)
var volume_sampler: sampler;

@group(2) @binding(0)
var colormap_texture: texture_1d<f32>;
@group(2) @binding(1)
var colormap_sampler: sampler;
@group(2) @binding(2)
var<uniform> clim: vec4<f32>; // x = min, y = max; zw não usados (padding)

// Qual eixo do volume esta fatia mostra e em que posição normalizada (0..1)
// ao longo dele. axis: 0 = Inline, 1 = Crossline, 2 = Time — mesma convenção
// do `AXIS_CONFIG` do diálogo 2D do Andromeda.
struct SliceParams {
    axis: u32,
    index: f32,
    _pad: vec2<f32>,
};

@group(3) @binding(0)
var<uniform> slice: SliceParams;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) normal: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world_position: vec3<f32>,
    @location(2) world_normal: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    let world_position = scene.model * vec4<f32>(input.position, 1.0);
    out.world_position = world_position.xyz;
    out.clip_position = scene.view_proj * world_position;
    out.uv = input.uv;

    // `model` só tem rotação (sem escala não-uniforme), então transformar a
    // normal por ele direto já é correto — não precisa da inversa-transposta.
    out.world_normal = normalize((scene.model * vec4<f32>(input.normal, 0.0)).xyz);

    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // O volume é amostrado na ordem (inline, xline, amostra). Fixamos o eixo
    // escolhido na posição `slice.index` e varremos os outros dois com o uv
    // do quad — é assim que uma seção Inline/Crossline/Time é definida.
    var coord: vec3<f32>;
    if (slice.axis == 0u) {
        coord = vec3<f32>(slice.index, input.uv.x, input.uv.y);
    } else if (slice.axis == 1u) {
        coord = vec3<f32>(input.uv.x, slice.index, input.uv.y);
    } else {
        coord = vec3<f32>(input.uv.x, input.uv.y, slice.index);
    }

    let raw_value = textureSample(volume_texture, volume_sampler, coord).r;

    // Normaliza pelo clim (igual o `clim=(min, max)` do Andromeda) antes de
    // indexar a LUT — os dois extremos do colormap ficam nos limites do dado,
    // não em 0..1 fixo.
    let t = clamp((raw_value - clim.x) / max(clim.y - clim.x, 1e-6), 0.0, 1.0);
    let albedo = textureSample(colormap_texture, colormap_sampler, t).rgb;

    if (scene.flags.x < 0.5) {
        // Visão 2D: cor crua do colormap, sem sombreamento.
        return vec4<f32>(albedo, 1.0);
    }

    var normal = normalize(input.world_normal);
    let view_dir = normalize(scene.camera_position.xyz - input.world_position);

    // Plano sem espessura: só existe uma normal por vértice, mas as duas faces
    // podem ficar visíveis (ex: objeto visto por trás). Vira a normal pro lado
    // de quem está olhando pra não escurecer a face errada.
    if dot(normal, view_dir) < 0.0 {
        normal = -normal;
    }

    let light_dir = normalize(scene.light_position.xyz - input.world_position);

    // Sem especular: com a luz colada na câmera (headlight), o brilho
    // especular fica sempre grudado bem no centro da tela, parecendo uma
    // superfície molhada/plástica — errado pra uma fatia sísmica, que é fosca.
    let ambient = 0.15;
    let diffuse = max(dot(normal, light_dir), 0.0);

    let lit = albedo * (ambient + diffuse);
    return vec4<f32>(lit, 1.0);
}
