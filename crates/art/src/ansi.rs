//! ANSI/VT100 escape-sequence parser producing a cell grid.
//!
//! `.ans` files are not text — they are programs for a VT100-ish terminal:
//! CP437 bytes interleaved with CSI sequences for color (SGR), cursor
//! movement (CUP/CUU/CUD/CUF/CUB, save/restore), and erasing (ED/EL). To
//! display them anywhere *other* than a real DOS screen we first "execute"
//! them into a [`Canvas`] of [`Cell`]s, which renderers then project to
//! modern terminals, plain text, or (later) images.
//!
//! Two era quirks are honored:
//! - **Bold = bright foreground**: SGR 1 selects the bright half of the
//!   16-color palette rather than a heavier font.
//! - **iCE colors**: art groups repurposed the blink attribute as a bright
//!   *background* bit. When iCE mode is on (set by the caller, usually from
//!   the SAUCE `TFlags`, or via the `CSI ?33 h/l` private sequence) SGR 5
//!   brightens the background instead of blinking.
//!
//! The parser is total: unknown or malformed sequences are skipped, cursor
//! jumps are clamped, and canvas growth is capped, so arbitrary bytes can
//! never panic or exhaust memory.

use crate::cp437::cp437_to_unicode;

/// Hard cap on canvas height so hostile input cannot allocate unbounded
/// memory (80 × 10_000 cells is ~6 MiB, plenty for any real art scroll).
pub const MAX_HEIGHT: usize = 10_000;

/// Default canvas width: the eternal 80 columns.
pub const DEFAULT_WIDTH: usize = 80;

/// Cap on a single numeric CSI parameter.
const MAX_PARAM: u16 = 32_767;
/// Cap on the number of CSI parameters we keep.
const MAX_PARAMS: usize = 32;

/// Character-cell attribute flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Attrs(u8);

impl Attrs {
    /// No attributes.
    pub const NONE: Attrs = Attrs(0);
    /// Bold (in classic ANSI art this means "bright foreground").
    pub const BOLD: Attrs = Attrs(1);
    /// Blink (only set when iCE colors are *off*; otherwise it becomes a
    /// bright background).
    pub const BLINK: Attrs = Attrs(1 << 1);
    /// Reverse video.
    pub const REVERSE: Attrs = Attrs(1 << 2);

    pub fn contains(self, other: Attrs) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Attrs) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Attrs) {
        self.0 &= !other.0;
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Attrs {
    type Output = Attrs;
    fn bitor(self, rhs: Attrs) -> Attrs {
        Attrs(self.0 | rhs.0)
    }
}

/// One character cell: a Unicode glyph plus resolved 16-color palette
/// indices (0–15; bold/iCE brightening is already applied) and attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: u8,
    pub bg: u8,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: 7,
            bg: 0,
            attrs: Attrs::NONE,
        }
    }
}

/// A grid of [`Cell`]s: fixed width, height grows as content is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    cells: Vec<Cell>,
    width: usize,
    height: usize,
    /// Cursor position as `(column, row)`, zero-based.
    cursor: (usize, usize),
}

