//! Wave 6 end-to-end tests: door games hosted on the telnet surface.
//!
//! The pure session-runner model (registry, node pool, FSM, bridge) is
//! unit-tested in `rabbithole-legacy-doors`; here we prove burrow actually
//! runs a door — spawns a process, writes its drop file, pumps bytes IAC-safe
//! both ways over a live telnet socket, refuses when nodes are exhausted,
//! kills on timeout, and stays off by default.
//!
//! ## The helper "doors"
//!
//! For a portable child process the doors spawn **this test binary itself**,
//! filtered to one of the `door_helper_*` "tests" below. The helpers are
//! inert under a normal `cargo test` run (they return immediately unless
//! `RABBITHOLE_DOOR_NODE` is present — an environment variable only the door
//! driver exports), so they double as the child's program when spawned as a
//! door and as trivially-passing tests otherwise.

use std::path::Path;
use std::time::Duration;

use burrow::Burrow;
use rabbithole_legacy_doors::{DoorDef, DropFile, IoMode, NodeRange};
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ---------------------------------------------------------------------------
// Helper "doors" (child-process modes of this very binary).

/// True when this process was launched *as a door child* by the driver.
fn spawned_as_door() -> bool {
    std::env::var("RABBITHOLE_DOOR_NODE").is_ok()
}

/// Door child: announce the node, emit a literal 0xFF (which must arrive
/// IAC-doubled at the telnet client), report the drop file's line count,
/// then echo one line read from stdin and exit cleanly.
#[test]
fn door_helper_echo() {
    if !spawned_as_door() {
        return; // running as a plain test: no-op
    }
    use std::io::{BufRead, Write};
    let node = std::env::var("RABBITHOLE_DOOR_NODE").unwrap();
    let drop_dir = std::env::var("RABBITHOLE_DOOR_DROPDIR").unwrap();
    let dropfile = std::fs::read_to_string(Path::new(&drop_dir).join("DOOR32.SYS")).unwrap();
    let mut out = std::io::stdout();
    write!(out, "DOOR-ECHO-START node={node}\r\n").unwrap();
    out.write_all(&[0xFF]).unwrap();
    write!(out, "\r\nDROPLINES={}\r\n", dropfile.lines().count()).unwrap();
    out.flush().unwrap();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    write!(out, "GOT {}\r\n", line.trim()).unwrap();
    out.flush().unwrap();
    std::process::exit(0);
}

