//! SAUCE (Standard Architecture for Universal Comment Extensions) records.
//!
//! Art-scene files carry their metadata in a 128-byte trailer appended
//! *after* an EOF (0x1A) byte, so DOS-era viewers using `TYPE` semantics
//! never displayed it. The trailer names the piece, its author and group,
//! the release date, and — crucially for rendering — the data/file type,
//! canvas width/height hints (`TInfo1/2`), the iCE-color flag (`TFlags`
//! bit 0), and the intended font. An optional `COMNT` block of 64-byte
//! comment lines sits immediately before the record.
//!
//! We read tolerantly (any 128-byte tail starting with `SAUCE00`) and
//! write strictly (spec-conformant layout, space padding, CP437 text) so
//! files we touch remain valid for every other scene tool.

use crate::cp437::{cp437_to_string, string_to_cp437_lossy};

/// The record signature: "SAUCE" followed by version "00".
pub const SAUCE_SIGNATURE: &[u8; 7] = b"SAUCE00";
/// Comment-block signature.
pub const COMMENT_SIGNATURE: &[u8; 5] = b"COMNT";
/// Size of the SAUCE record itself.
pub const SAUCE_RECORD_LEN: usize = 128;
/// Size of one comment line inside the `COMNT` block.
pub const COMMENT_LINE_LEN: usize = 64;

/// `TFlags` bit 0: non-blink (iCE color) mode.
pub const TFLAGS_ICE_COLORS: u8 = 0x01;

/// `DataType` values from the SAUCE spec.
pub mod data_type {
    pub const NONE: u8 = 0;
    pub const CHARACTER: u8 = 1;
    pub const BITMAP: u8 = 2;
    pub const VECTOR: u8 = 3;
    pub const AUDIO: u8 = 4;
    pub const BINARY_TEXT: u8 = 5;
    pub const XBIN: u8 = 6;
    pub const ARCHIVE: u8 = 7;
    pub const EXECUTABLE: u8 = 8;
}

/// `FileType` values for the `Character` data type.
pub mod character_file_type {
    pub const ASCII: u8 = 0;
    pub const ANSI: u8 = 1;
    pub const ANSIMATION: u8 = 2;
    pub const RIP_SCRIPT: u8 = 3;
    pub const PCBOARD: u8 = 4;
    pub const AVATAR: u8 = 5;
    pub const HTML: u8 = 6;
    pub const SOURCE: u8 = 7;
    pub const TUNDRA_DRAW: u8 = 8;
}

/// A parsed SAUCE record (plus its comment lines, if any).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SauceRecord {
    /// Title of the piece (≤ 35 chars).
    pub title: String,
    /// Author / artist handle (≤ 20 chars).
    pub author: String,
    /// Group the author belongs to (≤ 20 chars).
    pub group: String,
    /// Creation date as `CCYYMMDD` (8 chars).
    pub date: String,
    /// Original file size, excluding the SAUCE trailer.
    pub filesize: u32,
    /// See [`data_type`].
    pub datatype: u8,
    /// Meaning depends on `datatype`; see [`character_file_type`].
    pub filetype: u8,
    /// Type-dependent info (for Character/ANSI: width in columns).
    pub tinfo1: u16,
    /// Type-dependent info (for Character/ANSI: height in lines).
    pub tinfo2: u16,
    pub tinfo3: u16,
    pub tinfo4: u16,
    /// Comment lines from the `COMNT` block (each ≤ 64 chars).
    pub comments: Vec<String>,
    /// Type flags; bit 0 is iCE colors for character art.
    pub tflags: u8,
    /// Font name hint, e.g. `IBM VGA` (≤ 22 chars).
    pub tinfos: String,
}

impl SauceRecord {
    /// True when the record requests iCE colors (blink = bright background).
    pub fn ice_colors(&self) -> bool {
        self.tflags & TFLAGS_ICE_COLORS != 0
    }

    /// Canvas width hint for character art, if present.
    pub fn width_hint(&self) -> Option<usize> {
        (self.datatype == data_type::CHARACTER && self.tinfo1 > 0).then_some(self.tinfo1 as usize)
    }

