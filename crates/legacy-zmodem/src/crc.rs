//! The two checksums ZMODEM uses on the wire.
//!
//! ```text
//! CRC-16/XMODEM   poly 0x1021, init 0x0000, no reflection, no xor-out.
//!                 Covers headers (type + 4 position/flag bytes) and 16-bit
//!                 data subpackets (payload + frame-end byte). Transmitted
//!                 big-endian: [ crc >> 8 ][ crc & 0xFF ].
//!
//! CRC-32          poly 0xEDB88320 (reflected), init 0xFFFFFFFF,
//!                 xor-out 0xFFFFFFFF — the standard "zip/zlib" CRC-32.
//!                 Covers ZBIN32 headers and 32-bit data subpackets.
//!                 Transmitted little-endian: [ b0 ][ b1 ][ b2 ][ b3 ].
//! ```
//!
//! The classic `lrzsz` sources compute the 16-bit CRC with an augmented
//! (two-trailing-zero-bytes) shift register; the table-free loops here
//! produce identical values without the augmentation dance.

/// CRC-16/XMODEM over `data` (poly `0x1021`, init `0`, unreflected).
///
/// Check value: `crc16_xmodem(b"123456789") == 0x31C3`.
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Standard reflected CRC-32 over `data` (poly `0xEDB88320`, init and
/// xor-out `0xFFFF_FFFF`).
///
/// Check value: `crc32(b"123456789") == 0xCBF4_3926`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_known_vectors() {
        // Canonical CRC-16/XMODEM check value.
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
        assert_eq!(crc16_xmodem(b""), 0x0000);
        assert_eq!(crc16_xmodem(b"A"), 0x58E5);
        // Five zero bytes (a ZRQINIT header body) stay at zero.
        assert_eq!(crc16_xmodem(&[0, 0, 0, 0, 0]), 0x0000);
    }

    #[test]
    fn crc32_known_vectors() {
        // Canonical CRC-32 check value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
        assert_eq!(crc32(&[0]), 0xD202_EF8D);
    }

    #[test]
    fn crc16_header_example() {
        // ZRINIT (type 1) with CANFC32|CANOVIO|CANFDX in ZF0.
        let body = [1u8, 0, 0, 0, 0x23];
        let crc = crc16_xmodem(&body);
        // Recompute over body + big-endian CRC bytes: residue must be zero
        // for the unreflected, zero-xor-out variant.
        let mut with_crc = body.to_vec();
        with_crc.push((crc >> 8) as u8);
        with_crc.push(crc as u8);
        assert_eq!(crc16_xmodem(&with_crc), 0);
    }
}
