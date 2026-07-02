//! CP437/ANSI art → canvas draw ops.
//!
//! Parsing is **not** re-implemented here: this module leans on
//! [`rabbithole_art`], the workspace's CP437/ANSI pipeline, for the
//! [`Canvas`]/[`Cell`] model and the canonical 16-colour VGA
//! [`PALETTE`](rabbithole_art::raster::PALETTE). What it adds is the small,
//! **pure and host-tested** transformation from a parsed [`Canvas`] into a flat
//! list of [`DrawOp`]s — one styled cell each, with reverse-video already
//! resolved and colours pre-converted to RGB. Only [`paint`] (wasm-gated) does
//! any real DOM/canvas work, and it is a thin loop over the draw ops.
//!
//! ```
//! use rabbithole_ui_web::art::{parse_art, to_draw_ops};
//! let canvas = parse_art(b"\x1b[31mA");
//! let ops = to_draw_ops(&canvas);
//! assert_eq!(ops[0].ch, 'A');
//! assert_eq!(ops[0].fg, [0xAA, 0x00, 0x00]); // VGA red
//! ```

use rabbithole_art::ansi::{self, Attrs, Canvas, Cell};
use rabbithole_art::raster::PALETTE;
use rabbithole_art::sauce::SauceRecord;

/// Cell width in device pixels (matches the embedded 8×16 CP437 font).
pub const CELL_W: f64 = 8.0;
/// Cell height in device pixels.
pub const CELL_H: f64 = 16.0;

/// One drawable cell: a glyph plus its resolved foreground/background RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawOp {
    /// Column (zero-based).
    pub col: usize,
    /// Row (zero-based).
    pub row: usize,
    /// The glyph to draw.
    pub ch: char,
    /// Foreground colour as RGB (reverse video already applied).
    pub fg: [u8; 3],
    /// Background colour as RGB (reverse video already applied).
    pub bg: [u8; 3],
}

impl DrawOp {
    /// Top-left pixel position `(x, y)` of this cell.
    pub fn pixel_origin(&self) -> (f64, f64) {
        (self.col as f64 * CELL_W, self.row as f64 * CELL_H)
    }
}

/// Parse a complete CP437/ANSI file, honouring its SAUCE record for the canvas
/// width and iCE-colour flag (falling back to 80 columns when absent).
pub fn parse_art(bytes: &[u8]) -> Canvas {
    let body = SauceRecord::strip(bytes);
    let sauce = SauceRecord::from_bytes(bytes);
    let width = sauce
        .as_ref()
        .and_then(|s| s.width_hint())
        .unwrap_or(ansi::DEFAULT_WIDTH);
    let ice = sauce.as_ref().is_some_and(|s| s.ice_colors());
    ansi::parse_with(body, width, ice)
}

/// The RGB for a resolved 0–15 palette index.
fn rgb(index: u8) -> [u8; 3] {
    PALETTE[(index & 0x0F) as usize]
}

/// The `(foreground, background)` RGB a cell renders with, after reverse video.
fn cell_colors(cell: &Cell) -> ([u8; 3], [u8; 3]) {
    let (fg, bg) = if cell.attrs.contains(Attrs::REVERSE) {
        (cell.bg, cell.fg)
    } else {
        (cell.fg, cell.bg)
    };
    (rgb(fg), rgb(bg))
}

/// Project a parsed [`Canvas`] into a flat list of [`DrawOp`]s in row-major
/// order.
///
/// Every non-default cell yields an op; fully-default cells (space glyph on the
/// default background) are skipped so the caller only paints what matters. This
/// is the entire cells → drawing transformation, kept pure for host testing.
pub fn to_draw_ops(canvas: &Canvas) -> Vec<DrawOp> {
    let default = Cell::default();
    let mut ops = Vec::new();
    for (row, cells) in canvas.rows().enumerate() {
        for (col, cell) in cells.iter().enumerate() {
            if *cell == default {
                continue;
            }
            let (fg, bg) = cell_colors(cell);
            ops.push(DrawOp {
                col,
                row,
                ch: cell.ch,
                fg,
                bg,
            });
        }
    }
    ops
}

/// The `(width, height)` in device pixels a [`Canvas`] paints to.
pub fn pixel_size(canvas: &Canvas) -> (f64, f64) {
    (
        canvas.width() as f64 * CELL_W,
        canvas.height() as f64 * CELL_H,
    )
}

