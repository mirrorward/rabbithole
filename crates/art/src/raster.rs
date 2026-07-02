//! PNG thumbnail rasterizer: turn a [`Canvas`] into a real bitmap.
//!
//! The text renderers in [`crate::render`] project a canvas back onto a
//! terminal; this one projects it onto pixels so galleries and the web
//! client can show an ANSI file without a VT100. Each cell becomes an
//! [`crate::font::GLYPH_WIDTH`]×[`crate::font::GLYPH_HEIGHT`] block drawn
//! from the embedded CP437 font, painted in the classic 16-color VGA
//! palette (bright half included). Foreground/background come straight
//! from the already-resolved [`Cell`] indices; the reverse attribute swaps
//! them. Bold and iCE brightening were folded into the indices by the
//! parser, so no double-application happens here.
//!
//! Output is bounded: [`PngOptions::max_dimension`] caps the pixel extent,
//! cropping the grid rather than allocating unboundedly for hostile input.
//! Encoding writes into an in-memory `Vec<u8>` (an infallible sink), so the
//! function is total — arbitrary canvases render, never panic.

use crate::ansi::{Attrs, Canvas, Cell};
use crate::cp437::unicode_to_cp437;
use crate::font::{FONT_8X16, GLYPH_HEIGHT, GLYPH_WIDTH};

/// The canonical IBM VGA/DOS 16-color palette as RGB triples, indexed by
/// the resolved 0–15 palette index (0–7 normal, 8–15 bright).
pub const PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], // 0  black
    [0xAA, 0x00, 0x00], // 1  red
    [0x00, 0xAA, 0x00], // 2  green
    [0xAA, 0x55, 0x00], // 3  yellow/brown
    [0x00, 0x00, 0xAA], // 4  blue
    [0xAA, 0x00, 0xAA], // 5  magenta
    [0x00, 0xAA, 0xAA], // 6  cyan
    [0xAA, 0xAA, 0xAA], // 7  white/light grey
    [0x55, 0x55, 0x55], // 8  bright black (dark grey)
    [0xFF, 0x55, 0x55], // 9  bright red
    [0x55, 0xFF, 0x55], // 10 bright green
    [0xFF, 0xFF, 0x55], // 11 bright yellow
    [0x55, 0x55, 0xFF], // 12 bright blue
    [0xFF, 0x55, 0xFF], // 13 bright magenta
    [0x55, 0xFF, 0xFF], // 14 bright cyan
    [0xFF, 0xFF, 0xFF], // 15 bright white
];

/// Upper bound on the per-cell scale factor (keeps `GLYPH_* * scale` well
/// away from overflow and absurd allocations).
const MAX_SCALE: u32 = 64;
/// Hard ceiling for [`PngOptions::max_dimension`] regardless of caller.
const MAX_DIMENSION_CAP: u32 = 16_384;

/// Options controlling [`render_png`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngOptions {
    /// Integer scale factor applied to each cell. `1` yields the native
    /// [`GLYPH_WIDTH`]×[`GLYPH_HEIGHT`] pixels per cell; clamped to
    /// `1..=64`.
    pub scale: u32,
    /// Maximum output width *and* height in pixels. The cell grid is
    /// cropped (from the top-left) so neither axis exceeds this, bounding
    /// memory for large or hostile canvases. Clamped to `1..=16384`.
    pub max_dimension: u32,
}

impl Default for PngOptions {
    fn default() -> Self {
        PngOptions {
            scale: 1,
            max_dimension: 4096,
        }
    }
}

impl PngOptions {
    /// Options for a small preview: native cell size, tight dimension cap.
    pub fn thumbnail() -> Self {
        PngOptions {
            scale: 1,
            max_dimension: 640,
        }
    }
}

