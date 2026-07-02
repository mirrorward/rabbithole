//! A boxed, titled menu — the building block for the BBS UI.
//!
//! [`Menu`] renders a titled box with one row per item and the selected row
//! drawn in reverse video. It is deliberately tiny: it draws onto a
//! [`Screen`] using that screen's current pen (so callers control colors),
//! then leaves serialization to [`Screen::flush`]. The same widget powers
//! the local TUI and the telnet surface.

use crate::screen::{BoxStyle, Screen};

/// One space of padding on each side of the widest item, inside the box.
const PAD: usize = 1;

/// A titled, boxed list of selectable items.
#[derive(Clone, Debug)]
pub struct Menu<'a> {
    title: &'a str,
    items: &'a [&'a str],
    selected: usize,
    style: BoxStyle,
}

impl<'a> Menu<'a> {
    /// A single-line-boxed menu with the given title and items, initially
    /// selecting the first item.
    pub fn new(title: &'a str, items: &'a [&'a str]) -> Self {
        Menu {
            title,
            items,
            selected: 0,
            style: BoxStyle::Single,
        }
    }

    /// Set the selected item index (clamped to the last item on render).
    pub fn select(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Use a specific box style.
    pub fn style(mut self, style: BoxStyle) -> Self {
        self.style = style;
        self
    }

    /// The selected index, clamped to a valid item (0 when empty).
    pub fn selected(&self) -> usize {
        if self.items.is_empty() {
            0
        } else {
            self.selected.min(self.items.len() - 1)
        }
    }

    /// The inner content width: the widest of the title and items.
    fn content_width(&self) -> usize {
        let widest_item = self
            .items
            .iter()
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(0);
        widest_item.max(self.title.chars().count()).max(1) + PAD * 2
    }

    /// Total width of the rendered box, including borders.
    pub fn width(&self) -> usize {
        self.content_width() + 2
    }

    /// Total height of the rendered box, including borders.
    pub fn height(&self) -> usize {
        self.items.len() + 2
    }

    /// Draw the menu with its top-left corner at `(x, y)` on `screen`.
    ///
    /// The title is centered on the top border; each item occupies one
    /// interior row. The selected row is filled in reverse video across the
    /// full interior width so it reads as a highlight bar. The pen is
    /// restored to its incoming reverse state when done.
    pub fn render(&self, screen: &mut Screen, x: usize, y: usize) {
        let content_w = self.content_width();
        screen.draw_box(x, y, self.width(), self.height(), self.style);

        // Title centered on the top border row.
        if !self.title.is_empty() {
            let tlen = self.title.chars().count();
            let offset = content_w.saturating_sub(tlen) / 2;
            screen.move_to(x + 1 + offset, y).print(self.title);
        }

        let selected = self.selected();
        for (i, item) in self.items.iter().enumerate() {
            let row = y + 1 + i;
            let highlight = i == selected;
            screen.reverse(highlight);
            // Left-padded, then space-filled to the full interior width so
            // the reverse-video bar spans the whole row.
            let mut line = String::with_capacity(content_w);
            for _ in 0..PAD {
                line.push(' ');
            }
            line.push_str(item);
            let used = PAD + item.chars().count();
            for _ in used..content_w {
                line.push(' ');
            }
            screen.move_to(x + 1, row).print(&line);
            screen.reverse(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::screen::ScreenMode;
    use rabbithole_art::ansi::{self, Attrs};
    use rabbithole_art::render::render_plain;

    #[test]
    fn dimensions_track_widest_line() {
        let menu = Menu::new("Main", &["Chat", "Message Boards", "Files"]);
        // Widest item "Message Boards" = 14, +2 pad, +2 border = 18.
        assert_eq!(menu.width(), 18);
        assert_eq!(menu.height(), 5); // 3 items + 2 borders
    }

    #[test]
    fn selected_is_clamped() {
        let menu = Menu::new("t", &["a", "b"]).select(9);
        assert_eq!(menu.selected(), 1);
        let empty = Menu::new("t", &[]);
        assert_eq!(empty.selected(), 0);
    }

    #[test]
    fn render_draws_box_title_and_items() {
        let mut s = Screen::new(20, 6, ScreenMode::Utf8);
        Menu::new("MAIN", &["Chat", "Files"]).render(&mut s, 0, 0);
        // Box corner.
        assert_eq!(s.cell(0, 0).unwrap().ch, '┌');
        // Title centered on the top border.
        let canvas_row: String = (0..s.width()).map(|x| s.cell(x, 0).unwrap().ch).collect();
        assert!(canvas_row.contains("MAIN"), "top row was {canvas_row:?}");
        // First item text.
        assert_eq!(s.cell(1, 1).unwrap().ch, ' '); // left pad
        assert_eq!(s.cell(2, 1).unwrap().ch, 'C');
    }

    #[test]
    fn selected_row_is_reverse_video() {
        let mut s = Screen::new(20, 6, ScreenMode::Utf8);
        Menu::new("M", &["one", "two"])
            .select(1)
            .render(&mut s, 0, 0);
        // Row for "two" (index 1) is at y = 2.
        let sel = s.cell(2, 2).unwrap();
        assert_eq!(sel.ch, 't');
        assert!(sel.attrs.contains(Attrs::REVERSE));
        // The unselected row is not reversed.
        let unsel = s.cell(2, 1).unwrap();
        assert_eq!(unsel.ch, 'o');
        assert!(!unsel.attrs.contains(Attrs::REVERSE));
    }

    #[test]
    fn flush_uses_reverse_sgr_on_selected_row() {
        let mut s = Screen::new(16, 6, ScreenMode::Cp437Ansi);
        s.fg(Color::White).bg(Color::Blue);
        Menu::new("MENU", &["Alpha", "Beta"])
            .select(0)
            .render(&mut s, 0, 0);
        let wire = s.flush(ScreenMode::Cp437Ansi);

        // The highlighted row's SGR carries the reverse (";7") parameter.
        let text = String::from_utf8_lossy(&wire);
        assert!(text.contains(";7"), "no reverse SGR in flush: {text:?}");

        // Round-trip: the visible menu text is intact after CP437 parsing.
        let canvas = ansi::parse_with(&wire, 16, false);
        let plain = render_plain(&canvas);
        assert!(plain.contains("Alpha"), "got {plain:?}");
        assert!(plain.contains("Beta"), "got {plain:?}");
        assert!(plain.contains("MENU"), "got {plain:?}");
    }
}
