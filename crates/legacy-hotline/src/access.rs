//! Account access bitmaps and user flags.
//!
//! Two small bitmap types travel in account-admin transactions:
//!
//! - [`AccessMask`] — the classic 64-bit privilege bitmap carried by the
//!   USER_ACCESS field (110) in Login replies, GetUser/SetUser/NewUser, and the
//!   UserAccess push (354). Eight raw bytes on the wire.
//! - [`UserFlags`] — the 16-bit status word carried by the USER_FLAGS field
//!   (112) in the online-user list (away / admin / refuse-PM / refuse-chat).
//!
//! ## Two different bit conventions (important!)
//!
//! The access bitmap uses **big-endian bit order**: bit *n* lives in byte
//! `n / 8`, at mask `0x80 >> (n % 8)` — so bit 0 is the **most significant**
//! bit of byte 0. This matches the reference implementations (e.g. Mobius'
//! `AccessBitmap`, which tests `bits[i/8] & (1 << (7 - i%8))`).
//!
//! User flags are the opposite: plain integer (LSB-first) numbering, where
//! flag *n* has value `1 << n` in the big-endian 16-bit word — verified
//! against Mobius, whose `UserFlags.IsSet(i)` reads bit *i* of the u16 as an
//! integer (`big.Int::Bit`). So "away" (flag 0) is wire value `0x0001`.
//!
//! ## Privilege bit numbers
//!
//! The named bits below follow the classic Hotline 1.9 protocol table
//! (`myAcc_*` constants): bits 0-37, with *send private messages* at bit 19.
//! (Mobius' Go constants diverge on that single privilege, placing it at
//! bit 40; the classic table is what 1.x clients were built against.)

use crate::error::HotlineError;
use crate::field::read_int;

/// Bit numbers for the well-known access privileges.
///
/// These are positions in the big-endian bit order described in the module
/// docs (bit 0 = MSB of byte 0), exactly the classic `myAcc_*` table.
pub mod bit {
    /// Delete files (0).
    pub const DELETE_FILES: u8 = 0;
    /// Upload files (1).
    pub const UPLOAD_FILES: u8 = 1;
    /// Download files (2).
    pub const DOWNLOAD_FILES: u8 = 2;
    /// Rename files (3).
    pub const RENAME_FILES: u8 = 3;
    /// Move files (4).
    pub const MOVE_FILES: u8 = 4;
    /// Create folders (5).
    pub const CREATE_FOLDERS: u8 = 5;
    /// Delete folders (6).
    pub const DELETE_FOLDERS: u8 = 6;
    /// Rename folders (7).
    pub const RENAME_FOLDERS: u8 = 7;
    /// Move folders (8).
    pub const MOVE_FOLDERS: u8 = 8;
    /// Read public chat (9).
    pub const READ_CHAT: u8 = 9;
    /// Send public chat (10).
    pub const SEND_CHAT: u8 = 10;
    /// Open (initiate) private chats (11).
    pub const OPEN_CHAT: u8 = 11;
    /// Close private chats (12).
    pub const CLOSE_CHAT: u8 = 12;
    /// Show up in the online user list (13).
    pub const SHOW_IN_LIST: u8 = 13;
    /// Create accounts (14).
    pub const CREATE_USERS: u8 = 14;
    /// Delete accounts (15).
    pub const DELETE_USERS: u8 = 15;
    /// Open (read) accounts (16).
    pub const OPEN_USERS: u8 = 16;
    /// Modify accounts (17).
    pub const MODIFY_USERS: u8 = 17;
    /// Change one's own password (18).
    pub const CHANGE_OWN_PASSWORD: u8 = 18;
    /// Send private messages (19).
    pub const SEND_PRIVATE_MESSAGES: u8 = 19;
    /// Read news articles (20).
    pub const NEWS_READ_ARTICLE: u8 = 20;
    /// Post news articles (21).
    pub const NEWS_POST_ARTICLE: u8 = 21;
    /// Disconnect (kick) users (22).
    pub const DISCONNECT_USERS: u8 = 22;
    /// Cannot be disconnected by others (23).
    pub const CANNOT_BE_DISCONNECTED: u8 = 23;
    /// Get another client's info text (24).
    pub const GET_CLIENT_INFO: u8 = 24;
    /// Upload outside the Uploads folder (25).
    pub const UPLOAD_ANYWHERE: u8 = 25;
    /// Use any display name (26).
    pub const ANY_NAME: u8 = 26;
    /// Skip the agreement (27).
    pub const NO_AGREEMENT: u8 = 27;
    /// Set file comments (28).
    pub const SET_FILE_COMMENT: u8 = 28;
    /// Set folder comments (29).
    pub const SET_FOLDER_COMMENT: u8 = 29;
    /// See inside drop boxes (30).
    pub const VIEW_DROP_BOXES: u8 = 30;
    /// Make aliases (31).
    pub const MAKE_ALIASES: u8 = 31;
    /// Broadcast to all users (32).
    pub const BROADCAST: u8 = 32;
    /// Delete news articles (33).
    pub const NEWS_DELETE_ARTICLE: u8 = 33;
    /// Create news categories (34).
    pub const NEWS_CREATE_CATEGORY: u8 = 34;
    /// Delete news categories (35).
    pub const NEWS_DELETE_CATEGORY: u8 = 35;
    /// Create news bundles/folders (36).
    pub const NEWS_CREATE_FOLDER: u8 = 36;
    /// Delete news bundles/folders (37).
    pub const NEWS_DELETE_FOLDER: u8 = 37;
}

