//! NNTP client command parsing (RFC 3977).
//!
//! [`Command::parse`] turns a single client command line into a typed
//! [`Command`]. Verbs are matched case-insensitively; arguments keep their
//! original case. A trailing CRLF (or bare CR/LF) is stripped before parsing.
//!
//! Recognised but malformed commands (e.g. `GROUP` with no name, an
//! unparseable article number, a bad message-id) produce a
//! [`CommandError`]. Verbs that are not recognised at all become
//! [`Command::Unknown`], carrying the original line so the caller can answer
//! with `500`. Parsing never panics on arbitrary input.

use crate::message_id::{MessageId, MessageIdError};

use thiserror::Error;

/// An article selector for `ARTICLE`/`HEAD`/`BODY`/`STAT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArticleRef {
    /// No argument: act on the currently selected article.
    Current,
    /// A numeric article number within the selected group.
    Number(u64),
    /// A specific `<message-id>`.
    MessageId(MessageId),
}

/// A numeric range as used by `LISTGROUP` and `OVER`/`XOVER`.
///
/// `low` is the first article; `high` is `Some(n)` for `low-n`, `None` for the
/// open-ended `low-` form. A bare number `n` parses as `low = n`,
/// `high = Some(n)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// First article number in the range (inclusive).
    pub low: u64,
    /// Last article number (inclusive), or `None` for an open range.
    pub high: Option<u64>,
}

/// The selector accepted by `OVER`/`XOVER`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverRef {
    /// No argument: the currently selected article.
    Current,
    /// A range of article numbers.
    Range(Range),
    /// A specific `<message-id>` (server must advertise this capability).
    MessageId(MessageId),
}

/// The keyword argument to `LIST`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKeyword {
    /// `LIST` or `LIST ACTIVE [wildmat]` — the active newsgroup list.
    Active(Option<String>),
    /// `LIST NEWSGROUPS [wildmat]` — group descriptions.
    Newsgroups(Option<String>),
    /// `LIST OVERVIEW.FMT` — the overview field format.
    OverviewFmt,
    /// Any other recognised-shape keyword, kept verbatim (uppercased).
    Other(String),
}

/// A parsed NNTP client command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `CAPABILITIES`
    Capabilities,
    /// `MODE READER`
    ModeReader,
    /// `GROUP <name>`
    Group(String),
    /// `LISTGROUP [group [range]]`
    ListGroup {
        /// Optional group name; `None` selects the current group.
        group: Option<String>,
        /// Optional article-number range.
        range: Option<Range>,
    },
    /// `ARTICLE [number|<message-id>]`
    Article(ArticleRef),
    /// `HEAD [number|<message-id>]`
    Head(ArticleRef),
    /// `BODY [number|<message-id>]`
    Body(ArticleRef),
    /// `STAT [number|<message-id>]`
    Stat(ArticleRef),
    /// `NEXT`
    Next,
    /// `LAST`
    Last,
    /// `LIST [keyword [wildmat]]`
    List(ListKeyword),
    /// `OVER [range|<message-id>]`
    Over(OverRef),
    /// `XOVER [range|<message-id>]` — the pre-RFC-3977 synonym for `OVER`.
    Xover(OverRef),
    /// `NEWNEWS <wildmat> <date> <time> [GMT]`
    NewNews {
        /// Wildmat pattern matching the newsgroups of interest.
        wildmat: String,
        /// Date argument (`yymmdd` or `yyyymmdd`), kept as text.
        date: String,
        /// Time argument (`hhmmss`), kept as text.
        time: String,
        /// Whether the `GMT` suffix was present.
        gmt: bool,
    },
    /// `POST`
    Post,
    /// `AUTHINFO USER <name>`
    AuthInfoUser(String),
    /// `AUTHINFO PASS <password>`
    AuthInfoPass(String),
    /// `DATE`
    Date,
    /// `QUIT`
    Quit,
    /// A verb this codec does not recognise; carries the original line.
    Unknown(String),
}

