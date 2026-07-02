//! Microsoft Binary Format (MBF) 4-byte single-precision floats.
//!
//! Classic QWK `.NDX` index files store their message pointer as a **4-byte MS
//! Binary Format float** (the `MKS$` encoding from GW-BASIC / QuickBASIC), *not*
//! IEEE-754. This module implements the conversion in both directions so the
//! index can be written correctly. Per the format research we *implement* the
//! encoder but never trust the index on read (many historical doors wrote buggy
//! `.NDX` files — a reader rescans `MESSAGES.DAT` instead).
//!
//! ## The 4-byte MBF layout
//!
//! ```text
//! on-disk bytes (little-endian):  [ m_low ][ m_mid ][ s | m_high ][ exp ]
//!                                    0        1         2            3
//!
//!   exp    : 8-bit exponent, excess-128 biased. exp == 0 means the value 0.
//!   s      : sign bit (bit 7 of byte 2).
//!   m_high : top 7 bits of the 23-bit mantissa (bits 6..0 of byte 2).
//!   m_mid  : mantissa bits 15..8.
//!   m_low  : mantissa bits 7..0.
//! ```
//!
//! The mantissa carries an implicit leading 1, and the value is
//! `(-1)^s * 1.mantissa * 2^(exp - 129)`. Concretely this makes the mantissa
//! bits identical to IEEE-754 single precision while the exponent is offset by
//! exactly **+2** (IEEE bias 127 vs. this scheme's effective 129). That +2 is
//! the whole trick.
//!
//! Reference values (as stored on disk):
//!
//! | value | bytes            |
//! |-------|------------------|
//! | 0.0   | `00 00 00 00`    |
//! | 0.5   | `00 00 00 80`    |
//! | 1.0   | `00 00 00 81`    |
//! | 2.0   | `00 00 00 82`    |
//! | 10.0  | `00 00 20 84`    |
//! | -1.0  | `00 00 80 81`    |

/// Encode an IEEE-754 `f32` as a 4-byte MBF single (little-endian on-disk order).
///
/// Zero, subnormals, and non-finite inputs encode to all-zero bytes (MBF has no
/// infinity/NaN encoding). Magnitudes that would overflow the 8-bit exponent
/// saturate to the largest representable MBF value of the matching sign.
pub fn encode_f32(x: f32) -> [u8; 4] {
    if x == 0.0 || !x.is_finite() {
        return [0, 0, 0, 0];
    }
    let bits = x.to_bits();
    let sign = ((bits >> 31) & 1) as u8;
    let exp = (bits >> 23) & 0xFF;
    let mant = bits & 0x007F_FFFF;

    if exp == 0 {
        // IEEE subnormal: too small for normalized MBF, underflow to zero.
        return [0, 0, 0, 0];
    }

    let mbf_exp = exp + 2;
    if mbf_exp > 0xFF {
        // Overflow: saturate to the largest MBF magnitude of this sign.
        let m_high = 0x7F | (sign << 7);
        return [0xFF, 0xFF, m_high, 0xFF];
    }

    let m_low = (mant & 0xFF) as u8;
    let m_mid = ((mant >> 8) & 0xFF) as u8;
    let m_high = (((mant >> 16) & 0x7F) as u8) | (sign << 7);
    [m_low, m_mid, m_high, mbf_exp as u8]
}

/// Decode a 4-byte MBF single (little-endian on-disk order) to `f32`.
///
/// A zero exponent decodes to `0.0`. Exponents of 1 or 2 (which correspond to
/// IEEE subnormals) underflow to `0.0`; such tiny values never occur in the
/// integer message pointers this codec cares about.
pub fn decode_f32(bytes: [u8; 4]) -> f32 {
    let e = bytes[3];
    if e < 3 {
        return 0.0;
    }
    let sign = (bytes[2] >> 7) as u32;
    let mant = (((bytes[2] & 0x7F) as u32) << 16) | ((bytes[1] as u32) << 8) | (bytes[0] as u32);
    let ieee_exp = (e as u32) - 2; // in 1..=253, always a valid normal exponent
    let out_bits = (sign << 31) | (ieee_exp << 23) | mant;
    f32::from_bits(out_bits)
}

