struct SceneUniform {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    // xyz usados; w é só padding pra alinhamento de 16 bytes em uniform buffers.
    light_position: vec4<f32>,
    camera_position: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> scene: SceneUniform;

@group(1) @binding(0)
var volume_texture: texture_3d<f32>;
@group(1) @binding(1)
var volume_sampler: sampler;

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
    // Fase 3: sempre a fatia do meio (w = 0.5). Amostrar um w escolhido pelo
    // usuário é trabalho da Fase 4/5 (equivalente ao AxisAlignedImage do VisPy).
    let albedo = textureSample(volume_texture, volume_sampler, vec3<f32>(input.uv, 0.5)).r;

    let normal = normalize(input.world_normal);
    let light_dir = normalize(scene.light_position.xyz - input.world_position);
    let view_dir = normalize(scene.camera_position.xyz - input.world_position);
    let half_dir = normalize(light_dir + view_dir);

    let ambient = 0.15;
    let diffuse = max(dot(normal, light_dir), 0.0);
    let specular = pow(max(dot(normal, half_dir), 0.0), 32.0) * 0.5;

    let lit = albedo * (ambient + diffuse) + specular;
    return vec4<f32>(lit, lit, lit, 1.0);
}
