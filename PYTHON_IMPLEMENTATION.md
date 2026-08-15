# Implementação Python — como integrar o Nebula no Andromeda

Guia prático pro lado Python (Andromeda). O `PLANO.md` documenta a evolução e a arquitetura do
motor em si; este arquivo documenta **como usá-lo corretamente** — o que aprendemos construindo
`python/embed_test.py` (o protótipo de validação), organizado pra quem for integrar de verdade
no módulo `src/core/visualization/seismic/` do Andromeda, substituindo o VisPy.

`python/embed_test.py` é o protótipo — vale como referência de código funcionando, mas não é pra
copiar direto: ele tem stand-ins (árvore de objetos falsa, dataset sintético) que não existem no
Andromeda real. Este documento explica os *padrões*, não repete o protótipo linha por linha.

## 1. Build e instalação

O Nebula é um módulo de extensão Python (`import nebula`) construído com `maturin` a partir do
crate `nebula/`.

```bash
cd nebula
python -m venv .venv          # uma vez só
.venv\Scripts\activate        # SEMPRE ativar antes de buildar
maturin develop --release
```

**Armadilha conhecida**: `maturin develop` sem uma venv ativa pode silenciosamente resolver pro
Python de sistema errado (ex: builda pra cp312 quando o processo real roda cp313). Sempre ativar
a venv explicitamente antes — não confiar em detecção automática.

No Andromeda de produção, isso vira uma dependência normal do ambiente do projeto (wheel
buildado no CI, ou instalado como dependência local editável) — o passo manual acima é só pro
ciclo de desenvolvimento do Nebula em si.

## 2. Mecanismo de embutimento (obrigatório, não é opcional)

O Nebula **não** usa `winit`. Ele espera um HWND (Windows) já existente, emprestado do Qt, e cria
a `wgpu::Surface` diretamente nele. O padrão testado e funcional:

```python
from PyQt5.QtGui import QWindow
from PyQt5.QtWidgets import QWidget
import nebula

class NebulaWindow(QWindow):
    def __init__(self, mode="orbit"):
        super().__init__()
        self.setSurfaceType(QWindow.OpenGLSurface)  # impede o Qt de pintar por cima
        self._mode = mode
        self._renderer = None

    def _ensure_renderer(self):
        if self._renderer is None:
            hwnd = int(self.winId())  # só válido depois que a janela nativa existe
            self._renderer = nebula.Renderer(hwnd, max(self.width(), 1), max(self.height(), 1), self._mode)
        return self._renderer

    def resizeEvent(self, event):
        renderer = self._ensure_renderer()
        renderer.resize(max(self.width(), 1), max(self.height(), 1))
        super().resizeEvent(event)
```

```python
render_window = NebulaWindow(mode="orbit")
container = QWidget.createWindowContainer(render_window, parent_widget)
layout.addWidget(container)
```

Pontos que **têm** que ser respeitados:

- **`winId()` só é válido depois do primeiro resize real.** Não dá pra criar o `Renderer` no
  `__init__` — a janela nativa por trás da `QWindow` ainda não existe nesse ponto. Por isso o
  `Renderer` é sempre criado sob demanda (`_ensure_renderer`), tipicamente disparado pelo
  primeiro `resizeEvent`.
- **Estado "pendente" pra dados que chegam antes do Renderer existir.** Se o código chamar
  `set_volume`/`add_slice`/etc. antes da janela ter aparecido na tela, guarde os argumentos e
  aplique no primeiro `render_frame()` depois que `_ensure_renderer()` já devolveu um objeto de
  verdade. Não dá pra simplesmente esperar — a UI precisa poder configurar a cena a qualquer
  momento, inclusive antes do primeiro `show()`.
- **Teardown correto — usar `closeEvent`, não `aboutToQuit`.** `aboutToQuit` do `QApplication`
  dispara tarde demais: o Qt já pode ter começado a destruir a `QWindow` embutida, e qualquer
  chamada ao `Renderer` depois disso (um `resizeEvent` tardio, por exemplo) pode tocar um HWND
  inválido. O padrão certo é uma flag setada no `closeEvent` da janela principal:

  ```python
  class MainWindow(QMainWindow):
      def closeEvent(self, event):
          self._timer.stop()
          self._render_window.shutdown()  # seta _closed = True
          super().closeEvent(event)
  ```

  E `_ensure_renderer()` sempre checa `if self._closed: return None` antes de qualquer coisa.
- **Erros de validação do wgpu não devem derrubar o processo.** Isso já é tratado dentro do
  Rust (`device.on_uncaptured_error` loga em vez de fazer `panic!`), então o lado Python não
  precisa de try/except especial pra isso — mas vale saber que mensagens
  `[nebula] erro wgpu não capturado (ignorado): ...` no console durante o fechamento da janela
  são esperadas e inofensivas (Surface stale durante o teardown do Qt), não indicam bug.

