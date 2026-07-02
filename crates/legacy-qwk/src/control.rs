//! `CONTROL.DAT` codec: the line-oriented ASCII packet manifest.
//!
//! `CONTROL.DAT` is CRLF-terminated text describing the BBS, the target user,
//! and the conferences carried by the packet. Line order (as written by this
//! codec):
//!
//! ```text
//!  1  BBS name
//!  2  BBS city / state
//!  3  BBS phone number
//!  4  sysop name
//!  5  <mailbox serial>,<BBS id>
//!  6  packet creation date/time   (e.g. "07-02-2026,13:45:00")
//!  7  user name (the packet's owner)
//!  8  0                            (reserved / menu placeholder)
//!  9  0                            (reserved)
//! 10  total number of messages in the packet
//! 11  highest conference index  (== conference count - 1)
//! ── then, per conference, two lines:  <number>  /  <name>
//! ── then, one per line: welcome / news / goodbye screen filenames
//! ```
//!
//! Line 11 follows the classic QWK convention of storing the **highest
//! conference index** (one less than the number of conferences); the reader adds
//! one back. Encoding then decoding is exact for any packet with at least one
//! conference.

use crate::error::QwkError;
use crate::text::decode_latin1;

/// A decoded `CONTROL.DAT`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ControlDat {
    /// BBS name (line 1).
    pub bbs_name: String,
    /// BBS city and state (line 2).
    pub city_state: String,
    /// BBS phone number (line 3).
    pub phone: String,
    /// Sysop name (line 4).
    pub sysop: String,
    /// Mailbox / mail-door serial number (line 5, before the comma).
    pub serial: String,
    /// BBS id (line 5, after the comma).
    pub bbs_id: String,
    /// Packet creation date/time (line 6), stored verbatim.
    pub date: String,
    /// User name the packet is built for (line 7).
    pub username: String,
    /// Total number of messages in the packet (line 10).
    pub total_messages: u32,
    /// Conferences as `(number, name)` pairs, in file order.
    pub conferences: Vec<(u16, String)>,
    /// Trailing screen filenames (welcome / news / goodbye, etc.).
    pub files: Vec<String>,
}

impl ControlDat {
    /// Serialize to CRLF-terminated bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut lines: Vec<String> = Vec::new();
        lines.push(self.bbs_name.clone());
        lines.push(self.city_state.clone());
        lines.push(self.phone.clone());
        lines.push(self.sysop.clone());
        lines.push(format!("{},{}", self.serial, self.bbs_id));
        lines.push(self.date.clone());
        lines.push(self.username.clone());
        lines.push("0".to_string());
        lines.push("0".to_string());
        lines.push(self.total_messages.to_string());
        // Classic QWK stores the highest conference index (count - 1).
        lines.push(self.conferences.len().saturating_sub(1).to_string());
        for (num, name) in &self.conferences {
            lines.push(num.to_string());
            lines.push(name.clone());
        }
        for f in &self.files {
            lines.push(f.clone());
        }

        let mut out = String::new();
        for line in &lines {
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.into_bytes()
    }

    /// Parse `CONTROL.DAT` bytes. Tolerates both CRLF and bare LF line endings.
    ///
    /// Returns [`QwkError::ControlTruncated`] if a required line is missing and
    /// [`QwkError::ControlNotNumeric`] if a numeric line cannot be parsed. Never
    /// panics.
    pub fn parse(bytes: &[u8]) -> Result<Self, QwkError> {
        let text = decode_latin1(bytes);
        let lines: Vec<&str> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();

        let bbs_name = line(&lines, 0, "BBS name")?.to_string();
        let city_state = line(&lines, 1, "city/state")?.to_string();
        let phone = line(&lines, 2, "phone")?.to_string();
        let sysop = line(&lines, 3, "sysop")?.to_string();

        let serial_line = line(&lines, 4, "serial/BBS id")?;
        let (serial, bbs_id) = match serial_line.split_once(',') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (serial_line.to_string(), String::new()),
        };

        let date = line(&lines, 5, "date")?.to_string();
        let username = line(&lines, 6, "user name")?.to_string();
        // Lines 7 and 8 are the two reserved "0" placeholders; skip them.
        let total_field = line(&lines, 9, "total messages")?;
        let total_messages =
            total_field
                .trim()
                .parse()
                .map_err(|_| QwkError::ControlNotNumeric {
                    field: "total messages",
                    value: total_field.to_string(),
                })?;

