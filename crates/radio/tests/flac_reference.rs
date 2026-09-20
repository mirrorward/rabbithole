//! The FLAC walker against a file the reference encoder made.
//!
//! A parser tested only on data it made itself can be wrong in the same way
//! twice and never know. `fixtures/reference-8k-mono.flac` was written by
//! `flac 1.5.0` (0.12 s of a 440 Hz tone, 8 kHz mono, block size 256,
//! `--no-padding --no-seektable`), so what it says about itself is the
//! outside truth this is measured against.

use rabbithole_radio::flac;

const REFERENCE: &[u8] = include_bytes!("fixtures/reference-8k-mono.flac");

#[test]
fn every_frame_of_a_real_file_is_found_and_paced() {
    let info = flac::streaminfo(REFERENCE).expect("a FLAC file");
    assert_eq!(
        (info.sample_rate, info.channels, info.bits_per_sample),
        (8_000, 1, 16),
        "what the encoder wrote in STREAMINFO"
    );
    assert_eq!(info.total_samples, 960);

    let frames = flac::frames(REFERENCE);
    assert_eq!(frames.len(), 4, "four block-sized frames: {frames:?}");
    // The stream's own sample count, reached by walking the frames.
    assert_eq!(
        frames.iter().map(|f| u64::from(f.samples)).sum::<u64>(),
        info.total_samples,
        "every sample accounted for"
    );
    // Frames are found back to back, from the first byte of audio to the
    // last: nothing skipped, nothing counted twice.
    assert_eq!(frames[0].offset, info.audio_at);
    assert!(
        frames
            .windows(2)
            .all(|w| w[0].offset + w[0].len == w[1].offset),
        "contiguous: {frames:?}"
    );
    let last = frames.last().unwrap();
    assert_eq!(last.offset + last.len, REFERENCE.len(), "to the last byte");

    // And the time adds up to the length of the audio.
    let micros: u64 = frames.iter().map(|f| f.micros()).sum();
    assert_eq!(micros, flac::micros(REFERENCE));
    assert_eq!(micros, 120_000, "0.12 s of tone");
}

#[test]
fn a_file_cut_short_gives_back_what_is_whole_and_no_more() {
    let frames = flac::frames(REFERENCE);
    let third = frames[2].offset;
    // Cut mid-frame: the frames before it are still found, and the part of
    // a frame that is left is not one.
    let cut = &REFERENCE[..third + 10];
    let found = flac::frames(cut);
    assert_eq!(found.len(), 2, "the two whole ones: {found:?}");
    assert_eq!(found[0].len, frames[0].len);
    // A file whose audio is gone entirely is no frames, not a panic.
    assert!(flac::frames(&REFERENCE[..flac::streaminfo(REFERENCE).unwrap().audio_at]).is_empty());
}

#[test]
fn audio_that_looks_like_a_frame_is_not_one() {
    // A sync pattern in the middle of a frame's audio must not split it:
    // the CRCs are what tell them apart. Plant one in a copy of the file
    // and the walk should still find the same frames — the planted bytes
    // break that frame's CRC-16, so it is not there to be found, and the
    // frames after it are.
    let frames = flac::frames(REFERENCE);
    let mut tampered = REFERENCE.to_vec();
    let inside = frames[1].offset + 8;
    tampered[inside] = 0xFF;
    tampered[inside + 1] = 0xF8;
    let found = flac::frames(&tampered);
    assert!(
        found.len() < frames.len(),
        "a frame whose bytes changed no longer checks out: {found:?}"
    );
    assert_eq!(
        found[0].len, frames[0].len,
        "the one before it is untouched"
    );
}

