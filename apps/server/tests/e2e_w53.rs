//! Wave 5.3a end-to-end tests: peer-wire contact cards and server-signed
//! capability tokens — the trust foundation the peer wire builds on.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::swarm::AdvertEntry;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_swarm::{CapError, CapToken};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Ticket Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

#[tokio::test]
async fn contact_card_joins_source_listings() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for n in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }

    let mut alice = login(&burrow, "alice").await;
    alice
        .swarm_advertise(
            vec![AdvertEntry::new([1; 32], 64, "a.bin", "text/plain")],
            0,
        )
        .await
        .unwrap();

    // No contact registered yet: sources are coordinator-only.
    let mut bob = login(&burrow, "bob").await;
    let list = bob.swarm_find([1; 32]).await.unwrap();
    assert_eq!(list.sources[0].endpoint, None);
    assert_eq!(list.sources[0].cert_fp, None);

    // Register a contact card; the endpoint appears with the observed IP
    // (loopback here) + the declared port, plus the pinned fingerprint.
    alice.swarm_contact(4655, [7; 32]).await.unwrap();
    let list = bob.swarm_find([1; 32]).await.unwrap();
    assert_eq!(list.sources[0].endpoint.as_deref(), Some("127.0.0.1:4655"));
    assert_eq!(list.sources[0].cert_fp, Some([7; 32]));

    // Port 0 is nonsense and refused.
    assert!(matches!(
        alice.swarm_contact(0, [7; 32]).await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

    burrow.shutdown().await;
}

#[tokio::test]
async fn tickets_verify_against_the_server_key() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let mut alice = login(&burrow, "alice").await;

    let root = [9u8; 32];
    let ticket = alice.swarm_ticket(root).await.unwrap();
    let token = CapToken::from_bytes(&ticket.token).expect("token decodes");
    assert_eq!(token.claim.expires_unix, ticket.expires_unix);
    assert_eq!(token.claim.fetcher, "alice");

    // The key every session learned at hello verifies the token — exactly
    // what a serving peer will do.
    let server_key = alice.server.server_key;
    let now = chrono::Utc::now().timestamp();
    assert_eq!(token.verify(&server_key, &root, now), Ok(()));
    // …and it is bound to its root and its expiry.
    assert_eq!(
        token.verify(&server_key, &[8; 32], now),
        Err(CapError::WrongRoot)
    );
    assert_eq!(
        token.verify(&server_key, &root, ticket.expires_unix),
        Err(CapError::Expired)
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn guests_cannot_get_tickets_or_register_contacts() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    let mut guest = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    guest.auth_guest(Some("Dormouse".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();

    // Guests hold neither FILE_DOWNLOAD nor SWARM_ADVERTISE.
    assert!(matches!(
        guest.swarm_ticket([1; 32]).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        guest.swarm_contact(4655, [7; 32]).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    burrow.shutdown().await;
}

#[tokio::test]
async fn full_peer_to_peer_fetch_via_the_coordinator() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for n in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }

    // Alice seeds a ~300 KiB file on a real peer-wire endpoint, registers
    // her contact card, and advertises the root. Bytes never touch the
    // server.
    let body: Vec<u8> = (0..300 * 1024 + 55).map(|i| (i % 251) as u8).collect();
    let src = work.path().join("seed.bin");
    std::fs::write(&src, &body).unwrap();
    let root = *blake3::hash(&body).as_bytes();

    let mut alice = login(&burrow, "alice").await;
    let seeds = std::sync::Arc::new(rabbithole_swarm::SeedStore::new());
    seeds.add(root, &src).unwrap();
    let peer = rabbithole_swarm::PeerServer::start(
        "127.0.0.1:0".parse().unwrap(),
        alice.server.server_key,
        seeds,
    )
    .await
    .unwrap();
    alice
        .swarm_advertise(
            vec![AdvertEntry::new(
                root,
                body.len() as u64,
                "seed.bin",
                "application/octet-stream",
            )],
            0,
        )
        .await
        .unwrap();
    alice
        .swarm_contact(peer.addr.port(), peer.fingerprint.0)
        .await
        .unwrap();

    // Bob: find → ticket → fetch straight from alice's peer, verified
    // block-by-block against the root.
    let mut bob = login(&burrow, "bob").await;
    let list = bob.swarm_find(root).await.unwrap();
    let source = &list.sources[0];
    let endpoint = source.endpoint.clone().expect("alice registered contact");
    let ticket = bob.swarm_ticket(root).await.unwrap();

    let dest = work.path().join("fetched.bin");
    let n = rabbithole_swarm::fetch_file(
        &endpoint,
        source.cert_fp.unwrap(),
        &ticket.token,
        root,
        source.size,
        &dest,
    )
    .await
    .unwrap();
    assert_eq!(n, body.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), body, "P2P bytes verified");

    peer.stop();
    burrow.shutdown().await;
}

/// A client that presents a portable identity AND proves possession (signs the
/// server's challenge nonce) is surfaced in the who-list with its key; a
/// handle-only client shows `None`. The verified half of key-based People de-dup,
/// end to end over a real Burrow with a real Ed25519 challenge/response.
#[tokio::test]
async fn who_list_carries_the_verified_pubkey() {
    use rabbithole_identity::IdentityKey;
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for n in ["keyed", "bare"] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());

    // "keyed" connects with an identity key; connect_with_identity signs the
    // challenge nonce and returns the KeyProof, so the server verifies possession.
    let key = IdentityKey::from_seed(&[42; 32]);
    let expected = key.public().0;
    let mut keyed = Client::connect_with_identity(&url, None, None, "e2e", "0", Some(&key))
        .await
        .unwrap();
    keyed.auth_password("keyed", "pw-pw-pw").await.unwrap();
    keyed.expect_welcome().await.unwrap();

    // "bare" connects the ordinary way (no key).
    let mut bare = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    bare.auth_password("bare", "pw-pw-pw").await.unwrap();
    bare.expect_welcome().await.unwrap();

    // The who-list carries the *verified* key (or None), not a raw claim.
    let who = keyed.who().await.unwrap();
    let keyed_row = who.iter().find(|u| u.screen_name == "keyed").unwrap();
    let bare_row = who.iter().find(|u| u.screen_name == "bare").unwrap();
    assert_eq!(
        keyed_row.pubkey,
        Some(expected),
        "verified session carries its key"
    );
    assert_eq!(bare_row.pubkey, None, "handle-only session has no key");

    burrow.shutdown().await;
}

/// Over QUIC the proof is channel-bound to the pinned cert fingerprint, and the
/// honest path still verifies: a client that connects to the real burrow's QUIC
/// endpoint with the correct pinned fingerprint gets its key surfaced. Confirms
/// the client (pinned fp) and server (own cert fp) compute the same binder — so
/// channel binding hardens QUIC without breaking it.
#[tokio::test]
async fn quic_client_with_pinned_fingerprint_is_verified() {
    use rabbithole_identity::IdentityKey;
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("qq", "pw-pw-pw", Role::User)
        .await
        .unwrap();

    let key = IdentityKey::from_seed(&[71; 32]);
    let fp = burrow.fingerprint.to_hex();
    let mut c = Client::connect_with_identity(
        &format!("127.0.0.1:{}", burrow.quic_addr.port()),
        None,
        Some(&fp),
        "e2e",
        "0",
        Some(&key),
    )
    .await
    .unwrap();
    c.auth_password("qq", "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();

    let who = c.who().await.unwrap();
    let row = who.iter().find(|u| u.screen_name == "qq").unwrap();
    assert_eq!(
        row.pubkey,
        Some(key.public().0),
        "QUIC channel-bound proof verifies on the honest path"
    );

    burrow.shutdown().await;
}

/// A cryptographically VALID signature by the real key, but over the WRONG
/// channel binder, is rejected — this is exactly a relayed proof (the victim
/// signed for the burrow they were on; a different burrow verifies over its own
/// cert fingerprint). Proves the channel binding, not just possession, is checked.
#[tokio::test]
async fn proof_over_wrong_channel_binder_is_rejected() {
    use rabbithole_identity::IdentityKey;
    use rabbithole_net::ws::WsTransport;
    use rabbithole_net::Transport;
    use rabbithole_proto::hello::{key_auth_message, Hello, HelloAck, KeyProof};
    use rabbithole_proto::session::AuthPassword;
    use rabbithole_proto::{CapabilitySet, Frame, RequestId};

    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for n in ["victim", "watcher"] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());

    // The client holds the real key and signs a genuine signature — but binds it
    // to some OTHER burrow's cert fingerprint (as a relay would), not the zero
    // binder this WS server verifies over.
    let key = IdentityKey::from_seed(&[55; 32]);
    let wrong_binder = [0xAB; 32]; // a different burrow's cert fp
    let mut conn = WsTransport.connect(&url).await.unwrap();
    conn.send(
        Frame::request(
            RequestId(1),
            &Hello::new("v", "0", CapabilitySet::default()).with_pubkey(Some(key.public().0)),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let ack: HelloAck = conn
        .recv()
        .await
        .unwrap()
        .unwrap()
        .decode()
        .unwrap()
        .unwrap();
    let nonce = ack.challenge.expect("challenged");
    let sig = key
        .sign(&key_auth_message(&wrong_binder, &nonce))
        .0
        .to_vec();
    conn.send(Frame::request(RequestId(2), &KeyProof::new(sig)).unwrap())
        .await
        .unwrap();
    let _ = conn.recv().await.unwrap(); // ack
    conn.send(Frame::request(RequestId(3), &AuthPassword::new("victim", "pw-pw-pw")).unwrap())
        .await
        .unwrap();
    let _ = conn.recv().await.unwrap();

    let mut watcher = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    watcher.auth_password("watcher", "pw-pw-pw").await.unwrap();
    watcher.expect_welcome().await.unwrap();
    let who = watcher.who().await.unwrap();
    let victim = who.iter().find(|u| u.screen_name == "victim").unwrap();
    assert_eq!(
        victim.pubkey, None,
        "a proof over the wrong channel binder is rejected"
    );

    burrow.shutdown().await;
}

/// A client that *claims* an identity key but cannot prove possession (sends a
/// garbage KeyProof) is NOT surfaced in the who-list — the whole point of the
/// challenge/response. Guards against the impersonation the review flagged:
/// reading a victim's public key and re-presenting it can't earn the key badge.
#[tokio::test]
async fn unproven_claimed_pubkey_is_not_surfaced() {
    use rabbithole_net::ws::WsTransport;
    use rabbithole_net::Transport;
    use rabbithole_proto::hello::{Hello, HelloAck, KeyProof};
    use rabbithole_proto::session::AuthPassword;
    use rabbithole_proto::{CapabilitySet, Frame, RequestId};

    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("mallory", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());

    // Raw handshake: claim a victim's key but sign nothing valid.
    let victim_key = [7u8; 32];
    let mut conn = WsTransport.connect(&url).await.unwrap();
    conn.send(
        Frame::request(
            RequestId(1),
            &Hello::new("evil", "0", CapabilitySet::default()).with_pubkey(Some(victim_key)),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let ack = conn.recv().await.unwrap().unwrap();
    let ack: HelloAck = ack.decode::<HelloAck>().unwrap().unwrap();
    assert!(ack.challenge.is_some(), "server challenged the claimed key");
    // A garbage signature: possession is not proved.
    conn.send(Frame::request(RequestId(2), &KeyProof::new(vec![0u8; 64])).unwrap())
        .await
        .unwrap();
    let _ = conn.recv().await.unwrap(); // ack (verification failed internally)
    conn.send(Frame::request(RequestId(3), &AuthPassword::new("mallory", "pw-pw-pw")).unwrap())
        .await
        .unwrap();
    // Drain the AuthOk reply so mallory is fully authenticated + joined to
    // presence before we query the roster (avoids a join race).
    let _ = conn.recv().await.unwrap();

    // A legit client reads the roster: mallory must appear WITHOUT a key.
    let mut good = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    burrow
        .shared
        .auth
        .create_account("good", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    good.auth_password("good", "pw-pw-pw").await.unwrap();
    good.expect_welcome().await.unwrap();
    let who = good.who().await.unwrap();
    let mallory = who.iter().find(|u| u.screen_name == "mallory").unwrap();
    assert_eq!(
        mallory.pubkey, None,
        "an unproven claimed key is never surfaced"
    );

    burrow.shutdown().await;
}

/// The native desktop download recipe end-to-end against a real coordinator:
/// three peers seed one file, the fetcher discovers all of them via `swarm_find`,
/// filters to dialable `SourcePeer`s, gets one ticket, and pulls the file
/// **multi-source** with `fetch_swarm_resumable` — byte-exact, every unit served
/// exactly once across the swarm. This is the Rust half of `run_swarm_download`
/// (apps/desktop), exercised against a live Burrow rather than composed pieces.
#[tokio::test]
async fn multi_source_swarm_fetch_via_coordinator() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for n in ["alice", "carol", "dave", "bob"] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }

    // ~3 MiB + tail → four 1 MiB work units, seeded on THREE peers.
    let body: Vec<u8> = (0..3 * 1024 * 1024 + 17).map(|i| (i % 251) as u8).collect();
    let src = work.path().join("seed.bin");
    std::fs::write(&src, &body).unwrap();
    let root = *blake3::hash(&body).as_bytes();

    // Each seeder logs in, stands up a peer-wire endpoint, advertises the root,
    // and registers its contact card. Sessions are kept alive (a contact card
    // dies with its session) until after the fetch.
    let mut peers = Vec::new();
    let mut sessions = Vec::new();
    for name in ["alice", "carol", "dave"] {
        let mut s = login(&burrow, name).await;
        let seeds = std::sync::Arc::new(rabbithole_swarm::SeedStore::new());
        seeds.add(root, &src).unwrap();
        let peer = rabbithole_swarm::PeerServer::start(
            "127.0.0.1:0".parse().unwrap(),
            s.server.server_key,
            seeds,
        )
        .await
        .unwrap();
        s.swarm_advertise(
            vec![AdvertEntry::new(
                root,
                body.len() as u64,
                "seed.bin",
                "application/octet-stream",
            )],
            0,
        )
        .await
        .unwrap();
        s.swarm_contact(peer.addr.port(), peer.fingerprint.0)
            .await
            .unwrap();
        peers.push(peer);
        sessions.push(s);
    }

    // Bob runs the recipe: find → filter to dialable SourcePeers → ticket → fetch.
    let mut bob = login(&burrow, "bob").await;
    let list = bob.swarm_find(root).await.unwrap();
    let sources: Vec<rabbithole_swarm::SourcePeer> = list
        .sources
        .iter()
        .filter_map(|s| {
            Some(rabbithole_swarm::SourcePeer {
                endpoint: s.endpoint.clone()?,
                cert_fp: s.cert_fp?,
            })
        })
        .collect();
    assert_eq!(
        sources.len(),
        3,
        "the coordinator returned all three dialable peers"
    );
    let ticket = bob.swarm_ticket(root).await.unwrap();

    let dest = work.path().join("fetched.bin");
    let report = rabbithole_swarm::fetch_swarm_resumable(
        &sources,
        &ticket.token,
        root,
        body.len() as u64,
        &dest,
    )
    .await
    .unwrap();
    assert_eq!(report.bytes, body.len() as u64);
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body,
        "multi-source bytes verified"
    );
    // Every one of the four units was served exactly once across the swarm.
    let total: u64 = report.per_source.iter().map(|(_, n)| n).sum();
    assert_eq!(total, 4, "each unit served once: {:?}", report.per_source);

    for p in peers {
        p.stop();
    }
    burrow.shutdown().await;
}

#[tokio::test]
async fn contact_card_dies_with_the_session() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for n in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }

    // Alice's first session registers a contact and advertises. A second
    // session re-advertises the same root WITHOUT a contact card.
    let mut a1 = login(&burrow, "alice").await;
    a1.swarm_advertise(
        vec![AdvertEntry::new([2; 32], 10, "x.bin", "text/plain")],
        0,
    )
    .await
    .unwrap();
    a1.swarm_contact(4655, [7; 32]).await.unwrap();
    let mut a2 = login(&burrow, "alice").await;
    a2.swarm_advertise(
        vec![AdvertEntry::new([2; 32], 10, "x.bin", "text/plain")],
        0,
    )
    .await
    .unwrap();

    let mut bob = login(&burrow, "bob").await;
    let list = bob.swarm_find([2; 32]).await.unwrap();
    assert_eq!(list.sources.len(), 2);
    assert_eq!(
        list.sources.iter().filter(|s| s.endpoint.is_some()).count(),
        1,
        "only the session that registered a contact exposes an endpoint"
    );

    // Close the contact-holding session: its advert AND endpoint vanish;
    // the second session's contactless advert remains.
    a1.close().await;
    let mut settled = false;
    for _ in 0..100 {
        let list = bob.swarm_find([2; 32]).await.unwrap();
        if list.sources.len() == 1 && list.sources[0].endpoint.is_none() {
            settled = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(settled, "contact card died with its session");

    burrow.shutdown().await;
}
