# Plano — Nebula: Motor de Visualização Sísmica 3D

## Objetivo final

**Nebula**: motor gráfico em Rust (via `wgpu`) para visualização sísmica 3D, exposto como
módulo de extensão Python (via `PyO3`) e embutido dentro de um `QWidget` do PyQt5, para uso
dentro do Andromeda. Substitui o motor atual baseado em VisPy (`src/core/visualization/seismic/`
no repo do Andromeda), com o objetivo de ter controle fino de pipeline e acesso a compute
shaders nativos — coisa que VisPy (OpenGL genérico via Python) não oferece.

## Decisão de stack

| Opção | Veredito | Motivo |
|---|---|---|
| OpenGL puro | Descontinuado após o protótipo inicial | Bom pra aprender o pipeline básico, mas sem compute shaders nativos — ruim pra volume rendering |
| **wgpu** | **Escolhido** | Baixo nível, cross-platform, compute shaders nativos (essencial pra ray marching/LOD de volumes), aceita `raw-window-handle` diretamente |
| Bevy | Descartado | ECS e convenções de engine atrapalham controle fino de um pipeline customizado (raycasting, transfer functions) |
| `qmetaobject-rs` | Descartado | Só suporta QML, não QWidget |
| `cxx-qt` | **Descartado** | Gera bindings Rust ↔ **C++**. O host real é **Python/PyQt5**, não C++/Qt — `cxx-qt` não se aplica aqui |
| **PyO3 + `maturin`** | **Escolhido (lado Python)** | Nebula vira um módulo de extensão Python nativo (`import nebula`), packaging via `maturin develop`/`pip install`, API ergonômica pro lado Python |

## Estado atual (resumo — detalhe fase a fase abaixo)

Fases 1–4 concluídas. O crate `nebula/` expõe uma classe `Renderer` com esta API:

| Método | Pra quê |
|---|---|
| `Renderer(hwnd, width, height, mode)` | `mode="orbit"` (3D, com luz) ou `"panzoom"` (2D, sem luz) |
| `resize(w, h)` | reconfigura a `Surface` e o aspect da câmera |
| `orbit(dx, dy)` / `pan(dx, dy)` / `zoom(delta)` | controle de câmera (orbit só afeta o modo `"orbit"`) |
| `add_volume(id, w, h, d, data)` / `remove_volume(id)` | textura 3D por id, ordem (inline, xline, amostra) |
| `set_volume_colormap(id, rgba, discrete)` / `set_volume_clim(id, min, max)` / `set_volume_opacity(id, opacity)` | LUT contínua ou discreta (fácies), faixa de valores, opacidade (padrão 1.0) |
| `add_slice(slice_id, volume_id, axis, index)` / `remove_slice(id)` / `set_slice_visible(id, bool)` / `set_slice_axis_index(id, axis, index)` | fatias (`AxisAlignedImage`) posicionadas de verdade no espaço 3D do cubo; várias por volume, todas simultâneas |
| `nudge_slice(id, screen_dx, screen_dy) -> index` | move uma fatia arrastando o mouse, projetando o eixo de movimento real na tela |
| `pick_slice(screen_x, screen_y) -> Optional[id]` | descobre qual fatia está embaixo do cursor (ray-cast) |
| `project_to_screen(x, y, z) -> Optional[(sx, sy)]` | mundo → tela, pra overlays Qt que não sejam texto |
| `add_text_label(id, x, y, z, text, r, g, b, scale)` / `remove_text_label(id)` / `set_text_label_visible(id, bool)` / `set_text_label_position(id, x, y, z)` | texto GPU nativo (billboard, fonte bitmap embutida) — sem Qt, sem overlay |
| `configure_axis_grid(width, height, depth)` | liga o grid numerado dos 3 eixos (INLINE/CROSSLINE/TIME, valores reais) — o Nebula escolhe sozinho, a cada frame, em qual aresta do cubo cada eixo aparece (a que está de costas pra câmera), igual um eixo 3D de matplotlib |
| `render()` | desenha um frame |

Convenção espacial fixa (cubo unitário -1..1, usada por `add_slice`/picking/labels): mundo
**X = Inline, Y = Time (topo = raso), Z = Crossline**.

Ver [`PYTHON_IMPLEMENTATION.md`](PYTHON_IMPLEMENTATION.md) pra como integrar isso no Andromeda de
verdade (o `python/embed_test.py` é só o protótipo de validação, não código pra copiar direto).

### Por que não `cxx-qt`

O plano original assumia um host em C++/Qt. Mas a aplicação host real é o **Andromeda**, que é
Python/PyQt5. `cxx-qt` não fala com PyQt5 (que usa bindings próprios via SIP). Então o Nebula
é uma `cdylib` comum construída com PyO3, sem nenhuma dependência de C++/Qt do lado Rust.

## Precedente encontrado no próprio Andromeda

O módulo sísmico atual (`vispy_3D_visualization_controller.py`) embute o canvas VisPy via
`canvas.native` (um `QOpenGLWidget`) direto num `QMdiSubWindow`/layout — sem `winId()`, sem
`createWindowContainer`.

Só que existe um **segundo backend no mesmo repo**, não usado pelo módulo sísmico, mas que é
o precedente estrutural mais próximo do que o Nebula vai precisar:
`core/helpers/widgets/adma_opengl_window.py` (usado no módulo de logs de poço). Ele troca
`QOpenGLWidget` por uma `QOpenGLWindow` (uma `QWindow` de verdade) embrulhada com:

```python
embedded = QWidget.createWindowContainer(native, host)
layout.addWidget(embedded)
```

Esse é o mecanismo de encaixe que o Nebula vai usar: uma `QWindow` no lado Python, embrulhada
via `createWindowContainer`, cujo handle nativo (HWND no Windows) é passado pro Rust — que cria
a `wgpu::Surface` diretamente nele, sem depender do `winit`. O `winit` some do caminho de
produção: quem possui a janela é o Qt, o Rust só recebe um handle emprestado.

## Risco arquitetural principal

O maior risco do projeto não é o shader de volume — é o **encaixe wgpu dentro de um QWidget do
PyQt5**. Por isso esse encaixe deve ser validado **antes** de qualquer investimento em técnicas
de visualização sísmica.

Mecanismo:
1. Python cria uma `QWindow` (com `setSurfaceType(QWindow.OpenGLSurface)` ou equivalente, pra
   impedir o Qt de tentar pintar por cima com seu próprio backing store).
