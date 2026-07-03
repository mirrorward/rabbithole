//! Golden byte-vector test for the `MESSAGES.DAT` codec.
//!
//! `MESSAGES.DAT` is a flat run of 128-byte records: a producer header block,
//! then for each message a 128-byte header block followed by its body blocks.
//! Bodies use the single byte `0xE3` as end-of-line and are space-padded to a
//! block boundary. This test pins the exact bytes of a one-message file so the
//! record layout (every documented offset) is locked, and asserts both
//! directions of the round-trip against those bytes.

#![forbid(unsafe_code)]

use rabbithole_legacy_qwk::messages::{BLOCK, DEFAULT_PRODUCER, EOL};
use rabbithole_legacy_qwk::{MessagesDat, QwkMessage};

/// Build the exact 3-block (producer + header + body) image by hand, writing
/// each field at its documented absolute offset over a space fill.
fn golden_image() -> Vec<u8> {
    let mut img = vec![b' '; 3 * BLOCK];

    // ---- Block 0: producer header ("Produced by ...", space-padded) ----
    let producer = DEFAULT_PRODUCER.as_bytes();
    img[..producer.len()].copy_from_slice(producer);

    // ---- Block 1: message header (offsets per MESSAGES.DAT spec) ----
    let h = BLOCK;
    img[h] = b' '; //                       [0]      status: public/unread
    img[h + 1..h + 3].copy_from_slice(b"42"); //     [1..8]   number "42" (space-padded)
                                              //                                                [8..16]  date  "" -> spaces
                                              //                                                [16..21] time  "" -> spaces
    img[h + 21..h + 24].copy_from_slice(b"ALL"); //  [21..46] To    (space-padded)
    img[h + 46..h + 51].copy_from_slice(b"SYSOP"); //[46..71] From
    img[h + 71..h + 73].copy_from_slice(b"Hi"); //   [71..96] Subject
                                                //                                                [96..108]  password "" -> spaces
                                                //                                                [108..116] reference "" -> spaces
    img[h + 116] = b'2'; //                          [116..122] block count = 2 (header+1 body)
    img[h + 122] = 0xE1; //                          [122]     active flag (0xE1 = active)
    img[h + 123] = 0x01; //                          [123..125] conference 1, little-endian
    img[h + 124] = 0x00;
    img[h + 125] = 0x01; //                          [125..127] logical index 1, little-endian
    img[h + 126] = 0x00;
    img[h + 127] = 0x00; //                          [127]     filler

    // ---- Block 2: body, "Hello\nWorld" with 0xE3 line ending, space-padded ----
    let b = 2 * BLOCK;
    img[b..b + 5].copy_from_slice(b"Hello");
    img[b + 5] = EOL; // the '\n' becomes 0xE3 on the wire
    img[b + 6..b + 11].copy_from_slice(b"World");

    img
}

fn golden_message() -> QwkMessage {
    // number 42, conference 1, To/From/Subject, one `\n` in the body.
    QwkMessage::new(1, 42, "ALL", "SYSOP", "Hi", "Hello\nWorld")
}

#[test]
fn messages_dat_encodes_to_golden_image() {
    let dat = MessagesDat::new(vec![golden_message()]);
    let bytes = dat.encode();
    assert_eq!(bytes.len(), 3 * BLOCK, "producer + header + one body block");
    assert_eq!(bytes, golden_image(), "MESSAGES.DAT record layout drifted");
}

#[test]
fn golden_image_decodes_to_message() {
    let img = golden_image();
    let dat = MessagesDat::decode(&img).expect("golden decodes");
    assert_eq!(dat.producer, DEFAULT_PRODUCER);
    assert_eq!(dat.messages.len(), 1);
    assert_eq!(dat.messages[0], golden_message());

    // Round-trip fixed point.
    assert_eq!(dat.encode(), img);
}

#[test]
fn body_uses_e3_not_crlf() {
    let img = golden_image();
    // The body block never contains a CR or LF; the line break is the single
    // 0xE3 byte at the start of the body block.
    let body = &img[2 * BLOCK..];
    assert!(!body.contains(&b'\r') && !body.contains(&b'\n'));
    assert_eq!(body.iter().filter(|&&b| b == EOL).count(), 1);
}
