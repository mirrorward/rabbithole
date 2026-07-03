//! Reticulum link establishment as a pure, sans-I/O state machine.
//!
//! A *link* is Reticulum's encrypted, forward-secret channel between an
//! initiating peer and a destination (see `RNS.Link` and
//! <https://reticulum.network/manual/understanding.html#link-establishment>).
//! This module models both ends — [`LinkInitiator`] and [`LinkResponder`] —
//! as state machines that consume and produce [`Packet`]s but perform **no
//! I/O, no clock reads, and no implicit randomness**: the caller injects
//! `now_ms` (a monotonic millisecond clock) into every time-sensitive call
//! and supplies the ephemeral key material (or uses the `*_generated`
//! conveniences, which sample the OS CSPRNG exactly like
//! [`Identity::generate`]).
//!
//! This is the pure core that the future RNS gateway sidecar/adapter (PLAN
//! §Wave 14 "RNS transport adapter") will drive from its socket loop;
//! `rabbit://` links gaining RNS destination hashes is a later swarm-crate
//! slice.
//!
//! # Handshake flow and wire formats
//!
//! ```text
//!  Initiator                                            Responder (owns the
//!  ---------                                            destination identity)
//!  LinkRequest  ── packet: LINKREQUEST / SINGLE / ctx NONE ──▶
//!    dest = destination hash                             validate, derive key
//!    +---------------------------+---------------------------+
//!    | initiator eph X25519 pub  | initiator eph Ed25519 pub |
//!    | 32 bytes                  | 32 bytes                  |
//!    +---------------------------+---------------------------+
//!
//!    link id = truncated packet hash (16 bytes) of the request packet —
//!    identical on both sides because hops/transport rewrites are excluded
//!    (see [`Packet::truncated_hash`]).
//!
//!  ◀── LinkProof ── packet: PROOF / LINK / ctx LRPROOF ──
//!        dest = link id                                  state → Handshake
//!    +----------------------------------------+---------------------------+
//!    | Ed25519 signature (64 bytes) over      | responder eph X25519 pub  |
//!    | link_id ‖ responder_x25519_pub ‖       | 32 bytes                  |
//!    | responder_identity_ed25519_pub, by the |                           |
//!    | destination's identity signing key     |                           |
//!    +----------------------------------------+---------------------------+
//!
//!  verify proof, derive key, state → Active, measure RTT
//!
//!  RTT message  ── packet: DATA / LINK / ctx LRRTT, body encrypted ──▶
//!    dest = link id                                      state → Active
//!    plaintext body: +-------------------+
//!                    | rtt_ms  (u64 BE)  |
//!                    +-------------------+
//! ```
//!
//! States traverse `Pending → Handshake → Active → Closed`; on the initiator
//! the `Handshake` step (key derivation) happens inside
//! [`process_proof`](LinkInitiator::process_proof), mirroring upstream where
//! `PENDING → HANDSHAKE → ACTIVE` all occur inside `validate_proof`. A link
//! that is not yet `Active` times out once the injected clock reaches its
//! establishment deadline (checked by [`poll`](LinkInitiator::poll) and by
//! every `process_*` call).
//!
//! # Link cipher
//!
//! The established channel key is `HKDF-SHA256(salt = link_id, ikm = X25519
//! shared secret)` — the same salt upstream uses (`Link.get_salt()` is the
//! link id). Divergences from upstream, in the spirit of the crate-level
//! divergence list:
//!
//! - Upstream expands 32 bytes and encrypts both directions with a
//!   Fernet-like token (AES-128-CBC + HMAC-SHA256, random IV). Here we expand
//!   **64** bytes and split them into two ChaCha20-Poly1305 keys — initiator→
//!   responder and responder→initiator — with deterministic counter nonces
//!   and strict replay rejection, so no randomness is needed after the
//!   handshake. Message framing:
//!
//!   ```text
//!   +------------------+------------------------------+
//!   | counter (u64 BE) | ChaCha20-Poly1305 ct ‖ tag   |
//!   +------------------+------------------------------+
//!   nonce = 0x00_00_00_00 ‖ counter_be   (12 bytes)
//!   ```
//!
//! - // SPEC-CHECK: the HKDF `info` is a crate-versioned string; upstream
//!   passes an empty context. Byte-compatibility is already broken by the
//!   AEAD substitution, so the interop pass must replace this whole cipher
//!   seam (`LinkCipher`) in one place.
//! - // SPEC-CHECK: upstream's RTT message body is a msgpack-packed float of
//!   seconds; [`LinkRtt`] uses a u64 of milliseconds, big-endian, to avoid a
//!   MessagePack dependency for one scalar.
//! - // SPEC-CHECK: [`DEFAULT_ESTABLISHMENT_TIMEOUT_MS`] mirrors upstream's
//!   `ESTABLISHMENT_TIMEOUT_PER_HOP = 6` seconds at a single hop; upstream
//!   scales by path hops and per-interface latency, which a transport slice
//!   must reintroduce.
//! - // SPEC-CHECK: 0.8-era upstream may append link-MTU signalling bytes to
//!   the link request; [`LinkRequest::from_bytes`] requires exactly 64 bytes
//!   and must be relaxed for those peers.
//!
//! Keepalives, `LINKCLOSE` teardown packets, `LINKIDENTIFY`, resources, and
//! channels are deferred to the transport slice; the [`context`] constants
//! for them are already pinned in [`packet`](crate::packet).

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

use crate::crypto::{NONCE_LENGTH, TAG_LENGTH};
use crate::destination::{Destination, DestinationHash, DESTINATION_HASH_LENGTH};
use crate::identity::{
    fill_random, hex_16, Identity, PublicIdentity, KEY_LENGTH, SIGNATURE_LENGTH,
};
use crate::packet::{context, DestinationType, Packet, PacketType};

/// Length in bytes of a link id (a truncated packet hash).
pub const LINK_ID_LENGTH: usize = DESTINATION_HASH_LENGTH;
/// Exact length of a serialized [`LinkRequest`] (two 32-byte public keys).
pub const LINK_REQUEST_LENGTH: usize = KEY_LENGTH * 2;
/// Exact length of a serialized [`LinkProof`] (signature + X25519 public).
pub const LINK_PROOF_LENGTH: usize = SIGNATURE_LENGTH + KEY_LENGTH;
/// Exact length of a serialized [`LinkRtt`] plaintext body.
pub const LINK_RTT_LENGTH: usize = 8;
/// Minimum length of a link message: counter + AEAD tag (empty plaintext).
pub const LINK_MESSAGE_MIN_LENGTH: usize = 8 + TAG_LENGTH;
/// Default establishment deadline: 6 s, upstream's per-hop budget at 1 hop.
pub const DEFAULT_ESTABLISHMENT_TIMEOUT_MS: u64 = 6_000;

