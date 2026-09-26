//! Wave 13 end-to-end tests: chat moderation hardening — room mutes and
//! slow-mode — across the native, Hotline, and telnet surfaces.
//!
//! Deterministic patterns throughout: state changes are observed through
//! acked requests and pushes, the only wall-clock dependency is the 1-second
//! timed mute (checked with a bounded poll, never a blind sleep).

use std::time::Duration;

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_legacy_hotline::constants::{field, transaction};
use rabbithole_legacy_hotline::{Field, Handshake, HandshakeReply, Transaction, TransactionHeader};
use rabbithole_proto::chat::{
    ChatMessage, RoomCreate, RoomInfoReply, RoomInvite, RoomJoin, RoomKick, RoomModeration,
    RoomModerationRequest, RoomMute, RoomMuted, RoomSlowMode, RoomSlowModeChanged, RoomUnmute,
};
use rabbithole_proto::presence::PresenceState;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig, LOBBY};
use rabbithole_store_server::repo::AuditRepo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Muted Warren".into(),
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

async fn start(cfg: ServerConfig) -> Burrow {
    let burrow = Burrow::start(cfg).await.unwrap();
    for (login, role) in [
        ("alice", Role::User),
        ("pest", Role::User),
        ("mo", Role::Moderator),
    ] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw", role)
            .await
            .unwrap();
    }
    burrow
}

async fn wait_push_named<F: Fn(&rabbithole_proto::Frame) -> bool>(
    label: &str,
    c: &mut Client,
    pred: F,
) -> rabbithole_proto::Frame {
    for _ in 0..20 {
        let frame = tokio::time::timeout(Duration::from_secs(5), c.next_push())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for push: {label}"))
            .unwrap()
            .expect("push");
        if pred(&frame) {
            return frame;
        }
    }
    panic!("expected push not seen: {label}");
}

