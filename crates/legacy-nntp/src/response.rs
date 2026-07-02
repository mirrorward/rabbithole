//! NNTP status responses (RFC 3977 §3.2).
//!
//! Every server reply opens with a status line: a three-digit code, a space,
//! and human-readable text, terminated by CRLF. This module provides:
//!
//! * [`Status`], a typed enum of the codes this codec cares about, each with a
//!   numeric [`Status::code`] and a sensible [`Status::default_text`];
//! * [`Response`], a `(code, text)` pair that [renders](Response::render) to
//!   `CODE text\r\n` and can be [parsed](Response::parse) back from a line.
//!
//! Response text is scrubbed of CR and LF on render so a status line can never
//! be split into two, and parsing never panics on arbitrary bytes.

use thiserror::Error;

/// A typed NNTP status code.
///
/// Only codes relevant to the reader/codec surface are enumerated; arbitrary
/// numeric codes can still be sent via [`Response::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 101 — capability list follows.
    CapabilitiesFollow,
    /// 111 — server date and time (`yyyymmddhhmmss`).
    DateFollows,
    /// 200 — service available, posting allowed (greeting).
    PostingAllowed,
    /// 201 — service available, posting prohibited (greeting).
    PostingProhibited,
    /// 203 — streaming permitted (`MODE STREAM`, RFC 4644).
    StreamingPermitted,
    /// 205 — connection closing (goodbye).
    ConnectionClosing,
    /// 211 — group selected / article numbers follow (`LISTGROUP`).
    GroupSelected,
    /// 215 — information follows (`LIST`).
    InformationFollows,
    /// 220 — article follows (head and body).
    ArticleFollows,
    /// 221 — article headers follow (`HEAD`).
    HeadFollows,
    /// 222 — article body follows (`BODY`).
    BodyFollows,
    /// 223 — article exists / selected (`STAT`, `NEXT`, `LAST`).
    ArticleExists,
    /// 224 — overview information follows (`OVER`/`XOVER`).
    OverviewFollows,
    /// 230 — list of new articles follows (`NEWNEWS`).
    NewArticlesFollow,
    /// 231 — list of new newsgroups follows (`NEWGROUPS`).
    NewGroupsFollow,
    /// 235 — article transferred OK (`IHAVE`, RFC 3977 §6.3.2).
    IhaveTransferOk,
    /// 238 — article wanted; send it via `TAKETHIS` (`CHECK`, RFC 4644).
    CheckWanted,
    /// 239 — article transferred OK (`TAKETHIS`, RFC 4644).
    TakethisAccepted,
    /// 240 — article received and posted OK.
    PostingOk,
    /// 281 — authentication accepted.
    AuthAccepted,
    /// 335 — send the article to be transferred (`IHAVE`, RFC 3977 §6.3.2).
    IhaveSendArticle,
    /// 340 — send the article to be posted (`POST`).
    SendArticle,
    /// 381 — more authentication information required (send `PASS`).
    MoreAuthRequired,
    /// 400 — service not available or no longer available.
    ServiceUnavailable,
    /// 403 — internal fault; command not performed.
    InternalFault,
    /// 411 — no such newsgroup.
    NoSuchGroup,
    /// 412 — no newsgroup has been selected.
    NoGroupSelected,
    /// 420 — current article number is invalid.
    CurrentArticleInvalid,
    /// 421 — no next article in this group.
    NoNextArticle,
    /// 422 — no previous article in this group.
    NoPreviousArticle,
    /// 423 — no article with that number (or in that range).
    NoArticleWithNumber,
    /// 430 — no article with that message-id.
    NoArticleWithMessageId,
    /// 431 — transfer not possible now; try again later (`CHECK`, RFC 4644).
    CheckDeferred,
    /// 435 — article not wanted (`IHAVE`, RFC 3977 §6.3.2).
    IhaveNotWanted,
    /// 436 — transfer not possible; try again later (`IHAVE`, RFC 3977 §6.3.2).
    IhaveDeferred,
    /// 437 — transfer rejected; do not retry (`IHAVE`, RFC 3977 §6.3.2).
    IhaveRejected,
    /// 438 — article not wanted (`CHECK`, RFC 4644).
    CheckNotWanted,
    /// 439 — article transfer failed / rejected (`TAKETHIS`, RFC 4644).
    TakethisRejected,
    /// 440 — posting not permitted.
    PostingNotPermitted,
    /// 441 — posting failed.
    PostingFailed,
    /// 450 — transfer permission denied (peer not authorised to feed).
    TransferPermissionDenied,
    /// 480 — command unavailable until the client has authenticated.
    AuthRequired,
    /// 481 — authentication credentials rejected.
    AuthRejected,
    /// 482 — authentication commands issued out of sequence.
    AuthSequenceError,
    /// 483 — command unavailable until a secure connection is established.
    EncryptionRequired,
    /// 500 — unknown command.
    UnknownCommand,
    /// 501 — syntax error in command arguments.
    SyntaxError,
    /// 502 — command unavailable / permission denied.
    CommandUnavailable,
    /// 503 — feature not supported.
    FeatureNotSupported,
}

