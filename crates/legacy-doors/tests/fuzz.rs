//! Deterministic totality sweep for the door drop-file readers.
//!
//! `read_door_sys` / `read_door32_sys` parse attacker-influenced text files
//! (a BBS drops them for third-party door programs) and `detect` sniffs raw
//! bytes. All three must be total — any input, including truncated files,
//! non-numeric fields, embedded NULs and random bytes, must yield `Ok`/`Err`
//! or `None`, never a panic. Golden output strings live in `tests/dropfiles.rs`;
//! this file drives a seeded generator (no `rand`, no clocks) plus mutations of
//! valid files.

#![forbid(unsafe_code)]

use rabbithole_legacy_doors::{
    detect, read_door32_sys, read_door_sys, write_door32_sys, write_door_sys, DoorContext,
};

fn feed(text: &str) {
    // None of these may panic; results are intentionally discarded.
    let _ = read_door_sys(text);
    let _ = read_door32_sys(text);
    let _ = detect(text.as_bytes());
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
    fn string(&mut self, len: usize) -> String {
        // Line-structured bytes plus digits and junk so field splitting, number
        // parsing and CRLF handling are all exercised.
        const ALPHABET: &[u8] = b"0129 \r\n:COM.-ANY\0\x08";
        (0..len)
            .map(|_| {
                let b = self.next_u8();
                if b & 1 == 0 {
                    ALPHABET[usize::from(b >> 1) % ALPHABET.len()] as char
                } else {
                    char::from(b)
                }
            })
            .collect()
    }
}

#[test]
fn readers_never_panic_on_random_text() {
    let mut rng = Lcg(0xD006_5EED_1234_5678);
    for _ in 0..20_000 {
        let len = usize::from(rng.next_u8()) % 160;
        let s = rng.string(len);
        feed(&s);
    }
}

#[test]
fn readers_never_panic_on_random_utf8_lossy_blobs() {
    let mut rng = Lcg(0xBADD_ECAF_0000_0001);
    for _ in 0..10_000 {
        let len = usize::from(rng.next_u8()) % 96;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next_u8()).collect();
        feed(&String::from_utf8_lossy(&bytes));
    }
}

#[test]
fn mutations_of_valid_dropfiles_never_panic() {
    let ctx = DoorContext::default();
    let seeds = [write_door_sys(&ctx), write_door32_sys(&ctx)];
    let mut rng = Lcg(0xFEED_FACE_0BAD_F00D);
    for seed in &seeds {
        // Every truncation (on char boundaries).
        for cut in 0..=seed.len() {
            if seed.is_char_boundary(cut) {
                feed(&seed[..cut]);
            }
        }
        // Random single-character substitutions.
        for _ in 0..3_000 {
            let mut chars: Vec<char> = seed.chars().collect();
            if chars.is_empty() {
                continue;
            }
            let idx = usize::from(rng.next_u8()) % chars.len();
            chars[idx] = char::from(rng.next_u8());
            let mutated: String = chars.into_iter().collect();
            feed(&mutated);
        }
    }
}
