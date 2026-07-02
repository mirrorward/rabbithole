//! QWKE extensions: the `DOOR.ID` advertisement and long To/From/Subject
//! kludges.
//!
//! Base QWK hard-truncates To/From/Subject to 25 bytes in the message header.
//! **QWKE** lifts that limit by carrying the full values as kludge lines at the
//! very start of the message body:
//!
//! ```text
//! To: a-very-long-recipient-name-that-exceeds-25-characters
//! From: an-equally-long-sender-name
//! Subject: a subject line far longer than the 25-byte header field allows
//! <blank line>
//! ...the real message text...
//! ```
//!
//! Support is advertised to the offline reader in `DOOR.ID`, a file of
//! `KEY = VALUE` lines. This module parses and emits both the `DOOR.ID`
//! key/value form and the body kludges, in both directions.

use crate::text::decode_latin1;

/// The three QWKE long-field kludge tags, in canonical emit order.
const TAGS: [&str; 3] = ["To:", "From:", "Subject:"];

/// Long To/From/Subject values extracted from (or to be written into) a message
/// body as QWKE kludge lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QwkeKludges {
    /// Full recipient, if a `To:` kludge was present.
    pub to: Option<String>,
    /// Full sender, if a `From:` kludge was present.
    pub from: Option<String>,
    /// Full subject, if a `Subject:` kludge was present.
    pub subject: Option<String>,
}

impl QwkeKludges {
    /// `true` when no kludge fields are set.
    pub fn is_empty(&self) -> bool {
        self.to.is_none() && self.from.is_none() && self.subject.is_none()
    }
}

/// Split leading QWKE kludge lines off the front of a (normalized `\n`) body.
///
/// Consumes a contiguous run of `To:` / `From:` / `Subject:` lines at the very
/// top of the body, plus a single optional blank separator line after them.
/// Returns the parsed kludges and the remaining body text. If the body does not
/// begin with a kludge line, the body is returned unchanged and the kludges are
/// empty. Never panics.
pub fn parse_kludges(body: &str) -> (QwkeKludges, String) {
    let mut kludges = QwkeKludges::default();
    let mut rest = body;
    let mut matched_any = false;

    loop {
        let (line, tail) = split_first_line(rest);
        match match_tag(line) {
            Some((tag, value)) => {
                match tag {
                    "To:" => kludges.to = Some(value.to_string()),
                    "From:" => kludges.from = Some(value.to_string()),
                    "Subject:" => kludges.subject = Some(value.to_string()),
                    _ => unreachable!("match_tag only returns known tags"),
                }
                matched_any = true;
                rest = tail;
            }
            None => break,
        }
    }

    if matched_any {
        // Swallow a single blank separator line between kludges and body.
        let (line, tail) = split_first_line(rest);
        if line.is_empty() {
            rest = tail;
        }
    }

    (kludges, rest.to_string())
}

/// Prepend QWKE kludge lines to a body. Set fields are emitted in `To:`,
/// `From:`, `Subject:` order, followed by a blank separator line, then `body`.
/// If no fields are set, `body` is returned unchanged.
pub fn prepend_kludges(kludges: &QwkeKludges, body: &str) -> String {
    if kludges.is_empty() {
        return body.to_string();
    }
    let mut out = String::new();
    if let Some(v) = &kludges.to {
        out.push_str("To: ");
        out.push_str(v);
        out.push('\n');
    }
    if let Some(v) = &kludges.from {
        out.push_str("From: ");
        out.push_str(v);
        out.push('\n');
    }
    if let Some(v) = &kludges.subject {
        out.push_str("Subject: ");
        out.push_str(v);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(body);
    out
}

/// Split `s` into (first line without its `\n`, remainder after the `\n`).
fn split_first_line(s: &str) -> (&str, &str) {
    match s.find('\n') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    }
}

/// If `line` begins with a known kludge tag, return `(tag, trimmed_value)`.
fn match_tag(line: &str) -> Option<(&'static str, &str)> {
    for tag in TAGS {
        if let Some(value) = line.strip_prefix(tag) {
            return Some((tag, value.trim_start()));
        }
    }
    None
}

/// A parsed `DOOR.ID` file: order-preserving `KEY = VALUE` entries.
///
/// `DOOR.ID` advertises the mail door and the extensions it supports (QWKE among
/// them). Keys are matched case-insensitively; values are preserved verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoorId {
    /// Ordered `(key, value)` entries. A flag line with no `=` stores an empty
    /// value.
    pub entries: Vec<(String, String)>,
}

impl DoorId {
    /// An empty `DOOR.ID`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a QWKE-advertising `DOOR.ID` for the given door/version/system,
    /// including an explicit `CONTROLTYPE = ADD` line and a `QWKE` support flag.
    ///
    /// The support flag is what [`DoorId::advertises_qwke`] detects; some doors
    /// signal QWKE via other feature lines, and the detector is lenient about
    /// where the `QWKE` token appears.
    pub fn qwke(door: &str, version: &str, system: &str) -> Self {
        Self {
            entries: vec![
                ("DOOR".into(), door.into()),
                ("VERSION".into(), version.into()),
                ("SYSTEM".into(), system.into()),
                ("CONTROLTYPE".into(), "ADD".into()),
                ("CONTROLNAME".into(), "QWKE".into()),
            ],
        }
    }

