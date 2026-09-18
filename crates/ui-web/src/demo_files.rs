//! What the seeded demo burrows' files actually contain.
//!
//! A demo download used to "succeed" with a buffer of zeros that nothing ever
//! saved: the row said Done and no file landed. For the file library to be
//! testable end to end the bytes have to be real, so they are. The text file
//! reads, the ANSI screen renders, and the archive is a genuine (if tiny) LHA
//! archive that an unarchiver opens.
//!
//! Pure, host-tested. A seeded node's advertised size is the length of what is
//! here, so the listing and the saved file never disagree.

/// `readme.txt`.
const README: &str = "\
THE WARREN: DEMO FILE LIBRARY
=============================

You are looking at a seeded burrow. Nothing here came over a network: the
demo exists so every part of the client can be tried without running a
server, and that includes downloading.

What is in this area

  readme.txt         This file. If you can read it, downloads work.
  utils/lister.lha   A real LHA archive (method -lh0-) holding LISTER.TXT.
  broken-mirror.lha  Always fails, on purpose, so the failure row, its
                     reason and Retry can be seen without breaking anything.

In the ANSI Gallery, welcome.ans is a CP437 colour screen. The Art section
renders it; any ANSI viewer will too.

Be kind, share freely, and mind the carrots.
";

/// `LISTER.TXT`, the one member of `lister.lha`.
const LISTER_TXT: &str = "\
LISTER 1.2: A FILE LISTER FOR THE WARREN
========================================

Usage:  lister [path]

Lists a directory the way a BBS file area does: name, size, date, and the
first line of the file's description. This copy is a demo fixture: it
exists so the archive you just downloaded has something true inside it.
";

/// `welcome.ans`: a CP437 welcome screen in ANSI colour.
const WELCOME_ANS: &[u8] = b"\x1b[0m\x1b[2J\x1b[1;1H\
\x1b[1;36m  \xC9\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xBB\r\n\
  \xBA\x1b[1;33m   W E L C O M E   T O   T H E  \x1b[1;36m\xBA\r\n\
  \xBA\x1b[1;35m          W A R R E N           \x1b[1;36m\xBA\r\n\
  \xC8\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xBC\r\n\
\r\n\
\x1b[0;32m   \xB0\xB1\xB2\xDB \x1b[1;37mchat \xFA boards \xFA files \xFA radio \x1b[0;32m\xDB\xB2\xB1\xB0\r\n\
\r\n\
\x1b[0;37m   Be kind, share freely, and mind the carrots.\r\n\
\x1b[0m";

/// The bytes of a seeded demo file, by name. `None` for a name the demo does
/// not carry (including the deliberately failing one, which has no bytes to
/// give: that is its job).
pub fn bytes_for(name: &str) -> Option<Vec<u8>> {
    match name {
        "readme.txt" => Some(README.as_bytes().to_vec()),
        "welcome.ans" => Some(WELCOME_ANS.to_vec()),
        "lister.lha" => Some(lha_stored("LISTER.TXT", LISTER_TXT.as_bytes())),
        _ => None,
    }
}

/// CRC-16/ARC, the checksum LHA stores for a member's data.
fn crc16_arc(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// A one-member LHA archive, level-0 header, method `-lh0-` (stored). Small
/// enough to write by hand and real enough that an unarchiver opens it.
fn lha_stored(member: &str, data: &[u8]) -> Vec<u8> {
    let name = member.as_bytes();
    let len = data.len() as u32;
    // 1997-03-14 12:00:00 in MS-DOS date/time, because a BBS utility should
    // look its age.
    let stamp: u32 = (((17u32 << 9) | (3 << 5) | 14) << 16) | (12 << 11);
    let mut header = Vec::with_capacity(24 + name.len());
    header.extend_from_slice(b"-lh0-");
    header.extend_from_slice(&len.to_le_bytes()); // packed size (stored)
    header.extend_from_slice(&len.to_le_bytes()); // original size
    header.extend_from_slice(&stamp.to_le_bytes());
    header.push(0x20); // attribute: archive
    header.push(0); // header level 0
    header.push(name.len() as u8);
    header.extend_from_slice(name);
    header.extend_from_slice(&crc16_arc(data).to_le_bytes());
    let checksum = header.iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
    let mut out = Vec::with_capacity(header.len() + 3 + data.len());
    out.push(header.len() as u8);
    out.push(checksum);
    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    out.push(0); // end of archive
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_the_one_every_lha_tool_expects() {
        // The CRC-16/ARC check value for "123456789".
        assert_eq!(crc16_arc(b"123456789"), 0xBB3D);
    }

    #[test]
    fn the_archive_is_a_well_formed_stored_lha_member() {
        let lha = bytes_for("lister.lha").expect("seeded");
        let header_len = lha[0] as usize;
        let header = &lha[2..2 + header_len];
        assert_eq!(&header[..5], b"-lh0-");
        let sum = header.iter().fold(0u8, |s, b| s.wrapping_add(*b));
        assert_eq!(lha[1], sum, "header checksum");
        let packed = u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize;
        let original = u32::from_le_bytes(header[9..13].try_into().unwrap()) as usize;
        assert_eq!(packed, original, "stored, not compressed");
        // Header bytes are file offsets minus two: attribute 17, level 18,
        // name length 19, then the name, then the data's CRC.
        assert_eq!(header[18], 0, "a level-0 header");
        let name_len = header[19] as usize;
        assert_eq!(&header[20..20 + name_len], b"LISTER.TXT");
        let data = &lha[2 + header_len..2 + header_len + packed];
        assert_eq!(data, LISTER_TXT.as_bytes());
        let crc = u16::from_le_bytes(header[20 + name_len..22 + name_len].try_into().unwrap());
        assert_eq!(crc, crc16_arc(data));
        assert_eq!(*lha.last().unwrap(), 0, "end-of-archive marker");
        assert_eq!(lha.len(), 2 + header_len + packed + 1);
    }

    #[test]
    fn the_text_reads_and_the_failing_file_has_nothing_to_give() {
        let readme = String::from_utf8(bytes_for("readme.txt").unwrap()).unwrap();
        assert!(readme.contains("downloads work"));
        assert!(bytes_for("welcome.ans").unwrap().starts_with(b"\x1b["));
        assert_eq!(bytes_for(crate::client::FAILING_DEMO_FILE), None);
        assert_eq!(bytes_for("nope.bin"), None);
    }
}
