//! Validated NNTP message identifiers (RFC 3977 §3.6).
//!
//! A [`MessageId`] is the `<local@example.com>` token that permanently and
//! globally names an article. This newtype guarantees, on construction, that
//! the value obeys the on-wire grammar so the rest of the codec can treat it
//! as an opaque, always-well-formed handle:
//!
//! * between 3 and 250 octets in length, inclusive;
//! * the first octet is `<` and the last is `>`;
//! * every octet in between is printable US-ASCII (`0x21..=0x7E`) and is not
//!   itself `>`.
//!
//! The angle brackets are part of the stored value, matching how the identifier
//! appears on the wire, so `Display` and [`MessageId::as_str`] round-trip
//! verbatim.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Minimum length of a message-id, including the angle brackets (`<x>`).
pub const MIN_LEN: usize = 3;
/// Maximum length of a message-id, including the angle brackets.
pub const MAX_LEN: usize = 250;

/// A syntactically valid NNTP message identifier, angle brackets included.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MessageId(String);

/// Reasons a string cannot be interpreted as a [`MessageId`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MessageIdError {
    /// The value was shorter than 3 or longer than 250 octets.
    #[error("message-id must be {MIN_LEN}..={MAX_LEN} octets, got {0}")]
    BadLength(usize),
    /// The value was not wrapped in `<` and `>`.
    #[error("message-id must be wrapped in angle brackets")]
    MissingBrackets,
    /// A byte between the brackets was not printable US-ASCII, or was `>`.
    #[error("message-id contains an invalid octet")]
    InvalidCharacter,
}

impl MessageId {
    /// Validate `raw` and wrap it as a [`MessageId`].
    ///
    /// # Errors
    ///
    /// Returns [`MessageIdError`] if `raw` violates the RFC 3977 grammar. Never
    /// panics, including on empty, truncated, or non-ASCII input.
    pub fn new(raw: impl Into<String>) -> Result<Self, MessageIdError> {
        let s = raw.into();
        let bytes = s.as_bytes();
        let len = bytes.len();
        if !(MIN_LEN..=MAX_LEN).contains(&len) {
            return Err(MessageIdError::BadLength(len));
        }
        // `len >= 3` guarantees these indices exist.
        if bytes[0] != b'<' || bytes[len - 1] != b'>' {
            return Err(MessageIdError::MissingBrackets);
        }
        for &b in &bytes[1..len - 1] {
            if !(0x21..=0x7E).contains(&b) || b == b'>' {
                return Err(MessageIdError::InvalidCharacter);
            }
        }
        Ok(MessageId(s))
    }

    /// The identifier as it appears on the wire, angle brackets included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the newtype, returning the owned string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MessageId({:?})", self.0)
    }
}

impl FromStr for MessageId {
    type Err = MessageIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        MessageId::new(s)
    }
}

impl TryFrom<&str> for MessageId {
    type Error = MessageIdError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        MessageId::new(s)
    }
}

impl TryFrom<String> for MessageId {
    type Error = MessageIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        MessageId::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_message_id() {
        let m = MessageId::new("<abc123@news.example.com>").unwrap();
        assert_eq!(m.as_str(), "<abc123@news.example.com>");
        assert_eq!(m.to_string(), "<abc123@news.example.com>");
    }

    #[test]
    fn shortest_valid_is_three_octets() {
        assert!(MessageId::new("<a>").is_ok());
    }

    #[test]
    fn rejects_missing_brackets() {
        assert_eq!(
            MessageId::new("abc@example.com"),
            Err(MessageIdError::MissingBrackets)
        );
        assert_eq!(
            MessageId::new("<abc@example.com"),
            Err(MessageIdError::MissingBrackets)
        );
    }

    #[test]
    fn rejects_bad_length() {
        assert_eq!(MessageId::new(""), Err(MessageIdError::BadLength(0)));
        assert_eq!(MessageId::new("<>"), Err(MessageIdError::BadLength(2)));
        let huge = format!("<{}>", "x".repeat(300));
        assert!(matches!(
            MessageId::new(huge),
            Err(MessageIdError::BadLength(_))
        ));
    }

    #[test]
    fn rejects_inner_space_and_control() {
        assert_eq!(
            MessageId::new("<a b@example.com>"),
            Err(MessageIdError::InvalidCharacter)
        );
        assert_eq!(
            MessageId::new("<a\tb@x>"),
            Err(MessageIdError::InvalidCharacter)
        );
    }

    #[test]
    fn rejects_inner_close_bracket() {
        assert_eq!(
            MessageId::new("<a>b@x>"),
            Err(MessageIdError::InvalidCharacter)
        );
    }

    #[test]
    fn rejects_non_ascii_without_panicking() {
        // Multibyte UTF-8 octets fall outside printable US-ASCII.
        assert_eq!(
            MessageId::new("<café@x>"),
            Err(MessageIdError::InvalidCharacter)
        );
        assert_eq!(
            MessageId::new("<\u{00e9}@x>"),
            Err(MessageIdError::InvalidCharacter)
        );
    }

    #[test]
    fn from_str_and_try_from_agree() {
        let a: MessageId = "<x@y>".parse().unwrap();
        let b = MessageId::try_from("<x@y>").unwrap();
        assert_eq!(a, b);
    }
}