2. Python chama `QWidget.createWindowContainer(window)` pra encaixar essa `QWindow` na árvore
   de widgets (`QMainWindow`, layout, etc.) — o Qt cede esse pedaço da UI como um "buraco".
3. Python pega o handle nativo da `QWindow` (`int(window.winId())`) e chama o construtor do
   `nebula.Renderer(hwnd, width, height)`.
4. Rust constrói manualmente um `raw_window_handle::Win32WindowHandle` a partir desse HWND
   (sem `winit` — não há `Window` do lado Rust, só um handle emprestado) e cria a
   `wgpu::Surface` via `create_surface_unsafe`.
5. Quem desenha dentro da `QWindow` é o wgpu via swapchain própria, sem passar pelo pipeline
   de pintura do Qt. Um `QTimer` (ou callback de eventos) do lado Python dispara
   `renderer.render()` periodicamente.

## Fase 1 — Protótipo de encaixe (concluída)

- [x] Migrar o triângulo standalone de `glutin`/`gl` para `wgpu` (janela própria via `winit`,
      valida só o pipeline gráfico, sem Qt) — mantido como `triangle_standalone/`, útil como
      referência/sanity check isolado do resto.
- [x] Criar o crate `nebula/` (PyO3, `crate-type = ["cdylib"]`), expondo uma classe
      `Renderer(hwnd: int, width: int, height: int)` com `.resize(w, h)` e `.render()`
  - `wgpu::Surface` criada via `create_surface_unsafe` a partir de um `Win32WindowHandle`
    construído manualmente (não vem de nenhum `winit::Window`)
  - Mesmo pipeline/shader do triângulo (critério de sucesso: mesmo resultado visual)
- [x] Script Python mínimo (PyQt5): `QWindow` → `createWindowContainer` → `nebula.Renderer`
      dirigido por `QTimer` (`python/embed_test.py`)
- [x] Validar resize: redimensionar a janela Qt reconfigura a `Surface` corretamente
- [x] Validar ciclo de vida: fechar/reabrir a janela não crasha nem vaza recursos

**Critério de saída da Fase 1**: triângulo desenhado via wgpu, visível e redimensionável,
dentro de uma janela PyQt5 real (via `createWindowContainer`), sem intervenção do pipeline de
pintura do Qt, chamável como `import nebula`.

## Fase 2 — Fundamentos wgpu: MVP e câmera orbital (concluída)

- [x] Uniform buffer + bind group pra matriz `view_proj` (`camera_bind_group`, `@group(0)`)
- [x] Geometria de teste com profundidade real: cubo indexado (`nebula/src/geometry.rs`),
      substituindo o triângulo 2D em clip-space da Fase 1
- [x] Depth buffer (`Depth32Float`) + `DepthStencilState`, recriado a cada resize
- [x] Câmera orbital (`nebula/src/camera.rs`, `OrbitCamera`) equivalente ao
      `MiddlePanTurntableCamera` do VisPy: parametrizada por azimute/elevação/distância/alvo
  - `orbit(dx, dy)` — botão esquerdo
  - `pan(dx, dy)` — botão do meio
  - `zoom(delta)` — botão direito / scroll
- [x] Input plugado do lado Python (`python/embed_test.py`): o Qt já possui o event loop e
      recebe os eventos de mouse na `QWindow`, então os handlers em Python só encaminham
      deltas pros métodos do `Renderer` — sem duplicar um sistema de input dentro do Rust
- [ ] Inércia de rotação (fica pra uma passada de polimento futura, não bloqueia as próximas fases)

**Critério de saída**: cubo colorido por vértice, com profundidade correta, giro/pan/zoom
controláveis por mouse dentro da janela PyQt5 — validado manualmente (drag esquerdo, meio,
scroll).

## Fase 3 — Textura 3D e upload de volume (concluída)

- [x] `nebula/src/volume.rs`: `Volume3D` — cria a textura 3D (`R32Float`), faz upload via
      `queue.write_texture` e monta o bind group (textura + sampler)
- [x] Sampling sempre `Nearest` (ver nota abaixo — sampling linear de `R32Float` mostrou um
      bug visual real, não só uma questão de suporte do adapter)
- [x] `Renderer::load_volume(width, height, depth, data)` — recebe o volume do lado Python via
      `pyo3::buffer::PyBuffer<f32>` (protocolo de buffer do numpy, sem depender da crate
      `numpy`/rust-numpy — evita mais uma dependência bleeding-edge pra rastrear)
- [x] Geometria de teste trocada: cubo da Fase 2 saiu, entrou um quad (`SliceVertex`) que
      amostra a fatia do meio (`w = 0.5`) do volume 3D — usa a mesma câmera/MVP da Fase 2,
      então dá pra orbitar e confirmar que está posicionado de verdade em 3D
- [x] Volume sintético de teste em `python/embed_test.py` (`build_synthetic_volume`):
      gradiente + xadrez 3D gerado com numpy, sem dependência de dados reais ainda

**Critério de saída**: padrão sintético gerado em Python aparece corretamente na fatia
renderizada — confirmado visualmente (usuário testou orbit/pan/zoom e viu o xadrez).

**Bug real encontrado e corrigido**: a primeira versão usava sampling `Linear` quando o
adapter suportava `FLOAT32_FILTERABLE`, e o resultado renderizado era um gradiente liso sem
nenhum traço do xadrez — não um blur sutil nas bordas dos blocos, o padrão inteiro sumia.
Isolei a causa comparando `textureSample` (sampler) com `textureLoad` (leitura direta por
texel, sem sampler): `textureLoad` mostrava o xadrez perfeito, provando que o dado chegava
certo na textura — o problema era só no sampler linear. Não cheguei à causa raiz exata dentro
do wgpu 30 (candidato mais provável: alguma interação de LOD/mip clamp com o ângulo de visão
bem inclinado da câmera orbital, já que só há 1 nível de mip), mas em vez de investigar mais
fundo numa API tão recente, simplifiquei: sampling de volume agora é sempre `Nearest`,
independente do adapter. Faz sentido além de contornar o bug — amplitude sísmica amostrada não
deveria ser interpolada silenciosamente entre vizinhos de qualquer forma.

**Fora de escopo desta fase** (fica pras seguintes): leitura de HDF5 real (isso continua sendo
trabalho do lado Python/Andromeda), múltiplas fatias ortogonais simultâneas, ray marching e
streaming/paginação de volumes grandes.

## Fase 3.5 — Iluminação dinâmica (concluída)