/// Wait (bounded) for an action to land in the audit log — audit writes are
/// fire-and-forget, so assertions poll instead of racing the spawn.
async fn wait_audited(burrow: &Burrow, action: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows = AuditRepo(&burrow.shared.pool).recent(100).await.unwrap();
        if rows.iter().any(|r| r.action == action) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "audit action never recorded: {action}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A moderator mutes a lobby member: sends are refused with the distinct
/// `Muted` code while pushes keep flowing to the muted member; unmute
/// restores the voice. Non-moderators can't mute, and both actions are
/// audited.
#[tokio::test]
async fn native_mute_refuses_sends_and_unmute_restores() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(test_config(dir.path())).await;
    let mut mo = login(&burrow, "mo").await;
    let mut alice = login(&burrow, "alice").await;
    let mut pest = login(&burrow, "pest").await;

    // A plain user may not mute anyone.
    assert!(matches!(
        alice.request_ack(&RoomMute::new(LOBBY, "pest", None)).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    // An unknown target is an honest NotFound.
    assert!(matches!(
        mo.request_ack(&RoomMute::new(LOBBY, "nobody-here", None))
            .await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    // The moderator mutes pest (permanent); room members see the push.
    mo.request_ack(&RoomMute::new(LOBBY, "pest", None))
        .await
        .unwrap();
    let frame = wait_push_named("alice-sees-mute", &mut alice, |f| {
        f.decode::<RoomMuted>().is_some()
    })
    .await;
    let push = frame.decode::<RoomMuted>().unwrap().unwrap();
    assert!(push.muted);
    assert_eq!(push.screen_name, "pest");
    assert_eq!(push.duration_secs, None);

    // Pest's sends are refused with the distinct code…
    assert!(matches!(
        pest.chat_send(LOBBY, "let me speak").await,
        Err(ClientError::Refused(ErrorCode::Muted))
    ));
    // …but pest still *receives* room events (muted, not deaf, and the
    // refused line never reached anyone).
    mo.chat_send(LOBBY, "order in the warren").await.unwrap();
    let frame = wait_push_named("pest-still-receives", &mut pest, |f| {
        f.decode::<ChatMessage>().is_some()
    })
    .await;
    let line = frame.decode::<ChatMessage>().unwrap().unwrap();
    assert_eq!(line.text, "order in the warren");

    // Unmute restores the voice; a second unmute finds nothing.
    mo.request_ack(&RoomUnmute::new(LOBBY, "pest"))
        .await
        .unwrap();
    pest.chat_send(LOBBY, "reformed").await.unwrap();
    assert!(matches!(
        mo.request_ack(&RoomUnmute::new(LOBBY, "pest")).await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    wait_audited(&burrow, "room-mute").await;
    wait_audited(&burrow, "room-unmute").await;
    burrow.shutdown().await;
}

/// A room says how it is being kept: its pace to everybody in it, who is
/// muted to its keepers, and to a muted person only that they are. A
/// private room says nothing to anybody outside it.
#[tokio::test]
async fn a_room_says_how_it_is_kept_and_to_whom() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(test_config(dir.path())).await;
    let mut mo = login(&burrow, "mo").await;
    let mut alice = login(&burrow, "alice").await;
    let mut pest = login(&burrow, "pest").await;

    mo.request_ack(&RoomMute::new(LOBBY, "pest", Some(600)))
        .await
        .unwrap();
    mo.request_ack(&RoomSlowMode::new(LOBBY, 30)).await.unwrap();
    let ask = || RoomModerationRequest::new(LOBBY);

    // The moderator: may keep it, sees the mute and roughly how long is left.
    let kept: RoomModeration = mo.request(&ask()).await.unwrap();
    assert!(kept.may_moderate);
    assert_eq!(kept.slow_mode_secs, 30);
    assert_eq!(kept.muted.len(), 1);
    assert_eq!(kept.muted[0].screen_name, "pest");
    let left = kept.muted[0].remaining_secs.expect("a timed mute");
    assert!((590..=600).contains(&left), "{left}");
    for who in ["mo", "alice", "pest"] {
        assert!(kept.members.iter().any(|m| m == who), "{who} is in it");
    }

    // Pest is told they are muted; alice is told nothing about it.
    let theirs: RoomModeration = pest.request(&ask()).await.unwrap();
    assert!(!theirs.may_moderate);
    assert_eq!(theirs.muted.len(), 1);
    let hers: RoomModeration = alice.request(&ask()).await.unwrap();
    assert!(hers.muted.is_empty(), "a mute is not everybody's business");
    assert_eq!(hers.slow_mode_secs, 30, "the pace is");
    assert!(hers.members.is_empty(), "nor is the room's roster");

    // A private room: its maker keeps it; nobody outside it hears of it.
    let _: RoomInfoReply = alice.request(&RoomCreate::new("den", true)).await.unwrap();
    let own: RoomModeration = alice
        .request(&RoomModerationRequest::new("den"))
        .await
        .unwrap();
    assert!(own.may_moderate, "its maker keeps it");

    // Somebody invisible is not shown to a keeper who is not a moderator,
    // the same as on the who-list; a moderator sees them.
    let _: RoomInfoReply = alice
        .request(&RoomCreate::new("hall", false))
        .await
        .unwrap();
    let _: RoomInfoReply = pest.request(&RoomJoin::new("hall")).await.unwrap();
    pest.presence_set(PresenceState::Invisible, None)
        .await
        .unwrap();
    let hall = || RoomModerationRequest::new("hall");
    let by_maker: RoomModeration = alice.request(&hall()).await.unwrap();
    assert!(by_maker.may_moderate);
    assert!(
        !by_maker.members.iter().any(|m| m == "pest"),
        "{:?}",
        by_maker.members
    );
    let by_mo: RoomModeration = mo.request(&hall()).await.unwrap();
    assert!(by_mo.members.iter().any(|m| m == "pest"));
    let outside = pest
        .request::<_, RoomModeration>(&RoomModerationRequest::new("den"))
        .await;
    assert!(
        matches!(outside, Err(ClientError::Refused(_))),
        "{outside:?}"
    );
    burrow.shutdown().await;
}

/// A 1-second timed mute expires on its own (lazy expiry): the refusal
/// clears within a bounded poll, no unmute needed.
#[tokio::test]
async fn timed_mute_expires() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(test_config(dir.path())).await;
    let mut mo = login(&burrow, "mo").await;
    let mut pest = login(&burrow, "pest").await;

    mo.request_ack(&RoomMute::new(LOBBY, "pest", Some(1)))
        .await
        .unwrap();
    assert!(matches!(
        pest.chat_send(LOBBY, "too soon").await,
        Err(ClientError::Refused(ErrorCode::Muted))
    ));

    // Bounded poll, paced well under the per-account msg refill so the
    // global limiter never interferes with the outcome.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match pest.chat_send(LOBBY, "free yet?").await {
            Ok(()) => break,
            Err(ClientError::Refused(ErrorCode::Muted)) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "timed mute never expired"
                );
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            Err(e) => panic!("unexpected refusal while waiting for expiry: {e}"),
        }
    }

    burrow.shutdown().await;
}

/// Slow-mode: the second send inside the window is refused with a
/// retry-after carried in the error code; moderators are exempt; only the
/// creator/moderators may set it; 0 turns it off; the interval is capped.
#[tokio::test]
async fn slow_mode_spacing_retry_after_and_exemptions() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(test_config(dir.path())).await;
    let mut mo = login(&burrow, "mo").await;
    let mut alice = login(&burrow, "alice").await;

    // Only moderators (or a room's creator) may set slow-mode.
    assert!(matches!(
        alice.request_ack(&RoomSlowMode::new(LOBBY, 30)).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // The moderator turns it on; members get the push with the applied value.
    mo.request_ack(&RoomSlowMode::new(LOBBY, 3600))
        .await
        .unwrap();
    let frame = wait_push_named("alice-sees-slow-mode", &mut alice, |f| {
        f.decode::<RoomSlowModeChanged>().is_some()
    })
    .await;
    let push = frame.decode::<RoomSlowModeChanged>().unwrap().unwrap();
    assert_eq!(push.seconds, 3600);
    assert_eq!(push.by, "mo");

    // First line is free; the second inside the window is refused with a
    // retry-after the client can surface.
    alice.chat_send(LOBBY, "measured words").await.unwrap();
    match alice.chat_send(LOBBY, "too fast").await {
        Err(ClientError::Refused(ErrorCode::SlowMode { retry_after_secs })) => {
            assert!(
                (1..=3600).contains(&retry_after_secs),
                "retry-after in range, got {retry_after_secs}"
            );
        }
        other => panic!("expected a slow-mode refusal, got {other:?}"),
    }

    // Moderators are exempt from the interval.
    mo.chat_send(LOBBY, "rapid").await.unwrap();
    mo.chat_send(LOBBY, "fire").await.unwrap();

    // Oversized asks clamp to the 3600 cap (observed via the service).
    mo.request_ack(&RoomSlowMode::new(LOBBY, 90_000))
        .await
        .unwrap();
    assert_eq!(burrow.shared.chat.slow_mode_secs(LOBBY), 3600);

    // 0 turns it off (clearing the per-member clocks): alice flows again.
    mo.request_ack(&RoomSlowMode::new(LOBBY, 0)).await.unwrap();
    wait_push_named("alice-sees-slow-mode-off", &mut alice, |f| {
        f.decode::<RoomSlowModeChanged>()
            .and_then(Result::ok)
            .is_some_and(|p| p.seconds == 0)
    })
    .await;
    alice.chat_send(LOBBY, "free").await.unwrap();
    alice.chat_send(LOBBY, "flow").await.unwrap();

    wait_audited(&burrow, "room-slow-mode").await;
    burrow.shutdown().await;
}

// ---------------------------------------------------------------------------
// Hotline: a scripted classic client (the e2e_w76 pattern, trimmed).

struct Hotline {
    stream: TcpStream,
    next_id: u32,
}

impl Hotline {
    async fn connect(addr: std::net::SocketAddr) -> Hotline {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(&Handshake::hotl().encode()).await.unwrap();
        let mut reply = [0u8; HandshakeReply::LEN];
        stream.read_exact(&mut reply).await.unwrap();
        assert!(HandshakeReply::decode(&reply).unwrap().is_ok());
        Hotline { stream, next_id: 1 }
    }

    async fn send(&mut self, type_: u16, fields: Vec<Field>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let txn = Transaction::request(type_, id, fields);
        self.stream.write_all(&txn.encode()).await.unwrap();
        id
    }

    async fn read_txn(&mut self) -> Transaction {
        let mut hdr = [0u8; TransactionHeader::LEN];
        self.stream.read_exact(&mut hdr).await.unwrap();
        let header = TransactionHeader::decode(&hdr).unwrap();
        let mut buf = hdr.to_vec();
        buf.resize(TransactionHeader::LEN + header.data_size as usize, 0);
        self.stream
            .read_exact(&mut buf[TransactionHeader::LEN..])
            .await
            .unwrap();
        Transaction::decode(&buf).unwrap()
    }

    async fn read_until(&mut self, type_: u16) -> Transaction {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let txn = self.read_txn().await;
                if txn.header.type_ == type_ {
                    return txn;
                }
            }
        })
        .await
        .expect("timed out waiting for transaction")
    }

    async fn notice(&mut self, chat_id: Option<u32>, text: &str) {
        let txn = self.read_until(transaction::CHAT_MSG).await;
        assert_eq!(txn_text(&txn, field::CHAT_TEXT), format!("\r({text})"));
        assert_eq!(txn_int(&txn, field::CHAT_ID), chat_id);
    }

    async fn chat_line(&mut self, chat_id: Option<u32>, text: &str) {
        let txn = self.read_until(transaction::CHAT_MSG).await;
        assert_eq!(txn_text(&txn, field::CHAT_TEXT), text);
        assert_eq!(txn_int(&txn, field::CHAT_ID), chat_id);
    }

    async fn login(&mut self, user: &str, pass: &str, name: &str) -> Transaction {
        let obfuscate = |s: &str| s.bytes().map(|b| !b).collect::<Vec<u8>>();
        let fields = vec![
            Field::new(field::LOGIN, obfuscate(user)),
            Field::new(field::PASSWORD, obfuscate(pass)),
            Field::text(field::USER_NAME, name),
            Field::int(field::USER_ICON_ID, 200),
        ];
        let id = self.send(transaction::LOGIN, fields).await;
        let reply = self.read_until(transaction::LOGIN).await;
        assert_eq!(reply.header.id, id);
        reply
    }

    async fn close(mut self) {
        let _ = self.stream.shutdown().await;
    }
}

