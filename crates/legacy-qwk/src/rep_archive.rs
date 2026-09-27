//! Bounded, in-memory classic ZIP reader for REP uploads. No paths are extracted.
//!
//! PKWARE APPNOTE 6.3.10 §§4.3.7, 4.3.9, 4.3.12, 4.3.16 and 4.4:
//! <https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT>.
//! Supports STORE and raw DEFLATE, including signed/unsigned data descriptors.
//! ZIP64, encryption, split archives, executable prefixes and nested paths are
//! deliberately unsupported. Errors never echo untrusted member names.

use crate::{crc32, ReplyPacket};
use std::collections::HashSet;

pub const MAX_ARCHIVE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MEMBER_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 32;
pub const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_REPLIES: usize = 1000;

#[derive(Debug, thiserror::Error)]
#[error("invalid REP archive: {0}")]
pub struct ArchiveError(pub &'static str);
type Result<T> = std::result::Result<T, ArchiveError>;
fn require(ok: bool, why: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(ArchiveError(why))
    }
}
fn bytes(b: &[u8], p: usize, n: usize) -> Result<&[u8]> {
    b.get(p..p.checked_add(n).ok_or(ArchiveError("offset overflow"))?)
        .ok_or(ArchiveError("truncated ZIP record"))
}
fn u16_at(b: &[u8], p: usize) -> Result<usize> {
    let s = bytes(b, p, 2)?;
    Ok(u16::from_le_bytes([s[0], s[1]]) as usize)
}
fn u32_at(b: &[u8], p: usize) -> Result<u32> {
    let s = bytes(b, p, 4)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn extra(b: &[u8]) -> Result<()> {
    let mut p = 0;
    while p < b.len() {
        require(u16_at(b, p)? != 1, "ZIP64 is unsupported")?;
        let n = u16_at(b, p + 2)?;
        bytes(b, p + 4, n)?;
        p += 4 + n;
    }
    Ok(())
}

/// Extract and parse the unique, case-insensitive `<bbs_id>.MSG` member.
/// Both its filename and its QWK header must match the exported BBS identity.
pub fn parse(b: &[u8], bbs_id: &str) -> Result<ReplyPacket> {
    require(b.len() <= MAX_ARCHIVE_BYTES, "archive too large")?;
    require(
        !bbs_id.is_empty()
            && bbs_id.len() <= 8
            && bbs_id.bytes().all(|c| c.is_ascii_alphanumeric()),
        "invalid BBS identity",
    )?;
    let start = b.len().saturating_sub(22 + 65535);
    let end = (start..b.len().saturating_sub(21))
        .rev()
        .find(|&p| {
            b.get(p..p + 4) == Some(&[0x50, 0x4b, 5, 6])
                && u16_at(b, p + 20).is_ok_and(|n| p + 22 + n == b.len())
        })
        .ok_or(ArchiveError("missing end record"))?;
    require(
        u16_at(b, end + 4)? == 0 && u16_at(b, end + 6)? == 0,
        "split archives are unsupported",
    )?;
    let count = u16_at(b, end + 10)?;
    require(
        count > 0 && count <= MAX_ENTRIES && u16_at(b, end + 8)? == count,
        "entry count out of bounds",
    )?;
    let central_len = u32_at(b, end + 12)? as usize;
    let central = u32_at(b, end + 16)? as usize;
    require(
        central <= b.len() && central.checked_add(central_len) == Some(end),
        "invalid central directory bounds",
    )?;
    let expected = format!("{bbs_id}.MSG");
    let mut names = HashSet::new();
    let mut ranges = Vec::new();
    let mut total = 0usize;
    let mut member = None;
    let mut p = central;
    for _ in 0..count {
        require(u32_at(b, p)? == 0x02014b50, "invalid central directory")?;
        bytes(b, p, 46)?;
        require(u16_at(b, p + 6)? <= 20, "unsupported ZIP version")?;
        let flags = u16_at(b, p + 8)?;
        require(
            flags & !(0x800 | 8 | 6) == 0,
            "unsupported ZIP flags or encryption",
        )?;
        let method = u16_at(b, p + 10)?;
        require(method == 0 || method == 8, "unsupported compression method")?;
        require(method == 8 || flags & 6 == 0, "invalid STORE flags")?;
        let crc = u32_at(b, p + 16)?;
        let compressed = u32_at(b, p + 20)? as usize;
        let size = u32_at(b, p + 24)? as usize;
        require(
            compressed <= MAX_ARCHIVE_BYTES && size <= MAX_MEMBER_BYTES,
            "member too large",
        )?;
        total = total
            .checked_add(size)
            .ok_or(ArchiveError("size overflow"))?;
        require(total <= MAX_TOTAL_BYTES, "total output too large")?;
        let name_len = u16_at(b, p + 28)?;
        let extra_len = u16_at(b, p + 30)?;
        let comment = u16_at(b, p + 32)?;
        require(u16_at(b, p + 34)? == 0, "split archives are unsupported")?;
        let attrs = u32_at(b, p + 38)?;
        require(
            attrs & 0x10 == 0 && matches!((attrs >> 16) & 0xf000, 0 | 0x8000),
            "non-regular member",
        )?;
        let local = u32_at(b, p + 42)? as usize;
        require(local <= central, "local offset out of bounds")?;
        let name = bytes(b, p + 46, name_len)?;
        require(
            !name.is_empty()
                && name.len() <= 255
                && name
                    .iter()
                    .all(|c| c.is_ascii_graphic() && !b"/\\:".contains(c))
                && name != b"."
                && name != b"..",
            "unsafe member name",
        )?;
        require(
            names.insert(name.to_ascii_lowercase()),
            "duplicate member name",
        )?;
        extra(bytes(b, p + 46 + name_len, extra_len)?)?;
        p = p
            .checked_add(46 + name_len + extra_len + comment)
            .ok_or(ArchiveError("offset overflow"))?;
        require(p <= end, "central record exceeds directory")?;
        require(u32_at(b, local)? == 0x04034b50, "invalid local header")?;
        bytes(b, local, 30)?;
        require(
            u16_at(b, local + 4)? <= 20
                && u16_at(b, local + 6)? == flags
                && u16_at(b, local + 8)? == method,
            "local header mismatch",
        )?;
        let ln = u16_at(b, local + 26)?;
        let le = u16_at(b, local + 28)?;
        require(bytes(b, local + 30, ln)? == name, "local filename mismatch")?;
        extra(bytes(b, local + 30 + ln, le)?)?;
        let data = local
            .checked_add(30 + ln + le)
            .ok_or(ArchiveError("offset overflow"))?;
        let payload = bytes(b, data, compressed)?;
        let mut stop = data + compressed;
        if flags & 8 == 0 {
            require(
                u32_at(b, local + 14)? == crc
                    && u32_at(b, local + 18)? as usize == compressed
                    && u32_at(b, local + 22)? as usize == size,
                "local size or CRC mismatch",
            )?;
        } else {
            require(
                (u32_at(b, local + 14)? == 0 || u32_at(b, local + 14)? == crc)
                    && (u32_at(b, local + 18)? == 0
                        || u32_at(b, local + 18)? as usize == compressed)
                    && (u32_at(b, local + 22)? == 0 || u32_at(b, local + 22)? as usize == size),
                "local descriptor mismatch",
            )?;
            // A CRC equal to the optional signature is ambiguous; validate
            // both complete layouts instead of treating the CRC as a marker.
            let matches = |q| {
                u32_at(b, q).ok() == Some(crc)
                    && u32_at(b, q + 4).ok() == Some(compressed as u32)
                    && u32_at(b, q + 8).ok() == Some(size as u32)
            };
            if u32_at(b, stop)? == 0x08074b50 && matches(stop + 4) {
                stop += 16;
            } else {
                require(matches(stop), "invalid data descriptor")?;
                stop += 12;
            }
        }
        require(stop <= central, "member overlaps central directory")?;
        ranges.push((local, stop));
        if name.eq_ignore_ascii_case(expected.as_bytes()) {
            let decoded = if method == 0 {
                require(compressed == size, "invalid STORE size")?;
                payload.to_vec()
            } else {
                let mut decoder = flate2::Decompress::new(false);
                let mut out = vec![0; size + 1];
                let status = decoder
                    .decompress(payload, &mut out, flate2::FlushDecompress::Finish)
                    .map_err(|_| ArchiveError("invalid DEFLATE stream"))?;
                require(
                    status == flate2::Status::StreamEnd && decoder.total_in() == compressed as u64,
                    "incomplete or trailing DEFLATE data",
                )?;
                out.truncate(decoder.total_out() as usize);
                out
            };
            require(decoded.len() == size, "decoded size mismatch")?;
            require(crc32(&decoded) == crc, "CRC mismatch")?;
            member = Some(decoded);
        }
    }
    require(p == end, "central directory count mismatch")?;
    ranges.sort_unstable();
    let mut cursor = 0;
    for (begin, end) in ranges {
        require(begin == cursor, "overlapping or unaccounted ZIP data")?;
        cursor = end;
    }
    require(cursor == central, "unaccounted ZIP data")?;
    let member = member.ok_or(ArchiveError("expected BBS message member missing"))?;
    // Check the record count before allocating decoded messages.
    let mut offset = 128;
    let mut count = 0;
    while offset < member.len() {
        let field = bytes(&member, offset + 116, 6)?;
        let n = std::str::from_utf8(field)
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .ok_or(ArchiveError("invalid message block count"))?;
        offset = offset
            .checked_add(
                n.checked_mul(128)
                    .ok_or(ArchiveError("message size overflow"))?,
            )
            .ok_or(ArchiveError("message size overflow"))?;
        require(offset <= member.len(), "truncated message")?;
        count += 1;
        require(count <= MAX_REPLIES, "too many replies")?;
    }
    let packet = ReplyPacket::parse(&member).map_err(|_| ArchiveError("invalid message member"))?;
    require(
        packet.header.eq_ignore_ascii_case(bbs_id),
        "BBS header mismatch",
    )?;
    Ok(packet)
}