/// Rasterize `canvas` to an RGB PNG using the embedded CP437 font and the
/// 16-color VGA palette, honoring foreground/background and the reverse
/// attribute. Returns the encoded PNG bytes (empty only if encoding fails,
/// which cannot happen for the in-memory sink).
pub fn render_png(canvas: &Canvas, opts: &PngOptions) -> Vec<u8> {
    let scale = opts.scale.clamp(1, MAX_SCALE);
    let max_dim = opts.max_dimension.clamp(1, MAX_DIMENSION_CAP);

    let cell_w = GLYPH_WIDTH as u32 * scale;
    let cell_h = GLYPH_HEIGHT as u32 * scale;

    // How many whole cells fit inside the dimension cap on each axis.
    let max_cols = (max_dim / cell_w) as usize;
    let max_rows = (max_dim / cell_h) as usize;
    let cols = canvas.width().min(max_cols);
    let rows = canvas.height().min(max_rows);

    // Guarantee a valid (>=1x1) image even for an empty/over-capped canvas.
    let width = (cols as u32 * cell_w).max(1);
    let height = (rows as u32 * cell_h).max(1);

    let scale = scale as usize;
    let stride = width as usize * 3;
    let mut buf = vec![0u8; stride * height as usize];

    for (cy, row) in canvas.rows().take(rows).enumerate() {
        let py0 = cy * GLYPH_HEIGHT * scale;
        for cx in 0..cols {
            let cell = row.get(cx).copied().unwrap_or_default();
            let (fg, bg) = cell_colors(&cell);
            let glyph = glyph_for(cell.ch);
            let px0 = cx * GLYPH_WIDTH * scale;
            for (gy, &bits) in glyph.iter().enumerate() {
                for gx in 0..GLYPH_WIDTH {
                    let on = bits & (0x80 >> gx) != 0;
                    let color = if on { fg } else { bg };
                    // Paint the scale×scale block for this glyph pixel.
                    for sy in 0..scale {
                        let py = py0 + gy * scale + sy;
                        let mut off = py * stride + (px0 + gx * scale) * 3;
                        for _ in 0..scale {
                            buf[off..off + 3].copy_from_slice(&color);
                            off += 3;
                        }
                    }
                }
            }
        }
    }

    encode_rgb(width, height, &buf)
}

/// Resolve a cell's foreground/background RGB, applying reverse video.
fn cell_colors(cell: &Cell) -> ([u8; 3], [u8; 3]) {
    let fg = PALETTE[cell.fg as usize & 0x0F];
    let bg = PALETTE[cell.bg as usize & 0x0F];
    if cell.attrs.contains(Attrs::REVERSE) {
        (bg, fg)
    } else {
        (fg, bg)
    }
}

/// The 8×16 bitmap for `ch`, or a blank glyph when it has no CP437 byte.
fn glyph_for(ch: char) -> &'static [u8; GLYPH_HEIGHT] {
    match unicode_to_cp437(ch) {
        Some(byte) => &FONT_8X16[byte as usize],
        None => &FONT_8X16[0x00], // 0x00 is blank in the VGA font
    }
}