    /// Parse the SAUCE trailer from a complete file, if one exists.
    pub fn from_bytes(file: &[u8]) -> Option<SauceRecord> {
        Self::read_trailer(file).map(|(record, _)| record)
    }

    /// The file content with any SAUCE trailer (record, comment block, and
    /// the EOF byte that precedes them) removed.
    pub fn strip(file: &[u8]) -> &[u8] {
        match Self::read_trailer(file) {
            Some((_, trailer_len)) => &file[..file.len() - trailer_len],
            None => file,
        }
    }

    /// Parse the trailer, returning the record and total trailer length
    /// (record + comment block + preceding 0x1A, when present).
    fn read_trailer(file: &[u8]) -> Option<(SauceRecord, usize)> {
        if file.len() < SAUCE_RECORD_LEN {
            return None;
        }
        let rec = &file[file.len() - SAUCE_RECORD_LEN..];
        if &rec[..7] != SAUCE_SIGNATURE {
            return None;
        }

        let field = |range: std::ops::Range<usize>| trim_field(&rec[range]);
        let u16le = |off: usize| u16::from_le_bytes([rec[off], rec[off + 1]]);

        let comment_count = rec[104] as usize;
        let mut record = SauceRecord {
            title: field(7..42),
            author: field(42..62),
            group: field(62..82),
            date: field(82..90),
            filesize: u32::from_le_bytes([rec[90], rec[91], rec[92], rec[93]]),
            datatype: rec[94],
            filetype: rec[95],
            tinfo1: u16le(96),
            tinfo2: u16le(98),
            tinfo3: u16le(100),
            tinfo4: u16le(102),
            comments: Vec::new(),
            tflags: rec[105],
            tinfos: trim_field(&rec[106..128]),
        };

        let mut trailer_len = SAUCE_RECORD_LEN;
        if comment_count > 0 {
            let block_len = COMMENT_SIGNATURE.len() + comment_count * COMMENT_LINE_LEN;
            let end = file.len() - SAUCE_RECORD_LEN;
            if end >= block_len && &file[end - block_len..end - block_len + 5] == COMMENT_SIGNATURE
            {
                let lines = &file[end - block_len + 5..end];
                record.comments = lines.chunks(COMMENT_LINE_LEN).map(trim_field).collect();
                trailer_len += block_len;
            }
            // Missing/corrupt COMNT block: tolerate — keep the record with
            // no comments rather than rejecting the whole trailer.
        }

        // The spec puts a DOS EOF byte before the trailer; claim it so
        // `strip` removes it too.
        let content_end = file.len() - trailer_len;
        if content_end > 0 && file[content_end - 1] == 0x1A {
            trailer_len += 1;
        }
        Some((record, trailer_len))
    }

    /// Encode the full trailer: EOF byte, optional `COMNT` block, and the
    /// 128-byte record. `filesize` is taken from the field as-is; use
    /// [`SauceRecord::append_to`] to have it filled in automatically.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            1 + SAUCE_RECORD_LEN
                + if self.comments.is_empty() {
                    0
                } else {
                    5 + self.comments.len() * COMMENT_LINE_LEN
                },
        );
        out.push(0x1A);

        let comment_count = self.comments.len().min(255);
        if comment_count > 0 {
            out.extend_from_slice(COMMENT_SIGNATURE);
            for line in &self.comments[..comment_count] {
                push_padded(&mut out, line, COMMENT_LINE_LEN, b' ');
            }
        }

        out.extend_from_slice(SAUCE_SIGNATURE);
        push_padded(&mut out, &self.title, 35, b' ');
        push_padded(&mut out, &self.author, 20, b' ');
        push_padded(&mut out, &self.group, 20, b' ');
        push_padded(&mut out, &self.date, 8, b' ');
        out.extend_from_slice(&self.filesize.to_le_bytes());
        out.push(self.datatype);
        out.push(self.filetype);
        out.extend_from_slice(&self.tinfo1.to_le_bytes());
        out.extend_from_slice(&self.tinfo2.to_le_bytes());
        out.extend_from_slice(&self.tinfo3.to_le_bytes());
        out.extend_from_slice(&self.tinfo4.to_le_bytes());
        out.push(comment_count as u8);
        out.push(self.tflags);
        // TInfoS is zero-padded per spec, unlike the space-padded strings.
        push_padded(&mut out, &self.tinfos, 22, 0);
        out
    }

    /// Append the SAUCE trailer to `content`, setting `filesize` to the
    /// content length (saturating at `u32::MAX` for pathological inputs).
    pub fn append_to(&self, content: &mut Vec<u8>) {
        let mut record = self.clone();
        record.filesize = u32::try_from(content.len()).unwrap_or(u32::MAX);
        content.extend_from_slice(&record.encode());
    }
}