/// A well-known access privilege, mapped to its bit number in the 64-bit mask.
///
/// `Privilege as u8` (or [`Privilege::bit`]) is the bit number from [`bit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Privilege {
    /// Delete files.
    DeleteFiles = bit::DELETE_FILES,
    /// Upload files.
    UploadFiles = bit::UPLOAD_FILES,
    /// Download files.
    DownloadFiles = bit::DOWNLOAD_FILES,
    /// Rename files.
    RenameFiles = bit::RENAME_FILES,
    /// Move files.
    MoveFiles = bit::MOVE_FILES,
    /// Create folders.
    CreateFolders = bit::CREATE_FOLDERS,
    /// Delete folders.
    DeleteFolders = bit::DELETE_FOLDERS,
    /// Rename folders.
    RenameFolders = bit::RENAME_FOLDERS,
    /// Move folders.
    MoveFolders = bit::MOVE_FOLDERS,
    /// Read public chat.
    ReadChat = bit::READ_CHAT,
    /// Send public chat.
    SendChat = bit::SEND_CHAT,
    /// Open (initiate) private chats.
    OpenChat = bit::OPEN_CHAT,
    /// Close private chats.
    CloseChat = bit::CLOSE_CHAT,
    /// Show up in the online user list.
    ShowInList = bit::SHOW_IN_LIST,
    /// Create accounts.
    CreateUsers = bit::CREATE_USERS,
    /// Delete accounts.
    DeleteUsers = bit::DELETE_USERS,
    /// Open (read) accounts.
    OpenUsers = bit::OPEN_USERS,
    /// Modify accounts.
    ModifyUsers = bit::MODIFY_USERS,
    /// Change one's own password.
    ChangeOwnPassword = bit::CHANGE_OWN_PASSWORD,
    /// Send private messages.
    SendPrivateMessages = bit::SEND_PRIVATE_MESSAGES,
    /// Read news articles.
    NewsReadArticle = bit::NEWS_READ_ARTICLE,
    /// Post news articles.
    NewsPostArticle = bit::NEWS_POST_ARTICLE,
    /// Disconnect (kick) users.
    DisconnectUsers = bit::DISCONNECT_USERS,
    /// Cannot be disconnected by others.
    CannotBeDisconnected = bit::CANNOT_BE_DISCONNECTED,
    /// Get another client's info text.
    GetClientInfo = bit::GET_CLIENT_INFO,
    /// Upload outside the Uploads folder.
    UploadAnywhere = bit::UPLOAD_ANYWHERE,
    /// Use any display name.
    AnyName = bit::ANY_NAME,
    /// Skip the agreement.
    NoAgreement = bit::NO_AGREEMENT,
    /// Set file comments.
    SetFileComment = bit::SET_FILE_COMMENT,
    /// Set folder comments.
    SetFolderComment = bit::SET_FOLDER_COMMENT,
    /// See inside drop boxes.
    ViewDropBoxes = bit::VIEW_DROP_BOXES,
    /// Make aliases.
    MakeAliases = bit::MAKE_ALIASES,
    /// Broadcast to all users.
    Broadcast = bit::BROADCAST,
    /// Delete news articles.
    NewsDeleteArticle = bit::NEWS_DELETE_ARTICLE,
    /// Create news categories.
    NewsCreateCategory = bit::NEWS_CREATE_CATEGORY,
    /// Delete news categories.
    NewsDeleteCategory = bit::NEWS_DELETE_CATEGORY,
    /// Create news bundles/folders.
    NewsCreateFolder = bit::NEWS_CREATE_FOLDER,
    /// Delete news bundles/folders.
    NewsDeleteFolder = bit::NEWS_DELETE_FOLDER,
}

