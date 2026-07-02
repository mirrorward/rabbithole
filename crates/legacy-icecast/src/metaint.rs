//! ICY in-band metadata interleaving (the `icy-metaint` math).
//!
//! When a listener sends `Icy-MetaData: 1`, the server promises to inject a
//! metadata block into the audio stream after every `icy-metaint` bytes of
//! audio. The block is:
//!
//! - one **length byte** = `ceil(payload_len / 16)` (the count of 16-byte
//!   units), and
//! - that many 16-byte units of payload: the metadata text, NUL-padded up to
//!   `length_byte * 16` bytes.
//!
//! When the metadata has not changed since the previous boundary, the whole
//! block collapses to a single `0x00` byte (length zero, no payload). The
//! payload text itself is `StreamTitle='<title>';` — see [`format_metadata`].
//!
//! [`IcyMetaInterleaver`] weaves these blocks into an arbitrary byte stream at
//! exact boundaries, so callers can feed audio in chunks of any size and get a
//! correctly-metaint'd stream back.
//!
//! ```
//! use rabbithole_legacy_icecast::{IcyMetaInterleaver, DEFAULT_METAINT};
//!
//! let mut weaver = IcyMetaInterleaver::new(16);
//! weaver.set_title("Song A");
//! let out = weaver.push(&[0xAAu8; 16]); // one full metaint of audio
//! assert_eq!(&out[..16], &[0xAA; 16]);   // audio preserved
//! assert_eq!(out[16], 2);                // "StreamTitle='Song A';" (21 B) -> 2 units
//! assert_eq!(DEFAULT_METAINT, 8192);
//! ```

/// The default `icy-metaint` spacing used by SHOUTcast/Icecast: a metadata
/// block after every 8192 bytes of audio.
pub const DEFAULT_METAINT: usize = 8192;

/// Maximum number of 16-byte units the single length byte can encode.
const MAX_UNITS: usize = 255;

/// Largest metadata payload the length byte can describe, in bytes.
const MAX_PAYLOAD: usize = MAX_UNITS * 16;

/// Builds one ICY metadata block for `title`.
///
/// The payload is `StreamTitle='<title>';`, NUL-padded up to a multiple of 16
/// bytes, prefixed with a length byte equal to the number of 16-byte units.
/// The returned vector is therefore always `1 + length_byte * 16` bytes long.
///
/// Overlong titles are truncated (at a UTF-8 char boundary) so the payload
/// never exceeds the 255-unit ceiling the length byte can encode.
pub fn format_metadata(title: &str) -> Vec<u8> {
    let mut payload = format!("StreamTitle='{title}';").into_bytes();
    if payload.len() > MAX_PAYLOAD {
        truncate_to_char_boundary(&mut payload, MAX_PAYLOAD);
    }
    let units = payload.len().div_ceil(16);
    let mut block = Vec::with_capacity(1 + units * 16);
    block.push(units as u8);
    block.extend_from_slice(&payload);
    block.resize(1 + units * 16, 0);
    block
}

