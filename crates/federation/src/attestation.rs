//! Cross-server identity attestation — the `persona@server` model.
//!
//! A [`PersonaAttestation`] is a home server's signed statement binding a
//! persona name to a persona-held Ed25519 key: "`alice`'s key on this server
//! is X, from `issued_at` until `expires_at`, generation N". It is signed by
//! the home server over domain-separated canonical postcard bytes
//! ([`ATTESTATION_CONTEXT`]) — the same self-certifying discipline as the
//! [`crate::handshake::PeerDescriptor`] and [`crate::catalog::SignedCatalog`]
//! — so any remote server can verify it offline against the home server's
//! known key. Freshness is checked against a **caller-supplied** clock; no
//! ambient time is read anywhere in this module.
//!
//! Personas are addressed as `persona@server` via [`FedAddress`], with a
//! strict, total parser (length caps, lowercase charset) that never panics.
//!
//! **Key continuity.** Personas rotate keys. A bare re-attestation by the
//! home server would let a malicious (or coerced) server silently swap a
//! persona's key and impersonate them to the rest of the federation. A
//! [`ContinuityChain`] closes that hole: each new generation must be
//! cross-signed by **both** the home server (the attestation signature) and
//! the *previous* persona key (a [`KeyRotation`] link, signed over
//! [`ROTATION_CONTEXT`]). Peers walking the chain with
//! [`ContinuityChain::verify`] therefore know every rotation was consented to
//! by the key holder, not just the server. Broken links, missing prev-key
//! signatures and generation gaps are errors, never panics.
//!
//! **Visiting users.** When a persona from server A knocks on server B, B
//! challenges them with fresh random bytes; the visitor answers with their
//! continuity chain plus a signature over the challenge by their current
//! persona key ([`sign_challenge`]). [`verify_visitor`] is the pure function
//! B runs over those bytes: chain valid, latest attestation fresh, challenge
//! signature under the attested key.

use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Domain separator for persona-attestation signatures (home server key).
pub const ATTESTATION_CONTEXT: &[u8] = b"rhp-fed-attestation-v1";

/// Domain separator for key-rotation cross-signatures (previous persona key).
pub const ROTATION_CONTEXT: &[u8] = b"rhp-fed-rotation-v1";

/// Domain separator for visitor challenge signatures (current persona key).
pub const CHALLENGE_CONTEXT: &[u8] = b"rhp-fed-visitor-challenge-v1";

/// Maximum byte length of a persona name (the part before the `@`).
pub const MAX_PERSONA_LEN: usize = 64;

/// Maximum byte length of a server name (the part after the `@`); matches
/// the DNS hostname limit.
pub const MAX_SERVER_LEN: usize = 253;

/// Minimum challenge length [`verify_visitor`] accepts. Shorter challenges
/// carry too little entropy to rule out replay; callers should use 32 fresh
/// random bytes.
pub const MIN_CHALLENGE_LEN: usize = 16;

/// Whether `name` is a well-formed persona name: 1..=[`MAX_PERSONA_LEN`]
/// bytes of lowercase ASCII alphanumerics plus `-`, `_`, `.`, starting and
/// ending with an alphanumeric.
pub fn is_valid_persona_name(name: &str) -> bool {
    is_valid_name(name, MAX_PERSONA_LEN)
}

/// Whether `name` is a well-formed server name: same charset and edge rules
/// as [`is_valid_persona_name`] but capped at [`MAX_SERVER_LEN`] bytes.
pub fn is_valid_server_name(name: &str) -> bool {
    is_valid_name(name, MAX_SERVER_LEN)
}

/// Shared name rule: non-empty, `<= max` bytes, charset `[a-z0-9-_.]`, first
/// and last bytes alphanumeric.
fn is_valid_name(name: &str, max: usize) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > max {
        return false;
    }
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    bytes
        .iter()
        .all(|&b| alnum(b) || matches!(b, b'-' | b'_' | b'.'))
        && alnum(bytes[0])
        && alnum(bytes[bytes.len() - 1])
}

/// Why a `persona@server` address failed to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    /// No `@` separator was found.
    #[error("address has no '@' separator")]
    MissingAt,
    /// The persona part is empty, too long, or uses forbidden characters.
    #[error("persona part is not a valid name")]
    BadPersona,
    /// The server part is empty, too long, or uses forbidden characters
    /// (a second `@` lands here, since `@` is outside the charset).
    #[error("server part is not a valid name")]
    BadServer,
}

/// A federated persona address: `persona@server`.
///
/// Both parts obey the slug rules documented on [`is_valid_persona_name`] /
/// [`is_valid_server_name`]; the constructors are the only way to build one,
/// so a `FedAddress` is well-formed by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FedAddress {
    persona: String,
    server: String,
}

impl FedAddress {
    /// Build an address from already-separated parts, validating both.
    pub fn new(
        persona: impl Into<String>,
        server: impl Into<String>,
    ) -> Result<FedAddress, AddressError> {
        let persona = persona.into();
        let server = server.into();
        if !is_valid_persona_name(&persona) {
            return Err(AddressError::BadPersona);
        }
        if !is_valid_server_name(&server) {
            return Err(AddressError::BadServer);
        }
        Ok(FedAddress { persona, server })
    }

