//! Welcome screen, theme bundle, and keyword teleport (session family,
//! Wave 2.3), plus the per-account server-theme preference (Wave 8).

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
    /// Structured light-mode design tokens (Wave 8): `--rh-*` custom
    /// property name → value, canonically sorted by name. Colours are hex
    /// (`#rgb`/`#rrggbb`); metrics are simple CSS lengths. The server
    /// validates against a closed grammar plus WCAG contrast rails before
    /// applying (`rabbithole-server-core::theme`) — free-form CSS never
    /// travels here.
    pub tokens_light: Vec<(String, String)>,
    /// Structured dark-mode design tokens (same grammar as `tokens_light`).
    pub tokens_dark: Vec<(String, String)>,
    /// Mode-independent metric tokens (spacing, radii, type scale).
    pub tokens_shared: Vec<(String, String)>,
}

impl ThemeBundle {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            accent_rgb: None,
            logo_ansi: None,
            banner: None,
            icons: Vec::new(),
            tokens_light: Vec::new(),
            tokens_dark: Vec::new(),
            tokens_shared: Vec::new(),
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

// ---------------------------------------------------------------------------
// Per-account server-theme preference (Wave 8): types 57..59.
// ---------------------------------------------------------------------------

/// Read this account's server-theme preference. → [`ThemePrefState`].
/// Accounts only (guests have no stored preferences).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThemePrefGet;

impl Message for ThemePrefGet {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 57;
}

/// Set this account's server-theme preference — the safety valve: with
/// `disable_server_theme` set, [`ThemeGet`] answers `NotFound` for this
/// account and the client renders its default tokens. → [`ThemePrefState`]
/// (the new state). Accounts only.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThemePrefSet {
    pub disable_server_theme: bool,
}

impl ThemePrefSet {
    pub fn new(disable_server_theme: bool) -> Self {
        Self {
            disable_server_theme,
        }
    }
}

impl Message for ThemePrefSet {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 58;
}

/// The account's current server-theme preference.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThemePrefState {
    pub disable_server_theme: bool,
}

impl ThemePrefState {
    pub fn new(disable_server_theme: bool) -> Self {
        Self {
            disable_server_theme,
        }
    }
}

impl Message for ThemePrefState {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 59;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_bundle_roundtrips_with_tokens() {
        let mut bundle = ThemeBundle::new("Wonderland");
        bundle.accent_rgb = Some([1, 2, 3]);
        bundle.logo_ansi = Some("== W8 ==".into());
        bundle.banner = Some([7; 32]);
        bundle.icons = vec![("dm".into(), [9; 32])];
        bundle.tokens_light = vec![("--rh-accent".into(), "#2b63d8".into())];
        bundle.tokens_dark = vec![("--rh-accent".into(), "#6c9cff".into())];
        bundle.tokens_shared = vec![("--rh-radius".into(), ".5rem".into())];
        let bytes = postcard::to_allocvec(&bundle).unwrap();
        let back: ThemeBundle = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, bundle);
    }

    #[test]
    fn theme_pref_roundtrips() {
        for on in [true, false] {
            let bytes = postcard::to_allocvec(&ThemePrefSet::new(on)).unwrap();
            let back: ThemePrefSet = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(back.disable_server_theme, on);
        }
    }
}