impl Privilege {
    /// Every named privilege, in bit order. Handy for iteration and tests.
    pub const ALL: [Privilege; 38] = [
        Privilege::DeleteFiles,
        Privilege::UploadFiles,
        Privilege::DownloadFiles,
        Privilege::RenameFiles,
        Privilege::MoveFiles,
        Privilege::CreateFolders,
        Privilege::DeleteFolders,
        Privilege::RenameFolders,
        Privilege::MoveFolders,
        Privilege::ReadChat,
        Privilege::SendChat,
        Privilege::OpenChat,
        Privilege::CloseChat,
        Privilege::ShowInList,
        Privilege::CreateUsers,
        Privilege::DeleteUsers,
        Privilege::OpenUsers,
        Privilege::ModifyUsers,
        Privilege::ChangeOwnPassword,
        Privilege::SendPrivateMessages,
        Privilege::NewsReadArticle,
        Privilege::NewsPostArticle,
        Privilege::DisconnectUsers,
        Privilege::CannotBeDisconnected,
        Privilege::GetClientInfo,
        Privilege::UploadAnywhere,
        Privilege::AnyName,
        Privilege::NoAgreement,
        Privilege::SetFileComment,
        Privilege::SetFolderComment,
        Privilege::ViewDropBoxes,
        Privilege::MakeAliases,
        Privilege::Broadcast,
        Privilege::NewsDeleteArticle,
        Privilege::NewsCreateCategory,
        Privilege::NewsDeleteCategory,
        Privilege::NewsCreateFolder,
        Privilege::NewsDeleteFolder,
    ];

    /// This privilege's bit number in the 64-bit access mask.
    pub const fn bit(self) -> u8 {
        self as u8
    }
}

/// The classic 64-bit access bitmap (USER_ACCESS field, id 110).
///
/// Eight raw bytes on the wire, in **big-endian bit order**: bit `n` is
/// `0x80 >> (n % 8)` of byte `n / 8`, so bit 0 is the MSB of byte 0. See the
/// module docs for the convention and its provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AccessMask([u8; AccessMask::LEN]);

impl AccessMask {
    /// Wire length of the mask, in bytes.
    pub const LEN: usize = 8;

    /// A mask with no privileges set.
    pub const NONE: AccessMask = AccessMask([0; Self::LEN]);

    /// Wrap the 8-byte wire form.
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// The 8-byte wire form (the USER_ACCESS field value).
    pub const fn to_bytes(self) -> [u8; Self::LEN] {
        self.0
    }