/// Truncates `buf` to at most `max` bytes, backing up to the previous UTF-8
/// char boundary so a multibyte sequence is never split. `buf` must be valid
/// UTF-8 up to `max`'s neighbourhood (it always is: it came from a `String`).
fn truncate_to_char_boundary(buf: &mut Vec<u8>, max: usize) {
    let mut end = max;
    // UTF-8 continuation bytes are 0b10xx_xxxx; back up off any of them.
    while end > 0 && (buf[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    buf.truncate(end);
}

/// Interleaves ICY metadata blocks into a byte stream at `metaint` boundaries.
///
/// Feed audio through [`push`](IcyMetaInterleaver::push) in chunks of any size;
/// after every `metaint` audio bytes the weaver appends a metadata block. The
/// block carries the current title only when it *changed* since the previous
/// boundary — otherwise a single `0x00` byte is emitted, exactly as ICY
/// requires. Only audio bytes count toward the boundary; injected metadata
/// bytes do not.
#[derive(Clone, Debug)]
pub struct IcyMetaInterleaver {
    metaint: usize,
    /// Audio bytes emitted since the last boundary (always `< metaint`).
    since_boundary: usize,
    /// Title the caller most recently set (the next block to emit).
    pending: Option<String>,
    /// Title carried by the most recent non-empty block actually emitted.
    emitted: Option<String>,
}

impl IcyMetaInterleaver {
    /// Creates an interleaver with the given metaint spacing.
    ///
    /// `metaint` is clamped to at least 1 so a boundary is always reachable
    /// (a zero metaint would mean "a block before every byte", which no client
    /// negotiates and which would never terminate the fill loop cleanly).
    pub fn new(metaint: usize) -> Self {
        Self {
            metaint: metaint.max(1),
            since_boundary: 0,
            pending: None,
            emitted: None,
        }
    }

    /// The metaint spacing this weaver interleaves at.
    pub fn metaint(&self) -> usize {
        self.metaint
    }

    /// Sets the current track title. The next boundary will carry it as a
    /// metadata block; subsequent boundaries emit `0x00` until it changes
    /// again. Setting the same title that was last emitted is a no-op
    /// (still yields `0x00` at the next boundary).
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.pending = Some(title.into());
    }

    /// Interleaves `audio` with metadata blocks and returns the wire bytes.
    ///
    /// Audio bytes appear at the same relative order and spacing as the input;
    /// only whole metadata blocks are spliced in at each `metaint` boundary.
    pub fn push(&mut self, audio: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(audio.len() + audio.len() / self.metaint + 1);
        let mut pos = 0;
        while pos < audio.len() {
            let room = self.metaint - self.since_boundary;
            let take = room.min(audio.len() - pos);
            out.extend_from_slice(&audio[pos..pos + take]);
            pos += take;
            self.since_boundary += take;
            if self.since_boundary == self.metaint {
                out.extend_from_slice(&self.boundary_block());
                self.since_boundary = 0;
            }
        }
        out
    }

    /// Produces the metadata block for the current boundary: a fresh
    /// `StreamTitle` block when the title changed since the last emitted one,
    /// otherwise the single `0x00` "unchanged" byte.
    fn boundary_block(&mut self) -> Vec<u8> {
        if self.pending != self.emitted {
            self.emitted = self.pending.clone();
            format_metadata(self.pending.as_deref().unwrap_or(""))
        } else {
            vec![0]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_metaint_is_8192() {
        assert_eq!(DEFAULT_METAINT, 8192);
        assert_eq!(IcyMetaInterleaver::new(DEFAULT_METAINT).metaint(), 8192);
    }

    #[test]
    fn metaint_clamped_to_one() {
        assert_eq!(IcyMetaInterleaver::new(0).metaint(), 1);
    }

    #[test]
    fn format_metadata_length_byte_is_units_of_16() {
        // "StreamTitle='';" = 15 bytes -> ceil(15/16) = 1 unit.
        let block = format_metadata("");
        assert_eq!(block[0], 1);
        assert_eq!(block.len(), 1 + 16);
        assert_eq!(&block[1..16], b"StreamTitle='';"); // 15-byte payload
        assert_eq!(&block[16..], &[0]); // one NUL padding byte to fill 16

        // A title pushing payload to 17..=32 bytes -> 2 units.
        let title = "0123456789"; // "StreamTitle='0123456789';" = 25 bytes
        let block = format_metadata(title);
        assert_eq!(block[0], 2);
        assert_eq!(block.len(), 1 + 32);
        // Everything past the payload is NUL.
        assert!(block[1 + 25..].iter().all(|&b| b == 0));
    }

    #[test]
    fn format_metadata_rounds_up_at_every_boundary() {
        for extra in 0..40usize {
            let title = "x".repeat(extra);
            let block = format_metadata(&title);
            let payload_len = format!("StreamTitle='{title}';").len();
            let units = payload_len.div_ceil(16);
            assert_eq!(block[0] as usize, units, "extra={extra}");
            assert_eq!(block.len(), 1 + units * 16, "extra={extra}");
        }
    }

    #[test]
    fn format_metadata_truncates_overlong_title() {
        let block = format_metadata(&"a".repeat(10_000));
        assert_eq!(block[0], 255);
        assert_eq!(block.len(), 1 + 255 * 16);
    }

    #[test]
    fn format_metadata_truncation_respects_char_boundary() {
        // A run of 3-byte chars that would straddle the 4080-byte cap.
        let block = format_metadata(&"あ".repeat(2_000));
        assert_eq!(block[0], 255);
        // Payload region must remain valid UTF-8 up to its NUL padding.
        let payload_end = block[1..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(block.len() - 1);
        assert!(std::str::from_utf8(&block[1..1 + payload_end]).is_ok());
    }

    #[test]
    fn emits_block_on_exact_boundary() {
        let mut w = IcyMetaInterleaver::new(16);
        w.set_title("Song A");
        let out = w.push(&[0xAA; 16]);
        assert_eq!(&out[..16], &[0xAA; 16]);
        let expected = format_metadata("Song A");
        assert_eq!(&out[16..], expected.as_slice());
    }

    #[test]
    fn emits_zero_byte_when_unchanged() {
        let mut w = IcyMetaInterleaver::new(4);
        w.set_title("Track");
        let first = w.push(&[1, 2, 3, 4]);
        assert_eq!(first[0..4], [1, 2, 3, 4]);
        assert_eq!(first[4], format_metadata("Track")[0]);
        // Second boundary with no title change -> single 0x00.
        let second = w.push(&[5, 6, 7, 8]);
        assert_eq!(second, vec![5, 6, 7, 8, 0]);
    }

    #[test]
    fn zero_byte_from_the_start_without_title() {
        let mut w = IcyMetaInterleaver::new(4);
        let out = w.push(&[1, 2, 3, 4]);
        assert_eq!(out, vec![1, 2, 3, 4, 0]);
    }

    #[test]
    fn setting_same_title_yields_unchanged() {
        let mut w = IcyMetaInterleaver::new(4);
        w.set_title("Same");
        let _ = w.push(&[0; 4]);
        w.set_title("Same"); // no real change
        let out = w.push(&[0; 4]);
        assert_eq!(*out.last().unwrap(), 0);
    }

    #[test]
    fn title_change_re_emits_block() {
        let mut w = IcyMetaInterleaver::new(4);
        w.set_title("First");
        let _ = w.push(&[0; 4]);
        w.set_title("Second");
        let out = w.push(&[0; 4]);
        assert_eq!(&out[4..], format_metadata("Second").as_slice());
    }

    #[test]
    fn boundary_spans_multiple_pushes() {
        let mut w = IcyMetaInterleaver::new(8);
        w.set_title("T");
        let a = w.push(&[1, 2, 3]);
        assert_eq!(a, vec![1, 2, 3]); // no boundary yet
        let b = w.push(&[4, 5, 6, 7, 8, 9]); // crosses at byte 8
                                             // First 5 bytes complete the metaint, then a block, then leftover 9.
        assert_eq!(&b[..5], &[4, 5, 6, 7, 8]);
        let block = format_metadata("T");
        assert_eq!(&b[5..5 + block.len()], block.as_slice());
        assert_eq!(&b[5 + block.len()..], &[9]);
    }

    #[test]
    fn audio_byte_positions_are_preserved() {
        // Feed a known ramp across many boundaries in odd-sized chunks, then
        // strip metadata blocks and confirm the audio comes back intact.
        let metaint = 8192;
        let mut w = IcyMetaInterleaver::new(metaint);
        w.set_title("Now Playing");
        let audio: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();

        let mut wire = Vec::new();
        for chunk in audio.chunks(1000) {
            wire.extend(w.push(chunk));
        }

        // Strip: read metaint audio bytes, then a length byte + length*16.
        let mut recovered = Vec::new();
        let mut i = 0;
        while i < wire.len() {
            let n = metaint.min(wire.len() - i);
            recovered.extend_from_slice(&wire[i..i + n]);
            i += n;
            if i >= wire.len() {
                break;
            }
            let units = wire[i] as usize;
            i += 1 + units * 16;
        }
        assert_eq!(recovered, audio);
    }

    #[test]
    fn empty_push_is_noop() {
        let mut w = IcyMetaInterleaver::new(16);
        assert!(w.push(&[]).is_empty());
    }
}
