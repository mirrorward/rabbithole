//! Session establishment: `Hello` / `HelloAck` and capability negotiation.
//!
//! The first frame on any control stream is a `Hello` request; the server
//! answers with `HelloAck`. Only after version + capability agreement does
//! authentication begin (Wave 1). Additive protocol features are gated on
//! capabilities, not version bumps.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};
use crate::version::ProtocolVersion;

/// A named optional protocol feature.
///
/// String-keyed (not bit-positional) so third-party extensions can
/// namespace their own (`"x-example-thing"`) without a registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Capability(pub String);

impl Capability {
    pub fn new(name: impl Into<String>) -> Self {
        Capability(name.into())
    }
}

/// The set of capabilities a peer offers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet(pub Vec<Capability>);

impl CapabilitySet {
    pub fn contains(&self, name: &str) -> bool {
        self.0.iter().any(|c| c.0 == name)
    }

    /// Capabilities present in both sets.
    pub fn intersect(&self, other: &CapabilitySet) -> CapabilitySet {
        CapabilitySet(
            self.0
                .iter()
                .filter(|c| other.0.contains(c))
                .cloned()
                .collect(),
        )
    }
}

/// Well-known capability names (Wave 0 defines the mechanism; waves add names).
pub mod caps {
    /// Server supports resuming a session with a token + replay cursor.
    pub const SESSION_RESUME: &str = "session-resume";
    /// Server supports Ed25519 challenge/response login.
    pub const KEY_AUTH: &str = "key-auth";
    /// Server allows guest sign-in.
    pub const GUEST: &str = "guest";
}

/// First frame from the connecting peer.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Highest protocol version the client speaks.
    pub version: ProtocolVersion,
    pub capabilities: CapabilitySet,
    /// Client software name, e.g. "rabbit-tui".
    pub client_name: String,
    /// Client software version, e.g. "0.1.0".
    pub client_version: String,
    /// The client's portable Ed25519 identity public key, when it carries one.
    /// The server pins it to this session and surfaces it in presence so peers
    /// can verify who's who across burrows. `None` for handle-only clients.
    /// (Additive field; appended per the additive-only-within-version policy.)
    pub client_pubkey: Option<[u8; 32]>,
}

impl Hello {
    /// Construct a hello offering our native protocol version.
    /// (`#[non_exhaustive]` structs need constructors outside their crate.)
    pub fn new(
        client_name: impl Into<String>,
        client_version: impl Into<String>,
        capabilities: CapabilitySet,
    ) -> Self {
        Hello {
            version: crate::version::PROTOCOL_VERSION,
            capabilities,
            client_name: client_name.into(),
            client_version: client_version.into(),
            client_pubkey: None,
        }
    }

    /// Attach the client's portable identity public key to the handshake.
    pub fn with_pubkey(mut self, pubkey: Option<[u8; 32]>) -> Self {
        self.client_pubkey = pubkey;
        self
    }
}

impl Message for Hello {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 1;
}

/// Server's answer to `Hello`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAck {
    /// The negotiated protocol version (min of the two, if supported).
    pub version: ProtocolVersion,
    /// Capabilities the server offers (client intersects with its own).
    pub capabilities: CapabilitySet,
    /// Server display name.
    pub server_name: String,
    /// Server software version.
    pub server_version: String,
    /// The server's Ed25519 identity public key (32 bytes) — the anchor
    /// for federation identity and theme-bundle/event signatures.
    pub server_key: [u8; 32],
    /// A random challenge nonce, present when the client offered a
    /// `client_pubkey` in its `Hello`. The client proves possession of the
    /// matching private key by signing this nonce and returning a [`KeyProof`];
    /// only then does the server treat the key as verified and surface it in
    /// presence. `None` when no key was offered. (Additive field.)
    pub challenge: Option<[u8; 32]>,
}

