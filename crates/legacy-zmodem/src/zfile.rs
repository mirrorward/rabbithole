//! The ZFILE information block: file name plus metadata.
//!
//! After a `ZFILE` header, the sender transmits one data subpacket
//! (terminated `ZCRCW`) whose payload describes the file:
//!
//! ```text
//! [ filename bytes ][ NUL ]
//! [ "length mtime mode serial" as ASCII ][ NUL ]
//!         length   decimal file size in bytes
//!         mtime    octal seconds since the Unix epoch
//!         mode     octal Unix file mode (e.g. 100644)
//!         serial   octal sender serial number (usually 0)
//! ```
//!
//! Every metadata field is optional, but they are positional: `mtime`
//! cannot appear without `length`, and so on. The spec asks senders to use
//! lowercase names without paths; this codec does not enforce that (the
//! host decides policy) but it does refuse NULs and empty names. The
//! trailing NUL after the info string is emitted for compatibility with
//! classic receivers and accepted-but-optional when parsing.

use thiserror::Error;

/// Errors from ZFILE info coding. Never panics on any input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FileInfoError {
    /// The payload had no NUL terminator after the file name.
    #[error("ZFILE payload missing NUL after file name")]
    MissingNameTerminator,
    /// The file name was empty.
    #[error("ZFILE payload has an empty file name")]
    EmptyName,
    /// The file name was not valid UTF-8.
    #[error("ZFILE file name is not valid UTF-8")]
    NameNotUtf8,
    /// A metadata field was not a valid number in its expected base.
    #[error("invalid {field} field in ZFILE info: {value:?}")]
    BadField {
        /// Which positional field was malformed.
        field: &'static str,
        /// The offending text.
        value: String,
    },
    /// A name to encode contained a NUL byte.
    #[error("file name contains a NUL byte")]
    NameContainsNul,
}

/// Parsed ZFILE information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// The file name (no NULs; the spec suggests lowercase, no path).
    pub name: String,
    /// File length in bytes (decimal on the wire).
    pub length: Option<u64>,
    /// Modification time, seconds since the Unix epoch (octal on the wire).
    pub mtime: Option<u64>,
    /// Unix file mode (octal on the wire, e.g. `0o100644`).
    pub mode: Option<u32>,
    /// Sender serial number (octal on the wire; almost always 0).
    pub serial: Option<u32>,
}

impl FileInfo {
    /// A minimal info block: just a name.
    pub fn new(name: impl Into<String>) -> Self {
        FileInfo {
            name: name.into(),
            length: None,
            mtime: None,
            mode: None,
            serial: None,
        }
    }

    /// Emit the subpacket payload (`name NUL info NUL`).
    ///
    /// Metadata fields are positional: any field that is `None` but
    /// followed by a `Some` field is emitted as `0`.
    pub fn encode(&self) -> Result<Vec<u8>, FileInfoError> {
        if self.name.is_empty() {
            return Err(FileInfoError::EmptyName);
        }
        if self.name.as_bytes().contains(&0) {
            return Err(FileInfoError::NameContainsNul);
        }
        let mut out = Vec::with_capacity(self.name.len() + 32);
        out.extend_from_slice(self.name.as_bytes());
        out.push(0);
        let fields: [Option<String>; 4] = [
            self.length.map(|v| format!("{v}")),
            self.mtime.map(|v| format!("{v:o}")),
            self.mode.map(|v| format!("{v:o}")),
            self.serial.map(|v| format!("{v:o}")),
        ];
        let last_present = fields.iter().rposition(Option::is_some);
        if let Some(last) = last_present {
            let mut info = String::new();
            for (i, field) in fields.iter().take(last + 1).enumerate() {
                if i > 0 {
                    info.push(' ');
                }
                match field {
                    Some(text) => info.push_str(text),
                    None => info.push('0'),
                }
            }
            out.extend_from_slice(info.as_bytes());
        }
        out.push(0);
        Ok(out)
    }

    /// Parse a ZFILE subpacket payload. Never panics.
    pub fn decode(payload: &[u8]) -> Result<Self, FileInfoError> {
        let nul = payload
            .iter()
            .position(|&b| b == 0)
            .ok_or(FileInfoError::MissingNameTerminator)?;
        if nul == 0 {
            return Err(FileInfoError::EmptyName);
        }
        let name = std::str::from_utf8(&payload[..nul])
            .map_err(|_| FileInfoError::NameNotUtf8)?
            .to_owned();
        // Info runs to the next NUL (or end of payload if the sender
        // omitted the trailing NUL).
        let rest = &payload[nul + 1..];
        let info_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let info = String::from_utf8_lossy(&rest[..info_end]);
        let mut fields = info.split_ascii_whitespace();
        let length = parse_field(fields.next(), "length", 10)?;
        let mtime = parse_field(fields.next(), "mtime", 8)?;
        let mode = parse_field(fields.next(), "mode", 8)?.map(|v| v as u32);
        let serial = parse_field(fields.next(), "serial", 8)?.map(|v| v as u32);
        // Further fields (files-remaining, bytes-remaining) are legal in
        // extended implementations; they are ignored here.
        Ok(FileInfo {
            name,
            length,
            mtime,
            mode,
            serial,
        })
    }
}