/// Reasons a recognised command could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandError {
    /// The line was empty (or only whitespace).
    #[error("empty command line")]
    Empty,
    /// A required argument was missing.
    #[error("{verb} requires an argument")]
    MissingArgument {
        /// The offending verb.
        verb: &'static str,
    },
    /// More arguments were supplied than the command accepts.
    #[error("{verb} accepts no such argument")]
    UnexpectedArgument {
        /// The offending verb.
        verb: &'static str,
    },
    /// An argument that should have been a number could not be parsed.
    #[error("invalid number: {0:?}")]
    InvalidNumber(String),
    /// An argument that should have been a range could not be parsed.
    #[error("invalid range: {0:?}")]
    InvalidRange(String),
    /// A `<message-id>` argument was malformed.
    #[error("invalid message-id: {0}")]
    MessageId(#[from] MessageIdError),
    /// An `AUTHINFO` subcommand other than `USER`/`PASS` was given.
    #[error("unsupported AUTHINFO subcommand: {0:?}")]
    InvalidAuthInfo(String),
}

impl Command {
    /// Parse a single command line into a [`Command`].
    ///
    /// A trailing CRLF/CR/LF is stripped. Unknown verbs yield
    /// [`Command::Unknown`]; malformed known commands yield [`CommandError`].
    ///
    /// # Errors
    ///
    /// See [`CommandError`]. Never panics, including on empty, truncated, or
    /// non-ASCII input.
    pub fn parse(line: &str) -> Result<Command, CommandError> {
        let line = line.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            return Err(CommandError::Empty);
        }

        let mut tokens = trimmed.split_whitespace();
        // `trimmed` is non-empty, so a first token always exists.
        let verb = tokens.next().unwrap_or("");
        let rest: Vec<&str> = tokens.collect();
        let verb_uc = verb.to_ascii_uppercase();

        match verb_uc.as_str() {
            "CAPABILITIES" => Ok(Command::Capabilities),
            "QUIT" => Ok(Command::Quit),
            "DATE" => Ok(Command::Date),
            "POST" => Ok(Command::Post),
            "NEXT" => Ok(Command::Next),
            "LAST" => Ok(Command::Last),

            "MODE" => {
                if rest.len() == 1 && rest[0].eq_ignore_ascii_case("READER") {
                    Ok(Command::ModeReader)
                } else {
                    Ok(Command::Unknown(line.to_string()))
                }
            }

            "GROUP" => match rest.as_slice() {
                [name] => Ok(Command::Group((*name).to_string())),
                [] => Err(CommandError::MissingArgument { verb: "GROUP" }),
                _ => Err(CommandError::UnexpectedArgument { verb: "GROUP" }),
            },

            "LISTGROUP" => match rest.as_slice() {
                [] => Ok(Command::ListGroup {
                    group: None,
                    range: None,
                }),
                [group] => Ok(Command::ListGroup {
                    group: Some((*group).to_string()),
                    range: None,
                }),
                [group, range] => Ok(Command::ListGroup {
                    group: Some((*group).to_string()),
                    range: Some(parse_range(range)?),
                }),
                _ => Err(CommandError::UnexpectedArgument { verb: "LISTGROUP" }),
            },

            "ARTICLE" => Ok(Command::Article(parse_article_ref("ARTICLE", &rest)?)),
            "HEAD" => Ok(Command::Head(parse_article_ref("HEAD", &rest)?)),
            "BODY" => Ok(Command::Body(parse_article_ref("BODY", &rest)?)),
            "STAT" => Ok(Command::Stat(parse_article_ref("STAT", &rest)?)),

            "OVER" => Ok(Command::Over(parse_over_ref("OVER", &rest)?)),
            "XOVER" => Ok(Command::Xover(parse_over_ref("XOVER", &rest)?)),

            "LIST" => Ok(Command::List(parse_list(&rest)?)),

            "NEWNEWS" => match rest.as_slice() {
                [wildmat, date, time] => Ok(Command::NewNews {
                    wildmat: (*wildmat).to_string(),
                    date: (*date).to_string(),
                    time: (*time).to_string(),
                    gmt: false,
                }),
                [wildmat, date, time, gmt] if gmt.eq_ignore_ascii_case("GMT") => {
                    Ok(Command::NewNews {
                        wildmat: (*wildmat).to_string(),
                        date: (*date).to_string(),
                        time: (*time).to_string(),
                        gmt: true,
                    })
                }
                [] | [_] | [_, _] => Err(CommandError::MissingArgument { verb: "NEWNEWS" }),
                _ => Err(CommandError::UnexpectedArgument { verb: "NEWNEWS" }),
            },

            "AUTHINFO" => parse_authinfo(&rest),

            _ => Ok(Command::Unknown(line.to_string())),
        }
    }
}

