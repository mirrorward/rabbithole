//! Content-addressed, signed board events — the federation-ready heart of
//! the message-base model (PLAN §6.2, §8.1).
//!
//! A post is an append-only event whose id is `blake3` of its canonical
//! serialization. Events are signed twice: by the author's key (portable
//! identity) and by the origin server's key (routing accountability).
//! Wave 3 mints and verifies these locally; Wave 9 floods them between
//! servers unchanged. Edits and deletions are *follow-up* events, never
//! mutations — the log only grows.

use rabbithole_identity::keys::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

/// What an event does. `#[non_exhaustive]` — federation may introduce
/// kinds a peer doesn't know; unknown kinds are stored and relayed but not
/// acted upon.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventBody {
    /// A new post (or threaded reply when `parent` is set).
    Post {
        board: String,
        /// Root event id of the thread (== id for a top-level post).
        root: Option<[u8; 32]>,
        /// Immediate parent event id (None = top-level).
        parent: Option<[u8; 32]>,
        subject: String,
        body: String,
        /// Body MIME: "text/plain" | "text/markdown" | "text/x-ansi".
        mime: String,
    },
    /// Supersede an earlier post's text (author or moderator).
    Edit {
        target: [u8; 32],
        subject: String,
        body: String,
        mime: String,
    },
    /// Retract a post (author or moderator). Text is dropped on display.
    Tombstone { target: [u8; 32] },
}

/// The signable core of an event (everything the id/signatures cover).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EventCore {
    /// Author identity as `persona@server`.
    author: String,
    /// Author's Ed25519 public key.
    author_key: [u8; 32],
    /// Origin server hostname/id.
    origin: String,
    created_at_unix_ms: i64,
    body: EventBody,
}

/// A complete signed event.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEvent {
    /// blake3 of the canonical `EventCore` — the content id.
    pub id: [u8; 32],
    pub author: String,
    pub author_key: [u8; 32],
    pub origin: String,
    pub created_at_unix_ms: i64,
    pub body: EventBody,
    /// Ed25519 over the canonical core, by the author key (64 bytes).
    pub author_sig: Vec<u8>,
    /// Ed25519 over the canonical core, by the origin server key (64 bytes).
    pub origin_sig: Vec<u8>,
}

/// Canonical bytes of a core: postcard is deterministic for a fixed struct
/// shape, which is what content-addressing needs.
fn canonical(core: &EventCore) -> Vec<u8> {
    postcard::to_allocvec(core).expect("EventCore is serializable")
}

/// Mint a fully-signed event.
pub fn mint(
    author: &str,
    author_key: &IdentityKey,
    origin: &str,
    origin_key: &IdentityKey,
    created_at_unix_ms: i64,
    body: EventBody,
) -> SignedEvent {
    let core = EventCore {
        author: author.to_string(),
        author_key: author_key.public().0,
        origin: origin.to_string(),
        created_at_unix_ms,
        body,
    };
    let bytes = canonical(&core);
    let id = *blake3::hash(&bytes).as_bytes();
    let author_sig = author_key.sign(&bytes).0.to_vec();
    let origin_sig = origin_key.sign(&bytes).0.to_vec();
    SignedEvent {
        id,
        author: core.author,
        author_key: core.author_key,
        origin: core.origin,
        created_at_unix_ms: core.created_at_unix_ms,
        body: core.body,
        author_sig,
        origin_sig,
    }
}

/// Verification outcome for an ingested event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// The id doesn't match the content (tampered or malformed).
    IdMismatch,
    /// The author signature is invalid.
    BadAuthorSig,
    /// The origin server signature is invalid.
    BadOriginSig,
}

impl SignedEvent {
    fn core(&self) -> EventCore {
        EventCore {
            author: self.author.clone(),
            author_key: self.author_key,
            origin: self.origin.clone(),
            created_at_unix_ms: self.created_at_unix_ms,
            body: self.body.clone(),
        }
    }

    /// Verify id + both signatures. `expected_origin_key` is the origin
    /// server's key (looked up from its `.well-known`/tracker entry in
    /// federation; the local key for home-minted events).
    pub fn verify(&self, expected_origin_key: &[u8; 32]) -> Result<(), VerifyError> {
        let bytes = canonical(&self.core());
        if *blake3::hash(&bytes).as_bytes() != self.id {
            return Err(VerifyError::IdMismatch);
        }
        let Ok(author_sig) = <[u8; 64]>::try_from(self.author_sig.as_slice()) else {
            return Err(VerifyError::BadAuthorSig);
        };
        let author_pk = PublicKey(self.author_key);
        if !author_pk.verify(&bytes, &Signature(author_sig)) {
            return Err(VerifyError::BadAuthorSig);
        }
        let Ok(origin_sig) = <[u8; 64]>::try_from(self.origin_sig.as_slice()) else {
            return Err(VerifyError::BadOriginSig);
        };
        let origin_pk = PublicKey(*expected_origin_key);
        if !origin_pk.verify(&bytes, &Signature(origin_sig)) {
            return Err(VerifyError::BadOriginSig);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(board: &str, subject: &str) -> EventBody {
        EventBody::Post {
            board: board.into(),
            root: None,
            parent: None,
            subject: subject.into(),
            body: "hello".into(),
            mime: "text/plain".into(),
        }
    }

    #[test]
    fn mint_and_verify_roundtrip() {
        let author = IdentityKey::generate();
        let origin = IdentityKey::generate();
        let ev = mint(
            "alice@home",
            &author,
            "home",
            &origin,
            1000,
            post("rabbit.general", "hi"),
        );
        assert!(ev.verify(&origin.public().0).is_ok());
    }

    #[test]
    fn id_is_content_addressed_and_stable() {
        let author = IdentityKey::generate();
        let origin = IdentityKey::generate();
        let a = mint("alice@home", &author, "home", &origin, 1000, post("b", "s"));
        let b = mint("alice@home", &author, "home", &origin, 1000, post("b", "s"));
        // Same content + keys + time → same id and signatures (deterministic).
        assert_eq!(a.id, b.id);
        // Different content → different id.
        let c = mint(
            "alice@home",
            &author,
            "home",
            &origin,
            1000,
            post("b", "different"),
        );
        assert_ne!(a.id, c.id);
    }

    #[test]
    fn tampering_is_detected() {
        let author = IdentityKey::generate();
        let origin = IdentityKey::generate();
        let mut ev = mint("alice@home", &author, "home", &origin, 1, post("b", "s"));
        assert!(ev.verify(&origin.public().0).is_ok());

        // Mutate the body without re-signing: id no longer matches.
        ev.body = post("b", "forged");
        assert_eq!(ev.verify(&origin.public().0), Err(VerifyError::IdMismatch));
    }

    #[test]
    fn wrong_origin_key_rejected() {
        let author = IdentityKey::generate();
        let origin = IdentityKey::generate();
        let ev = mint("alice@home", &author, "home", &origin, 1, post("b", "s"));
        let impostor = IdentityKey::generate();
        assert_eq!(
            ev.verify(&impostor.public().0),
            Err(VerifyError::BadOriginSig)
        );
    }

    #[test]
    fn author_sig_bound_to_author_key() {
        let author = IdentityKey::generate();
        let origin = IdentityKey::generate();
        let mut ev = mint("alice@home", &author, "home", &origin, 1, post("b", "s"));
        // Swap in a different author key (as a forwarder might try): the id
        // changes with the key, so this reads as IdMismatch — either way,
        // rejected.
        ev.author_key = IdentityKey::generate().public().0;
        assert!(ev.verify(&origin.public().0).is_err());
    }
}
