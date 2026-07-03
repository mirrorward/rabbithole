//! Wire encodings for the telnet surface: UTF-8 passthrough and CP437.
//!
//! Modern clients (and anything speaking UTF-8) get bytes verbatim; retro
//! clients get single-byte CP437 through the art crate's full translation
//! tables ([`rabbithole_art::cp437`]) — box drawing, shades, the works —
//! filling the seam this module carried as a lossy ASCII stub before the
//! Wave 6 art slice. Characters with no CP437 equivalent still become `?`.

/// Output/input byte encoding for a telnet session.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 passthrough (default).
    #[default]
    Utf8,
    /// CP437 single-byte mode (full art-crate tables; `?` outside the page).
    Cp437,
}

/// Encode `s` into `out` under `enc`. IAC escaping is the caller's job —
/// this is pure character translation.
pub fn encode_into(enc: Encoding, s: &str, out: &mut Vec<u8>) {
    match enc {
        Encoding::Utf8 => out.extend_from_slice(s.as_bytes()),
        Encoding::Cp437 => out.extend(s.chars().map(unicode_to_cp437)),
    }
}

/// Decode a received byte buffer under `enc`, lossily.
pub fn decode(enc: Encoding, bytes: &[u8]) -> String {
    match enc {
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Cp437 => bytes.iter().map(|&b| cp437_to_unicode(b)).collect(),
    }
}

/// Unicode → CP437. ASCII (including CR/LF and other controls) passes
/// through 1:1 — this is a *stream* encoding, so the art table's graphical
/// reading of the low range must not turn `♥` into an ETX byte on the wire;
/// such glyphs (and anything else without a printable CP437 slot) become
/// `?`. The high half (`é`, box drawing, shades…) uses the art crate's
/// reverse table.
pub fn unicode_to_cp437(c: char) -> u8 {
    if c.is_ascii() {
        return c as u8;
    }
    match rabbithole_art::unicode_to_cp437(c) {
        Some(b) if b >= 0x20 => b,
        _ => b'?',
    }
}

/// CP437 → Unicode. ASCII passes through 1:1 (low bytes are controls on a
/// stream, not glyphs); the high half uses the art crate's table.
pub fn cp437_to_unicode(b: u8) -> char {
    if b.is_ascii() {
        b as char
    } else {
        rabbithole_art::cp437_to_unicode(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_is_passthrough() {
        let mut out = Vec::new();
        encode_into(Encoding::Utf8, "café ♥", &mut out);
        assert_eq!(out, "café ♥".as_bytes());
        assert_eq!(decode(Encoding::Utf8, &out), "café ♥");
    }

    #[test]
    fn cp437_uses_the_real_tables() {
        let mut out = Vec::new();
        encode_into(Encoding::Cp437, "café ═╣░", &mut out);
        assert_eq!(out, [b'c', b'a', b'f', 0x82, b' ', 0xCD, 0xB9, 0xB0]);
        assert_eq!(decode(Encoding::Cp437, &out), "café ═╣░");
        // Characters outside the page still degrade to `?`, and glyphs that
        // live in the control range of the art table never emit control
        // bytes on the wire.
        let mut lossy = Vec::new();
        encode_into(Encoding::Cp437, "汉♥", &mut lossy);
        assert_eq!(lossy, b"??");
        // ASCII controls pass through 1:1 both ways.
        let mut nl = Vec::new();
        encode_into(Encoding::Cp437, "a\r\n", &mut nl);
        assert_eq!(nl, b"a\r\n");
        assert_eq!(decode(Encoding::Cp437, b"\x08"), "\u{8}");
    }
}