    /// Parse `"alice@burrow.example"`. Total: any input yields `Ok` or a
    /// specific [`AddressError`], never a panic. The split is on the *first*
    /// `@`; a second `@` fails the server-part charset check.
    pub fn parse(s: &str) -> Result<FedAddress, AddressError> {
        let (persona, server) = s.split_once('@').ok_or(AddressError::MissingAt)?;
        FedAddress::new(persona, server)
    }

    /// The persona part (before the `@`).
    pub fn persona(&self) -> &str {
        &self.persona
    }

    /// The server part (after the `@`).
    pub fn server(&self) -> &str {
        &self.server
    }
}

impl fmt::Display for FedAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.persona, self.server)
    }
}

impl FromStr for FedAddress {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        FedAddress::parse(s)
    }
}

/// Why an attestation failed to sign or verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AttestationError {
    /// The signature does not verify under the claimed home server key.
    #[error("attestation signature does not verify")]
    BadSignature,
    /// The supplied home server key does not match the key declared in the
    /// attestation body.
    #[error("attestation home server key mismatch")]
    KeyMismatch,
    /// The body could not be canonicalized for signing/verification.
    #[error("attestation body does not encode")]
    Encoding,
    /// The persona name violates the rules of [`is_valid_persona_name`].
    #[error("attestation persona name is not well-formed")]
    BadPersonaName,
    /// `now` is at or past `expires_at`.
    #[error("attestation has expired")]
    Expired,
    /// `now` is before `issued_at`.
    #[error("attestation is not yet valid")]
    NotYetValid,
}

/// A previous-key cross-signature authorizing a persona key rotation.
///
/// Rotation to a new key is only trustworthy if the *outgoing* key consented:
/// otherwise the home server could mint a fresh attestation for a key it
/// controls and impersonate the persona. `prev_sig` is the previous persona
/// key's Ed25519 signature over [`ROTATION_CONTEXT`] ‖ postcard(statement),
/// where the statement binds the persona name, home server key, `new_key`
/// and the target generation — so a rotation signature cannot be replayed
/// for another persona, another server, or another generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRotation {
    /// The incoming persona key. Must equal the enclosing attestation's
    /// `persona_key`; [`ContinuityChain::verify`] enforces this.
    pub new_key: [u8; 32],
    /// Signature by the *previous* generation's persona key over the
    /// rotation statement.
    pub prev_sig: Signature,
}

impl KeyRotation {
    /// Sign a rotation with the outgoing persona key `prev`, authorizing
    /// `new_key` as `persona_name`'s key at `generation` on the server
    /// identified by `home_server_key`.
    pub fn sign(
        prev: &IdentityKey,
        persona_name: &str,
        home_server_key: [u8; 32],
        new_key: [u8; 32],
        generation: u64,
    ) -> Result<KeyRotation, AttestationError> {
        let msg = rotation_signed_bytes(persona_name, home_server_key, new_key, generation)?;
        Ok(KeyRotation {
            new_key,
            prev_sig: prev.sign(&msg),
        })
    }

    /// Verify `prev_sig` under the outgoing key `prev_key` for the given
    /// statement parameters.
    pub fn verify(
        &self,
        prev_key: &PublicKey,
        persona_name: &str,
        home_server_key: [u8; 32],
        generation: u64,
    ) -> Result<(), ContinuityError> {
        let msg = rotation_signed_bytes(persona_name, home_server_key, self.new_key, generation)?;
        if prev_key.verify(&msg, &self.prev_sig) {
            Ok(())
        } else {
            Err(ContinuityError::BadRotationSignature)
        }
    }
}

/// The signable rotation statement (never sent on the wire; both sides
/// reconstruct it from the enclosing attestation).
#[derive(Serialize)]
struct RotationStatement<'a> {
    persona_name: &'a str,
    home_server_key: [u8; 32],
    new_key: [u8; 32],
    generation: u64,
}

/// The exact bytes a rotation signs: context ‖ postcard(statement).
fn rotation_signed_bytes(
    persona_name: &str,
    home_server_key: [u8; 32],
    new_key: [u8; 32],
    generation: u64,
) -> Result<Vec<u8>, AttestationError> {
    let statement = RotationStatement {
        persona_name,
        home_server_key,
        new_key,
        generation,
    };
    let mut msg = ROTATION_CONTEXT.to_vec();
    msg.extend(postcard::to_allocvec(&statement).map_err(|_| AttestationError::Encoding)?);
    Ok(msg)
}

/// The signable core of a persona attestation (everything the signature and
/// the [`attestation_id`](PersonaAttestation::attestation_id) cover).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationBody {
    /// The persona's name on its home server (rules of
    /// [`is_valid_persona_name`]).
    pub persona_name: String,
    /// The persona's own Ed25519 public key for this generation.
    pub persona_key: [u8; 32],
    /// The home server's Ed25519 public identity key. Stamped from the
    /// signing key by [`AttestationBody::sign`] so the document
    /// self-certifies.
    pub home_server_key: [u8; 32],
    /// Validity start, unix milliseconds.
    pub issued_at: i64,
    /// Validity end, unix milliseconds (exclusive: `now >= expires_at` is
    /// expired).
    pub expires_at: i64,
    /// Key generation, starting at 0 and incrementing by exactly 1 per
    /// rotation.
    pub generation: u64,
    /// Previous-key consent for this generation's key. Required by
    /// [`ContinuityChain::verify`] for every non-first link; `None` on the
    /// genesis attestation.
    pub rotation: Option<KeyRotation>,
}