fn txn_text(txn: &Transaction, id: u16) -> String {
    txn.fields
        .iter()
        .find(|f| f.id == id)
        .map(|f| String::from_utf8_lossy(&f.data).into_owned())
        .unwrap_or_default()
}

fn txn_int(txn: &Transaction, id: u16) -> Option<u32> {
    txn.fields
        .iter()
        .find(|f| f.id == id)
        .and_then(|f| rabbithole_legacy_hotline::read_int(&f.data).ok())
}

/// The Hotline surface observes a mute set natively: the classic CHAT_SEND
/// notify gets a private refusal line back in the chat window instead of a
/// broadcast.
#[tokio::test]
async fn hotline_surface_observes_mute() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        hotline_enabled: true,
        hotline_addr: "127.0.0.1:0".parse().unwrap(),
        ..test_config(dir.path())
    };
    let burrow = start(cfg).await;
    let mut mo = login(&burrow, "mo").await;

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut pest = Hotline::connect(addr).await;
    assert_eq!(pest.login("pest", "pw-pw-pw", "pest").await.header.error, 0);

    // Before the mute, pest's lobby line comes back through the shared bus.
    pest.send(
        transaction::CHAT_SEND,
        vec![Field::new(field::CHAT_TEXT, b"anyone home?".to_vec())],
    )
    .await;
    let echo = pest.read_until(transaction::CHAT_MSG).await;
    assert!(txn_text(&echo, field::CHAT_TEXT).contains("anyone home?"));

    // A native moderator mutes pest in the lobby.
    mo.request_ack(&RoomMute::new(LOBBY, "pest", None))
        .await
        .unwrap();
    pest.notice(None, "pest was muted in lobby until unmuted.")
        .await;

    // Pest's next line is refused: a private CHAT_MSG carries the refusal
    // text (ChatSend is a notify — there is no reply to carry an error).
    pest.send(
        transaction::CHAT_SEND,
        vec![Field::new(field::CHAT_TEXT, b"silenced?".to_vec())],
    )
    .await;
    let refusal = pest.read_until(transaction::CHAT_MSG).await;
    assert!(
        txn_text(&refusal, field::CHAT_TEXT).contains("muted"),
        "refusal names the mute: {:?}",
        txn_text(&refusal, field::CHAT_TEXT)
    );

    // The refused line never reached the room: the next lobby line anyone
    // sees is the moderator's probe (bus order is delivery order).
    mo.chat_send(LOBBY, "probe").await.unwrap();
    let next = pest.read_until(transaction::CHAT_MSG).await;
    let text = txn_text(&next, field::CHAT_TEXT);
    assert!(
        text.contains("probe") && !text.contains("silenced?"),
        "muted line must not broadcast, got {text:?}"
    );

    mo.request_ack(&RoomUnmute::new(LOBBY, "pest"))
        .await
        .unwrap();
    pest.notice(None, "pest was unmuted in lobby.").await;
    pest.send(
        transaction::CHAT_SEND,
        vec![Field::text(field::CHAT_TEXT, "voice restored")],
    )
    .await;
    assert!(txn_text(
        &pest.read_until(transaction::CHAT_MSG).await,
        field::CHAT_TEXT
    )
    .contains("voice restored"));

    mo.request_ack(&RoomSlowMode::new(LOBBY, 30)).await.unwrap();
    pest.notice(None, "Slow mode in lobby: one message every 30 seconds.")
        .await;
    pest.send(
        transaction::CHAT_SEND,
        vec![Field::text(field::CHAT_TEXT, "first paced line")],
    )
    .await;
    assert!(txn_text(
        &pest.read_until(transaction::CHAT_MSG).await,
        field::CHAT_TEXT
    )
    .contains("first paced line"));
    pest.send(
        transaction::CHAT_SEND,
        vec![Field::text(field::CHAT_TEXT, "too soon")],
    )
    .await;
    assert!(txn_text(
        &pest.read_until(transaction::CHAT_MSG).await,
        field::CHAT_TEXT
    )
    .contains("slow mode"));
    mo.request_ack(&RoomSlowMode::new(LOBBY, 0)).await.unwrap();
    pest.notice(None, "Slow mode in lobby is off.").await;
    pest.send(
        transaction::CHAT_SEND,
        vec![Field::text(field::CHAT_TEXT, "pace restored")],
    )
    .await;
    assert!(txn_text(
        &pest.read_until(transaction::CHAT_MSG).await,
        field::CHAT_TEXT
    )
    .contains("pace restored"));

    pest.close().await;
    burrow.shutdown().await;
}

