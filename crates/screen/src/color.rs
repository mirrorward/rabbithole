//! The classic 16-color text palette.
//!
//! BBS-era terminals speak a 16-entry palette: eight base colors (ANSI SGR
//! 30–37 foreground / 40–47 background) plus their eight bright variants
//! (90–97 / 100–107, historically reached via the "bold" bit). Both the
//! modern UTF-8 surface and the CP437/ANSI socket surface resolve to these
//! same indices, which line up 1:1 with [`rabbithole_art::Cell`]'s `fg`/`bg`
//! fields so a flushed screen round-trips cleanly through the art parser.

/// A palette entry in the standard 16-color text terminal palette.
///
/// The discriminant is the palette index (0–15): 0–7 are the base colors,
/// 8–15 the bright half. This matches the resolved indices produced by the
/// `rabbithole-art` ANSI parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Color {
    /// Palette 0.
    Black = 0,
    /// Palette 1.
    Red = 1,
    /// Palette 2.
    Green = 2,
    /// Palette 3 (brown on a real VGA).
    Yellow = 3,
    /// Palette 4.
    Blue = 4,
    /// Palette 5.
    Magenta = 5,
    /// Palette 6.
    Cyan = 6,
    /// Palette 7 (light gray).
    White = 7,
    /// Palette 8 (dark gray).
    BrightBlack = 8,
    /// Palette 9.
    BrightRed = 9,
    /// Palette 10.
    BrightGreen = 10,
    /// Palette 11.
    BrightYellow = 11,
    /// Palette 12.
    BrightBlue = 12,
    /// Palette 13.
    BrightMagenta = 13,
    /// Palette 14.
    BrightCyan = 14,
    /// Palette 15 (pure white).
    BrightWhite = 15,
}

impl Color {
    /// The 0–15 palette index for this color.
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// Whether this is one of the eight bright entries (index ≥ 8).
    pub const fn is_bright(self) -> bool {
        self.index() >= 8
    }

    /// The color for a palette index, wrapping the low four bits so any byte
    /// is total. Indices 0–15 map exactly.
    pub const fn from_index(index: u8) -> Color {
        match index & 0x0F {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::White,
            8 => Color::BrightBlack,
            9 => Color::BrightRed,
            10 => Color::BrightGreen,
            11 => Color::BrightYellow,
            12 => Color::BrightBlue,
            13 => Color::BrightMagenta,
            14 => Color::BrightCyan,
            _ => Color::BrightWhite,
        }
    }
}

impl From<Color> for u8 {
    fn from(color: Color) -> u8 {
        color.index()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_matches_discriminant() {
        assert_eq!(Color::Black.index(), 0);
        assert_eq!(Color::White.index(), 7);
        assert_eq!(Color::BrightBlack.index(), 8);
        assert_eq!(Color::BrightWhite.index(), 15);
    }

    #[test]
    fn from_index_round_trips_every_entry() {
        for i in 0..16u8 {
            assert_eq!(Color::from_index(i).index(), i);
        }
    }

    #[test]
    fn from_index_wraps_high_bytes() {
        assert_eq!(Color::from_index(16), Color::Black);
        assert_eq!(Color::from_index(255), Color::BrightWhite);
    }

    #[test]
    fn brightness_split() {
        assert!(!Color::White.is_bright());
        assert!(Color::BrightBlack.is_bright());
    }
}
