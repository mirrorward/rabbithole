//! Walking an MP3 file frame by frame, so it can be streamed at the speed it
//! plays.
//!
//! A radio station does not send a file; it sends audio *at the rate audio
//! happens*. To do that for a library track without decoding it, the server
//! needs only two facts per frame: how many bytes it is and how much time it
//! is. Both are in the four-byte MPEG audio frame header. This module reads
//! those headers and nothing else: no decode, no dependencies, no I/O.
//!
//! It is deliberately forgiving about what surrounds the audio, because real
//! files are: an ID3v2 tag in front is skipped by its declared size, an ID3v1
//! tag or other junk at the end simply fails to parse as a frame, and garbage
//! in the middle is resynchronised over a byte at a time. It is strict about
//! what it *accepts*: a header only counts as a frame if its fields are all
//! legal and the frame fits in the file, which is what keeps a stray `0xFF`
//! in a tag from being mistaken for audio.

/// One MPEG audio frame's place in the file and its length in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Byte offset of the frame's header in the file.
    pub offset: usize,
    /// Whole frame length in bytes, header included.
    pub len: usize,
    /// PCM samples this frame decodes to (per channel).
    pub samples: u32,
    /// Samples per second.
    pub sample_rate: u32,
    /// MPEG layer: 1, 2 or 3.
    pub layer: u8,
}

impl Frame {
    /// How long this frame plays for, in microseconds.
    pub fn micros(&self) -> u64 {
        u64::from(self.samples) * 1_000_000 / u64::from(self.sample_rate)
    }
}

/// Bytes to skip at the start of `bytes` for an ID3v2 tag, or 0 when there is
/// none. The tag's size is four 7-bit ("syncsafe") bytes, so that no tag byte
/// can ever look like a frame sync.
pub fn id3v2_len(bytes: &[u8]) -> usize {
    // Clamped: a tag that lies about its size cannot walk a reader off the
    // end of what it is holding.
    id3v2_says(bytes).min(bytes.len())
}

/// What a tag in front of a file says its own length is, even when the file
/// in hand stops short of it. A caller holding only the front of a file uses
/// this to go and read the part that matters; a caller holding the whole
/// file wants [`id3v2_len`], which is this clamped to what is there.
pub fn id3v2_says(front: &[u8]) -> usize {
    if front.len() < 10 || &front[..3] != b"ID3" {
        return 0;
    }
    let size = &front[6..10];
    if size.iter().any(|b| b & 0x80 != 0) {
        return 0; // not syncsafe: not a tag we understand
    }
    let body = size.iter().fold(0usize, |n, b| (n << 7) | usize::from(*b));
    let footer = if front[5] & 0x10 != 0 { 10 } else { 0 };
    10 + body + footer
}

/// Parse a frame header at `offset`, if a legal one that fits is there.
pub fn frame_at(bytes: &[u8], offset: usize) -> Option<Frame> {
    let h = bytes.get(offset..offset + 4)?;
    // Eleven sync bits.
    if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    // Version: 00 = MPEG 2.5, 01 = reserved, 10 = MPEG 2, 11 = MPEG 1.
    let version = (h[1] >> 3) & 0b11;
    // Layer: 00 = reserved, 01 = III, 10 = II, 11 = I.
    let layer = (h[1] >> 1) & 0b11;
    let bitrate_index = usize::from(h[2] >> 4);
    let rate_index = usize::from((h[2] >> 2) & 0b11);
    let padding = usize::from((h[2] >> 1) & 1);
    if version == 0b01
        || layer == 0b00
        || bitrate_index == 0
        || bitrate_index == 15
        || rate_index == 3
    {
        return None; // reserved values, or "free format", which has no length
    }
    let mpeg1 = version == 0b11;
    const V1_L1: [u32; 15] = [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ];
    const V1_L2: [u32; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ];
    const V1_L3: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const V2_L1: [u32; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ];
    const V2_L23: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let kbps = match (mpeg1, layer) {
        (true, 0b11) => V1_L1,
        (true, 0b10) => V1_L2,
        (true, _) => V1_L3,
        (false, 0b11) => V2_L1,
        (false, _) => V2_L23,
    }[bitrate_index];
    let sample_rate = match version {
        0b11 => [44_100, 48_000, 32_000],
        0b10 => [22_050, 24_000, 16_000],
        _ => [11_025, 12_000, 8_000],
    }[rate_index];
    let bitrate = kbps as usize * 1000;
    let rate = sample_rate as usize;
    let (samples, len) = match (layer, mpeg1) {
        // Layer I counts in 4-byte slots.
        (0b11, _) => (384, (12 * bitrate / rate + padding) * 4),
        (0b10, _) => (1152, 144 * bitrate / rate + padding),
        (_, true) => (1152, 144 * bitrate / rate + padding),
        (_, false) => (576, 72 * bitrate / rate + padding),
    };
    if len < 4 || offset + len > bytes.len() {
        return None; // a header whose frame runs off the end is not a frame
    }
    Some(Frame {
        offset,
        len,
        samples,
        sample_rate,
        layer: 4 - layer,
    })
}

