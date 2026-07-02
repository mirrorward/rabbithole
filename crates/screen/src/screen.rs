//! The [`Screen`] cell buffer and its wire serialization.
//!
//! A [`Screen`] is a fixed `width × height` grid of [`Cell`]s plus a drawing
//! "pen" (current position, colors, and attributes). Drawing methods are
//! chainable builders that mutate the grid; nothing touches a socket until
//! [`Screen::flush`] serializes the whole grid to a byte vector for a chosen
//! [`ScreenMode`]. The two modes differ only in how a glyph reaches the
//! wire — a Unicode codepoint (UTF-8) or a single CP437 byte — so the same
//! surface drives a modern terminal and a legacy telnet/SyncTERM client.
//!
//! Line-drawing uses the CP437 box glyphs (0xB3 `│`, 0xC4 `─`, 0xDA `┌`, …)
//! stored as their Unicode equivalents; [`rabbithole_art::cp437`] maps them
//! back to the original bytes on a CP437 flush, so box art survives the
//! round trip byte-for-byte.

use rabbithole_art::ansi::{Attrs, Cell};
use rabbithole_art::cp437::unicode_to_cp437;

use crate::color::Color;

/// How a [`Screen`] serializes glyphs to the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenMode {
    /// Modern terminals: emit Unicode glyphs as UTF-8, plus ANSI SGR.
    Utf8,
    /// Classic terminals / SyncTERM: emit CP437 bytes, plus ANSI SGR.
    Cp437Ansi,
}

/// Single- vs double-line box drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxStyle {
    /// `┌─┐ │ └─┘` (CP437 0xDA/0xC4/0xBF/0xB3/0xC0/0xD9).
    Single,
    /// `╔═╗ ║ ╚═╝` (CP437 0xC9/0xCD/0xBB/0xBA/0xC8/0xBC).
    Double,
}

impl BoxStyle {
    /// `(top_left, top_right, bottom_left, bottom_right, horizontal, vertical)`.
    const fn glyphs(self) -> (char, char, char, char, char, char) {
        match self {
            // 0xDA, 0xBF, 0xC0, 0xD9, 0xC4, 0xB3
            BoxStyle::Single => ('┌', '┐', '└', '┘', '─', '│'),
            // 0xC9, 0xBB, 0xC8, 0xBC, 0xCD, 0xBA
            BoxStyle::Double => ('╔', '╗', '╚', '╝', '═', '║'),
        }
    }
}

/// The current drawing pen: the style applied to newly written cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pen {
    fg: u8,
    bg: u8,
    attrs: Attrs,
}

impl Default for Pen {
    fn default() -> Self {
        // Match `Cell::default()` so an untouched screen flushes as blanks.
        Pen {
            fg: 7,
            bg: 0,
            attrs: Attrs::NONE,
        }
    }
}

/// A fixed-size grid of character cells with a chainable drawing API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    width: usize,
    height: usize,
    cells: Vec<Cell>,
    cursor: (usize, usize),
    pen: Pen,
    mode: ScreenMode,
}

