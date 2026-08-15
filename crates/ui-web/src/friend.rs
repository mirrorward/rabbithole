//! **Friendship**: a mutual, signed acknowledgement between two identities.
//!
//! Exchanged public keys are the floor, not the ceiling: seeing someone's key
//! in a roster proves nothing about *your relationship*. A friendship here is
//! two Ed25519 signatures over the same canonical statement — one from each
//! side — and only the pair together counts:
//!
//! ```text
//! message  =  "rabbithole-friend-v1" || min(pk_a, pk_b) || max(pk_b, pk_a)
//! friends  ⇔  valid sig_a over message  ∧  valid sig_b over message
//! ```
//!
//! Design properties, each doing real work:
//!
//! * **Order-independent message.** Both sides sign byte-identical input, so
//!   an attestation is meaningful no matter who signed first.
//! * **Self-authenticating.** The statement binds both public keys and the
//!   signature proves possession of one of them, so the *transport does not
//!   need to be trusted*: offers travel as ordinary DMs through whatever
//!   burrow both people happen to share, and a malicious burrow can neither
//!   forge an offer nor re-address someone else's — an offer that doesn't
//!   bind *your* key verifies and is discarded as not-for-you.
//! * **One-sided ≠ friends.** A stored offer (mine or theirs) renders as
//!   "offered", never as the badge. The badge requires both halves.
//!
//! What this deliberately is NOT: proof the person is *who they say they are*.
//! It proves the key you befriended is the key on the other end. Naming stays
//! human — verify fingerprints out of band if it matters.
//!
//! Crypto and codec are pure and host-tested; storage is wasm-gated.

use serde::{Deserialize, Serialize};

/// Domain separator: these signatures can never be confused with key-auth
/// (`rabbithole-key-auth-v2`) or any future signed statement.
pub const FRIEND_DOMAIN: &[u8] = b"rabbithole-friend-v1";

/// The DM marker an offer travels under. A regular message to a person the
/// sender can already DM — no new wire family, no server involvement.
const PAYLOAD_PREFIX: &str = "\u{1f91d}rh-friend-v1 ";

/// One stored friendship (or half of one).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friendship {
    /// Their public key, lowercase hex — the identity this record is about.
    pub peer_pub: String,
    /// The name we knew them by when the record was made (display only).
    pub peer_name: String,
    /// Our signature over the canonical statement, hex, if we've signed.
    pub my_sig: Option<String>,
    /// Their signature, hex, if we've received a valid one.
    pub their_sig: Option<String>,
}

/// Where a relationship stands. Rendering is driven off this, so the mapping
/// from stored halves to words lives in one tested place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Both halves present and verified at storage time: friends.
    Mutual,
    /// We signed; nothing valid from them yet.
    OfferedByMe,
    /// A valid offer from them awaits our signature.
    OfferedByThem,
    /// No record.
    None,
}

impl Friendship {
    pub fn status(&self) -> Status {
        match (&self.my_sig, &self.their_sig) {
            (Some(_), Some(_)) => Status::Mutual,
            (Some(_), None) => Status::OfferedByMe,
            (None, Some(_)) => Status::OfferedByThem,
            (None, None) => Status::None,
        }
    }
}

/// The canonical statement both sides sign: domain, then the two public keys
/// in lexicographic order of their lowercase hex. Byte-identical for both
/// signers regardless of who builds it.
pub fn attestation_message(pk_a_hex: &str, pk_b_hex: &str) -> Vec<u8> {
    let (a, b) = (pk_a_hex.to_lowercase(), pk_b_hex.to_lowercase());
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut msg = Vec::with_capacity(FRIEND_DOMAIN.len() + lo.len() + hi.len() + 2);
    msg.extend_from_slice(FRIEND_DOMAIN);
    msg.push(b'|');
    msg.extend_from_slice(lo.as_bytes());
    msg.push(b'|');
    msg.extend_from_slice(hi.as_bytes());
    msg
}

/// Sign the friendship statement binding `me` and `peer_pub_hex`.
pub fn sign(me: &crate::identity::Identity, peer_pub_hex: &str) -> String {
    let msg = attestation_message(&me.public_hex(), peer_pub_hex);
    hex::encode(me.sign(&msg))
}