## 3. Render loop

Um `QTimer` dirige o desenho — o Nebula não tem loop de eventos próprio, ele só desenha quando
mandado:

```python
timer = QTimer()
timer.timeout.connect(render_window.render_frame)
timer.start(0)  # dispara assim que o loop de eventos do Qt ficar livre
```

`render_frame()` é onde o padrão "pendente" da seção 2 se resolve — aplica qualquer
volume/colormap/clim/fatia que ainda não foi mandado pro Rust, e só então chama
`renderer.render()`. Ver `NebulaWindow.render_frame` em `embed_test.py` pro padrão completo.

## 4. API do `Renderer` — referência e convenções

### Câmeras

- `mode="orbit"`: câmera orbital 3D (azimute/elevação/distância), com iluminação (headlight —
  a luz acompanha a câmera, não é fixa no mundo). Pro canvas 3D principal.
- `mode="panzoom"`: câmera ortográfica 2D (só pan/zoom, sem rotação), sem iluminação (a seção 2D
  é leitura de dado, não superfície *lit*). Pro diálogo de seção 2D.

Cada `Renderer` tem seu próprio `wgpu::Device` — **não há compartilhamento de GPU entre
instâncias**. Se o Andromeda precisar de uma visão 3D e um diálogo 2D simultâneos (o caso comum:
usuário abre "Ver em 2D" com o cubo 3D ainda aberto), cada um é um `Renderer` separado, e os
mesmos arrays numpy (já em memória Python) são enviados pros dois. Isso duplica um pouco de VRAM,
mas evita a complexidade real de compartilhar texturas entre `Device`s diferentes — decisão
deliberada, não vale a pena resolver isso agora.

### Volumes e fatias

```python
renderer.add_volume(id, width, height, depth, data)       # data: PyBuffer<f32> C-contíguo
renderer.set_volume_colormap(id, rgba, discrete=False)     # rgba: PyBuffer<u8> (N,4)
renderer.set_volume_clim(id, min, max)
renderer.set_volume_opacity(id, opacity)                   # 0..1, padrão 1.0 (opaco)
renderer.remove_volume(id)

renderer.add_slice(slice_id, volume_id, axis, index)        # axis: 0=Inline,1=Crossline,2=Time; index: 0..1
renderer.set_slice_axis_index(slice_id, axis, index)
renderer.set_slice_visible(slice_id, visible)
renderer.remove_slice(slice_id)
```

Convenções que o Rust **exige** e não valida magicamente:

- `data` do volume precisa ser C-contíguo, `float32`, com exatamente `width*height*depth`
  elementos, na ordem **(inline, xline, amostra)** — mesma ordem de array que o HDF5 do
  Andromeda já usa hoje, não precisa transpor nada.
- `rgba` do colormap precisa ser `uint8`, shape `(N, 4)`, C-contíguo. O Nebula **não sabe nada**
  sobre matplotlib/VisPy/Petrel/Paradigm — quem resolve isso é o Python, amostrando qualquer
  colormap (nome do matplotlib, ou um `vispy.color.Colormap` já convertido de Petrel/Paradigm no
  Andromeda) em N pontos fixos:

  ```python
  def build_colormap_lut(cmap, resolution=256):
      import matplotlib
      c = matplotlib.colormaps[cmap] if isinstance(cmap, str) else cmap
      samples = c(np.linspace(0.0, 1.0, resolution))
      return np.ascontiguousarray(samples * 255.0, dtype=np.uint8)
  ```

- `discrete=True` é pra fácies/classes categóricas (sampler vira `Nearest`, sem interpolar entre
  cores vizinhas — degrau, não gradiente). `discrete=False` (padrão) é pra sísmica/atributos
  contínuos.
- `axis`: `0=Inline, 1=Crossline, 2=Time` — **mesma convenção exata** do `AXIS_CONFIG` do
  `Slice2DDialog` atual do Andromeda. Não precisa de tradução.
- `index` é **normalizado (0..1)**, não o índice inteiro real (ex: inline 970). Quem converte é
  o Python: `index = (inline_idx) / (n_inlines - 1)`. Isso é responsabilidade da UI que já sabe
  os limites do survey (o mesmo cálculo que `Slice2DDialog._get_display_position` já faz hoje,
  só invertido).
- Vários volumes e várias fatias por volume podem coexistir ao mesmo tempo (ex: sísmica +
  atributo elástico, cada um com sua Inline/Crossline/Time simultâneas) — não há limite
  artificial, é só um `HashMap` do lado Rust.

### Câmera e interação

```python
renderer.orbit(dx, dy)   # botão esquerdo (só afeta mode="orbit")
renderer.pan(dx, dy)     # botão do meio
renderer.zoom(delta)     # botão direito / scroll
```

