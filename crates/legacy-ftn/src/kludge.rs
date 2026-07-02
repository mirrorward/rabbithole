//! Kludge / control-line parsing within a message body, and the [`Message`]
//! model that layers over a packed record's raw body bytes.
//!
//! A FidoNet message body is a stream of CR-separated (`\r`, 0x0D) lines. Some
//! lines are machine-readable *control* lines rather than visible text:
//!
//! ```text
//!   AREA:ECHOTAG                     echomail area tag (first line, no SOH)
//!   \x01INTL 1:104/1 2:280/464       netmail routing (SOH-prefixed "kludge")
//!   \x01FMPT 7                        origin point
//!   \x01TOPT 3                        destination point
//!   \x01MSGID: 2:280/464 4d5e6f70     globally-unique id
//!   \x01REPLY: 2:280/464 1a2b3c4d     id being replied to
//!   \x01PID: GoldED+/LNX 1.1.5        producer (writer) id
//!   \x01TID: hpt/lnx 1.9              transport (tosser) id
//!   ...visible message text...
//!   --- GoldED+/LNX 1.1.5            tearline (three dashes)
//!    * Origin: The Board (2:280/464) origin line (leading space, asterisk)
//!   SEEN-BY: 280/464 464/1           echomail dupe/loop control (no SOH)
//!   \x01PATH: 280/464                 echomail routing history (SOH-prefixed)
//! ```
//!
//! `\x01` is the SOH control byte (Ctrl-A); such lines are invisible to
//! readers. [`Message::parse`] splits a raw body into these categories plus the
//! visible text (kept as **raw CP437 bytes**), and [`Message::serialize`]
//! re-emits them in a deterministic canonical order. Serialization normalizes
//! insignificant whitespace (e.g. the single space after a tearline's dashes),
//! so parse→serialize is idempotent on the canonical form.

use crate::cp437;

const SOH: u8 = 0x01;
const CR: u8 = 0x0D;

/// A parsed message: control lines separated from visible text.
///
/// The visible [`text`](Message::text) is stored as raw CP437 bytes so the
/// codec never forces a lossy Unicode round-trip; use [`Message::text_str`] to
/// decode it. Control-line values are ASCII and stored as `String`s.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    /// Echomail area tag from the `AREA:` line, if present (netmail has none).
    pub area: Option<String>,
    /// SOH-prefixed control lines (INTL/FMPT/TOPT/MSGID/REPLY/PID/TID/…),
    /// stored as raw content (without the SOH or trailing CR), in order.
    /// `PATH` lines are kept separately in [`path`](Message::path).
    pub kludges: Vec<String>,
    /// Visible message text as raw CP437 bytes (CR-separated lines).
    pub text: Vec<u8>,
    /// Tearline tagger (the text after `--- `), if a tearline is present.
    pub tearline: Option<String>,
    /// Origin line content (the text after ` * Origin: `), if present.
    pub origin: Option<String>,
    /// `SEEN-BY:` node lists, one entry per line, in order.
    pub seen_by: Vec<String>,
    /// `\x01PATH:` node lists, one entry per line, in order.
    pub path: Vec<String>,
}

impl Message {
    /// Decode the visible text to a Unicode string via CP437, preserving line
    /// breaks: `\r` (0x0D) and `\n` (0x0A) are kept as line-break characters
    /// rather than mapped to their CP437 glyphs (♪ / ◙), since in a message
    /// body they are structure, not content.
    pub fn text_str(&self) -> String {
        self.text
            .iter()
            .map(|&b| match b {
                CR => '\r',
                b'\n' => '\n',
                other => cp437::CP437_TO_UNICODE[other as usize],
            })
            .collect()
    }

    /// Look up the value of the first kludge whose tag matches `tag`
    /// (case-insensitive). The tag is the leading token before a space or
    /// colon; the value is the remainder with a leading `:`/spaces trimmed.
    pub fn kludge_value(&self, tag: &str) -> Option<&str> {
        self.kludges.iter().find_map(|k| {
            let (t, v) = split_kludge(k);
            if t.eq_ignore_ascii_case(tag) {
                Some(v)
            } else {
                None
            }
        })
    }

    /// Value of the `MSGID` kludge, if any.
    pub fn msgid(&self) -> Option<&str> {
        self.kludge_value("MSGID")
    }

