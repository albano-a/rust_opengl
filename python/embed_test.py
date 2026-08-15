"""Protótipo mínimo da Fase 1: triângulo renderizado via wgpu (módulo `nebula`),
embutido numa QWindow do PyQt5 através de createWindowContainer.

Inclui um dock de árvore de objetos e uma toolbar como stand-ins para os widgets reais
que cercam o canvas 3D no Andromeda (svwObjectTreeWidget / slicesToolBar), só pra
observar se o compositing extra do Qt afeta o FPS do canvas wgpu.

Rodar de dentro de nebula/.venv (com `nebula` instalado via `maturin develop`):
    python ../python/embed_test.py
"""

import sys
import time

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
        if renderer is not None:
            renderer.render()


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
    main_win.setWindowTitle("Andromeda + Nebula (protótipo Fase 1)")
    main_win.resize(1100, 700)

    container = QWidget.createWindowContainer(render_window, main_win)
    main_win.setCentralWidget(container)

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
