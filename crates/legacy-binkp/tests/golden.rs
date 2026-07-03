//! Golden byte-vector tests for the binkp frame codec.
//!
//! A binkp block is a 2-byte big-endian header (top bit = command vs. data,
//! low 15 bits = body length) followed by the body. Command bodies start with a
//! 1-byte `M_*` id. These fixtures pin the exact wire bytes of representative
//! frames so the framing and the typed command layer stay byte-compatible with
//! real FTN mailers; each asserts decode → typed parse → re-encode is a fixed
//! point. (Totality/never-panic sweeps live in `tests/fuzz.rs`.)

#![forbid(unsafe_code)]

use rabbithole_legacy_binkp::{decode_block, Address, Command, FileInfo, RawBlock};

/// `M_NUL` (id 0) carrying an info line.
///
/// ```text
/// 80 13            header: command bit + body length 0x13 = 19
/// 00               M_NUL id
/// "SYS RabbitHole BBS"  18 bytes of args
/// ```
const NUL_GOLDEN: &[u8] = b"\x80\x13\x00SYS RabbitHole BBS";

/// `M_ADR` (id 1) with a single 5D FTN address.
///
/// ```text
/// 80 16            header: command bit + body length 0x16 = 22
/// 01               M_ADR id
/// "2:5020/1042.0@fidonet"  21 bytes of args
/// ```
const ADR_GOLDEN: &[u8] = b"\x80\x16\x012:5020/1042.0@fidonet";

/// `M_FILE` (id 3): `name size unixtime offset`.
///
/// ```text
/// 80 1E            header: command bit + body length 0x1E = 30
/// 03               M_FILE id
/// "netmail.pkt 1234 1700000000 0"  29 bytes of args
/// ```
const FILE_GOLDEN: &[u8] = b"\x80\x1E\x03netmail.pkt 1234 1700000000 0";

/// A raw data block (top header bit clear).
///
/// ```text
/// 00 08            header: data, body length 8
/// "BINKDATA"       8 payload bytes
/// ```
const DATA_GOLDEN: &[u8] = b"\x00\x08BINKDATA";

/// Decode a golden frame, assert it is exactly one whole block, and return it.
fn decode_whole(golden: &[u8]) -> RawBlock {
    let (block, used) = decode_block(golden).expect("golden decodes");
    assert_eq!(used, golden.len(), "golden must be exactly one block");
    block
}

#[test]
fn nul_frame_is_golden() {
    let block = decode_whole(NUL_GOLDEN);
    let cmd = Command::from_block(&block).expect("typed parse");
    assert_eq!(cmd, Command::Nul("SYS RabbitHole BBS".into()));
    assert_eq!(cmd.to_block().encode().unwrap(), NUL_GOLDEN);
}

#[test]
fn adr_frame_is_golden() {
    let block = decode_whole(ADR_GOLDEN);
    let cmd = Command::from_block(&block).expect("typed parse");
    assert_eq!(
        cmd,
        Command::Adr(vec![Address::new(2, 5020, 1042, 0).with_domain("fidonet")])
    );
    assert_eq!(cmd.to_block().encode().unwrap(), ADR_GOLDEN);
}

#[test]
fn file_frame_is_golden() {
    let block = decode_whole(FILE_GOLDEN);
    let cmd = Command::from_block(&block).expect("typed parse");
    assert_eq!(
        cmd,
        Command::File(FileInfo::new("netmail.pkt", 1234, 1_700_000_000))
    );
    assert_eq!(cmd.to_block().encode().unwrap(), FILE_GOLDEN);
}

#[test]
fn data_frame_is_golden() {
    let block = decode_whole(DATA_GOLDEN);
    assert_eq!(block, RawBlock::Data(b"BINKDATA".to_vec()));
    assert_eq!(block.encode().unwrap(), DATA_GOLDEN);
}

#[test]
fn goldens_concatenate_into_one_stream() {
    // A real session interleaves frames back-to-back; decoding must walk them
    // one block at a time using the returned `used` length.
    let mut stream = Vec::new();
    for g in [NUL_GOLDEN, ADR_GOLDEN, FILE_GOLDEN, DATA_GOLDEN] {
        stream.extend_from_slice(g);
    }
    let mut pos = 0;
    let mut blocks = Vec::new();
    while pos < stream.len() {
        let (block, used) = decode_block(&stream[pos..]).expect("block");
        blocks.push(block);
        pos += used;
    }
    assert_eq!(blocks.len(), 4);
    assert_eq!(pos, stream.len());
    assert_eq!(
        Command::from_block(&blocks[0]).unwrap(),
        Command::Nul("SYS RabbitHole BBS".into())
    );
    assert_eq!(blocks[3], RawBlock::Data(b"BINKDATA".to_vec()));
}
