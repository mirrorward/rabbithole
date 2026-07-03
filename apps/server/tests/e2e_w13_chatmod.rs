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
    ChatMessage, RoomMute, RoomMuted, RoomSlowMode, RoomSlowModeChanged, RoomUnmute,
};
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
        loop {
            let txn = tokio::time::timeout(Duration::from_secs(5), self.read_txn())
                .await
                .expect("timed out waiting for transaction");
            if txn.header.type_ == type_ {
                return txn;
            }
        }
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

    pest.close().await;
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
    let pest_account = rabbithole_store_server::repo::AccountsRepo(&burrow.shared.pool)
        .by_login("pest")
        .await
        .unwrap()
        .expect("pest exists")
        .id;
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut pest = Telnet::connect(addr).await;
    pest.expect(b"login: ").await;
    pest.send("pest").await;
    pest.expect(b"password: ").await;
    pest.send("pw-pw-pw").await;
    pest.expect(b"Command: ").await;
    pest.send("c").await;
    pest.expect(b"--- Chat: lobby ---").await;
    pest.send("hello there").await;
    pest.expect(b"<pest> hello there").await;

    // Muted (service-side, as a moderator would): the next line refuses.
    let now = rabbithole_server_core::ratelimit::now_ms();
    burrow
        .shared
        .chat
        .mute(LOBBY, 0, true, pest_account, None, now)
        .unwrap();
    pest.send("can you hear me").await;
    pest.expect(b"(you are muted in this room)").await;

    // Unmute restores the flow.
    burrow
        .shared
        .chat
        .unmute(LOBBY, 0, true, pest_account, now)
        .unwrap();
    pest.send("im back").await;
    pest.expect(b"<pest> im back").await;

    burrow.shutdown().await;
}
