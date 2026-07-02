//! Fuzz-ish robustness tests: every decoder in the crate must survive
//! arbitrary and truncated bytes without panicking. Uses a deterministic
//! PCG-style generator so failures reproduce (std only, no `rand`).

use rabbithole_legacy_zmodem::{
    decode_header, decode_one, decode_subpacket, encode_subpacket, unescape, FileInfo, FrameEnd,
    FrameType, Header, HeaderFormat, Receiver, RecvEvent,
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
    let _ = decode_header(bytes);
    let _ = decode_subpacket(bytes, false);
    let _ = decode_subpacket(bytes, true);
    let _ = decode_one(bytes);
    let _ = unescape(bytes);
    let _ = FileInfo::decode(bytes);
}

#[test]
fn random_bytes_never_panic_any_decoder() {
    let mut rng = Lcg(0x5EED_CAFE_1234_5678);
    for round in 0..500 {
        let len = usize::from(rng.next_u8()) * 3 % 700;
        let buf = rng.buf(len);
        feed_all_decoders(&buf);
        let _ = round;
    }
}

#[test]
fn random_bytes_with_valid_looking_preambles_never_panic() {
    let mut rng = Lcg(0xBADD_ECAF_0000_0001);
    let preambles: [&[u8]; 6] = [b"*\x18A", b"**\x18B", b"*\x18C", b"*", b"\x18", b"**\x18B0"];
    for round in 0..300 {
        let preamble = preambles[round % preambles.len()];
        let mut buf = preamble.to_vec();
        let len = usize::from(rng.next_u8()) % 64;
        buf.extend(rng.buf(len));
        feed_all_decoders(&buf);
    }
}

#[test]
fn every_truncation_of_valid_frames_never_panics() {
    let mut frames: Vec<Vec<u8>> = Vec::new();
    for frame_type in FrameType::ALL {
        for format in [HeaderFormat::Hex, HeaderFormat::Bin16, HeaderFormat::Bin32] {
            frames.push(Header::with_pos(frame_type, 0x1811_FF7F).encode(format));
        }
    }
    let payload: Vec<u8> = (0..=255u8).collect();
    for end in [
        FrameEnd::Zcrce,
        FrameEnd::Zcrcg,
        FrameEnd::Zcrcq,
        FrameEnd::Zcrcw,
    ] {
        for wide in [false, true] {
            frames.push(encode_subpacket(&payload, end, wide).unwrap());
        }
    }
    let mut info = FileInfo::new("fuzz.bin");
    info.length = Some(9000);
    frames.push(info.encode().unwrap());

    for frame in frames {
        for cut in 0..=frame.len() {
            feed_all_decoders(&frame[..cut]);
        }
    }
}

#[test]
fn corrupted_valid_frames_never_panic() {
    let mut rng = Lcg(0x0DDB_1770_5EAF_00D5);
    let header = Header::with_pos(FrameType::Zdata, 0xA5A5_A5A5).encode_bin32();
    let sub = encode_subpacket(
        b"some payload with \x18 and \x11 inside",
        FrameEnd::Zcrcw,
        true,
    )
    .unwrap();
    for _ in 0..300 {
        for original in [&header, &sub] {
            let mut mutated = original.clone();
            let idx = usize::from(rng.next_u8()) % mutated.len();
            mutated[idx] ^= rng.next_u8() | 1;
            feed_all_decoders(&mutated);
        }
    }
}

#[test]
fn session_survives_random_event_storms() {
    let mut rng = Lcg(0xFEED_FACE_0BAD_F00D);
    for _ in 0..200 {
        let mut rx = Receiver::new();
        for _ in 0..40 {
            let event = if rng.next_u8() % 2 == 0 {
                let frame_type = FrameType::ALL[usize::from(rng.next_u8()) % 18];
                RecvEvent::Header(Header::with_pos(frame_type, u32::from(rng.next_u8())))
            } else {
                let end = [
                    FrameEnd::Zcrce,
                    FrameEnd::Zcrcg,
                    FrameEnd::Zcrcq,
                    FrameEnd::Zcrcw,
                ][usize::from(rng.next_u8()) % 4];
                let len = usize::from(rng.next_u8()) % 32;
                RecvEvent::Data {
                    payload: rng.buf(len),
                    end,
                }
            };
            // Errors are fine; panics are not.
            let _ = rx.advance(event);
        }
    }
}