fn parse_u64(s: &str) -> Result<u64, CommandError> {
    s.parse::<u64>()
        .map_err(|_| CommandError::InvalidNumber(s.to_string()))
}

fn parse_range(s: &str) -> Result<Range, CommandError> {
    if let Some((lo, hi)) = s.split_once('-') {
        let low = lo
            .parse::<u64>()
            .map_err(|_| CommandError::InvalidRange(s.to_string()))?;
        let high = if hi.is_empty() {
            None
        } else {
            Some(
                hi.parse::<u64>()
                    .map_err(|_| CommandError::InvalidRange(s.to_string()))?,
            )
        };
        Ok(Range { low, high })
    } else {
        let n = s
            .parse::<u64>()
            .map_err(|_| CommandError::InvalidRange(s.to_string()))?;
        Ok(Range {
            low: n,
            high: Some(n),
        })
    }
}

fn parse_article_ref(verb: &'static str, rest: &[&str]) -> Result<ArticleRef, CommandError> {
    match rest {
        [] => Ok(ArticleRef::Current),
        [arg] => {
            if arg.starts_with('<') {
                Ok(ArticleRef::MessageId(MessageId::new(*arg)?))
            } else {
                Ok(ArticleRef::Number(parse_u64(arg)?))
            }
        }
        _ => Err(CommandError::UnexpectedArgument { verb }),
    }
}

fn parse_over_ref(verb: &'static str, rest: &[&str]) -> Result<OverRef, CommandError> {
    match rest {
        [] => Ok(OverRef::Current),
        [arg] => {
            if arg.starts_with('<') {
                Ok(OverRef::MessageId(MessageId::new(*arg)?))
            } else {
                Ok(OverRef::Range(parse_range(arg)?))
            }
        }
        _ => Err(CommandError::UnexpectedArgument { verb }),
    }
}

fn parse_list(rest: &[&str]) -> Result<ListKeyword, CommandError> {
    match rest {
        [] => Ok(ListKeyword::Active(None)),
        [kw, tail @ ..] => {
            let kw_uc = kw.to_ascii_uppercase();
            let wildmat = |tail: &[&str]| -> Result<Option<String>, CommandError> {
                match tail {
                    [] => Ok(None),
                    [w] => Ok(Some((*w).to_string())),
                    _ => Err(CommandError::UnexpectedArgument { verb: "LIST" }),
                }
            };
            match kw_uc.as_str() {
                "ACTIVE" => Ok(ListKeyword::Active(wildmat(tail)?)),
                "NEWSGROUPS" => Ok(ListKeyword::Newsgroups(wildmat(tail)?)),
                "OVERVIEW.FMT" => {
                    if tail.is_empty() {
                        Ok(ListKeyword::OverviewFmt)
                    } else {
                        Err(CommandError::UnexpectedArgument { verb: "LIST" })
                    }
                }
                _ => Ok(ListKeyword::Other(kw_uc)),
            }
        }
    }
}

