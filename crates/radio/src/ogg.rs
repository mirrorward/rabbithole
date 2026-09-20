//! Walking an Ogg stream page by page, so it can be sent at the speed it
//! plays.
//!
//! The same job [`crate::mp3`] does for MPEG audio, for the container that
//! carries Opus and Vorbis. A page says which sample its audio ends at (its
//! *granule position*); the difference between two of those, over the
//! stream's sample rate, is how much time the later page is worth. That is
//! all a station needs to pace a track it never decodes.
//!
//! Nothing here validates a page's checksum or reads its audio: the bytes go
//! out as they came in, and the listener's decoder is the judge of them. It
//! is forgiving about what surrounds the audio (a page that does not parse
//! ends the walk rather than the process) and strict about what it accepts:
//! a page counts only when its header is whole, its segment table is whole,
//! and its payload fits in the file.

/// One Ogg page's place in the file and the sample its audio ends at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// Byte offset of the page's capture pattern in the file.
    pub offset: usize,
    /// Whole page length in bytes, header and payload.
    pub len: usize,
    /// The stream position this page's audio ends at, in samples, or
    /// `None` for a page that completes no packet (a header page, or the
    /// middle of a packet spanning pages).
    pub granule: Option<u64>,
    /// First page of a logical stream (carries the codec's identification
    /// header).
    pub beginning: bool,
    /// Last page of a logical stream.
    pub end: bool,
}

/// The header of an Ogg page, before its segment table.
const HEADER: usize = 27;

/// Parse the page at `offset`, if a whole one is there.
pub fn page_at(bytes: &[u8], offset: usize) -> Option<Page> {
    let head = bytes.get(offset..offset + HEADER)?;
    if &head[..4] != b"OggS" || head[4] != 0 {
        return None;
    }
    let flags = head[5];
    let granule = u64::from_le_bytes(head[6..14].try_into().ok()?);
    let segments = usize::from(head[26]);
    let table = bytes.get(offset + HEADER..offset + HEADER + segments)?;
    let payload: usize = table.iter().map(|n| usize::from(*n)).sum();
    let len = HEADER + segments + payload;
    // The page must fit whole; a truncated tail is not a page.
    bytes.get(offset..offset + len)?;
    Some(Page {
        offset,
        len,
        // -1 means "no packet ends here": nothing to pace by.
        granule: (granule != u64::MAX).then_some(granule),
        beginning: flags & 0x02 != 0,
        end: flags & 0x04 != 0,
    })
}

/// Every page of the stream, in order, stopping at the first thing that is
/// not one (a truncated last page, an ID3 tag some taggers append, junk).
pub fn pages(bytes: &[u8]) -> Vec<Page> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(page) = page_at(bytes, at) {
        at += page.len;
        out.push(page);
    }
    out
}

/// What a stream carries, as its first page says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Opus,
    Vorbis,
}

impl Codec {
    /// The rate granule positions are counted in. Opus always counts in 48
    /// kHz, whatever the audio was recorded at; Vorbis counts in its own.
    pub fn granule_rate(self, declared: u32) -> u32 {
        match self {
            Codec::Opus => 48_000,
            Codec::Vorbis => declared,
        }
    }
}

/// The codec and granule rate of an Ogg stream, from its first page.
/// `None` when this is not an Ogg stream, or carries something else.
pub fn codec(bytes: &[u8]) -> Option<(Codec, u32)> {
    let first = page_at(bytes, 0)?;
    let body = bytes
        .get(first.offset + first.len - payload_len(bytes, &first)?..first.offset + first.len)?;
    if body.starts_with(b"OpusHead") {
        return Some((Codec::Opus, Codec::Opus.granule_rate(0)));
    }
    if body.starts_with(b"\x01vorbis") {
        // The identification header: version (4), channels (1), then the
        // sample rate.
        let rate = body.get(12..16)?;
        let rate = u32::from_le_bytes(rate.try_into().ok()?);
        return (rate > 0).then_some((Codec::Vorbis, rate));
    }
    None
}

/// How many bytes of a page are payload.
fn payload_len(bytes: &[u8], page: &Page) -> Option<usize> {
    let segments = usize::from(*bytes.get(page.offset + 26)?);
    Some(page.len - HEADER - segments)
}

