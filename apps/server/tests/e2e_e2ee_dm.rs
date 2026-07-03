//! Wave 13 end-to-end tests: opt-in E2EE for 1:1 DMs.
//!
//! Two clients publish prekey bundles; one fetches the other's, establishes an
//! X3DH-lite + Double Ratchet session, and exchanges encrypted DMs. The server
//! relays the ciphertext opaquely — the crucial assertions here check that the
//! server-stored DM row holds NO plaintext, only the opaque payload. The
//! unchanged plaintext DM path and one-time-prekey exhaustion (fall back to
//! 3-DH) are covered too. Deterministic: seeded RNGs, bounded push pumping, no
//! sleeps.

use burrow::Burrow;
use rabbithole_core::{Client, E2eeIdentity, E2eeSession};
use rabbithole_proto::dm::DmReceived;
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::repo2::PersonasRepo;
use rabbithole_store_server::repo3::DmsRepo;
use rand::rngs::StdRng;
use rand::SeedableRng;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "E2EE Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn login(burrow: &Burrow, user: &str, pw: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, pw).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

/// Pump pushes until a `DmReceived` arrives (bounded), returning its message.
async fn wait_dm(c: &mut Client) -> rabbithole_proto::dm::DmMessage {
    for _ in 0..20 {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), c.next_push())
            .await
            .expect("timeout waiting for DM push")
            .unwrap()
            .expect("push");
        if let Some(Ok(dm)) = frame.decode::<DmReceived>() {
            return dm.message;
        }
    }
    panic!("expected DmReceived push not seen");
}

async fn account_id(burrow: &Burrow, screen_name: &str) -> i64 {
    PersonasRepo(&burrow.shared.pool)
        .by_screen_name(screen_name)
        .await
        .unwrap()
        .expect("persona")
        .account_id
}

#[tokio::test]
async fn e2ee_dm_roundtrip_server_holds_no_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for u in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(u, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }

    let mut alice = login(&burrow, "alice", "pw-pw-pw").await;
    let mut bob = login(&burrow, "bob", "pw-pw-pw").await;

    // Each side generates its E2EE identity and publishes a prekey bundle.
    let mut rng = StdRng::seed_from_u64(100);
    let alice_id = E2eeIdentity::generate(&mut rng);
    let bob_id = E2eeIdentity::generate(&mut rng);
    let alice_otps = E2eeIdentity::generate_one_time_prekeys(&mut rng, 4);
    let bob_otps = E2eeIdentity::generate_one_time_prekeys(&mut rng, 4);
    alice
        .key_bundle_publish(&alice_id.publish(&alice_otps))
        .await
        .unwrap();
    bob.key_bundle_publish(&bob_id.publish(&bob_otps))
        .await
        .unwrap();

    // Alice fetches Bob's bundle and establishes an initiator session.
    let bundle = alice.key_bundle_fetch("bob").await.unwrap();
    assert!(
        bundle.one_time_prekey.is_some(),
        "first fetch consumes a one-time prekey"
    );
    let mut a_sess = E2eeSession::initiate(&alice_id, &bundle, StdRng::seed_from_u64(1)).unwrap();

    // Alice sends the first encrypted DM.
    let secret1 = b"the eagle lands at midnight";
    let payload1 = a_sess.encrypt(secret1).unwrap();
    assert!(
        payload1.prekey.is_some(),
        "first message carries the prologue"
    );
    alice.dm_send_encrypted("bob", payload1).await.unwrap();

    // Bob receives an opaque payload (empty plaintext) and decrypts it.
    let msg1 = wait_dm(&mut bob).await;
    assert_eq!(msg1.text, "", "delivered plaintext field is empty");
    let enc1 = msg1.encrypted.clone().expect("encrypted carriage present");
    let prologue = enc1.prekey.clone().expect("prologue on first message");
    let mut b_sess = E2eeSession::respond(&bob_id, &prologue, StdRng::seed_from_u64(2));
    assert_eq!(b_sess.decrypt(&enc1).unwrap(), secret1);

    // The SERVER-STORED row holds NO plaintext: empty text + opaque blob that
    // does not contain the secret bytes.
    let a_acct = account_id(&burrow, "alice").await;
    let b_acct = account_id(&burrow, "bob").await;
    let rows = DmsRepo(&burrow.shared.pool)
        .thread(a_acct, b_acct, 0, 10)
        .await
        .unwrap();
    let stored = rows.last().expect("a stored DM");
    assert_eq!(stored.text, "", "server stored no plaintext");
    let blob = stored.encrypted.as_ref().expect("opaque ciphertext stored");
    assert!(
        !blob.windows(secret1.len()).any(|w| w == secret1),
        "the plaintext must not appear in the stored ciphertext blob"
    );

    // A second message ratchets the session forward (no prologue this time).
    let secret2 = b"rendezvous at the old mill";
    let payload2 = a_sess.encrypt(secret2).unwrap();
    assert!(
        payload2.prekey.is_none(),
        "subsequent messages omit the prologue"
    );
    alice.dm_send_encrypted("bob", payload2).await.unwrap();
    let msg2 = wait_dm(&mut bob).await;
    let enc2 = msg2.encrypted.expect("encrypted carriage present");
    assert_eq!(b_sess.decrypt(&enc2).unwrap(), secret2);

    // The plaintext DM path still works, unchanged, alongside E2EE.
    alice
        .dm_send(&rabbithole_proto::dm::DmSend::new(
            "bob",
            "hello in the clear",
        ))
        .await
        .unwrap();
    let plain = wait_dm(&mut bob).await;
    assert_eq!(plain.text, "hello in the clear");
    assert!(plain.encrypted.is_none());
}

