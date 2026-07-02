//! The Double Ratchet ([Signal Double Ratchet spec]).
//!
//! Provides a forward-secret, self-healing (post-compromise secure) channel for a
//! 1:1 conversation. Two ratchets are combined:
//!
//! - a **symmetric-key ratchet** advances a chain key once per message
//!   ([`crate::kdf::kdf_ck`]), so every message gets a unique key that is deleted
//!   after use (forward secrecy within a chain);
//! - a **Diffie–Hellman ratchet** mixes a fresh DH output into the root key
//!   ([`crate::kdf::kdf_rk`]) whenever a new ratchet public key is seen, starting a
//!   new chain (post-compromise security across chains).
//!
//! Each message carries a [`Header`] `{ratchet_pub, prev_chain_len, msg_num}` so the
//! receiver can detect ratchet steps and message gaps. Out-of-order and dropped
//! messages are handled by deriving and storing **skipped message keys**; this
//! store is bounded ([`Session::DEFAULT_MAX_SKIP`]) so a peer cannot force
//! unbounded key derivation or memory use by claiming a huge message number.
//!
//! Decryption is transactional: no session state is mutated until the AEAD tag
//! verifies, so a forged/tampered message is rejected without corrupting the
//! session.
//!
//! [Signal Double Ratchet spec]: https://signal.org/docs/specifications/doubleratchet/

use std::collections::HashMap;

use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::aead;
use crate::kdf::{kdf_ck, kdf_rk};
use crate::keys::{KeyPair, PreKeyPair, PublicKey};
use crate::x3dh::SharedSecret;
use crate::{Error, Result};

/// Per-message header, sent in the clear alongside the ciphertext.
///
/// The header is also mixed into the AEAD associated data, so tampering with any
/// field is detected on decryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// The sender's current ratchet public key.
    pub ratchet_pub: PublicKey,
    /// Number of messages in the sender's previous sending chain.
    pub prev_chain_len: u32,
    /// This message's index within the current sending chain.
    pub msg_num: u32,
}

impl Header {
    /// Deterministic byte encoding used for AEAD associated-data binding.
    fn to_bytes(self) -> [u8; 40] {
        let mut out = [0u8; 40];
        out[..32].copy_from_slice(self.ratchet_pub.as_bytes());
        out[32..36].copy_from_slice(&self.prev_chain_len.to_le_bytes());
        out[36..].copy_from_slice(&self.msg_num.to_le_bytes());
        out
    }
}

/// A ready-to-send ciphertext with its header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Ratchet header (public metadata, integrity-protected).
    pub header: Header,
    /// ChaCha20-Poly1305 ciphertext (tag appended).
    pub ciphertext: Vec<u8>,
}

/// Bind the caller's associated data to the header for the AEAD.
fn associated_data(ad: &[u8], header: &Header) -> Vec<u8> {
    let hdr = header.to_bytes();
    let mut out = Vec::with_capacity(ad.len() + hdr.len());
    out.extend_from_slice(ad);
    out.extend_from_slice(&hdr);
    out
}

/// A live Double Ratchet session for one peer.
///
/// Generic over the RNG so callers control the entropy source (native clients pass
/// the OS RNG; tests inject a seeded RNG for determinism). The RNG is used only
/// when the session generates a new ratchet key during a DH-ratchet step.
pub struct Session<R: RngCore + CryptoRng> {
    /// Our current ratchet key pair (`DHs`).
    dhs: KeyPair,
    /// Their current ratchet public key (`DHr`).
    dhr: Option<PublicKey>,
    /// Root key (`RK`).
    rk: [u8; 32],
    /// Sending chain key (`CKs`).
    cks: Option<[u8; 32]>,
    /// Receiving chain key (`CKr`).
    ckr: Option<[u8; 32]>,
    /// Sending message counter (`Ns`).
    ns: u32,
    /// Receiving message counter (`Nr`).
    nr: u32,
    /// Length of the previous sending chain (`PN`).
    pn: u32,
    /// Skipped-but-derived message keys, keyed by `(ratchet_pub, msg_num)`.
    skipped: HashMap<(PublicKey, u32), [u8; 32]>,
    /// Maximum number of skipped keys we will derive/store.
    max_skip: u32,
    /// Entropy source for DH-ratchet key generation.
    rng: R,
}

impl<R: RngCore + CryptoRng> Session<R> {
    /// Default bound on stored skipped message keys (DoS protection).
    pub const DEFAULT_MAX_SKIP: u32 = 1000;

    /// Initiator ("Alice") side: bootstrap from the X3DH shared secret and the
    /// responder's published prekey public key (`SPK_B`).
    ///
    /// Alice generates her first ratchet key and immediately DH-ratchets toward
    /// Bob's prekey to open her sending chain.
    pub fn initiator(shared: &SharedSecret, their_prekey: PublicKey, mut rng: R) -> Self {
        let dhs = KeyPair::generate(&mut rng);
        let (rk, cks) = kdf_rk(shared.as_bytes(), &dhs.dh(&their_prekey));
        Self {
            dhs,
            dhr: Some(their_prekey),
            rk,
            cks: Some(cks),
            ckr: None,
            ns: 0,
            nr: 0,
            pn: 0,
            skipped: HashMap::new(),
            max_skip: Self::DEFAULT_MAX_SKIP,
            rng,
        }
    }