/// Native private-room moderation is tagged for the correct classic chat
/// window, and only current members receive it. Lobby chat is an ordered
/// bus sentinel for outsiders and the kicked client, so absence needs no sleep.
#[tokio::test]
async fn hotline_private_notices_reach_members_only() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        hotline_enabled: true,
        hotline_addr: "127.0.0.1:0".parse().unwrap(),
        ..test_config(dir.path())
    };
    let burrow = start(cfg).await;
    let mut mo = login(&burrow, "mo").await;
    let _: RoomInfoReply = mo.request(&RoomCreate::new("den", true)).await.unwrap();
    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut pest = Hotline::connect(addr).await;
    assert_eq!(pest.login("pest", "pw-pw-pw", "pest").await.header.error, 0);
    // A round trip through chat proves each session is subscribed before
    // any private event is published.
    pest.send(
        transaction::CHAT_SEND,
        vec![Field::text(field::CHAT_TEXT, "ready")],
    )
    .await;
    pest.chat_line(None, "\rpest:  ready").await;
    let mut outsider = Hotline::connect(addr).await;
    assert_eq!(
        outsider
            .login("alice", "pw-pw-pw", "alice")
            .await
            .header
            .error,
        0
    );
    outsider
        .send(
            transaction::CHAT_SEND,
            vec![Field::text(field::CHAT_TEXT, "outsider ready")],
        )
        .await;
    outsider.chat_line(None, "\ralice:  outsider ready").await;
    pest.chat_line(None, "\ralice:  outsider ready").await;

    mo.request_ack(&RoomInvite::new("den", "pest"))
        .await
        .unwrap();
    let invited = pest.read_until(transaction::INVITE_TO_CHAT).await;
    let chat_id = txn_int(&invited, field::CHAT_ID).expect("private chat id");
    let join_id = pest
        .send(
            transaction::JOIN_CHAT,
            vec![Field::int(field::CHAT_ID, chat_id)],
        )
        .await;
    let joined = pest.read_until(transaction::JOIN_CHAT).await;
    assert_eq!(joined.header.id, join_id);
    assert_eq!(joined.header.error, 0);

    mo.request_ack(&RoomMute::new("den", "pest", Some(60)))
        .await
        .unwrap();
    pest.notice(Some(chat_id), "pest was muted in den for 60 seconds.")
        .await;
    mo.request_ack(&RoomUnmute::new("den", "pest"))
        .await
        .unwrap();
    pest.notice(Some(chat_id), "pest was unmuted in den.").await;
    mo.request_ack(&RoomSlowMode::new("den", 1)).await.unwrap();
    pest.notice(
        Some(chat_id),
        "Slow mode in den: one message every 1 second.",
    )
    .await;
    mo.request_ack(&RoomSlowMode::new("den", 0)).await.unwrap();
    pest.notice(Some(chat_id), "Slow mode in den is off.").await;

    mo.chat_send(LOBBY, "outsider barrier").await.unwrap();
    outsider.chat_line(None, "\rmo:  outsider barrier").await;
    pest.chat_line(None, "\rmo:  outsider barrier").await;

    mo.request_ack(&RoomKick::new("den", "pest", true))
        .await
        .unwrap();
    let kicked = pest.read_until(transaction::NOTIFY_CHAT_DELETE_USER).await;
    assert_eq!(txn_int(&kicked, field::CHAT_ID), Some(chat_id));
    mo.request_ack(&RoomMute::new("den", "pest", None))
        .await
        .unwrap();
    mo.request_ack(&RoomUnmute::new("den", "pest"))
        .await
        .unwrap();
    mo.request_ack(&RoomSlowMode::new("den", 30)).await.unwrap();
    mo.request_ack(&RoomSlowMode::new("den", 0)).await.unwrap();
    mo.chat_send(LOBBY, "kicked barrier").await.unwrap();
    pest.chat_line(None, "\rmo:  kicked barrier").await;
    outsider.chat_line(None, "\rmo:  kicked barrier").await;

    pest.close().await;
    outsider.close().await;
    burrow.shutdown().await;
}