impl Status {
    /// The numeric wire code.
    #[must_use]
    pub fn code(self) -> u16 {
        match self {
            Status::CapabilitiesFollow => 101,
            Status::DateFollows => 111,
            Status::PostingAllowed => 200,
            Status::PostingProhibited => 201,
            Status::StreamingPermitted => 203,
            Status::ConnectionClosing => 205,
            Status::GroupSelected => 211,
            Status::InformationFollows => 215,
            Status::ArticleFollows => 220,
            Status::HeadFollows => 221,
            Status::BodyFollows => 222,
            Status::ArticleExists => 223,
            Status::OverviewFollows => 224,
            Status::NewArticlesFollow => 230,
            Status::NewGroupsFollow => 231,
            Status::IhaveTransferOk => 235,
            Status::CheckWanted => 238,
            Status::TakethisAccepted => 239,
            Status::PostingOk => 240,
            Status::AuthAccepted => 281,
            Status::IhaveSendArticle => 335,
            Status::SendArticle => 340,
            Status::MoreAuthRequired => 381,
            Status::ServiceUnavailable => 400,
            Status::InternalFault => 403,
            Status::NoSuchGroup => 411,
            Status::NoGroupSelected => 412,
            Status::CurrentArticleInvalid => 420,
            Status::NoNextArticle => 421,
            Status::NoPreviousArticle => 422,
            Status::NoArticleWithNumber => 423,
            Status::NoArticleWithMessageId => 430,
            Status::CheckDeferred => 431,
            Status::IhaveNotWanted => 435,
            Status::IhaveDeferred => 436,
            Status::IhaveRejected => 437,
            Status::CheckNotWanted => 438,
            Status::TakethisRejected => 439,
            Status::PostingNotPermitted => 440,
            Status::PostingFailed => 441,
            Status::TransferPermissionDenied => 450,
            Status::AuthRequired => 480,
            Status::AuthRejected => 481,
            Status::AuthSequenceError => 482,
            Status::EncryptionRequired => 483,
            Status::UnknownCommand => 500,
            Status::SyntaxError => 501,
            Status::CommandUnavailable => 502,
            Status::FeatureNotSupported => 503,
        }
    }