    /// Responder ("Bob") side: bootstrap from the X3DH shared secret using his
    /// signed prekey key pair (`SPK_B`) as the initial ratchet key.
    ///
    /// Bob has no sending chain until he decrypts Alice's first message; calling
    /// [`Session::encrypt`] before then returns [`Error::NoSendingChain`].
    pub fn responder(shared: &SharedSecret, our_prekey: PreKeyPair, rng: R) -> Self {
        Self {
            dhs: our_prekey,
            dhr: None,
            rk: *shared.as_bytes(),
            cks: None,
            ckr: None,
            ns: 0,
            nr: 0,
            pn: 0,
            skipped: HashMap::new(),
            max_skip: Self::DEFAULT_MAX_SKIP,
            rng,
        }
    }

    /// Override the skipped-message-key bound (mainly for testing).
    pub fn set_max_skip(&mut self, max_skip: u32) {
        self.max_skip = max_skip;
    }

    /// Encrypt `plaintext`, binding `ad` as associated data.
    pub fn encrypt(&mut self, plaintext: &[u8], ad: &[u8]) -> Result<Message> {
        let cks = self.cks.ok_or(Error::NoSendingChain)?;
        let (next_cks, mk) = kdf_ck(&cks);
        let header = Header {
            ratchet_pub: self.dhs.public(),
            prev_chain_len: self.pn,
            msg_num: self.ns,
        };
        let ad_full = associated_data(ad, &header);
        let ciphertext = aead::seal(&mk, plaintext, &ad_full);
        self.cks = Some(next_cks);
        self.ns += 1;
        Ok(Message { header, ciphertext })
    }

    /// Decrypt `msg`, checking `ad`. Handles out-of-order, skipped, and dropped
    /// messages; returns [`Error::Decrypt`] on tamper/auth failure without
    /// mutating session state.
    pub fn decrypt(&mut self, msg: &Message, ad: &[u8]) -> Result<Vec<u8>> {
        let ad_full = associated_data(ad, &msg.header);

        // 1) A previously skipped message?
        let skipped_key = (msg.header.ratchet_pub, msg.header.msg_num);
        if let Some(mk) = self.skipped.get(&skipped_key).copied() {
            let pt = aead::open(&mk, &msg.ciphertext, &ad_full)?;
            self.skipped.remove(&skipped_key);
            return Ok(pt);
        }

        // 2) Stage all state changes in locals; commit only after the AEAD verifies.
        let mut staged: Vec<((PublicKey, u32), [u8; 32])> = Vec::new();
        let mut dhr = self.dhr;
        let mut ckr = self.ckr;
        let mut nr = self.nr;
        let mut rk = self.rk;
        let mut cks = self.cks;
        let mut ns = self.ns;
        let mut pn = self.pn;
        let mut new_dhs: Option<KeyPair> = None;

        if dhr != Some(msg.header.ratchet_pub) {
            // Finish the current receiving chain, then perform a DH ratchet.
            skip_on_chain(
                &mut ckr,
                &mut nr,
                dhr,
                msg.header.prev_chain_len,
                &mut staged,
                self.max_skip,
                self.skipped.len(),
            )?;

            let received = msg.header.ratchet_pub;
            pn = ns;
            ns = 0;
            nr = 0;
            let (rk_a, ckr_new) = kdf_rk(&rk, &self.dhs.dh(&received));
            let generated = KeyPair::generate(&mut self.rng);
            let (rk_b, cks_new) = kdf_rk(&rk_a, &generated.dh(&received));
            rk = rk_b;
            dhr = Some(received);
            ckr = Some(ckr_new);
            cks = Some(cks_new);
            new_dhs = Some(generated);
        }

        // Skip up to this message's number on the (possibly new) receiving chain.
        let already_stored = self.skipped.len() + staged.len();
        skip_on_chain(
            &mut ckr,
            &mut nr,
            dhr,
            msg.header.msg_num,
            &mut staged,
            self.max_skip,
            already_stored,
        )?;

        // Derive this message's key and attempt decryption.
        let chain = ckr.ok_or(Error::NoReceivingChain)?;
        let (ckr_next, mk) = kdf_ck(&chain);
        let pt = aead::open(&mk, &msg.ciphertext, &ad_full)?;

        // 3) Commit.
        self.dhr = dhr;
        self.rk = rk;
        self.ckr = Some(ckr_next);
        self.cks = cks;
        self.nr = nr + 1;
        self.ns = ns;
        self.pn = pn;
        if let Some(pair) = new_dhs {
            self.dhs = pair;
        }
        for (k, v) in staged {
            self.skipped.insert(k, v);
        }
        Ok(pt)
    }

    /// Number of skipped message keys currently stored (for tests/metrics).
    pub fn skipped_len(&self) -> usize {
        self.skipped.len()
    }
}

/// Advance a receiving chain from `nr` up to `until`, buffering the skipped
/// message keys. Enforces the skip bound to resist DoS.
#[allow(clippy::too_many_arguments)]
fn skip_on_chain(
    ckr: &mut Option<[u8; 32]>,
    nr: &mut u32,
    dhr: Option<PublicKey>,
    until: u32,
    out: &mut Vec<((PublicKey, u32), [u8; 32])>,
    max_skip: u32,
    already_stored: usize,
) -> Result<()> {
    if until <= *nr {
        return Ok(());
    }
    let to_skip = until - *nr;
    if to_skip > max_skip || already_stored + to_skip as usize > max_skip as usize {
        return Err(Error::TooManySkipped { max: max_skip });
    }
    if let (Some(ck), Some(dhrp)) = (ckr.as_mut(), dhr) {
        while *nr < until {
            let (next, mk) = kdf_ck(ck);
            out.push(((dhrp, *nr), mk));
            *ck = next;
            *nr += 1;
        }
    }
    Ok(())
}
