//! RH-29: real door children with durable usage and a bounded telnet transport.
//! The duplex transport makes backpressure deterministic; the existing W6
//! suite separately covers TCP login, negotiation and the menu-to-door path.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime},
};

use burrow::{doors::run_door, Burrow, Shared};
use rabbithole_legacy_doors::{DoorDef, DropFile, IoMode, NodeRange};
use rabbithole_legacy_telnet::TelnetStream;
use rabbithole_server_core::{AuthedUser, Role, ServerConfig};
use rabbithole_store_server::{doors::DoorUsageRepo, repo::AuditRepo};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    task::JoinHandle,
};

#[test]
fn daily_door_helper() {
    let Ok(id) = std::env::var("RABBITHOLE_DOOR_ID") else {
        return;
    };
    use std::io::{BufRead, Write};
    let mut out = std::io::stdout();
    out.write_all(b"DAILY-READY\r\n").unwrap();
    out.flush().unwrap();
    if id == "flood" {
        loop {
            out.write_all(&[b'x'; 8192]).unwrap();
            out.flush().unwrap();
        }
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    std::process::exit(0);
}

fn door(id: &str) -> DoorDef {
    DoorDef {
        id: id.into(),
        title: id.into(),
        command: vec![
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "daily_door_helper".into(),
            "--exact".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ],
        working_dir: None,
        dropfile: DropFile::Door32Sys,
        io_mode: IoMode::Socket,
        nodes: NodeRange::any(),
        daily_limit_mins: Some(1),
    }
}

fn config(path: &Path, doors: Vec<DoorDef>, session_max: u64) -> ServerConfig {
    ServerConfig {
        data_dir: path.to_path_buf(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        doors_enabled: true,
        doors_max_nodes: 4,
        doors_session_max_secs: session_max,
        doors,
        ..ServerConfig::default()
    }
}

fn day() -> i64 {
    (SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 86_400) as i64
}

async fn user(shared: &Arc<Shared>) -> AuthedUser {
    shared
        .auth
        .login_password("alice", "doors-fixture-password", None)
        .await
        .unwrap()
}

async fn create_user(shared: &Arc<Shared>) -> AuthedUser {
    shared
        .auth
        .create_account("alice", "doors-fixture-password", Role::User)
        .await
        .unwrap();
    user(shared).await
}

async fn charge(shared: &Arc<Shared>, account: i64, door: &str, spent: u64) {
    let usage = DoorUsageRepo(&shared.pool);
    let reservation = usage
        .reserve(account, door, day(), 60_000, spent)
        .await
        .unwrap()
        .unwrap();
    usage.settle(reservation.id, spent).await.unwrap();
}

type Run = JoinHandle<std::io::Result<()>>;

fn launch(
    shared: Arc<Shared>,
    authed: AuthedUser,
    id: &str,
    capacity: usize,
) -> (DuplexStream, Run) {
    let (caller, server) = tokio::io::duplex(capacity);
    let id = id.to_string();
    let run = tokio::spawn(async move {
        let mut telnet = TelnetStream::new(server);
        run_door(&mut telnet, &shared, &authed, &id).await
    });
    (caller, run)
}

async fn expect(caller: &mut DuplexStream, needle: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut seen = Vec::new();
        while !seen.windows(needle.len()).any(|part| part == needle) {
            let mut bytes = [0; 4096];
            let n = caller.read(&mut bytes).await.unwrap();
            assert!(
                n > 0,
                "missing {:?} in {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&seen)
            );
            seen.extend_from_slice(&bytes[..n]);
        }
        seen
    })
    .await
    .expect("bounded door output")
}