Decidida antes da Fase 4: iluminação de rasterização clássica (nada de ray tracing — pesado
demais pro objetivo aqui, que é clareza dos dados, não fotorrealismo, e `wgpu` ainda tem
suporte a ray tracing só experimental).

Primeira versão tinha dois problemas encontrados em revisão visual (comparando com
referências reais do Petrel/Ocean): o objeto girava sozinho (nunca foi pedido, foi uma
suposição errada) e a luz era fixa no mundo, o que deixa faces do objeto escuras dependendo do
ângulo — errado pra uma ferramenta onde o objetivo é conseguir *ler* a fatia sísmica de
qualquer ângulo, não simular iluminação realista. Corrigido pra:

- [x] Objeto **parado** — só a câmera orbita (igual Fase 2). `model` na `SceneUniform` fica
      sempre `Mat4::IDENTITY` por enquanto (a infraestrutura de model matrix continua útil pra
      Fase 5, só não gira sozinha).
- [x] Luz vira **headlight**: `light_position` recalculada todo frame como `camera.eye()`, em
      vez de uma posição fixa no mundo. Como a luz acompanha a câmera, o lado que você está
      olhando fica sempre bem iluminado — e isso já basta pra difusa "reagir" ao orbitar,
      sem precisar girar o objeto.
- [x] Normal de face dupla no `volume_slice.wgsl`: como o quad é um plano sem espessura (só
      uma normal por vértice), a normal é virada pro lado do observador
      (`dot(normal, view_dir) < 0.0 → normal = -normal`) antes de calcular a difusa — sem isso,
      olhar o objeto "por trás" apagava a face errada.
- [x] `SliceVertex` ganhou `normal: [f32; 3]` (constante `(0,0,1)` pro quad plano — superfícies
      curvas da Fase 5 vão precisar de normais de verdade por vértice)
- [x] `SceneUniform` (`nebula/src/lib.rs`): um bind group só (`@group(0)`) juntando
      `view_proj` + `model` + `light_position` + `camera_position`
- [x] Shading no `volume_slice.wgsl`: `albedo * (ambient + diffuse)` — só Lambertian, sem
      especular. Testamos com Blinn-Phong primeiro, mas com a luz colada na câmera o brilho
      especular fica sempre grudado bem no centro da tela (parece plástico/molhado) — errado
      pra uma fatia sísmica, que é fosca. `albedo` continua sendo o valor amostrado do volume.

**Critério de saída**: confirmado visualmente — capturas seguidas sem interação são idênticas
(objeto não gira sozinho); depois de orbitar a câmera, a superfície fica visivelmente mais
clara/legível na nova direção de visão, com o gradiente de sombra mudando de lado — luz
seguindo a câmera, exatamente o comportamento do Petrel usado como referência.

## Fase 3.7 — Colormap contínuo (concluída)

Escopo: só colormap **contínuo** (amplitude sísmica, atributos elásticos). LUT **categórica**
(facies, código→cor exata) fica de fora — isso é resolvido no lado VisPy do LogPlot Vision, que
continua existindo separado do Nebula.

- [x] `nebula/src/colormap.rs`: `Colormap` — textura 1D `Rgba8Unorm` (filtrável em qualquer
      hardware, sem a dor de cabeça do `FLOAT32_FILTERABLE` que tivemos com o volume na Fase 3),
      sampler `Linear` (colormap contínuo deve interpolar), e um uniform de `clim` (`vec4`,
      só `x`/`y` usados) — tudo num bind group próprio (`@group(2)`), **separado** do bind group
      do volume: trocar de colormap ou ajustar `clim` não deveria exigir recriar a textura 3D,
      e vice-versa (ciclos de vida independentes)
- [x] `Renderer::set_colormap(rgba: PyBuffer<u8>)` — recebe uma tabela RGBA `(N, 4)` já pronta.
      O Nebula não sabe nada sobre matplotlib/VisPy/Petrel/Paradigm: o lado Python amostra
      qualquer colormap (nome do matplotlib, ou objeto `vispy.color.Colormap` já convertido de
      Petrel/Paradigm no Andromeda) em N pontos fixos e manda a tabela pronta — mesmo contrato
      do `load_volume` (Python prepara o dado, Rust só sobe pra GPU)
- [x] `Renderer::set_clim(min, max)` — só reescreve o uniform, não recria a textura
- [x] Shader: `raw_value` do volume normalizado por `clim` vira índice de busca na LUT
      (`textureSample(colormap_texture, ..., t).rgb`), e esse RGB substitui o cinza como
      `albedo` que entra na iluminação
- [x] `python/embed_test.py`: `build_colormap_lut(name, resolution=256)` amostra qualquer
      colormap contínuo do matplotlib; `COLORMAP_NAME` no topo do arquivo é o único lugar que
      precisa mudar pra testar outro (`"jet"`, `"gray_r"`, `"seismic"`, etc. — mesmos nomes já
      usados no Andromeda hoje)
- [x] `build_seismic_volume` (novo `pattern="seismic"`): refletores em camadas (mergulho +
      dobra suaves, aleatórios por camada) convolvidos com uma wavelet de Ricker ao longo de Z
      — o mesmo princípio (refletividade × wavelet) de modelagem sísmica convolucional real,
      só sem física de propagação de onda. Bem mais convincente pra validar colormap +
      iluminação juntos do que o xadrez.
- [x] Corrigido o eixo da fatia amostrada: era uma fatia horizontal fixa em Z (`w = 0.5`, um
      "time-slice"), que não mostra a cara clássica de seção sísmica porque as camadas
      onduladas só aparecem numa seção **vertical**. Trocado pra fatiar na crossline do meio
      (`v = 0.5`, eixo X do quad = inline, eixo Y do quad = profundidade) — assim as camadas
      ficam visíveis como as ondulações típicas de uma seção real.
- [x] Câmera padrão normalizada pra crossline: `OrbitCamera::new` usava `azimuth=45°,
      elevation=30°` (bom pro cubo genérico da Fase 2), o que deixava a seção sísmica com cara
      de trapézio — vista de esguelha por padrão, mesmo depois de corrigir o eixo da fatia.
      Trocado pra `azimuth=0, elevation=0`: olha reto pro eixo Z do mundo, que é exatamente a
      direção perpendicular ao quad de seção (fatiado na crossline). Orbit continua livre pro
      usuário girar se quiser.