    /// A reasonable default human-readable text for this status.
    #[must_use]
    pub fn default_text(self) -> &'static str {
        match self {
            Status::CapabilitiesFollow => "Capability list follows",
            Status::DateFollows => "Server date and time",
            Status::PostingAllowed => "Service available, posting allowed",
            Status::PostingProhibited => "Service available, posting prohibited",
            Status::StreamingPermitted => "Streaming permitted",
            Status::ConnectionClosing => "Closing connection",
            Status::GroupSelected => "Group selected",
            Status::InformationFollows => "Information follows",
            Status::ArticleFollows => "Article follows",
            Status::HeadFollows => "Headers follow",
            Status::BodyFollows => "Body follows",
            Status::ArticleExists => "Article exists",
            Status::OverviewFollows => "Overview information follows",
            Status::NewArticlesFollow => "List of new articles follows",
            Status::NewGroupsFollow => "List of new newsgroups follows",
            Status::IhaveTransferOk => "Article transferred OK",
            Status::CheckWanted => "Send article to be transferred",
            Status::TakethisAccepted => "Article transferred OK",
            Status::PostingOk => "Article received OK",
            Status::AuthAccepted => "Authentication accepted",
            Status::IhaveSendArticle => "Send it",
            Status::SendArticle => "Send article to be posted",
            Status::MoreAuthRequired => "Password required",
            Status::ServiceUnavailable => "Service not available",
            Status::InternalFault => "Internal fault",
            Status::NoSuchGroup => "No such newsgroup",
            Status::NoGroupSelected => "No newsgroup selected",
            Status::CurrentArticleInvalid => "Current article number is invalid",
            Status::NoNextArticle => "No next article in this group",
            Status::NoPreviousArticle => "No previous article in this group",
            Status::NoArticleWithNumber => "No article with that number",
            Status::NoArticleWithMessageId => "No article with that message-id",
            Status::CheckDeferred => "Transfer not possible; try again later",
            Status::IhaveNotWanted => "Article not wanted",
            Status::IhaveDeferred => "Transfer not possible; try again later",
            Status::IhaveRejected => "Transfer rejected; do not retry",
            Status::CheckNotWanted => "Article not wanted",
            Status::TakethisRejected => "Transfer rejected",
            Status::PostingNotPermitted => "Posting not permitted",
            Status::PostingFailed => "Posting failed",
            Status::TransferPermissionDenied => "Transfer permission denied",
            Status::AuthRequired => "Authentication required",
            Status::AuthRejected => "Authentication rejected",
            Status::AuthSequenceError => "Authentication commands out of sequence",
            Status::EncryptionRequired => "Encryption required",
            Status::UnknownCommand => "Unknown command",
            Status::SyntaxError => "Syntax error",
            Status::CommandUnavailable => "Command unavailable",
            Status::FeatureNotSupported => "Feature not supported",
        }
    }

    /// Build a [`Response`] with this status's default text.
    #[must_use]
    pub fn response(self) -> Response {
        Response::new(self.code(), self.default_text())
    }
}

impl From<Status> for Response {
    fn from(status: Status) -> Self {
        status.response()
    }
}

/// A status line: a three-digit code and its accompanying text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    code: u16,
    text: String,
}

/// Reasons a status line could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResponseError {
    /// The line did not begin with a three-digit code.
    #[error("missing or malformed three-digit status code")]
    BadCode,
    /// The line was empty.
    #[error("empty response line")]
    Empty,
}

impl Response {
    /// Construct a response from an arbitrary numeric code and text.
    #[must_use]
    pub fn new(code: u16, text: impl Into<String>) -> Self {
        Response {
            code,
            text: text.into(),
        }
    }

    /// Construct a response from a [`Status`] with custom text.
    #[must_use]
    pub fn with_text(status: Status, text: impl Into<String>) -> Self {
        Response::new(status.code(), text)
    }

    /// The numeric status code.
    #[must_use]
    pub fn code(&self) -> u16 {
        self.code
    }

    /// The response text (never contains CR or LF once rendered).
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Render as a `CODE text\r\n` status line.
    ///
    /// Any CR/LF embedded in the text is replaced with a space so the status
    /// line cannot be split. If the text is empty, just `CODE\r\n` is produced.
    #[must_use]
    pub fn render(&self) -> String {
        let scrubbed: String = self
            .text
            .chars()
            .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
            .collect();
        let scrubbed = scrubbed.trim_end();
        if scrubbed.is_empty() {
            format!("{}\r\n", self.code)
        } else {
            format!("{} {}\r\n", self.code, scrubbed)
        }
    }

