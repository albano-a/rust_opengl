"""Protótipo de embedding (Fase 1) + câmera orbital (Fase 2) + textura de volume
(Fase 3): uma fatia de um volume 3D sintético, renderizada via wgpu (módulo
`nebula`), embutida numa QWindow do PyQt5 através de createWindowContainer,
com câmera controlada por mouse.

Controles (mesma convenção do MiddlePanTurntableCamera do VisPy no Andromeda):
    botão esquerdo arrastando  -> orbit (gira em torno do alvo)
    botão do meio arrastando   -> pan (translada o alvo)
    botão direito arrastando / scroll -> zoom

Inclui um dock de árvore de objetos e uma toolbar como stand-ins para os widgets reais
que cercam o canvas 3D no Andromeda (svwObjectTreeWidget / slicesToolBar), só pra
observar se o compositing extra do Qt afeta o FPS do canvas wgpu.

Rodar de dentro de nebula/.venv (com `nebula` instalado via `maturin develop`):
    python ../python/embed_test.py
"""

import sys
import time

import matplotlib
import numpy as np
from PyQt5.QtCore import QPoint, Qt, QTimer
from PyQt5.QtGui import QColor, QLinearGradient, QPainter, QWindow
from PyQt5.QtWidgets import (
    QAction,
    QApplication,
    QComboBox,
    QDialog,
    QDockWidget,
    QHBoxLayout,
    QLabel,
    QMainWindow,
    QSlider,
    QToolBar,
    QTreeWidget,
    QTreeWidgetItem,
    QVBoxLayout,
    QWidget,
)

import nebula

# Mesma convenção do AXIS_CONFIG do Slice2DDialog do Andromeda: 0=Inline,
# 1=Crossline, 2=Time. O shader do Nebula usa esses mesmos índices.
AXIS_NAMES = ["Inline", "Crossline", "Time"]
AXIS_INDEX = {name: i for i, name in enumerate(AXIS_NAMES)}

# Troque aqui pra testar outro colormap contínuo do matplotlib (ex: "jet",
# "gray_r", "seismic", "gist_rainbow_r" — os mesmos nomes já usados hoje no
# Andromeda). Qualquer colormap "contínuo" do matplotlib funciona; LUTs
# categóricas (ex: facies) são um caso à parte, resolvido no lado VisPy do
# LogPlot Vision, não aqui.
COLORMAP_NAME = "viridis"

# "seismic": refletores sintéticos (parece sísmica de verdade).
# "checker": mostra a variação de cor do colormap (útil pra validar a paleta).
# "constant": cinza uniforme, útil pra isolar e testar só a iluminação.
VOLUME_PATTERN = "seismic"


def build_colormap_lut(name: str, resolution: int = 256) -> np.ndarray:
    """Amostra um colormap do matplotlib em `resolution` pontos e devolve um
    array `uint8` `(resolution, 4)` RGBA — o formato que `Renderer.set_colormap`
    espera.

    O Nebula não sabe nada sobre matplotlib/VisPy: só recebe essa tabela já
    pronta. No Andromeda, os colormaps do Petrel/Paradigm já viram objetos
    `vispy.color.Colormap`, que também têm um jeito de amostrar em N pontos
    (`cmap.map(...)`) — o mesmo padrão se aplicaria trocando só essa função.
    """
    cmap = matplotlib.colormaps[name]
    samples = cmap(np.linspace(0.0, 1.0, resolution))  # (resolution, 4) float 0..1
    return np.ascontiguousarray(samples * 255.0, dtype=np.uint8)


def _ricker_wavelet(length: int, freq: float) -> np.ndarray:
    """Wavelet de Ricker ("chapéu mexicano") — a mesma forma de onda usada em
    modelagem sísmica sintética de verdade pra convolver com a refletividade."""
    t = np.arange(length) - length // 2
    a = (np.pi * freq * t) ** 2
    return ((1.0 - 2.0 * a) * np.exp(-a)).astype(np.float32)


