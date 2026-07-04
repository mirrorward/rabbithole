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

/// A client that presents a portable identity key in its handshake is surfaced
/// in the who-list with that key, while a handle-only client shows `None` — the
/// server half of verified-key People de-dup, end to end over a real Burrow.
#[tokio::test]
async fn who_list_carries_the_handshake_pubkey() {
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

    // "keyed" connects presenting a portable identity key.
    let mut keyed = Client::connect_with_identity(&url, None, None, "e2e", "0", Some([42; 32]))
        .await
        .unwrap();
    keyed.auth_password("keyed", "pw-pw-pw").await.unwrap();
    keyed.expect_welcome().await.unwrap();

    // "bare" connects the ordinary way (no key).
    let mut bare = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    bare.auth_password("bare", "pw-pw-pw").await.unwrap();
    bare.expect_welcome().await.unwrap();

    // The who-list echoes each session's key (or None).
    let who = keyed.who().await.unwrap();
    let keyed_row = who.iter().find(|u| u.screen_name == "keyed").unwrap();
    let bare_row = who.iter().find(|u| u.screen_name == "bare").unwrap();
    assert_eq!(keyed_row.pubkey, Some([42; 32]), "keyed session carries its handshake key");
    assert_eq!(bare_row.pubkey, None, "handle-only session has no key");

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
    assert_eq!(sources.len(), 3, "the coordinator returned all three dialable peers");
    let ticket = bob.swarm_ticket(root).await.unwrap();

    let dest = work.path().join("fetched.bin");
    let report =
        rabbithole_swarm::fetch_swarm_resumable(&sources, &ticket.token, root, body.len() as u64, &dest)
            .await
            .unwrap();
    assert_eq!(report.bytes, body.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), body, "multi-source bytes verified");
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
