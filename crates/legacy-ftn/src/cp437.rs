//! Minimal inline CP437 ↔ Unicode mapping for *text accessors*.
//!
//! FidoNet message bodies and name/subject fields are CP437 (IBM PC "OEM")
//! byte streams. The codec keeps those bytes **raw and lossless** all the way
//! through packing and unpacking — nothing in [`crate::packet`] or
//! [`crate::message`] ever forces a lossy Unicode round-trip. Decoding only
//! happens at the *edge*, when a caller asks for human-readable text via an
//! accessor such as [`crate::message::PackedMessage::body_text`].
//!
//! This module is deliberately self-contained (it does **not** depend on the
//! `art` crate) so the codec stays dependency-light. The table uses the
//! graphical interpretation of the C0 range (0x01–0x1F, 0x7F) — smileys, card
//! suits, arrows — matching what VGA text mode actually displayed.
//!
//! Round-trip note: every byte in `0x01..=0xFF` maps to a distinct Unicode
//! scalar, so [`decode`] followed by [`encode_lossy`] reproduces the original
//! bytes exactly (the sole collision, 0x00 vs 0x20 → `' '`, cannot occur
//! inside NUL-terminated fields).

/// Full 256-entry CP437 → Unicode table (graphical low range; 0x00 → space).
#[rustfmt::skip]
pub const CP437_TO_UNICODE: [char; 256] = [
    ' ', '☺', '☻', '♥', '♦', '♣', '♠', '•', '◘', '○', '◙', '♂', '♀', '♪', '♫', '☼',
    '►', '◄', '↕', '‼', '¶', '§', '▬', '↨', '↑', '↓', '→', '←', '∟', '↔', '▲', '▼',
    ' ', '!', '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/',
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?',
    '@', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O',
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '[', '\\', ']', '^', '_',
    '`', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o',
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '{', '|', '}', '~', '⌂',
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ',
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»',
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// Decode a CP437 byte slice into a `String`.
pub fn decode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| CP437_TO_UNICODE[b as usize])
        .collect()
}

/// Encode a `&str` as CP437 bytes, substituting `b'?'` for any character with
/// no CP437 representation.
pub fn encode_lossy(s: &str) -> Vec<u8> {
    s.chars()
        .map(|ch| {
            CP437_TO_UNICODE
                .iter()
                .position(|&c| c == ch)
                .map(|i| i as u8)
                // Prefer the canonical ASCII space (0x20) over the 0x00 blank.
                .map(|b| if ch == ' ' { b' ' } else { b })
                .unwrap_or(b'?')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_identity() {
        for b in 0x20u8..=0x7e {
            assert_eq!(decode(&[b]), (b as char).to_string());
        }
    }

    #[test]
    fn high_bytes_roundtrip_losslessly() {
        let bytes: Vec<u8> = (0x01u8..=0xff).collect();
        let s = decode(&bytes);
        assert_eq!(encode_lossy(&s), bytes);
    }

    #[test]
    fn box_drawing() {
        assert_eq!(decode(&[0xc9, 0xcd, 0xbb]), "╔═╗");
    }
}
