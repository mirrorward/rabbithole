//! Internal text helpers: lossless Latin-1 <-> `String` and fixed-width fields.
//!
//! QWK stores 8-bit text (historically CP437). This crate keeps the codec edge
//! simple and *lossless* by round-tripping raw bytes through ISO-8859-1
//! (Latin-1): byte `b` maps to the code point `U+00b`, and back. A caller that
//! wants CP437-correct glyphs applies a CP437 table on top; the byte values are
//! preserved regardless, which is what byte-exact round-tripping requires.

/// Decode bytes as ISO-8859-1: each byte becomes the code point of equal value.
/// Total and lossless for all 256 byte values.
pub(crate) fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Encode a string back to Latin-1 bytes. Code points above `U+00FF` (which the
/// codec never produces itself) are replaced with `b'?'`.
pub(crate) fn encode_latin1(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| if (c as u32) <= 0xFF { c as u8 } else { b'?' })
        .collect()
}

/// Write `value` into a fixed-width field, Latin-1 encoded, truncated to the
/// field width and right-padded with spaces (`0x20`). `dst.len()` is the width.
pub(crate) fn write_field(dst: &mut [u8], value: &str) {
    let bytes = encode_latin1(value);
    let n = bytes.len().min(dst.len());
    dst[..n].copy_from_slice(&bytes[..n]);
    dst[n..].fill(b' ');
}

/// Read a fixed-width field, trimming trailing spaces and NULs, then Latin-1
/// decoding the significant prefix.
pub(crate) fn read_field(src: &[u8]) -> String {
    let end = src
        .iter()
        .rposition(|&b| b != b' ' && b != 0)
        .map_or(0, |i| i + 1);
    decode_latin1(&src[..end])
}
