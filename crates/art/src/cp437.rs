//! CP437 (IBM PC "OEM") ↔ Unicode conversion.
//!
//! Every ANSI/ASCII art file from the BBS era is a stream of CP437 bytes,
//! so faithful conversion is the foundation of the whole art pipeline. We
//! use the *graphical* interpretation of the low range (0x01–0x1F, 0x7F):
//! smileys, card suits, and arrows rather than C0 control characters,
//! because that is what the VGA text mode actually displayed and what art
//! packs relied on. Byte 0x00 is mapped to a space (it renders as a blank
//! cell), which means the reverse lookup for `' '` yields the canonical
//! 0x20.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Full 256-entry CP437 → Unicode table (graphical low range).
#[rustfmt::skip]
pub const CP437_TO_UNICODE: [char; 256] = [
    // 0x00–0x0F (0x00 rendered as a blank cell)
    ' ', '☺', '☻', '♥', '♦', '♣', '♠', '•', '◘', '○', '◙', '♂', '♀', '♪', '♫', '☼',
    // 0x10–0x1F
    '►', '◄', '↕', '‼', '¶', '§', '▬', '↨', '↑', '↓', '→', '←', '∟', '↔', '▲', '▼',
    // 0x20–0x2F
    ' ', '!', '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/',
    // 0x30–0x3F
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?',
    // 0x40–0x4F
    '@', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O',
    // 0x50–0x5F
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '[', '\\', ']', '^', '_',
    // 0x60–0x6F
    '`', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o',
    // 0x70–0x7F
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '{', '|', '}', '~', '⌂',
    // 0x80–0x8F
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
    // 0x90–0x9F
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ',
    // 0xA0–0xAF
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»',
    // 0xB0–0xBF
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
    // 0xC0–0xCF
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
    // 0xD0–0xDF
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
    // 0xE0–0xEF
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
    // 0xF0–0xFF
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// Convert one CP437 byte to its Unicode character.
pub fn cp437_to_unicode(byte: u8) -> char {
    CP437_TO_UNICODE[byte as usize]
}

fn reverse_table() -> &'static HashMap<char, u8> {
    static REVERSE: OnceLock<HashMap<char, u8>> = OnceLock::new();
    REVERSE.get_or_init(|| {
        let mut map = HashMap::with_capacity(256);
        // Later entries win, so the duplicate blank (0x00 vs 0x20) resolves
        // to the canonical ASCII space.
        for (byte, ch) in CP437_TO_UNICODE.iter().enumerate() {
            map.insert(*ch, byte as u8);
        }
        map
    })
}

/// Convert a Unicode character to its CP437 byte, if one exists.
pub fn unicode_to_cp437(ch: char) -> Option<u8> {
    reverse_table().get(&ch).copied()
}

/// Convert a Unicode character to CP437, substituting `b'?'` when the
/// character has no CP437 equivalent.
pub fn unicode_to_cp437_lossy(ch: char) -> u8 {
    unicode_to_cp437(ch).unwrap_or(b'?')
}

/// Decode a CP437 byte slice into a `String`.
pub fn cp437_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| cp437_to_unicode(b)).collect()
}

/// Encode a string as CP437 bytes, substituting `b'?'` for characters
/// outside the code page.
pub fn string_to_cp437_lossy(s: &str) -> Vec<u8> {
    s.chars().map(unicode_to_cp437_lossy).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_256_entries_and_no_duplicates_beyond_blank() {
        let mut seen = HashMap::new();
        for (i, ch) in CP437_TO_UNICODE.iter().enumerate() {
            if let Some(prev) = seen.insert(*ch, i) {
                // The only allowed collision is the blank cell (0x00 vs 0x20).
                assert_eq!((prev, i), (0x00, 0x20), "unexpected duplicate {ch:?}");
            }
        }
    }

    #[test]
    fn ascii_range_is_identity() {
        for b in 0x20..=0x7Eu8 {
            assert_eq!(cp437_to_unicode(b), b as char);
            assert_eq!(unicode_to_cp437(b as char), Some(b));
        }
    }

    #[test]
    fn roundtrip_every_byte() {
        for b in 0u8..=255 {
            let ch = cp437_to_unicode(b);
            let back = unicode_to_cp437(ch).expect("every table char maps back");
            if b == 0x00 {
                // 0x00 renders as a blank, whose canonical byte is 0x20.
                assert_eq!(back, 0x20);
            } else {
                assert_eq!(back, b, "byte {b:#04x} did not roundtrip");
            }
        }
    }

    #[test]
    fn spot_check_classic_glyphs() {
        assert_eq!(cp437_to_unicode(0x01), '☺');
        assert_eq!(cp437_to_unicode(0x03), '♥');
        assert_eq!(cp437_to_unicode(0xB0), '░');
        assert_eq!(cp437_to_unicode(0xB1), '▒');
        assert_eq!(cp437_to_unicode(0xB2), '▓');
        assert_eq!(cp437_to_unicode(0xDB), '█');
        assert_eq!(cp437_to_unicode(0xDC), '▄');
        assert_eq!(cp437_to_unicode(0xDF), '▀');
        assert_eq!(cp437_to_unicode(0xCD), '═');
        assert_eq!(cp437_to_unicode(0xFF), '\u{a0}');
        assert_eq!(unicode_to_cp437('█'), Some(0xDB));
        assert_eq!(unicode_to_cp437('☺'), Some(0x01));
    }

    #[test]
    fn unmappable_unicode_becomes_question_mark() {
        assert_eq!(unicode_to_cp437('中'), None);
        assert_eq!(unicode_to_cp437_lossy('中'), b'?');
        assert_eq!(unicode_to_cp437_lossy('😀'), b'?');
        assert_eq!(unicode_to_cp437_lossy('€'), b'?');
    }

    #[test]
    fn string_helpers_roundtrip() {
        let bytes: Vec<u8> = vec![0xC9, 0xCD, 0xBB, b'h', b'i', 0xC8, 0xCD, 0xBC];
        let s = cp437_to_string(&bytes);
        assert_eq!(s, "╔═╗hi╚═╝");
        assert_eq!(string_to_cp437_lossy(&s), bytes);
    }

    #[test]
    fn string_encode_is_lossy_for_foreign_text() {
        assert_eq!(string_to_cp437_lossy("aé中"), vec![b'a', 0x82, b'?']);
    }
}