        let conf_field = line(&lines, 10, "conference count")?;
        let highest: i64 = conf_field
            .trim()
            .parse()
            .map_err(|_| QwkError::ControlNotNumeric {
                field: "conference count",
                value: conf_field.to_string(),
            })?;
        let conf_count = if highest < 0 {
            0
        } else {
            (highest + 1) as usize
        };

        let mut conferences = Vec::with_capacity(conf_count);
        let mut idx = 11;
        for _ in 0..conf_count {
            let num_field = line(&lines, idx, "conference number")?;
            idx += 1;
            let name = line(&lines, idx, "conference name")?;
            idx += 1;
            let num = num_field
                .trim()
                .parse()
                .map_err(|_| QwkError::ControlNotNumeric {
                    field: "conference number",
                    value: num_field.to_string(),
                })?;
            conferences.push((num, name.to_string()));
        }

        let files = lines
            .get(idx..)
            .unwrap_or(&[])
            .iter()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();

        Ok(Self {
            bbs_name,
            city_state,
            phone,
            sysop,
            serial,
            bbs_id,
            date,
            username,
            total_messages,
            conferences,
            files,
        })
    }
}

/// Fetch line `i`, or a truncation error naming the missing `field`.
fn line<'a>(lines: &[&'a str], i: usize, field: &'static str) -> Result<&'a str, QwkError> {
    lines
        .get(i)
        .copied()
        .ok_or(QwkError::ControlTruncated { field })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ControlDat {
        ControlDat {
            bbs_name: "RabbitHole BBS".into(),
            city_state: "Portland, OR".into(),
            phone: "503-555-0100".into(),
            sysop: "KEVIN".into(),
            serial: "12345".into(),
            bbs_id: "RABBIT".into(),
            date: "07-02-2026,13:45:00".into(),
            username: "KEVIN".into(),
            total_messages: 42,
            conferences: vec![
                (0, "Main Board".into()),
                (1, "General Chat".into()),
                (5, "Rust Programming".into()),
            ],
            files: vec!["WELCOME".into(), "NEWS".into(), "GOODBYE".into()],
        }
    }

    #[test]
    fn round_trip() {
        let ctrl = sample();
        let bytes = ctrl.to_bytes();
        let back = ControlDat::parse(&bytes).unwrap();
        assert_eq!(back, ctrl);
    }

    #[test]
    fn uses_crlf_line_endings() {
        let bytes = sample().to_bytes();
        assert!(bytes.windows(2).any(|w| w == b"\r\n"));
    }

    #[test]
    fn highest_index_is_count_minus_one() {
        let text = String::from_utf8(sample().to_bytes()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // 3 conferences => line 11 (index 10) is "2".
        assert_eq!(lines[10], "2");
    }

    #[test]
    fn serial_and_bbs_id_split_on_comma() {
        let back = ControlDat::parse(&sample().to_bytes()).unwrap();
        assert_eq!(back.serial, "12345");
        assert_eq!(back.bbs_id, "RABBIT");
    }

    #[test]
    fn tolerates_bare_lf() {
        let mut ctrl = sample();
        ctrl.files.clear();
        let crlf = String::from_utf8(ctrl.to_bytes()).unwrap();
        let lf = crlf.replace("\r\n", "\n");
        let back = ControlDat::parse(lf.as_bytes()).unwrap();
        assert_eq!(back.conferences, ctrl.conferences);
    }

    #[test]
    fn truncated_input_errors_not_panics() {
        let full = sample().to_bytes();
        for n in 0..full.len() {
            let _ = ControlDat::parse(&full[..n]);
        }
    }

    #[test]
    fn non_numeric_total_is_reported() {
        // Lines 1..=9, then a non-numeric "total messages" line (line 10).
        let text =
            "BBS\r\nCity\r\nPhone\r\nSysop\r\n1,ID\r\ndate\r\nUSER\r\n0\r\n0\r\nnotanumber\r\n";
        assert!(matches!(
            ControlDat::parse(text.as_bytes()),
            Err(QwkError::ControlNotNumeric {
                field: "total messages",
                ..
            })
        ));
    }

    #[test]
    fn non_numeric_conference_count_is_reported() {
        let text =
            "BBS\r\nCity\r\nPhone\r\nSysop\r\n1,ID\r\ndate\r\nUSER\r\n0\r\n0\r\n5\r\nxyz\r\n";
        assert!(matches!(
            ControlDat::parse(text.as_bytes()),
            Err(QwkError::ControlNotNumeric {
                field: "conference count",
                ..
            })
        ));
    }
}