/// Whether this looks like an Ogg stream a station could send: it begins
/// with a page, and that page says what it carries.
pub fn looks_like_ogg(bytes: &[u8]) -> bool {
    codec(bytes).is_some()
}

/// How long the stream plays for, in microseconds, as its last paced page
/// says. `0` when nothing in it is paced.
pub fn micros(bytes: &[u8], rate: u32) -> u64 {
    if rate == 0 {
        return 0;
    }
    pages(bytes)
        .iter()
        .filter_map(|p| p.granule)
        .max()
        .map(|last| last * 1_000_000 / u64::from(rate))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One page: `granule` (`None` for "no packet ends here"), `payload`
    /// bytes split into 255-byte segments, and the flags.
    pub(crate) fn page(
        granule: Option<u64>,
        payload: &[u8],
        beginning: bool,
        end: bool,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push((u8::from(beginning) << 1) | (u8::from(end) << 2));
        out.extend_from_slice(&granule.unwrap_or(u64::MAX).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes()); // serial
        out.extend_from_slice(&0u32.to_le_bytes()); // sequence
        out.extend_from_slice(&0u32.to_le_bytes()); // checksum, not checked here
        let mut table: Vec<u8> = Vec::new();
        let mut left = payload.len();
        while left >= 255 {
            table.push(255);
            left -= 255;
        }
        table.push(left as u8);
        out.push(table.len() as u8);
        out.extend_from_slice(&table);
        out.extend_from_slice(payload);
        out
    }

    fn opus_stream() -> Vec<u8> {
        let mut out = page(
            Some(0),
            b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00",
            true,
            false,
        );
        out.extend(page(
            Some(0),
            b"OpusTags\x00\x00\x00\x00\x00\x00\x00\x00",
            false,
            false,
        ));
        // A second of audio, then another half.
        out.extend(page(Some(48_000), &[7u8; 300], false, false));
        out.extend(page(Some(72_000), &[7u8; 150], false, true));
        out
    }

    #[test]
    fn a_stream_says_what_it_carries_and_how_long_it_is() {
        let stream = opus_stream();
        assert_eq!(codec(&stream), Some((Codec::Opus, 48_000)));
        assert!(looks_like_ogg(&stream));
        assert_eq!(pages(&stream).len(), 4);
        assert_eq!(micros(&stream, 48_000), 1_500_000);

        // Vorbis counts granule in its own rate, and says what that is.
        let mut vorbis = page(
            Some(0),
            b"\x01vorbis\x00\x00\x00\x00\x02\x44\xac\x00\x00",
            true,
            false,
        );
        vorbis.extend(page(Some(44_100), &[3u8; 120], false, true));
        assert_eq!(codec(&vorbis), Some((Codec::Vorbis, 44_100)));
        assert_eq!(micros(&vorbis, 44_100), 1_000_000);
    }

    #[test]
    fn nothing_that_is_not_a_stream_is_taken_for_one() {
        assert!(!looks_like_ogg(b""));
        assert!(!looks_like_ogg(b"ID3\x04\x00\x00\x00\x00\x00\x00"));
        // An Ogg page carrying something else (Theora, say) is not ours.
        let video = page(Some(0), b"\x80theora rest", true, false);
        assert!(!looks_like_ogg(&video));
        // A page cut short is not a page: the walk ends rather than reading
        // past the end.
        let whole = opus_stream();
        let cut = &whole[..whole.len() - 40];
        assert_eq!(pages(cut).len(), 3);
        assert!(looks_like_ogg(cut), "what is there still parses");
    }

    #[test]
    fn a_page_that_completes_no_packet_is_not_paced() {
        let mut stream = page(
            Some(0),
            b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00",
            true,
            false,
        );
        stream.extend(page(None, &[1u8; 400], false, false));
        stream.extend(page(Some(96_000), &[1u8; 400], false, true));
        let pages = pages(&stream);
        assert_eq!(pages[1].granule, None, "the middle of a packet");
        assert_eq!(pages[2].granule, Some(96_000));
        assert_eq!(micros(&stream, 48_000), 2_000_000);
    }
}
