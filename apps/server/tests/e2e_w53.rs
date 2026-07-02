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
