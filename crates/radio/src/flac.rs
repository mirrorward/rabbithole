//! Walking a FLAC file frame by frame, so it can be sent at the speed it
//! plays.
//!
//! The third of the three ([`crate::mp3`] reads frame headers,
//! [`crate::ogg`] reads page granules), and the awkward one: **a FLAC frame
//! header does not say how long the frame is**. An encoder writes a frame,
//! then the next, and a reader that is not decoding has to find where one
//! ends by looking for where the next begins — and be sure it has not been
//! fooled by audio that happens to look like a sync.
//!
//! Two things make that safe. A candidate start must parse as a whole frame
//! header *and* match the CRC-8 the encoder wrote over it; and the frame
//! that would end there must match the CRC-16 the encoder wrote over the
//! whole of it. Both have to be wrong at once for a false frame to pass,
//! which is not something audio does by accident.
//!
//! Nothing here decodes: it reads headers, counts samples, and checks the
//! two checksums. What a frame *sounds* like is the listener's business.

/// One FLAC frame's place in the file and its length in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Byte offset of the frame's sync code in the file.
    pub offset: usize,
    /// Whole frame length in bytes, header and CRC-16 included.
    pub len: usize,
    /// PCM samples this frame holds (per channel): its block size.
    pub samples: u32,
    /// Samples per second, from the frame or the stream.
    pub sample_rate: u32,
}

impl Frame {
    /// How long this frame plays for, in microseconds.
    pub fn micros(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        u64::from(self.samples) * 1_000_000 / u64::from(self.sample_rate)
    }
}

/// What the stream says about itself, from its STREAMINFO block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    /// Samples per channel in the whole stream, or 0 when the encoder did
    /// not know (a live encode).
    pub total_samples: u64,
    /// The largest frame the encoder wrote, in bytes, or 0 when it did not
    /// say. A frame cannot be longer than this, which bounds the search for
    /// where one ends.
    pub max_frame: u32,
    /// Where the audio starts: past the magic and every metadata block.
    pub audio_at: usize,
}

/// Block sizes by their 4-bit code. `None`: read it from the header itself
/// (codes 6 and 7), or not a block size at all (code 0).
const BLOCK_SIZES: [Option<u32>; 16] = [
    None,
    Some(192),
    Some(576),
    Some(1152),
    Some(2304),
    Some(4608),
    None,
    None,
    Some(256),
    Some(512),
    Some(1024),
    Some(2048),
    Some(4096),
    Some(8192),
    Some(16384),
    Some(32768),
];

/// Sample rates by their 4-bit code. `None`: from STREAMINFO (code 0), read
/// from the header (12, 13, 14), or invalid (15).
const SAMPLE_RATES: [Option<u32>; 16] = [
    None,
    Some(88_200),
    Some(176_400),
    Some(192_000),
    Some(8_000),
    Some(16_000),
    Some(22_050),
    Some(24_000),
    Some(32_000),
    Some(44_100),
    Some(48_000),
    Some(96_000),
    None,
    None,
    None,
    None,
];