impl HelloAck {
    pub fn new(
        version: ProtocolVersion,
        capabilities: CapabilitySet,
        server_name: impl Into<String>,
        server_version: impl Into<String>,
        server_key: [u8; 32],
    ) -> Self {
        HelloAck {
            version,
            capabilities,
            server_name: server_name.into(),
            server_version: server_version.into(),
            server_key,
            challenge: None,
        }
    }

    /// Attach the proof-of-possession challenge nonce (issued when the client
    /// offered a `client_pubkey`).
    pub fn with_challenge(mut self, challenge: Option<[u8; 32]>) -> Self {
        self.challenge = challenge;
        self
    }
}

impl Message for HelloAck {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 2;
}

/// Proof of possession of the `client_pubkey` offered in [`Hello`]: an Ed25519
/// signature over the [`HelloAck::challenge`] nonce. The server verifies it
/// against the claimed key before treating the identity as *verified*; an absent
/// or invalid proof leaves the key unverified (and unpublished in presence).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyProof {
    /// Ed25519 signature (64 bytes) over the challenge nonce.
    pub signature: Vec<u8>,
}

impl KeyProof {
    pub fn new(signature: Vec<u8>) -> Self {
        Self { signature }
    }
}

impl Message for KeyProof {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 3;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, RequestId};
    use crate::version::PROTOCOL_VERSION;

    #[test]
    fn hello_roundtrip_through_frame() {
        let hello = Hello::new(
            "rabbit",
            "0.1.0",
            CapabilitySet(vec![Capability::new(caps::SESSION_RESUME)]),
        );
        assert_eq!(hello.version, PROTOCOL_VERSION);
        let frame = Frame::request(RequestId(1), &hello).unwrap();
        let decoded = frame.decode::<Hello>().unwrap().unwrap();
        assert_eq!(decoded, hello);
    }

    #[test]
    fn hello_carries_optional_pubkey() {
        // Default: no key (the additive field is None).
        let plain = Hello::new("rabbit", "0.1.0", CapabilitySet(vec![]));
        assert_eq!(plain.client_pubkey, None);
        // With a key, it round-trips through the frame intact.
        let keyed = Hello::new("rabbit", "0.1.0", CapabilitySet(vec![])).with_pubkey(Some([9; 32]));
        let frame = Frame::request(RequestId(1), &keyed).unwrap();
        let decoded = frame.decode::<Hello>().unwrap().unwrap();
        assert_eq!(decoded.client_pubkey, Some([9; 32]));
        assert_eq!(decoded, keyed);
    }

    #[test]
    fn hello_ack_carries_optional_challenge() {
        let plain = HelloAck::new(PROTOCOL_VERSION, CapabilitySet::default(), "s", "0", [1; 32]);
        assert_eq!(plain.challenge, None);
        let challenged = HelloAck::new(PROTOCOL_VERSION, CapabilitySet::default(), "s", "0", [1; 32])
            .with_challenge(Some([0xAB; 32]));
        let frame = Frame::request(RequestId(1), &challenged).unwrap();
        let decoded = frame.decode::<HelloAck>().unwrap().unwrap();
        assert_eq!(decoded.challenge, Some([0xAB; 32]));
        assert_eq!(decoded, challenged);
    }

    #[test]
    fn key_proof_roundtrips() {
        let proof = KeyProof::new(vec![9u8; 64]);
        let frame = Frame::request(RequestId(1), &proof).unwrap();
        let decoded = frame.decode::<KeyProof>().unwrap().unwrap();
        assert_eq!(decoded.signature.len(), 64);
        assert_eq!(decoded, proof);
    }

    #[test]
    fn capability_intersection() {
        let a = CapabilitySet(vec![Capability::new("a"), Capability::new("b")]);
        let b = CapabilitySet(vec![Capability::new("b"), Capability::new("c")]);
        let both = a.intersect(&b);
        assert!(both.contains("b"));
        assert!(!both.contains("a"));
        assert!(!both.contains("c"));
    }
}