async fn joined(run: Run) {
    tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn repeated_reconnected_and_restarted_runs_share_the_account_allowance() {
    let work = tempfile::tempdir().unwrap();
    let cfg = config(work.path(), vec![door("daily")], 1);
    let burrow = Burrow::start(cfg.clone()).await.unwrap();
    let alice = create_user(&burrow.shared).await;
    charge(&burrow.shared, alice.account.id, "daily", 58_000).await;

    let (mut first, run) = launch(burrow.shared.clone(), alice.clone(), "daily", 4096);
    expect(&mut first, b"DAILY-READY").await;
    first.write_all(b"exit\r\n").await.unwrap();
    expect(&mut first, b"daily ended.").await;
    joined(run).await;
    let charged = DoorUsageRepo(&burrow.shared.pool)
        .charged_ms(alice.account.id, "daily", day())
        .await
        .unwrap();
    assert!(
        (58_001..60_000).contains(&charged),
        "short run must charge elapsed and refund unused time: {charged}"
    );

    // New authentication and persona spelling preserve the original account.
    let mut reconnected = user(&burrow.shared).await;
    reconnected.persona.screen_name = "Different persona".into();
    for _ in 0..2 {
        let (mut caller, run) = launch(burrow.shared.clone(), reconnected.clone(), "daily", 4096);
        // The final fractional remainder may expire before child startup;
        // admission plus timeout is the contract, not a readiness message.
        expect(&mut caller, b"Time limit reached").await;
        joined(run).await;
    }
    assert_eq!(
        DoorUsageRepo(&burrow.shared.pool)
            .charged_ms(alice.account.id, "daily", day())
            .await
            .unwrap(),
        60_000
    );
    let (mut refused, run) = launch(burrow.shared.clone(), reconnected, "daily", 4096);
    expect(&mut refused, b"No daily time remains").await;
    joined(run).await;
    burrow.shutdown().await;

    let restarted = Burrow::start(cfg).await.unwrap();
    let alice = user(&restarted.shared).await;
    let (mut refused, run) = launch(restarted.shared.clone(), alice, "daily", 4096);
    expect(&mut refused, b"No daily time remains").await;
    joined(run).await;
    restarted.shutdown().await;
}

#[tokio::test]
async fn an_active_run_reserves_time_against_concurrent_connections() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(config(work.path(), vec![door("daily")], 0))
        .await
        .unwrap();
    let alice = create_user(&burrow.shared).await;
    let (mut first, first_run) = launch(burrow.shared.clone(), alice.clone(), "daily", 4096);
    expect(&mut first, b"DAILY-READY").await;
    let (mut second, second_run) = launch(burrow.shared.clone(), alice.clone(), "daily", 4096);
    expect(&mut second, b"No daily time remains").await;
    joined(second_run).await;
    first.write_all(b"exit\r\n").await.unwrap();
    expect(&mut first, b"daily ended.").await;
    joined(first_run).await;
    let (mut retry, retry_run) = launch(burrow.shared.clone(), alice, "daily", 4096);
    expect(&mut retry, b"DAILY-READY").await;
    retry.write_all(b"exit\r\n").await.unwrap();
    expect(&mut retry, b"daily ended.").await;
    joined(retry_run).await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn failed_spawns_and_hangups_release_unused_prepaid_time() {
    let work = tempfile::tempdir().unwrap();
    let mut broken = door("broken");
    broken.command = vec![work
        .path()
        .join("nonexistent-door-program")
        .to_string_lossy()
        .into_owned()];
    let burrow = Burrow::start(config(work.path(), vec![broken, door("daily")], 0))
        .await
        .unwrap();
    let alice = create_user(&burrow.shared).await;
    let (mut caller, run) = launch(burrow.shared.clone(), alice.clone(), "broken", 4096);
    expect(&mut caller, b"The door failed to start").await;
    joined(run).await;
    assert_eq!(
        DoorUsageRepo(&burrow.shared.pool)
            .charged_ms(alice.account.id, "broken", day())
            .await
            .unwrap(),
        0
    );

    let (mut caller, run) = launch(burrow.shared.clone(), alice.clone(), "daily", 4096);
    expect(&mut caller, b"DAILY-READY").await;
    drop(caller);
    joined(run).await;
    let charged = DoorUsageRepo(&burrow.shared.pool)
        .charged_ms(alice.account.id, "daily", day())
        .await
        .unwrap();
    assert!((1..60_000).contains(&charged));
    let (mut retry, run) = launch(burrow.shared.clone(), alice, "daily", 4096);
    expect(&mut retry, b"DAILY-READY").await;
    retry.write_all(b"exit\r\n").await.unwrap();
    expect(&mut retry, b"daily ended.").await;
    joined(run).await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn deadlines_kill_children_even_when_banner_or_stdout_writes_are_blocked() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(config(work.path(), vec![door("banner"), door("flood")], 1))
        .await
        .unwrap();
    let alice = create_user(&burrow.shared).await;
    for (id, capacity) in [("banner", 1), ("flood", 4096)] {
        let (mut caller, run) = launch(burrow.shared.clone(), alice.clone(), id, capacity);
        if id == "flood" {
            expect(&mut caller, b"DAILY-READY").await;
        }
        // Stop consuming bytes. The audit is recorded only after drive has
        // killed/reaped the child and settled usage, before the blocked farewell.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if AuditRepo(&burrow.shared.pool)
                    .recent(50)
                    .await
                    .unwrap()
                    .iter()
                    .any(|row| {
                        row.action == "door-run"
                            && row.detail.starts_with(&format!("{id} node="))
                            && row.detail.contains("outcome=timed-out")
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("deadline must not wait for the telnet writer");
        let charged = DoorUsageRepo(&burrow.shared.pool)
            .charged_ms(alice.account.id, id, day())
            .await
            .unwrap();
        assert_eq!(
            charged, 1000,
            "global session maximum also bounds the charged daily usage"
        );
        expect(&mut caller, b"Time limit reached").await;
        // A one-byte transport also backpressures the remainder of the
        // farewell. Drain it before joining the task that writes those bytes.
        tokio::time::timeout(Duration::from_secs(10), caller.read_to_end(&mut Vec::new()))
            .await
            .unwrap()
            .unwrap();
        joined(run).await;
    }
    burrow.shutdown().await;
}