/// A CSS `#rrggbb` string for an RGB triple.
pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// Paint a parsed [`Canvas`] onto a 2D canvas context (browser only).
///
/// Sizes the element to the canvas, fills each cell's background, then draws
/// its glyph in the foreground colour. All the decision-making already happened
/// in [`to_draw_ops`]; this is the thin, untestable DOM edge.
#[cfg(target_arch = "wasm32")]
pub fn paint(element: &web_sys::HtmlCanvasElement, canvas: &Canvas) {
    use wasm_bindgen::JsCast;

    let (w, h) = pixel_size(canvas);
    element.set_width(w.max(CELL_W) as u32);
    element.set_height(h.max(CELL_H) as u32);

    let Ok(Some(obj)) = element.get_context("2d") else {
        return;
    };
    let Ok(ctx) = obj.dyn_into::<web_sys::CanvasRenderingContext2d>() else {
        return;
    };

    // Clear to the canvas default background (palette index 0, black).
    ctx.set_fill_style_str(&hex(rgb(0)));
    ctx.fill_rect(0.0, 0.0, w, h);
    ctx.set_font(&format!("{}px monospace", CELL_H as u32));
    ctx.set_text_baseline("bottom");

    for op in to_draw_ops(canvas) {
        let (x, y) = op.pixel_origin();
        ctx.set_fill_style_str(&hex(op.bg));
        ctx.fill_rect(x, y, CELL_W, CELL_H);
        if op.ch != ' ' {
            ctx.set_fill_style_str(&hex(op.fg));
            let _ = ctx.fill_text(&op.ch.to_string(), x, y + CELL_H);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_projects_colored_glyph() {
        let canvas = parse_art(b"\x1b[31;44mA");
        let ops = to_draw_ops(&canvas);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].ch, 'A');
        assert_eq!(ops[0].col, 0);
        assert_eq!(ops[0].row, 0);
        assert_eq!(ops[0].fg, [0xAA, 0x00, 0x00]); // VGA red
        assert_eq!(ops[0].bg, [0x00, 0x00, 0xAA]); // VGA blue
    }

    #[test]
    fn bold_selects_bright_foreground() {
        let canvas = parse_art(b"\x1b[1;34mX");
        let ops = to_draw_ops(&canvas);
        assert_eq!(ops[0].fg, [0x55, 0x55, 0xFF]); // bright blue (index 12)
    }

    #[test]
    fn reverse_video_swaps_colors() {
        let normal = to_draw_ops(&parse_art(b"\x1b[31;44mA"))[0];
        let reversed = to_draw_ops(&parse_art(b"\x1b[7;31;44mA"))[0];
        assert_eq!(reversed.fg, normal.bg);
        assert_eq!(reversed.bg, normal.fg);
    }

    #[test]
    fn default_cells_are_skipped() {
        // Two glyphs separated by a default (blank) cell via cursor-forward.
        let canvas = parse_art(b"A\x1b[2CB");
        let ops = to_draw_ops(&canvas);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].col, 0);
        assert_eq!(ops[1].col, 3);
    }

    #[test]
    fn multi_row_positions_are_row_major() {
        let canvas = parse_art(b"a\r\nb");
        let ops = to_draw_ops(&canvas);
        assert_eq!(ops[0].row, 0);
        assert_eq!(ops[1].row, 1);
    }

    #[test]
    fn pixel_size_tracks_grid() {
        let canvas = parse_art(b"hi\r\nthere");
        let (w, h) = pixel_size(&canvas);
        assert_eq!(w, 80.0 * CELL_W); // default 80 columns
        assert_eq!(h, 2.0 * CELL_H);
    }

    #[test]
    fn hex_formats_lowercase_padded() {
        assert_eq!(hex([0x00, 0xAA, 0x0F]), "#00aa0f");
        assert_eq!(hex([0xFF, 0xFF, 0xFF]), "#ffffff");
    }

    #[test]
    fn sauce_width_is_honored() {
        let record = SauceRecord {
            title: "demo".into(),
            datatype: rabbithole_art::sauce::data_type::CHARACTER,
            filetype: rabbithole_art::sauce::character_file_type::ANSI,
            tinfo1: 40,
            ..Default::default()
        };
        let mut file = b"hi".to_vec();
        record.append_to(&mut file);
        let canvas = parse_art(&file);
        assert_eq!(canvas.width(), 40);
    }
}