    /// Value of the `REPLY` kludge, if any.
    pub fn reply(&self) -> Option<&str> {
        self.kludge_value("REPLY")
    }

    /// Value of the `INTL` kludge, if any.
    pub fn intl(&self) -> Option<&str> {
        self.kludge_value("INTL")
    }

    /// Value of the `PID` kludge, if any.
    pub fn pid(&self) -> Option<&str> {
        self.kludge_value("PID")
    }

    /// Value of the `TID` kludge, if any.
    pub fn tid(&self) -> Option<&str> {
        self.kludge_value("TID")
    }

    /// Value of the `FMPT` (origin point) kludge, if any.
    pub fn fmpt(&self) -> Option<&str> {
        self.kludge_value("FMPT")
    }

    /// Value of the `TOPT` (destination point) kludge, if any.
    pub fn topt(&self) -> Option<&str> {
        self.kludge_value("TOPT")
    }

    /// Parse a raw message body into its parts. Total: never panics.
    pub fn parse(body: &[u8]) -> Message {
        let mut msg = Message::default();
        let mut text_lines: Vec<&[u8]> = Vec::new();

        let mut segs: Vec<&[u8]> = body.split(|&b| b == CR).collect();
        // A trailing CR yields a spurious empty final segment; drop it.
        if segs.last().is_some_and(|s| s.is_empty()) {
            segs.pop();
        }

        for seg in segs {
            let seg = strip_leading_lf(seg);
            if seg.first() == Some(&SOH) {
                let content = cp437::decode(&seg[1..]);
                if let Some(rest) = content.strip_prefix("PATH:") {
                    msg.path.push(rest.trim_start().to_string());
                } else {
                    msg.kludges.push(content);
                }
                continue;
            }

            if seg.starts_with(b"AREA:") {
                msg.area = Some(cp437::decode(&seg[b"AREA:".len()..]));
            } else if seg.starts_with(b"SEEN-BY:") {
                let rest = cp437::decode(&seg[b"SEEN-BY:".len()..]);
                msg.seen_by.push(rest.trim_start().to_string());
            } else if seg.starts_with(b" * Origin: ") {
                msg.origin = Some(cp437::decode(&seg[b" * Origin: ".len()..]));
            } else if seg.starts_with(b"---") {
                let rest = cp437::decode(&seg[3..]);
                msg.tearline = Some(rest.trim_start().to_string());
            } else {
                text_lines.push(seg);
            }
        }

        let mut text = Vec::new();
        for (i, line) in text_lines.iter().enumerate() {
            if i > 0 {
                text.push(CR);
            }
            text.extend_from_slice(line);
        }
        msg.text = text;
        msg
    }

    /// Serialize the message to raw body bytes in canonical order:
    /// `AREA:` → kludges → text → tearline → origin → `SEEN-BY:` → `PATH:`.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();

        if let Some(area) = &self.area {
            out.extend_from_slice(b"AREA:");
            out.extend_from_slice(&cp437::encode_lossy(area));
            out.push(CR);
        }

        for k in &self.kludges {
            out.push(SOH);
            out.extend_from_slice(&cp437::encode_lossy(k));
            out.push(CR);
        }

        out.extend_from_slice(&self.text);

        let has_trailer = self.tearline.is_some()
            || self.origin.is_some()
            || !self.seen_by.is_empty()
            || !self.path.is_empty();
        if !self.text.is_empty() && has_trailer {
            out.push(CR);
        }

        if let Some(t) = &self.tearline {
            out.extend_from_slice(b"---");
            if !t.is_empty() {
                out.push(b' ');
                out.extend_from_slice(&cp437::encode_lossy(t));
            }
            out.push(CR);
        }

        if let Some(o) = &self.origin {
            out.extend_from_slice(b" * Origin: ");
            out.extend_from_slice(&cp437::encode_lossy(o));
            out.push(CR);
        }

        for sb in &self.seen_by {
            out.extend_from_slice(b"SEEN-BY: ");
            out.extend_from_slice(&cp437::encode_lossy(sb));
            out.push(CR);
        }

        for p in &self.path {
            out.push(SOH);
            out.extend_from_slice(b"PATH: ");
            out.extend_from_slice(&cp437::encode_lossy(p));
            out.push(CR);
        }

        out
    }
}

