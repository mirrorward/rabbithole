use rabbithole_legacy_qwk::{
    crc32,
    rep_archive::{self, MAX_ARCHIVE_BYTES, MAX_ENTRIES, MAX_MEMBER_BYTES, MAX_REPLIES},
    zip_store, ReplyMessage, ReplyPacket,
};
use std::io::Write;
fn packet() -> ReplyPacket {
    ReplyPacket {
        header: "WARREN".into(),
        replies: vec![ReplyMessage::new(
            1,
            "ALL",
            "ALICE",
            "Hello",
            "A reply body",
        )],
    }
}
fn set16(b: &mut [u8], p: usize, n: usize) {
    b[p..p + 2].copy_from_slice(&(n as u16).to_le_bytes());
}
fn set32(b: &mut [u8], p: usize, n: usize) {
    b[p..p + 4].copy_from_slice(&(n as u32).to_le_bytes());
}
fn central(b: &[u8]) -> usize {
    let end = b.len() - 22;
    u32::from_le_bytes(b[end + 16..end + 20].try_into().unwrap()) as usize
}
// Independent classic ZIP construction, including streaming-writer descriptor
// layouts. Compression uses flate2; other bytes follow APPNOTE field offsets.
fn zip(data: &[u8], descriptor: usize, payload: Option<Vec<u8>>) -> Vec<u8> {
    let payload = payload.unwrap_or_else(|| {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    });
    let name = b"WARREN.MSG";
    let mut b = vec![0; 30];
    set32(&mut b, 0, 0x04034b50);
    set16(&mut b, 4, 20);
    set16(&mut b, 6, if descriptor > 0 { 8 } else { 0 });
    set16(&mut b, 8, 8);
    set16(&mut b, 26, name.len());
    if descriptor == 0 {
        set32(&mut b, 14, crc32(data) as usize);
        set32(&mut b, 18, payload.len());
        set32(&mut b, 22, data.len());
    }
    b.extend(name);
    b.extend(&payload);
    if descriptor > 0 {
        if descriptor == 16 {
            b.extend(0x08074b50u32.to_le_bytes());
        }
        b.extend(crc32(data).to_le_bytes());
        b.extend((payload.len() as u32).to_le_bytes());
        b.extend((data.len() as u32).to_le_bytes());
    }
    let at = b.len();
    let mut c = vec![0; 46];
    set32(&mut c, 0, 0x02014b50);
    set16(&mut c, 4, 20);
    set16(&mut c, 6, 20);
    set16(&mut c, 8, if descriptor > 0 { 8 } else { 0 });
    set16(&mut c, 10, 8);
    set32(&mut c, 16, crc32(data) as usize);
    set32(&mut c, 20, payload.len());
    set32(&mut c, 24, data.len());
    set16(&mut c, 28, name.len());
    b.extend(c);
    b.extend(name);
    let mut end = vec![0; 22];
    set32(&mut end, 0, 0x06054b50);
    set16(&mut end, 8, 1);
    set16(&mut end, 10, 1);
    set32(&mut end, 12, b.len() - at);
    set32(&mut end, 16, at);
    b.extend(end);
    b
}
#[test]
fn store_deflate_descriptors_and_safe_extra_members() {
    let p = packet();
    let bytes = p.encode();
    let stored = zip_store(&[("warren.msg", &bytes), ("READER.TXT", b"ignored metadata")]);
    assert_eq!(rep_archive::parse(&stored, "WARREN").unwrap(), p);
    for descriptor in [0, 12, 16] {
        assert_eq!(
            rep_archive::parse(&zip(&bytes, descriptor, None), "WARREN").unwrap(),
            p
        );
    }
}
#[test]
fn malformed_identity_paths_duplicates_and_features_are_rejected() {
    let bytes = packet().encode();
    for name in [
        "../WARREN.MSG",
        "/WARREN.MSG",
        "C:WARREN.MSG",
        "dir\\WARREN.MSG",
        ".",
        "..",
        "bad\n.MSG",
    ] {
        assert!(
            rep_archive::parse(&zip_store(&[(name, &bytes)]), "WARREN").is_err(),
            "{name:?}"
        );
    }
    assert!(rep_archive::parse(
        &zip_store(&[("WARREN.MSG", &bytes), ("warren.msg", &bytes)]),
        "WARREN"
    )
    .is_err());
    assert!(rep_archive::parse(&zip_store(&[("OTHER.MSG", &bytes)]), "WARREN").is_err());
    let mut wrong = packet();
    wrong.header = "OTHER".into();
    assert!(rep_archive::parse(&zip_store(&[("WARREN.MSG", &wrong.encode())]), "WARREN").is_err());
    let b = zip_store(&[("WARREN.MSG", &bytes)]);
    let c = central(&b);
    for (offset, value) in [
        (c + 8, 1),
        (c + 10, 99),
        (c + 34, 1),
        (c + 6, 45),
        (c + 38, 0x10),
    ] {
        let mut bad = b.clone();
        set16(&mut bad, offset, value);
        assert!(rep_archive::parse(&bad, "WARREN").is_err());
    }
    let mut bad = b.clone();
    set32(&mut bad, c + 42, u32::MAX as usize);
    assert!(rep_archive::parse(&bad, "WARREN").is_err());
    let mut bad = b.clone();
    set32(&mut bad, c + 38, 0xa000_0000);
    assert!(rep_archive::parse(&bad, "WARREN").is_err());
    let mut bad = b.clone();
    bad[30] = b'X';
    assert!(rep_archive::parse(&bad, "WARREN").is_err());
    let mut bad = b.clone();
    bad.push(0);
    assert!(rep_archive::parse(&bad, "WARREN").is_err());
    let mut bad = b.clone();
    bad[40] ^= 1;
    assert!(rep_archive::parse(&bad, "WARREN").is_err());
}
#[test]
fn deflate_requires_final_block_and_exact_input_output_crc() {
    let data = packet().encode();
    // Raw DEFLATE stored block: clearing BFINAL still emits every byte and
    // matches the advertised size/CRC, but must not be accepted as complete.
    let mut raw = vec![1];
    raw.extend((data.len() as u16).to_le_bytes());
    raw.extend((!(data.len() as u16)).to_le_bytes());
    raw.extend(&data);
    assert!(rep_archive::parse(&zip(&data, 0, Some(raw.clone())), "WARREN").is_ok());
    raw[0] = 0;
    assert!(rep_archive::parse(&zip(&data, 0, Some(raw)), "WARREN").is_err());
    let mut good = zip(&data, 16, None);
    let c = central(&good);
    let crc = u32::from_le_bytes(good[c + 16..c + 20].try_into().unwrap());
    set32(&mut good, c + 16, crc.wrapping_add(1) as usize);
    assert!(rep_archive::parse(&good, "WARREN").is_err());
    let mut raw = vec![1];
    raw.extend((data.len() as u16).to_le_bytes());
    raw.extend((!(data.len() as u16)).to_le_bytes());
    raw.extend(&data);
    raw.push(0);
    assert!(rep_archive::parse(&zip(&data, 0, Some(raw)), "WARREN").is_err());
}
#[test]
fn sizes_counts_and_every_truncation_are_bounded() {
    let bytes = packet().encode();
    let b = zip(&bytes, 16, None);
    for n in 0..b.len() {
        assert!(rep_archive::parse(&b[..n], "WARREN").is_err());
    }
    assert!(rep_archive::parse(&vec![0; MAX_ARCHIVE_BYTES + 1], "WARREN").is_err());
    let mut bad = b.clone();
    let c = central(&bad);
    set32(&mut bad, c + 24, MAX_MEMBER_BYTES + 1);
    assert!(rep_archive::parse(&bad, "WARREN").is_err());
    let names: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("{i}.MSG")).collect();
    let members: Vec<_> = names
        .iter()
        .map(|n| (n.as_str(), bytes.as_slice()))
        .collect();
    assert!(rep_archive::parse(&zip_store(&members), "WARREN").is_err());
    let mut p = packet();
    p.replies = vec![p.replies[0].clone(); MAX_REPLIES + 1];
    assert!(rep_archive::parse(&zip(&p.encode(), 0, None), "WARREN").is_err());
    // Every single-byte mutation is total (some benign metadata edits remain valid).
    for i in 0..b.len() {
        let mut bad = b.clone();
        bad[i] ^= 0xff;
        let _ = rep_archive::parse(&bad, "WARREN");
    }
}

#[test]
fn independently_written_python_zipfile_fixture() {
    // Checked-in Python stdlib zipfile ZIP_DEFLATED, timestamp 2026-09-26
    // 12:30. QWK records constructed from field offsets, not this codec.
    let packet =
        rep_archive::parse(include_bytes!("fixtures/python-deflate.rep"), "WARREN").unwrap();
    assert_eq!(packet.header, "WARREN");
    assert_eq!(packet.replies.len(), 1);
    assert_eq!(packet.replies[0].conference, 1);
    assert_eq!(packet.replies[0].subject, "Independent fixture");
    assert_eq!(packet.replies[0].body, "From Python zipfile.");
}