impl Canvas {
    pub fn new(width: usize) -> Self {
        Canvas {
            cells: Vec::new(),
            width: width.max(1),
            height: 0,
            cursor: (0, 0),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// The cell at `(x, y)`, or `None` outside the canvas.
    pub fn cell(&self, x: usize, y: usize) -> Option<&Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells.get(y * self.width + x)
    }

    /// Iterate over rows as cell slices.
    pub fn rows(&self) -> impl Iterator<Item = &[Cell]> {
        self.cells.chunks(self.width)
    }

    /// Grow the canvas (with default cells) so row `y` exists.
    fn ensure_row(&mut self, y: usize) {
        let y = y.min(MAX_HEIGHT - 1);
        if y >= self.height {
            self.height = y + 1;
            self.cells.resize(self.height * self.width, Cell::default());
        }
    }

    fn set(&mut self, x: usize, y: usize, cell: Cell) {
        self.ensure_row(y);
        if x < self.width && y < self.height {
            self.cells[y * self.width + x] = cell;
        }
    }

    fn clear_all(&mut self) {
        self.cells.fill(Cell::default());
    }

    /// Clear cells in the half-open linear range, clamped to the canvas.
    fn clear_range(&mut self, from: usize, to: usize) {
        let len = self.cells.len();
        let from = from.min(len);
        let to = to.min(len);
        if from < to {
            self.cells[from..to].fill(Cell::default());
        }
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas::new(DEFAULT_WIDTH)
    }
}

/// Current SGR pen state (the "graphic rendition" between writes).
#[derive(Debug, Clone, Copy)]
struct Pen {
    /// Foreground palette index as *selected* (0–7 from SGR 30–37, 8–15
    /// from 90–97); bold brightening is applied when a cell is written.
    fg: u8,
    /// Background palette index as selected (0–7 or 8–15).
    bg: u8,
    bold: bool,
    blink: bool,
    reverse: bool,
}

impl Default for Pen {
    fn default() -> Self {
        Pen {
            fg: 7,
            bg: 0,
            bold: false,
            blink: false,
            reverse: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    /// `ESC` + intermediate byte(s) (e.g. charset designation `ESC ( B`):
    /// consume until the final byte, then discard the whole sequence.
    EscapeIntermediate,
    Csi,
}

/// Streaming ANSI parser. Feed it bytes; take the [`Canvas`] when done.
#[derive(Debug)]
pub struct AnsiParser {
    canvas: Canvas,
    state: State,
    pen: Pen,
    saved_cursor: Option<(usize, usize)>,
    ice_colors: bool,
    /// CSI parameter accumulator.
    params: Vec<u16>,
    /// Current (possibly still-accumulating) CSI parameter.
    cur_param: Option<u16>,
    /// CSI sequence started with a private marker (`?`, `<`, `=`, `>`).
    private: bool,
    /// A SUB (0x1A) byte was seen: everything after it (SAUCE etc.) is
    /// metadata, not screen content.
    done: bool,
}

impl AnsiParser {
    pub fn new() -> Self {
        Self::with_width(DEFAULT_WIDTH)
    }

    pub fn with_width(width: usize) -> Self {
        AnsiParser {
            canvas: Canvas::new(width),
            state: State::Ground,
            pen: Pen::default(),
            saved_cursor: None,
            ice_colors: false,
            params: Vec::new(),
            cur_param: None,
            private: false,
            done: false,
        }
    }

    /// Enable/disable iCE color mode (blink bit selects bright background).
    /// Usually driven by the SAUCE record's `TFlags` bit 0.
    pub fn set_ice_colors(&mut self, on: bool) {
        self.ice_colors = on;
    }

    pub fn ice_colors(&self) -> bool {
        self.ice_colors
    }

    /// Process a chunk of CP437/ANSI bytes.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.done {
                return;
            }
            self.step(b);
        }
    }

