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

## Fase 1 — Protótipo de encaixe (foco atual)

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

## Fases seguintes

4. **Volume rendering**: ray marching no fragment/compute shader, transfer functions,
   slicing de planos axis-aligned (equivalente ao `AxisAlignedImage` do VisPy)
5. **Objetos sísmicos específicos** (ver seção "Cobertura funcional" abaixo): horizontes,
   poços/logs, linhas arbitrárias, overlays HUD
6. **Performance**: LOD, streaming de volumes grandes, profiling

## Cobertura funcional — o que o Nebula precisa equivaler/superar em relação ao VisPy atual

Levantado a partir do código real do Andromeda
(`src/core/visualization/seismic/`). Serve de checklist de paridade funcional pras Fases 3–6.

### Objetos de cena
- **Slices axis-aligned de volumes** (`AxisAlignedImage`): planos 2D dentro de um volume 3D,
  dados vindos de HDF5 (lazy, sem carregar tudo em memória). Precisa suportar múltiplos volumes
  simultâneos (sísmica, facies com LUT categórica, atributos elásticos multi-canal) alinhados
  num grid compartilhado via offset + escala por volume (ver "Sistema de coordenadas" abaixo).
- **Horizontes**: duas representações — picks (mesh via triangulação Delaunay da projeção XY)
  e superfície gridada interpolada. Hoje geradas via biblioteca externa `cigvis`; o Nebula
  precisa reimplementar essa geração de mesh.
- **Poços**: trajetória (tubo/mesh ao longo da trajetória 3D) + logs coloridos por colormap ao
  longo do tubo. Também depende de `cigvis` hoje (`create_well_logs`, `trajectory_mesh`).
- **Linhas arbitrárias**: seções extraídas por interpolação ao longo de uma polilinha
  não-axis-aligned através de um volume (2D e 3D).
- **Wireframe/grid box**: caixa 3D com ticks e labels nos 3 eixos (Inline/Crossline/Time).

### Overlays HUD (tela fixa, não fazem parte da geometria 3D do mundo)
- Bússola de norte e indicador de eixo XYZ, sincronizados manualmente com a rotação da câmera.
- Colorbar — hoje renderizada via matplotlib → raster estático (caro, replota a figura inteira
  a cada mudança de cmap/clim). No Nebula isso deve virar uma textura 1D de LUT + shader, não
  um raster gerado externamente.

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

- Dados sísmicos reais são tipicamente grandes (GBs) — desde a Fase 3 já pensar em
  streaming/paginação, não carregar o volume inteiro em memória de uma vez.
- Reavaliar o uso de compute shaders (wgpu) vs. fragment shader puro para o ray marching
  assim que houver volume real de teste.
- `cigvis` é usado hoje pra geração de mesh de superfícies, tubos de poço/log e linhas
  arbitrárias — é a maior peça de lógica geométrica a reimplementar em Rust.
