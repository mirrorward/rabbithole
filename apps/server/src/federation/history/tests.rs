use super::super::{handle_ihave, ingest_fed_event, FedEvent};
use super::*;
use rabbithole_identity::keys::IdentityKey;
use rabbithole_net::{NetError, PeerInfo, TransportKind};
use rabbithole_proto::Frame;
use rabbithole_server_core::{
    dedup::SeenKey,
    events::{mint, EventBody},
    ServerConfig, ServerEvent,
};
use rabbithole_store_server::repo4::{FollowupRow, FollowupsRepo, PostRow, PostsRepo};
use std::collections::HashSet;

fn position(n: u32) -> HistoryCursor {
    let mut event_id = [0; 32];
    event_id[..4].copy_from_slice(&n.to_be_bytes());
    HistoryCursor { event_id, kind: 0 }
}

#[test]
fn peer_budget_is_shared_and_links_keep_independent_fair_cursors() {
    let state = HistoryState::default();
    let now = Instant::now();
    let first = state.register([1; 32], now).unwrap();
    let second = state.register([1; 32], now).unwrap();
    let unrelated = state.register([2; 32], now).unwrap();
    for link in [&first, &second, &unrelated] {
        link.active(true);
    }
    assert!(
        second.begin(now, 5).is_none(),
        "first active link owns first turn"
    );
    first.begin(now, 5).unwrap().finish(Some(position(17)));
    assert!(first.begin(now + Duration::from_secs(5), 5).is_none());
    assert!(second.begin(now + Duration::from_secs(4), 5).is_none());
    let pass = second.begin(now + Duration::from_secs(5), 5).unwrap();
    assert_eq!(pass.cursor, None, "different interests start independently");
    pass.finish(Some(position(21)));
    let pass = first.begin(now + Duration::from_secs(10), 5).unwrap();
    assert_eq!(pass.cursor, Some(position(17)));
    assert!(
        second.begin(now + Duration::from_secs(100), 5).is_none(),
        "no concurrent peer pass"
    );
    drop(pass); // cancelled work keeps its old cursor and consumes one cadence
    unrelated.begin(now, 5).unwrap().finish(None);
    drop(second);
    let pass = first.begin(now + Duration::from_secs(15), 5).unwrap();
    assert_eq!(pass.cursor, Some(position(17)));
    pass.finish(None);
    drop(first);
    let again = state
        .register([1; 32], now + Duration::from_secs(15))
        .unwrap();
    again.active(true);
    assert!(
        again.begin(now + Duration::from_secs(19), 5).is_none(),
        "reconnect cannot reset peer budget"
    );
    assert!(again.begin(now + Duration::from_secs(20), 5).is_some());
}

#[test]
fn cadence_clamps_disabled_catchup_and_no_burst_after_idle() {
    for (configured, interval) in [(0, 60), (1, 5), (5, 5), (60, 60), (u64::MAX, 3600)] {
        let state = HistoryState::default();
        let now = Instant::now();
        let link = state.register([1; 32], now).unwrap();
        link.active(true);
        link.begin(now, configured).unwrap().finish(None);
        assert!(link
            .begin(now - Duration::from_secs(1), configured)
            .is_none());
        assert!(link
            .begin(
                now + Duration::from_secs(interval) - Duration::from_nanos(1),
                configured
            )
            .is_none());
        link.begin(now + Duration::from_secs(interval), configured)
            .unwrap()
            .finish(None);
        let later = now + Duration::from_secs(interval * 100);
        link.begin(later, configured).unwrap().finish(None);
        assert!(
            link.begin(later, configured).is_none(),
            "idle time never accrues catch-up passes"
        );
    }
}

#[test]
fn registrations_are_bounded_and_expired_disconnected_slots_are_reusable() {
    let state = HistoryState::default();
    let now = Instant::now();
    let mut links = Vec::new();
    for _ in 0..LINK_LIMIT {
        links.push(state.register([1; 32], now).unwrap());
    }
    assert!(state.register([1; 32], now).is_none());
    links.pop();
    assert!(state.register([1; 32], now).is_some());
    drop(links);
    let mut active = Vec::new();
    for n in 0..PEER_LIMIT {
        active.push(state.register(position(n as u32).event_id, now).unwrap());
    }
    assert!(state.register([255; 32], now).is_none());
    let first = active.remove(0);
    first.active(true);
    first.begin(now, 3600).unwrap().finish(None);
    drop(first);
    assert!(state
        .register([255; 32], now + Duration::from_secs(3599))
        .is_none());
    assert!(state
        .register([255; 32], now + Duration::from_secs(3600))
        .is_some());
}

