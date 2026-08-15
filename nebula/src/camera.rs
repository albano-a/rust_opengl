use glam::{Mat4, Vec3};

/// Câmera orbital (turntable), equivalente ao `MiddlePanTurntableCamera` do VisPy
/// usado hoje no Andromeda: gira em torno de um alvo fixo, mantendo distância e
/// "up" estáveis. Parametrizada por azimute/elevação em vez de posição livre pra
/// nunca perder a orientação (sem risco de gimbal lock visual esquisito).
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    pub azimuth: f32,
    pub elevation: f32,
    pub fovy_radians: f32,
    pub aspect: f32,
    pub znear: f32,
    pub zfar: f32,
}

impl OrbitCamera {
    const MIN_DISTANCE: f32 = 1.0;
    const MAX_DISTANCE: f32 = 100.0;
    // Evita a câmera cruzar o polo (elevação = ±90°), onde azimute perde sentido.
    const MAX_ELEVATION: f32 = 1.5;

    pub fn new(aspect: f32) -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 4.0,
            azimuth: std::f32::consts::FRAC_PI_4,
            elevation: std::f32::consts::FRAC_PI_6,
            fovy_radians: std::f32::consts::FRAC_PI_4,
            aspect,
            znear: 0.1,
            zfar: 100.0,
        }
    }

    fn eye(&self) -> Vec3 {
        let x = self.distance * self.elevation.cos() * self.azimuth.sin();
        let y = self.distance * self.elevation.sin();
        let z = self.distance * self.elevation.cos() * self.azimuth.cos();
        self.target + Vec3::new(x, y, z)
    }

    // `look_at_rh`/`perspective_rh` estão marcadas deprecated em favor de uma API
    // de câmera nova (glam::camera::*) recém-introduzida e ainda instável; ficamos
    // com as funções atuais até ela se firmar.
    #[allow(deprecated)]
    pub fn view_proj(&self) -> Mat4 {
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Y);
        let proj = Mat4::perspective_rh(self.fovy_radians, self.aspect.max(0.01), self.znear, self.zfar);
        proj * view
    }

    /// Botão esquerdo do mouse: gira a câmera em torno do alvo.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        const SENSITIVITY: f32 = 0.005;
        self.azimuth -= dx * SENSITIVITY;
        self.elevation = (self.elevation + dy * SENSITIVITY)
            .clamp(-Self::MAX_ELEVATION, Self::MAX_ELEVATION);
    }

    /// Botão do meio: translada o alvo no plano da tela (right/up relativos à câmera).
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let eye = self.eye();
        let forward = (self.target - eye).normalize();
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward).normalize();

        let sensitivity = 0.0015 * self.distance;
        self.target += -right * dx * sensitivity + up * dy * sensitivity;
    }

    /// Botão direito / scroll: aproxima ou afasta a câmera do alvo.
    pub fn zoom(&mut self, delta: f32) {
        self.distance = (self.distance - delta * 0.01 * self.distance)
            .clamp(Self::MIN_DISTANCE, Self::MAX_DISTANCE);
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        self.aspect = aspect.max(0.01);
    }
}
