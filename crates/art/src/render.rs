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
//!
//! PNG thumbnails are a later slice; they will consume the same canvas.

use crate::ansi::{Attrs, Canvas, Cell};

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
}
