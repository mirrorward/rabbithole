//! Canvas renderers: project a parsed [`Canvas`] onto modern targets.
//!
//! The parser deliberately splits "execute the ANSI program" from "draw
//! the result" so one canvas can feed many surfaces. This module covers
//! the two text targets:
//!
//! - [`render_ansi`]: fresh SGR sequences plus Unicode glyphs for a modern
//!   UTF-8 terminal. Colors are re-emitted from the *resolved* 16-color
//!   palette (bright via 90–97/100–107), so the output looks right even on
//!   terminals that don't treat bold as bright — and blink/reverse ride
//!   along for the retro purists.
//! - [`render_plain`]: glyphs only, for logs, search indexing, and clients
//!   that can't do color.
//! - [`render_html`]: a `<pre>` of styled `<span>` runs for the web client,
//!   using the same 16-color VGA palette as hex.
//!
//! PNG raster thumbnails live alongside these in [`crate::raster`].

use crate::ansi::{Attrs, Canvas, Cell};
use crate::raster::PALETTE;

/// Render to ANSI escape sequences + Unicode for a modern terminal.
///
/// Each line ends with `SGR 0` and a newline so colors never bleed into
/// surrounding output; trailing default-blank cells are trimmed.
pub fn render_ansi(canvas: &Canvas) -> String {
    let mut out = String::new();
    let default = Cell::default();
    for row in canvas.rows() {
        let end = row
            .iter()
            .rposition(|cell| *cell != default)
            .map_or(0, |i| i + 1);
        // (fg, bg, attrs) of the most recently emitted SGR.
        let mut state: Option<(u8, u8, Attrs)> = None;
        for cell in &row[..end] {
            let sgr = (cell.fg, cell.bg, cell.attrs);
            if state != Some(sgr) {
                push_sgr(&mut out, cell);
                state = Some(sgr);
            }
            out.push(cell.ch);
        }
        if state.is_some() {
            out.push_str("\x1b[0m");
        }
        out.push('\n');
    }
    out
}

/// Render glyphs only, stripping all color and attributes. Trailing
/// whitespace is trimmed per line.
pub fn render_plain(canvas: &Canvas) -> String {
    let mut out = String::new();
    for row in canvas.rows() {
        let end = row
            .iter()
            .rposition(|cell| cell.ch != ' ')
            .map_or(0, |i| i + 1);
        out.extend(row[..end].iter().map(|cell| cell.ch));
        out.push('\n');
    }
    out
}

/// Emit a full `SGR` reset-and-set for `cell`.
///
/// Bold is *not* re-emitted: the parser already folded it into a bright
/// foreground index, and emitting both would double-apply on terminals
/// that render bold as bright.
fn push_sgr(out: &mut String, cell: &Cell) {
    out.push_str("\x1b[0");
    if cell.attrs.contains(Attrs::BLINK) {
        out.push_str(";5");
    }
    if cell.attrs.contains(Attrs::REVERSE) {
        out.push_str(";7");
    }
    let fg = if cell.fg < 8 {
        30 + u16::from(cell.fg)
    } else {
        90 + u16::from(cell.fg - 8)
    };
    let bg = if cell.bg < 8 {
        40 + u16::from(cell.bg)
    } else {
        100 + u16::from(cell.bg - 8)
    };
    out.push(';');
    out.push_str(&fg.to_string());
    out.push(';');
    out.push_str(&bg.to_string());
    out.push('m');
}

/// Render to an HTML `<pre>` block for embedding in the web client.
///
/// Each line becomes runs of `<span style="color:#rrggbb;background:#rrggbb">`
/// coalesced across cells that share the same foreground, background, and
/// attributes; the reverse attribute swaps the two colors. Glyph text is
/// HTML-escaped. Colors come from the same 16-color VGA [`PALETTE`] the PNG
/// renderer uses. Trailing default-blank cells are trimmed per line, matching
/// [`render_ansi`].
pub fn render_html(canvas: &Canvas) -> String {
    let mut out = String::new();
    out.push_str("<pre class=\"ansi-art\">");
    let default = Cell::default();
    for row in canvas.rows() {
        let end = row
            .iter()
            .rposition(|cell| *cell != default)
            .map_or(0, |i| i + 1);
        let mut i = 0;
        while i < end {
            let key = style_key(&row[i]);
            let mut text = String::new();
            while i < end && style_key(&row[i]) == key {
                push_escaped(&mut text, row[i].ch);
                i += 1;
            }
            let (fg, bg) = key;
            out.push_str("<span style=\"color:");
            push_hex(&mut out, fg);
            out.push_str(";background:");
            push_hex(&mut out, bg);
            out.push_str("\">");
            out.push_str(&text);
            out.push_str("</span>");
        }
        out.push('\n');
    }
    out.push_str("</pre>");
    out
}

/// The (foreground, background) palette indices a cell renders with, after
/// applying reverse video.
fn style_key(cell: &Cell) -> (u8, u8) {
    let fg = cell.fg & 0x0F;
    let bg = cell.bg & 0x0F;
    if cell.attrs.contains(Attrs::REVERSE) {
        (bg, fg)
    } else {
        (fg, bg)
    }
}

/// Append `#rrggbb` for palette index `idx` (masked to 0–15).
fn push_hex(out: &mut String, idx: u8) {
    let [r, g, b] = PALETTE[idx as usize & 0x0F];
    out.push('#');
    for byte in [r, g, b] {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0F));
    }
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'a' + nibble - 10) as char,
    }
}