#[test]
fn a_tag_in_front_or_behind_is_not_mistaken_for_audio() {
    let frames = flac::frames(REFERENCE);

    // Taggers put ID3v2 in front of FLAC files, which the format does not
    // allow and every player skips. So does this.
    let mut tagged = Vec::new();
    let body = [0u8; 200];
    tagged.extend_from_slice(b"ID3\x04\x00\x00");
    tagged.extend_from_slice(&[0, 0, 1, 72]); // 200, seven bits a byte
    tagged.extend_from_slice(&body);
    tagged.extend_from_slice(REFERENCE);
    let info = flac::streaminfo(&tagged).expect("still a FLAC file");
    assert_eq!(
        info.audio_at,
        210 + flac::streaminfo(REFERENCE).unwrap().audio_at
    );
    let found = flac::frames(&tagged);
    assert_eq!(found.len(), frames.len());
    assert_eq!(
        found.iter().map(|f| u64::from(f.samples)).sum::<u64>(),
        info.total_samples
    );

    // And ID3v1 on the end is 128 bytes that are not the last frame.
    let mut trailing = REFERENCE.to_vec();
    trailing.extend_from_slice(b"TAG");
    trailing.extend(std::iter::repeat_n(b' ', 125));
    let found = flac::frames(&trailing);
    assert_eq!(found.len(), frames.len(), "the same frames: {found:?}");
    let last = found.last().unwrap();
    assert_eq!(
        last.offset + last.len,
        REFERENCE.len(),
        "the audio ends where it did; the tag is not sent"
    );
}

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

#[test]
fn a_thirty_two_bit_file_is_read_like_any_other() {
    // Sample size code 7 was reserved once and means 32 bits a sample now,
    // which the reference encoder writes. A parser that still calls it
    // reserved finds no frames at all in such a file — a station of
    // silence, saying it cannot play what it has. The fixture is rewritten
    // here to say 32 bits rather than shipping a second one: what matters
    // is the code path, not the audio.
    const BODY: usize = 8; // the magic and the first block header
    let mut wide = REFERENCE.to_vec();
    wide[BODY + 12] |= 0x01; // the top bit of (bits per sample) - 1 = 31
    wide[BODY + 13] = (wide[BODY + 13] & 0x0F) | 0xF0;
    assert_eq!(
        flac::streaminfo(&wide).unwrap().bits_per_sample,
        32,
        "the stream says 32 bits now"
    );

    // Every frame says so too, with both of its checksums put right.
    let frames = flac::frames(REFERENCE);
    for frame in &frames {
        let at = frame.offset;
        // This file's frames have a one-byte frame number, a block size
        // and rate from their codes: six bytes of header including the
        // CRC-8. The assertion below is what holds that to be true.
        let header = 5;
        assert_eq!(
            crc8(&wide[at..at + header]),
            wide[at + header],
            "header as expected"
        );
        wide[at + 3] = (wide[at + 3] & 0xF1) | (7 << 1);
        wide[at + header] = crc8(&wide[at..at + header]);
        let end = at + frame.len;
        let sum = crc16(&wide[at..end - 2]);
        wide[end - 2] = (sum >> 8) as u8;
        wide[end - 1] = sum as u8;
    }

    let found = flac::frames(&wide);
    assert_eq!(found.len(), frames.len(), "the same frames: {found:?}");
    assert_eq!(
        found.iter().map(|f| f.offset).collect::<Vec<_>>(),
        frames.iter().map(|f| f.offset).collect::<Vec<_>>(),
        "in the same places"
    );
}