/// HKDF `info` string for the link cipher (see the module-level SPEC-CHECK).
const LINK_HKDF_INFO: &[u8] = b"rabbithole-reticulum-interop:link:x25519-chacha20poly1305:v1";

/// Errors from link establishment and link messaging.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The operation is not valid in the link's current state.
    #[error("operation not valid in link state {0:?}")]
    WrongState(LinkState),
    /// The establishment deadline passed; the link is now closed.
    #[error("link establishment deadline passed")]
    TimedOut,
    /// The packet is not the kind (type/context) this handshake step expects.
    #[error("packet is not the expected kind for this handshake step")]
    UnexpectedPacket,
    /// A link request was addressed to a different destination.
    #[error("link request is not addressed to this destination")]
    WrongDestination,
    /// The accepting identity does not own the destination.
    #[error("identity does not own the destination being accepted")]
    NotDestinationOwner,
    /// A proof/RTT packet was addressed to a different link id.
    #[error("packet is not addressed to this link id")]
    WrongLink,
    /// A wire structure had the wrong length or shape.
    #[error("{0} has an invalid length or shape")]
    Malformed(&'static str),
    /// The link proof's Ed25519 signature did not verify.
    #[error("link proof signature is invalid")]
    BadSignature,
    /// The AEAD layer refused to encrypt (counter space exhausted or absurd
    /// plaintext length).
    #[error("link message failed to encrypt")]
    Encrypt,
    /// Authentication/decryption of a link message failed.
    #[error("link message failed to authenticate or decrypt")]
    Decrypt,
    /// A link message counter was replayed or regressed.
    #[error("link message counter {0} replayed or out of order")]
    Replayed(u64),
}

/// Which end of the link this state machine is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRole {
    /// The peer that sent the link request.
    Initiator,
    /// The destination owner that proves the link.
    Responder,
}

/// Link lifecycle states (`RNS.Link` status, condensed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Request sent (initiator) / just constructed, awaiting the next step.
    Pending,
    /// Key derived; awaiting the peer's confirmation (proof or RTT).
    Handshake,
    /// Both sides confirmed; link messages may flow.
    Active,
    /// Torn down; terminal.
    Closed,
}

/// Why a link reached [`LinkState::Closed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The establishment deadline elapsed before the link became active.
    Timeout,
    /// The local caller closed the link.
    Local,
}

/// A 16-byte link id — the truncated hash of the link request packet, and the
/// destination hash that all subsequent link packets are addressed to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkId(pub [u8; LINK_ID_LENGTH]);

impl LinkId {
    /// Derive the link id from a link request packet (either side, before or
    /// after transit — forwarding-mutable fields are excluded from the hash).
    pub fn from_request_packet(packet: &Packet) -> Self {
        Self(packet.truncated_hash())
    }

    /// The raw 16 id bytes.
    pub fn as_bytes(&self) -> &[u8; LINK_ID_LENGTH] {
        &self.0
    }
}

impl From<LinkId> for DestinationHash {
    fn from(id: LinkId) -> Self {
        DestinationHash(id.0)
    }
}

impl core::fmt::Display for LinkId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&hex_16(&self.0))
    }
}

impl core::fmt::Debug for LinkId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "LinkId({self})")
    }
}

/// The link request body: the initiator's two ephemeral public keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    /// Initiator's ephemeral X25519 public key (key agreement).
    pub x25519_pub: [u8; KEY_LENGTH],
    /// Initiator's ephemeral Ed25519 public key (link-layer signing).
    pub ed25519_pub: [u8; KEY_LENGTH],
}

impl LinkRequest {
    /// Serialize as `x25519_pub(32) || ed25519_pub(32)`.
    pub fn to_bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(LINK_REQUEST_LENGTH);
        out.extend_from_slice(&self.x25519_pub);
        out.extend_from_slice(&self.ed25519_pub);
        out
    }

    /// Parse from exactly [`LINK_REQUEST_LENGTH`] bytes (total — see the
    /// module-level SPEC-CHECK about 0.8-era MTU signalling suffixes).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LinkError> {
        if bytes.len() != LINK_REQUEST_LENGTH {
            return Err(LinkError::Malformed("link request"));
        }
        let mut x25519_pub = [0u8; KEY_LENGTH];
        x25519_pub.copy_from_slice(&bytes[..KEY_LENGTH]);
        let mut ed25519_pub = [0u8; KEY_LENGTH];
        ed25519_pub.copy_from_slice(&bytes[KEY_LENGTH..]);
        Ok(Self {
            x25519_pub,
            ed25519_pub,
        })
    }
}

/// The link proof body: the destination identity's signature plus the
/// responder's ephemeral X25519 public key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LinkProof {
    /// Ed25519 signature over `link_id || x25519_pub ||
    /// responder_identity_ed25519_pub` by the destination identity.
    pub signature: [u8; SIGNATURE_LENGTH],
    /// Responder's ephemeral X25519 public key.
    pub x25519_pub: [u8; KEY_LENGTH],
}

impl LinkProof {
    /// Serialize as `signature(64) || x25519_pub(32)`.
    pub fn to_bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(LINK_PROOF_LENGTH);
        out.extend_from_slice(&self.signature);
        out.extend_from_slice(&self.x25519_pub);
        out
    }

    /// Parse from exactly [`LINK_PROOF_LENGTH`] bytes (total).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LinkError> {
        if bytes.len() != LINK_PROOF_LENGTH {
            return Err(LinkError::Malformed("link proof"));
        }
        let mut signature = [0u8; SIGNATURE_LENGTH];
        signature.copy_from_slice(&bytes[..SIGNATURE_LENGTH]);
        let mut x25519_pub = [0u8; KEY_LENGTH];
        x25519_pub.copy_from_slice(&bytes[SIGNATURE_LENGTH..]);
        Ok(Self {
            signature,
            x25519_pub,
        })
    }
}

impl core::fmt::Debug for LinkProof {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Signatures are public data, but 96 hex bytes of Debug noise help
        // nobody; print lengths only.
        f.debug_struct("LinkProof").finish_non_exhaustive()
    }
}

/// The RTT measurement message the initiator sends to activate the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkRtt {
    /// Measured request→proof round trip, in milliseconds.
    pub rtt_ms: u64,
}

impl LinkRtt {
    /// Serialize as a big-endian u64 (see the module-level SPEC-CHECK).
    pub fn to_bytes(self) -> Vec<u8> {
        self.rtt_ms.to_be_bytes().to_vec()
    }

    /// Parse from exactly [`LINK_RTT_LENGTH`] bytes (total).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LinkError> {
        let arr: [u8; LINK_RTT_LENGTH] = bytes
            .try_into()
            .map_err(|_| LinkError::Malformed("link rtt"))?;
        Ok(Self {
            rtt_ms: u64::from_be_bytes(arr),
        })
    }
}