**Critério de saída**: confirmado visualmente — com `pattern="seismic"` + `viridis`, a seção
aparece retangular (sem distorção de perspectiva), de frente, mostrando camadas onduladas
coloridas (bandas roxo/amarelo sobre fundo verde-água) com mergulho e dobra visíveis, iluminação
(Fase 3.5) ainda aplicada corretamente por cima da cor.

## Fase 4 — Multi-volume, slicing por eixo, visão 2D e fácies (concluída)

Decidido antes de começar: a visão 2D (equivalente ao `Slice2DDialog` do Andromeda) não é um
sistema separado — é a **mesma** fatia (`SeismicSlice`), só vista por uma câmera diferente
(ortográfica, sem rotação, sem luz) em vez da `OrbitCamera` 3D. Isso significa que trocar a
posição do slider no 2D não re-sobe array nenhum: só reescreve o uniform `axis`/`index` de uma
fatia que já está inteira na GPU — o que também acelera o 3D (mesma estrutura).

- [x] `Renderer` deixou de ter um único `volume`/`colormap` implícito — agora guarda
      `volumes: HashMap<id, VolumeEntry>` (textura + colormap + clim próprios) e
      `slices: HashMap<id, SliceEntry>` (qual volume, qual eixo, qual posição, visível ou não).
      Vários volumes e várias fatias do mesmo volume podem coexistir (ex: uma Inline e uma
      Crossline do mesmo dataset, ao mesmo tempo) — mesmo modelo de dicionário por id que o
      `VolumeSlicesManager`/`Well3DPlotManager` do Andromeda já usa.
- [x] `add_volume(id, w, h, d, data)` / `remove_volume(id)` / `set_volume_colormap(id, rgba,
      discrete)` / `set_volume_clim(id, min, max)` / `add_slice(slice_id, volume_id, axis,
      index)` / `remove_slice` / `set_slice_visible` / `set_slice_axis_index` — substituem
      `load_volume`/`set_colormap`/`set_clim` da Fase 3.
- [x] Shader (`volume_slice.wgsl`) generalizado: eixo (0=Inline, 1=Crossline, 2=Time) e posição
      normalizada agora vêm de um uniform por fatia (`@group(3)`, `SliceParams`), em vez do
      `vec3<f32>(uv.x, 0.5, uv.y)` fixo da Fase 3.7 — a geometria do quad continua a mesma
      (posicionamento espacial de verdade em coordenadas de survey fica pra Fase 5/6).
- [x] Câmera ortográfica (`PanZoomCamera`, `nebula/src/camera.rs`) — só pan/zoom, sem rotação,
      equivalente ao `PanZoomCamera` do VisPy. `CameraKind` (enum `Orbit`/`PanZoom`) escolhido
      na criação do `Renderer` (`mode: "orbit"|"panzoom"`).
- [x] Iluminação desligável: `SceneUniform.flags.x` (1.0 = aplica ambiente+difusa, 0.0 = não) —
      a visão 2D é leitura de dado, não superfície *lit*; aplicar sombreamento nela escureceria
      a seção sem motivo.
- [x] Colormap **discreto** (fácies): `set_volume_colormap(..., discrete=True)` troca o sampler
      da LUT de `Linear` pra `Nearest` — vira degrau em vez de gradiente entre classes. Sísmica
      contínua não muda (`discrete=False`, já era o padrão da Fase 3.7).
- [x] Colorbar funcional (`ColorbarWidget`, `python/embed_test.py`): gradiente + ticks
      min/meio/max pintados em `QPainter` puro a partir do mesmo LUT `(N,4)` já usado pelo
      Nebula — fica **ao lado** do canvas no layout, não por cima, então não é um problema de
      overlay; não foi revisitado depois que o texto nativo (mais abaixo, "Terceira correção")
      existiu, mas dá pra portar se um dia fizer sentido ter tudo no mesmo lugar.
- [x] `Slice2DDialog` (`python/embed_test.py`): diálogo 2D de verdade — segunda `NebulaWindow`
      em modo `"panzoom"`, combobox de eixo, slider de posição, colorbar ao lado. Como cada
      `Renderer` tem seu próprio `wgpu::Device`, o diálogo 2D sobe o mesmo array numpy que já
      está em memória Python pro seu próprio `Renderer` — duplica um pouco de VRAM, mas evita a
      complexidade de compartilhar device/texturas entre janelas (não vale a pena agora).
- [x] **Ctrl+arrastar folheia a fatia ativa**: segurar Ctrl (ou apertar "Mover" na toolbar) e
      arrastar move a fatia ativa ao longo do seu próprio eixo, em vez de orbitar a câmera.
      `NebulaWindow.on_slice_changed` notifica quem hospeda a janela (`Slice2DDialog`) pra manter
      slider/combobox sincronizados mesmo quando a mudança vem do drag, não do widget.

**Correção importante (pós-teste visual contra referência Petrel/Ocean)**: a primeira versão
desta fase mostrava só **uma** fatia por vez, sempre um quad plano fixo de frente pra câmera —
"folhear" só trocava a textura amostrada, a geometria nunca se mexia no espaço. Errado: o pedido
sempre foi um **cubo sísmico de verdade**, com as três fatias (Inline/Crossline/Time) visíveis
**ao mesmo tempo**, como três planos perpendiculares que se cruzam dentro de uma caixa com
wireframe ao redor — exatamente como Petrel/Ocean mostram. Corrigido:

- [x] `SliceParams` (`@group(3)`) ganhou um `model: mat4x4<f32>` — cada fatia agora tem sua
      própria transformação no espaço 3D do cubo (rotação + translação), não só o `axis`/`index`
      que escolhe a coordenada de textura. `slice_model_matrix(axis, index)` (`lib.rs`) gira o
      quad plano de origem (que nasce no plano XY local) pra ficar perpendicular ao eixo certo e
      o translada até a posição normalizada. Convenção espacial do cubo: mundo X=Inline,
      Y=Time, Z=Crossline (cubo unitário -1..1).
- [x] Wireframe da caixa (`geometry.rs`: `LineVertex`/`WIREFRAME_VERTICES`/`WIREFRAME_INDICES`,
      `wireframe.wgsl`, novo `wireframe_pipeline`): 12 arestas de um cubo -1..1, desenhado com
      depth sempre passando e sem escrever no depth buffer — é referência espacial, fica visível
      por cima das fatias, não interage com elas.
- [x] `Renderer` passou a suportar várias fatias simultâneas de verdade (já dava pra isso desde
      o `HashMap<id, SliceEntry>`, só faltava o lado Python aproveitar): o demo agora adiciona
      as três (Inline/Crossline/Time) ao mesmo tempo por padrão.