def build_seismic_volume(
    width=128,
    height=128,
    depth=128,
    n_reflectors=16,
    n_faults=3,
    channel=True,
    seed=7,
):
    """Sísmica sintética de verdade: refletores em camadas (com mergulho e
    dobra suaves, diferentes por camada) convolvidos com uma wavelet Ricker ao
    longo do eixo de profundidade — o mesmo princípio (refletividade × wavelet)
    usado em modelagem sísmica convolucional real, só que sem física de
    propagação de onda por trás.

    Mais complexo que a versão original da Fase 3.7: falhas normais (degraus
    nas camadas) e um corpo de canal sinuoso — dá pra testar o cubo com algo
    que não seja só camadas paralelas planas, mais parecido com um survey de
    verdade.
    """
    rng = np.random.default_rng(seed)

    y_idx, x_idx = np.meshgrid(np.arange(height), np.arange(width), indexing="ij")
    xn = x_idx / max(width - 1, 1)
    yn = y_idx / max(height - 1, 1)

    reflectivity = np.zeros((depth, height, width), dtype=np.float32)

    base_positions = np.sort(rng.uniform(0.08, 0.92, n_reflectors))
    strengths = rng.uniform(-1.0, 1.0, n_reflectors)

    # Falhas normais simples: cada uma desloca (em amostras) tudo que está de
    # um lado de um plano de falha — que também pode ser inclinado com a
    # profundidade — criando as descontinuidades em degrau típicas de uma
    # seção real, que um empilhado de camadas paralelas nunca mostra.
    faults = [
        (rng.uniform(0.2, 0.8), rng.uniform(-0.3, 0.3), int(rng.integers(3, 10)) * int(rng.choice([-1, 1])))
        for _ in range(n_faults)
    ]

    for pos, strength in zip(base_positions, strengths):
        # Mergulho (plano inclinado) + dobra (senoide) — cada refletor com sua
        # própria variação, pra parecer camadas geológicas de verdade, não
        # planos paralelos perfeitos.
        dip_x = rng.uniform(-0.06, 0.06)
        dip_y = rng.uniform(-0.04, 0.04)
        fold_amp = rng.uniform(0.01, 0.05)
        fold_freq = rng.uniform(1.0, 3.0)
        fold_phase = rng.uniform(0.0, 2 * np.pi)

        undulation = (
            dip_x * (xn - 0.5)
            + dip_y * (yn - 0.5)
            + fold_amp * np.sin(2 * np.pi * fold_freq * xn + fold_phase)
        )
        depth_f = (pos + undulation) * (depth - 1)

        for fault_x, fault_dip, throw in faults:
            fault_plane = fault_x * (width - 1) + fault_dip * (depth_f - depth_f.mean())
            depth_f = np.where(x_idx > fault_plane, depth_f + throw, depth_f)

        depth_idx = np.clip(np.round(depth_f).astype(np.int32), 0, depth - 1)
        np.add.at(reflectivity, (depth_idx, y_idx, x_idx), strength)

    # Convolve cada traço (eixo Z) com a wavelet — é isso que transforma os
    # "espinhos" de refletividade nas ondulações típicas de seção sísmica.
    wavelet = _ricker_wavelet(length=17, freq=0.11)
    volume = np.apply_along_axis(
        lambda trace: np.convolve(trace, wavelet, mode="same"), axis=0, arr=reflectivity
    )

    if channel:
        # Corpo de canal sinuoso (ex: arenito de canal) atravessando o volume
        # numa profundidade média — um "alvo" geológico isolado no meio das
        # camadas, não só refletores planos.
        z_idx = np.arange(depth).reshape(depth, 1, 1)
        channel_center = 0.5 * height + 0.16 * height * np.sin(2 * np.pi * 1.4 * xn)
        channel_depth = 0.45 * depth + 0.05 * depth * np.sin(2 * np.pi * 2.2 * xn + 1.0)
        channel_half_width = 0.05 * height
        dist_from_center = np.abs(y_idx - channel_center)
        in_channel = (dist_from_center < channel_half_width) & (
            np.abs(z_idx - channel_depth) < 0.03 * depth
        )
        volume = np.where(in_channel, volume + 0.7, volume)

    # Ruído fraco pra não ficar sintético/limpo demais.
    volume = volume + rng.normal(0.0, 0.02, volume.shape).astype(np.float32)

    volume -= volume.min()
    max_value = volume.max()
    if max_value > 1e-9:
        volume /= max_value
    return np.ascontiguousarray(volume, dtype=np.float32)


