//! End-to-end tests for the E2EE core: X3DH-lite bootstrap + Double Ratchet.

use rabbithole_e2ee::keys::KeyPair;
use rabbithole_e2ee::{
    initiator_shared_secret, responder_shared_secret, sealed_open, sealed_seal, Error, Session,
};
use rand::rngs::StdRng;
use rand::SeedableRng;

const AD: &[u8] = b"rabbithole-dm-v1";

/// Establish a fresh Alice/Bob session pair via the X3DH-lite handshake.
fn establish() -> (Session<StdRng>, Session<StdRng>) {
    let mut setup = StdRng::seed_from_u64(0xABCD);
    let ik_a = KeyPair::generate(&mut setup);
    let ek_a = KeyPair::generate(&mut setup);
    let ik_b = KeyPair::generate(&mut setup);
    let spk_b = KeyPair::generate(&mut setup);

    let alice_sk = initiator_shared_secret(&ik_a, &ek_a, &ik_b.public(), &spk_b.public());
    let bob_sk = responder_shared_secret(&ik_b, &spk_b, &ik_a.public(), &ek_a.public());
    assert_eq!(alice_sk, bob_sk, "X3DH-lite must agree");

    let alice = Session::initiator(&alice_sk, spk_b.public(), StdRng::seed_from_u64(1));
    let bob = Session::responder(&bob_sk, spk_b, StdRng::seed_from_u64(2));
    (alice, bob)
}

#[test]
fn full_conversation_both_directions() {
    let (mut alice, mut bob) = establish();

    // Alice -> Bob (opens Bob's receiving chain and, via the reply, his sending chain).
    let m1 = alice.encrypt(b"hello bob", AD).unwrap();
    assert_eq!(bob.decrypt(&m1, AD).unwrap(), b"hello bob");

    // Bob can now reply.
    let r1 = bob.encrypt(b"hi alice", AD).unwrap();
    assert_eq!(alice.decrypt(&r1, AD).unwrap(), b"hi alice");

    // Several rounds, exercising DH ratchets in both directions.
    for i in 0..5u8 {
        let a = alice.encrypt(&[b'a', i], AD).unwrap();
        assert_eq!(bob.decrypt(&a, AD).unwrap(), vec![b'a', i]);
        let b = bob.encrypt(&[b'b', i], AD).unwrap();
        assert_eq!(alice.decrypt(&b, AD).unwrap(), vec![b'b', i]);
    }

    // Multiple messages in one direction without a reply (same sending chain).
    let bursts: Vec<_> = (0..4u8).map(|i| alice.encrypt(&[i], AD).unwrap()).collect();
    for (i, m) in bursts.iter().enumerate() {
        assert_eq!(bob.decrypt(m, AD).unwrap(), vec![i as u8]);
    }
}

#[test]
fn responder_cannot_send_before_first_receive() {
    let (_alice, mut bob) = establish();
    assert!(matches!(
        bob.encrypt(b"too early", AD),
        Err(Error::NoSendingChain)
    ));
}

#[test]
fn out_of_order_within_a_chain() {
    let (mut alice, mut bob) = establish();

    let m1 = alice.encrypt(b"one", AD).unwrap();
    let m2 = alice.encrypt(b"two", AD).unwrap();
    let m3 = alice.encrypt(b"three", AD).unwrap();

    // Deliver 3, then 1, then 2.
    assert_eq!(bob.decrypt(&m3, AD).unwrap(), b"three");
    assert_eq!(bob.decrypt(&m1, AD).unwrap(), b"one");
    assert_eq!(bob.decrypt(&m2, AD).unwrap(), b"two");
    assert_eq!(bob.skipped_len(), 0, "all skipped keys consumed");
}

