//! Server-signed capability tokens (Wave 5).
//!
//! The origin server is the swarm's trust anchor: a peer serves bytes only
//! to fetchers presenting a capability its own server signed. A token binds
//! **who** (the fetcher's screen name) to **what** (one blake3 root) until
//! **when** (an expiry) — nothing else. Peers verify with the server's
//! public identity key, which every session already learned during hello,
//! so verification needs no extra round trip and works offline from the
//! server once the token is in hand.
//!
//! The signed message is domain-separated (`CAP_CONTEXT`) postcard bytes of
//! the claim, so a capability can never be confused with any other surface
//! (board events, theme bundles) signed by the same key.

use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

/// Domain separator for capability signatures.
pub const CAP_CONTEXT: &[u8] = b"rhp-swarm-cap-v1";

/// What a capability asserts. Kept minimal on purpose: scope growth
/// (ranges, rate classes) belongs in new context versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapClaim {
    /// The one blake3 root this token authorizes fetching.
    pub root: [u8; 32],
    /// The fetcher's screen name (peers may show it in transfer UIs).
    pub fetcher: String,
    /// Unix seconds after which the token is dead.
    pub expires_unix: i64,
}

/// A signed capability: the claim plus the server's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapToken {
    pub claim: CapClaim,
    pub sig: Signature,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapError {
    #[error("signature does not verify")]
    BadSignature,
    #[error("token expired")]
    Expired,
    #[error("token is for a different root")]
    WrongRoot,
    #[error("claim does not encode")]
    Encoding,
}

impl CapToken {
    /// Sign a capability for `fetcher` to fetch `root` until `expires_unix`.
    pub fn issue(
        key: &IdentityKey,
        root: [u8; 32],
        fetcher: impl Into<String>,
        expires_unix: i64,
    ) -> Result<CapToken, CapError> {
        let claim = CapClaim {
            root,
            fetcher: fetcher.into(),
            expires_unix,
        };
        let msg = signed_bytes(&claim)?;
        Ok(CapToken {
            sig: key.sign(&msg),
            claim,
        })
    }

    /// Peer-side check: the signature is the server's, the token is for
    /// `root`, and it hasn't expired at `now_unix`.
    pub fn verify(
        &self,
        server_key: &[u8; 32],
        root: &[u8; 32],
        now_unix: i64,
    ) -> Result<(), CapError> {
        if self.claim.root != *root {
            return Err(CapError::WrongRoot);
        }
        if now_unix >= self.claim.expires_unix {
            return Err(CapError::Expired);
        }
        let msg = signed_bytes(&self.claim)?;
        if !PublicKey(*server_key).verify(&msg, &self.sig) {
            return Err(CapError::BadSignature);
        }
        Ok(())
    }

    /// Wire form (postcard) for carrying the token opaquely in proto
    /// messages and peer hellos.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("token serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<CapToken> {
        postcard::from_bytes(bytes).ok()
    }
}

/// The exact bytes the server signs: context || postcard(claim).
fn signed_bytes(claim: &CapClaim) -> Result<Vec<u8>, CapError> {
    let mut msg = CAP_CONTEXT.to_vec();
    msg.extend(postcard::to_allocvec(claim).map_err(|_| CapError::Encoding)?);
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> IdentityKey {
        IdentityKey::from_seed(&[42u8; 32])
    }

    #[test]
    fn issue_verify_roundtrip_including_wire_form() {
        let k = key();
        let token = CapToken::issue(&k, [1; 32], "alice", 1_000).unwrap();
        assert_eq!(token.verify(&k.public().0, &[1; 32], 999), Ok(()));

        let wire = token.to_bytes();
        let back = CapToken::from_bytes(&wire).unwrap();
        assert_eq!(back, token);
        assert_eq!(back.verify(&k.public().0, &[1; 32], 999), Ok(()));
    }

    #[test]
    fn wrong_root_expired_and_wrong_key_fail() {
        let k = key();
        let token = CapToken::issue(&k, [1; 32], "alice", 1_000).unwrap();

        assert_eq!(
            token.verify(&k.public().0, &[2; 32], 999),
            Err(CapError::WrongRoot)
        );
        assert_eq!(
            token.verify(&k.public().0, &[1; 32], 1_000),
            Err(CapError::Expired),
            "expiry instant itself is dead"
        );
        let other = IdentityKey::from_seed(&[7u8; 32]);
        assert_eq!(
            token.verify(&other.public().0, &[1; 32], 999),
            Err(CapError::BadSignature)
        );
    }

    #[test]
    fn tampered_claim_fails() {
        let k = key();
        let mut token = CapToken::issue(&k, [1; 32], "alice", 1_000).unwrap();
        token.claim.fetcher = "mallory".into();
        assert_eq!(
            token.verify(&k.public().0, &[1; 32], 999),
            Err(CapError::BadSignature)
        );
    }
}
