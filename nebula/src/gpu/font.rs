//! Fonte bitmap 5x7 embutida — sem dependência externa (nada de `freetype`/
//! `ab_glyph`/etc.), sem passar pelo Python: o Nebula é dono do próprio
//! texto, igual qualquer motor gráfico completo deveria ser. Caractere
//! desconhecido é simplesmente ignorado (avança o cursor, não desenha nada).
//!
//! Cada glifo é 5 colunas x 7 linhas, desenhado como arte ASCII (`#`=aceso,
//! `.`=apagado) — mais fácil de revisar/editar visualmente no código do que
//! bit patterns numéricos.

pub const GLYPH_COLS_IN_ATLAS: u32 = 8;
pub const PIXEL_SCALE: u32 = 3;
pub const GLYPH_W: u32 = 5 * PIXEL_SCALE;
pub const GLYPH_H: u32 = 7 * PIXEL_SCALE;
pub const CELL_W: u32 = GLYPH_W + PIXEL_SCALE;
pub const CELL_H: u32 = GLYPH_H + PIXEL_SCALE;

type GlyphRows = [&'static str; 7];

const GLYPHS: &[(char, GlyphRows)] = &[
    (
        '0',
        [
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ],
    ),
    (
        '1',
        [
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
    ),
    (
        '2',
        [
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
    ),
    (
        '3',
        [
            ".###.", "#...#", "....#", "..##.", "....#", "#...#", ".###.",
        ],
    ),
    (
        '4',
        [
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
    ),
    (
        '5',
        [
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ],
    ),
    (
        '6',
        [
            "..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###.",
        ],
    ),
    (
        '7',
        [
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
    ),
    (
        '8',
        [
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
    ),
    (
        '9',
        [
            ".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##..",
        ],
    ),
    (
        'A',
        [
            "..#..", ".#.#.", "#...#", "#...#", "#####", "#...#", "#...#",
        ],
    ),
    (
        'B',
        [
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ],
    ),
    (
        'C',
        [
            ".####", "#....", "#....", "#....", "#....", "#....", ".####",
        ],
    ),
    (
        'D',
        [
            "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
        ],
    ),
    (
        'E',
        [
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ],
    ),
    (
        'F',
        [
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ],
    ),
    (
        'G',
        [
            ".####", "#....", "#....", "#.###", "#...#", "#...#", ".####",
        ],
    ),
    (
        'H',
        [
            "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
    ),
    (
        'I',
        [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####",
        ],
    ),
    (
        'J',
        [
            "..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##..",
        ],
    ),
    (
        'K',
        [
            "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
        ],
    ),
    (
        'L',
        [
            "#....", "#....", "#....", "#....", "#....", "#....", "#####",
        ],
    ),
    (
        'M',
        [
            "#...#", "##.##", "#.#.#", "#...#", "#...#", "#...#", "#...#",
        ],
    ),
    (
        'N',
        [
            "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#",
        ],
    ),
    (
        'O',
        [
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
    ),
    (
        'P',
        [
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
    ),
    (
        'Q',
        [
            ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#",
        ],
    ),
    (
        'R',
        [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
    ),
    (
        'S',
        [
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
    ),
    (
        'T',
        [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ],
    ),
    (
        'U',
        [
            "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
    ),
    (
        'V',
        [
            "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#..",
        ],
    ),
    (
        'W',
        [
            "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "#.#.#", ".#.#.",
        ],
    ),
    (
        'X',
        [
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
        ],
    ),
    (
        'Y',
        [
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ],
    ),
    (
        'Z',
        [
            "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####",
        ],
    ),
    (
        ' ',
        [
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ],
    ),
    (
        '-',
        [
            ".....", ".....", ".....", "#####", ".....", ".....", ".....",
        ],
    ),
    (
        '.',
        [
            ".....", ".....", ".....", ".....", ".....", "..#..", "..#..",
        ],
    ),
    (
        ':',
        [
            ".....", "..#..", ".....", ".....", "..#..", ".....", ".....",
        ],
    ),
    (
        '/',
        [
            "....#", "...#.", "..#..", "..#..", ".#...", "#....", ".....",
        ],
    ),
    (
        '_',
        [
            ".....", ".....", ".....", ".....", ".....", ".....", "#####",
        ],
    ),
];

fn glyph_index(c: char) -> Option<usize> {
    GLYPHS.iter().position(|(gc, _)| *gc == c)
}

pub fn atlas_rows() -> u32 {
    (GLYPHS.len() as u32).div_ceil(GLYPH_COLS_IN_ATLAS)
}

pub fn atlas_size() -> (u32, u32) {
    (GLYPH_COLS_IN_ATLAS * CELL_W, atlas_rows() * CELL_H)
}

/// UV do retângulo do glifo (só a área do desenho, sem o padding da célula,
/// pra não vazar pixel de um glifo vizinho na borda) — `None` se o
/// caractere não existe na fonte (o chamador decide o que fazer, tipicamente
/// pular e não desenhar nada).
pub fn glyph_uv(c: char) -> Option<(f32, f32, f32, f32)> {
    let idx = glyph_index(c.to_ascii_uppercase())?;
    let col = (idx as u32) % GLYPH_COLS_IN_ATLAS;
    let row = (idx as u32) / GLYPH_COLS_IN_ATLAS;
    let (atlas_w, atlas_h) = atlas_size();
    let u0 = (col * CELL_W) as f32 / atlas_w as f32;
    let v0 = (row * CELL_H) as f32 / atlas_h as f32;
    let u1 = (col * CELL_W + GLYPH_W) as f32 / atlas_w as f32;
    let v1 = (row * CELL_H + GLYPH_H) as f32 / atlas_h as f32;
    Some((u0, v0, u1, v1))
}

/// Rasteriza a fonte inteira num buffer `R8` (0 ou 255) — construído uma
/// única vez no `Renderer::new()`, não a cada label.
pub fn build_atlas_raster() -> (Vec<u8>, u32, u32) {
    let (atlas_w, atlas_h) = atlas_size();
    let mut data = vec![0u8; (atlas_w * atlas_h) as usize];

    for (idx, (_, rows)) in GLYPHS.iter().enumerate() {
        let col = (idx as u32) % GLYPH_COLS_IN_ATLAS;
        let row = (idx as u32) / GLYPH_COLS_IN_ATLAS;
        let base_x = col * CELL_W;
        let base_y = row * CELL_H;

        for (ry, row_str) in rows.iter().enumerate() {
            for (rx, ch) in row_str.chars().enumerate() {
                if ch != '#' {
                    continue;
                }
                for dy in 0..PIXEL_SCALE {
                    for dx in 0..PIXEL_SCALE {
                        let px = base_x + (rx as u32) * PIXEL_SCALE + dx;
                        let py = base_y + (ry as u32) * PIXEL_SCALE + dy;
                        data[(py * atlas_w + px) as usize] = 255;
                    }
                }
            }
        }
    }

    (data, atlas_w, atlas_h)
}
