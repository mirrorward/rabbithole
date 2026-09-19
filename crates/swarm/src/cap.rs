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

/// Domain separator for capabilities a burrow signs for another burrow,
/// fetching a file sent to it: its own context, so a person's token is never
/// taken as a burrow's or the reverse.
pub const S2S_CAP_CONTEXT: &[u8] = b"rhp-swarm-cap-s2s-v1";

/// What a capability for another burrow asserts: that burrow's server key
/// in place of a person's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct S2sCapClaim {
    /// The one blake3 root this token authorizes fetching.
    pub root: [u8; 32],
    /// The server key of the burrow the file was sent to. A peer cannot
    /// check who is fetching (the peer wire has no client authentication),
    /// so this records whom the source meant, as a person's name does.
    pub fetcher_key: [u8; 32],
    /// Unix seconds after which the token is dead.
    pub expires_unix: i64,
}

/// A capability for another burrow: the claim plus the source burrow's
/// signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct S2sCapToken {
    pub claim: S2sCapClaim,
    pub sig: Signature,
}

impl S2sCapToken {
    /// Sign a capability for the burrow `fetcher_key` to fetch `root` from
    /// this burrow's peers until `expires_unix`.
    pub fn issue(
        key: &IdentityKey,
        root: [u8; 32],
        fetcher_key: [u8; 32],
        expires_unix: i64,
    ) -> Result<S2sCapToken, CapError> {
        let claim = S2sCapClaim {
            root,
            fetcher_key,
            expires_unix,
        };
        let msg = s2s_signed_bytes(&claim)?;
        Ok(S2sCapToken {
            sig: key.sign(&msg),
            claim,
        })
    }

    /// Peer-side check, as [`CapToken::verify`], under the burrows' context.
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
        let msg = s2s_signed_bytes(&self.claim)?;
        if !PublicKey(*server_key).verify(&msg, &self.sig) {
            return Err(CapError::BadSignature);
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("token serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<S2sCapToken> {
        postcard::from_bytes(bytes).ok()
    }
}

fn s2s_signed_bytes(claim: &S2sCapClaim) -> Result<Vec<u8>, CapError> {
    let mut msg = S2S_CAP_CONTEXT.to_vec();
    msg.extend(postcard::to_allocvec(claim).map_err(|_| CapError::Encoding)?);
    Ok(msg)
}

/// Whether `token` lets its bearer fetch `root` from a peer of the burrow
/// whose key is `server_key`, at `now_unix`: a person's capability or a
/// burrow's, each checked under its own context. The token carries no tag,
/// so both readings are tried; a signature made under one context never
/// verifies under the other.
pub fn token_allows(token: &[u8], server_key: &[u8; 32], root: &[u8; 32], now_unix: i64) -> bool {
    let person =
        CapToken::from_bytes(token).is_some_and(|t| t.verify(server_key, root, now_unix).is_ok());
    person
        || S2sCapToken::from_bytes(token)
            .is_some_and(|t| t.verify(server_key, root, now_unix).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> IdentityKey {
        IdentityKey::from_seed(&[42u8; 32])
    }

    #[test]
    fn a_burrows_token_and_a_persons_are_never_taken_for_each_other() {
        let k = key();
        let server = k.public().0;
        let burrow = S2sCapToken::issue(&k, [1; 32], [7; 32], 1_000).unwrap();
        assert_eq!(burrow.verify(&server, &[1; 32], 999), Ok(()));
        let wire = burrow.to_bytes();
        assert_eq!(S2sCapToken::from_bytes(&wire).unwrap(), burrow);
        assert!(token_allows(&wire, &server, &[1; 32], 999));
        assert!(!token_allows(&wire, &server, &[1; 32], 1_000), "expired");
        assert!(!token_allows(&wire, &server, &[2; 32], 999), "wrong root");
        assert!(
            !token_allows(&wire, &[9; 32], &[1; 32], 999),
            "wrong burrow"
        );

        // The same claim signed under the person's context does not pass as
        // a burrow's token, nor the reverse.
        let forged = S2sCapToken {
            claim: burrow.claim.clone(),
            sig: k.sign(&{
                let mut m = CAP_CONTEXT.to_vec();
                m.extend(postcard::to_allocvec(&burrow.claim).unwrap());
                m
            }),
        };
        assert_eq!(
            forged.verify(&server, &[1; 32], 999),
            Err(CapError::BadSignature)
        );
        assert!(!token_allows(&forged.to_bytes(), &server, &[1; 32], 999));
        let person = CapToken::issue(&k, [1; 32], "alice", 1_000).unwrap();
        assert!(token_allows(&person.to_bytes(), &server, &[1; 32], 999));
        let as_burrow = S2sCapToken::from_bytes(&person.to_bytes());
        assert!(as_burrow.is_none_or(|t| t.verify(&server, &[1; 32], 999).is_err()));
        assert!(!token_allows(&[], &server, &[1; 32], 999));
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
