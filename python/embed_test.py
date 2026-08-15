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

import numpy as np
from PyQt5.QtCore import Qt, QTimer
from PyQt5.QtGui import QWindow
from PyQt5.QtWidgets import (
    QAction,
    QApplication,
    QComboBox,
    QDockWidget,
    QMainWindow,
    QToolBar,
    QTreeWidget,
    QTreeWidgetItem,
    QWidget,
)

import nebula


def build_synthetic_volume(width=64, height=64, depth=64):
    """Volume escalar sintético (gradiente + xadrez 3D) só pra provar o caminho
    de upload Python -> textura 3D — sem ligação com dados sísmicos reais ainda.

    Layout: array numpy C-contíguo de shape (depth, height, width), que é
    exatamente a ordem de bytes que `Renderer.load_volume` espera (x/width é o
    eixo que varia mais rápido na memória).
    """
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
    """

    def __init__(self):
        super().__init__()
        self.setSurfaceType(QWindow.OpenGLSurface)
        self._renderer = None
        self._closed = False
        self._drag_button = None
        self._last_pos = None
        self._pending_volume = None

    def set_volume(self, width: int, height: int, depth: int, data: np.ndarray):
        # O Renderer só existe depois do primeiro resize real (winId() só é
        # válido depois que a QWindow tem uma janela nativa por trás). Guardamos
        # o volume e mandamos pro Rust assim que o Renderer estiver pronto.
        self._pending_volume = (width, height, depth, data)

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
            self._renderer = nebula.Renderer(hwnd, max(self.width(), 1), max(self.height(), 1))
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
            renderer.load_volume(width, height, depth, data)
            self._pending_volume = None

        renderer.render()

    def mousePressEvent(self, event):
        self._drag_button = event.button()
        self._last_pos = event.pos()

    def mouseReleaseEvent(self, event):
        self._drag_button = None
        self._last_pos = None

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


def build_slices_toolbar(main_win: QMainWindow) -> QToolBar:
    """Réplica simplificada da slicesToolBar do Andromeda (seletor de eixo +
    combobox de exagero vertical)."""
    toolbar = QToolBar("Slices", main_win)

    axis_combo = QComboBox()
    axis_combo.addItems(["Inline", "Crossline", "Time"])
    toolbar.addWidget(axis_combo)

    toolbar.addSeparator()

    scale_combo = QComboBox()
    scale_combo.addItems(["25%", "50%", "100%", "200%", "500%"])
    scale_combo.setCurrentText("100%")
    toolbar.addWidget(scale_combo)

    toolbar.addSeparator()
    toolbar.addAction(QAction("Reset View", main_win))

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

    volume_dim = 64
    volume_data = build_synthetic_volume(volume_dim, volume_dim, volume_dim)
    render_window.set_volume(volume_dim, volume_dim, volume_dim, volume_data)

    tree_dock = QDockWidget("Object Tree", main_win)
    tree_dock.setWidget(build_object_tree())
    main_win.addDockWidget(Qt.LeftDockWidgetArea, tree_dock)

    main_win.addToolBar(Qt.TopToolBarArea, build_slices_toolbar(main_win))

    status_bar = main_win.statusBar()
    fps_state = {"last_time": time.perf_counter(), "frames": 0}

    def tick():
        render_window.render_frame()
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
