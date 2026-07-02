//! Fragment reassembly for oversized transaction bodies.
//!
//! When a transaction body is larger than a single frame carries, Hotline
//! splits it: the first frame's header sets `total_size` to the full body
//! length but `data_size` to only the bytes in that frame. Continuation frames
//! repeat the header (same `id`, same `total_size`) and carry the next chunk in
//! their own `data_size`. The receiver concatenates the chunks, keyed by `id`,
//! until it has `total_size` bytes.
//!
//! ```text
//! frame 1: [hdr id=42 total=300 data=100][ 100 body bytes ]
//! frame 2: [hdr id=42 total=300 data=100][ next 100 bytes ]
//! frame 3: [hdr id=42 total=300 data=100][ final 100 bytes ] -> body complete
//! ```
//!
//! [`Reassembler`] accumulates chunks and yields a fully-decoded
//! [`Transaction`] the moment a body reaches `total_size`. A body that fits in
//! one frame (`data_size == total_size`) is returned immediately without being
//! buffered.

use std::collections::HashMap;

use crate::error::HotlineError;
use crate::transaction::{decode_body, Transaction, TransactionHeader};

/// Ceiling on a reassembled transaction body (16 MiB).
///
/// A `total_size` above this is rejected before any buffer is allocated, so a
/// corrupt or hostile header cannot trigger a multi-gigabyte allocation. Real
/// Hotline control transactions are kilobytes; bulk data rides the separate
/// HTXF channel, not the transaction stream.
pub const MAX_TRANSACTION_BODY: usize = 16 * 1024 * 1024;

/// A partially-received transaction body, buffered until complete.
#[derive(Debug)]
struct Partial {
    header: TransactionHeader,
    total_size: u32,
    body: Vec<u8>,
}

/// Accumulates transaction fragments and emits complete transactions.
///
/// Fragments are correlated by transaction `id`. The reassembler holds only
/// in-flight (incomplete) transactions; completed ones are removed as they are
/// returned.
#[derive(Debug, Default)]
pub struct Reassembler {
    partial: HashMap<u32, Partial>,
}

impl Reassembler {
    /// Create an empty reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of transactions currently mid-reassembly.
    pub fn pending(&self) -> usize {
        self.partial.len()
    }