- [x] **Semântica da combobox corrigida**: no Andromeda, a combobox de eixo só *seleciona qual*
      fatia vai ser movida — quem move de verdade é apertar o botão "Mover" (modo liga/desliga)
      e arrastar. Não era pra combobox sozinha resetar a posição pro meio. Corrigido:
      `NebulaWindow.active_slice_id` (setado pela combobox) + `set_move_mode` (botão "Mover") +
      Ctrl+arrastar como atalho adicional (não existe no Andromeda, mas não atrapalha).
- [x] **`nudge_slice(slice_id, screen_dx, screen_dy)`** (`lib.rs`): antes o drag usava
      `dy * sensibilidade` fixo, sem relação com a orientação real da fatia. Agora projeta a
      direção do eixo de movimento da fatia (mundo) pra tela sob a câmera atual (`view_proj`) e
      usa a componente do arrasto do mouse alinhada com essa direção — arrastar "ao longo" do
      eixo do grid na tela avança a fatia de verdade, arrastar perpendicular a ele quase não
      move nada. Só faz sentido pra câmera orbital 3D (na visão 2D ortográfica o eixo de
      movimento aponta pra dentro da tela, sem direção projetável — lá continua usando `dy` direto).

**Fora de escopo desta fase** (fica pra Fase 5, dependem dos dados de horizonte/poço existirem
primeiro): ray marching/transfer functions pro volume 3D cheio (fácies/corpo geológico),
horizonte como linha sobreposta na seção 2D, traço do poço sobreposto na seção 2D.

**Segunda correção (mesma sessão de teste visual)**: duas coisas mais erradas encontradas
depois de ver o cubo de verdade renderizando:

- [x] **"Opacidade" indesejada era o wireframe vazando através das fatias opacas**, não um bug
      de blend nas fatias em si. A caixa usava `depth_compare: Always` (desenhava por cima de
      tudo, inclusive das arestas de trás que deveriam estar escondidas atrás de uma fatia
      opaca) — trocado pra `depth_compare: Less` (`depth_write` continua `false`, então o
      wireframe não bloqueia nada desenhado depois dele, só respeita o que já foi desenhado
      antes). Aproveitando, também **adicionada opacidade de verdade** por volume
      (`set_volume_opacity`, padrão 1.0 = opaco) — igual o Andromeda tem — reaproveitando o
      padding `z` do uniform de `clim` (`colormap.rs`) e trocando o blend do `slice_pipeline` de
      `REPLACE` pra `ALPHA_BLENDING` (idêntico a opaco quando `opacity=1.0`, só mistura de
      verdade se o usuário abaixar o slider).
- [x] **Ctrl+arrastar agora funciona em cima de qualquer fatia da cena, sem precisar da
      combobox** — `Renderer::pick_slice(screen_x, screen_y)` (`lib.rs`) desprojeta o pixel num
      raio de mundo (inversa da `view_proj`) e testa interseção contra o plano de cada fatia
      visível (usando a mesma `slice_model_matrix`), devolvendo a mais próxima da câmera dentro
      dos limites do quad. `NebulaWindow.mousePressEvent` faz o pick assim que Ctrl é
      pressionado; a combobox+"Mover" continuam sendo o fluxo explícito (estilo Andromeda) que
      não depende de picking.

**Fechamento da fase**: ticks numéricos (IL/XL/Time) nos cantos do wireframe e verificação
definitiva do pick de fatia por hover — as duas pendências que ficaram após a correção do cubo.

- [x] `Renderer::project_to_screen(x, y, z) -> Option<(f32, f32)>` (`lib.rs`): projeta um ponto
      do cubo (-1..1) pra coordenada de tela sob a câmera atual — mesma ideia do `pick_slice`, só
      que na direção contrária (mundo → tela em vez de tela → mundo). Serve pros labels de eixo
      agora e pro nome da cabeça do poço na Fase 5, sem o Nebula precisar saber renderizar texto.
- [x] `EdgeLabelsOverlay` (`python/embed_test.py`): 5 `QLabel`s nos cantos do wireframe
      ("IL 0/XL 0/T 0", "IL max/XL 0", etc., igual o Petrel numera a caixa), reposicionados a
      cada frame via `project_to_screen`. **Achado importante**: `createWindowContainer` embute
      uma janela nativa de verdade, e widgets Qt comuns *filhos* do container não compõem por
      cima dela — settar `Qt.WA_AlwaysStackOnTop` "resolvia" visualmente mas **derrubava o
      processo** (provável conflito com a `wgpu::Surface` criada a partir do HWND cru). A solução
      que funcionou e não crasha: uma janela-`Tool` **separada** (`Qt.FramelessWindowHint |
      Qt.WindowStaysOnTopHint`, sem foco, `WA_TransparentForMouseEvents` pra não atrapalhar o
      orbit/pan/zoom por baixo), reposicionada manualmente pra cobrir a área do canvas a cada
      frame — técnica padrão do Qt pra overlay sobre widgets nativos/OpenGL.
- [x] **Achado de ferramental**: `PrintWindow` (usado nas capturas de tela deste projeto) só
      captura o conteúdo de um HWND específico — não vê outras janelas de nível superior
      compostas por cima dela pelo Windows (é exatamente o caso da janela-`Tool` acima). As
      capturas via `PrintWindow` mostravam o cubo sem os labels mesmo com eles renderizando
      corretamente; só uma captura de tela de verdade (`Graphics.CopyFromScreen`) confirmou que
      funcionava. Vale lembrar disso da próxima vez que uma feature parecer "invisível" numa
      captura mas não crashar nem logar erro.
- [x] **Verificação do `pick_slice`** (não deu pra confirmar com automação de mouse via Win32
      `mouse_event`/`keybd_event` — o modificador Ctrl não chegava confiável no `mousePressEvent`
      do Qt): confirmado construindo um `QMouseEvent` com `Qt.ControlModifier` diretamente e
      chamando os handlers da `NebulaWindow`, sem depender de automação de SO nenhuma — mesmo
      caminho de código que um usuário real dispara. Resultado: clique no centro pega a fatia
      Crossline (a que está de frente por padrão); depois de orbitar, o mesmo clique no centro
      pega a fatia Inline (a que passou a ficar na frente); um Ctrl+arrastar na fatia pega mudou
      o `index` de verdade (0.5 → 0.44) via `nudge_slice`. Pipeline completo (pick → nudge)
      funcionando ponta a ponta.

