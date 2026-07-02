//! Peering list-line models and multi-line block builders.
//!
//! The transit and reader surfaces share a handful of tab/space-delimited
//! listing formats. This module gives each a small, validated line type with
//! matching `encode`/`parse`, plus helpers that frame a slice of them into a
//! ready-to-send data block (see [`crate::datablock`]):
//!
//! * [`ActiveGroup`] — one line of the active file, `name high low status`, as
//!   returned by `LIST ACTIVE` (`215`) and, in the same shape, by `NEWGROUPS`
//!   (`231`, RFC 3977 §7.3).
//! * [`ActiveTimesEntry`] — one line of `LIST ACTIVE.TIMES`,
//!   `name created creator` (RFC 3977 §7.6.4).
//! * [`NewsgroupDescription`] — one line of `LIST NEWSGROUPS`,
//!   `name<TAB>description` (RFC 3977 §7.6.6).
//! * [`new_articles_block`] — the `NEWNEWS` (`230`) body: one message-id per
//!   line (RFC 3977 §7.4).
//!
//! Free-text fields are scrubbed of the delimiters that would otherwise add a
//! spurious column, and every parser is total: arbitrary input yields a
//! [`ListingError`], never a panic.

use crate::datablock::encode_lines;
use crate::message_id::MessageId;

use thiserror::Error;

/// Reasons a listing line could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ListingError {
    /// The line did not split into the expected number of fields.
    #[error("expected {expected} fields, found {found}")]
    FieldCount {
        /// Number of fields the format requires.
        expected: usize,
        /// Number of fields actually present.
        found: usize,
    },
    /// A numeric field (article water mark or creation time) was unparseable.
    #[error("invalid number in field {field:?}: {value:?}")]
    BadNumber {
        /// Which field failed.
        field: &'static str,
        /// The offending text.
        value: String,
    },
    /// A required field was empty (e.g. an empty group name).
    #[error("empty {0} field")]
    Empty(&'static str),
}

/// Replace runs-breaking whitespace/control characters with a single space so a
/// free-text field cannot introduce a new column or split the line.
fn scrub_spaces(s: &str) -> String {
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

/// One active-file line: `name high low status` (RFC 3977 §6.1.1, §7.3).
///
/// `high`/`low` are the reported high and low water marks; `status` is the
/// posting-status flag (`y`, `n`, `m`, `x`, `j`, or `=other.group`). The same
/// shape is used by `NEWGROUPS` (`231`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveGroup {
    /// Newsgroup name.
    pub name: String,
    /// Reported high water mark (highest article number).
    pub high: u64,
    /// Reported low water mark (lowest article number).
    pub low: u64,
    /// Posting-status flag, kept verbatim.
    pub status: String,
}

impl ActiveGroup {
    /// Encode as one `name high low status` line (no CRLF).
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{} {} {} {}",
            scrub_spaces(&self.name),
            self.high,
            self.low,
            scrub_spaces(&self.status)
        )
    }

    /// Parse one active-file line (CRLF already stripped).
    ///
    /// # Errors
    ///
    /// [`ListingError`] on the wrong field count or an unparseable water mark.
    pub fn parse(line: &str) -> Result<ActiveGroup, ListingError> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 4 {
            return Err(ListingError::FieldCount {
                expected: 4,
                found: fields.len(),
            });
        }
        let high = fields[1]
            .parse::<u64>()
            .map_err(|_| ListingError::BadNumber {
                field: "high",
                value: fields[1].to_string(),
            })?;
        let low = fields[2]
            .parse::<u64>()
            .map_err(|_| ListingError::BadNumber {
                field: "low",
                value: fields[2].to_string(),
            })?;
        Ok(ActiveGroup {
            name: fields[0].to_string(),
            high,
            low,
            status: fields[3].to_string(),
        })
    }
}

/// One `LIST ACTIVE.TIMES` line: `name created creator` (RFC 3977 §7.6.4).
///
/// `created` is the group's creation time in seconds since the Unix epoch;
/// `creator` is free text (usually an email address).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTimesEntry {
    /// Newsgroup name.
    pub name: String,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Who created the group (free text).
    pub creator: String,
}