#[tokio::test]
async fn e2ee_session_survives_one_time_prekey_exhaustion() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for u in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(u, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let mut alice = login(&burrow, "alice", "pw-pw-pw").await;
    let mut bob = login(&burrow, "bob", "pw-pw-pw").await;

    let mut rng = StdRng::seed_from_u64(200);
    let alice_id = E2eeIdentity::generate(&mut rng);
    let bob_id = E2eeIdentity::generate(&mut rng);
    // Bob publishes exactly ONE one-time prekey.
    let bob_otps = E2eeIdentity::generate_one_time_prekeys(&mut rng, 1);
    bob.key_bundle_publish(&bob_id.publish(&bob_otps))
        .await
        .unwrap();

    // First fetch consumes the only OTP.
    let first = alice.key_bundle_fetch("bob").await.unwrap();
    assert!(first.one_time_prekey.is_some());
    // Second fetch: pool exhausted, but the bundle still returns (OTP = None).
    let second = alice.key_bundle_fetch("bob").await.unwrap();
    assert!(
        second.one_time_prekey.is_none(),
        "pool exhausted falls back to no one-time prekey"
    );

    // A session established from the OTP-less bundle still works end to end.
    let mut a_sess = E2eeSession::initiate(&alice_id, &second, StdRng::seed_from_u64(1)).unwrap();
    let secret = b"no prekey, still secret";
    let payload = a_sess.encrypt(secret).unwrap();
    alice.dm_send_encrypted("bob", payload).await.unwrap();

    let msg = wait_dm(&mut bob).await;
    let enc = msg.encrypted.expect("encrypted carriage present");
    let prologue = enc.prekey.clone().expect("prologue present");
    assert!(prologue.one_time_prekey.is_none());
    let mut b_sess = E2eeSession::respond(&bob_id, &prologue, StdRng::seed_from_u64(2));
    assert_eq!(b_sess.decrypt(&enc).unwrap(), secret);
}

#[tokio::test]
async fn key_bundle_fetch_unknown_persona_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let mut alice = login(&burrow, "alice", "pw-pw-pw").await;

    // No such persona.
    assert!(matches!(
        alice.key_bundle_fetch("ghost").await,
        Err(rabbithole_core::ClientError::Refused(
            rabbithole_proto::ErrorCode::NotFound
        ))
    ));

    // Persona exists but never published a bundle: also NotFound.
    burrow
        .shared
        .auth
        .create_account("bob", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    assert!(matches!(
        alice.key_bundle_fetch("bob").await,
        Err(rabbithole_core::ClientError::Refused(
            rabbithole_proto::ErrorCode::NotFound
        ))
    ));
}
