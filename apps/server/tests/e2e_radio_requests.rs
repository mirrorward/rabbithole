//! RH-96: real sessions watch one queue, projected for their own account.
use std::time::Duration;

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::radio::*;
use rabbithole_proto::{ErrorCode, FrameKind};
use rabbithole_server_core::{Role, ServerConfig, ServerEvent};

fn tracks() -> Vec<rabbithole_radio::Track> {
    (1..=6)
        .map(|n| {
            rabbithole_radio::Track::new(
                rabbithole_radio::TrackId(n),
                format!("song-{n}.mp3"),
                "The Lagomorphs",
                180_000,
                rabbithole_radio::BlobId([n as u8; 32]),
            )
        })
        .collect()
}

async fn connect(url: &str, login: &str) -> Client {
    let mut client = Client::connect(url, None, None, "radio-watch-test", "0")
        .await
        .unwrap();
    client.auth_password(login, "pw-pw-pw-pw").await.unwrap();
    client.expect_welcome().await.unwrap();
    client
}

async fn watch(client: &mut Client, station: Option<&str>) {
    let reply: RadioRequestsWatching = client
        .request(&RadioRequestsWatch::new(station.map(str::to_string)))
        .await
        .unwrap();
    assert_eq!(reply.station.as_deref(), station);
}

async fn queue(client: &mut Client) -> RadioRequests {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let push = client.next_push().await.unwrap().expect("connected");
            if let Some(Ok(view)) = push.decode::<RadioRequests>() {
                assert_eq!(push.kind, FrameKind::Push);
                assert_eq!(
                    push.id.0, 0,
                    "ephemeral queue snapshots are never replay-stamped"
                );
                return view;
            }
        }
    })
    .await
    .expect("watched queue changed without another request")
}

async fn no_queue(client: &mut Client) {
    assert!(tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            let push = client.next_push().await.unwrap().expect("connected");
            assert!(
                push.decode::<RadioRequests>().is_none(),
                "an unwatched station leaked a queue"
            );
        }
    })
    .await
    .is_err());
}