struct Wire {
    sent: Vec<Frame>,
    peer: PeerInfo,
}
impl Wire {
    fn new() -> Self {
        Self {
            sent: Vec::new(),
            peer: PeerInfo {
                remote_addr: "127.0.0.1:12345".parse().unwrap(),
                transport: TransportKind::Quic,
            },
        }
    }
    fn offers(&mut self) -> Vec<IHave> {
        self.sent
            .drain(..)
            .map(|frame| {
                assert_eq!(frame.message_type, MT_IHAVE);
                postcard::from_bytes(&frame.payload.0).unwrap()
            })
            .collect()
    }
}
#[async_trait::async_trait]
impl Connection for Wire {
    async fn send(&mut self, frame: Frame) -> Result<(), NetError> {
        self.sent.push(frame);
        Ok(())
    }
    async fn recv(&mut self) -> Result<Option<Frame>, NetError> {
        Ok(None)
    }
    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
    async fn close(&mut self) {}
}
async fn server(path: &std::path::Path) -> crate::Burrow {
    let b = crate::Burrow::start(ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: path.to_owned(),
        federation_board_subscribe: vec!["*".into()],
        federation_history_reoffer_secs: 5,
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    b.shared
        .peers
        .seed_approved([91; 32], "Remote", Some("remote".into()));
    b
}
fn edge(shared: &Shared, now: Instant) -> FloodEdge {
    let mut edge = FloodEdge::new([91; 32], "remote".into());
    edge.history = shared.fed_history.register(edge.peer_key, now);
    edge.interest = Interest::All;
    edge
}
async fn row(shared: &Shared, n: u32, board: &str) -> [u8; 32] {
    let id = position(n).event_id;
    PostsRepo(&shared.pool)
        .insert(&PostRow {
            event_id: id,
            board_slug: board.into(),
            root_id: Some(id),
            parent_id: None,
            author: "author@origin".into(),
            subject: "subject".into(),
            body: "body".into(),
            mime: "text/plain".into(),
            created_at: i64::MAX - i64::from(n),
            edited: false,
            tombstoned: false,
            event_blob: Vec::new(),
        })
        .await
        .unwrap();
    id
}
async fn followup(shared: &Shared, id: [u8; 32], target: [u8; 32], board: &str) {
    FollowupsRepo(&shared.pool)
        .insert(&FollowupRow {
            event_id: id,
            target_id: target,
            root_id: target,
            board_slug: board.into(),
            kind: 1,
            origin: "origin".into(),
            applied: true,
            created_at: 0,
            event_blob: Vec::new(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn bounded_pages_recover_past_filtered_prefix_and_seen_bloom_including_followups() {
    let work = tempfile::tempdir().unwrap();
    let b = server(work.path()).await;
    b.shared
        .boards
        .create_board("private", "Private", "", 2, None, 0)
        .await
        .unwrap();
    b.shared
        .boards
        .create_board("category", "Category", "", 0, None, 0)
        .await
        .unwrap();
    for n in 0..300 {
        row(&b.shared, n, "private").await;
    }
    row(&b.shared, 300, "category").await;
    let names: Vec<_> = (0..10).map(|i| format!("board{i}")).collect();
    let mut expected = HashSet::new();
    for (i, name) in names.iter().enumerate() {
        b.shared
            .boards
            .create_board(name, name, "", 2, None, 0)
            .await
            .unwrap();
        for j in 0..32 {
            expected.insert(row(&b.shared, 301 + (i * 32 + j) as u32, name).await);
        }
    }
    b.shared
        .config
        .update(|c| c.federation_board_subscribe = names.clone());
    let held = position(301).event_id;
    b.shared
        .moderation
        .quarantine_set(
            rabbithole_proto::admin::subject_kind::POST,
            &held,
            "review",
            "operator",
        )
        .await
        .unwrap();
    expected.remove(&held);
    let edit = position(1000).event_id;
    followup(&b.shared, edit, position(302).event_id, "board0").await;
    expected.insert(edit);
    followup(&b.shared, position(1001).event_id, held, "board0").await;
    let now = Instant::now();
    let mut edge = edge(&b.shared, now);
    for id in &expected {
        edge.seen.insert(id);
    }
    let mut wire = Wire::new();
    offer_at(&mut wire, &b.shared, &mut edge, false, now)
        .await
        .unwrap();
    assert!(
        wire.sent.is_empty(),
        "256 filtered entries do not stall the next page"
    );
    let mut recovered = HashSet::new();
    let mut repeated = false;
    for tick in 1..12 {
        offer_at(
            &mut wire,
            &b.shared,
            &mut edge,
            true,
            now + Duration::from_secs(tick * 5),
        )
        .await
        .unwrap();
        let offers = wire.offers();
        assert!(offers.len() <= OFFER_FRAMES);
        assert!(
            offers
                .iter()
                .map(|offer| offer.event_ids.len())
                .sum::<usize>()
                <= OFFER_IDS
        );
        for offer in offers {
            assert!(names.contains(&offer.board));
            for id in offer.event_ids {
                assert!(
                    expected.contains(&id),
                    "no private, non-postable or quarantined IDs"
                );
                repeated |= !recovered.insert(id);
            }
        }
    }
    assert_eq!(recovered, expected);
    assert!(
        repeated,
        "wrap re-offers history even after its Bloom was populated"
    );
    b.shutdown().await;
}

#[tokio::test]
async fn disabled_periodic_keeps_catchup_bounded_and_live_opt_out_is_immediate() {
    let work = tempfile::tempdir().unwrap();
    let b = server(work.path()).await;
    b.shared
        .boards
        .create_board("shared", "Shared", "", 2, None, 0)
        .await
        .unwrap();
    row(&b.shared, 1, "shared").await;
    b.shared
        .config
        .update(|c| c.federation_history_reoffer_secs = 0);
    let now = Instant::now();
    let mut edge = edge(&b.shared, now);
    let mut wire = Wire::new();
    offer_at(&mut wire, &b.shared, &mut edge, true, now)
        .await
        .unwrap();
    assert!(wire.sent.is_empty());
    offer_at(&mut wire, &b.shared, &mut edge, false, now)
        .await
        .unwrap();
    assert_eq!(wire.offers().len(), 1);
    for tick in 0..60 {
        offer_at(
            &mut wire,
            &b.shared,
            &mut edge,
            false,
            now + Duration::from_secs(tick),
        )
        .await
        .unwrap();
        assert!(
            wire.sent.is_empty(),
            "repeated subscriptions cannot reset the cooldown"
        );
    }
    offer_at(
        &mut wire,
        &b.shared,
        &mut edge,
        false,
        now + Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(wire.offers().len(), 1);
    b.shared.config.update(|c| {
        c.federation_history_reoffer_secs = 5;
        c.federation_board_subscribe.clear();
    });
    offer_at(
        &mut wire,
        &b.shared,
        &mut edge,
        true,
        now + Duration::from_secs(70),
    )
    .await
    .unwrap();
    assert!(wire.sent.is_empty());
    b.shared
        .config
        .update(|c| c.federation_board_subscribe = vec!["*".into()]);
    edge.interest = Interest::Boards(HashSet::new());
    offer_at(
        &mut wire,
        &b.shared,
        &mut edge,
        true,
        now + Duration::from_secs(75),
    )
    .await
    .unwrap();
    assert!(wire.sent.is_empty());
    b.shutdown().await;
}

#[tokio::test]
async fn approval_is_rechecked_after_the_history_query_await() {
    use std::{future::Future, task::Poll};
    let work = tempfile::tempdir().unwrap();
    let b = server(work.path()).await;
    b.shared
        .boards
        .create_board("shared", "Shared", "", 2, None, 0)
        .await
        .unwrap();
    row(&b.shared, 1, "shared").await;
    let now = Instant::now();
    let mut edge = edge(&b.shared, now);
    let mut wire = Wire::new();
    let mut held = Vec::new();
    for _ in 0..b.shared.pool.options().get_max_connections() {
        held.push(b.shared.pool.acquire().await.unwrap());
    }
    {
        let pending = offer_at(&mut wire, &b.shared, &mut edge, true, now);
        tokio::pin!(pending);
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        b.shared
            .catalogs
            .revoke_peer(&b.shared.peers, &[91; 32])
            .unwrap();
        drop(held);
        assert!(pending.await.is_err());
    }
    assert!(wire.sent.is_empty());
    b.shutdown().await;
}

#[tokio::test]
async fn durable_followup_dedup_and_failed_ingest_allow_recovery_without_duplicates() {
    let work = tempfile::tempdir().unwrap();
    let b = server(work.path()).await;
    let author = IdentityKey::from_seed(&[7; 32]);
    let origin = IdentityKey::from_seed(&b.shared.server_signing_seed);
    let signed = mint(
        "author@origin",
        &author,
        &b.shared.origin_name(),
        &origin,
        1,
        EventBody::Post {
            board: "restored".into(),
            root: None,
            parent: None,
            subject: "subject".into(),
            body: "body".into(),
            mime: "text/plain".into(),
        },
    );
    let fe = FedEvent {
        id: signed.id,
        bytes: postcard::to_allocvec(&signed).unwrap(),
    };
    let now = Instant::now();
    let mut edge = edge(&b.shared, now);
    // A board deleted between delivery validation and the ingest cannot poison
    // the dedup gate for 30 days. Restoring its destination permits retry.
    assert!(ingest_fed_event(
        &b.shared,
        &mut edge,
        "restored",
        &fe,
        &b.shared.server_key,
        0
    )
    .await
    .is_err());
    assert!(!b.shared.dedup.seen(&SeenKey::Event(fe.id)));
    b.shared
        .boards
        .create_board("restored", "Restored", "", 2, None, 0)
        .await
        .unwrap();
    let mut events = b.shared.bus.subscribe();
    ingest_fed_event(
        &b.shared,
        &mut edge,
        "restored",
        &fe,
        &b.shared.server_key,
        0,
    )
    .await
    .unwrap();
    assert!(matches!(events.try_recv().unwrap(), ServerEvent::BoardPost { id, .. } if id == fe.id));
    ingest_fed_event(
        &b.shared,
        &mut edge,
        "restored",
        &fe,
        &b.shared.server_key,
        0,
    )
    .await
    .unwrap();
    assert!(events.try_recv().is_err());
    let (_, edit) = b
        .shared
        .boards
        .edit(
            fe.id,
            "author@origin",
            &[7; 32],
            "edited",
            "edited",
            "text/plain",
            2,
        )
        .await
        .unwrap();
    assert!(
        !b.shared.dedup.seen(&SeenKey::Event(edit)),
        "freshly minted local followup has no memory entry"
    );
    let mut wire = Wire::new();
    handle_ihave(
        &mut wire,
        &b.shared,
        &mut edge,
        &fed_frame(
            FrameKind::Push,
            MT_IHAVE,
            &IHave {
                board: "restored".into(),
                event_ids: vec![fe.id, edit],
            },
        ),
    )
    .await
    .unwrap();
    assert!(
        wire.sent.is_empty(),
        "both durable event tables suppress needless recovery pulls"
    );
    b.shutdown().await;
}

#[tokio::test]
async fn distinct_boards_share_eight_frame_budget_and_storage_errors_back_off() {
    let work = tempfile::tempdir().unwrap();
    let b = server(work.path()).await;
    for n in 0..10 {
        let name = format!("board{n}");
        b.shared
            .boards
            .create_board(&name, &name, "", 2, None, 0)
            .await
            .unwrap();
        row(&b.shared, n, &name).await;
    }
    let now = Instant::now();
    let mut edge = edge(&b.shared, now);
    let mut wire = Wire::new();
    offer_at(&mut wire, &b.shared, &mut edge, true, now)
        .await
        .unwrap();
    assert_eq!(wire.offers().len(), 8);
    offer_at(
        &mut wire,
        &b.shared,
        &mut edge,
        true,
        now + Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(
        wire.offers().len(),
        2,
        "ninth board is the first next candidate, not skipped"
    );
    b.shared.pool.close().await;
    offer_at(
        &mut wire,
        &b.shared,
        &mut edge,
        true,
        now + Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(
        wire.sent.is_empty(),
        "local storage error does not terminate the connection"
    );
    let registration = edge.history.as_ref().unwrap();
    assert!(registration
        .begin(now + Duration::from_secs(14), 5)
        .is_none());
    assert!(
        registration
            .begin(now + Duration::from_secs(15), 5)
            .is_some(),
        "failed pass released its busy guard"
    );
    b.shutdown().await;
}