fn parse_authinfo(rest: &[&str]) -> Result<Command, CommandError> {
    match rest {
        [sub, value] if sub.eq_ignore_ascii_case("USER") => {
            Ok(Command::AuthInfoUser((*value).to_string()))
        }
        [sub, value] if sub.eq_ignore_ascii_case("PASS") => {
            Ok(Command::AuthInfoPass((*value).to_string()))
        }
        [sub] if sub.eq_ignore_ascii_case("USER") || sub.eq_ignore_ascii_case("PASS") => {
            Err(CommandError::MissingArgument { verb: "AUTHINFO" })
        }
        [] => Err(CommandError::MissingArgument { verb: "AUTHINFO" }),
        [sub, ..] => Err(CommandError::InvalidAuthInfo((*sub).to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_verbs_case_insensitively() {
        assert_eq!(
            Command::parse("CAPABILITIES\r\n"),
            Ok(Command::Capabilities)
        );
        assert_eq!(Command::parse("quit"), Ok(Command::Quit));
        assert_eq!(Command::parse("Date"), Ok(Command::Date));
        assert_eq!(Command::parse("post\r\n"), Ok(Command::Post));
        assert_eq!(Command::parse("NEXT"), Ok(Command::Next));
        assert_eq!(Command::parse("last"), Ok(Command::Last));
    }

    #[test]
    fn parses_mode_reader() {
        assert_eq!(Command::parse("MODE READER"), Ok(Command::ModeReader));
        assert_eq!(Command::parse("mode reader\r\n"), Ok(Command::ModeReader));
        assert_eq!(
            Command::parse("MODE STREAM"),
            Ok(Command::Unknown("MODE STREAM".to_string()))
        );
    }

    #[test]
    fn parses_group() {
        assert_eq!(
            Command::parse("GROUP rabbit.general"),
            Ok(Command::Group("rabbit.general".to_string()))
        );
        assert_eq!(
            Command::parse("GROUP"),
            Err(CommandError::MissingArgument { verb: "GROUP" })
        );
    }

    #[test]
    fn parses_listgroup_variants() {
        assert_eq!(
            Command::parse("LISTGROUP"),
            Ok(Command::ListGroup {
                group: None,
                range: None
            })
        );
        assert_eq!(
            Command::parse("LISTGROUP misc.test"),
            Ok(Command::ListGroup {
                group: Some("misc.test".to_string()),
                range: None
            })
        );
        assert_eq!(
            Command::parse("LISTGROUP misc.test 10-20"),
            Ok(Command::ListGroup {
                group: Some("misc.test".to_string()),
                range: Some(Range {
                    low: 10,
                    high: Some(20)
                })
            })
        );
    }

    #[test]
    fn parses_article_by_number_and_id_and_current() {
        assert_eq!(
            Command::parse("ARTICLE 42"),
            Ok(Command::Article(ArticleRef::Number(42)))
        );
        assert_eq!(
            Command::parse("HEAD <a@b>"),
            Ok(Command::Head(ArticleRef::MessageId(
                MessageId::new("<a@b>").unwrap()
            )))
        );
        assert_eq!(
            Command::parse("BODY"),
            Ok(Command::Body(ArticleRef::Current))
        );
        assert_eq!(
            Command::parse("STAT 7"),
            Ok(Command::Stat(ArticleRef::Number(7)))
        );
    }

    #[test]
    fn article_bad_number_errors() {
        assert_eq!(
            Command::parse("ARTICLE notanum"),
            Err(CommandError::InvalidNumber("notanum".to_string()))
        );
    }

    #[test]
    fn article_bad_message_id_errors() {
        assert!(matches!(
            Command::parse("ARTICLE <bad"),
            Err(CommandError::MessageId(_))
        ));
    }

    #[test]
    fn parses_over_and_xover() {
        assert_eq!(
            Command::parse("OVER 1-5"),
            Ok(Command::Over(OverRef::Range(Range {
                low: 1,
                high: Some(5)
            })))
        );
        assert_eq!(
            Command::parse("XOVER 3-"),
            Ok(Command::Xover(OverRef::Range(Range { low: 3, high: None })))
        );
        assert_eq!(Command::parse("OVER"), Ok(Command::Over(OverRef::Current)));
        assert_eq!(
            Command::parse("OVER <x@y>"),
            Ok(Command::Over(OverRef::MessageId(
                MessageId::new("<x@y>").unwrap()
            )))
        );
    }

    #[test]
    fn bare_number_range_is_single_article() {
        assert_eq!(
            Command::parse("XOVER 12"),
            Ok(Command::Xover(OverRef::Range(Range {
                low: 12,
                high: Some(12)
            })))
        );
    }

    #[test]
    fn parses_list_keywords() {
        assert_eq!(
            Command::parse("LIST"),
            Ok(Command::List(ListKeyword::Active(None)))
        );
        assert_eq!(
            Command::parse("LIST ACTIVE rabbit.*"),
            Ok(Command::List(ListKeyword::Active(Some(
                "rabbit.*".to_string()
            ))))
        );
        assert_eq!(
            Command::parse("list newsgroups"),
            Ok(Command::List(ListKeyword::Newsgroups(None)))
        );
        assert_eq!(
            Command::parse("LIST OVERVIEW.FMT"),
            Ok(Command::List(ListKeyword::OverviewFmt))
        );
        assert_eq!(
            Command::parse("LIST DISTRIBUTIONS"),
            Ok(Command::List(ListKeyword::Other(
                "DISTRIBUTIONS".to_string()
            )))
        );
    }

    #[test]
    fn parses_newnews() {
        assert_eq!(
            Command::parse("NEWNEWS rabbit.* 20260101 000000 GMT"),
            Ok(Command::NewNews {
                wildmat: "rabbit.*".to_string(),
                date: "20260101".to_string(),
                time: "000000".to_string(),
                gmt: true,
            })
        );
        assert_eq!(
            Command::parse("NEWNEWS rabbit.* 20260101 000000"),
            Ok(Command::NewNews {
                wildmat: "rabbit.*".to_string(),
                date: "20260101".to_string(),
                time: "000000".to_string(),
                gmt: false,
            })
        );
        assert_eq!(
            Command::parse("NEWNEWS rabbit.*"),
            Err(CommandError::MissingArgument { verb: "NEWNEWS" })
        );
    }

    #[test]
    fn parses_authinfo() {
        assert_eq!(
            Command::parse("AUTHINFO USER kevin"),
            Ok(Command::AuthInfoUser("kevin".to_string()))
        );
        assert_eq!(
            Command::parse("AUTHINFO PASS s3cr3t"),
            Ok(Command::AuthInfoPass("s3cr3t".to_string()))
        );
        assert_eq!(
            Command::parse("AUTHINFO SASL PLAIN"),
            Err(CommandError::InvalidAuthInfo("SASL".to_string()))
        );
        assert_eq!(
            Command::parse("AUTHINFO USER"),
            Err(CommandError::MissingArgument { verb: "AUTHINFO" })
        );
    }

    #[test]
    fn unknown_verb_is_captured() {
        assert_eq!(
            Command::parse("FROBNICATE now"),
            Ok(Command::Unknown("FROBNICATE now".to_string()))
        );
    }

    #[test]
    fn empty_line_errors() {
        assert_eq!(Command::parse(""), Err(CommandError::Empty));
        assert_eq!(Command::parse("   \r\n"), Err(CommandError::Empty));
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for probe in [
            "",
            "\r\n",
            "GROUP",
            "OVER -",
            "OVER 1-2-3",
            "ARTICLE <",
            "\0\0",
            "LISTGROUP a b c d",
            "AUTHINFO",
            "NEWNEWS a b c d e",
        ] {
            let _ = Command::parse(probe);
        }
    }
}