    /// Finish parsing and return the canvas.
    pub fn finish(self) -> Canvas {
        self.canvas
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    fn step(&mut self, b: u8) {
        match self.state {
            State::Ground => self.ground(b),
            State::Escape => self.escape(b),
            State::EscapeIntermediate => self.escape_intermediate(b),
            State::Csi => self.csi(b),
        }
    }

    fn ground(&mut self, b: u8) {
        match b {
            0x1B => self.state = State::Escape,
            0x1A => self.done = true,
            b'\r' => self.canvas.cursor.0 = 0,
            b'\n' => {
                let y = &mut self.canvas.cursor.1;
                *y = (*y + 1).min(MAX_HEIGHT - 1);
            }
            b'\t' => {
                let (x, _) = self.canvas.cursor;
                let next = (x / 8 + 1) * 8;
                self.canvas.cursor.0 = next.min(self.canvas.width.saturating_sub(1));
            }
            // Other C0 controls (NUL, BEL, BS, ...) are ignored rather than
            // printed: real art relies on their glyphs only via `put_char`
            // paths that don't exist here, and skipping matches ansilove.
            0x00 | 0x07 | 0x08 | 0x0B | 0x0C | 0x0E..=0x19 | 0x1C..=0x1F => {}
            _ => self.put_char(cp437_to_unicode(b)),
        }
    }

    fn escape(&mut self, b: u8) {
        match b {
            b'[' => {
                self.params.clear();
                self.cur_param = None;
                self.private = false;
                self.state = State::Csi;
            }
            // DECSC/DECRC: ESC 7 / ESC 8 save/restore cursor.
            b'7' => {
                self.saved_cursor = Some(self.canvas.cursor);
                self.state = State::Ground;
            }
            b'8' => {
                if let Some(cur) = self.saved_cursor {
                    self.canvas.cursor = cur;
                }
                self.state = State::Ground;
            }
            0x1B => {} // ESC ESC: stay in escape state.
            // Intermediate byte (charset designation etc.): the sequence
            // continues until a final byte; consume and discard it.
            0x20..=0x2F => self.state = State::EscapeIntermediate,
            // Anything else (unsupported single-byte ESC codes): skip the
            // byte and return to ground.
            _ => self.state = State::Ground,
        }
    }

    fn escape_intermediate(&mut self, b: u8) {
        match b {
            0x20..=0x2F => {}
            0x1B => self.state = State::Escape,
            // Final byte (or anything unexpected): drop the sequence.
            _ => self.state = State::Ground,
        }
    }

    fn csi(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                let d = u16::from(b - b'0');
                let v = self.cur_param.unwrap_or(0);
                self.cur_param = Some(v.saturating_mul(10).saturating_add(d).min(MAX_PARAM));
            }
            b';' | b':' => {
                self.push_param();
            }
            b'?' | b'<' | b'=' | b'>' => self.private = true,
            0x1B => self.state = State::Escape,
            // Intermediate bytes: ignore (we don't support any sequences
            // that use them, but we must still consume up to the final).
            0x20..=0x2F => {}
            // Final byte: dispatch and return to ground.
            0x40..=0x7E => {
                self.push_param();
                self.dispatch_csi(b);
                self.state = State::Ground;
            }
            // C0 controls or stray bytes inside a CSI: abort the sequence.
            _ => self.state = State::Ground,
        }
    }

    fn push_param(&mut self) {
        if self.params.len() < MAX_PARAMS {
            self.params.push(self.cur_param.unwrap_or(0));
        }
        self.cur_param = None;
    }

    /// Parameter `i`, mapping missing/zero to `default`.
    fn param_or(&self, i: usize, default: u16) -> u16 {
        match self.params.get(i).copied().unwrap_or(0) {
            0 => default,
            v => v,
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        if self.private {
            self.dispatch_private(final_byte);
            return;
        }
        let (x, y) = self.canvas.cursor;
        let width = self.canvas.width;
        match final_byte {
            b'm' => self.sgr(),
            b'A' => self.canvas.cursor.1 = y.saturating_sub(self.param_or(0, 1) as usize),
            b'B' => self.canvas.cursor.1 = (y + self.param_or(0, 1) as usize).min(MAX_HEIGHT - 1),
            b'C' => self.canvas.cursor.0 = (x + self.param_or(0, 1) as usize).min(width - 1),
            b'D' => self.canvas.cursor.0 = x.saturating_sub(self.param_or(0, 1) as usize),
            b'H' | b'f' => {
                let row = self.param_or(0, 1) as usize - 1;
                let col = self.param_or(1, 1) as usize - 1;
                self.canvas.cursor = (col.min(width - 1), row.min(MAX_HEIGHT - 1));
            }
            b's' => self.saved_cursor = Some(self.canvas.cursor),
            b'u' => {
                if let Some(cur) = self.saved_cursor {
                    self.canvas.cursor = cur;
                }
            }
            b'J' => self.erase_display(),
            b'K' => self.erase_line(),
            // Unknown final byte: tolerated, skipped.
            _ => {}
        }
    }

    fn dispatch_private(&mut self, final_byte: u8) {
        // CSI ?33h / ?33l: SyncTERM's iCE color toggle. Everything else
        // private (cursor visibility, screen modes, ...) is skipped.
        match final_byte {
            b'h' if self.params.first() == Some(&33) => self.ice_colors = true,
            b'l' if self.params.first() == Some(&33) => self.ice_colors = false,
            _ => {}
        }
    }

    fn sgr(&mut self) {
        // `CSI m` with no parameters means reset.
        if self.params.is_empty() {
            self.pen = Pen::default();
            return;
        }
        for i in 0..self.params.len() {
            let p = self.params[i];
            match p {
                0 => self.pen = Pen::default(),
                1 => self.pen.bold = true,
                5 | 6 => self.pen.blink = true,
                7 => self.pen.reverse = true,
                22 => self.pen.bold = false,
                25 => self.pen.blink = false,
                27 => self.pen.reverse = false,
                30..=37 => self.pen.fg = (p - 30) as u8,
                39 => self.pen.fg = 7,
                90..=97 => self.pen.fg = (p - 90) as u8 + 8,
                40..=47 => self.pen.bg = (p - 40) as u8,
                49 => self.pen.bg = 0,
                100..=107 => self.pen.bg = (p - 100) as u8 + 8,
                // 38/48 (256-color / truecolor) and everything else are
                // outside the 16-color art palette: skip.
                _ => {}
            }
        }
    }

    fn erase_display(&mut self) {
        let (x, y) = self.canvas.cursor;
        let width = self.canvas.width;
        match self.params.first().copied().unwrap_or(0) {
            0 => {
                let from = y * width + x;
                let to = self.canvas.cells.len();
                self.canvas.clear_range(from, to);
            }
            1 => self.canvas.clear_range(0, y * width + x + 1),
            2 | 3 => {
                self.canvas.clear_all();
                // Most DOS-era renderers home the cursor on ED 2.
                self.canvas.cursor = (0, 0);
            }
            _ => {}
        }
    }

    fn erase_line(&mut self) {
        let (x, y) = self.canvas.cursor;
        let width = self.canvas.width;
        if y >= self.canvas.height {
            return;
        }
        let line = y * width;
        match self.params.first().copied().unwrap_or(0) {
            0 => self.canvas.clear_range(line + x, line + width),
            1 => self.canvas.clear_range(line, line + x + 1),
            2 => self.canvas.clear_range(line, line + width),
            _ => {}
        }
    }

    fn put_char(&mut self, ch: char) {
        let pen = self.pen;
        let fg = if pen.bold && pen.fg < 8 {
            pen.fg + 8
        } else {
            pen.fg
        };
        let bg = if self.ice_colors && pen.blink && pen.bg < 8 {
            pen.bg + 8
        } else {
            pen.bg
        };
        let mut attrs = Attrs::NONE;
        if pen.bold {
            attrs.insert(Attrs::BOLD);
        }
        if pen.blink && !self.ice_colors {
            attrs.insert(Attrs::BLINK);
        }
        if pen.reverse {
            attrs.insert(Attrs::REVERSE);
        }
        let (x, y) = self.canvas.cursor;
        self.canvas.set(x, y, Cell { ch, fg, bg, attrs });
        // Advance with wrap at the right margin.
        if x + 1 >= self.canvas.width {
            self.canvas.cursor = (0, (y + 1).min(MAX_HEIGHT - 1));
        } else {
            self.canvas.cursor = (x + 1, y);
        }
    }
}