fn parse_field(
    text: Option<&str>,
    field: &'static str,
    radix: u32,
) -> Result<Option<u64>, FileInfoError> {
    match text {
        None => Ok(None),
        Some(t) => u64::from_str_radix(t, radix)
            .map(Some)
            .map_err(|_| FileInfoError::BadField {
                field,
                value: t.to_owned(),
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_full_info_block() {
        let info = FileInfo {
            name: "rabbit.txt".into(),
            length: Some(1024),
            mtime: Some(0o17570520744), // some epoch seconds, octal on the wire
            mode: Some(0o100644),
            serial: Some(0),
        };
        let payload = info.encode().unwrap();
        assert_eq!(
            payload,
            b"rabbit.txt\x001024 17570520744 100644 0\0".to_vec()
        );
    }

    #[test]
    fn encodes_name_only() {
        let payload = FileInfo::new("hole.bin").encode().unwrap();
        assert_eq!(payload, b"hole.bin\0\0".to_vec());
    }

    #[test]
    fn positional_gaps_are_zero_filled() {
        let mut info = FileInfo::new("gap.dat");
        info.mode = Some(0o100600);
        let payload = info.encode().unwrap();
        // length and mtime backfilled with 0 so mode stays in position 3.
        assert_eq!(payload, b"gap.dat\x000 0 100600\0".to_vec());
    }

    #[test]
    fn round_trips() {
        let cases = [
            FileInfo::new("plain"),
            FileInfo {
                length: Some(0),
                ..FileInfo::new("zero.len")
            },
            FileInfo {
                name: "full house.tar.gz".into(),
                length: Some(u64::from(u32::MAX) + 42),
                mtime: Some(1_766_000_000),
                mode: Some(0o100755),
                serial: Some(7),
            },
        ];
        for info in cases {
            let decoded = FileInfo::decode(&info.encode().unwrap()).unwrap();
            assert_eq!(decoded, info, "case {}", info.name);
        }
    }

    #[test]
    fn parses_classic_sz_payload() {
        // As emitted by lsz: octal mtime and mode, decimal length.
        let payload = b"readme.txt\x004096 13337515345 100644 0\0";
        let info = FileInfo::decode(payload).unwrap();
        assert_eq!(info.name, "readme.txt");
        assert_eq!(info.length, Some(4096));
        assert_eq!(info.mtime, Some(0o13337515345));
        assert_eq!(info.mode, Some(0o100644));
        assert_eq!(info.serial, Some(0));
    }

    #[test]
    fn trailing_nul_is_optional_and_extra_fields_ignored() {
        let info = FileInfo::decode(b"a\x00123 0 0 0 3 99999").unwrap();
        assert_eq!(info.name, "a");
        assert_eq!(info.length, Some(123));
        let bare = FileInfo::decode(b"b\0").unwrap();
        assert_eq!(bare.name, "b");
        assert_eq!(bare.length, None);
    }

    #[test]
    fn rejects_malformed_payloads() {
        assert_eq!(
            FileInfo::decode(b"no-nul"),
            Err(FileInfoError::MissingNameTerminator)
        );
        assert_eq!(FileInfo::decode(b"\0info"), Err(FileInfoError::EmptyName));
        assert_eq!(
            FileInfo::decode(&[0xFF, 0xFE, 0, 0]),
            Err(FileInfoError::NameNotUtf8)
        );
        assert!(matches!(
            FileInfo::decode(b"f\0notanumber"),
            Err(FileInfoError::BadField {
                field: "length",
                ..
            })
        ));
        assert!(matches!(
            FileInfo::decode(b"f\x00100 99"), // 9 is not an octal digit (mtime)
            Err(FileInfoError::BadField { field: "mtime", .. })
        ));
    }

    #[test]
    fn rejects_bad_names_on_encode() {
        assert_eq!(FileInfo::new("").encode(), Err(FileInfoError::EmptyName));
        assert_eq!(
            FileInfo::new("a\0b").encode(),
            Err(FileInfoError::NameContainsNul)
        );
    }

    #[test]
    fn decode_of_arbitrary_bytes_never_panics() {
        let mut state: u64 = 0xDEAD_BEEF_CAFE_F00D;
        for len in 0..200usize {
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                buf.push((state >> 33) as u8);
            }
            let _ = FileInfo::decode(&buf);
        }
    }
}