/// Encode a tightly-packed RGB8 buffer as a PNG into a fresh `Vec<u8>`.
fn encode_rgb(width: u32, height: u32, buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    if let Ok(mut writer) = encoder.write_header() {
        // Writing to a `Vec` never fails; ignore the infallible result.
        let _ = writer.write_image_data(buf);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::parse;

    /// Decode a PNG and return `(width, height, rgb_pixels)`.
    fn decode(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(png_bytes);
        let mut reader = decoder.read_info().expect("valid PNG header");
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("valid PNG frame");
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    fn pixel(px: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
        let off = (y as usize * width as usize + x as usize) * 3;
        [px[off], px[off + 1], px[off + 2]]
    }

    #[test]
    fn png_has_signature_and_expected_dimensions() {
        // The canvas is a fixed-width grid (default 80 columns), one row.
        let canvas = parse(b"\x1b[31;44mAB");
        assert_eq!(canvas.height(), 1);
        let png = render_png(&canvas, &PngOptions::default());
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

        let (w, h, _px) = decode(&png);
        assert_eq!(w, canvas.width() as u32 * GLYPH_WIDTH as u32);
        assert_eq!(h, canvas.height() as u32 * GLYPH_HEIGHT as u32);
    }

    #[test]
    fn scale_multiplies_cell_size() {
        let canvas = parse(b"X");
        let opts = PngOptions {
            scale: 3,
            max_dimension: 4096,
        };
        let png = render_png(&canvas, &opts);
        let (w, h, _) = decode(&png);
        assert_eq!(w, canvas.width() as u32 * GLYPH_WIDTH as u32 * 3);
        assert_eq!(h, canvas.height() as u32 * GLYPH_HEIGHT as u32 * 3);
    }

    #[test]
    fn full_block_cell_is_pure_foreground() {
        // 0xDB is the full block glyph (all pixels set), so every pixel in
        // the cell must be the foreground color. Bright green on blue.
        let canvas = parse(b"\x1b[1;32;44m\xDB");
        let png = render_png(&canvas, &PngOptions::default());
        let (w, _h, px) = decode(&png);
        // Center pixel of the single cell.
        let center = pixel(&px, w, GLYPH_WIDTH as u32 / 2, GLYPH_HEIGHT as u32 / 2);
        assert_eq!(center, PALETTE[10]); // bright green
    }

    #[test]
    fn background_shows_where_glyph_is_empty() {
        // A space glyph is blank, so pixels are the background color.
        let canvas = parse(b"\x1b[37;41m \xDB");
        let png = render_png(&canvas, &PngOptions::default());
        let (w, _h, px) = decode(&png);
        let bg_pixel = pixel(&px, w, GLYPH_WIDTH as u32 / 2, GLYPH_HEIGHT as u32 / 2);
        assert_eq!(bg_pixel, PALETTE[1]); // red background of the space cell
    }

    #[test]
    fn reverse_attr_swaps_fg_and_bg() {
        let normal = cell_colors(&Cell {
            ch: '█',
            fg: 2,
            bg: 4,
            attrs: Attrs::NONE,
        });
        let reversed = cell_colors(&Cell {
            ch: '█',
            fg: 2,
            bg: 4,
            attrs: Attrs::REVERSE,
        });
        assert_eq!(normal, (PALETTE[2], PALETTE[4]));
        assert_eq!(reversed, (PALETTE[4], PALETTE[2]));
    }

    #[test]
    fn max_dimension_crops_the_grid() {
        // Force a canvas wider/taller than a tiny cap; output must not
        // exceed the cap and must still be a valid PNG.
        let mut bytes = Vec::new();
        for _ in 0..10 {
            bytes.extend_from_slice(b"XXXXXXXXXX\r\n");
        }
        let canvas = parse(&bytes);
        let opts = PngOptions {
            scale: 1,
            max_dimension: 20, // only two 8px cells / one 16px row fit
        };
        let png = render_png(&canvas, &opts);
        let (w, h, _) = decode(&png);
        assert!(w <= 20 && h <= 20, "got {w}x{h}");
        assert_eq!(w, 2 * GLYPH_WIDTH as u32); // 16px, the most that fits
        assert_eq!(h, GLYPH_HEIGHT as u32); // one 16px row
    }

    #[test]
    fn empty_canvas_yields_valid_png_with_min_height() {
        // A zero-row canvas still produces a valid PNG; height floors to 1.
        let canvas = parse(b"");
        assert_eq!(canvas.height(), 0);
        let png = render_png(&canvas, &PngOptions::default());
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        let (w, h, _) = decode(&png);
        assert!(w >= 1);
        assert_eq!(h, 1);
    }

    #[test]
    fn unknown_glyphs_fall_back_to_blank() {
        // A char with no CP437 mapping resolves to the blank (0x00) glyph.
        assert_eq!(glyph_for('你'), &FONT_8X16[0x00]);
        assert!(glyph_for('你').iter().all(|&row| row == 0));
        // Known glyphs still map through.
        assert_eq!(glyph_for('A'), &FONT_8X16[0x41]);
        assert_eq!(glyph_for('█'), &FONT_8X16[0xDB]);
    }

    /// Fuzz-ish: arbitrary canvases must rasterize without panicking and
    /// stay within the dimension cap.
    #[test]
    fn arbitrary_canvas_never_panics() {
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        for _ in 0..6 {
            let mut bytes = Vec::with_capacity(4096);
            for _ in 0..4096 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let b = (state >> 56) as u8;
                bytes.push(if b == 0x1A { b'.' } else { b });
            }
            let canvas = parse(&bytes);
            for opts in [
                PngOptions::default(),
                PngOptions::thumbnail(),
                PngOptions {
                    scale: 5,
                    max_dimension: 256,
                },
                PngOptions {
                    scale: 0,
                    max_dimension: 0,
                }, // clamped internally
            ] {
                let png = render_png(&canvas, &opts);
                assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
            }
        }
    }
}
