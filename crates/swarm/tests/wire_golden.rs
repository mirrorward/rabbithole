//! Golden byte-vector and totality tests for the swarm wire codecs.
//!
//! The `Manifest` and `CapToken` types are carried between peers as postcard
//! bytes, so their encoding is part of the on-the-wire contract: a field
//! reorder or a postcard version bump that changed the layout would silently
//! break interop with already-deployed peers. These tests pin the exact bytes
//! of a hand-verified message so any such drift fails loudly, and sweep the
//! decoders with hostile input to prove they are total (the crate is safe Rust
//! and every decode entry point must return `Ok`/`Err`, never panic).

#![forbid(unsafe_code)]

use rabbithole_identity::IdentityKey;
use rabbithole_swarm::{CapToken, Manifest, ManifestFile};

/// A one-file manifest whose postcard encoding is pinned below.
///
/// `Manifest::new` sorts files by path, so this is already canonical.
fn golden_manifest() -> Manifest {
    Manifest::new("m", vec![ManifestFile::new("a", 1, [0u8; 32], "")])
}

/// The exact postcard bytes `golden_manifest()` must serialize to.
///
/// postcard 1.x layout, fields in declaration order, integers as LEB128
/// varints, strings/seqs as `varint(len) ++ bytes`, `[u8; 32]` as 32 raw bytes:
///
/// ```text
///  off  bytes            field
///  ---  ---------------  -------------------------------------------------
///   0   01 6D            name:        len=1, "m"
///   2   80 80 40         chunk_size:  u32 varint = 1_048_576 (1 MiB)
///   5   01               files:       len=1
///   6   01 61              [0].path:  len=1, "a"
///   8   01                 [0].size:  u64 varint = 1
///   9   00*32              [0].root:  32-byte blake3 root (all zero here)
///  41   00                 [0].mime:  len=0, ""
/// ```
#[rustfmt::skip]
const MANIFEST_GOLDEN: &[u8] = &[
    0x01, 0x6D,             // name: "m"
    0x80, 0x80, 0x40,       // chunk_size = 1_048_576
    0x01,                   // 1 file
    0x01, 0x61,             // path "a"
    0x01,                   // size 1
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // root[0..8]
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // root[8..16]
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // root[16..24]
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // root[24..32]
    0x00,                   // mime ""
];

#[test]
fn manifest_encodes_to_golden_bytes() {
    let m = golden_manifest();
    assert_eq!(m.encode(), MANIFEST_GOLDEN, "manifest wire layout drifted");
}

#[test]
fn manifest_golden_bytes_decode_to_manifest() {
    let decoded = Manifest::decode(MANIFEST_GOLDEN).expect("golden decodes");
    assert_eq!(decoded, golden_manifest());
    // Round-trip fixed point: re-encoding the decoded value reproduces the
    // exact same bytes.
    assert_eq!(decoded.encode(), MANIFEST_GOLDEN);
}

/// The signature is Ed25519 (deterministic) and the seed is fixed, so a fully
/// specified token has one and only one wire form.
fn golden_token() -> CapToken {
    let key = IdentityKey::from_seed(&[42u8; 32]);
    CapToken::issue(&key, [1u8; 32], "alice", 1_000).expect("issue")
}

#[test]
fn cap_token_wire_form_is_stable() {
    let token = golden_token();
    let wire = token.to_bytes();
    // Structural checks that don't depend on the exact signature bytes:
    //   claim.root (32) ++ varint(len "alice") ++ "alice" ++
    //   zigzag-varint(expires_unix) ++ 64-byte Ed25519 signature.
    assert_eq!(
        &wire[..32],
        &[1u8; 32],
        "claim.root leads the postcard record"
    );
    assert_eq!(wire[32], 0x05, "fetcher length prefix = 5");
    assert_eq!(&wire[33..38], b"alice");

    // Round-trip fixed point.
    let back = CapToken::from_bytes(&wire).expect("golden decodes");
    assert_eq!(back, token);
    assert_eq!(back.to_bytes(), wire);
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
    fn buf(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u8()).collect()
    }
}

#[test]
fn decoders_never_panic_on_random_bytes() {
    let mut rng = Lcg(0x5EED_CAFE_1234_5678);
    for _ in 0..20_000 {
        let len = usize::from(rng.next_u8()) * 2 % 300;
        let buf = rng.buf(len);
        // Neither may panic regardless of input; both must return.
        let _ = Manifest::decode(&buf);
        let _ = CapToken::from_bytes(&buf);
    }
}

#[test]
fn every_truncation_and_bitflip_of_golden_never_panics() {
    let manifest = MANIFEST_GOLDEN.to_vec();
    let token = golden_token().to_bytes();
    for base in [&manifest, &token] {
        for cut in 0..=base.len() {
            let _ = Manifest::decode(&base[..cut]);
            let _ = CapToken::from_bytes(&base[..cut]);
        }
    }
    // Single-byte corruptions of each full frame.
    let mut rng = Lcg(0x0DDB_1770_5EAF_00D5);
    for base in [&manifest, &token] {
        for _ in 0..2_000 {
            let mut m = base.clone();
            if m.is_empty() {
                continue;
            }
            let idx = usize::from(rng.next_u8()) % m.len();
            m[idx] ^= rng.next_u8() | 1;
            let _ = Manifest::decode(&m);
            let _ = CapToken::from_bytes(&m);
        }
    }
}
