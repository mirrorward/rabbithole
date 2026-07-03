//! Flattened file objects (FFO) and the fork-offset resume structure — the
//! payload formats of the classic **HTXF** bulk-transfer channel.
//!
//! A Hotline file travels the data channel as a *flattened file object*: a
//! fixed `FILP` header, then one fork per section, each introduced by a
//! 16-byte fork header. The `INFO` fork carries the file's metadata
//! (platform, type/creator codes, dates, name, comment); the `DATA` fork
//! carries the raw bytes; an optional `MACR` fork carries a Mac resource
//! fork. A resumed transfer quotes per-fork offsets in an `RFLT` *resume
//! structure* (field 203, `FILE_RESUME_DATA`).
//!
//! Layouts are wire-verified against Hotline 1.9 and the Mobius server
//! (`hotline/flattened_file_object.go`, `hotline/file_resume_data.go`).
//!
//! ## FLAT header (24 bytes)
//!
//! ```text
//! offset  size  field       value
//! ------  ----  ----------  -----------------------------------------
//!   0      4    format      'F' 'I' 'L' 'P'
//!   4      2    version     0x0001
//!   6     16    reserved    zeros
//!  22      2    fork count  2, or 3 when a resource fork follows
//! ```
//!
//! ## Fork header (16 bytes)
//!
//! ```text
//! offset  size  field        value
//! ------  ----  -----------  ----------------------------------------
//!   0      4    fork type    'INFO' / 'DATA' / 'MACR'
//!   4      4    compression  0 (no compression was ever deployed)
//!   8      4    reserved     zeros
//!  12      4    data size    byte length of the fork body that follows
//! ```
//!
//! ## INFO fork body (72 fixed bytes + name + comment)
//!
//! ```text
//! offset  size  field           notes
//! ------  ----  --------------  -------------------------------------
//!   0      4    platform        'AMAC' or 'MWIN'
//!   4      4    type code       classic four-char file type
//!   8      4    creator code    classic four-char creator
//!  12      4    flags
//!  16      4    platform flags
//!  20     32    reserved
//!  52      8    create date     classic 8-byte date
//!  60      8    modify date     classic 8-byte date
//!  68      2    name script
//!  70      2    name length
//!  72      n    name
//!  72+n    2    comment length  (absent in some 1.2.x writers — optional)
//!  74+n    m    comment
//! ```
//!
//! ## RFLT resume structure (42-byte header + 16 bytes per fork)
//!
//! ```text
//! offset  size  field       value
//! ------  ----  ----------  -----------------------------------------
//!   0      4    format      'R' 'F' 'L' 'T'
//!   4      2    version     0x0001
//!   6     34    reserved    zeros
//!  40      2    fork count  number of fork entries that follow
//! per fork:
//!   0      4    fork type   'DATA' / 'MACR'
//!   4      4    offset      bytes already transferred (resume point)
//!   8      8    reserved    zeros
//! ```

use crate::error::HotlineError;

/// The FLAT-header magic that opens every flattened file object.
pub const FILP_MAGIC: [u8; 4] = *b"FILP";

/// The resume-structure magic.
pub const RFLT_MAGIC: [u8; 4] = *b"RFLT";

/// The metadata (information) fork type.
pub const FORK_INFO: [u8; 4] = *b"INFO";

/// The data fork type.
pub const FORK_DATA: [u8; 4] = *b"DATA";

/// The Mac resource fork type.
pub const FORK_MACR: [u8; 4] = *b"MACR";

/// The `AMAC` platform code carried in an INFO fork.
pub const PLATFORM_AMAC: [u8; 4] = *b"AMAC";

/// The fixed `FILP` header at the front of a flattened file object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatHeader {
    /// Number of forks that follow: 2 (INFO + DATA), or 3 with a resource
    /// fork.
    pub fork_count: u16,
}

impl FlatHeader {
    /// Wire length of the encoded header, in bytes.
    pub const LEN: usize = 24;