/// Every frame in `bytes`, in order. Skips a leading ID3v2 tag and
/// resynchronises over anything that is not a frame.
///
/// Four bytes that *parse* as a header are not yet a frame: eleven set bits
/// and a handful of legal fields turn up in tags, cover art and plain junk
/// (`FF FF 67 61`, the start of `\xFF\xFFgarbage`, is a well-formed Layer I
/// header). So the walker holds a candidate to two standards. Until the file
/// has shown what it is, a frame counts only if another legal frame of the
/// same kind follows exactly where it says one should. Once it has, every
/// frame must agree with it on layer and sample rate, which no real file
/// changes midway.
pub fn frames(bytes: &[u8]) -> Frames<'_> {
    Frames {
        bytes,
        at: id3v2_len(bytes),
        kind: None,
    }
}

/// Iterator over a file's frames. See [`frames`].
pub struct Frames<'a> {
    bytes: &'a [u8],
    at: usize,
    /// The (layer, sample rate) this file's frames have, once established.
    kind: Option<(u8, u32)>,
}

impl Iterator for Frames<'_> {
    type Item = Frame;

    fn next(&mut self) -> Option<Frame> {
        while self.at + 4 <= self.bytes.len() {
            if let Some(frame) = frame_at(self.bytes, self.at) {
                let kind = (frame.layer, frame.sample_rate);
                let believable = match self.kind {
                    Some(established) => established == kind,
                    None => frame_at(self.bytes, self.at + frame.len)
                        .is_some_and(|next| (next.layer, next.sample_rate) == kind),
                };
                if believable {
                    self.kind = Some(kind);
                    self.at += frame.len;
                    return Some(frame);
                }
            }
            self.at += 1;
        }
        None
    }
}

/// Does this look like an MP3 at all? True when two frames sit back to back
/// somewhere near the start: one legal-looking header can be an accident, two
/// in a row where the first says the second should be is not.
pub fn looks_like_mp3(bytes: &[u8]) -> bool {
    let start = id3v2_len(bytes);
    let window = bytes.len().min(start + 64 * 1024);
    (start..window.saturating_sub(4)).any(|at| {
        frame_at(bytes, at).is_some_and(|first| {
            frame_at(bytes, at + first.len).is_some_and(|next| {
                (next.layer, next.sample_rate) == (first.layer, first.sample_rate)
            })
        })
    })
}