#[tokio::test]
async fn watched_requests_and_votes_are_live_personalized_and_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Queue Warren".into(),
        data_dir: dir.path().join("srv"),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for (login, role) in [
        ("alice", Role::User),
        ("bob", Role::User),
        ("mo", Role::Moderator),
    ] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw-pw", role)
            .await
            .unwrap();
    }
    for slug in ["jukebox", "quiet"] {
        let rotation = tracks();
        burrow.shared.radio.install_program(
            slug,
            slug,
            "music",
            rotation.clone(),
            burrow::radio::sound_of_tracks(&rotation),
        );
    }
    let url = format!("ws://{}", burrow.ws_addr);
    let mut alice = connect(&url, "alice").await;
    let mut bob = connect(&url, "bob").await;
    let mut mo = connect(&url, "mo").await;
    watch(&mut alice, Some("jukebox")).await;
    watch(&mut bob, Some("jukebox")).await;
    watch(&mut mo, Some("quiet")).await;
    let initial: RadioRequests = bob
        .request(&RadioRequestsRequest::new("jukebox"))
        .await
        .unwrap();
    assert!(initial.queue.is_empty());

    let asked: RadioRequests = alice
        .request(&RadioRequest::new("jukebox", 4))
        .await
        .unwrap();
    assert!(asked.queue[0].mine);
    let hers = queue(&mut alice).await;
    let his = queue(&mut bob).await;
    assert!(hers.queue[0].mine);
    assert!(
        !his.queue[0].mine,
        "another listener's vote state stays private"
    );
    assert_eq!(his.queue[0].votes, 1);
    assert_eq!(his.queue[0].id, 4);
    no_queue(&mut mo).await;
    let voted: RadioRequests = bob
        .request(&RadioRequestVote::new("jukebox", 4))
        .await
        .unwrap();
    assert_eq!(voted.queue[0].votes, 2);
    assert!(voted.queue[0].mine);
    assert_eq!(queue(&mut alice).await.queue[0].votes, 2);
    assert!(queue(&mut bob).await.queue[0].mine);
    assert_eq!(
        burrow.shared.radio.now_playing("jukebox").unwrap().title,
        "song-1.mp3"
    );

    // Watch is a read operation. A guest receives their own view but cannot vote.
    let mut guest = Client::connect(&url, None, None, "guest", "0")
        .await
        .unwrap();
    guest.auth_guest(Some("visitor".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    watch(&mut guest, Some("jukebox")).await;
    let denied = guest
        .request::<_, RadioRequests>(&RadioRequestVote::new("jukebox", 4))
        .await;
    assert!(matches!(
        denied,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Actual moderation requests invalidate watchers without another song or read.
    use rabbithole_proto::admin::{
        subject_kind, DenyHashAdd, DenyHashRemove, QuarantineClear, QuarantineSet,
    };
    mo.request_ack(&QuarantineSet::new(
        subject_kind::FILE,
        vec![4; 32],
        "review",
    ))
    .await
    .unwrap();
    assert!(queue(&mut alice).await.queue.is_empty());
    assert!(queue(&mut bob).await.queue.is_empty());
    assert!(queue(&mut guest).await.queue.is_empty());
    // The other watched station receives its own empty queue, never jukebox's.
    assert_eq!(queue(&mut mo).await.station, "quiet");
    mo.request_ack(&QuarantineClear::new(subject_kind::FILE, vec![4; 32]))
        .await
        .unwrap();
    assert_eq!(queue(&mut alice).await.queue[0].votes, 2);
    assert!(!queue(&mut guest).await.queue[0].mine);
    let _ = queue(&mut bob).await;
    let _ = queue(&mut mo).await;
    mo.request_ack(&DenyHashAdd::new([4; 32], "held"))
        .await
        .unwrap();
    assert!(queue(&mut alice).await.queue.is_empty());
    mo.request_ack(&DenyHashRemove::new([4; 32])).await.unwrap();
    assert_eq!(queue(&mut alice).await.queue[0].id, 4);
    let _ = queue(&mut bob).await;
    let _ = queue(&mut bob).await;

    // The local operator's supported control path invalidates the same view.
    for (command, hidden) in [("hash-deny", true), ("hash-allow", false)] {
        let result = burrow::ctl::handle(
            &burrow.shared,
            &serde_json::json!({
                "cmd": command, "hash": hex::encode([4; 32]), "reason": "ctl review"
            }),
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(queue(&mut alice).await.queue.is_empty(), hidden);
        assert_eq!(queue(&mut bob).await.queue.is_empty(), hidden);
    }

    // Replacing a watch is bounded to one station; None stops it entirely.
    watch(&mut bob, Some("quiet")).await;
    let _: RadioRequests = alice
        .request(&RadioRequest::new("jukebox", 3))
        .await
        .unwrap();
    let _ = queue(&mut alice).await;
    no_queue(&mut bob).await;
    watch(&mut bob, None).await;
    let _: RadioRequests = alice.request(&RadioRequest::new("quiet", 2)).await.unwrap();
    no_queue(&mut bob).await;

    // A station going away retires the session's watch. A later queued event
    // cannot resurrect the old queue (actual sign-off is also covered in w114).
    burrow.shared.bus.publish(ServerEvent::RadioOff {
        station: "jukebox".into(),
    });
    loop {
        if alice
            .next_push()
            .await
            .unwrap()
            .unwrap()
            .decode::<RadioOff>()
            .is_some()
        {
            break;
        }
    }
    burrow
        .shared
        .bus
        .publish(ServerEvent::RadioRequestsChanged {
            station: "jukebox".into(),
        });
    no_queue(&mut alice).await;
    // A fresh authenticated connection receives no inherited watch/replay.
    drop(alice);
    let mut alice = connect(&url, "alice").await;
    burrow
        .shared
        .bus
        .publish(ServerEvent::RadioRequestsChanged {
            station: "jukebox".into(),
        });
    no_queue(&mut alice).await;
    watch(&mut alice, Some("jukebox")).await;
    let snapshot: RadioRequests = alice
        .request(&RadioRequestsRequest::new("jukebox"))
        .await
        .unwrap();
    assert_eq!(snapshot.queue.len(), 2);
    assert!(snapshot.queue.iter().all(|track| track.mine));
    // The same live snapshot path follows consumption, rather than leaving
    // the just-started track waiting until this listener asks again.
    burrow.shared.radio.advance("jukebox", 1);
    let now = burrow.shared.radio.now_playing("jukebox").unwrap();
    burrow.shared.bus.publish(ServerEvent::RadioNowPlaying {
        station: "jukebox".into(),
        title: now.title,
        artist: now.artist,
        dj: now.dj,
        listeners: 0,
    });
    let remaining = queue(&mut alice).await;
    assert_eq!(remaining.queue.len(), 1);
    assert!(remaining.queue.iter().all(|track| track.id != 4));
    let bad = alice
        .request::<_, RadioRequestsWatching>(&RadioRequestsWatch::new(Some("missing".into())))
        .await;
    assert!(matches!(
        bad,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));
    burrow
        .shared
        .bus
        .publish(ServerEvent::RadioRequestsChanged {
            station: "jukebox".into(),
        });
    no_queue(&mut alice).await;
    burrow.shutdown().await;
}