impl AttestationBody {
    /// Start an attestation body for `persona_name` holding `persona_key` at
    /// `generation`. Validity defaults to the empty window `[0, 0)`; set it
    /// with [`with_validity`](Self::with_validity).
    pub fn new(persona_name: impl Into<String>, persona_key: [u8; 32], generation: u64) -> Self {
        Self {
            persona_name: persona_name.into(),
            persona_key,
            home_server_key: [0u8; 32],
            issued_at: 0,
            expires_at: 0,
            generation,
            rotation: None,
        }
    }

    /// Builder: set the validity window (unix ms, `expires_at` exclusive).
    pub fn with_validity(mut self, issued_at: i64, expires_at: i64) -> Self {
        self.issued_at = issued_at;
        self.expires_at = expires_at;
        self
    }

    /// Builder: attach the previous-key rotation consent.
    pub fn with_rotation(mut self, rotation: KeyRotation) -> Self {
        self.rotation = Some(rotation);
        self
    }

    /// Canonical bytes for hashing/signing: `postcard(self)`.
    fn canonical(&self) -> Result<Vec<u8>, AttestationError> {
        postcard::to_allocvec(self).map_err(|_| AttestationError::Encoding)
    }

    /// Sign this body with the home server's key. The declared
    /// `home_server_key` is overwritten with `key`'s public key so the
    /// document always self-certifies. Rejects ill-formed persona names up
    /// front so a server cannot mint an attestation its peers will refuse.
    pub fn sign(mut self, key: &IdentityKey) -> Result<PersonaAttestation, AttestationError> {
        if !is_valid_persona_name(&self.persona_name) {
            return Err(AttestationError::BadPersonaName);
        }
        self.home_server_key = key.public().0;
        let msg = signed_bytes(&self)?;
        let sig = key.sign(&msg);
        Ok(PersonaAttestation { body: self, sig })
    }
}

/// An [`AttestationBody`] plus the home server's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaAttestation {
    /// The signed statement.
    pub body: AttestationBody,
    /// Ed25519 signature over [`ATTESTATION_CONTEXT`] ‖ postcard(body), by
    /// the key named in `body.home_server_key`.
    pub sig: Signature,
}

impl PersonaAttestation {
    /// The stable content id of this attestation: `blake3` of the body's
    /// canonical bytes.
    pub fn attestation_id(&self) -> Result<[u8; 32], AttestationError> {
        Ok(*blake3::hash(&self.body.canonical()?).as_bytes())
    }

    /// Verify signature, shape *and* freshness at the caller-supplied clock
    /// `now_ms` (unix ms — no ambient time is read).
    pub fn verify(&self, home_server_key: &PublicKey, now_ms: i64) -> Result<(), AttestationError> {
        self.verify_signature(home_server_key)?;
        self.check_freshness(now_ms)
    }

    /// Verify signature and shape only, without a freshness check. Used by
    /// [`ContinuityChain::verify`] on historical links, whose validity
    /// windows have naturally lapsed.
    pub fn verify_signature(&self, home_server_key: &PublicKey) -> Result<(), AttestationError> {
        if self.body.home_server_key != home_server_key.0 {
            return Err(AttestationError::KeyMismatch);
        }
        if !is_valid_persona_name(&self.body.persona_name) {
            return Err(AttestationError::BadPersonaName);
        }
        let msg = signed_bytes(&self.body)?;
        if home_server_key.verify(&msg, &self.sig) {
            Ok(())
        } else {
            Err(AttestationError::BadSignature)
        }
    }

    /// Freshness at `now_ms`: `issued_at <= now_ms < expires_at`.
    fn check_freshness(&self, now_ms: i64) -> Result<(), AttestationError> {
        if now_ms < self.body.issued_at {
            return Err(AttestationError::NotYetValid);
        }
        if now_ms >= self.body.expires_at {
            return Err(AttestationError::Expired);
        }
        Ok(())
    }

    /// Wire form (postcard) for presenting to a remote server.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("attestation serializes")
    }

    /// Decode from bytes; `None` on malformed input (never panics). The
    /// caller must still [`verify`](Self::verify).
    pub fn from_bytes(bytes: &[u8]) -> Option<PersonaAttestation> {
        postcard::from_bytes(bytes).ok()
    }
}

/// The exact bytes signed: context ‖ postcard(body).
fn signed_bytes(body: &AttestationBody) -> Result<Vec<u8>, AttestationError> {
    let mut msg = ATTESTATION_CONTEXT.to_vec();
    msg.extend(body.canonical()?);
    Ok(msg)
}

