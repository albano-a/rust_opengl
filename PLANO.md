# Plano — Motor de Visualização Sísmica 3D (Andromeda)

## Objetivo final

Motor gráfico em Rust para visualização sísmica 3D, embutido dentro de um `QWidget`
(aplicação host em Qt/Andromeda).

## Decisão de stack

| Opção | Veredito | Motivo |
|---|---|---|
| OpenGL puro (atual) | Descontinuar após o protótipo | Bom pra aprender o pipeline básico, mas sem compute shaders nativos — ruim pra volume rendering |
| **wgpu** | **Escolhido** | Baixo nível, cross-platform, compute shaders nativos (essencial pra ray marching/LOD de volumes), aceita `raw-window-handle` diretamente |
| Bevy | Descartado por ora | ECS e convenções de engine atrapalham controle fino de um pipeline customizado (raycasting, transfer functions) |
| `qmetaobject-rs` | Descartado | Só suporta QML, não QWidget; projeto em manutenção passiva (foco migrou pro Slint) |
| `cxx-qt` | **Escolhido (lado Qt)** | Bindings Rust↔Qt seguras e idiomáticas, com suporte a QWidget |

## Risco arquitetural principal

O maior risco do projeto não é o shader de volume — é o **encaixe wgpu dentro de um QWidget**.
Por isso esse encaixe deve ser validado **antes** de qualquer investimento em técnicas de
visualização sísmica.

Mecanismo:
1. Obter o *native window handle* (HWND no Windows) do `QWidget` via `cxx-qt`
   (`QWidget::winId()` / `QWindow::fromWinId()` / `QWidget::createWindowContainer()`).
2. Passar esse HWND pro `wgpu` através de `raw-window-handle` para criar a `Surface`.
3. O Qt cede o widget como um "buraco" na UI — quem desenha dentro é o wgpu via swapchain
   própria, sem passar pelo pipeline de pintura do Qt.

## Fase 1 — Protótipo de encaixe (foco atual)

Objetivo: provar que um triângulo renderizado via wgpu aparece dentro de um QWidget real,
antes de tocar em qualquer coisa relacionada a dados sísmicos.

- [ ] Migrar o triângulo atual (`src/main.rs`) de `glutin`/`gl` para `wgpu`
  - Trocar criação de contexto OpenGL por `wgpu::Instance` + `Surface` + `Device`/`Queue`
  - Reescrever vertex/fragment shaders em WGSL (substituindo `shader.vert`/`shader.frag`)
  - Manter o mesmo resultado visual (triângulo colorido) como critério de sucesso
- [ ] Criar app host mínima em Qt/C++ com um `QWidget`
- [ ] Integrar `cxx-qt` para expor o `winId()` do `QWidget` ao lado Rust
- [ ] Criar a `wgpu::Surface` a partir do HWND do `QWidget` (via `raw-window-handle`)
- [ ] Validar resize: `QWidget` redimensiona → `Surface` é reconfigurada corretamente
- [ ] Validar ciclo de vida: fechar/reabrir o widget não crasha nem vaza recursos

**Critério de saída da Fase 1**: triângulo desenhado via wgpu, visível e redimensionável,
dentro de um QWidget nativo, sem intervenção do pipeline de pintura do Qt.

## Fases seguintes (não iniciar antes da Fase 1 validada)

2. **Fundamentos wgpu**: uniforms, matrizes MVP, câmera 3D orbital, texturas 2D
3. **Volumes**: texturas 3D, upload de dados sísmicos (formato a definir — SEG-Y?)
4. **Volume rendering**: ray marching no fragment/compute shader, transfer functions,
   slicing de planos
5. **Performance**: LOD, streaming de volumes grandes (dados sísmicos costumam ter
   gigabytes), profiling

## Notas

- Dados sísmicos reais são tipicamente grandes (GBs) — desde a Fase 3 já pensar em
  streaming/paginação, não carregar o volume inteiro em memória de uma vez.
- Reavaliar o uso de compute shaders (wgpu) vs. fragment shader puro para o ray marching
  assim que houver volume real de teste.
