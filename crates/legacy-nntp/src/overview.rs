//! The `OVER`/`XOVER` overview line model (RFC 3977 §8).
//!
//! An overview line is a tab-separated summary of one article, sent one per
//! line inside a `224` data block. The first field is the article number; the
//! remaining fields follow the standard `OVERVIEW.FMT` order:
//!
//! ```text
//! number \t Subject \t From \t Date \t Message-ID \t References \t :bytes \t :lines
//! ```
//!
//! For the mandatory fields the values appear without their header names (the
//! names live only in `LIST OVERVIEW.FMT`). This module provides [`Overview`]
//! plus [`Overview::encode`]/[`Overview::parse`] for the line body (no CRLF —
//! frame with [`crate::datablock`]) and the canonical [`OVERVIEW_FMT`] format
//! listing.
//!
//! Encoding scrubs tab/CR/LF out of free-text fields so a line can never gain a
//! spurious column, and parsing never panics on arbitrary input.

use crate::message_id::{MessageId, MessageIdError};

use thiserror::Error;

/// The standard `LIST OVERVIEW.FMT` field listing, in order.
///
/// The trailing `:bytes`/`:lines` entries are metadata fields (rendered as
/// their value only in overview data lines); the rest are header fields.
pub const OVERVIEW_FMT: [&str; 7] = [
    "Subject:",
    "From:",
    "Date:",
    "Message-ID:",
    "References:",
    ":bytes",
    ":lines",
];

/// A parsed overview record for a single article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    /// Article number within its group.
    pub number: u64,
    /// `Subject` header value.
    pub subject: String,
    /// `From` header value.
    pub from: String,
    /// `Date` header value (kept as text).
    pub date: String,
    /// `Message-ID` header value.
    pub message_id: MessageId,
    /// `References` header, split into individual message-ids (possibly empty).
    pub references: Vec<MessageId>,
    /// Byte count of the article (the `:bytes` metadata field).
    pub bytes: u64,
    /// Line count of the article body (the `:lines` metadata field).
    pub lines: u64,
}

/// Reasons an overview line could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OverviewError {
    /// The line did not have the expected eight tab-separated fields.
    #[error("expected 8 tab-separated fields, found {0}")]
    FieldCount(usize),
    /// The article number could not be parsed.
    #[error("invalid article number: {0:?}")]
    BadNumber(String),
    /// The `:bytes` field could not be parsed.
    #[error("invalid byte count: {0:?}")]
    BadBytes(String),
    /// The `:lines` field could not be parsed.
    #[error("invalid line count: {0:?}")]
    BadLines(String),
    /// The `Message-ID` field was malformed.
    #[error("invalid message-id: {0}")]
    MessageId(#[from] MessageIdError),
    /// A `References` entry was malformed.
    #[error("invalid reference: {0}")]
    Reference(MessageIdError),
}

/// Render the standard `OVERVIEW.FMT` reply as a framed data block.
#[must_use]
pub fn overview_fmt_block() -> String {
    crate::datablock::encode_lines(&OVERVIEW_FMT)
}