#[test]
fn dropped_message_does_not_block_later_ones() {
    let (mut alice, mut bob) = establish();

    let _m1 = alice.encrypt(b"one", AD).unwrap();
    let m2 = alice.encrypt(b"two", AD).unwrap();
    let m3 = alice.encrypt(b"three", AD).unwrap();

    // m1 is dropped entirely; m2 and m3 still decrypt.
    assert_eq!(bob.decrypt(&m2, AD).unwrap(), b"two");
    assert_eq!(bob.decrypt(&m3, AD).unwrap(), b"three");
    // The key for the dropped m1 is buffered as skipped.
    assert_eq!(bob.skipped_len(), 1);
}

#[test]
fn skipped_across_dh_ratchet() {
    let (mut alice, mut bob) = establish();

    // Alice sends two, Bob receives only the first.
    let a1 = alice.encrypt(b"a1", AD).unwrap();
    let _a2 = alice.encrypt(b"a2", AD).unwrap();
    assert_eq!(bob.decrypt(&a1, AD).unwrap(), b"a1");

    // Bob replies (new ratchet), Alice reads it and replies again -> another ratchet.
    let b1 = bob.encrypt(b"b1", AD).unwrap();
    assert_eq!(alice.decrypt(&b1, AD).unwrap(), b"b1");

    // Alice sends across the new chain; a2 (old chain) is still decryptable later.
    let a3 = alice.encrypt(b"a3", AD).unwrap();
    assert_eq!(bob.decrypt(&a3, AD).unwrap(), b"a3");
    assert_eq!(bob.decrypt(&_a2, AD).unwrap(), b"a2");
}

#[test]
fn tamper_is_detected_and_session_survives() {
    let (mut alice, mut bob) = establish();

    let good = alice.encrypt(b"authentic", AD).unwrap();
    let mut bad = good.clone();
    bad.ciphertext[0] ^= 0xff;

    assert!(matches!(bob.decrypt(&bad, AD), Err(Error::Decrypt)));
    // Wrong associated data is also rejected.
    assert!(matches!(
        bob.decrypt(&good, b"wrong-ad"),
        Err(Error::Decrypt)
    ));
    // The session was not corrupted by the failed attempts.
    assert_eq!(bob.decrypt(&good, AD).unwrap(), b"authentic");
}

#[test]
fn skipped_key_bound_is_enforced() {
    let (mut alice, mut bob) = establish();
    bob.set_max_skip(5);

    // Establish Bob's receiving chain.
    let m0 = alice.encrypt(b"m0", AD).unwrap();
    assert_eq!(bob.decrypt(&m0, AD).unwrap(), b"m0");

    // Forge a message far ahead in the same chain: exceeds the skip bound.
    let mut ahead = alice.encrypt(b"ahead", AD).unwrap();
    ahead.header.msg_num = 100;
    assert!(matches!(
        bob.decrypt(&ahead, AD),
        Err(Error::TooManySkipped { max: 5 })
    ));

    // Within the bound, skipping succeeds.
    for _ in 0..4 {
        let _ = alice.encrypt(b"filler", AD).unwrap();
    }
    let m5 = alice.encrypt(b"m5", AD).unwrap();
    assert_eq!(bob.decrypt(&m5, AD).unwrap(), b"m5");
    assert!(bob.skipped_len() <= 5);
}

#[test]
fn sealed_sender_roundtrip() {
    let mut rng = StdRng::seed_from_u64(99);
    let bob = KeyPair::generate(&mut rng);

    let env = sealed_seal(
        &bob.public(),
        b"metadata-light hello",
        b"envelope-ad",
        &mut rng,
    );
    // The envelope carries only an unlinkable ephemeral key, not Bob-derived data.
    assert_ne!(env.ephemeral_pub, bob.public());
    assert_eq!(
        sealed_open(&bob, &env, b"envelope-ad").unwrap(),
        b"metadata-light hello"
    );

    // Wrong recipient cannot open it.
    let mallory = KeyPair::generate(&mut rng);
    assert!(matches!(
        sealed_open(&mallory, &env, b"envelope-ad"),
        Err(Error::Decrypt)
    ));
}