impl ActiveTimesEntry {
    /// Encode as one `name created creator` line (no CRLF).
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{} {} {}",
            scrub_spaces(&self.name),
            self.created,
            scrub_spaces(&self.creator)
        )
    }

    /// Parse one `LIST ACTIVE.TIMES` line (CRLF already stripped).
    ///
    /// The name and creation time are the first two whitespace-delimited
    /// tokens; everything after is the creator (which may contain spaces).
    ///
    /// # Errors
    ///
    /// [`ListingError`] if fewer than three fields are present or the creation
    /// time is not a number.
    pub fn parse(line: &str) -> Result<ActiveTimesEntry, ListingError> {
        let trimmed = line.trim();
        let mut parts = trimmed.splitn(3, char::is_whitespace);
        let name = parts.next().unwrap_or("");
        let created = parts.next();
        let creator = parts.next();
        match (name, created, creator) {
            (name, Some(created), Some(creator)) if !name.is_empty() => {
                let created = created
                    .parse::<u64>()
                    .map_err(|_| ListingError::BadNumber {
                        field: "created",
                        value: created.to_string(),
                    })?;
                Ok(ActiveTimesEntry {
                    name: name.to_string(),
                    created,
                    creator: creator.trim_start().to_string(),
                })
            }
            _ => {
                let found = trimmed.split_whitespace().count();
                Err(ListingError::FieldCount { expected: 3, found })
            }
        }
    }
}

/// One `LIST NEWSGROUPS` line: `name<TAB>description` (RFC 3977 §7.6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsgroupDescription {
    /// Newsgroup name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
}

impl NewsgroupDescription {
    /// Encode as a `name<TAB>description` line (no CRLF).
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{}\t{}",
            scrub_spaces(&self.name),
            scrub_spaces(&self.description)
        )
    }

    /// Parse one `LIST NEWSGROUPS` line (CRLF already stripped).
    ///
    /// The name is the first whitespace-delimited token; the description is the
    /// remainder with leading whitespace trimmed (tab or spaces are accepted as
    /// the separator, per common practice).
    ///
    /// # Errors
    ///
    /// [`ListingError::Empty`] if the name is missing.
    pub fn parse(line: &str) -> Result<NewsgroupDescription, ListingError> {
        let trimmed = line.trim_start();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("");
        if name.is_empty() {
            return Err(ListingError::Empty("name"));
        }
        let description = parts.next().unwrap_or("").trim_start().to_string();
        Ok(NewsgroupDescription {
            name: name.to_string(),
            description,
        })
    }
}

/// Frame active-file lines into a data block (`LIST ACTIVE`, `215`).
#[must_use]
pub fn active_block(groups: &[ActiveGroup]) -> String {
    let lines: Vec<String> = groups.iter().map(ActiveGroup::encode).collect();
    encode_lines(&lines)
}

/// Frame active-file lines into the `NEWGROUPS` body (`231`, RFC 3977 §7.3).
///
/// New newsgroups are reported in the active-file line format, so this is the
/// same framing as [`active_block`], named for its command for clarity.
#[must_use]
pub fn new_groups_block(groups: &[ActiveGroup]) -> String {
    active_block(groups)
}

/// Frame `LIST ACTIVE.TIMES` lines into a data block (RFC 3977 §7.6.4).
#[must_use]
pub fn active_times_block(entries: &[ActiveTimesEntry]) -> String {
    let lines: Vec<String> = entries.iter().map(ActiveTimesEntry::encode).collect();
    encode_lines(&lines)
}

/// Frame `LIST NEWSGROUPS` lines into a data block (RFC 3977 §7.6.6).
#[must_use]
pub fn newsgroups_block(descriptions: &[NewsgroupDescription]) -> String {
    let lines: Vec<String> = descriptions
        .iter()
        .map(NewsgroupDescription::encode)
        .collect();
    encode_lines(&lines)
}