def build_synthetic_volume(width=64, height=64, depth=64, pattern="constant"):
    """Volume escalar sintético só pra provar o caminho de upload Python ->
    textura 3D — sem ligação com dados sísmicos reais ainda.

    `pattern="constant"`: cinza uniforme (albedo = 0.7 em todo canto), útil pra
    testar iluminação isoladamente — sem xadrez/gradiente competindo
    visualmente com a sombra.
    `pattern="checker"`: gradiente + xadrez 3D, útil pra validar que o upload
    da textura preserva a estrutura espacial certa (foi o teste da Fase 3).
    `pattern="seismic"`: refletores em camadas convolvidos com wavelet Ricker
    (`build_seismic_volume`) — parece sísmica de verdade, bom pra validar
    colormap + iluminação juntos.

    Layout: array numpy C-contíguo de shape (depth, height, width), que é
    exatamente a ordem de bytes que `Renderer.load_volume` espera (x/width é o
    eixo que varia mais rápido na memória).
    """
    if pattern == "constant":
        return np.full((depth, height, width), 0.7, dtype=np.float32)

    if pattern == "seismic":
        return build_seismic_volume(width, height, depth)

    x = np.linspace(0.0, 1.0, width, dtype=np.float32)
    y = np.linspace(0.0, 1.0, height, dtype=np.float32)
    z = np.linspace(0.0, 1.0, depth, dtype=np.float32)
    zz, yy, xx = np.meshgrid(z, y, x, indexing="ij")

    # Xadrez só em (x, y): fica idêntico em qualquer fatia Z, então nunca
    # cancela no sampling bilinear entre duas camadas de profundidade vizinhas
    # (o que acontecia quando Z também entrava na paridade do xadrez e a fatia
    # amostrada caía exatamente numa borda de paridade).
    checker_xy = ((xx * 8).astype(np.int32) + (yy * 8).astype(np.int32)) % 2

    volume = 0.35 * checker_xy.astype(np.float32) + 0.35 * xx + 0.3 * zz
    return np.ascontiguousarray(volume, dtype=np.float32)


class MainWindow(QMainWindow):
    """QMainWindow cujo closeEvent corta o render antes do Qt começar a destruir
    os widgets filhos (aboutToQuit dispara tarde demais para isso — a essa altura
    a QWindow embutida já pode ter sido invalidada)."""

    def __init__(self, render_window: "NebulaWindow", timer: QTimer):
        super().__init__()
        self._render_window = render_window
        self._timer = timer

    def closeEvent(self, event):
        self._timer.stop()
        self._render_window.shutdown()
        super().closeEvent(event)


