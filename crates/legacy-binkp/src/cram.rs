//! CRAM-MD5 authentication (FTS-1027 §"Password encryption").
//!
//! Instead of sending the session password in the clear, the *answering* side
//! advertises a random challenge in one of its `M_NUL` frames:
//!
//! ```text
//!   M_NUL "OPT CRAM-MD5-<challenge-hex>"
//! ```
//!
//! The *originating* side answers with an `M_PWD` whose value is the keyed
//! digest of that challenge:
//!
//! ```text
//!   digest  = HMAC-MD5( key = password, message = challenge-bytes )
//!   M_PWD "CRAM-MD5-<digest-hex>"
//! ```
//!
//! Both the challenge and the digest travel as lowercase hex. This module
//! parses the challenge out of a NUL option line, computes the HMAC-MD5
//! digest, and formats/parses the `CRAM-MD5-<hex>` wrapper.

use hmac::{Hmac, Mac};
use md5::Md5;

/// The prefix binkp uses for the CRAM-MD5 option and password value.
pub const CRAM_MD5_PREFIX: &str = "CRAM-MD5-";

type HmacMd5 = Hmac<Md5>;

/// Encode bytes as lowercase hex.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decode a hex string (upper or lower case). Returns `None` on odd length or
/// any non-hex digit — never panics.
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    let nibble = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Some(out)
}

/// Scan an `M_NUL` info line for a `CRAM-MD5-<hex>` option and return the
/// decoded challenge bytes, if present and well-formed.
///
/// The line may be a bare option token, `OPT CRAM-MD5-…`, or a longer option
/// list; any whitespace-separated token carrying the prefix is accepted.
pub fn parse_challenge(nul_line: &str) -> Option<Vec<u8>> {
    for token in nul_line.split_whitespace() {
        if let Some(hex) = token.strip_prefix(CRAM_MD5_PREFIX) {
            return from_hex(hex);
        }
    }
    None
}

/// Compute the raw 16-byte HMAC-MD5 digest for `password` over `challenge`.
pub fn cram_md5_digest(password: &[u8], challenge: &[u8]) -> [u8; 16] {
    // HMAC accepts a key of any length, so this never errors.
    let mut mac = HmacMd5::new_from_slice(password).expect("HMAC-MD5 accepts keys of any length");
    mac.update(challenge);
    let out = mac.finalize().into_bytes();
    let mut digest = [0u8; 16];
    digest.copy_from_slice(&out);
    digest
}

/// Build the `M_PWD` value for a CRAM-MD5 response: `CRAM-MD5-<digest-hex>`.
pub fn cram_md5_response(password: &[u8], challenge: &[u8]) -> String {
    let digest = cram_md5_digest(password, challenge);
    format!("{CRAM_MD5_PREFIX}{}", to_hex(&digest))
}

/// Format a challenge as the option token an answering side advertises:
/// `CRAM-MD5-<challenge-hex>`.
pub fn cram_md5_option(challenge: &[u8]) -> String {
    format!("{CRAM_MD5_PREFIX}{}", to_hex(challenge))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = [0x00, 0x0f, 0xa5, 0xff, 0x10];
        assert_eq!(to_hex(&bytes), "000fa5ff10");
        assert_eq!(from_hex("000fa5ff10").unwrap(), bytes);
        assert_eq!(from_hex("000FA5FF10").unwrap(), bytes);
    }

    #[test]
    fn from_hex_rejects_bad_input() {
        assert_eq!(from_hex("abc"), None); // odd length
        assert_eq!(from_hex("zz"), None); // non-hex
        assert_eq!(from_hex(""), Some(vec![]));
    }

    /// RFC 2104 / RFC 2202 HMAC-MD5 test case 1:
    /// key = 16×0x0b, data = "Hi There" → 9294727a3638bb1c13f48ef8158bfc9d.
    #[test]
    fn hmac_md5_known_answer_rfc2202() {
        let key = [0x0bu8; 16];
        let digest = cram_md5_digest(&key, b"Hi There");
        assert_eq!(to_hex(&digest), "9294727a3638bb1c13f48ef8158bfc9d");
    }

    /// RFC 2202 HMAC-MD5 test case 2: key = "Jefe",
    /// data = "what do ya want for nothing?".
    #[test]
    fn hmac_md5_known_answer_jefe() {
        let digest = cram_md5_digest(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(to_hex(&digest), "750c783e6ab0b503eaa86e310a5db738");
    }

    #[test]
    fn parses_challenge_from_opt_line() {
        let line = "OPT CRAM-MD5-0123456789abcdef GZ";
        assert_eq!(
            parse_challenge(line),
            Some(vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef])
        );
        assert_eq!(parse_challenge("OPT GZ ND"), None);
        assert_eq!(parse_challenge("CRAM-MD5-zz"), None); // bad hex
    }

    #[test]
    fn response_uses_challenge_as_message_and_password_as_key() {
        // End-to-end: option out, response in, verify by recomputation.
        let challenge = vec![0xde, 0xad, 0xbe, 0xef];
        let option = cram_md5_option(&challenge);
        assert_eq!(option, "CRAM-MD5-deadbeef");
        let parsed = parse_challenge(&format!("OPT {option}")).unwrap();
        assert_eq!(parsed, challenge);

        let response = cram_md5_response(b"secret", &parsed);
        let expected = cram_md5_response(b"secret", &challenge);
        assert_eq!(response, expected);
        assert!(response.starts_with(CRAM_MD5_PREFIX));
    }
}
