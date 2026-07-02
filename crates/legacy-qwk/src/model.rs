//! The canonical [`QwkMessage`] model shared across the codec.
//!
//! A `QwkMessage` holds message text with **`\n` line endings normalized
//! internally**; the `MESSAGES.DAT` codec converts to and from the QWK on-disk
//! `0xE3` end-of-line marker at the edge (see [`crate::messages`]). The 25-byte
//! To/From/Subject header limits are *not* enforced here — long values live in
//! these fields and, when a packet is QWKE-extended, are additionally emitted as
//! body kludge lines (see [`crate::qwke`]).

/// A single QWK message: its header metadata plus normalized body text.
///
/// String fields hold decoded text (Latin-1 at the byte edge); the body uses
/// `\n` line separators regardless of the `0xE3` markers used on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QwkMessage {
    /// Status flag byte (offset 0). Common values: `b' '` public/unread,
    /// `b'-'` public/read, `b'*'` private/unread, `b'+'` private/read.
    pub status: u8,
    /// Message number (header offsets 1..8, ASCII on disk).
    pub number: u32,
    /// Conference number (header offsets 123..125, little-endian on disk).
    pub conference: u16,
    /// Date stamp, `MM-DD-YY` (header offsets 8..16).
    pub date: String,
    /// Time stamp, `HH:MM` (header offsets 16..21).
    pub time: String,
    /// Recipient (header offsets 21..46, 25 bytes on disk; may be longer here
    /// when QWKE kludges carry the full value).
    pub to: String,
    /// Sender (header offsets 46..71).
    pub from: String,
    /// Subject (header offsets 71..96).
    pub subject: String,
    /// Password (header offsets 96..108); usually empty.
    pub password: String,
    /// Reference (reply-to) message number (header offsets 108..116). `0` means
    /// "no reference".
    pub reference: u32,
    /// Active flag (header offset 122): `true` => `0xE1` (active), `false` =>
    /// `0xE2` (killed/inactive).
    pub active: bool,
    /// Message body with `\n` line endings. Converted to/from `0xE3` by the
    /// codec.
    pub body: String,
}

impl Default for QwkMessage {
    fn default() -> Self {
        Self {
            status: b' ',
            number: 0,
            conference: 0,
            date: String::new(),
            time: String::new(),
            to: String::new(),
            from: String::new(),
            subject: String::new(),
            password: String::new(),
            reference: 0,
            active: true,
            body: String::new(),
        }
    }
}

impl QwkMessage {
    /// Build a plain public message with the given routing/subject/body,
    /// leaving status/date/time/password at their defaults.
    pub fn new(
        conference: u16,
        number: u32,
        to: impl Into<String>,
        from: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            conference,
            number,
            to: to.into(),
            from: from.into(),
            subject: subject.into(),
            body: body.into(),
            ..Self::default()
        }
    }
}