    /// Decode a USER_ACCESS field value. Strict: exactly 8 bytes.
    ///
    /// Shorter input is [`HotlineError::Truncated`]; longer input is
    /// [`HotlineError::TrailingBytes`], so a valid mask round-trips
    /// byte-for-byte.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        if bytes.len() < Self::LEN {
            return Err(HotlineError::Truncated {
                need: Self::LEN - bytes.len(),
                have: bytes.len(),
            });
        }
        if bytes.len() > Self::LEN {
            return Err(HotlineError::TrailingBytes(bytes.len() - Self::LEN));
        }
        let mut out = [0u8; Self::LEN];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Whether bit `n` (big-endian bit order, 0-63) is set.
    ///
    /// Out-of-range bit numbers read as unset — never a panic.
    pub fn bit(&self, n: u8) -> bool {
        if n >= 64 {
            return false;
        }
        self.0[usize::from(n / 8)] & (0x80 >> (n % 8)) != 0
    }

    /// Set or clear bit `n` (big-endian bit order, 0-63).
    ///
    /// Out-of-range bit numbers are ignored — never a panic.
    pub fn set_bit(&mut self, n: u8, on: bool) {
        if n >= 64 {
            return;
        }
        let mask = 0x80 >> (n % 8);
        let byte = &mut self.0[usize::from(n / 8)];
        if on {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }

    /// Whether this mask grants `privilege`.
    pub fn has(&self, privilege: Privilege) -> bool {
        self.bit(privilege.bit())
    }

    /// Grant `privilege` (set its bit).
    pub fn grant(&mut self, privilege: Privilege) {
        self.set_bit(privilege.bit(), true);
    }

    /// Revoke `privilege` (clear its bit).
    pub fn revoke(&mut self, privilege: Privilege) {
        self.set_bit(privilege.bit(), false);
    }
}

impl FromIterator<Privilege> for AccessMask {
    fn from_iter<I: IntoIterator<Item = Privilege>>(iter: I) -> Self {
        let mut mask = AccessMask::NONE;
        for p in iter {
            mask.grant(p);
        }
        mask
    }
}

/// Flag numbers for the well-known user flags (see [`UserFlags`]).
///
/// Unlike the access-mask bits these are plain integer (LSB-first) positions:
/// flag `n` has wire value `1 << n`.
pub mod flag {
    /// User is away (0, wire value `0x0001`).
    pub const AWAY: u8 = 0;
    /// User is an admin — shown in red by classic clients (1, `0x0002`).
    pub const ADMIN: u8 = 1;
    /// User refuses private messages (2, `0x0004`).
    pub const REFUSE_PRIVATE_MESSAGES: u8 = 2;
    /// User refuses private chat (3, `0x0008`).
    pub const REFUSE_PRIVATE_CHAT: u8 = 3;
}

/// The 16-bit user-flags word (USER_FLAGS field, id 112).
///
/// Sent as a size-dependent integer field; the word itself is big-endian on
/// the wire and its flags use plain **LSB-first** integer bit numbering (flag
/// `n` = `1 << n`), *not* the access mask's big-endian bit order.
///
/// Verified against Mobius (`UserFlags.IsSet` reads bit `i` of the big-endian
/// u16 as an integer) and the classic protocol table: away = 0 (`0x0001`),
/// admin = 1 (`0x0002`), refuse private messages = 2 (`0x0004`), refuse
/// private chat = 3 (`0x0008`). Note this differs from folklore lists that
/// start at "admin = 0": the away flag is bit 0. "Automatic response" is not
/// a user flag at all — it travels as its own field in instant-message flows —
/// so no constant is defined for it here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct UserFlags(u16);

impl UserFlags {
    /// Wire length of the flags word when written full-width, in bytes.
    pub const LEN: usize = 2;

    /// No flags set.
    pub const NONE: UserFlags = UserFlags(0);

    /// Wrap a raw 16-bit flags word.
    pub const fn from_word(word: u16) -> Self {
        Self(word)
    }