/// Verify one half: does `sig_hex` prove `signer_pub_hex` signed the statement
/// binding the two keys? Malformed anything is simply `false` — this runs on
/// attacker-controlled DM text.
pub fn verify_half(signer_pub_hex: &str, other_pub_hex: &str, sig_hex: &str) -> bool {
    use ed25519_dalek::{Signature, VerifyingKey};
    let Ok(pk_bytes) = hex::decode(signer_pub_hex.trim()) else {
        return false;
    };
    let Ok(pk_bytes) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk_bytes) else {
        return false;
    };
    // Small-order / identity-point keys make the default verification
    // equation satisfiable by garbage (the all-zeros key accepts an all-zeros
    // signature over ANY message — this module's own test caught it). No
    // honest identity is a weak point, so reject them and use strict
    // verification, which checks the canonical, torsion-free equation.
    if vk.is_weak() {
        return false;
    }
    let Ok(sig_bytes) = hex::decode(sig_hex.trim()) else {
        return false;
    };
    let Ok(sig_bytes) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    let msg = attestation_message(signer_pub_hex, other_pub_hex);
    vk.verify_strict(&msg, &Signature::from_bytes(&sig_bytes))
        .is_ok()
}

/// Encode an offer for transport as a DM body.
pub fn encode_offer(my_pub_hex: &str, sig_hex: &str) -> String {
    format!(
        "{PAYLOAD_PREFIX}{} {}",
        my_pub_hex.to_lowercase(),
        sig_hex.to_lowercase()
    )
}

/// Parse a DM body as a friendship offer: `(sender_pub_hex, sig_hex)`.
/// Not-an-offer is `None`; verification is the caller's next step, always.
pub fn parse_offer(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix(PAYLOAD_PREFIX)?;
    let mut parts = rest.split_whitespace();
    let pk = parts.next()?;
    let sig = parts.next()?;
    if parts.next().is_some() || pk.len() != 64 || sig.len() != 128 {
        return None;
    }
    if !pk.chars().all(|c| c.is_ascii_hexdigit()) || !sig.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((pk.to_lowercase(), sig.to_lowercase()))
}

/// How an offer renders in a DM scrollback instead of its raw payload.
pub fn display_line(incoming: bool) -> &'static str {
    if incoming {
        "\u{1f91d} sent you a friendship attestation"
    } else {
        "\u{1f91d} you sent a friendship attestation"
    }
}

/// Apply a **verified** incoming offer to the store, returning the updated
/// record. Idempotent: replaying the same offer changes nothing.
pub fn record_their_offer(
    store: &mut Vec<Friendship>,
    peer_pub: &str,
    peer_name: &str,
    sig_hex: &str,
) -> Friendship {
    let peer_pub = peer_pub.to_lowercase();
    let entry = match store.iter_mut().find(|f| f.peer_pub == peer_pub) {
        Some(e) => e,
        None => {
            store.push(Friendship {
                peer_pub: peer_pub.clone(),
                ..Default::default()
            });
            store.last_mut().expect("just pushed")
        }
    };
    entry.peer_name = peer_name.to_string();
    entry.their_sig = Some(sig_hex.to_lowercase());
    entry.clone()
}

/// Record our own signature for `peer_pub` (offer sent, or offer accepted).
pub fn record_my_sig(
    store: &mut Vec<Friendship>,
    peer_pub: &str,
    peer_name: &str,
    sig_hex: &str,
) -> Friendship {
    let peer_pub = peer_pub.to_lowercase();
    let entry = match store.iter_mut().find(|f| f.peer_pub == peer_pub) {
        Some(e) => e,
        None => {
            store.push(Friendship {
                peer_pub: peer_pub.clone(),
                ..Default::default()
            });
            store.last_mut().expect("just pushed")
        }
    };
    if !peer_name.is_empty() {
        entry.peer_name = peer_name.to_string();
    }
    entry.my_sig = Some(sig_hex.to_lowercase());
    entry.clone()
}

/// Look up the relationship with a peer key.
pub fn status_of(store: &[Friendship], peer_pub: &str) -> Status {
    store
        .iter()
        .find(|f| f.peer_pub == peer_pub.to_lowercase())
        .map(|f| f.status())
        .unwrap_or(Status::None)
}

#[cfg(target_arch = "wasm32")]
pub mod storage {
    use super::Friendship;

    const KEY: &str = "rh.friends.v1";

