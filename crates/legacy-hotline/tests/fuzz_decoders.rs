//! Fuzz-ish robustness tests: every decoder must return `Err` (never panic)
//! on random, truncated, or hostile input.
//!
//! Uses a tiny deterministic PRNG so the corpus is reproducible without pulling
//! in a `rand` dependency.

#![forbid(unsafe_code)]

use rabbithole_legacy_hotline::field::{decode_params, read_int, Field};
use rabbithole_legacy_hotline::{
    Handshake, HandshakeReply, Reassembler, Transaction, TransactionHeader,
};

/// Deterministic xorshift64* PRNG.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn byte(&mut self) -> u8 {
        (self.next_u64() & 0xFF) as u8
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.byte()).collect()
    }
}

/// Run every decoder over a blob; the only requirement is that none panic.
fn hammer(blob: &[u8]) {
    let _ = Handshake::decode(blob);
    let _ = HandshakeReply::decode(blob);
    let _ = TransactionHeader::decode(blob);
    let _ = Transaction::decode(blob);
    let _ = decode_params(blob);
    let _ = read_int(blob);
    let _ = Field::decode(blob);

    // Also feed the header + remainder into the reassembler if a header parses.
    if let Ok(header) = TransactionHeader::decode(blob) {
        let mut r = Reassembler::new();
        let chunk = if blob.len() > TransactionHeader::LEN {
            &blob[TransactionHeader::LEN..]
        } else {
            &[][..]
        };
        let _ = r.push(&header, chunk);
    }
}

#[test]
fn random_blobs_never_panic() {
    let mut rng = Rng::new(0xC0FF_EE12_3456_789A);
    for _ in 0..20_000 {
        let len = (rng.next_u64() % 64) as usize;
        let blob = rng.bytes(len);
        hammer(&blob);
    }
}

#[test]
fn truncations_of_valid_frames_never_panic() {
    // Start from several well-formed encodings and truncate at every length.
    let mut samples: Vec<Vec<u8>> = Vec::new();
    samples.push(Handshake::hotl().encode().to_vec());
    samples.push(HandshakeReply::ok().encode().to_vec());
    samples.push(
        Transaction::request(
            107,
            1,
            vec![Field::text(105, "guest"), Field::int(103, 4242)],
        )
        .encode(),
    );
    samples.push(Transaction::reply(300, 9, 0, vec![Field::int(116, 3)]).encode());

    for sample in &samples {
        for cut in 0..=sample.len() {
            hammer(&sample[..cut]);
        }
    }
}

#[test]
fn corrupted_length_fields_never_panic() {
    // A header claiming a giant data_size must not allocate wildly or panic;
    // Transaction::decode should simply report truncation.
    let mut header = TransactionHeader {
        flags: 0,
        is_reply: 0,
        type_: 107,
        id: 1,
        error: 0,
        total_size: u32::MAX,
        data_size: u32::MAX,
    }
    .encode()
    .to_vec();
    header.extend_from_slice(&[0xFF; 8]);
    assert!(Transaction::decode(&header).is_err());
    hammer(&header);

    // Parameter list claiming more fields than exist.
    let bogus_params = [0xFF, 0xFF, 0x00, 0x01];
    assert!(decode_params(&bogus_params).is_err());
    hammer(&bogus_params);
}
