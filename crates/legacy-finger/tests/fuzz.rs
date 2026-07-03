//! Deterministic totality sweep for the finger query parser and the response
//! renderer, plus a couple of pinned query classifications.
//!
//! The finger server hands attacker-controlled bytes straight to
//! [`parse_query`] and passes user profile text through [`to_wire`]/[`sanitize`]
//! before it hits the socket. The renderer must be total (never panic) and must
//! enforce two safety invariants on *any* input: control/escape characters are
//! stripped, and line endings are always CRLF. This sweep drives a seeded
//! generator through both (no `rand`, no clocks).
//!
//! `parse_query` is total over arbitrary UTF-8 (a leading multi-byte char no
//! longer panics — the `/W`-detection now compares bytes, not `rest[..2]`); see
//! `parse_query_handles_leading_multibyte_char` below. The parser sweep drives
//! both ASCII (finger's real wire alphabet) and `from_utf8_lossy`-decoded
//! arbitrary bytes, exactly as the server does.

#![forbid(unsafe_code)]

use rabbithole_legacy_finger::{parse_query, sanitize, to_wire, Query};

#[test]
fn query_classification_is_pinned() {
    assert_eq!(parse_query("\r\n"), Query::Who);
    assert_eq!(parse_query(""), Query::Who);
    assert_eq!(parse_query("alice"), Query::User("alice".into()));
    assert_eq!(parse_query("/W alice"), Query::User("alice".into()));
    assert_eq!(parse_query("alice@example.com"), Query::Forward);
}

/// `sanitize` must drop ESC and other control bytes (keeping only TAB/CR/LF and
/// printable-and-above), and `to_wire` must never emit a bare CR or LF.
fn assert_render_invariants(input: &str) {
    let clean = sanitize(input);
    assert!(!clean.contains('\x1b'), "ESC survived sanitize: {clean:?}");
    for c in clean.chars() {
        let ok = c == '\t' || c == '\r' || c == '\n' || (c >= '\x20' && c != '\x7f');
        assert!(ok, "control char survived sanitize: {c:?}");
    }

    let wire = to_wire(input);
    assert!(!wire.contains('\x1b'), "ESC survived to_wire: {wire:?}");
    // Every line ending is CRLF: stripping CRLF pairs leaves no stray CR/LF.
    assert!(
        !wire.replace("\r\n", "").contains(['\r', '\n']),
        "non-CRLF line ending in wire output: {wire:?}"
    );
}

/// Deterministic 64-bit LCG (std only, reproducible without `rand`).
struct Lcg(u64);

impl Lcg {
    fn next_u8(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u8
    }
    /// ASCII-only string (bytes < 128), biased toward finger's structural chars.
    fn ascii(&mut self, len: usize) -> String {
        const ALPHABET: &[u8] = b"ab @/.\r\n\t\x00\x08WwZ0129";
        (0..len)
            .map(|_| {
                let b = self.next_u8();
                if b & 1 == 0 {
                    ALPHABET[usize::from(b >> 1) % ALPHABET.len()] as char
                } else {
                    // Any printable/control ASCII, never >= 0x80.
                    char::from(b & 0x7f)
                }
            })
            .collect()
    }
}

#[test]
fn parser_is_total_over_ascii_and_arbitrary_bytes() {
    // parse_query must be total on ASCII (finger's real wire alphabet) …
    let mut rng = Lcg(0xF14E_6E12_3456_789A);
    for _ in 0..20_000 {
        let len = usize::from(rng.next_u8()) % 128;
        let s = rng.ascii(len);
        let _ = parse_query(&s);
    }
    // … and on the lossy-decoded arbitrary bytes the server actually hands it
    // (a leading multi-byte U+FFFD used to slice-panic — see the regression
    // test below).
    let mut rng = Lcg(0x51CE_0FF1_CE00_1234);
    for _ in 0..20_000 {
        let len = usize::from(rng.next_u8()) % 128;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next_u8()).collect();
        let s = String::from_utf8_lossy(&bytes);
        let _ = parse_query(&s);
    }
}

#[test]
fn renderer_never_panics_and_keeps_invariants_over_unicode() {
    // The renderer must be total and invariant-preserving even on arbitrary
    // UTF-8, including multi-byte chars and replacement characters.
    let mut rng = Lcg(0x0DDB_1770_5EAF_00D5);
    for _ in 0..20_000 {
        let len = usize::from(rng.next_u8()) % 96;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next_u8()).collect();
        let s = String::from_utf8_lossy(&bytes);
        assert_render_invariants(&s);
    }
}

/// Regression test for a formerly remotely-triggerable panic in
/// `parse_query`.
///
/// The server decodes the query line with `String::from_utf8_lossy`, so a line
/// beginning with an invalid byte such as `0xFF` becomes a leading U+FFFD
/// replacement char (3 UTF-8 bytes). The old `/W`-detection sliced `rest[..2]`,
/// which split that char and panicked ("byte index 2 is not a char boundary").
/// The parser now compares the first two *bytes*, so a leading multi-byte char
/// parses as an ordinary (refused-on-`@`-or-served) username with no panic.
#[test]
fn parse_query_handles_leading_multibyte_char() {
    // Exactly the string the server hands to parse_query for a `0xFF x` line.
    let line = String::from_utf8_lossy(&[0xFF, b'x']).into_owned();
    assert_eq!(parse_query(&line), Query::User(line.clone()));

    // A bare replacement char, and one glued to a name, are also total.
    let bare = String::from_utf8_lossy(&[0xFF]).into_owned();
    assert_eq!(parse_query(&bare), Query::User(bare.clone()));
    // A leading multi-byte char followed by '@' is still a forward refusal.
    let fwd = String::from_utf8_lossy(&[0xFF, b'@', b'h']).into_owned();
    assert_eq!(parse_query(&fwd), Query::Forward);
    // The real `/W` verbose flag still works and is stripped.
    assert_eq!(parse_query("/W alice"), Query::User("alice".into()));
    assert_eq!(parse_query("/Wbob"), Query::User("bob".into()));
}
