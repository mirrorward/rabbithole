//! Per-conference `.NDX` index: 5-byte records with an MBF float pointer.
//!
//! Each conference gets a `nnn.NDX` file (nnn = zero-padded conference number)
//! that indexes its messages inside `MESSAGES.DAT`. Every record is **5 bytes**:
//!
//! ```text
//!  bytes 0..4  message pointer, 4-byte Microsoft Binary Format float (see mbf)
//!  byte  4     conference number (low byte)
//! ```
//!
//! In the classic format the 4-byte pointer is the **1-based block number** of
//! the message header within `MESSAGES.DAT`; this codec treats it as an opaque
//! `u32` (the packer supplies the value). Per the format research we *implement*
//! the MBF encoder but never trust a decoded index — a reader rescans
//! `MESSAGES.DAT` because historical doors wrote buggy `.NDX` files.

use crate::error::QwkError;
use crate::mbf;

/// Bytes per `.NDX` record.
pub const RECORD: usize = 5;

/// One `.NDX` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdxRecord {
    /// Message pointer, MBF-encoded on disk. Classically the 1-based block
    /// number of the message header in `MESSAGES.DAT`.
    pub number: u32,
    /// Conference number (low byte only, per the on-disk format).
    pub conference: u8,
}

impl NdxRecord {
    /// Construct a record.
    pub fn new(number: u32, conference: u8) -> Self {
        Self { number, conference }
    }
}

/// Encode records to a `.NDX` byte stream (`RECORD` bytes each).
pub fn encode(records: &[NdxRecord]) -> Vec<u8> {
    let mut out = Vec::with_capacity(records.len() * RECORD);
    for r in records {
        out.extend_from_slice(&mbf::encode(r.number));
        out.push(r.conference);
    }
    out
}

/// Decode a `.NDX` byte stream.
///
/// Returns [`QwkError::PartialRecord`] if the length is not a whole multiple of
/// [`RECORD`]. Never panics.
pub fn decode(bytes: &[u8]) -> Result<Vec<NdxRecord>, QwkError> {
    let remainder = bytes.len() % RECORD;
    if remainder != 0 {
        return Err(QwkError::PartialRecord {
            record_len: RECORD,
            remainder,
        });
    }
    let records = bytes
        .chunks_exact(RECORD)
        .map(|c| NdxRecord {
            number: mbf::decode([c[0], c[1], c[2], c[3]]),
            conference: c[4],
        })
        .collect();
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let records = vec![
            NdxRecord::new(2, 0),
            NdxRecord::new(5, 0),
            NdxRecord::new(17, 3),
            NdxRecord::new(1000, 12),
        ];
        let bytes = encode(&records);
        assert_eq!(bytes.len(), records.len() * RECORD);
        assert_eq!(decode(&bytes).unwrap(), records);
    }

    #[test]
    fn first_four_bytes_are_mbf_pointer() {
        let bytes = encode(&[NdxRecord::new(1, 7)]);
        assert_eq!(&bytes[0..4], &mbf::encode(1));
        assert_eq!(bytes[4], 7);
    }

    #[test]
    fn partial_record_errors() {
        assert!(matches!(
            decode(&[0u8; 4]),
            Err(QwkError::PartialRecord {
                record_len: 5,
                remainder: 4
            })
        ));
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(decode(&[]).unwrap(), vec![]);
    }

    #[test]
    fn decode_arbitrary_lengths_never_panics() {
        for len in 0..40usize {
            let bytes = vec![0xABu8; len];
            let _ = decode(&bytes);
        }
    }
}
