//! X3DH-lite asynchronous handshake ([Signal X3DH spec]).
//!
//! Lets an initiator (Alice) establish a shared secret with a responder (Bob) from
//! Bob's *published* keys alone, so Bob need not be online. "Lite" means we use the
//! three-DH variant (identity + signed prekey, **no** one-time prekey); the
//! optional one-time prekey (`DH4`) that libsignal adds for extra replay
//! resistance is out of scope for this core slice.
//!
//! # The exact DH combination
//!
//! Alice holds identity key `IK_A` and generates an ephemeral key `EK_A`. Bob
//! publishes identity key `IK_B` and a signed prekey `SPK_B`. Both sides compute:
//!
//! ```text
//! DH1 = DH(IK_A, SPK_B)     // binds Alice's identity to Bob's prekey
//! DH2 = DH(EK_A, IK_B)      // binds Bob's identity to Alice's ephemeral
//! DH3 = DH(EK_A, SPK_B)     // ephemeral<->prekey; the forward-secret part
//! SK  = KDF(DH1 || DH2 || DH3)
//! ```
//!
//! Bob computes the mirror DHs with the private keys he holds, arriving at the same
//! `SK`. `DH1` and `DH2` provide mutual authentication (each mixes one long-term
//! identity key); `DH3` provides forward secrecy (both inputs are ephemeral or
//! rotated). The concatenation order is fixed so both parties agree.
//!
//! The resulting [`SharedSecret`] seeds the [`crate::ratchet`] as the initial root
//! key: the initiator calls [`crate::Session::initiator`] with Bob's `SPK_B` public
//! key, and Bob calls [`crate::Session::responder`] with his `SPK_B` key pair,
//! which doubles as his first Double Ratchet key.
//!
//! [Signal X3DH spec]: https://signal.org/docs/specifications/x3dh/

use zeroize::Zeroize;

use crate::keys::{IdentityKeyPair, PreKeyPair, PublicKey};

/// The 32-byte secret established by the handshake. Zeroized on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct SharedSecret(pub(crate) [u8; 32]);

impl SharedSecret {
    /// The raw bytes (handle with care).
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl core::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SharedSecret(..)")
    }
}

impl Drop for SharedSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

fn derive(dh1: [u8; 32], dh2: [u8; 32], dh3: [u8; 32]) -> SharedSecret {
    let mut concat = [0u8; 96];
    concat[..32].copy_from_slice(&dh1);
    concat[32..64].copy_from_slice(&dh2);
    concat[64..].copy_from_slice(&dh3);
    let sk = crate::kdf::kdf_x3dh(&concat);
    concat.zeroize();
    SharedSecret(sk)
}

/// Initiator (Alice) side of the handshake.
///
/// - `our_identity`: Alice's long-term identity key pair (`IK_A`).
/// - `our_ephemeral`: a fresh ephemeral key pair for this handshake (`EK_A`).
/// - `their_identity`: Bob's published identity public key (`IK_B`).
/// - `their_prekey`: Bob's published signed prekey public key (`SPK_B`).
pub fn initiator_shared_secret(
    our_identity: &IdentityKeyPair,
    our_ephemeral: &PreKeyPair,
    their_identity: &PublicKey,
    their_prekey: &PublicKey,
) -> SharedSecret {
    let dh1 = our_identity.dh(their_prekey);
    let dh2 = our_ephemeral.dh(their_identity);
    let dh3 = our_ephemeral.dh(their_prekey);
    derive(dh1, dh2, dh3)
}

/// Responder (Bob) side of the handshake — computes the mirror of
/// [`initiator_shared_secret`].
///
/// - `our_identity`: Bob's long-term identity key pair (`IK_B`).
/// - `our_prekey`: Bob's signed prekey key pair (`SPK_B`).
/// - `their_identity`: Alice's identity public key (`IK_A`).
/// - `their_ephemeral`: Alice's ephemeral public key (`EK_A`).
pub fn responder_shared_secret(
    our_identity: &IdentityKeyPair,
    our_prekey: &PreKeyPair,
    their_identity: &PublicKey,
    their_ephemeral: &PublicKey,
) -> SharedSecret {
    let dh1 = our_prekey.dh(their_identity);
    let dh2 = our_identity.dh(their_ephemeral);
    let dh3 = our_prekey.dh(their_ephemeral);
    derive(dh1, dh2, dh3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyPair;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn both_sides_agree() {
        let mut rng = StdRng::seed_from_u64(42);
        let ik_a = KeyPair::generate(&mut rng);
        let ek_a = KeyPair::generate(&mut rng);
        let ik_b = KeyPair::generate(&mut rng);
        let spk_b = KeyPair::generate(&mut rng);

        let alice = initiator_shared_secret(&ik_a, &ek_a, &ik_b.public(), &spk_b.public());
        let bob = responder_shared_secret(&ik_b, &spk_b, &ik_a.public(), &ek_a.public());
        assert_eq!(alice, bob);
    }

    #[test]
    fn wrong_identity_disagrees() {
        let mut rng = StdRng::seed_from_u64(7);
        let ik_a = KeyPair::generate(&mut rng);
        let ek_a = KeyPair::generate(&mut rng);
        let ik_b = KeyPair::generate(&mut rng);
        let spk_b = KeyPair::generate(&mut rng);
        let imposter = KeyPair::generate(&mut rng);

        let alice = initiator_shared_secret(&ik_a, &ek_a, &ik_b.public(), &spk_b.public());
        // Bob computes with an attacker's identity in place of Alice's.
        let bob = responder_shared_secret(&ik_b, &spk_b, &imposter.public(), &ek_a.public());
        assert_ne!(alice, bob);
    }
}