fn strip_leading_lf(seg: &[u8]) -> &[u8] {
    if seg.first() == Some(&b'\n') {
        &seg[1..]
    } else {
        seg
    }
}

/// Split a raw kludge line into `(tag, value)`. The tag is the leading run of
/// non-space/non-colon characters; the value is the remainder with any leading
/// `:` and surrounding spaces trimmed.
fn split_kludge(raw: &str) -> (&str, &str) {
    let end = raw.find([' ', ':']).unwrap_or(raw.len());
    let tag = &raw[..end];
    let mut rest = &raw[end..];
    rest = rest.strip_prefix(':').unwrap_or(rest);
    (tag, rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_echomail() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"AREA:R20.GENERAL\r");
        b.extend_from_slice(b"\x01MSGID: 2:280/464 4d5e6f70\r");
        b.extend_from_slice(b"\x01REPLY: 2:280/464 1a2b3c4d\r");
        b.extend_from_slice(b"\x01PID: GoldED+/LNX 1.1.5\r");
        b.extend_from_slice(b"Hello, echo!\r");
        b.extend_from_slice(b"Second line.\r");
        b.extend_from_slice(b"--- GoldED+/LNX 1.1.5\r");
        b.extend_from_slice(b" * Origin: The Warren (2:280/464)\r");
        b.extend_from_slice(b"SEEN-BY: 280/464 464/1\r");
        b.extend_from_slice(b"\x01PATH: 280/464\r");
        b
    }

    #[test]
    fn parse_echomail_fields() {
        let m = Message::parse(&sample_echomail());
        assert_eq!(m.area.as_deref(), Some("R20.GENERAL"));
        assert_eq!(m.msgid(), Some("2:280/464 4d5e6f70"));
        assert_eq!(m.reply(), Some("2:280/464 1a2b3c4d"));
        assert_eq!(m.pid(), Some("GoldED+/LNX 1.1.5"));
        assert_eq!(m.text_str(), "Hello, echo!\rSecond line.");
        assert_eq!(m.tearline.as_deref(), Some("GoldED+/LNX 1.1.5"));
        assert_eq!(m.origin.as_deref(), Some("The Warren (2:280/464)"));
        assert_eq!(m.seen_by, vec!["280/464 464/1".to_string()]);
        assert_eq!(m.path, vec!["280/464".to_string()]);
    }

    #[test]
    fn roundtrip_canonical_body() {
        let body = sample_echomail();
        let m = Message::parse(&body);
        assert_eq!(m.serialize(), body);
    }

    #[test]
    fn roundtrip_via_model() {
        let m = Message {
            area: None,
            kludges: vec![
                "INTL 1:104/1 2:280/464".to_string(),
                "MSGID: 2:280/464 aabbccdd".to_string(),
            ],
            text: b"Netmail body\rline two".to_vec(),
            tearline: Some("Msged".to_string()),
            origin: None,
            seen_by: Vec::new(),
            path: Vec::new(),
        };
        let reparsed = Message::parse(&m.serialize());
        assert_eq!(reparsed, m);
        assert_eq!(m.intl(), Some("1:104/1 2:280/464"));
    }

    #[test]
    fn bare_tearline_roundtrips() {
        let m = Message {
            tearline: Some(String::new()),
            text: b"hi".to_vec(),
            ..Default::default()
        };
        assert_eq!(Message::parse(&m.serialize()), m);
    }

    #[test]
    fn text_bytes_are_lossless() {
        // A visible-text line may not begin with a control marker (SOH,
        // "---", "AREA:", …) or it would parse as a control line — that is
        // inherent to the on-wire grammar. Given a well-formed text line, all
        // byte values (including high CP437 bytes) survive the round-trip.
        let mut text = vec![b'X'];
        text.extend(0x80u8..=0xFF);
        let m = Message {
            text,
            ..Default::default()
        };
        assert_eq!(Message::parse(&m.serialize()).text, m.text);
    }

    #[test]
    fn parse_never_panics_on_junk() {
        for junk in [
            &b""[..],
            b"\x01",
            b"\x01\x01\x01",
            b"---",
            b"AREA:",
            b" * Origin: ",
            b"\r\r\r",
            &[0xffu8; 32],
        ] {
            let m = Message::parse(junk);
            // serialize/parse should not panic either
            let _ = Message::parse(&m.serialize());
        }
    }
}