/// Append `ch` to `out`, HTML-escaping the characters that are significant
/// inside `<pre>` content.
fn push_escaped(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        _ => out.push(ch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::{parse, parse_with};

    #[test]
    fn plain_render_strips_attributes() {
        let canvas = parse(b"\x1b[1;31mRed\x1b[0m and \x1b[5;44mblink");
        assert_eq!(render_plain(&canvas), "Red and blink\n");
    }

    #[test]
    fn plain_render_keeps_grid_shape() {
        let canvas = parse(b"a\r\n\r\n\x1b[3;5Hb");
        assert_eq!(render_plain(&canvas), "a\n\n    b\n");
    }

    #[test]
    fn ansi_render_emits_colors_and_reset() {
        let canvas = parse(b"\x1b[31;44mX");
        assert_eq!(render_ansi(&canvas), "\x1b[0;31;44mX\x1b[0m\n");
    }

    #[test]
    fn ansi_render_uses_bright_codes_for_high_palette() {
        // Bold red foreground resolves to palette 9 -> SGR 91.
        let canvas = parse(b"\x1b[1;31mX");
        assert_eq!(render_ansi(&canvas), "\x1b[0;91;40mX\x1b[0m\n");
        // iCE bright background resolves to palette 12 -> SGR 104.
        let canvas = parse_with(b"\x1b[5;44mX", 80, true);
        assert_eq!(render_ansi(&canvas), "\x1b[0;37;104mX\x1b[0m\n");
    }

    #[test]
    fn ansi_render_carries_blink_and_reverse() {
        let canvas = parse(b"\x1b[5;7;33;41mX");
        assert_eq!(render_ansi(&canvas), "\x1b[0;5;7;33;41mX\x1b[0m\n");
    }

    #[test]
    fn ansi_render_coalesces_runs_of_same_style() {
        let canvas = parse(b"\x1b[32mab\x1b[33mc");
        assert_eq!(
            render_ansi(&canvas),
            "\x1b[0;32;40mab\x1b[0;33;40mc\x1b[0m\n"
        );
    }

    #[test]
    fn ansi_render_emits_unicode_glyphs() {
        let canvas = parse(&[0xC9, 0xCD, 0xBB]);
        assert_eq!(render_ansi(&canvas), "\x1b[0;37;40m╔═╗\x1b[0m\n");
    }

    #[test]
    fn trailing_blanks_are_trimmed_but_colored_blanks_kept() {
        // A colored space is content (background art!), not padding.
        let canvas = parse(b"a\x1b[44m \x1b[0m   ");
        let ansi = render_ansi(&canvas);
        assert_eq!(ansi, "\x1b[0;37;40ma\x1b[0;37;44m \x1b[0m\n");
        // Plain rendering treats any space as trimmable.
        assert_eq!(render_plain(&canvas), "a\n");
    }

    #[test]
    fn empty_canvas_renders_nothing() {
        let canvas = parse(b"");
        assert_eq!(render_ansi(&canvas), "");
        assert_eq!(render_plain(&canvas), "");
    }

    #[test]
    fn blank_lines_between_content_are_preserved() {
        let canvas = parse(b"top\r\n\r\nbottom");
        assert_eq!(render_plain(&canvas), "top\n\nbottom\n");
        let ansi = render_ansi(&canvas);
        assert_eq!(ansi.lines().count(), 3);
        assert_eq!(ansi.lines().nth(1), Some(""));
    }

    #[test]
    fn html_wraps_in_pre_with_color_spans() {
        let canvas = parse(b"\x1b[31;44mX");
        let html = render_html(&canvas);
        assert!(html.starts_with("<pre class=\"ansi-art\">"));
        assert!(html.ends_with("</pre>"));
        // Red (#aa0000) on blue (#0000aa), palette indices 1 and 4.
        assert!(
            html.contains("<span style=\"color:#aa0000;background:#0000aa\">X</span>"),
            "got {html}"
        );
    }

    #[test]
    fn html_escapes_markup_characters() {
        let canvas = parse(b"<a> & \"b\" 'c'");
        let html = render_html(&canvas);
        assert!(
            html.contains("&lt;a&gt; &amp; &quot;b&quot; &#39;c&#39;"),
            "got {html}"
        );
        assert!(!html.contains("<a>"));
    }

    #[test]
    fn html_coalesces_runs_and_swaps_on_reverse() {
        // Two greens then a yellow: one span for "ab", one for "c".
        let canvas = parse(b"\x1b[32mab\x1b[33mc");
        let html = render_html(&canvas);
        assert!(
            html.contains("background:#000000\">ab</span>"),
            "got {html}"
        );
        // Reverse video swaps fg/bg in the emitted style.
        let canvas = parse(b"\x1b[7;32;41mZ");
        let html = render_html(&canvas);
        // fg green(2)/bg red(1) reversed -> color red, background green.
        assert!(
            html.contains("color:#aa0000;background:#00aa00\">Z</span>"),
            "got {html}"
        );
    }

    #[test]
    fn html_empty_canvas_is_empty_pre() {
        let canvas = parse(b"");
        assert_eq!(render_html(&canvas), "<pre class=\"ansi-art\"></pre>");
    }

    /// Fuzz-ish: arbitrary canvases must render to HTML without panicking.
    #[test]
    fn html_arbitrary_canvas_never_panics() {
        let mut state: u64 = 0xDEAD_BEEF_CAFE_F00D;
        for _ in 0..4 {
            let mut bytes = Vec::with_capacity(2048);
            for _ in 0..2048 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let b = (state >> 56) as u8;
                bytes.push(if b == 0x1A { b'.' } else { b });
            }
            let canvas = parse(&bytes);
            let html = render_html(&canvas);
            assert!(html.starts_with("<pre"));
        }
    }
}