class NebulaWindow(QWindow):
    """QWindow nativa que hospeda a wgpu::Surface do Nebula.

    setSurfaceType(OpenGLSurface) impede o Qt de tentar pintar por cima com seu
    próprio backing store — quem desenha aqui dentro é o wgpu, via swapchain
    própria, sem passar pelo pipeline de pintura do Qt.

    `mode="orbit"` é a visão 3D de sempre (Fase 2+); `mode="panzoom"` é a
    visão 2D (Fase 4) — câmera ortográfica sem rotação, sem iluminação,
    usada pelo `Slice2DDialog`. É a mesma classe pros dois casos.

    Pode mostrar várias fatias ao mesmo tempo (`configure_slices`) — no cubo
    3D, as três (Inline/Crossline/Time) ficam sempre visíveis simultaneamente,
    como três planos de verdade se cruzando dentro do volume (igual o Ocean/
    Petrel), não um quad só trocando de conteúdo. Uma delas é a "fatia ativa"
    (`active_slice_id`): é nela que a combobox de eixo do Andromeda e o
    Ctrl+arrastar atuam — a combobox só *seleciona qual* fatia mexer, quem
    efetivamente move é o botão "Mover" (`set_move_mode`) ou o Ctrl+arrastar.
    """

    def __init__(self, mode: str = "orbit"):
        super().__init__()
        self.setSurfaceType(QWindow.OpenGLSurface)
        self._mode = mode
        self._renderer = None
        self._closed = False
        self._drag_button = None
        self._last_pos = None
        self._pending_volume = None
        self._pending_colormap = None
        self._pending_clim = None
        self._pending_opacity = None
        # slice_id -> (axis, index); adicionadas de verdade no Rust na
        # primeira render_frame() depois que o Renderer existe.
        self._slices = {}
        self._slices_added = set()
        self.active_slice_id = None
        self.move_mode = False
        # Fatia descoberta debaixo do cursor no início de um Ctrl+arrastar
        # (não depende da combobox — só de onde o mouse realmente está).
        self._ctrl_drag_slice_id = None
        # Chamado (slice_id, axis, index) sempre que uma fatia muda, inclusive
        # por Ctrl+arrastar/Mover — quem hospeda a janela (ex: Slice2DDialog)
        # pode plugar aqui pra manter slider/combobox em sincronia.
        self.on_slice_changed = None

    def configure_slices(self, slices: dict, active_slice_id):
        """Define o conjunto de fatias mostradas (chamar antes do primeiro
        `render_frame`, ou seja, antes da janela aparecer) — `slices` é
        `{slice_id: (axis, index)}`."""
        self._slices = dict(slices)
        self.active_slice_id = active_slice_id

    def set_volume(self, width: int, height: int, depth: int, data: np.ndarray):
        # O Renderer só existe depois do primeiro resize real (winId() só é
        # válido depois que a QWindow tem uma janela nativa por trás). Guardamos
        # o volume e mandamos pro Rust assim que o Renderer estiver pronto.
        self._pending_volume = (width, height, depth, data)

    def set_colormap(self, rgba: np.ndarray, discrete: bool = False):
        self._pending_colormap = (rgba, discrete)

    def set_clim(self, min_value: float, max_value: float):
        self._pending_clim = (min_value, max_value)

    def set_volume_opacity(self, opacity: float):
        # Igual o Andromeda: opacidade é ajustável, mas o padrão é 1.0
        # (totalmente opaco) — só some quando o usuário mexe de propósito.
        self._pending_opacity = opacity

    def set_move_mode(self, enabled: bool):
        # Equivalente ao botão "Mover" do Andromeda: com o modo ativo, um
        # arrasto simples (sem precisar de Ctrl) já move a fatia ativa em vez
        # de orbitar a câmera.
        self.move_mode = bool(enabled)

    def set_slice(self, slice_id: int, axis: int, index: float):
        self._slices[slice_id] = (axis, index)
        renderer = self._renderer
        if renderer is not None and slice_id in self._slices_added:
            renderer.set_slice_axis_index(slice_id, axis, index)
        if self.on_slice_changed is not None:
            self.on_slice_changed(slice_id, axis, index)

    def project_to_screen(self, x: float, y: float, z: float):
        """Projeta um ponto do cubo (-1..1, mesma convenção do wireframe) pra
        coordenada de tela sob a câmera atual — usado pelos labels de eixo
        (`EdgeLabelsOverlay`) pra se posicionar sem o Nebula saber desenhar
        texto. Devolve `None` se o Renderer ainda não existe ou o ponto está
        atrás da câmera."""
        if self._renderer is None:
            return None
        return self._renderer.project_to_screen(x, y, z)

    def shutdown(self):
        # Chamado antes do Qt começar a destruir a janela nativa: depois disso,
        # o HWND por trás desta QWindow pode deixar de ser válido a qualquer
        # momento, então paramos de tocar na Surface.
        self._closed = True

    def _ensure_renderer(self):
        if self._closed:
            return None
        if self._renderer is None:
            hwnd = int(self.winId())
            self._renderer = nebula.Renderer(hwnd, max(self.width(), 1), max(self.height(), 1), self._mode)
        return self._renderer

    def resizeEvent(self, event):
        renderer = self._ensure_renderer()
        if renderer is not None:
            renderer.resize(max(self.width(), 1), max(self.height(), 1))
        super().resizeEvent(event)

    def render_frame(self):
        renderer = self._ensure_renderer()
        if renderer is None:
            return

        if self._pending_volume is not None:
            width, height, depth, data = self._pending_volume
            renderer.add_volume(0, width, height, depth, data)
            self._pending_volume = None

        if self._pending_colormap is not None:
            rgba, discrete = self._pending_colormap
            renderer.set_volume_colormap(0, rgba, discrete)
            self._pending_colormap = None

        if self._pending_clim is not None:
            renderer.set_volume_clim(0, *self._pending_clim)
            self._pending_clim = None

        if self._pending_opacity is not None:
            renderer.set_volume_opacity(0, self._pending_opacity)
            self._pending_opacity = None

        for slice_id, (axis, index) in self._slices.items():
            if slice_id not in self._slices_added:
                renderer.add_slice(slice_id, 0, axis, index)
                self._slices_added.add(slice_id)

        renderer.render()

    def mousePressEvent(self, event):
        self._drag_button = event.button()
        self._last_pos = event.pos()
        self._ctrl_drag_slice_id = None
        if event.modifiers() & Qt.ControlModifier:
            renderer = self._ensure_renderer()
            if renderer is not None:
                # Ctrl+clique não precisa da combobox: descobre embaixo de
                # qual fatia o cursor está de verdade, na cena.
                pos = event.pos()
                self._ctrl_drag_slice_id = renderer.pick_slice(float(pos.x()), float(pos.y()))

    def mouseReleaseEvent(self, event):
        self._drag_button = None
        self._last_pos = None
        self._ctrl_drag_slice_id = None

    def mouseMoveEvent(self, event):
        if self._last_pos is None:
            return
        renderer = self._ensure_renderer()
        if renderer is None:
            return

        pos = event.pos()
        dx = float(pos.x() - self._last_pos.x())
        dy = float(pos.y() - self._last_pos.y())
        self._last_pos = pos

        ctrl_held = bool(event.modifiers() & Qt.ControlModifier)
        if ctrl_held:
            # Ctrl+arrastar sempre mexe na fatia que está embaixo do cursor
            # (descoberta no mousePressEvent), independente da combobox.
            target_slice_id = self._ctrl_drag_slice_id
        elif self.move_mode:
            # Botão "Mover" da toolbar: usa a fatia selecionada na combobox
            # (igual o fluxo do Andromeda).
            target_slice_id = self.active_slice_id
        else:
            target_slice_id = None

        if target_slice_id is not None and target_slice_id in self._slices:
            axis, _old_index = self._slices[target_slice_id]
            if self._mode == "orbit":
                # Projeta a direção do eixo de movimento da fatia (em
                # coordenadas de mundo, sob a câmera atual) pra tela, e usa a
                # componente do arrasto alinhada com essa direção — arrastar
                # "ao longo" do eixo do grid avança a fatia de verdade,
                # arrastar perpendicular não faz quase nada.
                new_index = renderer.nudge_slice(target_slice_id, dx, dy)
            else:
                # Visão 2D: a fatia aparece de frente, então "mover ao longo
                # do eixo" é ir pra dentro/fora da tela — não existe direção
                # de arrasto pra projetar, então usa dy direto.
                new_index = min(1.0, max(0.0, self._slices[target_slice_id][1] + dy * 0.002))
                renderer.set_slice_axis_index(target_slice_id, axis, new_index)
            self._slices[target_slice_id] = (axis, new_index)
            if self.on_slice_changed is not None:
                self.on_slice_changed(target_slice_id, axis, new_index)
            return

        if self._mode == "panzoom":
            # 2D não tem orbit — botão esquerdo também vira pan.
            if self._drag_button in (Qt.LeftButton, Qt.MiddleButton):
                renderer.pan(dx, dy)
            elif self._drag_button == Qt.RightButton:
                renderer.zoom(dy)
            return

        if self._drag_button == Qt.LeftButton:
            renderer.orbit(dx, dy)
        elif self._drag_button == Qt.MiddleButton:
            renderer.pan(dx, dy)
        elif self._drag_button == Qt.RightButton:
            renderer.zoom(dy)

    def wheelEvent(self, event):
        renderer = self._ensure_renderer()
        if renderer is not None:
            renderer.zoom(event.angleDelta().y() / 8.0)


