//! Convenção espacial do cubo sísmico (cubo unitário -1..1 em cada eixo,
//! mesma caixa do wireframe em `geometry.rs`): mundo X = Inline, mundo Y =
//! Crossline, mundo Z = Time (fatia rasa fica em cima — Z é o eixo "pra
//! cima" da câmera, ver `OrbitCamera` em `camera.rs`, não Y). Funções puras,
//! sem GPU/PyO3 — reusadas tanto pela API de fatias (`slice_api.rs`) quanto
//! pelo grid de eixo (`text_api.rs`).

use glam::{Mat4, Vec3};

/// Devolve a direção de translação de cada tipo de fatia (Inline/Crossline/
/// Time) no espaço de mundo.
pub(crate) fn slice_move_axis(axis: u32) -> Vec3 {
    match axis {
        0 => Vec3::X, // Inline
        1 => Vec3::Y, // Crossline
        _ => Vec3::Z, // Time
    }
}

/// Gira o quad plano (que nasce na origem, no plano XY local) pra ficar
/// perpendicular ao eixo certo e o translada até a posição normalizada
/// (0..1 -> -1..1) — é isso que faz Inline/Crossline/Time virarem três
/// planos de verdade se cruzando dentro do cubo, em vez de um quad fixo só
/// trocando de textura.
pub(crate) fn slice_model_matrix(axis: u32, index: f32) -> Mat4 {
    let pos = index.clamp(0.0, 1.0) * 2.0 - 1.0;
    match axis {
        0 => {
            // Inline fixo em X=pos; plano varre Crossline (Y) e Time (Z).
            Mat4::from_translation(Vec3::new(pos, 0.0, 0.0))
                * Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2)
        }
        1 => {
            // Crossline fixo em Y=pos; plano varre Inline (X) e Time (Z).
            Mat4::from_translation(Vec3::new(0.0, pos, 0.0))
                * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)
        }
        _ => {
            // Time fixo em Z=pos; plano varre Inline (X) e Crossline (Y) —
            // é o quad plano na sua orientação original, só transladado.
            Mat4::from_translation(Vec3::new(0.0, 0.0, pos))
        }
    }
}

/// Posiciona um volume DENTRO do cubo unitário -1..1 da survey. `origin`/
/// `extent` são frações 0..1 do espaço normalizado da survey (não do
/// próprio volume) — ex: um volume que começa na metade da survey e ocupa
/// 30% dela num eixo tem origin=0.5, extent=0.3 nesse eixo. Aplicado ANTES
/// de `slice_model_matrix` (que continua operando no espaço -1..1 local do
/// próprio volume, sem saber nada sobre a survey): a fatia nasce no cubo
/// local do volume, e essa matriz encolhe/translada esse cubo local pro
/// pedaço certo do cubo da survey. Default origin=(0,0,0), extent=(1,1,1) é
/// identidade (volume cobre a survey inteira, o caso mais comum: a sísmica
/// principal) — reduz exatamente à conta antiga quando não há sub-região.
pub(crate) fn volume_placement_matrix(origin: Vec3, extent: Vec3) -> Mat4 {
    // Derivação: um ponto local p (-1..1) normaliza pra (p+1)/2 (0..1 no
    // volume), passa pro espaço da survey como origin + normalizado*extent
    // (0..1 na survey), e volta pra -1..1 multiplicando por 2 e subtraindo
    // 1 — que simplifica pra escala `extent` + translação `origin*2-1+extent`.
    let translation = origin * 2.0 - Vec3::ONE + extent;
    Mat4::from_translation(translation) * Mat4::from_scale(extent)
}

/// Ponto num eixo do cubo, deslocado `out` unidades pra fora da caixa -1..1
/// nas duas direções perpendiculares ao eixo `axis` — usado tanto pro ponto
/// que fica exatamente na aresta (`out=1.0`) quanto pros pontos mais afastados
/// (tick, texto do valor, nome do eixo). `near_side` são os sinais (±1) do
/// canto do cubo mais perto da câmera, escolhido pra cada um dos 3 eixos do
/// mundo (ver `update_axis_grid` em `text_api.rs` pro porquê de ser o canto
/// próximo, não o distante).
pub(crate) fn axis_grid_point(axis: u32, local: f32, near_side: Vec3, out: f32) -> Vec3 {
    match axis {
        0 => Vec3::new(local, near_side.y * out, near_side.z * out), // Inline varia em X
        1 => Vec3::new(near_side.x * out, local, near_side.z * out), // Crossline varia em Y
        _ => Vec3::new(near_side.x * out, near_side.y * out, local), // Time varia em Z
    }
}