/// The file's playing time in milliseconds: the sum of its frames.
pub fn duration_ms(bytes: &[u8]) -> u64 {
    frames(bytes).map(|f| f.micros()).sum::<u64>() / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A structurally valid frame: a real header and a silent body.
    /// `FF FB 90 00` = MPEG-1 Layer III, 128 kbit/s, 44.1 kHz, no padding.
    fn frame(header: [u8; 4]) -> Vec<u8> {
        let len = frame_at(&[&header[..], &[0u8; 2048][..]].concat(), 0)
            .expect("a legal header")
            .len;
        let mut f = header.to_vec();
        f.resize(len, 0);
        f
    }

    const CD_128: [u8; 4] = [0xFF, 0xFB, 0x90, 0x00];

    #[test]
    fn the_common_frame_is_417_bytes_and_26_milliseconds() {
        let bytes = frame(CD_128);
        let f = frame_at(&bytes, 0).unwrap();
        assert_eq!(
            (f.len, f.samples, f.sample_rate, f.layer),
            (417, 1152, 44_100, 3)
        );
        assert_eq!(f.micros(), 26_122);
        // The padding bit adds one byte.
        let padded = frame([0xFF, 0xFB, 0x92, 0x00]);
        assert_eq!(frame_at(&padded, 0).unwrap().len, 418);
        // MPEG-2 Layer III, 64 kbit/s at 22.05 kHz: half the samples.
        let low = frame([0xFF, 0xF3, 0x80, 0x00]);
        let f = frame_at(&low, 0).unwrap();
        assert_eq!((f.len, f.samples, f.sample_rate), (208, 576, 22_050));
    }

    #[test]
    fn reserved_fields_and_frames_that_run_off_the_end_are_not_frames() {
        let ok = frame(CD_128);
        assert!(frame_at(&ok, 0).is_some());
        assert!(frame_at(&ok[..416], 0).is_none(), "one byte short");
        let mut bad = ok.clone();
        bad[1] = 0xEB; // reserved version
        assert!(frame_at(&bad, 0).is_none());
        let mut bad = ok.clone();
        bad[1] = 0xF9; // reserved layer
        assert!(frame_at(&bad, 0).is_none());
        let mut bad = ok.clone();
        bad[2] = 0xF0; // bitrate index 15
        assert!(frame_at(&bad, 0).is_none());
        let mut bad = ok.clone();
        bad[2] = 0x00; // "free format": no way to know its length
        assert!(frame_at(&bad, 0).is_none());
        let mut bad = ok;
        bad[2] = 0x9C; // reserved sample rate
        assert!(frame_at(&bad, 0).is_none());
        assert!(frame_at(&[0xFF, 0xFB], 0).is_none(), "not even a header");
    }

    #[test]
    fn tags_in_front_junk_in_the_middle_and_a_tag_at_the_end_are_walked_past() {
        let one = frame(CD_128);
        // An ID3v2 tag of 300 body bytes, full of 0xFF to tempt the parser.
        let mut file = b"ID3\x04\x00\x00".to_vec();
        file.extend_from_slice(&[0x00, 0x00, 0x02, 0x2C]); // syncsafe 300
        file.extend(std::iter::repeat_n(0xFF, 300));
        assert_eq!(id3v2_len(&file), 310);
        for _ in 0..3 {
            file.extend_from_slice(&one);
        }
        file.extend_from_slice(b"\xFF\xFFgarbage in the middle\xFF");
        file.extend_from_slice(&one);
        file.extend_from_slice(b"TAG");
        file.extend_from_slice(&[0u8; 125]); // an ID3v1 tag

        let found: Vec<Frame> = frames(&file).collect();
        assert_eq!(found.len(), 4);
        assert_eq!(found[0].offset, 310);
        assert_eq!(found[1].offset, 310 + 417);
        // The junk begins `FF FF 67 61`, which parses as a legal Layer I header.
        // It is not believed: this file's frames are Layer III at 44.1 kHz.
        assert!(frame_at(&file, found[2].offset + 417).is_some());
        assert_eq!(found[3].offset, found[2].offset + 417 + 24);
        assert!(found
            .iter()
            .all(|f| f.layer == 3 && f.sample_rate == 44_100));
        assert_eq!(duration_ms(&file), 4 * 26_122 / 1000);
        assert!(looks_like_mp3(&file));
    }

    #[test]
    fn a_file_that_is_not_mp3_is_not_mistaken_for_one() {
        assert!(!looks_like_mp3(b""));
        assert!(!looks_like_mp3(b"OggS\x00\x02 definitely not mpeg audio"));
        // One accidental header with nothing legal after it is not enough.
        let mut lone = frame(CD_128);
        lone.extend_from_slice(&[0x12; 900]);
        assert!(!looks_like_mp3(&lone));
        assert_eq!(frames(b"no frames here").count(), 0);
        assert_eq!(duration_ms(b"no frames here"), 0);
        // A tag that lies about its size cannot walk off the end.
        let mut liar = b"ID3\x04\x00\x00\x7F\x7F\x7F\x7F".to_vec();
        liar.extend_from_slice(&[0; 20]);
        assert_eq!(id3v2_len(&liar), liar.len());
        assert_eq!(id3v2_says(&liar), 10 + 0x0FFF_FFFF, "what it claims");
        assert_eq!(frames(&liar).count(), 0);
    }
}