/// Frame a list of message-ids into the `NEWNEWS` body (`230`, RFC 3977 §7.4).
///
/// Each id occupies its own line, angle brackets included.
#[must_use]
pub fn new_articles_block(ids: &[MessageId]) -> String {
    let lines: Vec<&str> = ids.iter().map(MessageId::as_str).collect();
    encode_lines(&lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datablock::decode_lines;

    #[test]
    fn active_group_round_trips() {
        let g = ActiveGroup {
            name: "rabbit.general".to_string(),
            high: 3002,
            low: 1,
            status: "y".to_string(),
        };
        assert_eq!(g.encode(), "rabbit.general 3002 1 y");
        assert_eq!(ActiveGroup::parse(&g.encode()).unwrap(), g);
    }

    #[test]
    fn active_group_equals_alias_status() {
        let line = "rabbit.moderated 5 5 =rabbit.general";
        let g = ActiveGroup::parse(line).unwrap();
        assert_eq!(g.status, "=rabbit.general");
        assert_eq!(g.encode(), line);
    }

    #[test]
    fn active_group_rejects_bad_shape() {
        assert_eq!(
            ActiveGroup::parse("only three fields"),
            Err(ListingError::FieldCount {
                expected: 4,
                found: 3
            })
        );
        assert!(matches!(
            ActiveGroup::parse("g high 1 y"),
            Err(ListingError::BadNumber { field: "high", .. })
        ));
    }

    #[test]
    fn active_times_round_trips_with_spacey_creator() {
        let e = ActiveTimesEntry {
            name: "rabbit.general".to_string(),
            created: 1_751_000_000,
            creator: "kevin@phunc.com".to_string(),
        };
        assert_eq!(ActiveTimesEntry::parse(&e.encode()).unwrap(), e);
        // Creator may contain spaces; the first two tokens fix name and time.
        let parsed = ActiveTimesEntry::parse("misc.test 1751000000 news admin").unwrap();
        assert_eq!(parsed.name, "misc.test");
        assert_eq!(parsed.created, 1_751_000_000);
        assert_eq!(parsed.creator, "news admin");
    }

    #[test]
    fn active_times_rejects_missing_fields_and_bad_time() {
        assert!(matches!(
            ActiveTimesEntry::parse("group 123"),
            Err(ListingError::FieldCount { expected: 3, .. })
        ));
        assert!(matches!(
            ActiveTimesEntry::parse("group notanumber creator"),
            Err(ListingError::BadNumber {
                field: "created",
                ..
            })
        ));
    }

    #[test]
    fn newsgroup_description_round_trips() {
        let d = NewsgroupDescription {
            name: "rabbit.general".to_string(),
            description: "General warren chatter".to_string(),
        };
        assert_eq!(d.encode(), "rabbit.general\tGeneral warren chatter");
        assert_eq!(NewsgroupDescription::parse(&d.encode()).unwrap(), d);
        // A space separator is tolerated on parse.
        let parsed = NewsgroupDescription::parse("misc.test  Testing, please ignore").unwrap();
        assert_eq!(parsed.name, "misc.test");
        assert_eq!(parsed.description, "Testing, please ignore");
    }

    #[test]
    fn newsgroup_description_requires_name() {
        assert_eq!(
            NewsgroupDescription::parse("   "),
            Err(ListingError::Empty("name"))
        );
        // Missing description is allowed (empty).
        let d = NewsgroupDescription::parse("rabbit.empty").unwrap();
        assert_eq!(d.description, "");
    }

    #[test]
    fn scrubbing_prevents_line_splitting() {
        // A CR/LF injected into the status must never split the wire line;
        // group names/status never legitimately contain whitespace.
        let g = ActiveGroup {
            name: "rabbit.general".to_string(),
            high: 1,
            low: 1,
            status: "y\r\nEVIL".to_string(),
        };
        let line = g.encode();
        assert!(!line.contains('\r'));
        assert!(!line.contains('\n'));
        // A tab in a description column is scrubbed so it stays one column.
        let d = NewsgroupDescription {
            name: "rabbit.general".to_string(),
            description: "has\ttab".to_string(),
        };
        assert_eq!(d.encode().split('\t').count(), 2);
    }

    #[test]
    fn blocks_are_framed_and_decodable() {
        let groups = vec![
            ActiveGroup {
                name: "rabbit.general".to_string(),
                high: 10,
                low: 1,
                status: "y".to_string(),
            },
            ActiveGroup {
                name: "rabbit.binaries".to_string(),
                high: 0,
                low: 1,
                status: "n".to_string(),
            },
        ];
        let block = active_block(&groups);
        assert!(block.ends_with(".\r\n"));
        let decoded = decode_lines(&block).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(ActiveGroup::parse(&decoded[0]).unwrap(), groups[0]);
        // NEWGROUPS uses the identical framing.
        assert_eq!(new_groups_block(&groups), block);
    }

    #[test]
    fn new_articles_block_lists_ids() {
        let ids = vec![
            MessageId::new("<a@x>").unwrap(),
            MessageId::new("<b@y>").unwrap(),
        ];
        let block = new_articles_block(&ids);
        assert_eq!(block, "<a@x>\r\n<b@y>\r\n.\r\n");
    }

    #[test]
    fn empty_blocks_are_just_terminator() {
        assert_eq!(active_block(&[]), ".\r\n");
        assert_eq!(active_times_block(&[]), ".\r\n");
        assert_eq!(newsgroups_block(&[]), ".\r\n");
        assert_eq!(new_articles_block(&[]), ".\r\n");
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for probe in ["", " ", "\t\t\t", "\0", "a b c d e", "1 2 3"] {
            let _ = ActiveGroup::parse(probe);
            let _ = ActiveTimesEntry::parse(probe);
            let _ = NewsgroupDescription::parse(probe);
        }
    }
}