// ---------------------------------------------------------------------------
// Telnet: the marker-driven scripted client (the e2e_w6 pattern, trimmed).

struct Telnet {
    sock: TcpStream,
    buf: Vec<u8>,
    pos: usize,
}

impl Telnet {
    async fn connect(addr: std::net::SocketAddr) -> Telnet {
        Telnet {
            sock: TcpStream::connect(addr).await.unwrap(),
            buf: Vec::new(),
            pos: 0,
        }
    }

    async fn send(&mut self, line: &str) {
        self.sock
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }

    async fn login(&mut self, user: &str) {
        self.expect(b"login: ").await;
        self.send(user).await;
        self.expect(b"password: ").await;
        self.send("pw-pw-pw").await;
        self.expect(b"Command: ").await;
    }

    async fn notice(&mut self, text: &str) {
        // TelnetStream translates the notice's newline to the wire CRLF.
        self.expect(format!("({text})\r\n").as_bytes()).await;
    }

    async fn expect_without(&mut self, needle: &[u8], forbidden: &[&[u8]]) {
        let start = self.pos;
        self.expect(needle).await;
        let observed = &self.buf[start..self.pos];
        for text in forbidden {
            assert!(
                !observed.windows(text.len()).any(|window| window == *text),
                "unexpected notice {:?} before sentinel: {:?}",
                String::from_utf8_lossy(text),
                String::from_utf8_lossy(observed)
            );
        }
    }