/// Per-direction AEAD state for an established link.
///
/// Derived once per link: 64 bytes of HKDF-SHA256 output split into the
/// initiator→responder key (first 32) and responder→initiator key (last 32).
/// Nonces are deterministic counters, so the cipher needs no randomness and
/// rejects replays by construction.
struct LinkCipher {
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_counter: u64,
    recv_highest: Option<u64>,
}

impl LinkCipher {
    fn derive(shared: &[u8; 32], link_id: &LinkId, role: LinkRole) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(&link_id.0), shared);
        let mut okm = [0u8; 64];
        // `expand` only fails past 255*HashLen output bytes; 64 is fine.
        hk.expand(LINK_HKDF_INFO, &mut okm)
            .expect("HKDF-SHA256 expand of 64 bytes is always valid");
        let mut initiator_to_responder = [0u8; 32];
        initiator_to_responder.copy_from_slice(&okm[..32]);
        let mut responder_to_initiator = [0u8; 32];
        responder_to_initiator.copy_from_slice(&okm[32..]);
        let (send_key, recv_key) = match role {
            LinkRole::Initiator => (initiator_to_responder, responder_to_initiator),
            LinkRole::Responder => (responder_to_initiator, initiator_to_responder),
        };
        Self {
            send_key,
            recv_key,
            send_counter: 0,
            recv_highest: None,
        }
    }

    fn nonce_for(counter: u64) -> [u8; NONCE_LENGTH] {
        let mut nonce = [0u8; NONCE_LENGTH];
        nonce[4..].copy_from_slice(&counter.to_be_bytes());
        nonce
    }

    fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, LinkError> {
        let counter = self.send_counter;
        self.send_counter = counter.checked_add(1).ok_or(LinkError::Encrypt)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.send_key));
        let nonce = Self::nonce_for(counter);
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .map_err(|_| LinkError::Encrypt)?;
        let mut out = Vec::with_capacity(8 + ciphertext.len());
        out.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    fn decrypt(&mut self, message: &[u8]) -> Result<Vec<u8>, LinkError> {
        if message.len() < LINK_MESSAGE_MIN_LENGTH {
            return Err(LinkError::Malformed("link message"));
        }
        let mut counter_bytes = [0u8; 8];
        counter_bytes.copy_from_slice(&message[..8]);
        let counter = u64::from_be_bytes(counter_bytes);
        if let Some(highest) = self.recv_highest {
            if counter <= highest {
                return Err(LinkError::Replayed(counter));
            }
        }
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.recv_key));
        let nonce = Self::nonce_for(counter);
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce), &message[8..])
            .map_err(|_| LinkError::Decrypt)?;
        self.recv_highest = Some(counter);
        Ok(plaintext)
    }
}

impl core::fmt::Debug for LinkCipher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never print key material.
        f.debug_struct("LinkCipher")
            .field("send_counter", &self.send_counter)
            .field("recv_highest", &self.recv_highest)
            .finish_non_exhaustive()
    }
}

/// State shared by both roles: lifecycle, deadline, RTT, and the cipher.
#[derive(Debug)]
struct LinkCore {
    role: LinkRole,
    state: LinkState,
    close_reason: Option<CloseReason>,
    link_id: LinkId,
    started_at_ms: u64,
    deadline_ms: u64,
    rtt_ms: Option<u64>,
    cipher: Option<LinkCipher>,
}

impl LinkCore {
    fn new(role: LinkRole, link_id: LinkId, now_ms: u64, timeout_ms: u64) -> Self {
        Self {
            role,
            state: LinkState::Pending,
            close_reason: None,
            link_id,
            started_at_ms: now_ms,
            deadline_ms: now_ms.saturating_add(timeout_ms),
            rtt_ms: None,
            cipher: None,
        }
    }

    fn close(&mut self, reason: CloseReason) {
        if self.state != LinkState::Closed {
            self.state = LinkState::Closed;
            self.close_reason = Some(reason);
            self.cipher = None;
        }
    }

    /// Advance the establishment timeout against the injected clock.
    fn poll(&mut self, now_ms: u64) -> LinkState {
        if matches!(self.state, LinkState::Pending | LinkState::Handshake)
            && now_ms >= self.deadline_ms
        {
            self.close(CloseReason::Timeout);
        }
        self.state
    }

    /// Like [`poll`](Self::poll), but surfaces a *fresh* expiry as
    /// [`LinkError::TimedOut`] for use at the top of `process_*` calls. A
    /// link that was already closed earlier reports `WrongState` from the
    /// subsequent state check instead.
    fn ensure_not_expired(&mut self, now_ms: u64) -> Result<(), LinkError> {
        if matches!(self.state, LinkState::Pending | LinkState::Handshake)
            && now_ms >= self.deadline_ms
        {
            self.close(CloseReason::Timeout);
            return Err(LinkError::TimedOut);
        }
        Ok(())
    }

    fn require_state(&self, state: LinkState) -> Result<(), LinkError> {
        if self.state != state {
            return Err(LinkError::WrongState(self.state));
        }
        Ok(())
    }

    fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, LinkError> {
        self.require_state(LinkState::Active)?;
        let Some(cipher) = self.cipher.as_mut() else {
            return Err(LinkError::WrongState(self.state));
        };
        cipher.encrypt(plaintext)
    }

    fn decrypt_message(&mut self, message: &[u8]) -> Result<Vec<u8>, LinkError> {
        self.require_state(LinkState::Active)?;
        let Some(cipher) = self.cipher.as_mut() else {
            return Err(LinkError::WrongState(self.state));
        };
        cipher.decrypt(message)
    }
}

/// The initiating end of a link.
///
/// Construct with [`start`](Self::start) (which yields the link request
/// packet to transmit), feed the returning proof packet to
/// [`process_proof`](Self::process_proof) (which yields the RTT packet to
/// transmit), then exchange link messages with
/// [`encrypt_message`](Self::encrypt_message) /
/// [`decrypt_message`](Self::decrypt_message).
pub struct LinkInitiator {
    core: LinkCore,
    eph_secret: StaticSecret,
    eph_signing: SigningKey,
    peer_identity: PublicIdentity,
}

impl LinkInitiator {
    /// Begin establishing a link to `destination`, using caller-supplied
    /// ephemeral key material (`eph_x25519_secret`, `eph_ed25519_seed`) and
    /// clock. Returns the state machine (in [`LinkState::Pending`]) and the
    /// link request packet to transmit.
    pub fn start(
        destination: &Destination,
        eph_x25519_secret: [u8; 32],
        eph_ed25519_seed: [u8; 32],
        now_ms: u64,
        timeout_ms: u64,
    ) -> (Self, Packet) {
        let eph_secret = StaticSecret::from(eph_x25519_secret);
        let eph_signing = SigningKey::from_bytes(&eph_ed25519_seed);
        let request = LinkRequest {
            x25519_pub: XPublicKey::from(&eph_secret).to_bytes(),
            ed25519_pub: eph_signing.verifying_key().to_bytes(),
        };
        let packet = Packet::new_header1(
            DestinationType::Single,
            PacketType::LinkRequest,
            destination.hash(),
            context::NONE,
            request.to_bytes(),
        );
        let link_id = LinkId::from_request_packet(&packet);
        let initiator = Self {
            core: LinkCore::new(LinkRole::Initiator, link_id, now_ms, timeout_ms),
            eph_secret,
            eph_signing,
            peer_identity: *destination.identity(),
        };
        (initiator, packet)
    }

