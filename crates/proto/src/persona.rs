//! Persona management and registration (session family additions, Wave 2).
//!
//! An account holds up to N personas (AOL's screen-names lesson); each has
//! its own profile card, avatar, and banner. Sessions are bound to one
//! persona at a time and may switch live.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// The lightweight, fun profile card (PLAN §9.1).
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub location: Option<String>,
    pub interests: Option<String>,
    pub quote: Option<String>,
    /// Free text — also served as the finger `.plan` in Wave 6.
    pub plan: Option<String>,
    pub pronouns: Option<String>,
}

impl Profile {
    pub fn new(
        location: Option<String>,
        interests: Option<String>,
        quote: Option<String>,
        plan: Option<String>,
        pronouns: Option<String>,
    ) -> Self {
        Self {
            location,
            interests,
            quote,
            plan,
            pronouns,
        }
    }
}

/// A persona as the wire sees it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaInfo {
    pub id: i64,
    pub screen_name: String,
    pub is_default: bool,
    pub profile: Profile,
    /// blake3 blob ids, fetched via `BlobGet`.
    pub avatar: Option<[u8; 32]>,
    pub banner: Option<[u8; 32]>,
    pub directory_visible: bool,
}

impl PersonaInfo {
    pub fn new(id: i64, screen_name: impl Into<String>) -> Self {
        Self {
            id,
            screen_name: screen_name.into(),
            is_default: false,
            profile: Profile::default(),
            avatar: None,
            banner: None,
            directory_visible: true,
        }
    }
}

/// Create an account (pre-auth). Honors the server's registration mode:
/// open, invite (code required), or closed. → [`crate::session::AuthOk`]
/// (auto-signed-in) or `Forbidden`/`AlreadyExists`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Register {
    pub login: String,
    pub password: String,
    pub invite_code: Option<String>,
}

impl Register {
    pub fn new(
        login: impl Into<String>,
        password: impl Into<String>,
        invite_code: Option<String>,
    ) -> Self {
        Self {
            login: login.into(),
            password: password.into(),
            invite_code,
        }
    }
}

impl Message for Register {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 14;
}

/// List my personas. → [`PersonaList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PersonaListRequest;

impl Message for PersonaListRequest {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 50;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PersonaList {
    pub personas: Vec<PersonaInfo>,
    /// The persona this session is currently using.
    pub active_id: i64,
}

impl PersonaList {
    pub fn new(personas: Vec<PersonaInfo>, active_id: i64) -> Self {
        Self {
            personas,
            active_id,
        }
    }
}

impl Message for PersonaList {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 55;
}

/// Create a persona. → [`PersonaReply`] or `AlreadyExists`/`TooLarge`
/// (persona cap reached → `Forbidden`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaCreate {
    pub screen_name: String,
}

impl PersonaCreate {
    pub fn new(screen_name: impl Into<String>) -> Self {
        Self {
            screen_name: screen_name.into(),
        }
    }
}

impl Message for PersonaCreate {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 51;
}

/// Update a persona's profile/appearance. `None` fields are unchanged;
/// to clear a text field send `Some("")`. → [`PersonaReply`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaUpdate {
    pub id: i64,
    pub profile: Option<Profile>,
    /// `Some(None)` clears the avatar; `Some(Some(id))` sets it.
    pub avatar: Option<Option<[u8; 32]>>,
    pub banner: Option<Option<[u8; 32]>>,
    pub directory_visible: Option<bool>,
}

impl Message for PersonaUpdate {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 52;
}

/// Delete a persona (not the last one; not while another session uses it).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaDelete {
    pub id: i64,
}

impl PersonaDelete {
    pub fn new(id: i64) -> Self {
        Self { id }
    }
}

impl Message for PersonaDelete {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 53;
}

/// Switch this session to another of my personas. → [`PersonaReply`];
/// presence broadcasts the change.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaSwitch {
    pub id: i64,
}

impl PersonaSwitch {
    pub fn new(id: i64) -> Self {
        Self { id }
    }
}

impl Message for PersonaSwitch {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 54;
}

/// Reply carrying a single persona.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaReply {
    pub persona: PersonaInfo,
}

impl PersonaReply {
    pub fn new(persona: PersonaInfo) -> Self {
        Self { persona }
    }
}

impl Message for PersonaReply {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 56;
}

// ---- TOTP enrollment ----------------------------------------------------

/// Begin TOTP enrollment. → [`TotpEnrollInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TotpEnrollBegin;

impl Message for TotpEnrollBegin {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 60;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpEnrollInfo {
    pub secret_base32: String,
    pub provisioning_url: String,
}

impl TotpEnrollInfo {
    pub fn new(secret_base32: impl Into<String>, provisioning_url: impl Into<String>) -> Self {
        Self {
            secret_base32: secret_base32.into(),
            provisioning_url: provisioning_url.into(),
        }
    }
}

impl Message for TotpEnrollInfo {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 61;
}

/// Confirm enrollment with a current code. → [`RecoveryCodes`] (shown once).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpEnrollConfirm {
    pub code: String,
}

impl TotpEnrollConfirm {
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

impl Message for TotpEnrollConfirm {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 62;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RecoveryCodes {
    pub codes: Vec<String>,
}

impl RecoveryCodes {
    pub fn new(codes: Vec<String>) -> Self {
        Self { codes }
    }
}

impl Message for RecoveryCodes {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 63;
}

/// Disable TOTP (requires the account password). → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpDisable {
    pub password: String,
}

impl TotpDisable {
    pub fn new(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
        }
    }
}

impl Message for TotpDisable {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 64;
}

/// Enroll an Ed25519 public key for key auth / event signing. → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEnroll {
    pub pubkey: [u8; 32],
}

impl Message for KeyEnroll {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 65;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, RequestId};

    #[test]
    fn persona_update_tri_state_roundtrips() {
        let update = PersonaUpdate {
            id: 3,
            profile: Some(Profile {
                quote: Some("we're all mad here".into()),
                ..Default::default()
            }),
            avatar: Some(None),          // clear
            banner: Some(Some([9; 32])), // set
            directory_visible: None,     // unchanged
        };
        let frame = Frame::request(RequestId(1), &update).unwrap();
        assert_eq!(frame.decode::<PersonaUpdate>().unwrap().unwrap(), update);
    }
}