    /// Parse a status line back into a [`Response`].
    ///
    /// A trailing CRLF/CR/LF is stripped. The line must start with exactly
    /// three ASCII digits. Never panics on arbitrary input.
    ///
    /// # Errors
    ///
    /// Returns [`ResponseError`] if the line is empty or does not begin with a
    /// three-digit code.
    pub fn parse(line: &str) -> Result<Response, ResponseError> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return Err(ResponseError::Empty);
        }
        let bytes = line.as_bytes();
        if bytes.len() < 3 || !bytes[..3].iter().all(u8::is_ascii_digit) {
            return Err(ResponseError::BadCode);
        }
        // A fourth byte, if present, must be the separating space.
        if bytes.len() > 3 && bytes[3] != b' ' {
            return Err(ResponseError::BadCode);
        }
        // Three ASCII digits fit comfortably in a u16.
        let code: u16 = line[..3].parse().map_err(|_| ResponseError::BadCode)?;
        let text = line.get(4..).unwrap_or("").to_string();
        Ok(Response { code, text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_match_rfc() {
        assert_eq!(Status::GroupSelected.code(), 211);
        assert_eq!(Status::ArticleFollows.code(), 220);
        assert_eq!(Status::OverviewFollows.code(), 224);
        assert_eq!(Status::PostingOk.code(), 240);
        assert_eq!(Status::SendArticle.code(), 340);
        assert_eq!(Status::MoreAuthRequired.code(), 381);
        assert_eq!(Status::AuthAccepted.code(), 281);
        assert_eq!(Status::AuthRejected.code(), 481);
        assert_eq!(Status::NoSuchGroup.code(), 411);
        assert_eq!(Status::NoArticleWithMessageId.code(), 430);
        assert_eq!(Status::UnknownCommand.code(), 500);
    }

    #[test]
    fn transit_status_codes_match_rfc() {
        assert_eq!(Status::StreamingPermitted.code(), 203);
        assert_eq!(Status::IhaveTransferOk.code(), 235);
        assert_eq!(Status::CheckWanted.code(), 238);
        assert_eq!(Status::TakethisAccepted.code(), 239);
        assert_eq!(Status::IhaveSendArticle.code(), 335);
        assert_eq!(Status::CheckDeferred.code(), 431);
        assert_eq!(Status::IhaveNotWanted.code(), 435);
        assert_eq!(Status::IhaveDeferred.code(), 436);
        assert_eq!(Status::IhaveRejected.code(), 437);
        assert_eq!(Status::CheckNotWanted.code(), 438);
        assert_eq!(Status::TakethisRejected.code(), 439);
        assert_eq!(Status::TransferPermissionDenied.code(), 450);
    }

    #[test]
    fn renders_status_line() {
        assert_eq!(
            Status::UnknownCommand.response().render(),
            "500 Unknown command\r\n"
        );
        assert_eq!(
            Response::new(211, "3 1 3 misc.test").render(),
            "211 3 1 3 misc.test\r\n"
        );
    }

    #[test]
    fn render_scrubs_embedded_newlines() {
        assert_eq!(
            Response::new(500, "bad\r\ninjection").render(),
            "500 bad  injection\r\n"
        );
    }

    #[test]
    fn render_empty_text_is_code_only() {
        assert_eq!(Response::new(205, "").render(), "205\r\n");
    }

    #[test]
    fn round_trips_through_parse() {
        let original = Response::new(211, "3 1 3 misc.test");
        let wire = original.render();
        let parsed = Response::parse(&wire).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert_eq!(Response::parse(""), Err(ResponseError::Empty));
        assert_eq!(Response::parse("2"), Err(ResponseError::BadCode));
        assert_eq!(Response::parse("2x0 hi"), Err(ResponseError::BadCode));
        assert_eq!(Response::parse("200hi"), Err(ResponseError::BadCode));
    }

    #[test]
    fn parse_code_only_line() {
        assert_eq!(Response::parse("205\r\n"), Ok(Response::new(205, "")));
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for probe in ["", "\r\n", "\0\0\0", "999", "abc def", "12", "42 \r"] {
            let _ = Response::parse(probe);
        }
    }
}
