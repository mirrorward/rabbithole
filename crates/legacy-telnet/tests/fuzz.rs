//! Golden negotiation exchange plus a deterministic totality sweep for the
//! telnet option parser.
//!
//! `Parser::feed` is the wire decoder: it splits an arbitrary byte stream into
//! data, IAC commands and subnegotiations, and must be total — no input,
//! however hostile or fragmented, may panic. These tests pin one exact
//! IAC exchange (bytes -> events) and then hammer the parser with random,
//! truncated, and mutated byte streams using a seeded generator (no `rand`).

#![forbid(unsafe_code)]

use rabbithole_legacy_telnet::proto::{opt, DO, DONT, IAC, SB, SE, TTYPE_IS, WILL};
use rabbithole_legacy_telnet::{Event, Parser};

fn parse(bytes: &[u8]) -> Vec<Event> {
    let mut p = Parser::new();
    let mut ev = Vec::new();
    p.feed(bytes, &mut ev);
    ev
}

/// A pinned client->server exchange: agree to SGA, offer NAWS + TTYPE, report
/// an 80x24 window, and answer a terminal-type subnegotiation with "ANSI",
/// with a doubled IAC (0xFF 0xFF) inside a leading data run.
#[rustfmt::skip]
const EXCHANGE: &[u8] = &[
    b'h', b'i', IAC, IAC, b'!',              // data "hi" + literal 0xFF + "!"
    IAC, WILL, opt::SGA,                     // WILL SGA
    IAC, DO, opt::NAWS,                      // DO NAWS
    IAC, SB, opt::NAWS, 0, 80, 0, 24, IAC, SE, // NAWS window 80x24
    IAC, SB, opt::TTYPE, TTYPE_IS, b'A', b'N', b'S', b'I', IAC, SE, // TTYPE IS "ANSI"
];

#[test]
fn negotiation_exchange_is_golden() {
    let events = parse(EXCHANGE);
    assert_eq!(
        events,
        vec![
            Event::Data(b"hi\xFF!".to_vec()), // doubled IAC undoubles to one 0xFF
            Event::Will(opt::SGA),
            Event::Do(opt::NAWS),
            Event::Subnegotiation(opt::NAWS, vec![0, 80, 0, 24]),
            Event::Subnegotiation(opt::TTYPE, b"\x00ANSI".to_vec()),
        ]
    );
}

/// Merge runs of adjacent `Data` events into one, leaving command events as-is.
/// Byte-at-a-time feeding legitimately emits one `Data` event per byte; the
/// decoded *content* must still be identical once coalesced.
fn coalesce(events: Vec<Event>) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::new();
    for ev in events {
        match (out.last_mut(), ev) {
            (Some(Event::Data(prev)), Event::Data(more)) => prev.extend(more),
            (_, ev) => out.push(ev),
        }
    }
    out
}

#[test]
fn feeding_the_exchange_one_byte_at_a_time_yields_the_same_events() {
    // Fragmentation must not change the decode: a command split across feeds is
    // buffered until complete, and data (however chunked) reassembles to the
    // same bytes.
    let mut p = Parser::new();
    let mut ev = Vec::new();
    for &b in EXCHANGE {
        p.feed(&[b], &mut ev);
    }
    assert_eq!(coalesce(ev), parse(EXCHANGE));
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
fn parser_never_panics_on_random_bytes() {
    let mut rng = Lcg(0x7E10_E700_1234_5678);
    for _ in 0..20_000 {
        let len = usize::from(rng.next_u8()) % 128;
        let _ = parse(&rng.buf(len));
    }
}

#[test]
fn parser_never_panics_on_iac_heavy_streams() {
    // Bias toward telnet control bytes so more of the command/subneg state
    // machine is exercised.
    let mut rng = Lcg(0xFACE_B00C_0000_0001);
    let control = [IAC, DO, DONT, WILL, SB, SE, opt::NAWS, opt::TTYPE, 0, 1];
    for _ in 0..20_000 {
        let len = usize::from(rng.next_u8()) % 96;
        let buf: Vec<u8> = (0..len)
            .map(|_| {
                let b = rng.next_u8();
                if b & 3 == 0 {
                    b
                } else {
                    control[usize::from(b >> 2) % control.len()]
                }
            })
            .collect();
        // Feed in random-sized chunks so state carries across calls.
        let mut p = Parser::new();
        let mut ev = Vec::new();
        let mut pos = 0;
        while pos < buf.len() {
            let step = 1 + usize::from(rng.next_u8()) % 7;
            let end = (pos + step).min(buf.len());
            p.feed(&buf[pos..end], &mut ev);
            pos = end;
        }
    }
}

#[test]
fn mutations_of_the_golden_exchange_never_panic() {
    let mut rng = Lcg(0x0DDB_1770_5EAF_00D5);
    for cut in 0..=EXCHANGE.len() {
        let _ = parse(&EXCHANGE[..cut]);
    }
    for _ in 0..5_000 {
        let mut m = EXCHANGE.to_vec();
        let idx = usize::from(rng.next_u8()) % m.len();
        m[idx] ^= rng.next_u8() | 1;
        let _ = parse(&m);
    }
}