O Nebula **não tem sistema de input próprio** — o Qt já possui o loop de eventos e recebe os
eventos de mouse na `QWindow`; o lado Python só encaminha deltas pros métodos acima. Isso é
proposital: duplicar um sistema de input dentro do Rust não teria vantagem nenhuma aqui.

### Mover fatias interativamente

Dois padrões, os dois válidos e complementares:

1. **Fluxo explícito (igual o Andromeda já tem)**: uma combobox de eixo **seleciona qual fatia
   está ativa** — ela sozinha não move nada. Um botão "Mover" liga/desliga um modo em que
   arrastar o mouse move a fatia ativa. Do lado Python isso é: guardar
   `active_slice_id` (setado pela combobox) e um flag `move_mode` (setado pelo botão), e no
   `mouseMoveEvent`, se `move_mode` estiver ligado, chamar `renderer.nudge_slice(active_slice_id,
   dx, dy)` em vez de `orbit`/`pan`.
2. **Atalho por hover (não existe no Andromeda hoje, mas não atrapalha, é aditivo)**: segurar
   Ctrl e arrastar em cima de **qualquer** fatia da cena move ela, sem precisar selecionar nada
   antes. Implementado com `renderer.pick_slice(screen_x, screen_y)` chamado no
   `mousePressEvent` assim que Ctrl é detectado (`event.modifiers() & Qt.ControlModifier`),
   guardando o id devolvido; o `mouseMoveEvent` usa esse id em vez do `active_slice_id` enquanto
   Ctrl estiver segurado.

```python
new_index = renderer.nudge_slice(slice_id, dx, dy)  # dx,dy = delta do mouse em pixels desde o último evento
```

`nudge_slice` projeta a direção real de movimento da fatia (em coordenadas de mundo) pra tela
sob a câmera atual, e usa a componente do arrasto do mouse alinhada com essa direção — arrastar
"ao longo" do eixo do grid, do jeito que ele aparece na tela àquele ângulo de câmera, avança a
fatia de verdade; arrastar perpendicular a ele quase não move nada. Isso só faz sentido na
câmera orbital 3D — na visão 2D ortográfica, o eixo de movimento aponta pra dentro da tela
(sem direção projetável em 2D), então lá o padrão certo é usar `dy` direto:

```python
if mode == "orbit":
    new_index = renderer.nudge_slice(slice_id, dx, dy)
else:  # panzoom
    new_index = clamp(current_index + dy * SENSITIVITY, 0.0, 1.0)
    renderer.set_slice_axis_index(slice_id, axis, new_index)
```

**Importante ao testar essa interação**: automação de mouse via Win32
(`mouse_event`/`keybd_event`) não entrega o modificador Ctrl de forma confiável ao
`mousePressEvent` do Qt — não é bug do Nebula, é uma limitação da automação. Pra testar
picking/modificadores de verdade, construa um `QMouseEvent` diretamente com o modificador
desejado e chame o handler:

```python
from PyQt5.QtGui import QMouseEvent
event = QMouseEvent(QMouseEvent.MouseButtonPress, pos, Qt.LeftButton, Qt.LeftButton, Qt.ControlModifier)
render_window.mousePressEvent(event)
```

Isso exercita exatamente o mesmo caminho de código que um usuário real dispara, sem depender de
nenhuma sincronização de SO.

## 5. Texto e overlays sobre o canvas (labels, colorbar, HUD)

**Texto é nativo do Nebula** — não peça ao Qt pra desenhar texto posicionado no espaço 3D. O
Rust tem uma fonte bitmap embutida (sem nenhuma dependência externa) e desenha os labels como
geometria de verdade (billboards sempre de frente pra câmera), via `add_text_label`:

```python
renderer.add_text_label(
    id, x, y, z,      # posição no mundo (convenção do cubo -1..1: X=Inline, Y=Time, Z=Crossline)
    "IL 970\nXL 1650", # \n quebra linha
    0.5, 0.83, 1.0,    # cor (r, g, b), 0..1
    0.14,              # escala
)
```

Uma vez adicionado, o label acompanha orbit/pan/zoom **sozinho** — não precisa recalcular
posição de tela a cada frame do lado Python, porque não é um overlay 2D, é geometria real da
cena. `remove_text_label(id)`, `set_text_label_visible(id, bool)` e
`set_text_label_position(id, x, y, z)` completam a API. Fonte cobre A-Z, 0-9 e pontuação básica
(`- . : / _` e espaço); minúsculas são tratadas como maiúsculas automaticamente.

Isso cobre labels de eixo, nome de poço, qualquer anotação 3D — não é preciso (nem recomendado)
recriar o padrão de overlay Qt abaixo pra texto. A colorbar continua sendo `QPainter` puro (ver
`ColorbarWidget`), porque ela fica **ao lado** do canvas no layout, não por cima — não é overlay.

