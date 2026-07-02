//! Peering handshake and the signed server descriptor.
//!
//! When two servers open an S2S channel (Wave 9 wires the transport) they
//! exchange a [`PeerHello`] / [`PeerHelloAck`] pair announcing who they are
//! and which protocol they speak. Admission (accept/reject) is an admin
//! decision reflected in [`PeerHelloAck::accepted`]; this crate models the
//! bytes, not the approval policy.
//!
//! A [`PeerDescriptor`] is the self-certifying `.well-known/rabbithole/server`
//! document: a public, signed statement of a server's identity, addresses,
//! and features. It is signed with the server's own key over
//! domain-separated canonical bytes ([`DESCRIPTOR_CONTEXT`]) so anyone — a
//! peer, a tracker, or a browser — can fetch it and verify continuity of the
//! server key without a round trip.

use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

/// Domain separator for server descriptor signatures.
pub const DESCRIPTOR_CONTEXT: &[u8] = b"rhp-fed-descriptor-v1";

/// Opening announcement a server sends when dialing a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerHello {
    /// The dialing server's Ed25519 public identity key.
    pub server_key: [u8; 32],
    /// Human-readable server name (e.g. `"rabbithole.example"`).
    pub server_name: String,
    /// Federation protocol version the sender speaks.
    pub protocol_version: u32,
    /// Free-form software id/version (e.g. `"rabbithole/0.5.0"`).
    pub software: String,
}

/// The peer's reply to a [`PeerHello`]: its own identity plus whether it is
/// willing to peer (admin-gated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerHelloAck {
    /// The responding server's Ed25519 public identity key.
    pub server_key: [u8; 32],
    /// The responding server's human-readable name.
    pub server_name: String,
    /// Federation protocol version the responder speaks.
    pub protocol_version: u32,
    /// The responder's software id/version.
    pub software: String,
    /// Whether the responder accepts the peering (admin approval flow).
    pub accepted: bool,
}

/// The signable core of a server descriptor (everything the signature
/// covers). Kept separate from the signature so signing and verification
/// operate on identical bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorBody {
    /// The server's Ed25519 public identity key — the document is
    /// self-certifying: the signature is checked against this key.
    pub server_key: [u8; 32],
    /// Human-readable server name.
    pub name: String,
    /// Reachable addresses (host:port, URLs, or later RNS destinations).
    pub addresses: Vec<String>,
    /// Advertised feature tags (e.g. `"boards"`, `"swarm"`, `"radio"`).
    pub features: Vec<String>,
    /// Issuance time, unix milliseconds — lets consumers prefer the freshest
    /// descriptor and detect rollbacks.
    pub issued_at: i64,
}

/// A signed `.well-known/rabbithole/server` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerDescriptor {
    /// The signed statement.
    pub body: DescriptorBody,
    /// Ed25519 signature over [`DESCRIPTOR_CONTEXT`] ‖ postcard(body), by the
    /// key named in `body.server_key`.
    pub sig: Signature,
}

/// Why a descriptor failed to verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    /// The signature does not verify under the declared server key.
    #[error("descriptor signature does not verify")]
    BadSignature,
    /// The body could not be canonicalized for signing/verification.
    #[error("descriptor body does not encode")]
    Encoding,
}

impl PeerDescriptor {
    /// Sign a descriptor. The declared `server_key` is overwritten with
    /// `key`'s public key so the document always self-certifies.
    pub fn sign(
        key: &IdentityKey,
        mut body: DescriptorBody,
    ) -> Result<PeerDescriptor, DescriptorError> {
        body.server_key = key.public().0;
        let msg = signed_bytes(&body)?;
        Ok(PeerDescriptor {
            sig: key.sign(&msg),
            body,
        })
    }

    /// Verify the signature against the key declared in the body. On success
    /// the caller may trust `body.server_key` as the authenticated identity.
    pub fn verify(&self) -> Result<(), DescriptorError> {
        let msg = signed_bytes(&self.body)?;
        if PublicKey(self.body.server_key).verify(&msg, &self.sig) {
            Ok(())
        } else {
            Err(DescriptorError::BadSignature)
        }
    }

    /// Wire form (postcard) for serving at `.well-known` or relaying via a
    /// tracker.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("descriptor serializes")
    }

    /// Decode a descriptor from bytes. Returns `None` on malformed input
    /// (never panics); the caller must still [`verify`](Self::verify).
    pub fn from_bytes(bytes: &[u8]) -> Option<PeerDescriptor> {
        postcard::from_bytes(bytes).ok()
    }
}

/// The exact bytes signed: context ‖ postcard(body).
fn signed_bytes(body: &DescriptorBody) -> Result<Vec<u8>, DescriptorError> {
    let mut msg = DESCRIPTOR_CONTEXT.to_vec();
    msg.extend(postcard::to_allocvec(body).map_err(|_| DescriptorError::Encoding)?);
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> DescriptorBody {
        DescriptorBody {
            server_key: [0u8; 32],
            name: "rabbithole.example".into(),
            addresses: vec!["quic://rabbithole.example:4433".into()],
            features: vec!["boards".into(), "swarm".into()],
            issued_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn hello_and_ack_postcard_roundtrip() {
        let hello = PeerHello {
            server_key: [7u8; 32],
            server_name: "a".into(),
            protocol_version: 1,
            software: "rabbithole/0.5.0".into(),
        };
        let back: PeerHello =
            postcard::from_bytes(&postcard::to_allocvec(&hello).unwrap()).unwrap();
        assert_eq!(hello, back);

        let ack = PeerHelloAck {
            server_key: [8u8; 32],
            server_name: "b".into(),
            protocol_version: 1,
            software: "rabbithole/0.5.0".into(),
            accepted: true,
        };
        let back: PeerHelloAck =
            postcard::from_bytes(&postcard::to_allocvec(&ack).unwrap()).unwrap();
        assert_eq!(ack, back);
    }

    #[test]
    fn sign_verify_roundtrip_and_wire_form() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let desc = PeerDescriptor::sign(&key, body()).unwrap();
        // signing stamped the real public key into the body.
        assert_eq!(desc.body.server_key, key.public().0);
        assert_eq!(desc.verify(), Ok(()));

        // Round-trips through the wire form and still verifies.
        let wire = desc.to_bytes();
        let back = PeerDescriptor::from_bytes(&wire).unwrap();
        assert_eq!(back, desc);
        assert_eq!(back.verify(), Ok(()));
    }

    #[test]
    fn tampered_body_fails_verification() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let mut desc = PeerDescriptor::sign(&key, body()).unwrap();
        desc.body.addresses.push("quic://evil.example:4433".into());
        assert_eq!(desc.verify(), Err(DescriptorError::BadSignature));
    }

    #[test]
    fn impersonating_key_fails_verification() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let mut desc = PeerDescriptor::sign(&key, body()).unwrap();
        // Claim a different server key without a matching signature.
        desc.body.server_key = IdentityKey::from_seed(&[9u8; 32]).public().0;
        assert_eq!(desc.verify(), Err(DescriptorError::BadSignature));
    }

    #[test]
    fn decoder_never_panics_on_garbage() {
        assert!(PeerDescriptor::from_bytes(&[0xff; 3]).is_none());
        assert!(PeerDescriptor::from_bytes(&[]).is_none());
    }
}