**Terceira correção — texto virou nativo, não é mais overlay Qt**: o `EdgeLabelsOverlay`
descrito acima (janela-`Tool` separada) funcionava, mas o usuário foi claro: o Nebula precisa
ser um motor gráfico **completo**, com texto de verdade renderizado por ele mesmo — "nem que
tenha que haver um atlas de fontes". Substituído por:

- [x] **Fonte bitmap 5x7 embutida** (`font.rs`), sem nenhuma dependência externa (nada de
      `freetype`/`ab_glyph`/`glyphon`) — desenhada como arte ASCII no próprio código-fonte
      (mais fácil de revisar visualmente que bit patterns numéricos), cobrindo A-Z, 0-9 e
      pontuação básica (espaço, `-`, `.`, `:`, `/`, `_`). Rasterizada uma vez em `Renderer::new()`
      num atlas `R8Unorm` (upscale 3x + sampler `Linear` pra suavizar os blocos).
- [x] **Billboard de texto** (`text.rs`/`text.wgsl`): cada label é uma malha de quads (um por
      caractere, gerada uma vez a partir da string) que sempre encara a câmera — os eixos
      "direita"/"cima" da câmera (`OrbitCamera::basis`/`PanZoomCamera::basis`, novos campos
      `camera_right`/`camera_up` no `SceneUniform`) entram no vertex shader pra rotacionar cada
      quad na direção da tela, não do objeto. Suporta `\n` (texto em várias linhas). Sempre
      visível por cima de tudo (`depth_compare: Always`, sem escrever profundidade) — mesmo
      espírito HUD do wireframe antes dele ter sido corrigido pra respeitar profundidade, só que
      aqui é intencional (anotação, não geometria espacial).
- [x] `add_text_label(id, x, y, z, text, r, g, b, scale)` / `remove_text_label` /
      `set_text_label_visible` / `set_text_label_position` — API igual em espírito a
      `add_slice`: o Python decide *o quê* mostrar (o Nebula não sabe o que é um "IL" ou um
      survey), o Rust desenha.
- [x] **`EdgeLabelsOverlay` removida inteiramente** do lado Python — nem janela-`Tool`, nem
      `WA_AlwaysStackOnTop`, nem reposicionamento a cada frame. Substituída, na correção
      seguinte, pelo grid numerado de eixo de verdade (`configure_axis_grid`) — ver "Quinta
      correção" abaixo.

**Quarta correção — opacidade não escondia mais nada atrás dela, exceto quando não devia
esconder nada**: com `opacity=0`, a fatia devia ficar 100% invisível — inclusive pro depth
buffer, senão o wireframe atrás dela continuava sendo ocluído por uma fatia que não desenha cor
nenhuma. Causa: `depth_write_enabled: true` do `slice_pipeline` escreve profundidade
incondicionalmente, mesmo quando o alpha de saída é zero. Corrigido com um `discard` no início do
`fs_main` (`volume_slice.wgsl`) quando `clim.z <= 0.001` — `discard` no WGSL pula o fragmento
inteiro (cor **e** profundidade), então uma fatia totalmente transparente para de existir pro
depth test, não só visualmente. Opacidades intermediárias (ex: 50%) continuam escrevendo
profundidade normalmente — é a limitação normal de alpha blending simples sem ordenação de
transparência, fora de escopo por enquanto.

**Quinta correção — grid de eixo numerado e adaptativo à câmera**: os 5 labels de canto fixos
(`EdgeLabelsOverlay`/`add_wireframe_edge_labels`) mostravam só os extremos, sempre nos mesmos
cantos, mesmo quando a câmera girava e aquele canto passava a ficar na frente do cubo (tampando
a leitura do volume) ou virava de costas (ilegível/invertido). O usuário pediu um eixo de
verdade: numerado feito um grid de matplotlib, com valores reais de Inline/Crossline/Time, e que
os labels/ticks migrem sozinhos pro lado do cubo que está visível/de costas pra câmera — igual
qualquer software 3D de verdade (Petrel, matplotlib `mplot3d`) faz.

- [x] `Renderer::configure_axis_grid(width, height, depth)` (`lib.rs`): cria, uma única vez, 3
      labels de nome de eixo ("INLINE"/"CROSSLINE"/"TIME") + 5 labels de valor de tick por eixo
      (18 labels no total, ids reservados numa faixa própria — `AXIS_GRID_ID_BASE` — bem longe de
      qualquer id que o lado Python normalmente escolhe). O texto de cada tick já nasce com o
      valor real (`0`, `32`, `64`...), calculado a partir de `width`/`height`/`depth` — Time é
      invertido (`Y=+1` é a amostra 0, mais rasa; `Y=-1` é a última amostra), mesma convenção do
      resto do motor.
- [x] **Posição recalculada a cada frame** (`Renderer::update_axis_grid`, chamado do `render()`):
      nenhuma malha é retesselada — só os uniforms de posição de cada label (`queue.write_buffer`)
      e um buffer dinâmico pequeno com os traços curtos de cada tick (desenhado pelo
      `wireframe_pipeline` já existente, só que com um buffer próprio,
      `axis_tick_lines_buffer`). A cada frame: pega a direção câmera→alvo, e pra cada um dos 3
      eixos escolhe a aresta do cubo cujas duas coordenadas fixas são o lado **oposto** de onde a
      câmera está (`axis_grid_point`) — ou seja, a aresta "de trás", que nunca cruza o volume
      renderizado nem fica de cabeça pra baixo, não importa o ângulo de orbit.
- [x] **Tamanho de fonte configurável num lugar só**: duas constantes no topo de `lib.rs`
      (`AXIS_TICK_TEXT_SCALE`, `AXIS_CAPTION_TEXT_SCALE`) — antes cada `add_text_label` espalhado
      pelo `embed_test.py` tinha sua própria `scale` fixa (e grande demais, "0.14"); agora ajustar
      o tamanho do grid inteiro é mudar duas linhas, sem tocar em nenhuma chamada.
- [x] `add_wireframe_edge_labels` removida do `embed_test.py`; `main()` chama
      `render_window.configure_axis_grid(volume_dim, volume_dim, volume_dim)` uma vez (mesmo
      padrão "pendente" de `add_slice`/`add_text_label` — só efetiva na primeira `render_frame()`
      depois que o `Renderer` existe).
