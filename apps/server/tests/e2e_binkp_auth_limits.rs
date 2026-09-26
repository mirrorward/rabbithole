//! Authentication budgets at the real inbound binkp password boundary.
//! CONN is disabled to isolate AUTH; no wall-clock sleeps drive recovery.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use burrow::{ftn, Burrow};
use rabbithole_legacy_binkp::{
    cram_md5_response, decode_block, parse_challenge, Address, Command, RawBlock,
};
use rabbithole_server_core::ratelimit::{self, class, Scope};
use rabbithole_server_core::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PASSWORD: &str = "binkp-test-secret";

fn config(dir: &Path) -> ServerConfig {
    ServerConfig {
        data_dir: dir.join("server"),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ftn_enabled: true,
        ftn_addr: "127.0.0.1:0".parse().unwrap(),
        ftn_node: "2:280/1".into(),
        ftn_password: PASSWORD.into(),
        ftn_inbound_dir: dir.join("in"),
        ftn_outbound_dir: dir.join("out"),
        ratelimit_conn_per_min: 0,
        ratelimit_auth_per_min: 1,
        ratelimit_auth_burst: 2,
        ..Default::default()
    }
}

struct Peer {
    stream: TcpStream,
    challenge: Option<Vec<u8>>,
}

impl Peer {
    async fn connect(addr: SocketAddr) -> Self {
        let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .expect("binkp connects promptly")
            .unwrap();
        let mut peer = Self {
            stream,
            challenge: None,
        };
        loop {
            match peer.command().await.expect("server greeting") {
                Command::Nul(text) => {
                    if let Some(challenge) = parse_challenge(&text) {
                        peer.challenge = Some(challenge);
                    }
                }
                Command::Adr(_) => return peer,
                other => panic!("unexpected greeting: {other:?}"),
            }
        }
    }

    async fn send(&mut self, command: Command) {
        self.stream
            .write_all(&command.to_block().encode().unwrap())
            .await
            .unwrap();
    }

    async fn command(&mut self) -> Option<Command> {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let header = self.stream.read_u16().await?;
            let mut bytes = header.to_be_bytes().to_vec();
            bytes.resize(2 + usize::from(header & 0x7fff), 0);
            self.stream.read_exact(&mut bytes[2..]).await?;
            let (block, used) = decode_block(&bytes).expect("valid binkp framing");
            assert_eq!(used, bytes.len());
            let RawBlock::Command { id, args } = block else {
                panic!("authentication never sends a file body");
            };
            Ok::<_, io::Error>(Command::parse(id, &args).expect("valid command"))
        })
        .await
        .expect("binkp responds or closes promptly");
        match result {
            Ok(command) => Some(command),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                ) =>
            {
                None
            }
            Err(error) => panic!("binkp read failed: {error}"),
        }
    }

    async fn authenticate(&mut self, password: &str, cram: bool, claimed_node: u16) -> Command {
        self.send(Command::Adr(vec![Address::new(2, 280, claimed_node, 0)]))
            .await;
        let response = if cram {
            cram_md5_response(
                password.as_bytes(),
                self.challenge.as_deref().expect("CRAM challenge"),
            )
        } else {
            password.into()
        };
        self.send(Command::Pwd(response)).await;
        self.command().await.expect("password receives a response")
    }

    async fn finish(&mut self) {
        self.send(Command::Eob).await;
        assert_eq!(self.command().await, Some(Command::Eob));
        assert_eq!(self.command().await, None);
    }
}

async fn attempt(addr: SocketAddr, password: &str, cram: bool, claimed_node: u16) -> Command {
    let mut peer = Peer::connect(addr).await;
    let reply = peer.authenticate(password, cram, claimed_node).await;
    if matches!(reply, Command::Ok(_)) {
        peer.finish().await;
    } else {
        assert_eq!(peer.command().await, None, "rejected session closes");
    }
    reply
}

