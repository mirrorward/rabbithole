//! Wave 13 end-to-end tests: token-bucket rate limiting across surfaces.
//!
//! Each test boots its own burrow with **tight** limits set through config
//! overrides (never by sleeping), so the outcomes are deterministic: refills
//! are slow enough (per-minute) that a test never races a refill, and bursts
//! are small enough to exhaust in a handful of requests.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

fn base_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Limited Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn ws_client(burrow: &Burrow) -> Client {
    Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .expect("client connects")
}

/// A chat flood hits the per-account `msg` budget: the send is refused with
/// `RateLimited`, but the session stays alive and keeps serving requests.
#[tokio::test]
async fn chat_flood_is_limited_but_session_survives() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        ratelimit_msg_per_sec: 1,
        ratelimit_msg_burst: 2,
        ..base_config(&dir.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();

    let mut alice = ws_client(&burrow).await;
    alice.auth_guest(Some("Alice".into())).await.unwrap();
    alice.expect_welcome().await.unwrap();

    // The burst passes; the flood is then refused with RateLimited. The
    // refill is 1/sec, so a tight loop of 10 cannot outrun the budget.
    let mut allowed = 0u32;
    let mut limited = false;
    for i in 0..10 {
        match alice.chat_send("lobby", &format!("flood {i}")).await {
            Ok(()) => allowed += 1,
            Err(ClientError::Refused(ErrorCode::RateLimited)) => {
                limited = true;
                break;
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert!(allowed >= 2, "the burst was allowed (got {allowed})");
    assert!(limited, "the flood was rate limited");

    // The session survived the refusal: other requests still answer.
    let who = alice.who().await.expect("session still alive");
    assert!(who.iter().any(|u| u.screen_name == "Alice (guest)"));

    burrow.shutdown().await;
}

/// One NNTP `AUTHINFO` exchange: USER (expect 381), then PASS. Returns the
/// PASS status line, or `None` if the server closed the connection.
async fn authinfo_attempt(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    pass: &str,
) -> Option<String> {
    write.write_all(b"AUTHINFO USER alice\r\n").await.ok()?;
    let mut line = String::new();
    if reader.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    assert!(line.starts_with("381"), "USER expects 381: {line:?}");
    write
        .write_all(format!("AUTHINFO PASS {pass}\r\n").as_bytes())
        .await
        .ok()?;
    line.clear();
    if reader.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    Some(line)
}

/// Hammering logins on a legacy surface (NNTP `AUTHINFO`) drains the per-IP
/// `auth` budget: refused attempts answer 481 and the connection is closed
/// once the bucket is empty.
#[tokio::test]
async fn nntp_login_hammer_is_limited_and_closed() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        nntp_enabled: true,
        nntp_addr: "127.0.0.1:0".parse().unwrap(),
        nntp_auth_require_tls: false, // plaintext AUTHINFO under test
        ratelimit_auth_per_min: 1,    // refill far slower than the test runs
        ratelimit_auth_burst: 2,
        ..base_config(&dir.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "correct-horse", Role::User)
        .await
        .unwrap();
    let addr = burrow.nntp_addr.expect("nntp enabled");

    let sock = TcpStream::connect(addr).await.unwrap();
    let (rd, mut wr) = sock.into_split();
    let mut reader = BufReader::new(rd);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("200"), "greeting: {line:?}");

    // Two failures fit the burst; both answer 481 and keep the session.
    for _ in 0..2 {
        let status = authinfo_attempt(&mut reader, &mut wr, "wrong")
            .await
            .expect("attempt answered");
        assert!(status.starts_with("481"), "rejected: {status:?}");
    }
    // The third failure exhausts the budget: 481, then the server hangs up.
    let status = authinfo_attempt(&mut reader, &mut wr, "wrong")
        .await
        .expect("limited attempt still answered");
    assert!(status.starts_with("481"), "rejected: {status:?}");
    line.clear();
    let n = reader.read_line(&mut line).await.unwrap_or(0);
    assert_eq!(n, 0, "connection closed after the auth budget ran out");

    burrow.shutdown().await;
}

/// The per-IP `conn` budget at the accept loop: excess connections are
/// dropped on the floor (accepted, then closed without a greeting).
#[tokio::test]
async fn conn_budget_drops_excess_connections() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        nntp_enabled: true,
        nntp_addr: "127.0.0.1:0".parse().unwrap(),
        ratelimit_conn_per_min: 1, // refill far slower than the test runs
        ratelimit_conn_burst: 2,
        ..base_config(&dir.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    let addr = burrow.nntp_addr.expect("nntp enabled");

    // The burst admits two connections (each greets)…
    for _ in 0..2 {
        let sock = TcpStream::connect(addr).await.unwrap();
        let mut reader = BufReader::new(sock);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("200"), "greeting: {line:?}");
    }
    // …the third is dropped before any greeting.
    let sock = TcpStream::connect(addr).await.unwrap();
    let mut reader = BufReader::new(sock);
    let mut line = String::new();
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_line(&mut line),
    )
    .await
    .expect("dropped promptly")
    .unwrap_or(0);
    assert_eq!(n, 0, "over-budget connection dropped, got {line:?}");

    burrow.shutdown().await;
}

/// `ratelimit_enabled=false` switches everything off: the same tiny budgets
/// impose no limits anywhere.
#[tokio::test]
async fn disabled_ratelimit_imposes_no_limits() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        nntp_enabled: true,
        nntp_addr: "127.0.0.1:0".parse().unwrap(),
        nntp_auth_require_tls: false, // plaintext AUTHINFO under test
        ratelimit_enabled: false,
        ratelimit_conn_per_min: 1,
        ratelimit_conn_burst: 1,
        ratelimit_auth_per_min: 1,
        ratelimit_auth_burst: 1,
        ratelimit_msg_per_sec: 1,
        ratelimit_msg_burst: 1,
        ..base_config(&dir.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    let addr = burrow.nntp_addr.expect("nntp enabled");

    // Auth hammering: every failure answers 481 and the connection stays up.
    let sock = TcpStream::connect(addr).await.unwrap();
    let (rd, mut wr) = sock.into_split();
    let mut reader = BufReader::new(rd);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("200"), "greeting: {line:?}");
    for _ in 0..5 {
        let status = authinfo_attempt(&mut reader, &mut wr, "wrong")
            .await
            .expect("attempt answered");
        assert!(status.starts_with("481"), "rejected: {status:?}");
    }
    wr.write_all(b"DATE\r\n").await.unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("111"), "session survived: {line:?}");

    // Chat flooding: every send is accepted.
    let mut alice = ws_client(&burrow).await;
    alice.auth_guest(Some("Alice".into())).await.unwrap();
    alice.expect_welcome().await.unwrap();
    for i in 0..5 {
        alice
            .chat_send("lobby", &format!("unlimited {i}"))
            .await
            .expect("no limit while disabled");
    }

    burrow.shutdown().await;
}
