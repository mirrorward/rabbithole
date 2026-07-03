//! A minimal, dependency-free **STORE-method** ZIP writer — the outer envelope
//! a `.QWK` / `.REP` packet is delivered in.
//!
//! QWK packets are ZIP archives of the members this crate encodes
//! (`MESSAGES.DAT`, `CONTROL.DAT`, `.NDX`, `DOOR.ID`). The classic BBS door
//! programs that produced them used STORE (no compression) as often as not,
//! and STORE keeps this dependency-free and **deterministic** — the same
//! members always yield byte-identical output (fixed 0 mod-time/date), which is
//! what the golden tests pin. Any conformant unzip reads the result; the
//! round-trip test here reads it back with a tiny in-tree parser.
//!
//! Deliberately write-only and STORE-only: reading/inflating an inbound `.REP`
//! ZIP is a separate concern (today the `.REP` ingest path takes the already
//! extracted `<BBSID>.MSG` member, see [`crate::reply`]).

/// CRC-32 (IEEE 802.3, the ZIP polynomial), bitwise so there is no static
/// table to carry. Matches the standard `crc32("123456789") == 0xCBF43926`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Bundle `members` (`(filename, bytes)`) into a STORE-method ZIP archive.
///
/// Pure, total, and deterministic: no timestamps, no compression, entries in
/// the order given. Sizes are `u32` (the classic ZIP format) — a QWK packet is
/// far below the 4 GiB ceiling.
pub fn zip_store(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();

    for (name, data) in members {
        let name = name.as_bytes();
        let crc = crc32(data);
        let size = data.len() as u32;
        let offset = out.len() as u32;

        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // signature
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed (2.0)
        out.extend_from_slice(&0u16.to_le_bytes()); // general-purpose flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method 0 = store
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time (fixed)
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date (fixed)
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed == stored
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra-field length
        out.extend_from_slice(name);
        out.extend_from_slice(data);

        // Matching central-directory record.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // signature
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // mod time
        central.extend_from_slice(&0u16.to_le_bytes()); // mod date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes()); // local header offset
        central.extend_from_slice(name);
    }

    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);

    // End of central directory record.
    let n = members.len() as u16;
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // signature
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with central dir
    out.extend_from_slice(&n.to_le_bytes()); // entries this disk
    out.extend_from_slice(&n.to_le_bytes()); // entries total
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    /// A tiny STORE-only reader: walk the local file headers (flags = 0 means
    /// the sizes live in the header, no data descriptor) until the central
    /// directory. Proves the writer's output is self-consistent + readable.
    fn read_store_zip(b: &[u8]) -> Vec<(String, Vec<u8>)> {
        let u16at = |i: usize| u16::from_le_bytes(b[i..i + 2].try_into().unwrap()) as usize;
        let u32at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
        let mut out = Vec::new();
        let mut i = 0;
        while i + 4 <= b.len() && b[i..i + 4] == 0x0403_4b50u32.to_le_bytes() {
            let comp = u32at(i + 18);
            let nlen = u16at(i + 26);
            let elen = u16at(i + 28);
            let name = String::from_utf8_lossy(&b[i + 30..i + 30 + nlen]).into_owned();
            let data_start = i + 30 + nlen + elen;
            out.push((name, b[data_start..data_start + comp].to_vec()));
            i = data_start + comp;
        }
        out
    }

    #[test]
    fn zip_is_deterministic_and_round_trips() {
        let members: &[(&str, &[u8])] = &[
            ("MESSAGES.DAT", b"the message blocks"),
            ("CONTROL.DAT", b"BBS\r\nconf list\r\n"),
            ("000.NDX", &[0x00, 0x80, 0x40, 0x00]),
        ];
        let zip = zip_store(members);

        // Same input → byte-identical output (no timestamps).
        assert_eq!(zip, zip_store(members), "deterministic");
        // Recognisable envelope.
        assert!(
            zip.starts_with(&0x0403_4b50u32.to_le_bytes()),
            "local header"
        );
        assert!(
            zip.windows(4).any(|w| w == 0x0605_4b50u32.to_le_bytes()),
            "end-of-central-directory record present"
        );

        // Reads back to exactly the members that went in, in order.
        let back = read_store_zip(&zip);
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].0, "MESSAGES.DAT");
        assert_eq!(back[0].1, b"the message blocks");
        assert_eq!(back[2].0, "000.NDX");
        assert_eq!(back[2].1, &[0x00, 0x80, 0x40, 0x00]);
    }

    #[test]
    fn empty_archive_is_valid() {
        let zip = zip_store(&[]);
        // Just the 22-byte EOCD, zero entries.
        assert_eq!(zip.len(), 22);
        assert!(zip.starts_with(&0x0605_4b50u32.to_le_bytes()));
        assert!(read_store_zip(&zip).is_empty());
    }
}