/// Door child: announce, then hang forever — for node-exhaustion and
/// timeout-kill coverage. Only the driver's kill ends it.
#[test]
fn door_helper_hang() {
    if !spawned_as_door() {
        return;
    }
    use std::io::Write;
    let mut out = std::io::stdout();
    write!(out, "DOOR-HANG-START\r\n").unwrap();
    out.flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// A door definition that spawns this test binary in `helper` mode.
fn helper_door(id: &str, helper: &str, nodes: NodeRange) -> DoorDef {
    let exe = std::env::current_exe().unwrap();
    DoorDef {
        id: id.into(),
        title: format!("{id} (test door)"),
        command: vec![
            exe.to_string_lossy().into_owned(),
            helper.into(),
            "--exact".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ],
        working_dir: None,
        dropfile: DropFile::Door32Sys,
        io_mode: IoMode::Socket,
        nodes,
        daily_limit_mins: None,
    }
}

fn test_config(dir: &Path, doors: Vec<DoorDef>) -> ServerConfig {
    ServerConfig {
        name: "Door Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        doors_enabled: true,
        doors,
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

// ---------------------------------------------------------------------------
// A tiny deterministic telnet client: accumulate bytes, wait for markers.

struct Client {
    sock: TcpStream,
    buf: Vec<u8>,
    /// Bytes before `pos` were already consumed by an earlier `expect`.
    pos: usize,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Client {
        Client {
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

    /// Read until one of `needles` appears past the consumption point;
    /// return which one. Advances the point past the match — no blind
    /// sleeps, no double-matching stale output.
    async fn expect_any(&mut self, needles: &[&[u8]]) -> usize {
        let deadline = Duration::from_secs(30);
        tokio::time::timeout(deadline, async {
            loop {
                for (i, needle) in needles.iter().enumerate() {
                    if let Some(at) = find(&self.buf[self.pos..], needle) {
                        self.pos += at + needle.len();
                        return i;
                    }
                }
                let mut chunk = [0u8; 4096];
                let n = self.sock.read(&mut chunk).await.expect("telnet read");
                assert!(
                    n > 0,
                    "EOF while waiting for {:?}; unconsumed: {:?}",
                    lossy(needles),
                    String::from_utf8_lossy(&self.buf[self.pos..])
                );
                self.buf.extend_from_slice(&chunk[..n]);
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {:?}; unconsumed: {:?}",
                lossy(needles),
                String::from_utf8_lossy(&self.buf[self.pos..])
            )
        })
    }

    async fn expect(&mut self, needle: &[u8]) {
        self.expect_any(&[needle]).await;
    }

    /// Log in through the shell prompts and wait for the main-menu prompt.
    async fn login(&mut self, user: &str, pass: &str) {
        self.expect(b"login: ").await;
        self.send(user).await;
        self.expect(b"password: ").await;
        self.send(pass).await;
        self.expect(b"Command: ").await;
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn lossy(needles: &[&[u8]]) -> Vec<String> {
    needles
        .iter()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// The tests.

#[tokio::test]
async fn door_runs_and_output_reaches_the_client() {
    let work = tempfile::tempdir().unwrap();
    let cfg = test_config(
        &work.path().join("srv"),
        vec![helper_door("echo", "door_helper_echo", NodeRange::any())],
    );
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;

    // The door menu lists the installed door.
    c.send("doors").await;
    c.expect(b"echo (test door)").await;
    c.expect(b"Command: ").await;

    // Launch: the child's greeting proves spawn + node 1 + stdout→telnet.
    c.send("door echo").await;
    c.expect(b"DOOR-ECHO-START node=1").await;
    let after_start = c.pos;

    // DROPLINES=11 proves the DOOR32.SYS drop file was written (11 lines)
    // and read back by the child from the per-node drop dir.
    c.expect(b"DROPLINES=11").await;

    // The literal 0xFF the door emitted must arrive IAC-doubled: the only
    // 0xFF 0xFF pair on the wire (negotiation never doubles).
    assert!(
        find(&c.buf[after_start..], &[0xFF, 0xFF]).is_some(),
        "door's 0xFF byte was IAC-doubled on the telnet leg"
    );

    // Keystrokes reach the child's stdin; its echo proves the return path.
    c.send("ping").await;
    c.expect(b"GOT ping").await;

    // Clean exit lands back on the main menu.
    c.expect(b"ended").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye, alice!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn node_exhaustion_refuses_then_releases() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = test_config(
        &work.path().join("srv"),
        vec![
            helper_door("hang", "door_helper_hang", NodeRange::any()),
            helper_door("echo", "door_helper_echo", NodeRange::any()),
        ],
    );
    cfg.doors_max_nodes = 1; // one node for the whole board
    let burrow = Burrow::start(cfg).await.unwrap();
    for login in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let addr = burrow.telnet_addr.expect("telnet enabled");

    // Alice occupies the only node.
    let mut a = Client::connect(addr).await;
    a.login("alice", "pw-pw-pw").await;
    a.send("door hang").await;
    a.expect(b"DOOR-HANG-START").await;

    // Bob is refused: the pool has no free node.
    let mut b = Client::connect(addr).await;
    b.login("bob", "pw-pw-pw").await;
    b.send("door echo").await;
    b.expect(b"All door nodes are busy").await;
    b.expect(b"Command: ").await;

    // Alice hangs up; the driver kills her door and the lease releases the
    // node. Bob retries until the pool hands it to him (bounded, no blind
    // sleep: each retry waits on the server's actual answer).
    drop(a);
    let mut entered = false;
    for _ in 0..100 {
        b.send("door echo").await;
        let hit = b
            .expect_any(&[b"DOOR-ECHO-START", b"All door nodes are busy"])
            .await;
        if hit == 0 {
            entered = true;
            break;
        }
        b.expect(b"Command: ").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(entered, "released node was never re-allocated");
    b.send("ping").await;
    b.expect(b"GOT ping").await;
    b.expect(b"Command: ").await;
    b.send("q").await;
    b.expect(b"Goodbye, bob!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn doors_are_off_by_default() {
    assert!(
        !ServerConfig::default().doors_enabled,
        "doors must be opt-in"
    );

    let work = tempfile::tempdir().unwrap();
    // Doors *listed* but not enabled: the switch, not the list, governs.
    let mut cfg = test_config(
        &work.path().join("srv"),
        vec![helper_door("echo", "door_helper_echo", NodeRange::any())],
    );
    cfg.doors_enabled = false;
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.send("doors").await;
    c.expect(b"Doors are not enabled").await;
    c.expect(b"Command: ").await;
    c.send("door echo").await;
    c.expect(b"Doors are not enabled").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn timeout_kills_the_door() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = test_config(
        &work.path().join("srv"),
        vec![helper_door("hang", "door_helper_hang", NodeRange::any())],
    );
    cfg.doors_session_max_secs = 1; // tightest budget the config offers
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.send("door hang").await;
    c.expect(b"DOOR-HANG-START").await;
    // The driver kills (and reaps) the child before printing this, so seeing
    // it back on the menu proves TimedOut → kill, not just a message.
    c.expect(b"Time limit reached").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn guests_lack_the_door_capability() {
    let work = tempfile::tempdir().unwrap();
    let cfg = test_config(
        &work.path().join("srv"),
        vec![helper_door("echo", "door_helper_echo", NodeRange::any())],
    );
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("visitor", "pw-pw-pw", Role::Guest)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("visitor", "pw-pw-pw").await;
    c.send("door echo").await;
    c.expect(b"You do not have access to that door").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye").await;

    burrow.shutdown().await;
}