    /// [`start`](Self::start) with fresh OS-CSPRNG ephemerals and the default
    /// establishment timeout.
    pub fn start_generated(destination: &Destination, now_ms: u64) -> (Self, Packet) {
        let mut x = [0u8; 32];
        fill_random(&mut x);
        let mut ed = [0u8; 32];
        fill_random(&mut ed);
        Self::start(destination, x, ed, now_ms, DEFAULT_ESTABLISHMENT_TIMEOUT_MS)
    }

    /// Process the responder's proof packet.
    ///
    /// On success the link derives its cipher, becomes [`LinkState::Active`],
    /// records the measured RTT (`now_ms - start`), and returns the encrypted
    /// RTT packet to transmit. On failure the state is unchanged (except for
    /// deadline expiry, which closes the link).
    pub fn process_proof(&mut self, packet: &Packet, now_ms: u64) -> Result<Packet, LinkError> {
        self.core.ensure_not_expired(now_ms)?;
        self.core.require_state(LinkState::Pending)?;
        if packet.packet_type != PacketType::Proof || packet.context != context::LRPROOF {
            return Err(LinkError::UnexpectedPacket);
        }
        if packet.destination_hash != self.core.link_id.0 {
            return Err(LinkError::WrongLink);
        }
        let proof = LinkProof::from_bytes(&packet.data)?;

        // The responder signs with the destination identity's Ed25519 key
        // over: link_id || responder_x25519_pub || responder_ed25519_pub.
        let mut signed = Vec::with_capacity(LINK_ID_LENGTH + KEY_LENGTH * 2);
        signed.extend_from_slice(&self.core.link_id.0);
        signed.extend_from_slice(&proof.x25519_pub);
        signed.extend_from_slice(&self.peer_identity.ed25519_public());
        if !self.peer_identity.verify(&signed, &proof.signature) {
            return Err(LinkError::BadSignature);
        }

        // Handshake: derive the link cipher from the ECDH shared secret.
        self.core.state = LinkState::Handshake;
        let shared = self
            .eph_secret
            .diffie_hellman(&XPublicKey::from(proof.x25519_pub))
            .to_bytes();
        self.core.cipher = Some(LinkCipher::derive(
            &shared,
            &self.core.link_id,
            LinkRole::Initiator,
        ));

        // Proof verified: the link is active on this side; measure RTT and
        // build the (encrypted) RTT packet that activates the responder.
        self.core.state = LinkState::Active;
        let rtt_ms = now_ms.saturating_sub(self.core.started_at_ms);
        self.core.rtt_ms = Some(rtt_ms);
        let body = self.core.encrypt_message(&LinkRtt { rtt_ms }.to_bytes())?;
        Ok(Packet::new_header1(
            DestinationType::Link,
            PacketType::Data,
            self.core.link_id.0,
            context::LRRTT,
            body,
        ))
    }

    /// Sign `message` with the link's ephemeral Ed25519 key (the one carried
    /// in the link request), as upstream `Link.sign` does for link-layer
    /// proofs and identification.
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LENGTH] {
        self.eph_signing.sign(message).to_bytes()
    }

    /// Advance the establishment timeout; returns the (possibly updated)
    /// state. Active links are not expired here (keepalives are a transport-
    /// slice concern).
    pub fn poll(&mut self, now_ms: u64) -> LinkState {
        self.core.poll(now_ms)
    }

    /// Close the link locally (terminal).
    pub fn close(&mut self) {
        self.core.close(CloseReason::Local);
    }

    /// Encrypt a link message for the peer ([`LinkState::Active`] only).
    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, LinkError> {
        self.core.encrypt_message(plaintext)
    }

    /// Decrypt (and replay-check) a link message from the peer.
    pub fn decrypt_message(&mut self, message: &[u8]) -> Result<Vec<u8>, LinkError> {
        self.core.decrypt_message(message)
    }

    /// This end's role: [`LinkRole::Initiator`].
    pub fn role(&self) -> LinkRole {
        self.core.role
    }

    /// The current lifecycle state.
    pub fn state(&self) -> LinkState {
        self.core.state
    }

    /// Why the link closed, once [`LinkState::Closed`].
    pub fn close_reason(&self) -> Option<CloseReason> {
        self.core.close_reason
    }

    /// The derived link id.
    pub fn link_id(&self) -> LinkId {
        self.core.link_id
    }

    /// The measured request→proof round trip, once the proof arrived.
    pub fn rtt_ms(&self) -> Option<u64> {
        self.core.rtt_ms
    }
}

impl core::fmt::Debug for LinkInitiator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never print ephemeral secrets.
        f.debug_struct("LinkInitiator")
            .field("state", &self.core.state)
            .field("link_id", &self.core.link_id)
            .finish_non_exhaustive()
    }
}

/// The responding (destination-owning) end of a link.
///
/// Construct with [`accept`](Self::accept) on an inbound link request packet
/// (which yields the proof packet to transmit), feed the initiator's RTT
/// packet to [`process_rtt`](Self::process_rtt), then exchange link messages.
pub struct LinkResponder {
    core: LinkCore,
    peer_x25519_pub: [u8; KEY_LENGTH],
    peer_sig_pub: [u8; KEY_LENGTH],
}