    async fn expect(&mut self, needle: &[u8]) {
        let find = |hay: &[u8]| {
            (hay.len() >= needle.len())
                .then(|| hay.windows(needle.len()).position(|w| w == needle))
                .flatten()
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(at) = find(&self.buf[self.pos..]) {
                    self.pos += at + needle.len();
                    return;
                }
                let mut chunk = [0u8; 4096];
                let n = self.sock.read(&mut chunk).await.expect("telnet read");
                assert!(
                    n > 0,
                    "EOF waiting for {:?}",
                    String::from_utf8_lossy(needle)
                );
                self.buf.extend_from_slice(&chunk[..n]);
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {:?}; unconsumed: {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.buf[self.pos..])
            )
        })
    }
}

/// The telnet lobby observes a mute: the typed line is answered with the
/// refusal line instead of echoing through the room, and an unmute restores
/// the flow.
#[tokio::test]
async fn telnet_surface_observes_mute() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        ..test_config(dir.path())
    };
    let burrow = start(cfg).await;
    let mut mo = login(&burrow, "mo").await;
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut pest = Telnet::connect(addr).await;
    pest.login("pest").await;
    pest.send("c").await;
    pest.expect(b"--- Chat: lobby ---").await;
    pest.send("hello there").await;
    pest.expect(b"<pest> hello there").await;

    // A real native moderator's action reaches the idle terminal before the
    // person tries to speak, then the existing send gate still refuses.
    mo.request_ack(&RoomMute::new(LOBBY, "pest", Some(60)))
        .await
        .unwrap();
    pest.notice("pest was muted in lobby for 60 seconds.").await;
    pest.send("can you hear me").await;
    pest.expect(b"(you are muted in this room)").await;

    // Unmute restores the flow.
    mo.request_ack(&RoomUnmute::new(LOBBY, "pest"))
        .await
        .unwrap();
    pest.notice("pest was unmuted in lobby.").await;
    pest.send("im back").await;
    pest.expect(b"<pest> im back").await;

    mo.request_ack(&RoomSlowMode::new(LOBBY, 30)).await.unwrap();
    pest.notice("Slow mode in lobby: one message every 30 seconds.")
        .await;
    pest.send("first paced line").await;
    pest.expect(b"<pest> first paced line").await;
    pest.send("too soon").await;
    pest.expect(b"(slow mode is on: wait ").await;
    mo.request_ack(&RoomSlowMode::new(LOBBY, 0)).await.unwrap();
    pest.notice("Slow mode in lobby is off.").await;
    pest.send("pace restored").await;
    pest.expect(b"<pest> pace restored").await;

    burrow.shutdown().await;
}