/// Why a continuity chain failed to verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContinuityError {
    /// A link failed individual attestation verification.
    #[error(transparent)]
    Attestation(#[from] AttestationError),
    /// The chain contains no attestations.
    #[error("continuity chain is empty")]
    Empty,
    /// Links disagree on the persona name.
    #[error("continuity chain mixes personas")]
    PersonaMismatch,
    /// A link's generation is not exactly its predecessor's plus one.
    #[error("continuity chain has a generation gap")]
    GenerationGap,
    /// A non-first link carries no [`KeyRotation`] — the swap-attack shape:
    /// a server re-attesting a new key without the previous key's consent.
    #[error("rotation is missing the previous key's consent signature")]
    MissingRotation,
    /// A link's rotation authorizes a different key than the link attests.
    #[error("rotation new key does not match the attested persona key")]
    RotationKeyMismatch,
    /// A rotation's `prev_sig` does not verify under the previous persona
    /// key (or was minted for a different persona/server/generation).
    #[error("rotation signature does not verify under the previous key")]
    BadRotationSignature,
    /// The visitor challenge is shorter than [`MIN_CHALLENGE_LEN`].
    #[error("visitor challenge is too short")]
    ChallengeTooShort,
    /// The visitor's challenge signature does not verify under the attested
    /// persona key.
    #[error("visitor challenge signature does not verify")]
    BadChallengeSignature,
}

/// An append-only history of a persona's attestations on one home server,
/// oldest first, one entry per key generation.
///
/// The chain is plain data; nothing is checked on construction or
/// [`push`](Self::push). All guarantees come from [`verify`](Self::verify),
/// which is total over arbitrary contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityChain {
    /// The attestations, oldest first.
    pub attestations: Vec<PersonaAttestation>,
}

impl ContinuityChain {
    /// Start a chain from its first (trusted-anchor) attestation.
    pub fn new(genesis: PersonaAttestation) -> Self {
        Self {
            attestations: vec![genesis],
        }
    }

    /// Append the next generation's attestation. No checks are performed
    /// here; [`verify`](Self::verify) is the arbiter.
    pub fn push(&mut self, attestation: PersonaAttestation) {
        self.attestations.push(attestation);
    }

    /// Number of links (generations) in the chain.
    pub fn len(&self) -> usize {
        self.attestations.len()
    }

    /// Whether the chain holds no attestations (never verifies).
    pub fn is_empty(&self) -> bool {
        self.attestations.is_empty()
    }

    /// The most recent attested persona key, if any. Meaningful only after
    /// [`verify`](Self::verify) has succeeded.
    pub fn latest_key(&self) -> Option<PublicKey> {
        self.attestations
            .last()
            .map(|a| PublicKey(a.body.persona_key))
    }

    /// The most recent attestation, if any.
    pub fn latest(&self) -> Option<&PersonaAttestation> {
        self.attestations.last()
    }

    /// Walk the whole chain against the home server's key at `now_ms`.
    ///
    /// Checks, in order:
    /// - the chain is non-empty;
    /// - every link's home-server signature, declared key and persona name
    ///   ([`PersonaAttestation::verify_signature`]);
    /// - every link names the same persona;
    /// - generations increase by exactly 1 between adjacent links;
    /// - every non-first link carries a [`KeyRotation`] whose `new_key`
    ///   matches the link's `persona_key` and whose `prev_sig` verifies
    ///   under the *previous* link's persona key — so a rotation was never
    ///   a unilateral server-side swap;
    /// - the **latest** link is fresh at `now_ms` (historical links are
    ///   allowed to have lapsed).
    ///
    /// Returns the latest attested persona key on success. Total: malformed
    /// chains yield errors, never panics.
    pub fn verify(
        &self,
        home_server_key: &PublicKey,
        now_ms: i64,
    ) -> Result<PublicKey, ContinuityError> {
        let first = self.attestations.first().ok_or(ContinuityError::Empty)?;
        for att in &self.attestations {
            att.verify_signature(home_server_key)?;
            if att.body.persona_name != first.body.persona_name {
                return Err(ContinuityError::PersonaMismatch);
            }
        }
        for pair in self.attestations.windows(2) {
            let (prev, next) = (&pair[0], &pair[1]);
            if prev.body.generation.checked_add(1) != Some(next.body.generation) {
                return Err(ContinuityError::GenerationGap);
            }
            let rotation = next.body.rotation.ok_or(ContinuityError::MissingRotation)?;
            if rotation.new_key != next.body.persona_key {
                return Err(ContinuityError::RotationKeyMismatch);
            }
            rotation.verify(
                &PublicKey(prev.body.persona_key),
                &next.body.persona_name,
                next.body.home_server_key,
                next.body.generation,
            )?;
        }
        let latest = self.attestations.last().ok_or(ContinuityError::Empty)?;
        latest.check_freshness(now_ms)?;
        Ok(PublicKey(latest.body.persona_key))
    }

    /// Wire form (postcard) for presenting to a remote server.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("continuity chain serializes")
    }

    /// Decode from bytes; `None` on malformed input (never panics). The
    /// caller must still [`verify`](Self::verify).
    pub fn from_bytes(bytes: &[u8]) -> Option<ContinuityChain> {
        postcard::from_bytes(bytes).ok()
    }
}

/// Sign a visitor challenge with the current persona key. The signature
/// covers [`CHALLENGE_CONTEXT`] ‖ challenge, so it can never double as any
/// other signed surface.
pub fn sign_challenge(persona_key: &IdentityKey, challenge: &[u8]) -> Signature {
    persona_key.sign(&challenge_signed_bytes(challenge))
}