    /// Serialize to the fixed 24-byte wire form (version 1, zero reserved).
    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..4].copy_from_slice(&FILP_MAGIC);
        out[4..6].copy_from_slice(&1u16.to_be_bytes());
        out[22..24].copy_from_slice(&self.fork_count.to_be_bytes());
        out
    }

    /// Parse a 24-byte FLAT header, validating the `FILP` magic.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        if bytes.len() < Self::LEN {
            return Err(HotlineError::Truncated {
                need: Self::LEN - bytes.len(),
                have: bytes.len(),
            });
        }
        let magic: [u8; 4] = bytes[0..4].try_into().expect("slice is 4 bytes");
        if magic != FILP_MAGIC {
            return Err(HotlineError::BadProtocolId {
                expected: FILP_MAGIC,
                got: magic,
            });
        }
        Ok(Self {
            fork_count: u16::from_be_bytes([bytes[22], bytes[23]]),
        })
    }
}

/// A 16-byte fork header introducing one fork's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForkHeader {
    /// Fork type: [`FORK_INFO`], [`FORK_DATA`], or [`FORK_MACR`].
    pub fork_type: [u8; 4],
    /// Byte length of the fork body that follows this header.
    pub data_size: u32,
}

impl ForkHeader {
    /// Wire length of an encoded fork header, in bytes.
    pub const LEN: usize = 16;

    /// Serialize to the fixed 16-byte wire form (no compression, zero
    /// reserved — no compression scheme was ever deployed for HTXF).
    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..4].copy_from_slice(&self.fork_type);
        out[12..16].copy_from_slice(&self.data_size.to_be_bytes());
        out
    }

    /// Parse a 16-byte fork header. Total: any fork type is accepted — the
    /// caller decides which forks it understands (unknown forks are skipped
    /// by real clients and servers alike).
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        if bytes.len() < Self::LEN {
            return Err(HotlineError::Truncated {
                need: Self::LEN - bytes.len(),
                have: bytes.len(),
            });
        }
        Ok(Self {
            fork_type: bytes[0..4].try_into().expect("slice is 4 bytes"),
            data_size: u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        })
    }
}

/// The INFO fork body: a file's classic metadata record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoFork {
    /// Platform code, canonically [`PLATFORM_AMAC`].
    pub platform: [u8; 4],
    /// Four-char file type code (e.g. `TEXT`, `BINA`).
    pub type_code: [u8; 4],
    /// Four-char creator code.
    pub creator_code: [u8; 4],
    /// Finder flags (opaque here).
    pub flags: u32,
    /// Platform flags (opaque here).
    pub platform_flags: u32,
    /// Classic 8-byte create date.
    pub create_date: [u8; 8],
    /// Classic 8-byte modify date.
    pub modify_date: [u8; 8],
    /// File name bytes (UTF-8/MacRoman stored verbatim).
    pub name: Vec<u8>,
    /// File comment bytes.
    pub comment: Vec<u8>,
}

/// Fixed-size prefix of an INFO fork body (everything before the name).
const INFO_FIXED_LEN: usize = 72;

impl InfoFork {
    /// A minimal INFO fork: platform `AMAC`, the given codes and name, zero
    /// flags and dates.
    pub fn new(type_code: [u8; 4], creator_code: [u8; 4], name: &[u8], comment: &[u8]) -> Self {
        Self {
            platform: PLATFORM_AMAC,
            type_code,
            creator_code,
            flags: 0,
            platform_flags: 0,
            create_date: [0; 8],
            modify_date: [0; 8],
            name: name.to_vec(),
            comment: comment.to_vec(),
        }
    }