impl LinkResponder {
    /// Accept an inbound link request addressed to `destination`, which must
    /// be owned by `identity` (its identity signs the proof). Returns the
    /// state machine (in [`LinkState::Handshake`], key already derived) and
    /// the proof packet to transmit.
    pub fn accept(
        identity: &Identity,
        destination: &Destination,
        packet: &Packet,
        eph_x25519_secret: [u8; 32],
        now_ms: u64,
        timeout_ms: u64,
    ) -> Result<(Self, Packet), LinkError> {
        if packet.packet_type != PacketType::LinkRequest {
            return Err(LinkError::UnexpectedPacket);
        }
        if packet.destination_hash != destination.hash() {
            return Err(LinkError::WrongDestination);
        }
        if identity.public_identity() != *destination.identity() {
            return Err(LinkError::NotDestinationOwner);
        }
        let request = LinkRequest::from_bytes(&packet.data)?;

        let link_id = LinkId::from_request_packet(packet);
        let eph_secret = StaticSecret::from(eph_x25519_secret);
        let x25519_pub = XPublicKey::from(&eph_secret).to_bytes();
        let shared = eph_secret
            .diffie_hellman(&XPublicKey::from(request.x25519_pub))
            .to_bytes();

        let mut core = LinkCore::new(LinkRole::Responder, link_id, now_ms, timeout_ms);
        core.cipher = Some(LinkCipher::derive(&shared, &link_id, LinkRole::Responder));
        core.state = LinkState::Handshake;

        // Prove the link: sign link_id || our ephemeral X25519 public || our
        // identity's Ed25519 public with the destination identity.
        let mut signed = Vec::with_capacity(LINK_ID_LENGTH + KEY_LENGTH * 2);
        signed.extend_from_slice(&link_id.0);
        signed.extend_from_slice(&x25519_pub);
        signed.extend_from_slice(&identity.ed25519_public());
        let proof = LinkProof {
            signature: identity.sign(&signed),
            x25519_pub,
        };
        let proof_packet = Packet::new_header1(
            DestinationType::Link,
            PacketType::Proof,
            link_id.0,
            context::LRPROOF,
            proof.to_bytes(),
        );

        let responder = Self {
            core,
            peer_x25519_pub: request.x25519_pub,
            peer_sig_pub: request.ed25519_pub,
        };
        Ok((responder, proof_packet))
    }

    /// [`accept`](Self::accept) with a fresh OS-CSPRNG ephemeral and the
    /// default establishment timeout.
    pub fn accept_generated(
        identity: &Identity,
        destination: &Destination,
        packet: &Packet,
        now_ms: u64,
    ) -> Result<(Self, Packet), LinkError> {
        let mut x = [0u8; 32];
        fill_random(&mut x);
        Self::accept(
            identity,
            destination,
            packet,
            x,
            now_ms,
            DEFAULT_ESTABLISHMENT_TIMEOUT_MS,
        )
    }

    /// Process the initiator's RTT packet: decrypt it with the link cipher,
    /// record the initiator-measured RTT, and become [`LinkState::Active`].
    /// Returns the RTT in milliseconds.
    pub fn process_rtt(&mut self, packet: &Packet, now_ms: u64) -> Result<u64, LinkError> {
        self.core.ensure_not_expired(now_ms)?;
        self.core.require_state(LinkState::Handshake)?;
        if packet.packet_type != PacketType::Data || packet.context != context::LRRTT {
            return Err(LinkError::UnexpectedPacket);
        }
        if packet.destination_hash != self.core.link_id.0 {
            return Err(LinkError::WrongLink);
        }
        let Some(cipher) = self.core.cipher.as_mut() else {
            return Err(LinkError::WrongState(self.core.state));
        };
        let plaintext = cipher.decrypt(&packet.data)?;
        let rtt = LinkRtt::from_bytes(&plaintext)?;
        self.core.rtt_ms = Some(rtt.rtt_ms);
        self.core.state = LinkState::Active;
        Ok(rtt.rtt_ms)
    }

    /// Verify a peer (initiator) signature against the ephemeral Ed25519 key
    /// carried in the link request. Returns `false` on any malformed
    /// key/signature.
    pub fn verify_peer(&self, message: &[u8], signature: &[u8; SIGNATURE_LENGTH]) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&self.peer_sig_pub) else {
            return false;
        };
        vk.verify(message, &ed25519_dalek::Signature::from_bytes(signature))
            .is_ok()
    }

    /// Advance the establishment timeout; returns the (possibly updated)
    /// state.
    pub fn poll(&mut self, now_ms: u64) -> LinkState {
        self.core.poll(now_ms)
    }

    /// Close the link locally (terminal).
    pub fn close(&mut self) {
        self.core.close(CloseReason::Local);
    }

    /// Encrypt a link message for the peer ([`LinkState::Active`] only).
    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, LinkError> {
        self.core.encrypt_message(plaintext)
    }

    /// Decrypt (and replay-check) a link message from the peer.
    pub fn decrypt_message(&mut self, message: &[u8]) -> Result<Vec<u8>, LinkError> {
        self.core.decrypt_message(message)
    }

    /// This end's role: [`LinkRole::Responder`].
    pub fn role(&self) -> LinkRole {
        self.core.role
    }

    /// The current lifecycle state.
    pub fn state(&self) -> LinkState {
        self.core.state
    }

    /// Why the link closed, once [`LinkState::Closed`].
    pub fn close_reason(&self) -> Option<CloseReason> {
        self.core.close_reason
    }

    /// The derived link id.
    pub fn link_id(&self) -> LinkId {
        self.core.link_id
    }

    /// The initiator-reported RTT, once the RTT message arrived.
    pub fn rtt_ms(&self) -> Option<u64> {
        self.core.rtt_ms
    }

    /// The initiator's ephemeral X25519 public key (from the request).
    pub fn peer_x25519_pub(&self) -> &[u8; KEY_LENGTH] {
        &self.peer_x25519_pub
    }

    /// The initiator's ephemeral Ed25519 public key (from the request).
    pub fn peer_sig_pub(&self) -> &[u8; KEY_LENGTH] {
        &self.peer_sig_pub
    }
}

