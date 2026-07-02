//! Well-known Hotline field ids and transaction type numbers.
//!
//! These are the classic, on-the-wire numeric constants shared by every
//! Hotline client and server since the late 1990s. They are grouped into two
//! submodules: [`field`] for TLV parameter ids and [`transaction`] for
//! transaction types. The core chat/login set landed with the Wave 7.1 codec
//! slice, the news/file sets with W7.4, and the account-admin set (transactions
//! 350-355 and their field aliases) with the account-admin slice.

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

    /// Alias for [`LOGIN`] (105) when it names the target account in the
    /// admin flows (NewUser / DeleteUser / GetUser / SetUser).
    pub const USER_LOGIN: u16 = LOGIN;

    /// Alias for [`PASSWORD`] (106) in the admin flows. Like the login copy it
    /// travels obfuscated (see [`crate::field::obfuscate`]).
    pub const USER_PASSWORD: u16 = PASSWORD;

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

    // ---- Files (200-220) -------------------------------------------------

    /// Packed "file name with info" record (200) returned per directory entry
    /// by GetFileNameList. Layout: `type(4) creator(4) size(4) rsvd(4)
    /// name_script(2) name_len(2) name(name_len)`, all big-endian.
    pub const FILE_NAME_WITH_INFO: u16 = 200;

    /// A bare file/folder name (201), text.
    pub const FILE_NAME: u16 = 201;

    /// A structured file path (202): `count(2)` then per component
    /// `rsvd(2) len(1) name(len)`. The first component is the area slug.
    pub const FILE_PATH: u16 = 202;

    /// File resume data (203), blob — used by resumed transfers (deferred).
    pub const FILE_RESUME_DATA: u16 = 203;

    /// File transfer options (204), integer.
    pub const FILE_TRANSFER_OPTIONS: u16 = 204;

    /// Human-readable file type string (205), text.
    pub const FILE_TYPE_STRING: u16 = 205;

    /// Human-readable file creator string (206), text.
    pub const FILE_CREATOR_STRING: u16 = 206;

    /// File size in bytes (207), integer.
    pub const FILE_SIZE: u16 = 207;

    /// File create date (208), 8-byte classic date.
    pub const FILE_CREATE_DATE: u16 = 208;

    /// File modify date (209), 8-byte classic date.
    pub const FILE_MODIFY_DATE: u16 = 209;

    /// File comment (210), text.
    pub const FILE_COMMENT: u16 = 210;

    /// Four-char file type code (213), blob.
    pub const FILE_TYPE: u16 = 213;

    /// Count of items in a folder (220), integer.
    pub const FOLDER_ITEM_COUNT: u16 = 220;

    /// Quoted original message (214), text — 1.5+ clients attach the text
    /// being replied to when sending an instant message (108); the server
    /// relays it verbatim in the ServerMsg (104) push.
    pub const QUOTING_MSG: u16 = 214;

    /// Automatic-response text (215), text — set via SetClientUserInfo (304);
    /// a non-empty value marks the user away and is echoed back to anyone who
    /// IMs them (an empty value clears it).
    pub const AUTOMATIC_RESPONSE: u16 = 215;

    // ---- News (321-337) --------------------------------------------------

    /// Threaded-news article-list blob (321): the flattened list of a
    /// category's articles, returned by GetNewsArtNameList.
    pub const NEWS_ART_LIST_DATA: u16 = 321;

    /// News category name (322), text.
    pub const NEWS_CAT_NAME: u16 = 322;

    /// News category/bundle list record (323): one per child of a news path,
    /// returned by GetNewsCatNameList.
    pub const NEWS_CAT_LIST_DATA_15: u16 = 323;

    /// Structured news path (325): same wire shape as [`FILE_PATH`]; each
    /// component names a category/bundle.
    pub const NEWS_PATH: u16 = 325;

    /// News article id (326), integer.
    pub const NEWS_ART_ID: u16 = 326;

    /// News article data flavor (327), text — e.g. `text/plain`.
    pub const NEWS_ART_DATA_FLAV: u16 = 327;

    /// News article title (328), text.
    pub const NEWS_ART_TITLE: u16 = 328;

    /// News article poster (329), text.
    pub const NEWS_ART_POSTER: u16 = 329;

    /// News article date (330), 8-byte classic date.
    pub const NEWS_ART_DATE: u16 = 330;

    /// Previous sibling article id (331), integer.
    pub const NEWS_ART_PREV: u16 = 331;

    /// Next sibling article id (332), integer.
    pub const NEWS_ART_NEXT: u16 = 332;

    /// News article body (333), blob.
    pub const NEWS_ART_DATA: u16 = 333;

    /// News article flags (334), integer.
    pub const NEWS_ART_FLAGS: u16 = 334;

    /// Parent article id (335), integer.
    pub const NEWS_ART_PARENT: u16 = 335;

    /// First-child article id (336), integer.
    pub const NEWS_ART_FIRST_CHILD: u16 = 336;
}

