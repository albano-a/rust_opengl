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
            // Azimute/elevação 0 = olhando reto pro eixo Z do mundo, que é
            // exatamente a direção perpendicular ao quad de seção sísmica
            // (fatiado na crossline do meio). Os 45°/30° de antes eram bons
            // pra mostrar o cubo genérico da Fase 2, mas deixavam a seção
            // sísmica com cara de trapézio, vista de esguelha por padrão.
            azimuth: 0.0,
            elevation: 0.0,
            fovy_radians: std::f32::consts::FRAC_PI_4,
            aspect,
            znear: 0.1,
            zfar: 100.0,
        }
    }

    /// Posição da câmera no mundo — usada pelo shader pro termo especular
    /// (que depende de onde o observador está, ao contrário da difusa).
    pub fn eye(&self) -> Vec3 {
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

    /// Eixos "direita"/"cima" da câmera em coordenadas de mundo — usados pra
    /// billboard de texto (o quad de cada caractere sempre de frente pra
    /// tela, não pro objeto). Mesma conta do `pan()`, só devolvida em vez de
    /// aplicada.
    pub fn basis(&self) -> (Vec3, Vec3) {
        let eye = self.eye();
        let forward = (self.target - eye).normalize();
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward).normalize();
        (right, up)
    }
}

/// Câmera 2D (pan + zoom, sem rotação), equivalente ao `PanZoomCamera` do
/// VisPy usado hoje no diálogo de seção 2D do Andromeda. Olha sempre reto
/// pro plano XY a partir de +Z, com projeção ortográfica — sem perspectiva,
/// porque uma seção 2D é leitura de dado, não uma cena espacial.
pub struct PanZoomCamera {
    pub target: Vec3,
    pub zoom: f32,
    pub aspect: f32,
}

impl PanZoomCamera {
    const MIN_ZOOM: f32 = 0.02;
    const MAX_ZOOM: f32 = 50.0;

    pub fn new(aspect: f32) -> Self {
        Self { target: Vec3::ZERO, zoom: 1.2, aspect }
    }

    pub fn eye(&self) -> Vec3 {
        self.target + Vec3::new(0.0, 0.0, 5.0)
    }

    #[allow(deprecated)]
    pub fn view_proj(&self) -> Mat4 {
        let half_h = self.zoom;
        let half_w = half_h * self.aspect.max(0.01);
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Y);
        let proj = Mat4::orthographic_rh(-half_w, half_w, -half_h, half_h, 0.1, 100.0);
        proj * view
    }

    /// Arrastar com o botão esquerdo/do meio: translada o alvo no plano.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let sensitivity = 0.002 * self.zoom;
        self.target += Vec3::new(-dx * sensitivity, dy * sensitivity, 0.0);
    }

    /// Scroll: aproxima/afasta (reduz/aumenta a área ortográfica visível).
    pub fn zoom(&mut self, delta: f32) {
        self.zoom = (self.zoom - delta * 0.01 * self.zoom).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        self.aspect = aspect.max(0.01);
    }

    /// Câmera ortográfica nunca inclina — eixos fixos do mundo.
    pub fn basis(&self) -> (Vec3, Vec3) {
        (Vec3::X, Vec3::Y)
    }
}

/// As duas câmeras que o Nebula suporta hoje: orbital (visão 3D, com luz) e
/// pan/zoom (visão 2D, sem luz — ver `SceneUniform::flags`). Um `Renderer` usa
/// uma ou outra pra vida inteira, escolhida na criação.
pub enum CameraKind {
    Orbit(OrbitCamera),
    PanZoom(PanZoomCamera),
}

impl CameraKind {
    pub fn view_proj(&self) -> Mat4 {
        match self {
            CameraKind::Orbit(c) => c.view_proj(),
            CameraKind::PanZoom(c) => c.view_proj(),
        }
    }

    pub fn eye(&self) -> Vec3 {
        match self {
            CameraKind::Orbit(c) => c.eye(),
            CameraKind::PanZoom(c) => c.eye(),
        }
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        match self {
            CameraKind::Orbit(c) => c.set_aspect(aspect),
            CameraKind::PanZoom(c) => c.set_aspect(aspect),
        }
    }

    pub fn basis(&self) -> (Vec3, Vec3) {
        match self {
            CameraKind::Orbit(c) => c.basis(),
            CameraKind::PanZoom(c) => c.basis(),
        }
    }

    /// Seção 2D é dado plano, não superfície lit — a luz (difusa/ambiente)
    /// só faz sentido na visão 3D orbital.
    pub fn lighting_enabled(&self) -> f32 {
        match self {
            CameraKind::Orbit(_) => 1.0,
            CameraKind::PanZoom(_) => 0.0,
        }
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        if let CameraKind::Orbit(c) = self {
            c.orbit(dx, dy);
        }
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        match self {
            CameraKind::Orbit(c) => c.pan(dx, dy),
            CameraKind::PanZoom(c) => c.pan(dx, dy),
        }
    }

    pub fn zoom(&mut self, delta: f32) {
        match self {
            CameraKind::Orbit(c) => c.zoom(delta),
            CameraKind::PanZoom(c) => c.zoom(delta),
        }
    }
}
