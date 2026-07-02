//! Well-known Hotline field ids and transaction type numbers.
//!
//! These are the classic, on-the-wire numeric constants shared by every
//! Hotline client and server since the late 1990s. They are grouped into two
//! submodules: [`field`] for TLV parameter ids and [`transaction`] for
//! transaction types. Only the constants needed by the Wave 7.1 codec slice are
//! defined here; the remaining news/file/account sets land with later slices.

/// Well-known TLV parameter field ids (the `id` in a [`crate::field::Field`]).
///
/// A Hotline field is identified by a 16-bit number; the value's meaning and
/// width (text, integer, or blob) is a property of the id, documented per
/// constant below.
pub mod field {
    /// Human-readable error text (100). Carried in error replies.
    pub const ERROR_TEXT: u16 = 100;

    /// Generic "data" field (101). In chat transactions this is the chat line
    /// text; it also carries flat-news article bodies and server banners.
    pub const DATA: u16 = 101;

    /// Alias for [`DATA`] (101) when used as the chat message text.
    pub const CHAT_TEXT: u16 = DATA;

    /// User's display name (102), text.
    pub const USER_NAME: u16 = 102;

    /// User's session id (103), integer.
    pub const USER_ID: u16 = 103;

    /// User's icon id (104), integer — index into the classic icon set.
    pub const USER_ICON_ID: u16 = 104;

    /// Login name / account handle (105), text. Sent obfuscated at login.
    pub const LOGIN: u16 = 105;

    /// Password (106), text. Sent obfuscated at login.
    pub const PASSWORD: u16 = 106;

    /// Reference number (107), integer — file-transfer handle handed back to
    /// the client to open on the HTXF channel.
    pub const REF_NUM: u16 = 107;

    /// Transfer size in bytes (108), integer.
    pub const TRANSFER_SIZE: u16 = 108;

    /// Chat options (109), integer — e.g. formatted vs. plain.
    pub const CHAT_OPTIONS: u16 = 109;

    /// User access privilege bitmask (110), 8-byte big-endian in practice.
    pub const USER_ACCESS: u16 = 110;

    /// User flags (112), integer — admin / away / refuse-PM / refuse-chat bits.
    pub const USER_FLAGS: u16 = 112;

    /// Options (113), integer.
    pub const OPTIONS: u16 = 113;

    /// Private-chat room id (114), integer.
    pub const CHAT_ID: u16 = 114;

    /// Private-chat room subject (115), text.
    pub const CHAT_SUBJECT: u16 = 115;

    /// Waiting-clients count (116), integer.
    pub const WAITING_COUNT: u16 = 116;

    /// Server version (160), integer.
    pub const VERSION: u16 = 160;

    /// Server name (162), text.
    pub const SERVER_NAME: u16 = 162;
}

/// Well-known transaction type numbers (the `type_` in a
/// [`crate::transaction::TransactionHeader`]).
pub mod transaction {
    /// Error / generic reply marker (0).
    pub const ERROR: u16 = 0;

    /// Get flat-news messages (101).
    pub const GET_MESSAGES: u16 = 101;

    /// Post a flat-news message (102).
    pub const NEW_MESSAGE: u16 = 102;

    /// Server broadcast message to a client (104).
    pub const SERVER_MSG: u16 = 104;

    /// Send a line of public chat (105).
    pub const CHAT_SEND: u16 = 105;

    /// Incoming public chat line pushed to clients (106).
    pub const CHAT_MSG: u16 = 106;

    /// Login (107).
    pub const LOGIN: u16 = 107;

    /// Send a private instant message (108).
    pub const SEND_INSTANT_MSG: u16 = 108;

    /// Show the server agreement (109).
    pub const SHOW_AGREEMENT: u16 = 109;

    /// Disconnect (kick) a user (110).
    pub const DISCONNECT_USER: u16 = 110;

    /// Disconnect message / ban notice (111).
    pub const DISCONNECT_MSG: u16 = 111;

    /// Invite a user to a new private chat (112).
    pub const INVITE_NEW_CHAT: u16 = 112;

    /// Invite a user to an existing private chat (113).
    pub const INVITE_TO_CHAT: u16 = 113;

    /// Reject a private-chat invitation (114).
    pub const REJECT_CHAT_INVITE: u16 = 114;

    /// Join a private chat (115).
    pub const JOIN_CHAT: u16 = 115;

    /// Leave a private chat (116).
    pub const LEAVE_CHAT: u16 = 116;

    /// Notify that a private-chat participant changed (117).
    pub const NOTIFY_CHAT_CHANGE_USER: u16 = 117;

    /// Notify that a private-chat participant left (118).
    pub const NOTIFY_CHAT_DELETE_USER: u16 = 118;

    /// Notify of a private-chat subject change (119).
    pub const NOTIFY_CHAT_SUBJECT: u16 = 119;

    /// Set a private-chat subject (120).
    pub const SET_CHAT_SUBJECT: u16 = 120;

    /// Client accepted the agreement (121).
    pub const AGREED: u16 = 121;

    /// Get the online user name list (300).
    pub const GET_USER_NAME_LIST: u16 = 300;

    /// Push: a user's info changed / joined (301).
    pub const NOTIFY_CHANGE_USER: u16 = 301;

    /// Push: a user disconnected (302).
    pub const NOTIFY_DELETE_USER: u16 = 302;

    /// Get another client's info text (303).
    pub const GET_CLIENT_INFO_TEXT: u16 = 303;

    /// Set this client's user info (name/icon) (304).
    pub const SET_CLIENT_USER_INFO: u16 = 304;

    /// Keep-alive ping (500).
    pub const KEEP_ALIVE: u16 = 500;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_ids_match_wire_values() {
        assert_eq!(field::ERROR_TEXT, 100);
        assert_eq!(field::CHAT_TEXT, 101);
        assert_eq!(field::USER_NAME, 102);
        assert_eq!(field::USER_ID, 103);
        assert_eq!(field::USER_ICON_ID, 104);
        assert_eq!(field::LOGIN, 105);
        assert_eq!(field::PASSWORD, 106);
    }

    #[test]
    fn transaction_types_match_wire_values() {
        assert_eq!(transaction::CHAT_SEND, 105);
        assert_eq!(transaction::LOGIN, 107);
        assert_eq!(transaction::GET_USER_NAME_LIST, 300);
        assert_eq!(transaction::KEEP_ALIVE, 500);
    }
}
