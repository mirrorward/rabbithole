//! Wire encodings for the telnet surface: UTF-8 passthrough and CP437.
//!
//! Modern clients (and anything speaking UTF-8) get bytes verbatim; retro
//! clients get single-byte CP437. For now the CP437 mapping is deliberately
//! lossy — ASCII passes through, everything else becomes `?` — with
//! [`unicode_to_cp437`] / [`cp437_to_unicode`] as the seam the art crate's
//! real translation tables (box drawing, shades, the works) replace in the
//! Wave 6 art slice.

/// Output/input byte encoding for a telnet session.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 passthrough (default).
    #[default]
    Utf8,
    /// CP437 single-byte mode; currently lossy outside ASCII.
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

/// Unicode → CP437, lossy: ASCII maps 1:1, everything else is `?`.
/// Seam for the art crate's full CP437 table (later Wave 6 slice).
pub fn unicode_to_cp437(c: char) -> u8 {
    if c.is_ascii() {
        c as u8
    } else {
        b'?'
    }
}

/// CP437 → Unicode, lossy: ASCII maps 1:1, everything else is `?`.
/// Seam for the art crate's full CP437 table (later Wave 6 slice).
pub fn cp437_to_unicode(b: u8) -> char {
    if b.is_ascii() {
        b as char
    } else {
        '?'
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
    fn cp437_is_lossy_outside_ascii_for_now() {
        let mut out = Vec::new();
        encode_into(Encoding::Cp437, "café ♥!", &mut out);
        assert_eq!(out, b"caf? ?!");
        assert_eq!(decode(Encoding::Cp437, &[b'A', 0xFF, b'B']), "A?B");
    }
}