    /// Case-insensitive lookup of the first value for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// Append or replace the first entry for `key` (case-insensitive).
    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
        {
            slot.1 = value.to_string();
        } else {
            self.entries.push((key.to_string(), value.to_string()));
        }
    }

    /// `true` if any entry carries the `QWKE` token in its key or value,
    /// signalling QWKE support.
    pub fn advertises_qwke(&self) -> bool {
        self.entries.iter().any(|(k, v)| {
            k.to_ascii_uppercase().contains("QWKE") || v.to_ascii_uppercase().contains("QWKE")
        })
    }

    /// Parse `DOOR.ID` text. Blank lines are ignored; `KEY = VALUE` lines split
    /// on the first `=`; a line with no `=` becomes a flag entry with an empty
    /// value. Total — never fails.
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw).trim();
            if line.is_empty() {
                continue;
            }
            match line.split_once('=') {
                Some((k, v)) => entries.push((k.trim().to_string(), v.trim().to_string())),
                None => entries.push((line.to_string(), String::new())),
            }
        }
        Self { entries }
    }

    /// Parse from raw bytes (Latin-1 decoded first).
    pub fn parse_bytes(bytes: &[u8]) -> Self {
        Self::parse(&decode_latin1(bytes))
    }

    /// Render to CRLF-terminated `DOOR.ID` text. Entries with an empty value are
    /// written as bare flag lines.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.entries {
            if v.is_empty() {
                out.push_str(k);
            } else {
                out.push_str(k);
                out.push_str(" = ");
                out.push_str(v);
            }
            out.push_str("\r\n");
        }
        out
    }

    /// Render to CRLF-terminated bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.render().into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn door_id_round_trip() {
        let door = DoorId::qwke("RabbitHole", "1.0", "RabbitHole BBS v0.4");
        let bytes = door.to_bytes();
        let back = DoorId::parse_bytes(&bytes);
        assert_eq!(back, door);
    }

    #[test]
    fn door_id_advertises_qwke() {
        let door = DoorId::qwke("RabbitHole", "1.0", "sys");
        assert!(door.advertises_qwke());
        assert_eq!(door.get("DOOR"), Some("RabbitHole"));
        assert_eq!(door.get("version"), Some("1.0")); // case-insensitive
    }

    #[test]
    fn door_id_without_qwke_not_detected() {
        let text = "DOOR = Generic\r\nVERSION = 2\r\n";
        assert!(!DoorId::parse(text).advertises_qwke());
    }

    #[test]
    fn door_id_flag_lines_and_no_spaces() {
        let door = DoorId::parse("RECEIPT\r\nDOOR=Tight\r\n");
        assert_eq!(door.entries[0], ("RECEIPT".to_string(), String::new()));
        assert_eq!(door.entries[1], ("DOOR".to_string(), "Tight".to_string()));
        // Flag lines re-render without an `=`.
        assert!(door.render().starts_with("RECEIPT\r\n"));
    }

    #[test]
    fn door_id_set_replaces_and_appends() {
        let mut door = DoorId::new();
        door.set("DOOR", "A");
        door.set("door", "B"); // replaces, case-insensitive
        door.set("VERSION", "9");
        assert_eq!(door.get("DOOR"), Some("B"));
        assert_eq!(door.entries.len(), 2);
    }

    #[test]
    fn kludges_round_trip() {
        let kludges = QwkeKludges {
            to: Some("A really long recipient name over twenty five chars".into()),
            from: Some("An equally verbose sender identity".into()),
            subject: Some("A subject longer than the 25-byte QWK header field permits".into()),
        };
        let body = "First body line\nSecond body line";
        let combined = prepend_kludges(&kludges, body);
        let (parsed, rest) = parse_kludges(&combined);
        assert_eq!(parsed, kludges);
        assert_eq!(rest, body);
    }

    #[test]
    fn kludges_partial_subset() {
        let kludges = QwkeKludges {
            to: None,
            from: None,
            subject: Some("Only a subject".into()),
        };
        let combined = prepend_kludges(&kludges, "hello");
        let (parsed, rest) = parse_kludges(&combined);
        assert_eq!(parsed, kludges);
        assert_eq!(rest, "hello");
    }

    #[test]
    fn body_without_kludges_is_untouched() {
        let body = "Just a normal message\nno kludges here";
        let (parsed, rest) = parse_kludges(body);
        assert!(parsed.is_empty());
        assert_eq!(rest, body);
    }

    #[test]
    fn empty_kludges_dont_alter_body() {
        let body = "text";
        assert_eq!(prepend_kludges(&QwkeKludges::default(), body), body);
    }

    #[test]
    fn parse_never_panics_on_odd_input() {
        for s in [
            "",
            "\n\n\n",
            "To:",
            "To:\n",
            "Subject: x",
            "\r\n=\r\n== = =",
        ] {
            let _ = parse_kludges(s);
            let _ = DoorId::parse(s);
        }
    }
}