impl Default for AnsiParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a complete CP437/ANSI byte stream at the default 80 columns.
pub fn parse(bytes: &[u8]) -> Canvas {
    parse_with(bytes, DEFAULT_WIDTH, false)
}

/// Parse with explicit width and iCE color mode (typically taken from the
/// file's SAUCE record).
pub fn parse_with(bytes: &[u8], width: usize, ice_colors: bool) -> Canvas {
    let mut parser = AnsiParser::with_width(width);
    parser.set_ice_colors(ice_colors);
    parser.feed(bytes);
    parser.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(canvas: &Canvas, x: usize, y: usize) -> Cell {
        *canvas.cell(x, y).expect("cell in range")
    }

    #[test]
    fn plain_text_lands_in_row_zero() {
        let canvas = parse(b"Hi!");
        assert_eq!(canvas.height(), 1);
        assert_eq!(cell(&canvas, 0, 0).ch, 'H');
        assert_eq!(cell(&canvas, 1, 0).ch, 'i');
        assert_eq!(cell(&canvas, 2, 0).ch, '!');
        assert_eq!(cell(&canvas, 0, 0).fg, 7);
        assert_eq!(cell(&canvas, 0, 0).bg, 0);
        assert!(cell(&canvas, 0, 0).attrs.is_empty());
    }

    #[test]
    fn cp437_bytes_decode_to_glyphs() {
        let canvas = parse(&[0xDB, 0xB0, 0x01]);
        assert_eq!(cell(&canvas, 0, 0).ch, '█');
        assert_eq!(cell(&canvas, 1, 0).ch, '░');
        assert_eq!(cell(&canvas, 2, 0).ch, '☺');
    }

    #[test]
    fn crlf_moves_to_next_line() {
        let canvas = parse(b"ab\r\ncd");
        assert_eq!(cell(&canvas, 0, 0).ch, 'a');
        assert_eq!(cell(&canvas, 0, 1).ch, 'c');
        assert_eq!(cell(&canvas, 1, 1).ch, 'd');
        assert_eq!(canvas.height(), 2);
    }

    #[test]
    fn bare_lf_keeps_column() {
        let canvas = parse(b"ab\ncd");
        // LF moves down only; CR is what returns to column 0.
        assert_eq!(cell(&canvas, 2, 1).ch, 'c');
        assert_eq!(cell(&canvas, 3, 1).ch, 'd');
    }

    #[test]
    fn tab_advances_to_next_stop() {
        let canvas = parse(b"a\tb");
        assert_eq!(cell(&canvas, 0, 0).ch, 'a');
        assert_eq!(cell(&canvas, 8, 0).ch, 'b');
    }

    #[test]
    fn wraps_at_right_margin() {
        let line: Vec<u8> = std::iter::repeat_n(b'x', 81).collect();
        let canvas = parse(&line);
        assert_eq!(canvas.height(), 2);
        assert_eq!(cell(&canvas, 79, 0).ch, 'x');
        assert_eq!(cell(&canvas, 0, 1).ch, 'x');
        assert_eq!(canvas.cursor(), (1, 1));
    }

    #[test]
    fn sgr_basic_colors() {
        let canvas = parse(b"\x1b[31;44mX");
        let c = cell(&canvas, 0, 0);
        assert_eq!(c.fg, 1);
        assert_eq!(c.bg, 4);
    }

    #[test]
    fn sgr_bold_brightens_foreground() {
        let canvas = parse(b"\x1b[1;34mX\x1b[22mY");
        assert_eq!(cell(&canvas, 0, 0).fg, 12); // bright blue
        assert!(cell(&canvas, 0, 0).attrs.contains(Attrs::BOLD));
        assert_eq!(cell(&canvas, 1, 0).fg, 4); // bold off -> base blue
    }

    #[test]
    fn sgr_bright_ranges() {
        let canvas = parse(b"\x1b[95;103mX");
        let c = cell(&canvas, 0, 0);
        assert_eq!(c.fg, 13);
        assert_eq!(c.bg, 11);
    }

    #[test]
    fn sgr_reset_restores_defaults() {
        let canvas = parse(b"\x1b[1;31;44mA\x1b[0mB\x1b[33mC\x1b[mD");
        assert_eq!(
            cell(&canvas, 1, 0),
            Cell {
                ch: 'B',
                ..Cell::default()
            }
        );
        // Bare `CSI m` also resets.
        assert_eq!(
            cell(&canvas, 3, 0),
            Cell {
                ch: 'D',
                ..Cell::default()
            }
        );
    }

    #[test]
    fn sgr_default_color_params() {
        let canvas = parse(b"\x1b[31;44m\x1b[39;49mX");
        let c = cell(&canvas, 0, 0);
        assert_eq!(c.fg, 7);
        assert_eq!(c.bg, 0);
    }

    #[test]
    fn blink_without_ice_sets_attr() {
        let canvas = parse(b"\x1b[5;41mX");
        let c = cell(&canvas, 0, 0);
        assert!(c.attrs.contains(Attrs::BLINK));
        assert_eq!(c.bg, 1);
    }

    #[test]
    fn blink_with_ice_brightens_background() {
        let canvas = parse_with(b"\x1b[5;41mX\x1b[25mY", DEFAULT_WIDTH, true);
        let x = cell(&canvas, 0, 0);
        assert!(!x.attrs.contains(Attrs::BLINK));
        assert_eq!(x.bg, 9); // bright red background
        let y = cell(&canvas, 1, 0);
        assert_eq!(y.bg, 1); // blink off -> base red
    }

    #[test]
    fn ice_mode_via_private_sequence() {
        let canvas = parse(b"\x1b[?33h\x1b[5;44mX\x1b[?33lY");
        assert_eq!(cell(&canvas, 0, 0).bg, 12);
        assert_eq!(cell(&canvas, 1, 0).bg, 4);
        assert!(cell(&canvas, 1, 0).attrs.contains(Attrs::BLINK));
    }

    #[test]
    fn reverse_video_attr() {
        let canvas = parse(b"\x1b[7mX\x1b[27mY");
        assert!(cell(&canvas, 0, 0).attrs.contains(Attrs::REVERSE));
        assert!(!cell(&canvas, 1, 0).attrs.contains(Attrs::REVERSE));
    }

    #[test]
    fn cursor_position_cup() {
        let canvas = parse(b"\x1b[3;5HX");
        assert_eq!(cell(&canvas, 4, 2).ch, 'X');
        // `f` (HVP) is an alias.
        let canvas = parse(b"\x1b[2;2fY");
        assert_eq!(cell(&canvas, 1, 1).ch, 'Y');
        // Missing params default to 1;1 (home).
        let canvas = parse(b"abc\x1b[HZ");
        assert_eq!(cell(&canvas, 0, 0).ch, 'Z');
    }

    #[test]
    fn cursor_relative_moves() {
        let canvas = parse(b"\x1b[5;10H\x1b[2A\x1b[3C\x1b[1B\x1b[4DX");
        // Start (9,4): up 2 -> (9,2), fwd 3 -> (12,2), down 1 -> (12,3),
        // back 4 -> (8,3).
        assert_eq!(cell(&canvas, 8, 3).ch, 'X');
    }

    #[test]
    fn cursor_moves_clamp_at_edges() {
        let canvas = parse(b"\x1b[99A\x1b[999DX");
        assert_eq!(cell(&canvas, 0, 0).ch, 'X');
        let canvas = parse(b"\x1b[999CX");
        assert_eq!(cell(&canvas, 79, 0).ch, 'X');
        // Column clamps to the width even for CUP.
        let canvas = parse(b"\x1b[1;999HY");
        assert_eq!(cell(&canvas, 79, 0).ch, 'Y');
    }

    #[test]
    fn cursor_save_restore_csi_and_esc() {
        let canvas = parse(b"\x1b[2;3H\x1b[s\x1b[5;5H\x1b[uX");
        assert_eq!(cell(&canvas, 2, 1).ch, 'X');
        let canvas = parse(b"\x1b[4;7H\x1b7\x1b[1;1H\x1b8Y");
        assert_eq!(cell(&canvas, 6, 3).ch, 'Y');
        // Restore without save is a no-op.
        let canvas = parse(b"ab\x1b[uZ");
        assert_eq!(cell(&canvas, 2, 0).ch, 'Z');
    }

    #[test]
    fn erase_display_variants() {
        // ED 2 clears everything and homes the cursor.
        let canvas = parse(b"hello\x1b[2JX");
        assert_eq!(cell(&canvas, 0, 0).ch, 'X');
        assert_eq!(cell(&canvas, 1, 0).ch, ' ');
        // ED 0: cursor to end.
        let canvas = parse(b"abcdef\x1b[1;3H\x1b[0J");
        assert_eq!(cell(&canvas, 1, 0).ch, 'b');
        assert_eq!(cell(&canvas, 2, 0).ch, ' ');
        assert_eq!(cell(&canvas, 5, 0).ch, ' ');
        // ED 1: start through cursor.
        let canvas = parse(b"abcdef\x1b[1;3H\x1b[1J");
        assert_eq!(cell(&canvas, 0, 0).ch, ' ');
        assert_eq!(cell(&canvas, 2, 0).ch, ' ');
        assert_eq!(cell(&canvas, 3, 0).ch, 'd');
    }

    #[test]
    fn erase_line_variants() {
        let canvas = parse(b"abcdef\r\nsecond\x1b[1;3H\x1b[K");
        assert_eq!(cell(&canvas, 1, 0).ch, 'b');
        assert_eq!(cell(&canvas, 2, 0).ch, ' ');
        assert_eq!(cell(&canvas, 5, 0).ch, ' ');
        assert_eq!(cell(&canvas, 0, 1).ch, 's'); // other line untouched

        let canvas = parse(b"abcdef\x1b[1;3H\x1b[1K");
        assert_eq!(cell(&canvas, 0, 0).ch, ' ');
        assert_eq!(cell(&canvas, 2, 0).ch, ' ');
        assert_eq!(cell(&canvas, 3, 0).ch, 'd');

        let canvas = parse(b"abcdef\x1b[2K");
        assert_eq!(cell(&canvas, 0, 0).ch, ' ');
        assert_eq!(cell(&canvas, 5, 0).ch, ' ');
    }

    #[test]
    fn erased_cells_are_default_colored() {
        let canvas = parse(b"\x1b[31;44mabc\x1b[1;1H\x1b[2K");
        assert_eq!(cell(&canvas, 0, 0), Cell::default());
    }

    #[test]
    fn sub_byte_stops_parsing() {
        let canvas = parse(b"ok\x1aSAUCE00garbage that must not render");
        assert_eq!(canvas.height(), 1);
        assert_eq!(cell(&canvas, 0, 0).ch, 'o');
        assert_eq!(cell(&canvas, 2, 0).ch, ' ');
    }

    #[test]
    fn unknown_sequences_are_skipped() {
        // Unknown final bytes, private modes, and ESC codes must vanish.
        let canvas = parse(b"a\x1b[10;20zb\x1b[?25lc\x1b(Bd\x1b[38;5;196me");
        assert_eq!(cell(&canvas, 0, 0).ch, 'a');
        assert_eq!(cell(&canvas, 1, 0).ch, 'b');
        assert_eq!(cell(&canvas, 2, 0).ch, 'c');
        assert_eq!(cell(&canvas, 3, 0).ch, 'd');
        let e = cell(&canvas, 4, 0);
        assert_eq!(e.ch, 'e');
        assert_eq!(e.fg, 7); // 256-color SGR ignored
    }

    #[test]
    fn malformed_sequences_do_not_panic() {
        // Truncated escape at EOF.
        let _ = parse(b"abc\x1b");
        let _ = parse(b"abc\x1b[");
        let _ = parse(b"abc\x1b[31;");
        // Huge parameter values are clamped.
        let canvas = parse(b"\x1b[99999999999999999;99999999999999999HX");
        assert!(canvas.height() <= MAX_HEIGHT);
        // Absurd numbers of parameters are capped.
        let mut seq = b"\x1b[".to_vec();
        for _ in 0..1000 {
            seq.extend_from_slice(b"1;");
        }
        seq.extend_from_slice(b"mX");
        let canvas = parse(&seq);
        assert_eq!(cell(&canvas, 0, 0).ch, 'X');
    }

    #[test]
    fn esc_inside_csi_restarts_sequence() {
        let canvas = parse(b"\x1b[31\x1b[32mX");
        // The aborted sequence is dropped; the second one wins.
        assert_eq!(cell(&canvas, 0, 0).fg, 2);
    }

    #[test]
    fn height_growth_is_capped() {
        // Repeated CUD + writes cannot exceed MAX_HEIGHT.
        let mut bytes = Vec::new();
        for _ in 0..20 {
            bytes.extend_from_slice(b"\x1b[9999Bx");
        }
        let canvas = parse(&bytes);
        assert!(canvas.height() <= MAX_HEIGHT);
        assert_eq!(cell(&canvas, canvas.cursor().0 - 1, MAX_HEIGHT - 1).ch, 'x');
    }

    #[test]
    fn streaming_feed_matches_one_shot() {
        let bytes = b"\x1b[1;33mHi \x1b[0;44mthere\x1b[K!";
        let whole = parse(bytes);
        let mut parser = AnsiParser::new();
        for chunk in bytes.chunks(3) {
            parser.feed(chunk);
        }
        assert_eq!(parser.finish(), whole);
    }

    /// Fuzz-ish safety net: random byte soup must never panic and must
    /// stay within the memory cap.
    #[test]
    fn random_bytes_never_panic() {
        let mut state: u64 = 0x243F_6A88_85A3_08D3;
        for seed in 0..8u64 {
            state ^= seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut bytes = Vec::with_capacity(16 * 1024);
            let mut sub_free = Vec::with_capacity(16 * 1024);
            for _ in 0..16 * 1024 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let b = (state >> 56) as u8;
                bytes.push(b);
                // Variant without SUB so parsing doesn't stop early.
                sub_free.push(if b == 0x1A { b'.' } else { b });
            }
            for input in [&bytes, &sub_free] {
                let canvas = parse(input);
                assert!(canvas.height() <= MAX_HEIGHT);
                // Rendering the result must also be total.
                let _ = crate::render::render_ansi(&canvas);
                let _ = crate::render::render_plain(&canvas);
            }
        }
    }
}