/// Telnet moderation follows the active chat screen and current membership.
/// Both a lobby outsider and a kicked session are checked through ordered
/// output markers, not an arbitrary period without receiving data.
#[tokio::test]
async fn telnet_notices_follow_active_room_and_current_membership() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        // Private rooms are deliberately absent from public keyword lookup;
        // an operator alias reaches the room's normal invite-checked join.
        keywords: [("den".into(), "room:den".into())].into(),
        ..test_config(dir.path())
    };
    let burrow = start(cfg).await;
    let mut mo = login(&burrow, "mo").await;
    let _: RoomInfoReply = mo.request(&RoomCreate::new("den", true)).await.unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");
    let mut pest = Telnet::connect(addr).await;
    pest.login("pest").await;
    let mut outsider = Telnet::connect(addr).await;
    outsider.login("alice").await;
    mo.request_ack(&RoomInvite::new("den", "pest"))
        .await
        .unwrap();
    pest.send("/go den").await;
    pest.expect(b"--- Chat: den ---").await;
    pest.expect(b"(no recent chat)\r\n").await;
    outsider.send("c").await;
    outsider.expect(b"--- Chat: lobby ---").await;
    outsider.expect(b"(no recent chat)\r\n").await;

    // Pest remains a lobby member, but its active screen is the private den.
    mo.request_ack(&RoomMute::new(LOBBY, "alice", Some(60)))
        .await
        .unwrap();
    outsider
        .notice("alice was muted in lobby for 60 seconds.")
        .await;
    mo.request_ack(&RoomUnmute::new(LOBBY, "alice"))
        .await
        .unwrap();
    outsider.notice("alice was unmuted in lobby.").await;
    mo.request_ack(&RoomSlowMode::new(LOBBY, 1)).await.unwrap();
    outsider
        .notice("Slow mode in lobby: one message every 1 second.")
        .await;
    mo.request_ack(&RoomSlowMode::new(LOBBY, 0)).await.unwrap();
    outsider.notice("Slow mode in lobby is off.").await;
    mo.chat_send("den", "active room barrier").await.unwrap();
    pest.expect_without(
        b"<mo> active room barrier\r\n",
        &[b"alice was", b"Slow mode in lobby"],
    )
    .await;

    mo.request_ack(&RoomMute::new("den", "pest", Some(60)))
        .await
        .unwrap();
    pest.notice("pest was muted in den for 60 seconds.").await;
    mo.request_ack(&RoomUnmute::new("den", "pest"))
        .await
        .unwrap();
    pest.notice("pest was unmuted in den.").await;
    mo.request_ack(&RoomSlowMode::new("den", 1)).await.unwrap();
    pest.notice("Slow mode in den: one message every 1 second.")
        .await;
    mo.request_ack(&RoomSlowMode::new("den", 0)).await.unwrap();
    pest.notice("Slow mode in den is off.").await;
    mo.chat_send(LOBBY, "outsider barrier").await.unwrap();
    outsider
        .expect_without(
            b"<mo> outsider barrier\r\n",
            &[b"pest was", b"Slow mode in den"],
        )
        .await;

    mo.request_ack(&RoomKick::new("den", "pest", true))
        .await
        .unwrap();
    pest.expect(b"Command: ").await;
    mo.request_ack(&RoomMute::new("den", "pest", None))
        .await
        .unwrap();
    mo.request_ack(&RoomUnmute::new("den", "pest"))
        .await
        .unwrap();
    mo.request_ack(&RoomSlowMode::new("den", 30)).await.unwrap();
    mo.request_ack(&RoomSlowMode::new("den", 0)).await.unwrap();
    // A room kick returns to the menu; later room notices must not leak
    // through the abandoned chat screen while the session remains open.
    pest.send("q").await;
    pest.expect_without(b"Goodbye, pest!\r\n", &[b"pest was", b"Slow mode in den"])
        .await;

    burrow.shutdown().await;
}