- [x] Verificado por screenshot real (`Graphics.CopyFromScreen`) na visão inicial (reta, olhando
      pro eixo Z): INLINE aparece embaixo do cubo com `0, 32, 64, 95, 127`, CROSSLINE na diagonal
      inferior esquerda, TIME na lateral esquerda — todos nas arestas de trás, letra pequena e
      legível, sem sobrepor a fatia sísmica. **Lacuna de verificação**: não consegui uma captura
      de tela limpa confirmando visualmente a troca de aresta *depois* de orbitar (problema da
      ferramenta de captura nesta sessão, não do código — mesma classe de limitação já registrada
      na correção da opacidade); a lógica em si é álgebra vetorial determinística (inverte o sinal
      da direção câmera→alvo por eixo) e compilou/rodou sem erro em todos os testes.

## Fase 5 — Objetos sísmicos específicos (planejada)

Baseada na leitura direta do código atual do Andromeda (`visualization_managers/well/*.py`,
`visualization_managers/horizon/horizon_manager.py`) — ver "Cobertura funcional" abaixo pro
levantamento original. Decisão de fundo, reafirmada pelo usuário: **genericidade não é
necessária** — nada de sistema genérico de "scene objects"/ECS, só os tipos concretos que o
Andromeda realmente tem (fatia, poço, horizonte), cada um com seus próprios métodos, igual o
padrão que já existe (`add_volume`/`add_slice`).

### Peça compartilhada nova: `mesh.rs`

Poço (trajetória, log) e horizonte (picks, grid) são todos **malhas com um escalar por
vértice** (profundidade, valor de log, amplitude) colorido por colormap — exatamente o problema
que o `@group(2)` (textura 1D + sampler + `clim`) da Fase 3.7/4 já resolve. Diferença chave em
relação ao Vispy/`cigvis` de hoje: lá a cor é **assada** em CPU via matplotlib
(`create_well_logs`, `add_horizon_surface_to_canvas`), então trocar colormap/clim exige
retesselar a malha inteira (`replot_well_log`). Aqui a cor é sampleada **no shader**, então
trocar clim/colormap de um poço ou horizonte vira só reescrever um uniform — melhoria real sobre
o VisPy atual, de graça, porque a arquitetura já foi construída certa desde a Fase 3.7.

- `MeshVertex { position: [f32;3], normal: [f32;3], scalar: f32 }` — normal calculada em Rust
  (média ponderada por área das faces adjacentes), já que nem `cigvis.trajectory_mesh` nem a
  triangulação Delaunay do Python devolvem normal pronta.
  Reaproveita `@group(0)` (câmera/luz, sem mudança) e o mesmo layout de `@group(2)`
  (colormap+clim) do volume. **Sem** `@group(1)` (textura de volume) nem `@group(3)` (slice
  params) — não fazem sentido pra malha.
- Upload genérico: `MeshEntry { vertex_buffer, index_buffer, num_indices, colormap, clim,
  visible }`, guardado num `HashMap<id, MeshEntry>` por tipo (mesmo padrão de
  `volumes`/`slices` da Fase 4).

### Well

Python continua 100% responsável pelo domínio (checkshot, transformação survey→índice de grid,
flip de profundidade — tudo que `Well3DPositionGenerator.run()` já faz hoje); o Nebula só recebe
arrays `(x, y, z)` prontos.

- `add_well_trajectory(id, vertices, indices, color)` — a tesselação do tubo continua vindo do
  Python via `cigvis.meshs.well_logs.trajectory_mesh` (é só geometria de varredura de círculo ao
  longo de uma polilinha; reimplementar em Rust não compensa — poços são minúsculos perto de um
  volume). O Rust só sobe `verts`/`faces`, igual `add_volume` já faz com o array de voxels.
- `add_well_log(id, vertices, indices, scalar_per_vertex, clim)` — mesmo tubo, mas cor vem do
  colormap em shader agora, não mais assada no Python. `replot_well_log` deixa de existir:
  trocar clim vira `set_mesh_clim(id, ...)`, tão barato quanto `set_volume_clim`.
- `remove_well(id)` / `set_well_visible(id, bool)`.
- **Cabeça do poço**: malha pequena (anel + cruz, igual `generate_well_heads_visuals` já faz)
  em vez de linhas soltas do VisPy. O **label de texto** (nome do poço, coordenadas) usa
  `add_text_label` (texto GPU nativo, Fase 4) — não precisa de nenhuma peça nova, já existe.

### Horizon

O usuário sinalizou que pretende **redesenhar** o horizonte do zero (hoje é só picks
triangulados + grid), com atributos (principalmente decomposição espectral — 3 horizontes de
frequências diferentes sobrepostos) e picking interativo. A API abaixo já nasce preparada pra
isso sem exigir retrabalho:

- `add_horizon_picks(id, vertices, triangle_indices, scalar_per_vertex, clim)` — a triangulação
  Delaunay continua no Python (`scipy.spatial.Delaunay`, é álgebra 2D, não é trabalho de
  renderer).
- `add_horizon_grid(id, n1, n2, z_values)` — aqui vale tesselar **no Rust**: malha de grid
  estruturado é trivial (lattice regular, não precisa de Delaunay), evita ida e volta de dados
  que o Python já não precisaria processar.
- **Ponto de extensão pra decomposição espectral**: em vez de um `scalar: f32` único por
  vértice, a struct interna guarda `attributes: HashMap<String, Vec<f32>>` (Z, amplitude, banda
  de frequência 1/2/3...) + `set_active_attribute(id, name)` escolhendo qual alimenta o
  colormap agora. **Não** implementar o blend de 3 bandas sobrepostas nesta fase — só deixar o
  layout pronto pra não exigir reescrever tudo quando o redesenho for fechado.
- `remove_horizon(id)` / `set_horizon_visible(id, bool)`.

### Horizonte/poço sobrepostos na seção 2D (adiado da Fase 4)

Pipeline novo e pequeno, `line.wgsl`: vértices só posição+cor, sem luz, `LineList`, reaproveita
só `@group(0)`. Dados: perfil de Z do horizonte ao longo da linha de inline/crossline atual vira
uma polyline 2D; o traço do poço na seção vira uma linha vertical na posição onde ele cruza —
os dois calculados em Python a partir de dados que já existem, sem peça nova de domínio.

### Picking generalizado