/// Encode a `u32` (e.g. a QWK message number or 1-based block pointer) as MBF.
///
/// Values up to 2^24 are represented exactly; larger values round to the nearest
/// `f32` first, matching what a period-correct BASIC door would have written.
pub fn encode(value: u32) -> [u8; 4] {
    encode_f32(value as f32)
}

/// Decode a 4-byte MBF single back to the nearest `u32`.
///
/// Negative or non-finite results clamp to `0`; values beyond [`u32::MAX`]
/// saturate. This is the safe read path the `.NDX` decoder uses.
pub fn decode(bytes: [u8; 4]) -> u32 {
    let v = decode_f32(bytes);
    if v.is_sign_negative() || v.is_nan() {
        0
    } else {
        // `f32 as u32` saturates at u32::MAX since Rust 1.45.
        v.round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_float_encodings() {
        assert_eq!(encode_f32(0.0), [0x00, 0x00, 0x00, 0x00]);
        assert_eq!(encode_f32(0.5), [0x00, 0x00, 0x00, 0x80]);
        assert_eq!(encode_f32(1.0), [0x00, 0x00, 0x00, 0x81]);
        assert_eq!(encode_f32(2.0), [0x00, 0x00, 0x00, 0x82]);
        assert_eq!(encode_f32(10.0), [0x00, 0x00, 0x20, 0x84]);
        assert_eq!(encode_f32(-1.0), [0x00, 0x00, 0x80, 0x81]);
    }

    #[test]
    fn known_float_decodings() {
        assert_eq!(decode_f32([0x00, 0x00, 0x00, 0x00]), 0.0);
        assert_eq!(decode_f32([0x00, 0x00, 0x00, 0x80]), 0.5);
        assert_eq!(decode_f32([0x00, 0x00, 0x00, 0x81]), 1.0);
        assert_eq!(decode_f32([0x00, 0x00, 0x00, 0x82]), 2.0);
        assert_eq!(decode_f32([0x00, 0x00, 0x20, 0x84]), 10.0);
        assert_eq!(decode_f32([0x00, 0x00, 0x80, 0x81]), -1.0);
    }

    #[test]
    fn non_finite_and_zero_encode_to_zero() {
        assert_eq!(encode_f32(f32::INFINITY), [0, 0, 0, 0]);
        assert_eq!(encode_f32(f32::NEG_INFINITY), [0, 0, 0, 0]);
        assert_eq!(encode_f32(f32::NAN), [0, 0, 0, 0]);
        assert_eq!(encode(0), [0, 0, 0, 0]);
    }

    #[test]
    fn u32_round_trip_exact_range() {
        for &n in &[
            0u32, 1, 2, 3, 100, 255, 256, 1000, 65535, 1_000_000, 16_777_215, 16_777_216,
        ] {
            assert_eq!(decode(encode(n)), n, "round trip failed for {n}");
        }
    }

    #[test]
    fn float_round_trip() {
        for &x in &[1.0f32, 2.0, 0.5, 3.5, 100.0, 12345.0, 0.125] {
            let back = decode_f32(encode_f32(x));
            assert_eq!(back, x, "float round trip failed for {x}");
        }
    }

    #[test]
    fn decode_never_panics_on_arbitrary_bytes() {
        // Exhaustively exercise the exponent byte with assorted mantissas.
        for e in 0u16..=255 {
            for &m in &[0u8, 0x7F, 0x80, 0xFF] {
                let _ = decode([m, m, m, e as u8]);
                let _ = decode_f32([m, m, m, e as u8]);
            }
        }
    }
}