/// Well-known transaction type numbers (the `type_` in a
/// [`crate::transaction::TransactionHeader`]).
pub mod transaction {
    /// Error / generic reply marker (0).
    pub const ERROR: u16 = 0;

    /// Get flat-news messages (101).
    pub const GET_MESSAGES: u16 = 101;

    /// Post a flat-news message (102). In this bridge, a client posting to the
    /// flat message board sends this (some clients use [`OLD_POST_NEWS`]).
    pub const NEW_MESSAGE: u16 = 102;

    /// Classic "post flat news" (103) — the older post path some clients use.
    pub const OLD_POST_NEWS: u16 = 103;

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

    // ---- Files (200-213) -------------------------------------------------

    /// Browse a file directory (200).
    pub const GET_FILE_NAME_LIST: u16 = 200;

    /// Negotiate a file download (202); the bulk transfer runs on HTXF.
    pub const DOWNLOAD_FILE: u16 = 202;

    /// Negotiate a file upload (203) — deferred.
    pub const UPLOAD_FILE: u16 = 203;

    /// Delete a file/folder (204).
    pub const DELETE_FILE: u16 = 204;

    /// Create a new folder (205).
    pub const NEW_FOLDER: u16 = 205;

    /// Get a file's info (206).
    pub const GET_FILE_INFO: u16 = 206;

    /// Set a file's info (207).
    pub const SET_FILE_INFO: u16 = 207;

    /// Negotiate a folder download (210) — deferred.
    pub const DOWNLOAD_FOLDER: u16 = 210;

    /// Download info / progress (211) — deferred.
    pub const DOWNLOAD_INFO: u16 = 211;

    // ---- News: threaded (370-411) ----------------------------------------

    /// List news categories/bundles at a path (370).
    pub const GET_NEWS_CAT_NAME_LIST: u16 = 370;

    /// List a category's article threads (371).
    pub const GET_NEWS_ART_NAME_LIST: u16 = 371;

    /// Delete a news category/bundle item (380).
    pub const DEL_NEWS_ITEM: u16 = 380;

    /// Create a news bundle/folder (381).
    pub const NEW_NEWS_FOLDER: u16 = 381;

    /// Create a news category (382).
    pub const NEW_NEWS_CATEGORY: u16 = 382;

    /// Get a single article's data (400).
    pub const GET_NEWS_ART_DATA: u16 = 400;

    /// Post a threaded news article (410).
    pub const POST_NEWS_ART: u16 = 410;

    /// Delete a threaded news article (411).
    pub const DEL_NEWS_ART: u16 = 411;

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

    // ---- Accounts (350-355) ------------------------------------------------

    /// Create a new account (350). Carries USER_NAME (102), USER_LOGIN (105,
    /// obfuscated), USER_PASSWORD (106, obfuscated), and USER_ACCESS (110).
    pub const NEW_USER: u16 = 350;

    /// Delete an account (351). Carries USER_LOGIN (105, obfuscated).
    pub const DELETE_USER: u16 = 351;

    /// Fetch an account for editing (352). The reply carries the account's
    /// name, login, password placeholder, and access bitmap.
    pub const GET_USER: u16 = 352;

