//! Golden wire-line tests and a deterministic totality sweep for the NNTP
//! text codecs (RFC 3977).
//!
//! NNTP is line-oriented, so "the wire" is exact bytes of ASCII lines. These
//! fixtures pin a full `OVER`/`XOVER` overview line and its framing inside a
//! multi-line data block, then sweep every line parser with mutated and random
//! input to prove they are total (the parsers must never panic on hostile
//! input — only ever `Ok`/`Err`).

#![forbid(unsafe_code)]

use rabbithole_legacy_nntp::{
    decode_block, decode_lines, encode_lines, Command, MessageId, Overview, Response,
};

/// A representative overview record.
fn golden_overview() -> Overview {
    Overview {
        number: 3000,
        subject: "Re: warren dig progress".to_string(),
        from: "Kevin <kevin@phunc.com>".to_string(),
        date: "Wed, 01 Jul 2026 12:00:00 +0000".to_string(),
        message_id: MessageId::new("<abc@news.example.com>").unwrap(),
        references: vec![
            MessageId::new("<root@news.example.com>").unwrap(),
            MessageId::new("<parent@news.example.com>").unwrap(),
        ],
        bytes: 4321,
        lines: 42,
    }
}

/// The exact overview line body (no CRLF): eight tab-separated columns in
/// `OVERVIEW.FMT` order — number, Subject, From, Date, Message-ID, References
/// (space-joined), :bytes, :lines.
const OVERVIEW_LINE: &str = "3000\t\
     Re: warren dig progress\t\
     Kevin <kevin@phunc.com>\t\
     Wed, 01 Jul 2026 12:00:00 +0000\t\
     <abc@news.example.com>\t\
     <root@news.example.com> <parent@news.example.com>\t\
     4321\t42";

#[test]
fn overview_line_is_golden() {
    assert_eq!(golden_overview().encode(), OVERVIEW_LINE);
    // Exactly eight columns, single-tab separated.
    assert_eq!(OVERVIEW_LINE.split('\t').count(), 8);
    let parsed = Overview::parse(OVERVIEW_LINE).expect("golden parses");
    assert_eq!(parsed, golden_overview());
    // Round-trip fixed point.
    assert_eq!(parsed.encode(), OVERVIEW_LINE);
}

/// The same record framed as a `224` overview data block: the line, then the
/// dot-terminator, all CRLF-delimited.
const OVERVIEW_BLOCK: &str = "3000\t\
     Re: warren dig progress\t\
     Kevin <kevin@phunc.com>\t\
     Wed, 01 Jul 2026 12:00:00 +0000\t\
     <abc@news.example.com>\t\
     <root@news.example.com> <parent@news.example.com>\t\
     4321\t42\r\n.\r\n";

#[test]
fn overview_data_block_is_golden() {
    let block = encode_lines(&[OVERVIEW_LINE]);
    assert_eq!(block, OVERVIEW_BLOCK);
    // And the block decodes back to exactly the one overview line.
    let lines = decode_lines(OVERVIEW_BLOCK).expect("block decodes");
    assert_eq!(lines, vec![OVERVIEW_LINE]);
    assert_eq!(Overview::parse(&lines[0]).unwrap(), golden_overview());
}

/// Dot-stuffing golden: a body whose lines start with '.' must be doubled on
/// send and undoubled on receive, framed with a lone-dot terminator.
#[test]
fn dot_stuffing_is_golden() {
    let body = ".signature line\nplain line";
    // decode(encode(body)) is a fixed point through the on-wire doubled form.
    let wire = rabbithole_legacy_nntp::encode_block(body);
    assert_eq!(wire, "..signature line\r\nplain line\r\n.\r\n");
    assert_eq!(decode_block(&wire).unwrap(), body);
}

/// Deterministic 64-bit LCG; std only, reproducible without `rand`.
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
        // A mix of structural bytes and printable ASCII so the parsers see tab
        // columns, dots, CRLFs and message-id brackets.
        const ALPHABET: &[u8] = b"abc \t\r\n.<>@:0129-/\0";
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

fn feed_all(s: &str) {
    // None of these may panic regardless of input; Results are discarded.
    let _ = Overview::parse(s);
    let _ = decode_lines(s);
    let _ = decode_block(s);
    let _ = Command::parse(s);
    let _ = Response::parse(s);
    let _ = MessageId::new(s);
}

#[test]
fn line_parsers_never_panic_on_random_input() {
    let mut rng = Lcg(0x0DDF_1E1D_5EED_0001);
    for _ in 0..20_000 {
        let len = usize::from(rng.next_u8()) % 96;
        let s = rng.string(len);
        feed_all(&s);
    }
}

#[test]
fn mutations_of_golden_never_panic() {
    let seeds = [
        OVERVIEW_LINE,
        OVERVIEW_BLOCK,
        "GROUP misc.test",
        "220 <a@b>",
    ];
    let mut rng = Lcg(0xC0DE_F00D_1234_5678);
    for seed in seeds {
        // Every truncation.
        for cut in 0..=seed.len() {
            if seed.is_char_boundary(cut) {
                feed_all(&seed[..cut]);
            }
        }
        // Random single-character substitutions.
        for _ in 0..2_000 {
            let mut chars: Vec<char> = seed.chars().collect();
            if chars.is_empty() {
                continue;
            }
            let idx = usize::from(rng.next_u8()) % chars.len();
            chars[idx] = char::from(rng.next_u8());
            let mutated: String = chars.into_iter().collect();
            feed_all(&mutated);
        }
    }
}