    /// The raw 16-bit flags word.
    pub const fn word(self) -> u16 {
        self.0
    }

    /// The 2-byte big-endian wire form (the USER_FLAGS field value).
    pub const fn to_bytes(self) -> [u8; Self::LEN] {
        self.0.to_be_bytes()
    }

    /// Wrap the 2-byte big-endian wire form.
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(u16::from_be_bytes(bytes))
    }

    /// Decode a USER_FLAGS field value (a size-dependent integer, so 0-, 1-,
    /// 2-, or 4-byte values are accepted; see [`read_int`]).
    ///
    /// A 4-byte value larger than `u16::MAX` is [`HotlineError::TooLarge`] —
    /// the flags word is only 16 bits.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        let value = read_int(bytes)?;
        u16::try_from(value)
            .map(Self)
            .map_err(|_| HotlineError::TooLarge {
                size: value as usize,
                max: usize::from(u16::MAX),
            })
    }

    /// Whether flag `n` (LSB-first, 0-15) is set.
    ///
    /// Out-of-range flag numbers read as unset — never a panic.
    pub fn bit(&self, n: u8) -> bool {
        if n >= 16 {
            return false;
        }
        self.0 & (1 << n) != 0
    }

    /// Set or clear flag `n` (LSB-first, 0-15).
    ///
    /// Out-of-range flag numbers are ignored — never a panic.
    pub fn set_bit(&mut self, n: u8, on: bool) {
        if n >= 16 {
            return;
        }
        if on {
            self.0 |= 1 << n;
        } else {
            self.0 &= !(1 << n);
        }
    }

    /// Whether the away flag ([`flag::AWAY`]) is set.
    pub fn is_away(&self) -> bool {
        self.bit(flag::AWAY)
    }

    /// Set or clear the away flag.
    pub fn set_away(&mut self, on: bool) {
        self.set_bit(flag::AWAY, on);
    }

    /// Whether the admin flag ([`flag::ADMIN`]) is set.
    pub fn is_admin(&self) -> bool {
        self.bit(flag::ADMIN)
    }

    /// Set or clear the admin flag.
    pub fn set_admin(&mut self, on: bool) {
        self.set_bit(flag::ADMIN, on);
    }

    /// Whether the refuse-private-messages flag is set.
    pub fn refuses_private_messages(&self) -> bool {
        self.bit(flag::REFUSE_PRIVATE_MESSAGES)
    }

    /// Set or clear the refuse-private-messages flag.
    pub fn set_refuse_private_messages(&mut self, on: bool) {
        self.set_bit(flag::REFUSE_PRIVATE_MESSAGES, on);
    }

    /// Whether the refuse-private-chat flag is set.
    pub fn refuses_private_chat(&self) -> bool {
        self.bit(flag::REFUSE_PRIVATE_CHAT)
    }

    /// Set or clear the refuse-private-chat flag.
    pub fn set_refuse_private_chat(&mut self, on: bool) {
        self.set_bit(flag::REFUSE_PRIVATE_CHAT, on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_zero_is_msb_of_byte_zero() {
        let mut mask = AccessMask::NONE;
        mask.set_bit(0, true);
        assert_eq!(mask.to_bytes(), [0x80, 0, 0, 0, 0, 0, 0, 0]);
        assert!(mask.bit(0));
        assert!(!mask.bit(1));
    }

    #[test]
    fn big_endian_bit_order_known_vectors() {
        // bit 7 is the LSB of byte 0; bit 8 the MSB of byte 1; bit 63 the LSB
        // of byte 7.
        let mut mask = AccessMask::NONE;
        mask.set_bit(7, true);
        assert_eq!(mask.to_bytes(), [0x01, 0, 0, 0, 0, 0, 0, 0]);
        mask.set_bit(8, true);
        assert_eq!(mask.to_bytes(), [0x01, 0x80, 0, 0, 0, 0, 0, 0]);
        mask.set_bit(63, true);
        assert_eq!(mask.to_bytes(), [0x01, 0x80, 0, 0, 0, 0, 0, 0x01]);

        // A classic guest-ish trio: download (2) + read chat (9) + send chat
        // (10) => byte0 0x20, byte1 0x60.
        let guest: AccessMask = [
            Privilege::DownloadFiles,
            Privilege::ReadChat,
            Privilege::SendChat,
        ]
        .into_iter()
        .collect();
        assert_eq!(guest.to_bytes(), [0x20, 0x60, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn mask_roundtrip_through_wire_bytes() {
        let mut mask = AccessMask::NONE;
        for n in [0u8, 5, 7, 8, 13, 19, 22, 32, 37, 63] {
            mask.set_bit(n, true);
        }
        let back = AccessMask::decode(&mask.to_bytes()).unwrap();
        assert_eq!(back, mask);
        for n in 0..64u8 {
            let expect = matches!(n, 0 | 5 | 7 | 8 | 13 | 19 | 22 | 32 | 37 | 63);
            assert_eq!(back.bit(n), expect, "bit {n}");
        }
    }

    #[test]
    fn decode_rejects_wrong_lengths() {
        assert!(matches!(
            AccessMask::decode(&[0u8; 7]),
            Err(HotlineError::Truncated { need: 1, have: 7 })
        ));
        assert!(matches!(
            AccessMask::decode(&[0u8; 9]),
            Err(HotlineError::TrailingBytes(1))
        ));
        assert_eq!(AccessMask::decode(&[0u8; 8]).unwrap(), AccessMask::NONE);
    }

    #[test]
    fn out_of_range_bits_never_panic() {
        let mut mask = AccessMask::NONE;
        mask.set_bit(64, true);
        mask.set_bit(255, true);
        assert_eq!(mask, AccessMask::NONE);
        assert!(!mask.bit(64));
        assert!(!mask.bit(255));
    }

    #[test]
    fn every_privilege_maps_to_its_classic_bit() {
        let expected: [(Privilege, u8); 38] = [
            (Privilege::DeleteFiles, 0),
            (Privilege::UploadFiles, 1),
            (Privilege::DownloadFiles, 2),
            (Privilege::RenameFiles, 3),
            (Privilege::MoveFiles, 4),
            (Privilege::CreateFolders, 5),
            (Privilege::DeleteFolders, 6),
            (Privilege::RenameFolders, 7),
            (Privilege::MoveFolders, 8),
            (Privilege::ReadChat, 9),
            (Privilege::SendChat, 10),
            (Privilege::OpenChat, 11),
            (Privilege::CloseChat, 12),
            (Privilege::ShowInList, 13),
            (Privilege::CreateUsers, 14),
            (Privilege::DeleteUsers, 15),
            (Privilege::OpenUsers, 16),
            (Privilege::ModifyUsers, 17),
            (Privilege::ChangeOwnPassword, 18),
            (Privilege::SendPrivateMessages, 19),
            (Privilege::NewsReadArticle, 20),
            (Privilege::NewsPostArticle, 21),
            (Privilege::DisconnectUsers, 22),
            (Privilege::CannotBeDisconnected, 23),
            (Privilege::GetClientInfo, 24),
            (Privilege::UploadAnywhere, 25),
            (Privilege::AnyName, 26),
            (Privilege::NoAgreement, 27),
            (Privilege::SetFileComment, 28),
            (Privilege::SetFolderComment, 29),
            (Privilege::ViewDropBoxes, 30),
            (Privilege::MakeAliases, 31),
            (Privilege::Broadcast, 32),
            (Privilege::NewsDeleteArticle, 33),
            (Privilege::NewsCreateCategory, 34),
            (Privilege::NewsDeleteCategory, 35),
            (Privilege::NewsCreateFolder, 36),
            (Privilege::NewsDeleteFolder, 37),
        ];
        for (privilege, bit) in expected {
            assert_eq!(privilege.bit(), bit, "{privilege:?}");
        }
        // `ALL` covers every variant exactly once, in bit order.
        for (i, p) in Privilege::ALL.iter().enumerate() {
            assert_eq!(usize::from(p.bit()), i);
        }
    }

    #[test]
    fn grant_revoke_has() {
        let mut mask = AccessMask::NONE;
        assert!(!mask.has(Privilege::Broadcast));
        mask.grant(Privilege::Broadcast);
        mask.grant(Privilege::DisconnectUsers);
        assert!(mask.has(Privilege::Broadcast));
        assert!(mask.has(Privilege::DisconnectUsers));
        // Broadcast is bit 32 => MSB of byte 4; disconnect is bit 22 => byte 2
        // mask 0x02.
        assert_eq!(mask.to_bytes()[4], 0x80);
        assert_eq!(mask.to_bytes()[2], 0x02);
        mask.revoke(Privilege::Broadcast);
        assert!(!mask.has(Privilege::Broadcast));
        assert!(mask.has(Privilege::DisconnectUsers));
    }

    #[test]
    fn all_privileges_roundtrip_through_mask() {
        let mask: AccessMask = Privilege::ALL.into_iter().collect();
        for p in Privilege::ALL {
            assert!(mask.has(p), "{p:?}");
        }
        let back = AccessMask::decode(&mask.to_bytes()).unwrap();
        assert_eq!(back, mask);
        // Bits 38-63 stay clear.
        for n in 38..64u8 {
            assert!(!back.bit(n), "bit {n}");
        }
    }

    #[test]
    fn user_flags_wire_values() {
        // LSB-first numbering: away = 0x0001, admin = 0x0002, refuse PM =
        // 0x0004, refuse chat = 0x0008.
        let mut flags = UserFlags::NONE;
        flags.set_away(true);
        assert_eq!(flags.word(), 0x0001);
        flags.set_admin(true);
        assert_eq!(flags.word(), 0x0003);
        flags.set_refuse_private_messages(true);
        flags.set_refuse_private_chat(true);
        assert_eq!(flags.word(), 0x000F);
        assert_eq!(flags.to_bytes(), [0x00, 0x0F]);

        flags.set_away(false);
        assert!(!flags.is_away());
        assert!(flags.is_admin());
        assert!(flags.refuses_private_messages());
        assert!(flags.refuses_private_chat());
        assert_eq!(flags.word(), 0x000E);
    }

    #[test]
    fn user_flags_roundtrip_and_decode_widths() {
        let flags = UserFlags::from_word(0x0203);
        assert_eq!(UserFlags::from_bytes(flags.to_bytes()), flags);
        assert_eq!(UserFlags::decode(&flags.to_bytes()).unwrap(), flags);
        // Size-dependent integer widths: empty = 0, 1-byte, 4-byte in range.
        assert_eq!(UserFlags::decode(&[]).unwrap(), UserFlags::NONE);
        assert_eq!(UserFlags::decode(&[0x02]).unwrap().word(), 2);
        assert_eq!(
            UserFlags::decode(&[0x00, 0x00, 0x00, 0x03]).unwrap().word(),
            3
        );
        // A 4-byte value that overflows the 16-bit word is rejected.
        assert!(matches!(
            UserFlags::decode(&[0x00, 0x01, 0x00, 0x00]),
            Err(HotlineError::TooLarge { .. })
        ));
        // And a bad integer width propagates from `read_int`.
        assert!(matches!(
            UserFlags::decode(&[0, 0, 0]),
            Err(HotlineError::BadIntWidth(3))
        ));
    }

    #[test]
    fn user_flags_out_of_range_bits_never_panic() {
        let mut flags = UserFlags::NONE;
        flags.set_bit(16, true);
        flags.set_bit(255, true);
        assert_eq!(flags, UserFlags::NONE);
        assert!(!flags.bit(16));
        assert!(!flags.bit(255));
    }
}