fn ipv4_scope() -> Scope {
    Scope::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

#[tokio::test]
async fn failures_share_the_source_ip_budget_at_pwd_and_successes_do_not_spend_it() {
    let dir = tempfile::tempdir().unwrap();
    let server = Burrow::start(config(dir.path())).await.unwrap();
    let addr = server.ftn_addr.unwrap();

    for cram in [false, true] {
        assert!(matches!(
            attempt(addr, PASSWORD, cram, 464).await,
            Command::Ok(_)
        ));
    }
    // Admission to the connection/greeting does not reserve an auth attempt.
    let mut already_connected = Peer::connect(addr).await;
    for (cram, node) in [(false, 464), (true, 999)] {
        assert_eq!(
            attempt(addr, "incorrect", cram, node).await,
            Command::Err("bad password".into()),
            "both failures fit the burst even after successful logins"
        );
    }
    assert!(matches!(
        already_connected.authenticate(PASSWORD, true, 1000).await,
        Command::Bsy(_)
    ));
    assert_eq!(already_connected.command().await, None);
    assert!(matches!(
        attempt(addr, PASSWORD, false, 2000).await,
        Command::Bsy(_)
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn an_exhausted_ip_does_not_block_another_peer_and_service_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.ratelimit_auth_burst = 1;
    let server = Burrow::start(cfg).await.unwrap();
    let ipv4 = server.ftn_addr.unwrap();
    // Both real loopback addresses work without OS-specific 127.0.0.2 aliases.
    // The listeners deliberately share the exact same server/limiter.
    let (ipv6, ipv6_task) = ftn::spawn_ftn(
        server.shared.clone(),
        "[::1]:0".parse().unwrap(),
        dir.path().join("v6-in"),
        dir.path().join("v6-out"),
    )
    .await
    .unwrap();
    assert!(matches!(
        attempt(ipv4, "incorrect", false, 464).await,
        Command::Err(_)
    ));
    assert!(matches!(
        attempt(ipv4, PASSWORD, false, 465).await,
        Command::Bsy(_)
    ));
    assert!(matches!(
        attempt(ipv6, PASSWORD, true, 464).await,
        Command::Ok(_)
    ));
    assert!(!server.shared.rate_probe(ipv4_scope(), class::AUTH));

    // Drive the limiter's existing explicit-clock expiry hook beyond both
    // full refill and audit-note expiry, without a minute-long wall-clock wait.
    // A full expired bucket is equivalent to a fresh one; the retry still
    // traverses the actual socket password boundary.
    server
        .shared
        .ratelimit
        .sweep(ratelimit::now_ms().saturating_add(120_001));
    assert!(matches!(
        attempt(ipv4, PASSWORD, true, 466).await,
        Command::Ok(_)
    ));
    assert!(matches!(
        attempt(ipv4, "incorrect-again", false, 467).await,
        Command::Err(_)
    ));
    assert!(matches!(
        attempt(ipv4, PASSWORD, true, 468).await,
        Command::Bsy(_)
    ));
    ipv6_task.abort();
    let _ = ipv6_task.await;
    server.shutdown().await;
}

#[tokio::test]
async fn only_failed_password_verification_is_charged_even_if_the_peer_disappears() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.ratelimit_auth_burst = 1;
    let server = Burrow::start(cfg).await.unwrap();
    let addr = server.ftn_addr.unwrap();

    // Peer-supplied reason strings and protocol errors must not masquerade as
    // failed verification. A disconnect before M_PWD is likewise not a login.
    let mut peer = Peer::connect(addr).await;
    peer.send(Command::Err("bad password".into())).await;
    assert_eq!(peer.command().await, None);
    let mut peer = Peer::connect(addr).await;
    peer.stream
        .write_all(&RawBlock::Data(vec![1]).encode().unwrap())
        .await
        .unwrap();
    assert_eq!(peer.command().await, None);
    let mut peer = Peer::connect(addr).await;
    peer.stream.shutdown().await.unwrap();
    assert_eq!(peer.command().await, None);
    // A PWD in the authenticated transfer phase is a protocol error, not
    // another failed authentication attempt.
    let mut peer = Peer::connect(addr).await;
    assert!(matches!(
        peer.authenticate(PASSWORD, false, 464).await,
        Command::Ok(_)
    ));
    assert_eq!(peer.command().await, Some(Command::Eob));
    peer.send(Command::Pwd("incorrect".into())).await;
    assert_eq!(peer.command().await, None);
    assert!(server.shared.rate_probe(ipv4_scope(), class::AUTH));

    let mut peer = Peer::connect(addr).await;
    peer.send(Command::Pwd("incorrect".into())).await;
    // Do not wait for M_ERR: accounting cannot depend on delivering it.
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.shared.rate_probe(ipv4_scope(), class::AUTH) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed verification charged even after peer close");
    assert!(matches!(
        attempt(addr, PASSWORD, false, 465).await,
        Command::Bsy(_)
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn disabled_auth_limits_preserve_rejections_and_later_success() {
    for globally_disabled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.ratelimit_enabled = !globally_disabled;
        cfg.ratelimit_auth_per_min = if globally_disabled { 1 } else { 0 };
        cfg.ratelimit_auth_burst = 0;
        let server = Burrow::start(cfg).await.unwrap();
        let addr = server.ftn_addr.unwrap();
        for node in 460..464 {
            assert_eq!(
                attempt(addr, "incorrect", false, node).await,
                Command::Err("bad password".into())
            );
        }
        assert!(matches!(
            attempt(addr, PASSWORD, true, 464).await,
            Command::Ok(_)
        ));
        server.shutdown().await;
    }
}

#[tokio::test]
async fn unsecured_links_do_not_spend_the_auth_budget() {
    for password in ["", "-"] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.ftn_password = password.into();
        cfg.ratelimit_auth_burst = 1;
        let server = Burrow::start(cfg).await.unwrap();
        for node in 460..464 {
            assert!(matches!(
                attempt(server.ftn_addr.unwrap(), "anything", false, node).await,
                Command::Ok(_)
            ));
        }
        assert!(server.shared.rate_probe(ipv4_scope(), class::AUTH));
        server.shutdown().await;
    }
}