    /// Update (or rename) an account (353). Same fields as [`NEW_USER`]; a
    /// changed login in the DATA field renames the account.
    pub const SET_USER: u16 = 353;

    /// Push: this session's access bitmap changed (354). Carries USER_ACCESS
    /// (110) so the client can grey out menus it may no longer use.
    pub const USER_ACCESS: u16 = 354;

    /// Broadcast an admin message to every connected user (355).
    pub const USER_BROADCAST: u16 = 355;

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
        // News + file field ids (added with the W7.4 slice).
        assert_eq!(field::FILE_NAME_WITH_INFO, 200);
        assert_eq!(field::FILE_PATH, 202);
        assert_eq!(field::NEWS_ART_LIST_DATA, 321);
        assert_eq!(field::NEWS_CAT_LIST_DATA_15, 323);
        assert_eq!(field::NEWS_PATH, 325);
        assert_eq!(field::NEWS_ART_DATA, 333);
        // Account-admin field ids (added with the account-admin slice).
        assert_eq!(field::DATA, 101);
        assert_eq!(field::USER_NAME, 102);
        assert_eq!(field::USER_LOGIN, 105);
        assert_eq!(field::USER_PASSWORD, 106);
        assert_eq!(field::USER_ACCESS, 110);
        assert_eq!(field::USER_FLAGS, 112);
        assert_eq!(field::OPTIONS, 113);
        // Private-chat + IM field ids (added with the W7.6 private-chat slice).
        assert_eq!(field::CHAT_ID, 114);
        assert_eq!(field::CHAT_SUBJECT, 115);
        assert_eq!(field::QUOTING_MSG, 214);
        assert_eq!(field::AUTOMATIC_RESPONSE, 215);
    }

    #[test]
    fn transaction_types_match_wire_values() {
        assert_eq!(transaction::CHAT_SEND, 105);
        assert_eq!(transaction::LOGIN, 107);
        assert_eq!(transaction::GET_USER_NAME_LIST, 300);
        assert_eq!(transaction::KEEP_ALIVE, 500);
        // News + file transaction types (added with the W7.4 slice).
        assert_eq!(transaction::GET_FILE_NAME_LIST, 200);
        assert_eq!(transaction::DOWNLOAD_FILE, 202);
        assert_eq!(transaction::GET_NEWS_CAT_NAME_LIST, 370);
        assert_eq!(transaction::GET_NEWS_ART_NAME_LIST, 371);
        assert_eq!(transaction::GET_NEWS_ART_DATA, 400);
        assert_eq!(transaction::POST_NEWS_ART, 410);
        // Account-admin transaction types (added with the account-admin slice).
        assert_eq!(transaction::DISCONNECT_USER, 110);
        assert_eq!(transaction::DISCONNECT_MSG, 111);
        assert_eq!(transaction::GET_CLIENT_INFO_TEXT, 303);
        assert_eq!(transaction::NEW_USER, 350);
        assert_eq!(transaction::DELETE_USER, 351);
        assert_eq!(transaction::GET_USER, 352);
        assert_eq!(transaction::SET_USER, 353);
        assert_eq!(transaction::USER_ACCESS, 354);
        assert_eq!(transaction::USER_BROADCAST, 355);
        // Private-chat transaction types (added with the W7.6 slice).
        assert_eq!(transaction::INVITE_NEW_CHAT, 112);
        assert_eq!(transaction::INVITE_TO_CHAT, 113);
        assert_eq!(transaction::REJECT_CHAT_INVITE, 114);
        assert_eq!(transaction::JOIN_CHAT, 115);
        assert_eq!(transaction::LEAVE_CHAT, 116);
        assert_eq!(transaction::NOTIFY_CHAT_CHANGE_USER, 117);
        assert_eq!(transaction::NOTIFY_CHAT_DELETE_USER, 118);
        assert_eq!(transaction::NOTIFY_CHAT_SUBJECT, 119);
        assert_eq!(transaction::SET_CHAT_SUBJECT, 120);
    }
}