### Se algum dia precisar de um overlay Qt de verdade (não-texto) sobre o canvas

`Renderer.project_to_screen(x, y, z)` (mundo → tela) ainda existe pra esse caso raro — algo que
precise ser um `QWidget` interativo de verdade posicionado sobre a cena 3D (não é o caso de
texto, que já é nativo). Duas armadilhas documentadas aqui porque custaram tempo real de debug:

**`createWindowContainer` embute uma janela nativa de verdade.** Widgets Qt comuns, mesmo sendo
filhos diretos do widget container, **não compõem visualmente por cima dela** — limitação
conhecida do Qt. A saída "óbvia" seria `widget.setAttribute(Qt.WA_AlwaysStackOnTop)`, e ela até
resolve visualmente... **mas derruba o processo** nesta combinação específica (`wgpu::Surface`
criada a partir de um HWND cru + Qt tentando promover o widget a janela nativa própria — conflito
não totalmente diagnosticado, mas reproduzido de forma consistente). Se algum overlay não-texto
for mesmo necessário, use uma janela `Tool` separada em vez disso (sempre no topo, sem foco,
`WA_TransparentForMouseEvents` pra não atrapalhar orbit/pan/zoom por baixo, reposicionada
manualmente pra cobrir o canvas a cada frame via `container.mapToGlobal(QPoint(0,0))`) — técnica
que funcionou sem crash quando os labels de eixo ainda eram Qt (antes de virarem nativos), mas
hoje só vale a pena se for um widget interativo de verdade, não texto estático.

**`PrintWindow` (Win32) só captura o conteúdo de um HWND específico** — não vê outras janelas de
nível superior compostas por cima dela pelo Windows (o caso da janela-`Tool` acima). Se estiver
validando algo assim via captura de tela automatizada e a feature parecer "invisível" mesmo sem
erro nenhum, confirme com uma captura de tela real (`Graphics.CopyFromScreen`) antes de assumir
bug. Não se aplica a texto nativo — ele é geometria normal dentro do mesmo HWND do canvas,
captura em qualquer método de screenshot.

## 6. Mapeamento pros managers existentes do Andromeda

Não é objetivo deste documento reescrever `VolumeSlicesManager`/`HorizonManager`/
`Well3DPlotManager` — é mapear o que cada um faz hoje pro que ele vai chamar no Nebula:

| Manager atual | Método(s) Vispy hoje | Chamada Nebula equivalente |
|---|---|---|
| `VolumeSlicesManager` | cria/atualiza `AxisAlignedImage` por dataset/eixo | `add_volume` (uma vez por dataset) + `add_slice`/`set_slice_axis_index` (uma vez por eixo visível) |
| Colorbar (via `ColorbarManager`) | raster matplotlib, replota a cada mudança | `ColorbarWidget` (Qt puro), só chama `.update()` |
| `HorizonManager.add_horizon_surface_to_canvas` | `scene.visuals.Mesh` com cor assada via matplotlib | `add_horizon_picks`/`add_horizon_grid` (Fase 5) — cor vem do colormap em shader, não mais assada |
| `Well3DPlotManager.add_well_plot` | `WellSceneRenderer.add_trajectory`/`add_log`/`add_head` | `add_well_trajectory`/`add_well_log` (Fase 5) — mesma tesselação `cigvis`, upload muda |
| `Slice2DDialog` | `scene.visuals.Image` + `PanZoomCamera` do VisPy | segunda `NebulaWindow(mode="panzoom")`, mesmo volume re-enviado |

A ideia geral: os managers continuam existindo e continuam donos da lógica de domínio (que
dataset carregar, que checkbox está marcada, que clim usar) — só a chamada final de
"desenhar isso" troca de um visual VisPy pra um método do `Renderer`.

## 7. O que ainda não existe (não tentar usar)

- Horizontes e poços — API planejada em detalhe no `PLANO.md` (Fase 5), ainda não implementada
  no Rust.
- Linhas arbitrárias (seção não-axis-aligned).
- Streaming de volumes grandes — `add_volume` hoje espera o array inteiro em memória. Dados
  reais de survey (GBs) vão precisar do trabalho de paginação da Fase 6 antes de irem pra
  produção.
- Ray marching / transfer functions pro volume 3D cheio (fácies, corpo geológico) — só as
  fatias axis-aligned existem hoje.
- Bússola/indicador de eixo XYZ como HUD.
- Sistema de coordenadas do survey real (offset/escala por volume, quadrantes de azimute,
  exagero vertical) — o cubo hoje é sempre um cubo unitário `-1..1`; a conversão de coordenadas
  reais de survey pra essa faixa normalizada é responsabilidade do Python por enquanto, não tem
  suporte nativo no Nebula ainda.