/// STREAMINFO's "longest frame I wrote" bounds the search for the next
/// frame, and a file can carry a number that is too small: `ffmpeg -c copy`
/// cutting two files together keeps the first one's STREAMINFO, and the
/// second one's frames can be longer than anything the first one held. The
/// walk has to survive that without either giving up or — far worse —
/// taking the whole rest of the file for one frame and sending it as if it
/// were a fraction of a second of music.
#[test]
fn a_file_that_understates_its_longest_frame_is_still_walked() {
    let truth = flac::frames(REFERENCE);
    let longest = truth.iter().map(|f| f.len).max().unwrap();

    // STREAMINFO's max-frame field: three bytes, after the 4-byte magic,
    // the 4-byte block header, the two block sizes and the min-frame size.
    const MAX_FRAME: usize = 8 + 2 + 2 + 3;
    let rewrite = |most: usize| {
        let mut bytes = REFERENCE.to_vec();
        bytes[MAX_FRAME] = (most >> 16) as u8;
        bytes[MAX_FRAME + 1] = (most >> 8) as u8;
        bytes[MAX_FRAME + 2] = most as u8;
        bytes
    };
    // The file says its longest frame is one byte shorter than it is.
    let understated = rewrite(longest - 1);
    let found = flac::frames(&understated);
    assert_eq!(
        found.iter().map(|f| (f.offset, f.len)).collect::<Vec<_>>(),
        truth.iter().map(|f| (f.offset, f.len)).collect::<Vec<_>>(),
        "every frame, at its own length, whatever STREAMINFO claims"
    );
    // And it says nothing at all, which the format allows.
    let silent = rewrite(0);
    assert_eq!(
        flac::frames(&silent)
            .iter()
            .map(|f| (f.offset, f.len))
            .collect::<Vec<_>>(),
        truth.iter().map(|f| (f.offset, f.len)).collect::<Vec<_>>(),
        "no bound written is the whole file as the bound"
    );
    // The thing that must not happen: one frame credited with the rest of
    // the file. A frame is never longer than the longest one really is.
    assert!(
        flac::frames(&understated).iter().all(|f| f.len <= longest),
        "nothing swallowed the file"
    );
}

/// A tagger writes its tag onto a finished file. ID3v1 is 128 bytes, but
/// APEv2 and Lyrics3 are whatever length their contents are, and the last
/// frame has to be found anyway — every play would otherwise be short by a
/// block, and the station's clock short by that much on every track.
#[test]
fn a_tag_appended_after_the_audio_does_not_cost_the_last_frame() {
    let truth = flac::frames(REFERENCE);
    let same_as_truth = |bytes: &[u8], what: &str| {
        let found = flac::frames(bytes);
        assert_eq!(
            found.iter().map(|f| (f.offset, f.len)).collect::<Vec<_>>(),
            truth.iter().map(|f| (f.offset, f.len)).collect::<Vec<_>>(),
            "the audio ends where it ends, {what}"
        );
    };
    let with = |tail: &[u8]| {
        let mut bytes = REFERENCE.to_vec();
        bytes.extend_from_slice(tail);
        bytes
    };
    // An APEv2 tag: its footer is the last 32 bytes, its header the first.
    let mut ape = b"APETAGEX".to_vec();
    ape.extend_from_slice(&[0u8; 34]);
    same_as_truth(&with(&ape), "APEv2");
    same_as_truth(&with(b"LYRICSBEGIN and the words"), "Lyrics3");
    // An ID3v2 tag at the end of the file, which the tag's own spec allows.
    same_as_truth(&with(b"ID3\x04\x00\x00\x00\x00\x00\x0a0123456789"), "ID3v2");
    // Zeros, which some writers pad with. This CRC stays put while it is
    // fed zeros, so the end of the file agrees with the check as surely as
    // the end of the audio does — a megabyte of padding must not be sent
    // as if it were the last 21 milliseconds of the song.
    same_as_truth(&with(&[0u8; 77]), "padding");
    same_as_truth(&with(&vec![0u8; 1 << 20]), "a megabyte of padding");
    // ID3v1 again, the one that already worked.
    let mut id3v1 = b"TAG".to_vec();
    id3v1.extend_from_slice(&[0u8; 125]);
    same_as_truth(&with(&id3v1), "ID3v1");

    // Bytes that are not a tag anybody writes are not taken for audio
    // either: the last frame is left out rather than sent with junk on the
    // end of it. Honest, and never more than the file.
    let junk = with(b"\x01\x02\x03\x04\x05");
    let found = flac::frames(&junk);
    assert!(found.len() < truth.len(), "{found:?}");
    assert!(found
        .iter()
        .all(|f| f.offset + f.len <= REFERENCE.len() - 1));
}