    /// Serialize the fork body. Name and comment are clamped to `u16::MAX`
    /// bytes (the protocol allows 128-byte names; we tolerate more on read
    /// and never emit past the length fields' capacity).
    pub fn encode(&self) -> Vec<u8> {
        let name_len = self.name.len().min(u16::MAX as usize);
        let comment_len = self.comment.len().min(u16::MAX as usize);
        let mut out = Vec::with_capacity(INFO_FIXED_LEN + name_len + 2 + comment_len);
        out.extend_from_slice(&self.platform);
        out.extend_from_slice(&self.type_code);
        out.extend_from_slice(&self.creator_code);
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&self.platform_flags.to_be_bytes());
        out.extend_from_slice(&[0u8; 32]); // reserved
        out.extend_from_slice(&self.create_date);
        out.extend_from_slice(&self.modify_date);
        out.extend_from_slice(&0u16.to_be_bytes()); // name script
        out.extend_from_slice(&(name_len as u16).to_be_bytes());
        out.extend_from_slice(&self.name[..name_len]);
        out.extend_from_slice(&(comment_len as u16).to_be_bytes());
        out.extend_from_slice(&self.comment[..comment_len]);
        out
    }

    /// Parse an INFO fork body. The comment length/bytes are optional (some
    /// 1.2.x-era writers stop after the name); trailing bytes past the
    /// comment are tolerated, as real servers tolerate them.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        if bytes.len() < INFO_FIXED_LEN {
            return Err(HotlineError::Truncated {
                need: INFO_FIXED_LEN - bytes.len(),
                have: bytes.len(),
            });
        }
        let name_len = u16::from_be_bytes([bytes[70], bytes[71]]) as usize;
        let name_end = INFO_FIXED_LEN + name_len;
        if bytes.len() < name_end {
            return Err(HotlineError::Truncated {
                need: name_end - bytes.len(),
                have: bytes.len(),
            });
        }
        let comment = if bytes.len() >= name_end + 2 {
            let comment_len = u16::from_be_bytes([bytes[name_end], bytes[name_end + 1]]) as usize;
            let comment_end = name_end + 2 + comment_len;
            if bytes.len() < comment_end {
                return Err(HotlineError::Truncated {
                    need: comment_end - bytes.len(),
                    have: bytes.len(),
                });
            }
            bytes[name_end + 2..comment_end].to_vec()
        } else {
            Vec::new()
        };
        Ok(Self {
            platform: bytes[0..4].try_into().expect("slice is 4 bytes"),
            type_code: bytes[4..8].try_into().expect("slice is 4 bytes"),
            creator_code: bytes[8..12].try_into().expect("slice is 4 bytes"),
            flags: u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            platform_flags: u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            create_date: bytes[52..60].try_into().expect("slice is 8 bytes"),
            modify_date: bytes[60..68].try_into().expect("slice is 8 bytes"),
            name: bytes[INFO_FIXED_LEN..name_end].to_vec(),
            comment,
        })
    }
}

/// One fork's resume point inside a [`FileResumeData`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForkOffset {
    /// Fork type, usually [`FORK_DATA`].
    pub fork_type: [u8; 4],
    /// Bytes of this fork already transferred — the offset to resume from.
    pub offset: u32,
}

/// The `RFLT` resume structure carried in the `FILE_RESUME_DATA` field
/// (203): per-fork offsets a resumed transfer continues from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResumeData {
    /// Per-fork resume points, in wire order.
    pub forks: Vec<ForkOffset>,
}

/// Fixed-size prefix of an RFLT structure (everything before the entries).
const RFLT_HEADER_LEN: usize = 42;

/// Wire length of one fork entry in an RFLT structure.
const RFLT_FORK_LEN: usize = 16;

impl FileResumeData {
    /// A resume structure with a single DATA-fork offset — what a client or
    /// server quotes for a plain (fork-less) file.
    pub fn for_data_offset(offset: u32) -> Self {
        Self {
            forks: vec![ForkOffset {
                fork_type: FORK_DATA,
                offset,
            }],
        }
    }

    /// The DATA fork's resume offset, if one is listed.
    pub fn data_fork_offset(&self) -> Option<u32> {
        self.forks
            .iter()
            .find(|f| f.fork_type == FORK_DATA)
            .map(|f| f.offset)
    }