/// The CRC the encoder writes over a frame header (polynomial 0x07, no
/// reflection, zero initial value — CRC-8/SMBUS).
fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The CRC the encoder writes over a whole frame (polynomial 0x8005, no
/// reflection, zero initial value — CRC-16/ARC without its reflections,
/// which the catalogues call CRC-16/UMTS or BUYPASS).
fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Feed `bytes` into a CRC-16 already covering what came before.
fn crc16_more(mut crc: u16, bytes: &[u8]) -> u16 {
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// What the stream says about itself, and where its audio starts. `None`
/// when this is not a FLAC file, or its STREAMINFO is not whole.
pub fn streaminfo(bytes: &[u8]) -> Option<Stream> {
    // The format says the magic comes first, and taggers disagree: an
    // ID3v2 tag in front of a FLAC file is common enough that every player
    // skips it, so this does too.
    let start = crate::mp3::id3v2_len(bytes);
    if bytes.get(start..start + 4)? != b"fLaC" {
        return None;
    }
    let mut at = start + 4;
    let mut found: Option<Stream> = None;
    loop {
        let header = bytes.get(at..at + 4)?;
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7F;
        let len =
            usize::from(header[1]) << 16 | usize::from(header[2]) << 8 | usize::from(header[3]);
        let body = bytes.get(at + 4..at + 4 + len)?;
        if kind == 0 {
            // Sample rate (20), channels - 1 (3), bits - 1 (5), then the
            // sample count (36), packed across eight bytes from offset 10.
            let packed = body.get(10..18)?;
            let rate = (u32::from(packed[0]) << 12)
                | (u32::from(packed[1]) << 4)
                | (u32::from(packed[2]) >> 4);
            let channels = ((packed[2] >> 1) & 0x07) + 1;
            let bits = (((packed[2] & 0x01) << 4) | (packed[3] >> 4)) + 1;
            let total = (u64::from(packed[3] & 0x0F) << 32)
                | (u64::from(packed[4]) << 24)
                | (u64::from(packed[5]) << 16)
                | (u64::from(packed[6]) << 8)
                | u64::from(packed[7]);
            let max_frame =
                (u32::from(body[7]) << 16) | (u32::from(body[8]) << 8) | u32::from(body[9]);
            found = Some(Stream {
                sample_rate: rate,
                channels,
                bits_per_sample: bits,
                total_samples: total,
                max_frame,
                audio_at: 0, // filled in once the last block is past
            });
        }
        at += 4 + len;
        if last {
            break;
        }
    }
    let mut info = found?;
    info.audio_at = at;
    (info.sample_rate > 0).then_some(info)
}

/// Whether this looks like a FLAC file a station could send.
pub fn looks_like_flac(bytes: &[u8]) -> bool {
    streaminfo(bytes).is_some()
}

/// A frame header at `at`, if a whole and self-consistent one is there:
/// returns (block size, sample rate, header length).
fn header_at(bytes: &[u8], at: usize, info: &Stream) -> Option<(u32, u32, usize)> {
    let head = bytes.get(at..at + 4)?;
    // Fourteen sync bits, then a reserved bit that is zero in every real
    // frame, then the blocking strategy. Insisting on the reserved zero
    // turns half the bytes that look like a sync into the audio they are.
    if head[0] != 0xFF || head[1] & 0xFE != 0xF8 {
        return None;
    }
    let block_code = (head[2] >> 4) as usize;
    let rate_code = (head[2] & 0x0F) as usize;
    let channels = head[3] >> 4;
    let size_code = (head[3] >> 1) & 0x07;
    // Reserved bits are zero in a real frame, and a way to rule out a
    // sync that is really audio.
    if head[3] & 0x01 != 0 || block_code == 0 || rate_code == 15 || channels > 10 {
        return None;
    }
    // Sample size 3 has never meant anything. 7 used to be reserved too and
    // now means 32 bits a sample, which the reference encoder writes, so a
    // parser that refuses it finds no frames at all in such a file.
    if size_code == 3 {
        return None;
    }
    // The frame or sample number, in the UTF-8 shape (up to seven bytes).
    let lead = *bytes.get(at + 4)?;
    let coded = match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        0xF8..=0xFB => 5,
        0xFC..=0xFD => 6,
        0xFE => 7,
        _ => return None,
    };
    let mut p = at + 4 + coded;
    let mut block = BLOCK_SIZES[block_code];
    if block_code == 6 {
        block = Some(u32::from(*bytes.get(p)?) + 1);
        p += 1;
    } else if block_code == 7 {
        let pair = bytes.get(p..p + 2)?;
        block = Some((u32::from(pair[0]) << 8 | u32::from(pair[1])) + 1);
        p += 2;
    }
    let mut rate = if rate_code == 0 {
        Some(info.sample_rate)
    } else {
        SAMPLE_RATES[rate_code]
    };
    match rate_code {
        12 => {
            rate = Some(u32::from(*bytes.get(p)?) * 1000);
            p += 1;
        }
        13 | 14 => {
            let pair = bytes.get(p..p + 2)?;
            let n = u32::from(pair[0]) << 8 | u32::from(pair[1]);
            rate = Some(if rate_code == 13 { n } else { n * 10 });
            p += 2;
        }
        _ => {}
    }
    // The encoder's own check on everything above.
    if crc8(bytes.get(at..p)?) != *bytes.get(p)? {
        return None;
    }
    Some((block?, rate?, p + 1 - at))
}

/// Every frame of the file, in order. A frame counts only when its header
/// checks out and the CRC-16 over the whole of it matches, so audio that
/// looks like a sync does not become a frame.
pub fn frames(bytes: &[u8]) -> Vec<Frame> {
    let Some(info) = streaminfo(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut at = info.audio_at;
    while at + 6 < bytes.len() {
        let Some((samples, sample_rate, header_len)) = header_at(bytes, at, &info) else {
            at += 1;
            continue;
        };
        let Some(end) = frame_end(bytes, at, at + header_len, &info) else {
            break;
        };
        out.push(Frame {
            offset: at,
            len: end - at,
            samples,
            sample_rate,
        });
        at = end;
    }
    out
}

/// Where the frame starting at `at` ends: the next place a frame begins and
/// the CRC-16 before it matches, or the end of the file for the last one.
fn frame_end(bytes: &[u8], at: usize, after_header: usize, info: &Stream) -> Option<usize> {
    // The running CRC covers `at .. covered`, two bytes behind the probe,
    // so each step costs one byte rather than the whole frame again.
    let mut covered = after_header.saturating_sub(2).max(at);
    let mut crc = crc16(bytes.get(at..covered)?);
    let mut probe = after_header;
    // A frame is never longer than the encoder said its longest is, so a
    // search that has gone past that is following a false sync, not a
    // frame. Without that word from the encoder, the whole file is the
    // bound.
    let limit = match info.max_frame {
        0 => bytes.len(),
        most => (at + most as usize + 2).min(bytes.len()),
    };
    while probe + 1 < limit {
        if bytes[probe] == 0xFF
            && bytes[probe + 1] & 0xFE == 0xF8
            && probe >= at + 6
            && header_at(bytes, probe, info).is_some()
        {
            let want = bytes.get(probe - 2..probe)?;
            let want = u16::from(want[0]) << 8 | u16::from(want[1]);
            if crc16_more(crc, bytes.get(covered..probe - 2)?) == want {
                return Some(probe);
            }
        }
        probe += 1;
        if probe >= 2 && probe - 2 > covered {
            crc = crc16_more(crc, bytes.get(covered..probe - 2)?);
            covered = probe - 2;
        }
    }
    // The last frame: its CRC-16 is the last two bytes of the audio. An
    // ID3v1 tag some taggers append after it is not audio and is not sent.
    let tail = bytes.len().checked_sub(128);
    for end in [Some(bytes.len()), tail].into_iter().flatten() {
        if end > at + 6 && end <= bytes.len() {
            let want = bytes.get(end - 2..end)?;
            let want = u16::from(want[0]) << 8 | u16::from(want[1]);
            if crc16(bytes.get(at..end - 2)?) == want {
                return Some(end);
            }
        }
    }
    None
}

/// How long the whole stream plays for, in microseconds, as STREAMINFO
/// says. `0` when it does not say.
pub fn micros(bytes: &[u8]) -> u64 {
    let Some(info) = streaminfo(bytes) else {
        return 0;
    };
    if info.sample_rate == 0 {
        return 0;
    }
    info.total_samples * 1_000_000 / u64::from(info.sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksums_are_the_ones_the_catalogues_name() {
        // Checked against the standard check value of each CRC over
        // "123456789", so a wrong table cannot agree with itself.
        assert_eq!(crc8(b"123456789"), 0xF4, "CRC-8/SMBUS");
        assert_eq!(crc16(b"123456789"), 0xFEE8, "CRC-16 with polynomial 0x8005");
        // Feeding it in two halves is the same as feeding it whole.
        let split = crc16_more(crc16(b"12345"), b"6789");
        assert_eq!(split, 0xFEE8);
    }

    #[test]
    fn nothing_that_is_not_a_flac_file_is_taken_for_one() {
        assert!(!looks_like_flac(b""));
        assert!(!looks_like_flac(b"fLaC"));
        assert!(!looks_like_flac(b"ID3\x04\x00\x00\x00\x00\x00\x00"));
        assert!(frames(b"not audio at all").is_empty());
    }
}