class ColorbarWidget(QWidget):
    """Colorbar funcional (Fase 4): gradiente + ticks min/meio/max, pintados
    em Qt puro (QPainter) a partir do mesmo LUT `(N,4)` uint8 já usado pelo
    Nebula (`build_colormap_lut`) e do `clim` atual. Deliberadamente fora do
    wgpu — texto em GPU exigiria um sistema de fonte/atlas novo (mesma razão
    pela qual o label da cabeça do poço, na Fase 5, também vai ficar em Qt).
    """

    def __init__(self, lut: np.ndarray, clim: tuple, parent=None):
        super().__init__(parent)
        self._lut = lut
        self._clim = clim
        self.setMinimumWidth(90)

    def set_lut(self, lut: np.ndarray):
        self._lut = lut
        self.update()

    def set_clim(self, clim: tuple):
        self._clim = clim
        self.update()

    def paintEvent(self, event):
        painter = QPainter(self)
        rect = self.rect()
        bar_width = 24
        margin = 10
        bar_rect = rect.adjusted(margin, margin, -(rect.width() - bar_width - margin), -margin)

        n = len(self._lut)
        gradient = QLinearGradient(0, bar_rect.top(), 0, bar_rect.bottom())
        step = max(1, n // 32)
        for i in range(0, n, step):
            # Topo da barra = valor máximo do clim (convenção usual de colorbar vertical).
            t = 1.0 - i / max(n - 1, 1)
            r, g, b, a = (int(c) for c in self._lut[i])
            gradient.setColorAt(t, QColor(r, g, b, a))

        painter.fillRect(bar_rect, gradient)
        painter.setPen(QColor(180, 180, 180))
        painter.drawRect(bar_rect)

        vmin, vmax = self._clim
        painter.setPen(QColor(220, 220, 220))
        for value, y in (
            (vmax, bar_rect.top()),
            ((vmin + vmax) * 0.5, bar_rect.center().y()),
            (vmin, bar_rect.bottom()),
        ):
            painter.drawText(bar_rect.right() + 6, y + 4, f"{value:.2f}")


class EdgeLabelsOverlay:
    """Labels de eixo (IL/XL/Time) nos cantos do wireframe do cubo — o que
    faltava pro cubo 3D bater com a referência do Petrel/Ocean (que numera os
    cantos da caixa). Reposicionados a cada frame via
    `NebulaWindow.project_to_screen` — o Nebula continua sem saber nada sobre
    renderizar texto (mesma decisão do `ColorbarWidget` e do plano futuro pro
    nome da cabeça do poço na Fase 5).

    `createWindowContainer` embute uma janela nativa de verdade; widgets Qt
    comuns filhos do container não compõem por cima dela (limitação
    conhecida), e forçar isso com `Qt.WA_AlwaysStackOnTop` se mostrou
    instável aqui (derrubou o processo, provavelmente por conflitar com a
    `wgpu::Surface` criada a partir do HWND cru). Em vez disso, os labels
    moram numa janela-`Tool` separada, sempre no topo, sem receber foco nem
    eventos de mouse (`WA_TransparentForMouseEvents` — não pode atrapalhar
    o orbit/pan/zoom do canvas por baixo), reposicionada manualmente pra
    cobrir exatamente a área do canvas a cada frame — a técnica padrão pra
    overlay sobre widgets nativos/OpenGL no Qt.

    Convenção espacial (mesma do wireframe em `geometry.rs`/`lib.rs`): mundo
    X = Inline, Y = Time (topo = raso), Z = Crossline, cubo unitário -1..1.
    """

    def __init__(self, render_window: "NebulaWindow", container: QWidget, width: int, height: int, depth: int):
        self._render_window = render_window
        self._container = container

        self._overlay = QWidget(
            container.window(),
            Qt.Tool | Qt.FramelessWindowHint | Qt.WindowStaysOnTopHint | Qt.WindowDoesNotAcceptFocus,
        )
        self._overlay.setAttribute(Qt.WA_TranslucentBackground)
        self._overlay.setAttribute(Qt.WA_TransparentForMouseEvents)
        self._overlay.setStyleSheet("background: transparent;")
        self._overlay.show()

        # (texto, ponto no cubo -1..1). Os quatro cantos de cima levam
        # IL/XL combinados (igual o Petrel mostra "IL970\nXL1650" junto no
        # canto) e um canto ganha também o Time — o outro extremo do Time
        # fica sozinho no canto de baixo correspondente.
        self._anchors = [
            ("IL 0\nXL 0\nT 0", (-1.0, 1.0, -1.0)),
            (f"IL {width - 1}\nXL 0", (1.0, 1.0, -1.0)),
            (f"IL {width - 1}\nXL {height - 1}", (1.0, 1.0, 1.0)),
            (f"IL 0\nXL {height - 1}", (-1.0, 1.0, 1.0)),
            (f"T {depth - 1}", (-1.0, -1.0, -1.0)),
        ]

        self._labels = []
        for text, _point in self._anchors:
            label = QLabel(text, self._overlay)
            label.setStyleSheet(
                "color: #7fd4ff; background: rgba(20,30,40,140); "
                "padding: 2px 4px; font-size: 10px; font-weight: bold;"
            )
            label.adjustSize()
            label.show()
            self._labels.append(label)

    def update_positions(self):
        top_left = self._container.mapToGlobal(QPoint(0, 0))
        self._overlay.setGeometry(top_left.x(), top_left.y(), self._container.width(), self._container.height())

        for label, (_text, point) in zip(self._labels, self._anchors):
            screen = self._render_window.project_to_screen(*point)
            if screen is None:
                label.hide()
                continue
            sx, sy = screen
            # Fora da viewport (atrás/aos lados) não precisa desenhar.
            if sx < -200 or sy < -200 or sx > self._container.width() + 200 or sy > self._container.height() + 200:
                label.hide()
                continue
            label.show()
            label.move(int(sx) - label.width() // 2, int(sy) - label.height() // 2)


class Slice2DDialog(QDialog):
    """Visão 2D de uma fatia (Fase 4) — equivalente ao `Slice2DDialog` do
    Andromeda, mas rodando no Nebula: câmera ortográfica sem luz
    (`NebulaWindow(mode="panzoom")`), colorbar funcional ao lado, seletor de
    eixo (Inline/Crossline/Time) e slider de posição. Sobrepor horizonte/poço
    na seção fica pra Fase 5, já que depende dos dados desses objetos
    existirem no Nebula primeiro.
    """

    def __init__(self, parent, volume_dim: int, volume_data: np.ndarray, colormap_lut: np.ndarray, clim: tuple):
        super().__init__(parent)
        self.setWindowTitle("Nebula - Seção 2D")
        self.resize(720, 620)

        self._render_window = NebulaWindow(mode="panzoom")
        container = QWidget.createWindowContainer(self._render_window, self)

        self._axis_combo = QComboBox()
        self._axis_combo.addItems(AXIS_NAMES)
        self._axis_combo.setCurrentText("Crossline")

        self._position_slider = QSlider(Qt.Horizontal)
        self._position_slider.setMinimum(0)
        self._position_slider.setMaximum(1000)
        self._position_slider.setValue(500)

        self._colorbar = ColorbarWidget(colormap_lut, clim)

        top_bar = QHBoxLayout()
        top_bar.addWidget(QLabel("Eixo:"))
        top_bar.addWidget(self._axis_combo)
        top_bar.addWidget(QLabel("Posição:"))
        top_bar.addWidget(self._position_slider, stretch=1)

        canvas_row = QHBoxLayout()
        canvas_row.addWidget(container, stretch=1)
        canvas_row.addWidget(self._colorbar)

        layout = QVBoxLayout(self)
        layout.addLayout(top_bar)
        layout.addLayout(canvas_row, stretch=1)

        self._render_window.set_volume(volume_dim, volume_dim, volume_dim, volume_data)
        self._render_window.set_colormap(colormap_lut)
        self._render_window.set_clim(*clim)
        # O diálogo 2D mostra uma fatia por vez (a que o combobox escolher) —
        # diferente do cubo 3D, que mostra as três ao mesmo tempo.
        self._render_window.configure_slices({0: (AXIS_INDEX["Crossline"], 0.5)}, active_slice_id=0)

        self._axis_combo.currentTextChanged.connect(self._on_axis_changed)
        self._position_slider.valueChanged.connect(self._on_position_changed)
        # Ctrl+arrastar dentro do canvas muda a fatia sem passar pelo slider —
        # mantém o slider/combobox mostrando a posição de verdade mesmo assim.
        self._render_window.on_slice_changed = self._on_render_slice_changed

        self._timer = QTimer(self)
        self._timer.timeout.connect(self._render_window.render_frame)
        self._timer.start(16)

    def _on_axis_changed(self, name: str):
        # Igual o Andromeda: trocar de eixo reseta pro meio do volume.
        self._render_window.set_slice(0, AXIS_INDEX[name], 0.5)
        self._position_slider.blockSignals(True)
        self._position_slider.setValue(self._position_slider.maximum() // 2)
        self._position_slider.blockSignals(False)

    def _on_position_changed(self, _value: int):
        axis = AXIS_INDEX[self._axis_combo.currentText()]
        index = self._position_slider.value() / self._position_slider.maximum()
        self._render_window.set_slice(0, axis, index)

    def _on_render_slice_changed(self, _slice_id: int, axis: int, index: float):
        self._axis_combo.blockSignals(True)
        self._axis_combo.setCurrentText(AXIS_NAMES[axis])
        self._axis_combo.blockSignals(False)
        self._position_slider.blockSignals(True)
        self._position_slider.setValue(int(round(index * self._position_slider.maximum())))
        self._position_slider.blockSignals(False)

    def closeEvent(self, event):
        self._timer.stop()
        self._render_window.shutdown()
        super().closeEvent(event)


def build_object_tree() -> QTreeWidget:
    """Réplica simplificada da svwObjectTreeWidget do Andromeda (grupos por tipo
    de objeto: Seismic / Elastic Attributes / Facies / Horizons / Wells)."""
    tree = QTreeWidget()
    tree.setHeaderLabel("Objects")

    groups = {
        "Seismic": ["survey_amplitude.h5", "survey_amplitude_filtered.h5"],
        "Elastic Attributes": ["ai_inversion.h5"],
        "Facies": ["facies_classification.h5"],
        "Horizons": ["top_reservoir", "base_reservoir"],
        "Wells": ["WELL-001", "WELL-002", "WELL-003"],
    }
    for group_name, children in groups.items():
        group_item = QTreeWidgetItem([group_name])
        group_item.setFlags(group_item.flags() | Qt.ItemIsUserCheckable)
        group_item.setCheckState(0, Qt.Unchecked)
        for child_name in children:
            child_item = QTreeWidgetItem([child_name])
            child_item.setFlags(child_item.flags() | Qt.ItemIsUserCheckable)
            child_item.setCheckState(0, Qt.Unchecked)
            group_item.addChild(child_item)
        tree.addTopLevelItem(group_item)

    tree.expandAll()
    return tree


def build_slices_toolbar(main_win: QMainWindow, render_window: "NebulaWindow", open_2d_callback) -> QToolBar:
    """Réplica da slicesToolBar do Andromeda — e do jeito certo (Fase 4,
    corrigido): a combobox só *seleciona qual* das três fatias (Inline/
    Crossline/Time, sempre as três visíveis ao mesmo tempo no cubo) vai ser
    movida; quem move de verdade é apertar "Mover" (fica pressionado — modo
    ligado) e arrastar o mouse, ou Ctrl+arrastar a qualquer momento como
    atalho. A combobox não muda a posição sozinha."""
    toolbar = QToolBar("Slices", main_win)

    axis_combo = QComboBox()
    axis_combo.addItems(AXIS_NAMES)
    axis_combo.setCurrentText("Crossline")
    axis_combo.currentTextChanged.connect(
        lambda name: setattr(render_window, "active_slice_id", AXIS_INDEX[name])
    )
    toolbar.addWidget(axis_combo)

    move_action = QAction("Mover", main_win)
    move_action.setCheckable(True)
    move_action.toggled.connect(render_window.set_move_mode)
    toolbar.addAction(move_action)

    toolbar.addSeparator()

    # Opacidade do volume — igual o Andromeda deixa o usuário ajustar, mas
    # começa em 100% (totalmente opaco).
    toolbar.addWidget(QLabel("Opacidade:"))
    opacity_slider = QSlider(Qt.Horizontal)
    opacity_slider.setMinimum(0)
    opacity_slider.setMaximum(100)
    opacity_slider.setValue(100)
    opacity_slider.setMaximumWidth(120)
    opacity_slider.valueChanged.connect(lambda v: render_window.set_volume_opacity(v / 100.0))
    toolbar.addWidget(opacity_slider)

    toolbar.addSeparator()

    scale_combo = QComboBox()
    scale_combo.addItems(["25%", "50%", "100%", "200%", "500%"])
    scale_combo.setCurrentText("100%")
    toolbar.addWidget(scale_combo)

    toolbar.addSeparator()
    toolbar.addAction(QAction("Reset View", main_win))

    toolbar.addSeparator()
    view_2d_action = QAction("Ver em 2D", main_win)
    view_2d_action.triggered.connect(open_2d_callback)
    toolbar.addAction(view_2d_action)

    return toolbar


def main():
    app = QApplication(sys.argv)

    render_window = NebulaWindow()
    timer = QTimer()

    main_win = MainWindow(render_window, timer)
    main_win.setWindowTitle("Andromeda + Nebula (protótipo Fase 3)")
    main_win.resize(1100, 700)

    container = QWidget.createWindowContainer(render_window, main_win)
    main_win.setCentralWidget(container)

    volume_dim = 128
    volume_data = build_synthetic_volume(volume_dim, volume_dim, volume_dim, pattern=VOLUME_PATTERN)
    colormap_lut = build_colormap_lut(COLORMAP_NAME)
    clim = (0.0, 1.0)  # bate com a faixa de valores do volume sintético

    render_window.set_volume(volume_dim, volume_dim, volume_dim, volume_data)
    render_window.set_colormap(colormap_lut)
    render_window.set_clim(*clim)
    # As três fatias (Inline/Crossline/Time) sempre visíveis ao mesmo tempo,
    # cruzando dentro do cubo — ids batendo com o índice do eixo, já que só
    # existe uma fatia por eixo neste protótipo. Crossline começa ativa (era
    # a única fatia mostrada nas fases anteriores).
    render_window.configure_slices(
        {
            AXIS_INDEX["Inline"]: (AXIS_INDEX["Inline"], 0.5),
            AXIS_INDEX["Crossline"]: (AXIS_INDEX["Crossline"], 0.5),
            AXIS_INDEX["Time"]: (AXIS_INDEX["Time"], 0.5),
        },
        active_slice_id=AXIS_INDEX["Crossline"],
    )

    edge_labels = EdgeLabelsOverlay(render_window, container, volume_dim, volume_dim, volume_dim)

    tree_dock = QDockWidget("Object Tree", main_win)
    tree_dock.setWidget(build_object_tree())
    main_win.addDockWidget(Qt.LeftDockWidgetArea, tree_dock)

    dialog_2d = {"instance": None}

    def open_2d_dialog():
        if dialog_2d["instance"] is None:
            dialog_2d["instance"] = Slice2DDialog(main_win, volume_dim, volume_data, colormap_lut, clim)
        dialog_2d["instance"].show()
        dialog_2d["instance"].raise_()
        dialog_2d["instance"].activateWindow()

    main_win.addToolBar(Qt.TopToolBarArea, build_slices_toolbar(main_win, render_window, open_2d_dialog))

    status_bar = main_win.statusBar()
    fps_state = {"last_time": time.perf_counter(), "frames": 0}

    def tick():
        render_window.render_frame()
        edge_labels.update_positions()
        fps_state["frames"] += 1
        now = time.perf_counter()
        elapsed = now - fps_state["last_time"]
        if elapsed >= 0.5:
            fps = fps_state["frames"] / elapsed
            status_bar.showMessage(f"{fps:.1f} FPS")
            fps_state["frames"] = 0
            fps_state["last_time"] = now

    timer.timeout.connect(tick)
    timer.start(0)  # dispara o quanto antes o loop de eventos ficar livre

    main_win.show()
    sys.exit(app.exec_())


if __name__ == "__main__":
    main()
