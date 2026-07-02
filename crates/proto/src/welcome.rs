//! Welcome screen, theme bundle, and keyword teleport (session family,
//! Wave 2.3).

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// Fetch the composed welcome screen. → [`WelcomeScreen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WelcomeScreenRequest;

impl Message for WelcomeScreenRequest {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 42;
}

/// One welcome-screen widget. Order matters; clients render top to bottom
/// and skip variants they don't know.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WelcomeWidget {
    Motd(String),
    /// Unread DM count (accounts only).
    UnreadDms(u64),
    /// Who's on right now (count + a sample of names).
    OnlineNow {
        count: u32,
        sample: Vec<String>,
    },
    /// Operator-featured content.
    Featured {
        title: String,
        body: String,
    },
    /// One-line news ticker.
    Ticker(String),
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WelcomeScreen {
    pub widgets: Vec<WelcomeWidget>,
}

impl WelcomeScreen {
    pub fn new(widgets: Vec<WelcomeWidget>) -> Self {
        Self { widgets }
    }
}

impl Message for WelcomeScreen {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 43;
}

/// Fetch the server's theme bundle. → [`ThemeReply`] (or `NotFound` when
/// the server has no theme configured).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThemeGet;

impl Message for ThemeGet {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 44;
}

/// The theme bundle payload — postcard-encoded inside [`ThemeReply`] so
/// the signature covers stable bytes.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThemeBundle {
    /// Theme display name (usually the server name).
    pub name: String,
    /// Accent color, RGB.
    pub accent_rgb: Option<[u8; 3]>,
    /// ANSI/CP437 logo art (also used by the telnet surface in Wave 6).
    pub logo_ansi: Option<String>,
    /// Raster banner blob (fetched via BlobGet).
    pub banner: Option<[u8; 32]>,
    /// Named icon overrides → blob ids.
    pub icons: Vec<(String, [u8; 32])>,
}

impl ThemeBundle {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            accent_rgb: None,
            logo_ansi: None,
            banner: None,
            icons: Vec::new(),
        }
    }
}

/// Signed theme bundle: `bundle` is postcard-encoded [`ThemeBundle`];
/// `signature` is Ed25519 over those exact bytes with the server identity
/// key from `HelloAck.server_key`. Clients verify before applying and
/// cache by blake3 of `bundle`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeReply {
    pub bundle: Vec<u8>,
    pub signature: Vec<u8>,
}

impl ThemeReply {
    pub fn new(bundle: Vec<u8>, signature: Vec<u8>) -> Self {
        Self { bundle, signature }
    }
}

impl Message for ThemeReply {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 45;
}

/// Keyword teleport (the AOL `/go` primitive). → [`KeywordTarget`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeywordGo {
    pub word: String,
}

impl KeywordGo {
    pub fn new(word: impl Into<String>) -> Self {
        Self { word: word.into() }
    }
}

impl Message for KeywordGo {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 46;
}

/// Where a keyword leads.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeywordKind {
    Room,
    User,
    Url,
    /// Nothing matched (the target echoes the query).
    Unknown,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeywordTarget {
    pub kind: KeywordKind,
    pub target: String,
}

impl KeywordTarget {
    pub fn new(kind: KeywordKind, target: impl Into<String>) -> Self {
        Self {
            kind,
            target: target.into(),
        }
    }
}

impl Message for KeywordTarget {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 47;
}
