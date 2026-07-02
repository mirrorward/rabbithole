//! Listener-side de-interleaving of ICY in-band metadata — the inverse of
//! [`IcyMetaInterleaver`](crate::IcyMetaInterleaver).
//!
//! A stream negotiated with `icy-metaint:<n>` arrives on the wire as:
//!
//! ```text
//! +----------------+-----+------------------+----------------+-----+---
//! | n audio bytes  | len | len * 16 payload | n audio bytes  | len | …
//! +----------------+-----+------------------+----------------+-----+---
//!                    1 B   0 bytes when len == 0 ("no change")
//! ```
//!
//! [`MetaintReader`] consumes that format incrementally — feed it received
//! bytes in chunks of any size, across any boundary — and yields the clean
//! audio plus any decoded [`StreamTitle`] updates. It is total: the length
//! byte alone drives the framing, so a garbage metadata payload never panics
//! and never desynchronises the audio — it is counted
//! ([`MetaintReader::malformed_blocks`]), skipped, and reading continues.
//! Both empty-block spellings are treated as "no change": the canonical
//! single `0x00` length byte and the sloppier non-zero-length block whose
//! payload is all NULs.
//!
//! ```
//! use rabbithole_legacy_icecast::{IcyMetaInterleaver, MetaintReader};
//!
//! // Server side splices…
//! let mut weaver = IcyMetaInterleaver::new(16);
//! weaver.set_title("Song A");
//! let wire = weaver.push(&[0xAA; 32]);
//!
//! // …player side strips.
//! let mut reader = MetaintReader::new(16);
//! let out = reader.push(&wire);
//! assert_eq!(out.audio, vec![0xAA; 32]);
//! assert_eq!(out.updates[0].title, "Song A");
//! ```

use crate::metaint::{parse_stream_title, StreamTitle};

/// What one [`MetaintReader::push`] recovered from the wire bytes: the audio
/// with metadata blocks stripped out, plus zero or more decoded title updates
/// (in stream order — a large chunk can cross several metaint boundaries).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeinterleavedChunk {
    /// The audio bytes, in order, with all metadata blocks removed.
    pub audio: Vec<u8>,
    /// Title updates decoded from non-empty metadata blocks, oldest first.
    /// Empty ("no change") and malformed blocks contribute nothing here.
    pub updates: Vec<StreamTitle>,
}

/// Where the reader is within the wire framing.
#[derive(Clone, Debug)]
enum State {
    /// Expecting `remaining` more audio bytes before the next length byte.
    Audio { remaining: usize },
    /// Expecting the one-byte metadata length (16-byte units).
    Length,
    /// Collecting a `need`-byte metadata payload (`buf.len() < need`).
    Payload { need: usize, buf: Vec<u8> },
}

/// Incremental decoder for a metaint'd ICY stream (the client/player side).
///
/// Construct with the `icy-metaint` value the server advertised, then feed
/// the raw body bytes through [`push`](MetaintReader::push) in chunks of any
/// size; state (mid-audio, mid-block) carries across calls. See the
/// [module docs](self) for the wire shape and totality guarantees.
#[derive(Clone, Debug)]
pub struct MetaintReader {
    metaint: usize,
    state: State,
    malformed_blocks: u64,
}

impl MetaintReader {
    /// Creates a reader for a stream with the given metaint spacing.
    ///
    /// `metaint` is clamped to at least 1, mirroring
    /// [`IcyMetaInterleaver::new`](crate::IcyMetaInterleaver::new), so the two
    /// sides always agree on where boundaries fall.
    pub fn new(metaint: usize) -> Self {
        let metaint = metaint.max(1);
        Self {
            metaint,
            state: State::Audio { remaining: metaint },
            malformed_blocks: 0,
        }
    }

    /// The metaint spacing this reader de-interleaves at.
    pub fn metaint(&self) -> usize {
        self.metaint
    }

    /// How many complete, non-empty metadata blocks failed to parse as
    /// `StreamTitle` payloads and were skipped. Purely diagnostic — the audio
    /// framing is unaffected.
    pub fn malformed_blocks(&self) -> u64 {
        self.malformed_blocks
    }