/// Decode a fixed-width CP437 field, trimming pad bytes (spaces and NULs).
fn trim_field(raw: &[u8]) -> String {
    let end = raw
        .iter()
        .rposition(|&b| b != b' ' && b != 0)
        .map_or(0, |i| i + 1);
    cp437_to_string(&raw[..end])
}

/// Encode `s` as CP437, truncated/padded to exactly `len` bytes.
fn push_padded(out: &mut Vec<u8>, s: &str, len: usize, pad: u8) {
    let mut bytes = string_to_cp437_lossy(s);
    bytes.truncate(len);
    bytes.resize(len, pad);
    out.extend_from_slice(&bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SauceRecord {
        SauceRecord {
            title: "Warren Welcome".into(),
            author: "kevin".into(),
            group: "RabbitHole".into(),
            date: "20260702".into(),
            filesize: 0, // filled by append_to
            datatype: data_type::CHARACTER,
            filetype: character_file_type::ANSI,
            tinfo1: 80,
            tinfo2: 25,
            tinfo3: 0,
            tinfo4: 0,
            comments: vec!["first line".into(), "second line".into()],
            tflags: TFLAGS_ICE_COLORS,
            tinfos: "IBM VGA".into(),
        }
    }

    #[test]
    fn roundtrip_with_comments() {
        let record = sample();
        let mut file = b"\x1b[1;33mart body".to_vec();
        record.append_to(&mut file);

        let parsed = SauceRecord::from_bytes(&file).expect("record present");
        assert_eq!(parsed.title, "Warren Welcome");
        assert_eq!(parsed.author, "kevin");
        assert_eq!(parsed.group, "RabbitHole");
        assert_eq!(parsed.date, "20260702");
        assert_eq!(parsed.filesize, 15);
        assert_eq!(parsed.datatype, data_type::CHARACTER);
        assert_eq!(parsed.filetype, character_file_type::ANSI);
        assert_eq!((parsed.tinfo1, parsed.tinfo2), (80, 25));
        assert_eq!(parsed.comments, vec!["first line", "second line"]);
        assert!(parsed.ice_colors());
        assert_eq!(parsed.tinfos, "IBM VGA");
        assert_eq!(parsed.width_hint(), Some(80));
    }

    #[test]
    fn roundtrip_without_comments() {
        let mut record = sample();
        record.comments.clear();
        record.tflags = 0;
        let mut file = b"body".to_vec();
        record.append_to(&mut file);

        // Trailer = 0x1A + 128-byte record only.
        assert_eq!(file.len(), 4 + 1 + SAUCE_RECORD_LEN);
        let parsed = SauceRecord::from_bytes(&file).unwrap();
        assert!(parsed.comments.is_empty());
        assert!(!parsed.ice_colors());
        assert_eq!(parsed.filesize, 4);
    }

    #[test]
    fn strip_recovers_original_content() {
        let record = sample();
        let content = b"the actual ansi art".to_vec();
        let mut file = content.clone();
        record.append_to(&mut file);
        assert_eq!(SauceRecord::strip(&file), content.as_slice());
        // Files without SAUCE are returned untouched.
        assert_eq!(SauceRecord::strip(&content), content.as_slice());
    }

    #[test]
    fn encode_layout_matches_spec() {
        let record = sample();
        let bytes = record.encode();
        let comment_block = 5 + 2 * COMMENT_LINE_LEN;
        assert_eq!(bytes.len(), 1 + comment_block + SAUCE_RECORD_LEN);
        assert_eq!(bytes[0], 0x1A);
        assert_eq!(&bytes[1..6], COMMENT_SIGNATURE);
        let rec = &bytes[1 + comment_block..];
        assert_eq!(&rec[..7], SAUCE_SIGNATURE);
        // Title is space-padded to 35 bytes.
        assert_eq!(&rec[7..21], b"Warren Welcome");
        assert!(rec[21..42].iter().all(|&b| b == b' '));
        // Comment count and flags.
        assert_eq!(rec[104], 2);
        assert_eq!(rec[105], TFLAGS_ICE_COLORS);
        // TInfoS is zero-padded.
        assert_eq!(&rec[106..113], b"IBM VGA");
        assert!(rec[113..128].iter().all(|&b| b == 0));
    }

    #[test]
    fn no_record_in_short_or_plain_files() {
        assert_eq!(SauceRecord::from_bytes(b""), None);
        assert_eq!(SauceRecord::from_bytes(b"short"), None);
        let plain = vec![b'x'; 500];
        assert_eq!(SauceRecord::from_bytes(&plain), None);
    }

    #[test]
    fn wrong_signature_is_rejected() {
        let mut file = b"body".to_vec();
        sample().append_to(&mut file);
        let n = file.len();
        file[n - SAUCE_RECORD_LEN] = b'X'; // corrupt "SAUCE"
        assert_eq!(SauceRecord::from_bytes(&file), None);
    }

    #[test]
    fn missing_comment_block_is_tolerated() {
        let mut record = sample();
        record.comments.clear();
        let mut file = b"body".to_vec();
        record.append_to(&mut file);
        // Claim two comments that don't exist.
        let n = file.len();
        file[n - 24] = 2;
        let parsed = SauceRecord::from_bytes(&file).expect("record still readable");
        assert!(parsed.comments.is_empty());
        assert_eq!(parsed.title, "Warren Welcome");
    }

    #[test]
    fn overlong_fields_are_truncated_on_write() {
        let mut record = sample();
        record.title = "T".repeat(100);
        record.author = "A".repeat(100);
        record.comments = vec!["C".repeat(200)];
        let mut file = Vec::new();
        record.append_to(&mut file);
        let parsed = SauceRecord::from_bytes(&file).unwrap();
        assert_eq!(parsed.title, "T".repeat(35));
        assert_eq!(parsed.author, "A".repeat(20));
        assert_eq!(parsed.comments, vec!["C".repeat(COMMENT_LINE_LEN)]);
    }

    #[test]
    fn cp437_text_in_fields_roundtrips() {
        let mut record = sample();
        record.title = "café ░▒▓".into();
        let mut file = b"x".to_vec();
        record.append_to(&mut file);
        let parsed = SauceRecord::from_bytes(&file).unwrap();
        assert_eq!(parsed.title, "café ░▒▓");
    }

    #[test]
    fn null_padded_fields_are_trimmed_on_read() {
        // Some writers pad with NULs instead of spaces; accept both.
        let mut file = b"body".to_vec();
        sample().append_to(&mut file);
        let n = file.len();
        // Rewrite the author field as "bob" + NUL padding.
        let author_off = n - SAUCE_RECORD_LEN + 42;
        file[author_off..author_off + 20].fill(0);
        file[author_off..author_off + 3].copy_from_slice(b"bob");
        let parsed = SauceRecord::from_bytes(&file).unwrap();
        assert_eq!(parsed.author, "bob");
    }

    #[test]
    fn eof_byte_absence_is_tolerated() {
        // A trailer written without the 0x1A still parses; strip keeps
        // the content intact.
        let record = sample();
        let trailer = record.encode();
        let mut file = b"content".to_vec();
        file.extend_from_slice(&trailer[1..]); // skip the EOF byte
        let parsed = SauceRecord::from_bytes(&file).expect("record present");
        assert_eq!(parsed.title, "Warren Welcome");
        assert_eq!(SauceRecord::strip(&file), b"content");
    }
}