    fn store() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn load() -> Vec<Friendship> {
        store()
            .and_then(|s| s.get_item(KEY).ok().flatten())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn save(list: &[Friendship]) {
        if let (Some(s), Ok(json)) = (store(), serde_json::to_string(list)) {
            let _ = s.set_item(KEY, &json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn pair() -> (Identity, Identity) {
        (
            Identity::from_seed([7u8; 32]),
            Identity::from_seed([9u8; 32]),
        )
    }

    #[test]
    fn both_sides_sign_the_same_bytes() {
        let (a, b) = pair();
        // Order-independence is what makes "who offers first" irrelevant.
        assert_eq!(
            attestation_message(&a.public_hex(), &b.public_hex()),
            attestation_message(&b.public_hex(), &a.public_hex()),
        );
        // …and the domain separator keeps it disjoint from key-auth signing.
        assert!(attestation_message(&a.public_hex(), &b.public_hex()).starts_with(FRIEND_DOMAIN));
    }

    #[test]
    fn a_signature_verifies_only_for_its_own_pair() {
        let (a, b) = pair();
        let c = Identity::from_seed([11u8; 32]);
        let sig = sign(&a, &b.public_hex());
        assert!(verify_half(&a.public_hex(), &b.public_hex(), &sig));
        // The same signature re-addressed to a different pair fails: a burrow
        // cannot take alice→bob's offer and present it to carol as hers.
        assert!(!verify_half(&a.public_hex(), &c.public_hex(), &sig));
        // Nor can carol claim she signed it.
        assert!(!verify_half(&c.public_hex(), &b.public_hex(), &sig));
    }

    #[test]
    fn malformed_input_never_verifies_or_panics() {
        let (a, b) = pair();
        // This function runs on attacker-controlled DM text.
        for bad in ["", "zz", "deadbeef", &"f".repeat(127), &"g".repeat(128)] {
            assert!(!verify_half(&a.public_hex(), &b.public_hex(), bad));
        }
        assert!(!verify_half(
            "nothex",
            &b.public_hex(),
            &sign(&a, &b.public_hex())
        ));
        // The small-order forgery: an all-zeros key "verifies" an all-zeros
        // signature under the default equation. Strict verification + weak-key
        // rejection is what makes this false.
        assert!(!verify_half(
            &"0".repeat(64),
            &b.public_hex(),
            &"0".repeat(128)
        ));
        // ...and an honest signature still passes strict verification.
        assert!(verify_half(
            &a.public_hex(),
            &b.public_hex(),
            &sign(&a, &b.public_hex())
        ));
    }

    #[test]
    fn offers_round_trip_through_a_dm_body() {
        let (a, b) = pair();
        let sig = sign(&a, &b.public_hex());
        let body = encode_offer(&a.public_hex(), &sig);
        let (pk, s) = parse_offer(&body).expect("round-trips");
        assert_eq!(pk, a.public_hex());
        assert!(verify_half(&pk, &b.public_hex(), &s));
        // Ordinary chat never parses as an offer — including near-misses.
        assert!(parse_offer("hey, friend request?").is_none());
        assert!(parse_offer("\u{1f91d}rh-friend-v1 tooshort sig").is_none());
        assert!(parse_offer(&format!(
            "{}{} {} extra",
            "\u{1f91d}rh-friend-v1 ",
            "a".repeat(64),
            "b".repeat(128)
        ))
        .is_none());
    }

    #[test]
    fn friendship_requires_both_halves() {
        let (a, b) = pair();
        let mut store = Vec::new();
        // Their valid offer arrives: offered-by-them, NOT friends.
        let their_sig = sign(&b, &a.public_hex());
        record_their_offer(&mut store, &b.public_hex(), "bob", &their_sig);
        assert_eq!(status_of(&store, &b.public_hex()), Status::OfferedByThem);
        // We sign back: now — and only now — mutual.
        let my_sig = sign(&a, &b.public_hex());
        record_my_sig(&mut store, &b.public_hex(), "bob", &my_sig);
        assert_eq!(status_of(&store, &b.public_hex()), Status::Mutual);
        // Replay changes nothing.
        record_their_offer(&mut store, &b.public_hex(), "bob", &their_sig);
        assert_eq!(store.len(), 1);
        assert_eq!(status_of(&store, &b.public_hex()), Status::Mutual);
        // A stranger's key: no record, no status.
        assert_eq!(status_of(&store, &"c".repeat(64)), Status::None);
    }

    #[test]
    fn a_relayed_offer_for_someone_else_is_not_for_me() {
        // The scenario the self-authenticating design exists for: a malicious
        // burrow takes bob's genuine offer to carol and delivers it to me.
        // The signature is real, but it binds bob+carol, so verifying it
        // against MY key fails and the offer is discarded.
        let (me, bob) = pair();
        let carol = Identity::from_seed([13u8; 32]);
        let for_carol = sign(&bob, &carol.public_hex());
        let dm = encode_offer(&bob.public_hex(), &for_carol);
        let (pk, sig) = parse_offer(&dm).unwrap();
        assert!(
            !verify_half(&pk, &me.public_hex(), &sig),
            "not addressed to me"
        );
        assert!(
            verify_half(&pk, &carol.public_hex(), &sig),
            "genuine for carol"
        );
    }

    #[test]
    fn my_offer_alone_is_not_friendship_either() {
        let (a, b) = pair();
        let mut store = Vec::new();
        record_my_sig(
            &mut store,
            &b.public_hex(),
            "bob",
            &sign(&a, &b.public_hex()),
        );
        assert_eq!(status_of(&store, &b.public_hex()), Status::OfferedByMe);
    }
}