    /// Consumes the next `bytes` of the wire stream and returns the audio and
    /// title updates they completed. Never fails, never panics; an empty (or
    /// entirely mid-block) chunk simply yields an empty result.
    pub fn push(&mut self, bytes: &[u8]) -> DeinterleavedChunk {
        let mut out = DeinterleavedChunk {
            audio: Vec::with_capacity(bytes.len()),
            updates: Vec::new(),
        };
        let mut pos = 0;
        while pos < bytes.len() {
            match self.state {
                State::Audio { remaining } => {
                    let take = remaining.min(bytes.len() - pos);
                    out.audio.extend_from_slice(&bytes[pos..pos + take]);
                    pos += take;
                    self.state = match remaining - take {
                        0 => State::Length,
                        left => State::Audio { remaining: left },
                    };
                }
                State::Length => {
                    let units = bytes[pos] as usize;
                    pos += 1;
                    self.state = if units == 0 {
                        State::Audio {
                            remaining: self.metaint,
                        }
                    } else {
                        State::Payload {
                            need: units * 16,
                            buf: Vec::with_capacity(units * 16),
                        }
                    };
                }
                State::Payload { need, ref mut buf } => {
                    let take = (need - buf.len()).min(bytes.len() - pos);
                    buf.extend_from_slice(&bytes[pos..pos + take]);
                    pos += take;
                    if buf.len() == need {
                        let payload = std::mem::take(buf);
                        // All-NUL payloads are the sloppy "no change" spelling,
                        // not an update and not an error.
                        if payload.iter().any(|&b| b != 0) {
                            match parse_stream_title(&payload) {
                                Ok(update) => out.updates.push(update),
                                Err(_) => self.malformed_blocks += 1,
                            }
                        }
                        self.state = State::Audio {
                            remaining: self.metaint,
                        };
                    }
                }
            }
        }
        out
    }
}

