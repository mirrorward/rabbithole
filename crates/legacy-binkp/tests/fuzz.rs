//! Fuzz-ish robustness tests: every decoder and the session FSM must survive
//! arbitrary and truncated bytes without panicking. Uses a deterministic
//! PCG-style generator so failures reproduce (std only, no `rand`).

use rabbithole_legacy_binkp::session::Event;
use rabbithole_legacy_binkp::{
    decode_block, from_hex, parse_address_list, parse_challenge, Command, RawBlock, Session,
    SessionConfig,
};

/// Deterministic 64-bit LCG; top bytes are decently mixed.
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

fn feed_all_decoders(bytes: &[u8]) {
    // None of these may panic; all Results are intentionally discarded.
    let _ = decode_block(bytes);
    if let Ok((RawBlock::Command { id, args }, _)) = decode_block(bytes) {
        let _ = Command::parse(id, &args);
    }
    // Command::parse over raw id/arg splits too.
    if !bytes.is_empty() {
        let _ = Command::parse(bytes[0], &bytes[1..]);
    }
    let _ = from_hex(&String::from_utf8_lossy(bytes));
    let _ = parse_challenge(&String::from_utf8_lossy(bytes));
    let _ = parse_address_list(&String::from_utf8_lossy(bytes));
}

#[test]
fn random_bytes_never_panic_any_decoder() {
    let mut rng = Lcg(0x5EED_CAFE_1234_5678);
    for _ in 0..1000 {
        let len = usize::from(rng.next_u8()) * 3 % 700;
        let buf = rng.buf(len);
        feed_all_decoders(&buf);
    }
}

#[test]
fn random_bytes_with_command_preambles_never_panic() {
    let mut rng = Lcg(0xBADD_ECAF_0000_0001);
    // Command headers for every id, plus a data header.
    for round in 0..500 {
        let id = (round % 12) as u8;
        let len = usize::from(rng.next_u8()) % 64;
        let header = 0x8000u16 | ((len as u16) + 1);
        let mut buf = header.to_be_bytes().to_vec();
        buf.push(id);
        buf.extend(rng.buf(len));
        feed_all_decoders(&buf);
        // Truncate at every cut point.
        for cut in 0..=buf.len() {
            feed_all_decoders(&buf[..cut]);
        }
    }
}

#[test]
fn every_truncation_of_valid_frames_never_panics() {
    let mut frames: Vec<Vec<u8>> = Vec::new();
    for cmd in [
        Command::Nul("OPT CRAM-MD5-deadbeef".into()),
        Command::Adr(parse_address_list("2:5020/1042.0@fidonet").unwrap()),
        Command::Pwd("CRAM-MD5-abcdef".into()),
        Command::File(rabbithole_legacy_binkp::FileInfo::new("f", 9000, 1)),
        Command::Ok("secure".into()),
        Command::Eob,
        Command::Got(rabbithole_legacy_binkp::FileId::new("f", 9000, 1)),
        Command::Err("boom".into()),
    ] {
        frames.push(cmd.to_block().encode().unwrap());
    }
    frames.push(RawBlock::Data((0..=255u8).collect()).encode().unwrap());

    for frame in frames {
        for cut in 0..=frame.len() {
            feed_all_decoders(&frame[..cut]);
        }
    }
}

#[test]
fn session_survives_random_event_storms() {
    let mut rng = Lcg(0xFEED_FACE_0BAD_F00D);
    for seed in 0..200 {
        let mut s = if seed % 2 == 0 {
            Session::originating(SessionConfig::default())
        } else {
            Session::answering(SessionConfig::default())
        };
        let _ = s.start();
        for _ in 0..40 {
            let event = if rng.next_u8() % 3 == 0 {
                let n = usize::from(rng.next_u8()) % 32;
                Event::Data(rng.buf(n))
            } else {
                let id = rng.next_u8() % 12;
                let n = usize::from(rng.next_u8()) % 24;
                let args = rng.buf(n);
                match Command::parse(id, &args) {
                    Ok(cmd) => Event::Command(cmd),
                    Err(_) => continue,
                }
            };
            // Errors are fine; panics are not.
            let _ = s.advance(event);
        }
    }
}