impl Screen {
    /// Create a `width × height` screen filled with blank cells. Both
    /// dimensions are clamped to at least 1. `mode` is the surface's native
    /// mode: it governs [`Screen::print`]'s lossy CP437 normalization. A
    /// buffer can still be flushed to either mode later.
    pub fn new(width: usize, height: usize, mode: ScreenMode) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Screen {
            width,
            height,
            cells: vec![Cell::default(); width * height],
            cursor: (0, 0),
            pen: Pen::default(),
            mode,
        }
    }

    /// Screen width in cells.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Screen height in cells.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The surface's native mode (as passed to [`Screen::new`]).
    pub fn mode(&self) -> ScreenMode {
        self.mode
    }

    /// Current cursor position as `(x, y)`, zero-based.
    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// Borrow the cell at `(x, y)`, or `None` if out of bounds.
    pub fn cell(&self, x: usize, y: usize) -> Option<&Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells.get(y * self.width + x)
    }

    /// Move the pen to `(x, y)`, clamped to the last valid cell.
    pub fn move_to(&mut self, x: usize, y: usize) -> &mut Self {
        self.cursor = (x.min(self.width - 1), y.min(self.height - 1));
        self
    }

    /// Set the foreground color for subsequent writes.
    pub fn fg(&mut self, color: Color) -> &mut Self {
        self.pen.fg = color.index();
        self
    }

    /// Set the background color for subsequent writes.
    pub fn bg(&mut self, color: Color) -> &mut Self {
        self.pen.bg = color.index();
        self
    }

    /// Toggle the bold attribute for subsequent writes.
    pub fn bold(&mut self, on: bool) -> &mut Self {
        self.set_attr(Attrs::BOLD, on)
    }

    /// Toggle reverse-video for subsequent writes.
    pub fn reverse(&mut self, on: bool) -> &mut Self {
        self.set_attr(Attrs::REVERSE, on)
    }

    /// Toggle the blink attribute for subsequent writes.
    pub fn blink(&mut self, on: bool) -> &mut Self {
        self.set_attr(Attrs::BLINK, on)
    }

    fn set_attr(&mut self, attr: Attrs, on: bool) -> &mut Self {
        if on {
            self.pen.attrs.insert(attr);
        } else {
            self.pen.attrs.remove(attr);
        }
        self
    }

    /// Reset the pen to the default style (light-gray on black, no
    /// attributes). Does not move the cursor.
    pub fn reset_style(&mut self) -> &mut Self {
        self.pen = Pen::default();
        self
    }

    /// Clear the whole buffer to blank cells and home the cursor. The pen
    /// style is preserved.
    pub fn clear(&mut self) -> &mut Self {
        self.cells.fill(Cell::default());
        self.cursor = (0, 0);
        self
    }

    /// Normalize a glyph for the surface's native mode: in CP437 mode any
    /// codepoint outside the code page becomes `'?'`.
    fn normalize(&self, ch: char) -> char {
        match self.mode {
            ScreenMode::Utf8 => ch,
            ScreenMode::Cp437Ansi => {
                if unicode_to_cp437(ch).is_some() {
                    ch
                } else {
                    '?'
                }
            }
        }
    }

    /// Write `ch` at `(x, y)` with the current pen. Out-of-bounds writes are
    /// dropped. Does not move the cursor.
    pub fn put(&mut self, x: usize, y: usize, ch: char) -> &mut Self {
        if x < self.width && y < self.height {
            let ch = self.normalize(ch);
            self.cells[y * self.width + x] = Cell {
                ch,
                fg: self.pen.fg,
                bg: self.pen.bg,
                attrs: self.pen.attrs,
            };
        }
        self
    }

    /// Print a string starting at the cursor, advancing left-to-right and
    /// wrapping at the right margin. `'\n'` moves to column 0 of the next
    /// row; `'\r'` returns to column 0. Writing past the bottom row stops.
    /// Glyphs are CP437-lossy in [`ScreenMode::Cp437Ansi`], Unicode in
    /// [`ScreenMode::Utf8`].
    pub fn print(&mut self, text: &str) -> &mut Self {
        for ch in text.chars() {
            match ch {
                '\n' => {
                    self.cursor = (0, self.cursor.1 + 1);
                }
                '\r' => {
                    self.cursor.0 = 0;
                }
                _ => {
                    let (x, y) = self.cursor;
                    if y >= self.height {
                        break;
                    }
                    if x < self.width {
                        self.put(x, y, ch);
                    }
                    // Advance with wrap.
                    if x + 1 >= self.width {
                        self.cursor = (0, y + 1);
                    } else {
                        self.cursor = (x + 1, y);
                    }
                }
            }
            if self.cursor.1 >= self.height {
                // Past the bottom edge: nothing more can land on screen.
                break;
            }
        }
        self
    }

    /// Draw a horizontal rule (a full-width `─` line) across row `y` using
    /// the current pen.
    pub fn hrule(&mut self, y: usize) -> &mut Self {
        if y < self.height {
            for x in 0..self.width {
                self.put(x, y, '─');
            }
        }
        self
    }

    /// Draw a box with its top-left corner at `(x, y)`, `w` cells wide and
    /// `h` cells tall, using the current pen. `w`/`h` below 2 are ignored
    /// (a box needs two rows/columns for its borders). The interior is left
    /// untouched.
    pub fn draw_box(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        style: BoxStyle,
    ) -> &mut Self {
        if w < 2 || h < 2 {
            return self;
        }
        let (tl, tr, bl, br, horiz, vert) = style.glyphs();
        let right = x + w - 1;
        let bottom = y + h - 1;

        // Corners.
        self.put(x, y, tl);
        self.put(right, y, tr);
        self.put(x, bottom, bl);
        self.put(right, bottom, br);

        // Top and bottom edges.
        for col in (x + 1)..right {
            self.put(col, y, horiz);
            self.put(col, bottom, horiz);
        }
        // Left and right edges.
        for row in (y + 1)..bottom {
            self.put(x, row, vert);
            self.put(right, row, vert);
        }
        self
    }

    /// Serialize the whole buffer to wire bytes for `mode`.
    ///
    /// The stream begins with an SGR reset, a clear-screen, and a cursor
    /// home (`ESC[0m ESC[2J ESC[H`). Each cell emits its glyph; a fresh SGR
    /// sequence is written only when the style changes from the previous
    /// cell, so runs of one style cost a single escape. Rows are separated
    /// by CRLF and the stream ends with a final SGR reset. Glyphs are UTF-8
    /// in [`ScreenMode::Utf8`] and single CP437 bytes (lossy) in
    /// [`ScreenMode::Cp437Ansi`].
    pub fn flush(&self, mode: ScreenMode) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");

        let mut last: Option<(u8, u8, Attrs)> = None;
        for (row_idx, row) in self.cells.chunks(self.width).enumerate() {
            if row_idx > 0 {
                out.extend_from_slice(b"\r\n");
            }
            for cell in row {
                let style = (cell.fg, cell.bg, cell.attrs);
                if last != Some(style) {
                    push_sgr(&mut out, cell);
                    last = Some(style);
                }
                push_glyph(&mut out, cell.ch, mode);
            }
        }
        out.extend_from_slice(b"\x1b[0m");
        out
    }
}