/// One-shot convenience over [`MetaintReader`]: de-interleaves a complete (or
/// truncated — any unfinished trailing block is simply dropped) wire capture
/// in a single call.
pub fn parse_metaint_stream(metaint: usize, stream: &[u8]) -> DeinterleavedChunk {
    MetaintReader::new(metaint).push(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metaint::{encode_stream_title, IcyMetaInterleaver};

    /// Round-trip through the real serving-side splicer, replayed to the
    /// reader at every chunk size from 1 to 17, with two mid-stream title
    /// changes. Audio must come back byte-identical and the updates in order.
    #[test]
    fn round_trips_splicer_output_across_chunk_sizes() {
        let metaint = 32;
        let audio: Vec<u8> = (0..400u32).map(|i| (i % 251) as u8).collect();

        let mut weaver = IcyMetaInterleaver::new(metaint);
        let mut wire = Vec::new();
        weaver.set_title("First");
        wire.extend(weaver.push(&audio[..150]));
        weaver.set_title("Second");
        wire.extend(weaver.push(&audio[150..300]));
        weaver.set_title("Third");
        wire.extend(weaver.push(&audio[300..]));

        for chunk_size in 1..=17 {
            let mut reader = MetaintReader::new(metaint);
            let mut got = DeinterleavedChunk::default();
            for chunk in wire.chunks(chunk_size) {
                let out = reader.push(chunk);
                got.audio.extend(out.audio);
                got.updates.extend(out.updates);
            }
            assert_eq!(got.audio, audio, "chunk_size={chunk_size}");
            let titles: Vec<&str> = got.updates.iter().map(|u| u.title.as_str()).collect();
            assert_eq!(
                titles,
                ["First", "Second", "Third"],
                "chunk_size={chunk_size}"
            );
            assert!(got.updates.iter().all(|u| u.url.is_none()));
            assert_eq!(reader.malformed_blocks(), 0, "chunk_size={chunk_size}");
        }
    }

    #[test]
    fn one_shot_matches_incremental() {
        let mut weaver = IcyMetaInterleaver::new(8);
        weaver.set_title("Tune");
        let wire = weaver.push(&[7u8; 40]);

        let one_shot = parse_metaint_stream(8, &wire);
        let mut reader = MetaintReader::new(8);
        let mut incremental = DeinterleavedChunk::default();
        for chunk in wire.chunks(3) {
            let out = reader.push(chunk);
            incremental.audio.extend(out.audio);
            incremental.updates.extend(out.updates);
        }
        assert_eq!(one_shot, incremental);
    }

    #[test]
    fn zero_length_blocks_yield_no_updates() {
        // metaint 4: [audio 4][00][audio 4][00][audio 1…]
        let wire = [1, 2, 3, 4, 0, 5, 6, 7, 8, 0, 9];
        let out = parse_metaint_stream(4, &wire);
        assert_eq!(out.audio, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert!(out.updates.is_empty());
    }

    #[test]
    fn all_nul_padded_block_is_no_change_not_malformed() {
        let mut wire = vec![1, 2, 3, 4];
        wire.push(2); // claims 32 payload bytes…
        wire.extend([0u8; 32]); // …all NUL
        wire.extend([5, 6]);
        let mut reader = MetaintReader::new(4);
        let out = reader.push(&wire);
        assert_eq!(out.audio, vec![1, 2, 3, 4, 5, 6]);
        assert!(out.updates.is_empty());
        assert_eq!(reader.malformed_blocks(), 0);
    }

    #[test]
    fn malformed_block_is_skipped_without_desync_or_panic() {
        let mut wire = vec![1, 2, 3, 4];
        wire.push(1);
        wire.extend(*b"total garbage!!!"); // 16 bytes, no StreamTitle
        wire.extend([5, 6, 7, 8]);
        wire.push(1);
        wire.extend([0xFF; 16]); // non-UTF-8 garbage, still no StreamTitle
        wire.extend([9]);

        let mut reader = MetaintReader::new(4);
        let out = reader.push(&wire);
        assert_eq!(out.audio, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert!(out.updates.is_empty());
        assert_eq!(reader.malformed_blocks(), 2);

        // The reader is still healthy: a good block afterwards decodes fine.
        let mut tail = vec![10, 11, 12]; // completes the metaint of 4
        tail.extend(encode_stream_title("Recovered", None));
        let out = reader.push(&tail);
        assert_eq!(out.audio, vec![10, 11, 12]);
        assert_eq!(out.updates[0].title, "Recovered");
    }

    #[test]
    fn decodes_stream_url_from_handcrafted_wire() {
        let mut wire = vec![1, 2, 3, 4];
        wire.extend(encode_stream_title("Song", Some("http://radio.example/")));
        wire.extend([9, 9]);
        let out = parse_metaint_stream(4, &wire);
        assert_eq!(out.audio, vec![1, 2, 3, 4, 9, 9]);
        assert_eq!(
            out.updates,
            vec![StreamTitle {
                title: "Song".to_string(),
                url: Some("http://radio.example/".to_string()),
            }]
        );
    }

    #[test]
    fn empty_title_update_is_distinct_from_empty_block() {
        let mut weaver = IcyMetaInterleaver::new(4);
        weaver.set_title("");
        let wire = weaver.push(&[0xAB; 4]);
        let out = parse_metaint_stream(4, &wire);
        assert_eq!(out.updates, vec![StreamTitle::default()]);
    }

    #[test]
    fn maximum_size_block_fed_byte_at_a_time() {
        let block = encode_stream_title(&"x".repeat(5_000), None);
        assert_eq!(block[0], 255); // truncated to the 255-unit ceiling
        let mut wire = vec![0xCD; 16];
        wire.extend(&block);
        wire.extend([0xEF; 3]);

        let mut reader = MetaintReader::new(16);
        let mut got = DeinterleavedChunk::default();
        for &b in &wire {
            let out = reader.push(&[b]);
            got.audio.extend(out.audio);
            got.updates.extend(out.updates);
        }
        let mut expected_audio = vec![0xCD; 16];
        expected_audio.extend([0xEF; 3]);
        assert_eq!(got.audio, expected_audio);
        assert_eq!(got.updates.len(), 1);
        assert!(got.updates[0].title.starts_with("xxx"));
        assert_eq!(reader.malformed_blocks(), 0);
    }

    #[test]
    fn truncated_stream_mid_block_yields_audio_so_far() {
        let mut wire = vec![1, 2, 3, 4];
        wire.push(2); // promises 32 payload bytes…
        wire.extend(*b"StreamTitle='cut"); // …but only 16 arrive
        let mut reader = MetaintReader::new(4);
        let out = reader.push(&wire);
        assert_eq!(out.audio, vec![1, 2, 3, 4]);
        assert!(out.updates.is_empty()); // incomplete block never surfaces
        assert_eq!(reader.malformed_blocks(), 0);
    }

    #[test]
    fn empty_push_is_noop() {
        let mut reader = MetaintReader::new(8);
        assert_eq!(reader.push(&[]), DeinterleavedChunk::default());
    }

    #[test]
    fn zero_metaint_clamps_to_one() {
        assert_eq!(MetaintReader::new(0).metaint(), 1);
        // And the clamped reader agrees with a clamped interleaver.
        let mut weaver = IcyMetaInterleaver::new(0);
        weaver.set_title("Tick");
        let wire = weaver.push(&[1, 2, 3]);
        let out = parse_metaint_stream(0, &wire);
        assert_eq!(out.audio, vec![1, 2, 3]);
        assert_eq!(out.updates.len(), 1);
    }
}