`pick_slice` (Fase 4) só testa fatias. Generalizar pra `pick(screen_x, screen_y) ->
Optional[(kind, id)]` testando fatias **e** malhas (poço, horizonte) é o que picking de
horizonte (criar um pick clicando na cena) vai precisar. **Nota de processo**: a forma confiável
de testar interação com modificador (Ctrl+clique) é construir um `QMouseEvent` diretamente e
chamar os handlers — a automação de mouse via Win32 (`mouse_event`/`keybd_event`) não entrega o
modificador Ctrl de forma confiável ao Qt (achado da Fase 4, ver acima).

### Fora de escopo da Fase 5 (decisões já tomadas, não revisitar sem motivo novo)

- LUT categórica de poço/log — fica no LogPlot Vision (VisPy), Nebula não trata disso.
- Blend de decomposição espectral (3 horizontes sobrepostos) — só a infraestrutura de múltiplos
  atributos fica pronta, o blend em si é trabalho futuro quando o redesenho do horizonte fechar.
- Criar picks de horizonte de fato (fluxo de edição) — só a infraestrutura de picking genérico.

## Fase 6 — Performance (planejada)

- **Streaming/paginação de volumes grandes**: dados sísmicos reais são GBs — carregar tudo de
  uma vez em `add_volume` não escala. Precisa de leitura lazy por chunk a partir do HDF5 (Python
  continua responsável pela paginação; o Rust recebe chunks e faz upload incremental na textura
  3D via `write_texture` com `Origin3d` diferente de zero, em vez de recriar a textura inteira).
- **LOD por distância de câmera**: mipmaps da textura de volume, ou downsample dinâmico da
  resolução amostrada, pra volumes que hoje forçariam recriar a textura inteira a cada zoom.
- **Ray marching / transfer functions** pro volume 3D cheio (fácies, corpo geológico) — adiado
  da Fase 4 porque não bloqueia poço/horizonte. Pipeline separado (`ray_march.wgsl`), desenha um
  cubo-proxy em vez do quad da fatia, acumula cor+opacidade front-to-back amostrando o volume ao
  longo do raio; reaproveita `@group(1)` (volume) e `@group(2)` (colormap) — só precisa que o
  canal alpha do colormap (hoje sempre opaco) vire a curva de opacidade da transfer function.
- **Profiling com dado real**: o volume sintético de 128³ usado nos testes (Fase 4) não estressa
  nada — sobra FPS (4000+). Só vale investir em otimização depois de testar com um volume de
  survey de verdade (centenas de MB a poucos GB).
- Reavaliar compute shaders (`wgpu`) vs. fragment shader puro pro ray marching quando houver
  esse volume real pra medir contra.

## Cobertura funcional — o que o Nebula precisa equivaler/superar em relação ao VisPy atual

Levantado a partir do código real do Andromeda
(`src/core/visualization/seismic/`). Serve de checklist de paridade funcional pras Fases 3–6.
Itens já resolvidos marcados com a fase que fechou eles; o resto é a Fase 5/6 acima.

### Objetos de cena
- ~~**Slices axis-aligned de volumes** (`AxisAlignedImage`)~~ — **feito (Fase 4)**: múltiplos
  volumes simultâneos, cada um com várias fatias, posicionadas de verdade no espaço 3D. Falta
  ainda: leitura lazy de HDF5 (streaming, Fase 6) e alinhamento por offset/escala de survey real
  (Fase 5/6, quando integrar com dados reais do Andromeda — o cubo hoje é sempre -1..1 unitário).
- **Horizontes**: duas representações — picks (mesh via triangulação Delaunay da projeção XY)
  e superfície gridada interpolada. Hoje geradas via biblioteca externa `cigvis`; plano de
  substituição na Fase 5 acima.
- **Poços**: trajetória (tubo/mesh ao longo da trajetória 3D) + logs coloridos por colormap ao
  longo do tubo. Também depende de `cigvis` hoje (`create_well_logs`, `trajectory_mesh`); plano
  de substituição na Fase 5 acima.
- **Linhas arbitrárias**: seções extraídas por interpolação ao longo de uma polilinha
  não-axis-aligned através de um volume (2D e 3D). Ainda não planejado em detalhe — depende do
  formato que o pipeline de linha da Fase 5 (horizonte/poço em 2D) assentar.
- ~~**Wireframe/grid box**~~ — **feito (Fase 4)**: caixa 3D com labels IL/XL/Time nos cantos,
  texto GPU nativo (`add_text_label`, fonte bitmap embutida). Só os 5 cantos principais, não
  ticks intermediários ao longo das arestas — suficiente por enquanto.

### Overlays HUD (tela fixa, não fazem parte da geometria 3D do mundo)
- Bússola de norte e indicador de eixo XYZ, sincronizados manualmente com a rotação da câmera —
  ainda não implementado.
- ~~Colorbar~~ — **feito (Fase 4)**: `ColorbarWidget` em Qt puro (`QPainter`), gradiente do
  mesmo LUT `(N,4)` já usado pelo Nebula, sem replotar nada a cada mudança de cmap/clim (ao
  contrário do raster matplotlib do VisPy atual).

### Sistema de coordenadas (precisa ser portado fielmente)
- Array de volume: shape `(n_inline, n_xline, n_samples)`.
- **Eixo Z invertido**: profundidade/tempo cresce pra baixo na tela; índice de array não.
- **Offset + escala por volume**: cada dataset (sísmica/facies/atributos) pode ter
  IL/XL/amostra min/max/sampling diferentes do survey de referência — todos reprojetados pro
  grid do survey pai via `world_offset` + `processed_axis_scales`.
- **Quadrantes de azimute**: orientação da bússola/geometria depende de em qual dos 4
  quadrantes (0-90/90-180/180-270/270-360°) o azimute do survey cai — inverte sinais X/Y.
- **Exagero vertical**: aplicado só na transformação de câmera (multiplica componente Z),
  nunca nos dados armazenados.

## Notas

- `cigvis` continua sendo usado do lado Python na Fase 5 (tesselação de tubo de poço via
  `trajectory_mesh`) — não é reimplementado em Rust, só o upload da malha resultante. A
  triangulação Delaunay de horizonte (`scipy.spatial.Delaunay`) segue a mesma lógica: é álgebra
  2D do lado Python, não trabalho de renderer.
- Ver [`PYTHON_IMPLEMENTATION.md`](PYTHON_IMPLEMENTATION.md) pro guia prático de como integrar
  o Nebula no Andromeda de verdade — instalação, ciclo de vida do `Renderer`, convenções de
  dados, e como isso se encaixa nos managers que já existem
  (`VolumeSlicesManager`/`HorizonManager`/`Well3DPlotManager`).