    /// Feed one frame's header and its body chunk.
    ///
    /// `chunk` is the `data_size` body bytes carried by this frame (callers
    /// slice it off the wire after the 20-byte header). Returns:
    /// - `Ok(Some(txn))` when this frame completes a transaction body,
    /// - `Ok(None)` when more fragments are still needed,
    /// - `Err(_)` on inconsistent fragments or a malformed completed body.
    pub fn push(
        &mut self,
        header: &TransactionHeader,
        chunk: &[u8],
    ) -> Result<Option<Transaction>, HotlineError> {
        let id = header.id;

        if header.total_size as usize > MAX_TRANSACTION_BODY {
            self.partial.remove(&id);
            return Err(HotlineError::TooLarge {
                size: header.total_size as usize,
                max: MAX_TRANSACTION_BODY,
            });
        }

        // Fast path: a single self-contained frame that isn't already part of
        // an in-flight reassembly.
        if !self.partial.contains_key(&id) && chunk.len() as u64 >= u64::from(header.total_size) {
            let body = &chunk[..header.total_size as usize];
            let fields = decode_body(header, body)?;
            return Ok(Some(Transaction {
                header: *header,
                fields,
            }));
        }

        let entry = self.partial.entry(id).or_insert_with(|| Partial {
            header: *header,
            total_size: header.total_size,
            body: Vec::with_capacity(header.total_size as usize),
        });

        if entry.total_size != header.total_size {
            let first = entry.total_size;
            self.partial.remove(&id);
            return Err(HotlineError::FragmentMismatch {
                id,
                first,
                next: header.total_size,
            });
        }

        entry.body.extend_from_slice(chunk);

        if entry.body.len() as u64 > u64::from(entry.total_size) {
            let have = entry.body.len();
            let total = entry.total_size;
            self.partial.remove(&id);
            return Err(HotlineError::FragmentOverflow { id, have, total });
        }

        if entry.body.len() as u64 == u64::from(entry.total_size) {
            let done = self.partial.remove(&id).expect("entry present");
            let fields = decode_body(&done.header, &done.body)?;
            return Ok(Some(Transaction {
                header: done.header,
                fields,
            }));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{field, transaction};
    use crate::field::{encode_params, Field};

    fn frame_headers(
        type_: u16,
        id: u32,
        total: u32,
        chunk_sizes: &[u32],
    ) -> Vec<TransactionHeader> {
        chunk_sizes
            .iter()
            .map(|&data_size| TransactionHeader {
                flags: 0,
                is_reply: 0,
                type_,
                id,
                error: 0,
                total_size: total,
                data_size,
            })
            .collect()
    }

    #[test]
    fn single_frame_completes_immediately() {
        let fields = vec![Field::text(field::CHAT_TEXT, "hi")];
        let body = encode_params(&fields);
        let header = TransactionHeader {
            flags: 0,
            is_reply: 0,
            type_: transaction::CHAT_SEND,
            id: 1,
            error: 0,
            total_size: body.len() as u32,
            data_size: body.len() as u32,
        };
        let mut r = Reassembler::new();
        let txn = r.push(&header, &body).unwrap().unwrap();
        assert_eq!(txn.fields, fields);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn multi_fragment_reassembly() {
        // Build a real body, then split it across three frames.
        let fields = vec![
            Field::text(field::USER_NAME, "somebody with a long enough name"),
            Field::int(field::USER_ID, 4242),
            Field::text(field::DATA, "the quick brown fox jumps over the lazy dog"),
        ];
        let body = encode_params(&fields);
        let total = body.len() as u32;

        // Three chunks; last one takes the remainder.
        let c1 = 20usize;
        let c2 = 30usize;
        let c3 = body.len() - c1 - c2;
        let headers = frame_headers(
            transaction::NOTIFY_CHANGE_USER,
            42,
            total,
            &[c1 as u32, c2 as u32, c3 as u32],
        );

        let mut r = Reassembler::new();
        assert!(r.push(&headers[0], &body[..c1]).unwrap().is_none());
        assert_eq!(r.pending(), 1);
        assert!(r.push(&headers[1], &body[c1..c1 + c2]).unwrap().is_none());
        let txn = r.push(&headers[2], &body[c1 + c2..]).unwrap().unwrap();
        assert_eq!(txn.fields, fields);
        assert_eq!(txn.header.id, 42);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn interleaved_transactions() {
        let body_a = encode_params(&[Field::text(field::USER_NAME, "alice")]);
        let body_b = encode_params(&[Field::text(field::USER_NAME, "bob")]);
        let ha = frame_headers(
            transaction::NOTIFY_CHANGE_USER,
            1,
            body_a.len() as u32,
            &[3, 0],
        );
        let hb = frame_headers(
            transaction::NOTIFY_CHANGE_USER,
            2,
            body_b.len() as u32,
            &[3, 0],
        );

        let mut r = Reassembler::new();
        assert!(r.push(&ha[0], &body_a[..3]).unwrap().is_none());
        assert!(r.push(&hb[0], &body_b[..3]).unwrap().is_none());
        assert_eq!(r.pending(), 2);
        let a = r.push(&ha[1], &body_a[3..]).unwrap().unwrap();
        let b = r.push(&hb[1], &body_b[3..]).unwrap().unwrap();
        assert_eq!(a.fields[0].as_text_lossy(), "alice");
        assert_eq!(b.fields[0].as_text_lossy(), "bob");
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn mismatched_total_size_errors() {
        let headers = frame_headers(transaction::CHAT_SEND, 9, 100, &[10]);
        let mut bad = headers[0];
        bad.total_size = 200;
        let mut r = Reassembler::new();
        r.push(&headers[0], &[0u8; 10]).unwrap();
        assert!(matches!(
            r.push(&bad, &[0u8; 10]),
            Err(HotlineError::FragmentMismatch { id: 9, .. })
        ));
        assert_eq!(r.pending(), 0, "mismatch drops the in-flight entry");
    }

    #[test]
    fn overflow_errors() {
        let headers = frame_headers(transaction::CHAT_SEND, 3, 10, &[6, 6]);
        let mut r = Reassembler::new();
        r.push(&headers[0], &[0u8; 6]).unwrap();
        assert!(matches!(
            r.push(&headers[1], &[0u8; 6]),
            Err(HotlineError::FragmentOverflow { id: 3, .. })
        ));
    }
}