/// Append `ch` to `out` in the encoding for `mode`.
fn push_glyph(out: &mut Vec<u8>, ch: char, mode: ScreenMode) {
    match mode {
        ScreenMode::Utf8 => {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
        ScreenMode::Cp437Ansi => {
            out.push(unicode_to_cp437(ch).unwrap_or(b'?'));
        }
    }
}

/// Append a full `SGR` reset-and-set for `cell`'s style.
fn push_sgr(out: &mut Vec<u8>, cell: &Cell) {
    // Start from a reset so the sequence fully specifies the style.
    out.extend_from_slice(b"\x1b[0");
    if cell.attrs.contains(Attrs::BOLD) {
        out.extend_from_slice(b";1");
    }
    if cell.attrs.contains(Attrs::BLINK) {
        out.extend_from_slice(b";5");
    }
    if cell.attrs.contains(Attrs::REVERSE) {
        out.extend_from_slice(b";7");
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
    out.push(b';');
    out.extend_from_slice(fg.to_string().as_bytes());
    out.push(b';');
    out.extend_from_slice(bg.to_string().as_bytes());
    out.push(b'm');
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_art::ansi::{self};
    use rabbithole_art::render::render_plain;

    fn cell_at(screen: &Screen, x: usize, y: usize) -> Cell {
        *screen.cell(x, y).expect("cell in range")
    }

    #[test]
    fn new_screen_is_blank_and_clamped() {
        let s = Screen::new(0, 0, ScreenMode::Utf8);
        assert_eq!((s.width(), s.height()), (1, 1));
        assert_eq!(cell_at(&s, 0, 0), Cell::default());
    }

    #[test]
    fn print_places_glyphs_with_pen() {
        let mut s = Screen::new(10, 2, ScreenMode::Utf8);
        s.fg(Color::BrightGreen)
            .bg(Color::Blue)
            .move_to(2, 0)
            .print("Hi");
        let c = cell_at(&s, 2, 0);
        assert_eq!(c.ch, 'H');
        assert_eq!(c.fg, Color::BrightGreen.index());
        assert_eq!(c.bg, Color::Blue.index());
        assert_eq!(cell_at(&s, 3, 0).ch, 'i');
        // Untouched cell stays default.
        assert_eq!(cell_at(&s, 0, 0), Cell::default());
    }

    #[test]
    fn print_wraps_at_right_margin() {
        let mut s = Screen::new(3, 2, ScreenMode::Utf8);
        s.print("abcd");
        assert_eq!(cell_at(&s, 2, 0).ch, 'c');
        assert_eq!(cell_at(&s, 0, 1).ch, 'd');
    }

    #[test]
    fn print_newline_moves_down_and_home() {
        let mut s = Screen::new(5, 3, ScreenMode::Utf8);
        s.print("ab\ncd");
        assert_eq!(cell_at(&s, 0, 0).ch, 'a');
        assert_eq!(cell_at(&s, 0, 1).ch, 'c');
        assert_eq!(cell_at(&s, 1, 1).ch, 'd');
    }

    #[test]
    fn print_stops_past_bottom() {
        let mut s = Screen::new(2, 1, ScreenMode::Utf8);
        s.print("abcd"); // only "ab" fits
        assert_eq!(cell_at(&s, 0, 0).ch, 'a');
        assert_eq!(cell_at(&s, 1, 0).ch, 'b');
        // Nothing panicked; cursor parked at the bottom edge.
        assert_eq!(s.cursor().1, 1);
    }

    #[test]
    fn cp437_mode_print_is_lossy() {
        let mut s = Screen::new(4, 1, ScreenMode::Cp437Ansi);
        s.print("a中"); // '中' has no CP437 byte
        assert_eq!(cell_at(&s, 0, 0).ch, 'a');
        assert_eq!(cell_at(&s, 1, 0).ch, '?');
    }

    #[test]
    fn utf8_mode_print_keeps_unicode() {
        let mut s = Screen::new(4, 1, ScreenMode::Utf8);
        s.print("中");
        assert_eq!(cell_at(&s, 0, 0).ch, '中');
    }

    #[test]
    fn clear_blanks_and_homes() {
        let mut s = Screen::new(4, 2, ScreenMode::Utf8);
        s.move_to(2, 1).print("x").clear();
        assert_eq!(cell_at(&s, 2, 1), Cell::default());
        assert_eq!(s.cursor(), (0, 0));
    }

    #[test]
    fn single_box_places_corner_and_edge_glyphs() {
        let mut s = Screen::new(6, 4, ScreenMode::Utf8);
        s.draw_box(0, 0, 4, 3, BoxStyle::Single);
        assert_eq!(cell_at(&s, 0, 0).ch, '┌');
        assert_eq!(cell_at(&s, 3, 0).ch, '┐');
        assert_eq!(cell_at(&s, 0, 2).ch, '└');
        assert_eq!(cell_at(&s, 3, 2).ch, '┘');
        assert_eq!(cell_at(&s, 1, 0).ch, '─');
        assert_eq!(cell_at(&s, 0, 1).ch, '│');
        // Interior untouched.
        assert_eq!(cell_at(&s, 1, 1), Cell::default());
    }

    #[test]
    fn double_box_uses_double_glyphs() {
        let mut s = Screen::new(6, 4, ScreenMode::Utf8);
        s.draw_box(0, 0, 4, 3, BoxStyle::Double);
        assert_eq!(cell_at(&s, 0, 0).ch, '╔');
        assert_eq!(cell_at(&s, 3, 0).ch, '╗');
        assert_eq!(cell_at(&s, 0, 2).ch, '╚');
        assert_eq!(cell_at(&s, 3, 2).ch, '╝');
        assert_eq!(cell_at(&s, 1, 0).ch, '═');
        assert_eq!(cell_at(&s, 0, 1).ch, '║');
    }

    #[test]
    fn degenerate_box_is_ignored() {
        let mut s = Screen::new(4, 4, ScreenMode::Utf8);
        s.draw_box(0, 0, 1, 3, BoxStyle::Single);
        assert_eq!(cell_at(&s, 0, 0), Cell::default());
    }

    #[test]
    fn hrule_fills_row() {
        let mut s = Screen::new(3, 2, ScreenMode::Utf8);
        s.hrule(1);
        for x in 0..3 {
            assert_eq!(cell_at(&s, x, 1).ch, '─');
        }
        assert_eq!(cell_at(&s, 0, 0), Cell::default());
    }

    #[test]
    fn flush_starts_with_reset_clear_home() {
        let s = Screen::new(2, 1, ScreenMode::Utf8);
        let out = s.flush(ScreenMode::Utf8);
        assert!(out.starts_with(b"\x1b[0m\x1b[2J\x1b[H"));
        assert!(out.ends_with(b"\x1b[0m"));
    }

    #[test]
    fn flush_utf8_emits_unicode_box_glyph() {
        let mut s = Screen::new(1, 1, ScreenMode::Utf8);
        s.put(0, 0, '─');
        let out = s.flush(ScreenMode::Utf8);
        // '─' is U+2500 -> UTF-8 E2 94 80.
        assert!(
            out.windows(3).any(|w| w == [0xE2, 0x94, 0x80]),
            "expected UTF-8 bytes for '─' in {out:?}"
        );
    }

    #[test]
    fn flush_cp437_emits_single_byte_glyph() {
        let mut s = Screen::new(1, 1, ScreenMode::Cp437Ansi);
        s.put(0, 0, '─');
        let out = s.flush(ScreenMode::Cp437Ansi);
        // '─' -> CP437 0xC4, a single byte (no multi-byte UTF-8).
        assert!(out.contains(&0xC4));
        assert!(!out.windows(3).any(|w| w == [0xE2, 0x94, 0x80]));
    }

    #[test]
    fn flush_emits_expected_sgr_and_coalesces_runs() {
        let mut s = Screen::new(3, 1, ScreenMode::Utf8);
        s.fg(Color::Red).bg(Color::Blue).print("ab");
        s.fg(Color::BrightYellow).print("c");
        let out = String::from_utf8(s.flush(ScreenMode::Utf8)).unwrap();
        // Red(1)/Blue(4): SGR 31/44 emitted once, then "ab".
        assert!(out.contains("\x1b[0;31;44mab"), "got {out:?}");
        // BrightYellow(11): bright fg SGR 93, base bg 44 carried.
        assert!(out.contains("\x1b[0;93;44mc"), "got {out:?}");
    }

    #[test]
    fn flush_bright_and_attr_sgr() {
        let mut s = Screen::new(1, 1, ScreenMode::Utf8);
        s.reverse(true)
            .blink(true)
            .fg(Color::BrightWhite)
            .bg(Color::Black)
            .put(0, 0, 'x');
        let out = String::from_utf8(s.flush(ScreenMode::Utf8)).unwrap();
        assert!(out.contains("\x1b[0;5;7;97;40mx"), "got {out:?}");
    }

    #[test]
    fn cp437_flush_round_trips_through_art_parser() {
        let mut s = Screen::new(12, 3, ScreenMode::Cp437Ansi);
        s.draw_box(0, 0, 12, 3, BoxStyle::Double);
        s.fg(Color::BrightCyan).move_to(1, 1).print("RabbitHole");
        let wire = s.flush(ScreenMode::Cp437Ansi);

        // Feed the CP437/ANSI bytes back through the art parser.
        let canvas = ansi::parse_with(&wire, 12, false);
        let text = render_plain(&canvas);
        // The visible art survives the round trip byte-for-byte.
        assert!(text.contains("╔══════════╗"), "got {text:?}");
        assert!(text.contains("║RabbitHole║"), "got {text:?}");
        assert!(text.contains("╚══════════╝"), "got {text:?}");
    }
}