/// Replace tab, CR, and LF with spaces so a free-text field stays one column.
fn scrub(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\t' || c == '\r' || c == '\n' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

impl Overview {
    /// Encode this record as one tab-separated overview line (no CRLF).
    ///
    /// Free-text fields have any tab/CR/LF replaced with spaces.
    #[must_use]
    pub fn encode(&self) -> String {
        let refs = self
            .references
            .iter()
            .map(MessageId::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.number,
            scrub(&self.subject),
            scrub(&self.from),
            scrub(&self.date),
            self.message_id.as_str(),
            refs,
            self.bytes,
            self.lines,
        )
    }

    /// Parse one overview line (a single line, CRLF already stripped).
    ///
    /// # Errors
    ///
    /// Returns [`OverviewError`] on the wrong field count or an unparseable
    /// field. Never panics on arbitrary input.
    pub fn parse(line: &str) -> Result<Overview, OverviewError> {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 8 {
            return Err(OverviewError::FieldCount(fields.len()));
        }
        let number = fields[0]
            .parse::<u64>()
            .map_err(|_| OverviewError::BadNumber(fields[0].to_string()))?;
        let subject = fields[1].to_string();
        let from = fields[2].to_string();
        let date = fields[3].to_string();
        let message_id = MessageId::new(fields[4])?;
        let references = if fields[5].trim().is_empty() {
            Vec::new()
        } else {
            fields[5]
                .split_whitespace()
                .map(|r| MessageId::new(r).map_err(OverviewError::Reference))
                .collect::<Result<Vec<_>, _>>()?
        };
        let bytes = fields[6]
            .parse::<u64>()
            .map_err(|_| OverviewError::BadBytes(fields[6].to_string()))?;
        let lines = fields[7]
            .parse::<u64>()
            .map_err(|_| OverviewError::BadLines(fields[7].to_string()))?;
        Ok(Overview {
            number,
            subject,
            from,
            date,
            message_id,
            references,
            bytes,
            lines,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Overview {
        Overview {
            number: 3000,
            subject: "Re: warren dig progress".to_string(),
            from: "Kevin <kevin@phunc.com>".to_string(),
            date: "Wed, 01 Jul 2026 12:00:00 +0000".to_string(),
            message_id: MessageId::new("<abc@news.example.com>").unwrap(),
            references: vec![
                MessageId::new("<root@news.example.com>").unwrap(),
                MessageId::new("<parent@news.example.com>").unwrap(),
            ],
            bytes: 4321,
            lines: 42,
        }
    }

    #[test]
    fn encodes_expected_layout() {
        let wire = sample().encode();
        let cols: Vec<&str> = wire.split('\t').collect();
        assert_eq!(cols.len(), 8);
        assert_eq!(cols[0], "3000");
        assert_eq!(cols[4], "<abc@news.example.com>");
        assert_eq!(cols[5], "<root@news.example.com> <parent@news.example.com>");
        assert_eq!(cols[6], "4321");
        assert_eq!(cols[7], "42");
    }

    #[test]
    fn round_trips() {
        let ov = sample();
        let parsed = Overview::parse(&ov.encode()).unwrap();
        assert_eq!(parsed, ov);
    }

    #[test]
    fn empty_references_round_trip() {
        let mut ov = sample();
        ov.references.clear();
        let parsed = Overview::parse(&ov.encode()).unwrap();
        assert_eq!(parsed, ov);
        assert!(parsed.references.is_empty());
    }

    #[test]
    fn scrubs_tabs_in_free_text() {
        let mut ov = sample();
        ov.subject = "has\ta\ttab".to_string();
        let wire = ov.encode();
        assert_eq!(wire.split('\t').count(), 8);
        assert!(wire.contains("has a tab"));
    }

    #[test]
    fn wrong_field_count_errors() {
        assert_eq!(
            Overview::parse("just\tthree\tfields"),
            Err(OverviewError::FieldCount(3))
        );
    }

    #[test]
    fn bad_number_errors() {
        let line = "notnum\ts\tf\td\t<a@b>\t\t1\t2";
        assert_eq!(
            Overview::parse(line),
            Err(OverviewError::BadNumber("notnum".to_string()))
        );
    }

    #[test]
    fn bad_message_id_errors() {
        let line = "1\ts\tf\td\tbroken\t\t1\t2";
        assert!(matches!(
            Overview::parse(line),
            Err(OverviewError::MessageId(_))
        ));
    }

    #[test]
    fn overview_fmt_block_is_framed() {
        let block = overview_fmt_block();
        assert!(block.starts_with("Subject:\r\n"));
        assert!(block.ends_with(":lines\r\n.\r\n"));
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for probe in ["", "\t\t\t\t\t\t\t", "\0", "1\t2\t3\t4\t5\t6\t7\t8\t9"] {
            let _ = Overview::parse(probe);
        }
    }
}