impl core::fmt::Debug for LinkResponder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never print ephemeral secrets (held inside the cipher).
        f.debug_struct("LinkResponder")
            .field("state", &self.core.state)
            .field("link_id", &self.core.link_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::HeaderType;

    const T0: u64 = 1_000;
    const TIMEOUT: u64 = 6_000;

    fn dest_identity() -> Identity {
        Identity::from_private_bytes(&[0x11; 32], &[0x22; 32])
    }

    fn destination() -> (Identity, Destination) {
        let id = dest_identity();
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow", "link"]);
        (id, dest)
    }

    /// Simulate transit: encode, decode, and bump forwarding-mutable fields.
    fn transit(packet: &Packet) -> Packet {
        let mut p = Packet::decode(&packet.encode().unwrap()).unwrap();
        p.hops = p.hops.wrapping_add(3);
        p
    }

    fn establish() -> (LinkInitiator, LinkResponder) {
        let (id, dest) = destination();
        let (mut ini, request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        let (mut rsp, proof) = LinkResponder::accept(
            &id,
            &dest,
            &transit(&request),
            [0x55; 32],
            T0 + 300,
            TIMEOUT,
        )
        .unwrap();
        let rtt_packet = ini.process_proof(&transit(&proof), T0 + 700).unwrap();
        rsp.process_rtt(&transit(&rtt_packet), T0 + 1_000).unwrap();
        (ini, rsp)
    }

    #[test]
    fn happy_path_traverses_states_and_measures_rtt() {
        let (id, dest) = destination();

        let (mut ini, request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        assert_eq!(ini.state(), LinkState::Pending);
        assert_eq!(ini.role(), LinkRole::Initiator);
        assert_eq!(request.packet_type, PacketType::LinkRequest);
        assert_eq!(request.destination_type, DestinationType::Single);
        assert_eq!(request.destination_hash, dest.hash());
        assert_eq!(request.data.len(), LINK_REQUEST_LENGTH);

        let received_request = transit(&request);
        let (mut rsp, proof) =
            LinkResponder::accept(&id, &dest, &received_request, [0x55; 32], T0 + 300, TIMEOUT)
                .unwrap();
        assert_eq!(rsp.state(), LinkState::Handshake);
        assert_eq!(rsp.role(), LinkRole::Responder);
        assert_eq!(rsp.link_id(), ini.link_id());
        assert_eq!(proof.packet_type, PacketType::Proof);
        assert_eq!(proof.destination_type, DestinationType::Link);
        assert_eq!(proof.context, context::LRPROOF);
        assert_eq!(proof.destination_hash, ini.link_id().0);
        assert_eq!(proof.data.len(), LINK_PROOF_LENGTH);

        let rtt_packet = ini.process_proof(&transit(&proof), T0 + 700).unwrap();
        assert_eq!(ini.state(), LinkState::Active);
        assert_eq!(ini.rtt_ms(), Some(700));
        assert_eq!(rtt_packet.packet_type, PacketType::Data);
        assert_eq!(rtt_packet.context, context::LRRTT);
        assert_eq!(rtt_packet.destination_hash, ini.link_id().0);

        let rtt = rsp.process_rtt(&transit(&rtt_packet), T0 + 1_000).unwrap();
        assert_eq!(rtt, 700);
        assert_eq!(rsp.state(), LinkState::Active);
        assert_eq!(rsp.rtt_ms(), Some(700));
    }

    #[test]
    fn pinned_link_id_vector() {
        // Fully deterministic inputs ⇒ the link id is pinned; this locks the
        // request layout AND the hashable-part rules in one assertion.
        let (_id, dest) = destination();
        let (ini, _request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        assert_eq!(
            ini.link_id().to_string(),
            "8d0050be38b93de9384a92a76a8dd08f"
        );
    }

    #[test]
    fn link_id_survives_transit_mutation() {
        let (_id, dest) = destination();
        let (ini, request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        // Hop bump + reroute through a HEADER_2 transport node.
        let mut mutated = transit(&request);
        mutated.header_type = HeaderType::Header2;
        mutated.transport_id = Some([0xEE; DESTINATION_HASH_LENGTH]);
        let rehashed = Packet::decode(&mutated.encode().unwrap()).unwrap();
        assert_eq!(LinkId::from_request_packet(&rehashed), ini.link_id());
        // And the Display/newtype plumbing agrees with the raw hash.
        assert_eq!(
            DestinationHash::from(ini.link_id()).as_bytes(),
            &rehashed.truncated_hash()
        );
    }

    #[test]
    fn wire_structs_roundtrip_and_reject_bad_lengths() {
        let req = LinkRequest {
            x25519_pub: [1; 32],
            ed25519_pub: [2; 32],
        };
        assert_eq!(LinkRequest::from_bytes(&req.to_bytes()).unwrap(), req);

        let proof = LinkProof {
            signature: [3; 64],
            x25519_pub: [4; 32],
        };
        assert_eq!(LinkProof::from_bytes(&proof.to_bytes()).unwrap(), proof);

        let rtt = LinkRtt {
            rtt_ms: 0x0102_0304_0506_0708,
        };
        assert_eq!(rtt.to_bytes(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(LinkRtt::from_bytes(&rtt.to_bytes()).unwrap(), rtt);

        for len in [0usize, 1, 63, 65, 200] {
            assert_eq!(
                LinkRequest::from_bytes(&vec![0; len]),
                Err(LinkError::Malformed("link request"))
            );
        }
        for len in [0usize, 95, 97] {
            assert_eq!(
                LinkProof::from_bytes(&vec![0; len]),
                Err(LinkError::Malformed("link proof"))
            );
        }
        for len in [0usize, 7, 9] {
            assert_eq!(
                LinkRtt::from_bytes(&vec![0; len]),
                Err(LinkError::Malformed("link rtt"))
            );
        }

        // Arbitrary-input sweep: parsing is total.
        let mut state: u64 = 0xFEED_FACE_0123_4567;
        for _ in 0..2000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = (state >> 56) as usize % 130;
            let mut buf = Vec::with_capacity(len);
            let mut s = state;
            for _ in 0..len {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push((s >> 40) as u8);
            }
            let _ = LinkRequest::from_bytes(&buf);
            let _ = LinkProof::from_bytes(&buf);
            let _ = LinkRtt::from_bytes(&buf);
        }
    }

    #[test]
    fn proof_from_wrong_identity_rejected() {
        let (_id, dest) = destination();
        let (mut ini, _request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);

        // A mallory identity forges a proof over the correct signed-content
        // shape but with its own signing key.
        let mallory = Identity::from_private_bytes(&[0x66; 32], &[0x77; 32]);
        let eph = StaticSecret::from([0x55; 32]);
        let x25519_pub = XPublicKey::from(&eph).to_bytes();
        let mut signed = Vec::new();
        signed.extend_from_slice(&ini.link_id().0);
        signed.extend_from_slice(&x25519_pub);
        signed.extend_from_slice(&mallory.ed25519_public());
        let forged = LinkProof {
            signature: mallory.sign(&signed),
            x25519_pub,
        };
        let packet = Packet::new_header1(
            DestinationType::Link,
            PacketType::Proof,
            ini.link_id().0,
            context::LRPROOF,
            forged.to_bytes(),
        );
        assert_eq!(
            ini.process_proof(&packet, T0 + 500),
            Err(LinkError::BadSignature)
        );
        // Failure leaves the link pending (upstream keeps waiting too).
        assert_eq!(ini.state(), LinkState::Pending);
    }

    #[test]
    fn tampered_proof_fields_rejected() {
        let (id, dest) = destination();
        let (mut ini, request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        let (_rsp, proof) = LinkResponder::accept(
            &id,
            &dest,
            &transit(&request),
            [0x55; 32],
            T0 + 300,
            TIMEOUT,
        )
        .unwrap();

        // Tampered responder key: the signature covers it.
        let mut swapped_key = proof.clone();
        swapped_key.data[SIGNATURE_LENGTH] ^= 0xFF;
        assert_eq!(
            ini.process_proof(&swapped_key, T0 + 500),
            Err(LinkError::BadSignature)
        );

        // Tampered signature byte.
        let mut bad_sig = proof.clone();
        bad_sig.data[0] ^= 0xFF;
        assert_eq!(
            ini.process_proof(&bad_sig, T0 + 500),
            Err(LinkError::BadSignature)
        );

        // Truncated proof body.
        let mut short = proof.clone();
        short.data.truncate(LINK_PROOF_LENGTH - 1);
        assert_eq!(
            ini.process_proof(&short, T0 + 500),
            Err(LinkError::Malformed("link proof"))
        );

        // Wrong link id in the address field.
        let mut misaddressed = proof.clone();
        misaddressed.destination_hash = [0xAB; DESTINATION_HASH_LENGTH];
        assert_eq!(
            ini.process_proof(&misaddressed, T0 + 500),
            Err(LinkError::WrongLink)
        );

        // Wrong packet type / context.
        let mut wrong_type = proof.clone();
        wrong_type.packet_type = PacketType::Data;
        assert_eq!(
            ini.process_proof(&wrong_type, T0 + 500),
            Err(LinkError::UnexpectedPacket)
        );
        let mut wrong_ctx = proof.clone();
        wrong_ctx.context = context::NONE;
        assert_eq!(
            ini.process_proof(&wrong_ctx, T0 + 500),
            Err(LinkError::UnexpectedPacket)
        );

        // After all those rejections, the genuine proof still establishes.
        assert_eq!(ini.state(), LinkState::Pending);
        assert!(ini.process_proof(&proof, T0 + 700).is_ok());
        assert_eq!(ini.state(), LinkState::Active);
    }

    #[test]
    fn accept_rejects_bad_requests() {
        let (id, dest) = destination();
        let (_ini, request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);

        let mut wrong_type = request.clone();
        wrong_type.packet_type = PacketType::Data;
        assert!(matches!(
            LinkResponder::accept(&id, &dest, &wrong_type, [0x55; 32], T0, TIMEOUT),
            Err(LinkError::UnexpectedPacket)
        ));

        let mut wrong_dest = request.clone();
        wrong_dest.destination_hash = [0x00; DESTINATION_HASH_LENGTH];
        assert!(matches!(
            LinkResponder::accept(&id, &dest, &wrong_dest, [0x55; 32], T0, TIMEOUT),
            Err(LinkError::WrongDestination)
        ));

        let mut short = request.clone();
        short.data.truncate(10);
        assert!(matches!(
            LinkResponder::accept(&id, &dest, &short, [0x55; 32], T0, TIMEOUT),
            Err(LinkError::Malformed("link request"))
        ));

        // An identity that does not own the destination cannot accept it.
        let mallory = Identity::from_private_bytes(&[0x66; 32], &[0x77; 32]);
        assert!(matches!(
            LinkResponder::accept(&mallory, &dest, &request, [0x55; 32], T0, TIMEOUT),
            Err(LinkError::NotDestinationOwner)
        ));
    }

    #[test]
    fn initiator_times_out() {
        let (_id, dest) = destination();
        let (mut ini, _request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        assert_eq!(ini.poll(T0 + TIMEOUT - 1), LinkState::Pending);
        assert_eq!(ini.poll(T0 + TIMEOUT), LinkState::Closed);
        assert_eq!(ini.close_reason(), Some(CloseReason::Timeout));
        // Terminal: a late proof is refused.
        let bogus = Packet::new_header1(
            DestinationType::Link,
            PacketType::Proof,
            ini.link_id().0,
            context::LRPROOF,
            vec![0; LINK_PROOF_LENGTH],
        );
        assert_eq!(
            ini.process_proof(&bogus, T0 + TIMEOUT + 1),
            Err(LinkError::WrongState(LinkState::Closed))
        );
    }

    #[test]
    fn process_calls_surface_deadline_expiry() {
        let (id, dest) = destination();
        let (mut ini, request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        let (mut rsp, proof) =
            LinkResponder::accept(&id, &dest, &transit(&request), [0x55; 32], T0, TIMEOUT).unwrap();

        // The proof arrives after the initiator's deadline: TimedOut + Closed.
        assert_eq!(
            ini.process_proof(&transit(&proof), T0 + TIMEOUT),
            Err(LinkError::TimedOut)
        );
        assert_eq!(ini.state(), LinkState::Closed);
        assert_eq!(ini.close_reason(), Some(CloseReason::Timeout));

        // The responder waits for an RTT that never comes.
        assert_eq!(rsp.poll(T0 + TIMEOUT - 1), LinkState::Handshake);
        let stale_rtt = Packet::new_header1(
            DestinationType::Link,
            PacketType::Data,
            rsp.link_id().0,
            context::LRRTT,
            vec![0; LINK_MESSAGE_MIN_LENGTH],
        );
        assert_eq!(
            rsp.process_rtt(&stale_rtt, T0 + TIMEOUT),
            Err(LinkError::TimedOut)
        );
        assert_eq!(rsp.state(), LinkState::Closed);
        assert_eq!(rsp.close_reason(), Some(CloseReason::Timeout));
    }

    #[test]
    fn established_link_encrypts_bidirectionally() {
        let (mut ini, mut rsp) = establish();

        let to_responder = ini.encrypt_message(b"down the rabbit hole").unwrap();
        assert_eq!(
            rsp.decrypt_message(&to_responder).unwrap(),
            b"down the rabbit hole"
        );

        let to_initiator = rsp.encrypt_message(b"welcome to the warren").unwrap();
        assert_eq!(
            ini.decrypt_message(&to_initiator).unwrap(),
            b"welcome to the warren"
        );

        // Directional keys differ: the sender cannot decrypt its own frame.
        let own = ini.encrypt_message(b"echo").unwrap();
        assert_eq!(ini.decrypt_message(&own), Err(LinkError::Decrypt));
    }

    #[test]
    fn replay_and_tampering_rejected() {
        let (mut ini, mut rsp) = establish();

        let msg = ini.encrypt_message(b"once").unwrap();
        assert!(rsp.decrypt_message(&msg).is_ok());
        // Exact replay: counter regression.
        let counter = u64::from_be_bytes(msg[..8].try_into().unwrap());
        assert_eq!(rsp.decrypt_message(&msg), Err(LinkError::Replayed(counter)));

        // Tampered ciphertext.
        let mut tampered = ini.encrypt_message(b"twice").unwrap();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        assert_eq!(rsp.decrypt_message(&tampered), Err(LinkError::Decrypt));

        // Too short to even carry a counter + tag.
        assert_eq!(
            rsp.decrypt_message(&[0u8; LINK_MESSAGE_MIN_LENGTH - 1]),
            Err(LinkError::Malformed("link message"))
        );

        // A failed decrypt does not advance the replay window: a fresh frame
        // (skipping the burned counter) still decrypts.
        let genuine = ini.encrypt_message(b"thrice").unwrap();
        assert_eq!(rsp.decrypt_message(&genuine).unwrap(), b"thrice");
    }

    #[test]
    fn rtt_body_is_encrypted_and_tamperproof() {
        let (id, dest) = destination();
        let (mut ini, request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        let (mut rsp, proof) = LinkResponder::accept(
            &id,
            &dest,
            &transit(&request),
            [0x55; 32],
            T0 + 300,
            TIMEOUT,
        )
        .unwrap();
        let rtt_packet = ini.process_proof(&transit(&proof), T0 + 700).unwrap();

        // The plaintext RTT must not appear in the packet body.
        assert_ne!(rtt_packet.data.len(), LINK_RTT_LENGTH);
        assert!(rtt_packet.data.len() >= LINK_MESSAGE_MIN_LENGTH + LINK_RTT_LENGTH);

        // Tampering with the encrypted body keeps the responder in Handshake.
        let mut tampered = rtt_packet.clone();
        let last = tampered.data.len() - 1;
        tampered.data[last] ^= 0xFF;
        assert_eq!(
            rsp.process_rtt(&tampered, T0 + 900),
            Err(LinkError::Decrypt)
        );
        assert_eq!(rsp.state(), LinkState::Handshake);

        // Wrong kind of packet.
        let mut wrong_ctx = rtt_packet.clone();
        wrong_ctx.context = context::NONE;
        assert_eq!(
            rsp.process_rtt(&wrong_ctx, T0 + 900),
            Err(LinkError::UnexpectedPacket)
        );
        let mut misaddressed = rtt_packet.clone();
        misaddressed.destination_hash = [0xCD; DESTINATION_HASH_LENGTH];
        assert_eq!(
            rsp.process_rtt(&misaddressed, T0 + 900),
            Err(LinkError::WrongLink)
        );

        // The genuine packet still activates. (Replay protection does not
        // interfere: failed decrypts never advanced the window.)
        assert_eq!(rsp.process_rtt(&rtt_packet, T0 + 1_000), Ok(700));
        assert_eq!(rsp.state(), LinkState::Active);
        // A second RTT is refused in Active.
        assert_eq!(
            rsp.process_rtt(&rtt_packet, T0 + 1_100),
            Err(LinkError::WrongState(LinkState::Active))
        );
    }

    #[test]
    fn messaging_requires_active_state() {
        let (id, dest) = destination();
        let (mut ini, request) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        assert_eq!(
            ini.encrypt_message(b"early"),
            Err(LinkError::WrongState(LinkState::Pending))
        );
        assert_eq!(
            ini.decrypt_message(&[0u8; 24]),
            Err(LinkError::WrongState(LinkState::Pending))
        );

        let (mut rsp, _proof) =
            LinkResponder::accept(&id, &dest, &transit(&request), [0x55; 32], T0, TIMEOUT).unwrap();
        assert_eq!(
            rsp.encrypt_message(b"early"),
            Err(LinkError::WrongState(LinkState::Handshake))
        );

        // Closed links refuse messaging too.
        let (mut ini2, mut rsp2) = establish();
        ini2.close();
        assert_eq!(ini2.close_reason(), Some(CloseReason::Local));
        assert_eq!(
            ini2.encrypt_message(b"late"),
            Err(LinkError::WrongState(LinkState::Closed))
        );
        rsp2.close();
        assert_eq!(
            rsp2.decrypt_message(&[0u8; 24]),
            Err(LinkError::WrongState(LinkState::Closed))
        );
    }

    #[test]
    fn close_is_sticky_and_local() {
        let (mut ini, _rsp) = establish();
        assert_eq!(ini.state(), LinkState::Active);
        ini.close();
        assert_eq!(ini.state(), LinkState::Closed);
        assert_eq!(ini.close_reason(), Some(CloseReason::Local));
        // Polling long after cannot re-reason the close as a timeout.
        assert_eq!(ini.poll(u64::MAX), LinkState::Closed);
        assert_eq!(ini.close_reason(), Some(CloseReason::Local));
    }

    #[test]
    fn active_links_do_not_establishment_timeout() {
        let (mut ini, mut rsp) = establish();
        assert_eq!(ini.poll(u64::MAX), LinkState::Active);
        assert_eq!(rsp.poll(u64::MAX), LinkState::Active);
    }

    #[test]
    fn link_layer_signing_seam_roundtrips() {
        let (ini, rsp) = establish();
        let sig = ini.sign(b"identify: white rabbit");
        assert!(rsp.verify_peer(b"identify: white rabbit", &sig));
        assert!(!rsp.verify_peer(b"identify: march hare", &sig));
    }

    #[test]
    fn generated_conveniences_establish_end_to_end() {
        let id = Identity::generate();
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow", "link"]);
        let (mut ini, request) = LinkInitiator::start_generated(&dest, T0);
        let (mut rsp, proof) =
            LinkResponder::accept_generated(&id, &dest, &transit(&request), T0 + 10).unwrap();
        let rtt_packet = ini.process_proof(&transit(&proof), T0 + 25).unwrap();
        assert_eq!(rsp.process_rtt(&transit(&rtt_packet), T0 + 40), Ok(25));
        let m = ini.encrypt_message(b"generated").unwrap();
        assert_eq!(rsp.decrypt_message(&m).unwrap(), b"generated");
    }

    #[test]
    fn distinct_ephemerals_produce_distinct_links_and_keys() {
        let (id, dest) = destination();
        let (mut a_ini, a_req) = LinkInitiator::start(&dest, [0x33; 32], [0x44; 32], T0, TIMEOUT);
        let (mut b_ini, b_req) = LinkInitiator::start(&dest, [0x99; 32], [0x44; 32], T0, TIMEOUT);
        assert_ne!(a_ini.link_id(), b_ini.link_id());

        let (mut a_rsp, a_proof) =
            LinkResponder::accept(&id, &dest, &a_req, [0x55; 32], T0, TIMEOUT).unwrap();
        let (mut b_rsp, b_proof) =
            LinkResponder::accept(&id, &dest, &b_req, [0x56; 32], T0, TIMEOUT).unwrap();
        let a_rtt = a_ini.process_proof(&a_proof, T0 + 1).unwrap();
        let b_rtt = b_ini.process_proof(&b_proof, T0 + 1).unwrap();
        a_rsp.process_rtt(&a_rtt, T0 + 2).unwrap();
        b_rsp.process_rtt(&b_rtt, T0 + 2).unwrap();

        // A frame from link A cannot cross into link B.
        let frame = a_ini.encrypt_message(b"cross").unwrap();
        assert_eq!(b_rsp.decrypt_message(&frame), Err(LinkError::Decrypt));
    }

    #[test]
    fn debug_output_never_leaks_secrets() {
        let (ini, rsp) = establish();
        let d1 = format!("{ini:?}");
        let d2 = format!("{rsp:?}");
        assert!(d1.contains("LinkInitiator") && d1.contains("link_id"));
        assert!(d2.contains("LinkResponder") && d2.contains("link_id"));
        // The known ephemeral seeds/keys must not be visible.
        assert!(!d1.contains("33, 33") && !d1.contains("0x33"));
        assert!(!d2.contains("85, 85") && !d2.contains("0x55"));
    }
}