/// Verify a visiting persona: their continuity `chain` (fetched or
/// presented), the visited server's knowledge of the visitor's home server
/// key, the visited server's clock `now_ms`, the fresh `challenge` bytes the
/// visited server minted, and the visitor's `persona_sig` over that
/// challenge ([`sign_challenge`]).
///
/// On success returns the attested persona key the visitor proved control
/// of. Pure function over supplied bytes — no I/O, no ambient time.
pub fn verify_visitor(
    chain: &ContinuityChain,
    home_server_key: &PublicKey,
    now_ms: i64,
    challenge: &[u8],
    persona_sig: &Signature,
) -> Result<PublicKey, ContinuityError> {
    if challenge.len() < MIN_CHALLENGE_LEN {
        return Err(ContinuityError::ChallengeTooShort);
    }
    let persona_key = chain.verify(home_server_key, now_ms)?;
    if persona_key.verify(&challenge_signed_bytes(challenge), persona_sig) {
        Ok(persona_key)
    } else {
        Err(ContinuityError::BadChallengeSignature)
    }
}

/// The exact bytes a challenge response signs: context ‖ challenge.
fn challenge_signed_bytes(challenge: &[u8]) -> Vec<u8> {
    let mut msg = CHALLENGE_CONTEXT.to_vec();
    msg.extend_from_slice(challenge);
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn server() -> IdentityKey {
        IdentityKey::from_seed(&[1u8; 32])
    }

    fn persona(gen: u8) -> IdentityKey {
        IdentityKey::from_seed(&[10 + gen; 32])
    }

    fn genesis() -> PersonaAttestation {
        AttestationBody::new("alice", persona(0).public().0, 0)
            .with_validity(NOW, NOW + HOUR)
            .sign(&server())
            .unwrap()
    }

    /// Build the next-generation attestation with a proper prev-key consent.
    fn rotate(
        prev_att: &PersonaAttestation,
        prev_key: &IdentityKey,
        next_key: &IdentityKey,
        issued_at: i64,
    ) -> PersonaAttestation {
        let generation = prev_att.body.generation + 1;
        let rotation = KeyRotation::sign(
            prev_key,
            &prev_att.body.persona_name,
            server().public().0,
            next_key.public().0,
            generation,
        )
        .unwrap();
        AttestationBody::new(
            prev_att.body.persona_name.clone(),
            next_key.public().0,
            generation,
        )
        .with_validity(issued_at, issued_at + HOUR)
        .with_rotation(rotation)
        .sign(&server())
        .unwrap()
    }

    // ---- PersonaAttestation ------------------------------------------------

    #[test]
    fn sign_verify_roundtrip_and_wire_form() {
        let att = genesis();
        // signing stamped the real public key into the body.
        assert_eq!(att.body.home_server_key, server().public().0);
        assert_eq!(att.verify(&server().public(), NOW), Ok(()));

        let back = PersonaAttestation::from_bytes(&att.to_bytes()).unwrap();
        assert_eq!(back, att);
        assert_eq!(back.verify(&server().public(), NOW), Ok(()));
    }

    #[test]
    fn attestation_id_is_stable_and_content_addressed() {
        let a = genesis();
        let b = genesis();
        assert_eq!(a.attestation_id().unwrap(), b.attestation_id().unwrap());

        let c = AttestationBody::new("bob", persona(0).public().0, 0)
            .with_validity(NOW, NOW + HOUR)
            .sign(&server())
            .unwrap();
        assert_ne!(a.attestation_id().unwrap(), c.attestation_id().unwrap());
    }

    #[test]
    fn expiry_is_checked_against_the_injected_clock() {
        let att = genesis();
        assert_eq!(att.verify(&server().public(), NOW), Ok(()));
        // Last valid instant is expires_at - 1; expires_at itself is expired.
        assert_eq!(att.verify(&server().public(), NOW + HOUR - 1), Ok(()));
        assert_eq!(
            att.verify(&server().public(), NOW + HOUR),
            Err(AttestationError::Expired)
        );
        assert_eq!(
            att.verify(&server().public(), NOW - 1),
            Err(AttestationError::NotYetValid)
        );
    }

    #[test]
    fn tampered_fields_fail_verification() {
        let cases: [fn(&mut PersonaAttestation); 5] = [
            |a| a.body.persona_name = "mallory".into(),
            |a| a.body.persona_key = [0xaa; 32],
            |a| a.body.generation = 7,
            |a| a.body.expires_at += HOUR,
            |a| a.body.issued_at -= HOUR,
        ];
        for tamper in cases {
            let mut att = genesis();
            tamper(&mut att);
            assert_eq!(
                att.verify(&server().public(), NOW),
                Err(AttestationError::BadSignature)
            );
        }
    }

    #[test]
    fn wrong_key_is_rejected_as_mismatch() {
        let att = genesis();
        let other = IdentityKey::from_seed(&[9u8; 32]);
        assert_eq!(
            att.verify(&other.public(), NOW),
            Err(AttestationError::KeyMismatch)
        );
    }

    #[test]
    fn impersonating_key_fails_verification() {
        let mut att = genesis();
        // Claim a different home server key without a matching signature.
        let impostor = IdentityKey::from_seed(&[9u8; 32]).public();
        att.body.home_server_key = impostor.0;
        assert_eq!(
            att.verify(&impostor, NOW),
            Err(AttestationError::BadSignature)
        );
    }

    #[test]
    fn sign_rejects_bad_persona_names() {
        for name in ["", "Alice", "al ice", "-alice", "alice-", "ali@ce"] {
            let body =
                AttestationBody::new(name, persona(0).public().0, 0).with_validity(NOW, NOW + HOUR);
            assert_eq!(
                body.sign(&server()).unwrap_err(),
                AttestationError::BadPersonaName,
                "name {name:?} should be rejected at signing"
            );
        }
        let long = "a".repeat(MAX_PERSONA_LEN + 1);
        let body =
            AttestationBody::new(long, persona(0).public().0, 0).with_validity(NOW, NOW + HOUR);
        assert_eq!(
            body.sign(&server()).unwrap_err(),
            AttestationError::BadPersonaName
        );
    }

    #[test]
    fn verify_rejects_bad_persona_name_even_with_valid_signature() {
        // A rogue server *can* sign anything; peers must still refuse.
        let mut body =
            AttestationBody::new("alice", persona(0).public().0, 0).with_validity(NOW, NOW + HOUR);
        body.persona_name = "NOT VALID".into();
        body.home_server_key = server().public().0;
        let msg = signed_bytes(&body).unwrap();
        let att = PersonaAttestation {
            sig: server().sign(&msg),
            body,
        };
        assert_eq!(
            att.verify(&server().public(), NOW),
            Err(AttestationError::BadPersonaName)
        );
    }

    // ---- FedAddress --------------------------------------------------------

    #[test]
    fn address_parse_accepts_valid_forms() {
        for (input, persona, server) in [
            ("alice@burrow.example", "alice", "burrow.example"),
            ("a@b", "a", "b"),
            (
                "under_score.dot-dash9@host-1.example",
                "under_score.dot-dash9",
                "host-1.example",
            ),
            ("0numeric@127.0.0.1", "0numeric", "127.0.0.1"),
        ] {
            let addr = FedAddress::parse(input).unwrap();
            assert_eq!(addr.persona(), persona);
            assert_eq!(addr.server(), server);
            // Display round-trips exactly.
            assert_eq!(addr.to_string(), input);
            assert_eq!(input.parse::<FedAddress>().unwrap(), addr);
        }
    }

    #[test]
    fn address_parse_rejects_invalid_forms() {
        use AddressError::*;
        let long_persona = format!("{}@ok.example", "a".repeat(MAX_PERSONA_LEN + 1));
        let long_server = format!("alice@{}", "a".repeat(MAX_SERVER_LEN + 1));
        let cases: Vec<(&str, AddressError)> = vec![
            ("", MissingAt),
            ("alice", MissingAt),
            ("@burrow.example", BadPersona),
            ("alice@", BadServer),
            ("Alice@burrow.example", BadPersona),
            ("alice@Burrow.example", BadServer),
            ("al ice@burrow.example", BadPersona),
            ("alice@burrow example", BadServer),
            ("-alice@burrow.example", BadPersona),
            ("alice-@burrow.example", BadPersona),
            ("alice@.burrow.example", BadServer),
            ("alice@burrow.example.", BadServer),
            ("a@b@c", BadServer),
            ("ålice@burrow.example", BadPersona),
            ("alice@bürrow.example", BadServer),
            (&long_persona, BadPersona),
            (&long_server, BadServer),
        ];
        for (input, want) in cases {
            assert_eq!(
                FedAddress::parse(input).unwrap_err(),
                want,
                "input {input:?}"
            );
        }
    }

    #[test]
    fn address_length_caps_are_exact() {
        let max_persona = "a".repeat(MAX_PERSONA_LEN);
        let addr = FedAddress::new(max_persona.clone(), "b").unwrap();
        assert_eq!(addr.persona().len(), MAX_PERSONA_LEN);
        assert_eq!(
            FedAddress::new(format!("{max_persona}a"), "b").unwrap_err(),
            AddressError::BadPersona
        );

        let max_server = "s".repeat(MAX_SERVER_LEN);
        assert!(FedAddress::new("a", max_server.clone()).is_ok());
        assert_eq!(
            FedAddress::new("a", format!("{max_server}s")).unwrap_err(),
            AddressError::BadServer
        );
    }

    // ---- ContinuityChain ---------------------------------------------------

    #[test]
    fn genesis_only_chain_verifies() {
        let chain = ContinuityChain::new(genesis());
        let key = chain.verify(&server().public(), NOW).unwrap();
        assert_eq!(key, persona(0).public());
        assert_eq!(chain.latest_key(), Some(persona(0).public()));
        assert_eq!(chain.len(), 1);
        assert!(!chain.is_empty());
    }

    #[test]
    fn rotation_happy_path_across_two_generations() {
        let gen0 = genesis();
        let gen1 = rotate(&gen0, &persona(0), &persona(1), NOW);
        let gen2 = rotate(&gen1, &persona(1), &persona(2), NOW);

        let mut chain = ContinuityChain::new(gen0);
        chain.push(gen1);
        chain.push(gen2);

        let key = chain.verify(&server().public(), NOW).unwrap();
        assert_eq!(key, persona(2).public());
        assert_eq!(chain.latest_key(), Some(persona(2).public()));
        assert_eq!(chain.latest().unwrap().body.generation, 2);
    }

    #[test]
    fn expired_history_is_fine_but_expired_latest_is_not() {
        // Genesis lapsed long ago; only the latest link must be fresh.
        let gen0 = AttestationBody::new("alice", persona(0).public().0, 0)
            .with_validity(NOW - 10 * HOUR, NOW - 9 * HOUR)
            .sign(&server())
            .unwrap();
        let gen1 = rotate(&gen0, &persona(0), &persona(1), NOW);
        let mut chain = ContinuityChain::new(gen0);
        chain.push(gen1);
        assert!(chain.verify(&server().public(), NOW).is_ok());
        assert_eq!(
            chain.verify(&server().public(), NOW + 2 * HOUR),
            Err(ContinuityError::Attestation(AttestationError::Expired))
        );
    }

    #[test]
    fn swap_attack_rotation_without_prev_consent_is_rejected() {
        // A malicious server re-attests a key it controls, with no rotation
        // link at all — the canonical server-side swap.
        let gen0 = genesis();
        let swapped = AttestationBody::new("alice", persona(1).public().0, 1)
            .with_validity(NOW, NOW + HOUR)
            .sign(&server())
            .unwrap();
        let mut chain = ContinuityChain::new(gen0);
        chain.push(swapped);
        assert_eq!(
            chain.verify(&server().public(), NOW),
            Err(ContinuityError::MissingRotation)
        );
    }

    #[test]
    fn swap_attack_rotation_signed_by_wrong_key_is_rejected() {
        // The server forges a rotation signed by a key it holds rather than
        // the persona's actual previous key.
        let gen0 = genesis();
        let attacker = IdentityKey::from_seed(&[0x66; 32]);
        let forged = KeyRotation::sign(
            &attacker,
            "alice",
            server().public().0,
            persona(1).public().0,
            1,
        )
        .unwrap();
        let gen1 = AttestationBody::new("alice", persona(1).public().0, 1)
            .with_validity(NOW, NOW + HOUR)
            .with_rotation(forged)
            .sign(&server())
            .unwrap();
        let mut chain = ContinuityChain::new(gen0);
        chain.push(gen1);
        assert_eq!(
            chain.verify(&server().public(), NOW),
            Err(ContinuityError::BadRotationSignature)
        );
    }

    #[test]
    fn rotation_authorizing_a_different_key_is_rejected() {
        // prev key consented to key A, but the attestation binds key B.
        let gen0 = genesis();
        let consent_for_a = KeyRotation::sign(
            &persona(0),
            "alice",
            server().public().0,
            persona(1).public().0,
            1,
        )
        .unwrap();
        let gen1 = AttestationBody::new("alice", persona(2).public().0, 1)
            .with_validity(NOW, NOW + HOUR)
            .with_rotation(consent_for_a)
            .sign(&server())
            .unwrap();
        let mut chain = ContinuityChain::new(gen0);
        chain.push(gen1);
        assert_eq!(
            chain.verify(&server().public(), NOW),
            Err(ContinuityError::RotationKeyMismatch)
        );
    }

    #[test]
    fn rotation_signature_is_generation_bound() {
        // A valid consent minted for generation 1 cannot be replayed at 2.
        let gen0 = genesis();
        let gen1 = rotate(&gen0, &persona(0), &persona(1), NOW);
        let replayed = gen1.body.rotation.unwrap();
        let gen2 = AttestationBody::new("alice", persona(1).public().0, 2)
            .with_validity(NOW, NOW + HOUR)
            .with_rotation(replayed)
            .sign(&server())
            .unwrap();
        let mut chain = ContinuityChain::new(gen1);
        chain.push(gen2);
        assert_eq!(
            chain.verify(&server().public(), NOW),
            Err(ContinuityError::BadRotationSignature)
        );
    }

    #[test]
    fn generation_gap_is_rejected() {
        let gen0 = genesis();
        // Properly consented rotation, but generation jumps 0 -> 2.
        let consent = KeyRotation::sign(
            &persona(0),
            "alice",
            server().public().0,
            persona(1).public().0,
            2,
        )
        .unwrap();
        let skipped = AttestationBody::new("alice", persona(1).public().0, 2)
            .with_validity(NOW, NOW + HOUR)
            .with_rotation(consent)
            .sign(&server())
            .unwrap();
        let mut chain = ContinuityChain::new(gen0);
        chain.push(skipped);
        assert_eq!(
            chain.verify(&server().public(), NOW),
            Err(ContinuityError::GenerationGap)
        );
    }

    #[test]
    fn mixed_persona_chain_is_rejected() {
        let gen0 = genesis();
        let bob = AttestationBody::new("bob", persona(1).public().0, 1)
            .with_validity(NOW, NOW + HOUR)
            .sign(&server())
            .unwrap();
        let mut chain = ContinuityChain::new(gen0);
        chain.push(bob);
        assert_eq!(
            chain.verify(&server().public(), NOW),
            Err(ContinuityError::PersonaMismatch)
        );
    }

    #[test]
    fn empty_chain_is_an_error_not_a_panic() {
        let chain = ContinuityChain {
            attestations: Vec::new(),
        };
        assert_eq!(
            chain.verify(&server().public(), NOW),
            Err(ContinuityError::Empty)
        );
        assert_eq!(chain.latest_key(), None);
        assert!(chain.latest().is_none());
        assert!(chain.is_empty());
    }

    #[test]
    fn chain_wire_roundtrip() {
        let gen0 = genesis();
        let gen1 = rotate(&gen0, &persona(0), &persona(1), NOW);
        let mut chain = ContinuityChain::new(gen0);
        chain.push(gen1);

        let back = ContinuityChain::from_bytes(&chain.to_bytes()).unwrap();
        assert_eq!(back, chain);
        assert!(back.verify(&server().public(), NOW).is_ok());
    }

    // ---- verify_visitor ----------------------------------------------------

    #[test]
    fn visitor_challenge_happy_path() {
        let gen0 = genesis();
        let gen1 = rotate(&gen0, &persona(0), &persona(1), NOW);
        let mut chain = ContinuityChain::new(gen0);
        chain.push(gen1);

        let challenge = [0x42u8; 32];
        let sig = sign_challenge(&persona(1), &challenge);
        let key = verify_visitor(&chain, &server().public(), NOW, &challenge, &sig).unwrap();
        assert_eq!(key, persona(1).public());
    }

    #[test]
    fn visitor_with_wrong_challenge_bytes_is_rejected() {
        let chain = ContinuityChain::new(genesis());
        let sig = sign_challenge(&persona(0), &[0x42u8; 32]);
        assert_eq!(
            verify_visitor(&chain, &server().public(), NOW, &[0x43u8; 32], &sig),
            Err(ContinuityError::BadChallengeSignature)
        );
    }

    #[test]
    fn visitor_signing_with_a_superseded_key_is_rejected() {
        // After rotation, only the latest key answers challenges.
        let gen0 = genesis();
        let gen1 = rotate(&gen0, &persona(0), &persona(1), NOW);
        let mut chain = ContinuityChain::new(gen0);
        chain.push(gen1);

        let challenge = [0x42u8; 32];
        let old_sig = sign_challenge(&persona(0), &challenge);
        assert_eq!(
            verify_visitor(&chain, &server().public(), NOW, &challenge, &old_sig),
            Err(ContinuityError::BadChallengeSignature)
        );
    }

    #[test]
    fn visitor_with_short_challenge_is_rejected() {
        let chain = ContinuityChain::new(genesis());
        let challenge = [0x42u8; MIN_CHALLENGE_LEN - 1];
        let sig = sign_challenge(&persona(0), &challenge);
        assert_eq!(
            verify_visitor(&chain, &server().public(), NOW, &challenge, &sig),
            Err(ContinuityError::ChallengeTooShort)
        );
        assert_eq!(
            verify_visitor(&chain, &server().public(), NOW, &[], &sig),
            Err(ContinuityError::ChallengeTooShort)
        );
    }

    #[test]
    fn visitor_with_expired_attestation_is_rejected() {
        let chain = ContinuityChain::new(genesis());
        let challenge = [0x42u8; 32];
        let sig = sign_challenge(&persona(0), &challenge);
        assert_eq!(
            verify_visitor(&chain, &server().public(), NOW + 2 * HOUR, &challenge, &sig),
            Err(ContinuityError::Attestation(AttestationError::Expired))
        );
    }

    // ---- malformed-bytes totality -------------------------------------------

    #[test]
    fn decoders_never_panic_on_garbage() {
        assert!(PersonaAttestation::from_bytes(&[0xff; 7]).is_none());
        assert!(PersonaAttestation::from_bytes(&[]).is_none());
        assert!(ContinuityChain::from_bytes(&[0xff; 7]).is_none());
        assert!(ContinuityChain::from_bytes(&[]).is_none());
    }

    #[test]
    fn truncated_wire_bytes_never_panic() {
        let att = genesis();
        let wire = att.to_bytes();
        for len in 0..wire.len() {
            // Every prefix must decode to None or something verify handles.
            if let Some(partial) = PersonaAttestation::from_bytes(&wire[..len]) {
                let _ = partial.verify(&server().public(), NOW);
            }
        }

        let chain = ContinuityChain::new(att);
        let wire = chain.to_bytes();
        for len in 0..wire.len() {
            if let Some(partial) = ContinuityChain::from_bytes(&wire[..len]) {
                let _ = partial.verify(&server().public(), NOW);
            }
        }
    }

    #[test]
    fn bit_flipped_wire_bytes_never_panic() {
        let gen0 = genesis();
        let gen1 = rotate(&gen0, &persona(0), &persona(1), NOW);
        let mut chain = ContinuityChain::new(gen0);
        chain.push(gen1);
        let wire = chain.to_bytes();
        for i in 0..wire.len() {
            let mut corrupt = wire.clone();
            corrupt[i] ^= 0x01;
            // Decoding and verifying arbitrary corruptions must be total.
            if let Some(bad) = ContinuityChain::from_bytes(&corrupt) {
                let _ = bad.verify(&server().public(), NOW);
                let _ = bad.latest_key();
            }
        }
    }
}