    /// Serialize to the wire form (version 1, zero reserved).
    pub fn encode(&self) -> Vec<u8> {
        let count = self.forks.len().min(u16::MAX as usize);
        let mut out = Vec::with_capacity(RFLT_HEADER_LEN + count * RFLT_FORK_LEN);
        out.extend_from_slice(&RFLT_MAGIC);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&[0u8; 34]); // reserved
        out.extend_from_slice(&(count as u16).to_be_bytes());
        for f in &self.forks[..count] {
            out.extend_from_slice(&f.fork_type);
            out.extend_from_slice(&f.offset.to_be_bytes());
            out.extend_from_slice(&[0u8; 8]); // reserved
        }
        out
    }

    /// Parse an RFLT structure, validating the magic. Fork entries beyond
    /// the buffer are an error; a fork count larger than the entries some
    /// writers actually append (a known Mobius quirk) is tolerated by
    /// reading only what is present when the declared count overruns.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        if bytes.len() < RFLT_HEADER_LEN {
            return Err(HotlineError::Truncated {
                need: RFLT_HEADER_LEN - bytes.len(),
                have: bytes.len(),
            });
        }
        let magic: [u8; 4] = bytes[0..4].try_into().expect("slice is 4 bytes");
        if magic != RFLT_MAGIC {
            return Err(HotlineError::BadProtocolId {
                expected: RFLT_MAGIC,
                got: magic,
            });
        }
        let declared = u16::from_be_bytes([bytes[40], bytes[41]]) as usize;
        // Mobius (and some classics) declare fork count 2 while appending a
        // single DATA entry; read the entries actually present, up to the
        // declared count.
        let available = (bytes.len() - RFLT_HEADER_LEN) / RFLT_FORK_LEN;
        let count = declared.min(available);
        let mut forks = Vec::with_capacity(count);
        for i in 0..count {
            let at = RFLT_HEADER_LEN + i * RFLT_FORK_LEN;
            forks.push(ForkOffset {
                fork_type: bytes[at..at + 4].try_into().expect("slice is 4 bytes"),
                offset: u32::from_be_bytes([
                    bytes[at + 4],
                    bytes[at + 5],
                    bytes[at + 6],
                    bytes[at + 7],
                ]),
            });
        }
        Ok(Self { forks })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_header_golden_bytes() {
        let hdr = FlatHeader { fork_count: 2 };
        let bytes = hdr.encode();
        // "FILP", version 1, 16 reserved zeros, fork count 2.
        let mut expected = Vec::new();
        expected.extend_from_slice(b"FILP");
        expected.extend_from_slice(&[0x00, 0x01]);
        expected.extend_from_slice(&[0u8; 16]);
        expected.extend_from_slice(&[0x00, 0x02]);
        assert_eq!(bytes.to_vec(), expected);
        assert_eq!(FlatHeader::decode(&bytes).unwrap(), hdr);
    }

    #[test]
    fn flat_header_rejects_bad_magic_and_short_input() {
        let mut bytes = FlatHeader { fork_count: 2 }.encode();
        bytes[0] = b'X';
        assert!(matches!(
            FlatHeader::decode(&bytes),
            Err(HotlineError::BadProtocolId { .. })
        ));
        assert!(matches!(
            FlatHeader::decode(&[0u8; 10]),
            Err(HotlineError::Truncated { .. })
        ));
    }

    #[test]
    fn fork_header_golden_bytes() {
        let hdr = ForkHeader {
            fork_type: FORK_DATA,
            data_size: 0x0102_0304,
        };
        let bytes = hdr.encode();
        let mut expected = Vec::new();
        expected.extend_from_slice(b"DATA");
        expected.extend_from_slice(&[0u8; 8]); // compression + reserved
        expected.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(bytes.to_vec(), expected);
        assert_eq!(ForkHeader::decode(&bytes).unwrap(), hdr);
        assert!(matches!(
            ForkHeader::decode(&[0u8; 3]),
            Err(HotlineError::Truncated { .. })
        ));
    }

    #[test]
    fn info_fork_roundtrips_name_and_comment() {
        let fork = InfoFork::new(*b"TEXT", *b"ttxt", b"readme.txt", b"hello there");
        let bytes = fork.encode();
        // Fixed prefix + name + comment length + comment.
        assert_eq!(bytes.len(), 72 + 10 + 2 + 11);
        assert_eq!(&bytes[0..4], b"AMAC");
        assert_eq!(&bytes[4..8], b"TEXT");
        assert_eq!(&bytes[70..72], &10u16.to_be_bytes());
        let back = InfoFork::decode(&bytes).unwrap();
        assert_eq!(back, fork);
    }

    #[test]
    fn info_fork_tolerates_missing_comment() {
        // A 1.2.x-style INFO fork that ends right after the name.
        let full = InfoFork::new(*b"BINA", *b"dosa", b"app.sit", b"").encode();
        let trimmed = &full[..72 + 7]; // stop after the name, no comment len
        let back = InfoFork::decode(trimmed).unwrap();
        assert_eq!(back.name, b"app.sit");
        assert!(back.comment.is_empty());
    }

    #[test]
    fn info_fork_rejects_truncation() {
        let bytes = InfoFork::new(*b"TEXT", *b"ttxt", b"long-name.txt", b"c").encode();
        // Cut inside the name.
        assert!(matches!(
            InfoFork::decode(&bytes[..75]),
            Err(HotlineError::Truncated { .. })
        ));
        // Cut inside the fixed prefix.
        assert!(matches!(
            InfoFork::decode(&bytes[..40]),
            Err(HotlineError::Truncated { .. })
        ));
    }

    #[test]
    fn resume_data_golden_bytes() {
        // The exact bytes Mobius/HL 1.9 put on the wire for a single
        // DATA-fork resume at offset 0x00012345.
        let rd = FileResumeData::for_data_offset(0x0001_2345);
        let bytes = rd.encode();
        let mut expected = Vec::new();
        expected.extend_from_slice(b"RFLT");
        expected.extend_from_slice(&[0x00, 0x01]);
        expected.extend_from_slice(&[0u8; 34]);
        expected.extend_from_slice(&[0x00, 0x01]); // one fork entry
        expected.extend_from_slice(b"DATA");
        expected.extend_from_slice(&[0x00, 0x01, 0x23, 0x45]);
        expected.extend_from_slice(&[0u8; 8]);
        assert_eq!(bytes, expected);
        let back = FileResumeData::decode(&bytes).unwrap();
        assert_eq!(back, rd);
        assert_eq!(back.data_fork_offset(), Some(0x0001_2345));
    }

    #[test]
    fn resume_data_tolerates_overdeclared_fork_count() {
        // Mobius declares fork count 2 while appending one DATA entry.
        let mut bytes = FileResumeData::for_data_offset(64).encode();
        bytes[41] = 2; // declared count > entries present
        let back = FileResumeData::decode(&bytes).unwrap();
        assert_eq!(back.forks.len(), 1);
        assert_eq!(back.data_fork_offset(), Some(64));
    }

    #[test]
    fn resume_data_rejects_bad_magic_and_short_input() {
        let mut bytes = FileResumeData::for_data_offset(1).encode();
        bytes[0] = b'X';
        assert!(matches!(
            FileResumeData::decode(&bytes),
            Err(HotlineError::BadProtocolId { .. })
        ));
        assert!(matches!(
            FileResumeData::decode(&[0u8; 20]),
            Err(HotlineError::Truncated { .. })
        ));
    }

    #[test]
    fn resume_data_multi_fork_lists_data_offset() {
        let rd = FileResumeData {
            forks: vec![
                ForkOffset {
                    fork_type: FORK_DATA,
                    offset: 100,
                },
                ForkOffset {
                    fork_type: FORK_MACR,
                    offset: 25,
                },
            ],
        };
        let back = FileResumeData::decode(&rd.encode()).unwrap();
        assert_eq!(back, rd);
        assert_eq!(back.data_fork_offset(), Some(100));
    }
}
