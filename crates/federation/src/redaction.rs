//! Tombstone / redact propagation — server-sovereign.
//!
//! A [`Redaction`] is a signed request from one server to drop a specific
//! event from a board. It is *propagated* across federation but **not
//! obeyed automatically**: each receiving server decides whether to apply it
//! (its own moderators, its own boards, its own rules). This crate models
//! and authenticates the statement; the apply decision lives in the ingest
//! service.
//!
//! Like the descriptor, a redaction is Ed25519-signed over domain-separated
//! canonical bytes ([`REDACTION_CONTEXT`]) by the issuing server's key, so a
//! relayed redaction is verifiable end-to-end and can't be spoofed by an
//! intermediary.

use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

/// Domain separator for redaction signatures.
pub const REDACTION_CONTEXT: &[u8] = b"rhp-fed-redaction-v1";

/// The signable core of a redaction (everything the signature covers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RedactionBody {
    board: String,
    target_event_id: [u8; 32],
    issued_by_server: [u8; 32],
}

/// A signed, propagable request to redact `target_event_id` on `board`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redaction {
    /// The board the target event lives on.
    pub board: String,
    /// Content id of the event to redact.
    pub target_event_id: [u8; 32],
    /// Ed25519 public key of the server issuing the redaction.
    pub issued_by_server: [u8; 32],
    /// Signature over [`REDACTION_CONTEXT`] ‖ postcard(core), by
    /// `issued_by_server`.
    pub sig: Signature,
}

/// Why a redaction failed to verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RedactionError {
    /// The signature does not verify under the issuing server's key.
    #[error("redaction signature does not verify")]
    BadSignature,
    /// The core could not be canonicalized for signing/verification.
    #[error("redaction body does not encode")]
    Encoding,
}

impl Redaction {
    /// Issue a signed redaction. `issued_by_server` is stamped from `key`.
    pub fn sign(
        key: &IdentityKey,
        board: impl Into<String>,
        target_event_id: [u8; 32],
    ) -> Result<Redaction, RedactionError> {
        let body = RedactionBody {
            board: board.into(),
            target_event_id,
            issued_by_server: key.public().0,
        };
        let msg = signed_bytes(&body)?;
        let sig = key.sign(&msg);
        Ok(Redaction {
            board: body.board,
            target_event_id: body.target_event_id,
            issued_by_server: body.issued_by_server,
            sig,
        })
    }

    /// Verify the signature against `issued_by_server`. Success authenticates
    /// the request; whether to *apply* it remains the receiver's decision.
    pub fn verify(&self) -> Result<(), RedactionError> {
        let body = RedactionBody {
            board: self.board.clone(),
            target_event_id: self.target_event_id,
            issued_by_server: self.issued_by_server,
        };
        let msg = signed_bytes(&body)?;
        if PublicKey(self.issued_by_server).verify(&msg, &self.sig) {
            Ok(())
        } else {
            Err(RedactionError::BadSignature)
        }
    }

    /// Wire form (postcard) for propagation.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("redaction serializes")
    }

    /// Decode from bytes; `None` on malformed input (never panics). The
    /// caller must still [`verify`](Self::verify).
    pub fn from_bytes(bytes: &[u8]) -> Option<Redaction> {
        postcard::from_bytes(bytes).ok()
    }
}

/// The exact bytes signed: context ‖ postcard(core).
fn signed_bytes(body: &RedactionBody) -> Result<Vec<u8>, RedactionError> {
    let mut msg = REDACTION_CONTEXT.to_vec();
    msg.extend(postcard::to_allocvec(body).map_err(|_| RedactionError::Encoding)?);
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip_and_wire_form() {
        let key = IdentityKey::from_seed(&[5u8; 32]);
        let r = Redaction::sign(&key, "rabbit.general", [7u8; 32]).unwrap();
        assert_eq!(r.issued_by_server, key.public().0);
        assert_eq!(r.verify(), Ok(()));

        let back = Redaction::from_bytes(&r.to_bytes()).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.verify(), Ok(()));
    }

    #[test]
    fn tampered_target_fails() {
        let key = IdentityKey::from_seed(&[5u8; 32]);
        let mut r = Redaction::sign(&key, "rabbit.general", [7u8; 32]).unwrap();
        r.target_event_id = [8u8; 32];
        assert_eq!(r.verify(), Err(RedactionError::BadSignature));
    }

    #[test]
    fn spoofed_issuer_fails() {
        let key = IdentityKey::from_seed(&[5u8; 32]);
        let mut r = Redaction::sign(&key, "rabbit.general", [7u8; 32]).unwrap();
        r.issued_by_server = IdentityKey::from_seed(&[6u8; 32]).public().0;
        assert_eq!(r.verify(), Err(RedactionError::BadSignature));
    }

    #[test]
    fn decoder_never_panics_on_garbage() {
        assert!(Redaction::from_bytes(&[0xff; 5]).is_none());
    }
}
